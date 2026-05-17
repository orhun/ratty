//! PTY runtime and parser state.

use std::collections::HashSet;
use std::env;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Context;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use vt100::{Callbacks, Parser, Screen};

use crate::config::AppConfig;

/// Command-line runtime overrides.
#[derive(Debug, Clone, Default)]
pub struct RuntimeOptions {
    /// Command and arguments to execute instead of the configured shell.
    pub command: Option<Vec<String>>,
    /// Working directory used for the spawned PTY command.
    pub working_dir: Option<PathBuf>,
}

/// Callback state for unhandled parser sequences.
#[derive(Default)]
pub struct TerminalParserCallbacks {
    seen_csi: HashSet<String>,
    seen_escape: HashSet<String>,
    pending_replies: Vec<Vec<u8>>,
    kitty_keyboard_flags: u8,
    modify_other_keys: Option<u8>,
}

impl TerminalParserCallbacks {
    /// Drains any terminal replies queued by parser callbacks.
    pub fn take_replies(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_replies)
    }

    /// Returns the active kitty keyboard enhancement flags.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.kitty_keyboard_flags
    }

    /// Returns the active xterm `modifyOtherKeys` level.
    pub fn modify_other_keys(&self) -> Option<u8> {
        self.modify_other_keys
    }
}

impl Callbacks for TerminalParserCallbacks {
    fn unhandled_csi(
        &mut self,
        screen: &mut Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        // CSI 0 c = primary device attributes request.
        if i1.is_none() && i2.is_none() && c == 'c' && params.len() == 1 && params[0] == [0] {
            self.pending_replies.push(b"\x1b[?1;2c".to_vec());
            return;
        }

        // CSI 5 n = device status report request.
        if i1.is_none() && i2.is_none() && c == 'n' && params.len() == 1 && params[0] == [5] {
            self.pending_replies.push(b"\x1b[0n".to_vec());
            return;
        }

        // CSI 6 n = cursor position report request.
        if i1.is_none() && i2.is_none() && c == 'n' && params.len() == 1 && params[0] == [6] {
            let (row, col) = screen.cursor_position();
            self.pending_replies
                .push(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
            return;
        }

        // CSI ? u = kitty keyboard protocol flag query. Reply with the currently active flags so
        // apps can detect whether enhanced key reporting is enabled.
        if i1 == Some(b'?') && i2.is_none() && c == 'u' && params.is_empty() {
            self.pending_replies
                .push(format!("\x1b[?{}u", self.kitty_keyboard_flags).into_bytes());
            return;
        }

        // CSI > flags u = enable kitty keyboard protocol flags for subsequent key reports.
        if i1 == Some(b'>') && i2.is_none() && c == 'u' && params.len() == 1 && params[0].len() == 1
        {
            self.kitty_keyboard_flags = params[0][0].min(u8::MAX as u16) as u8;
            return;
        }

        // CSI < 1 u = pop kitty keyboard enhancement state and fall back to legacy reporting.
        if i1 == Some(b'<') && i2.is_none() && c == 'u' && params.len() == 1 && params[0] == [1] {
            self.kitty_keyboard_flags = 0;
            return;
        }

        // CSI > 4 ; level m = xterm modifyOtherKeys mode. We track the current level so keys like
        // Ctrl+Enter can be encoded in the form the foreground app asked for.
        if i1 == Some(b'>') && i2.is_none() && c == 'm' {
            match params {
                [resource, level] if *resource == [4] && level.len() == 1 => {
                    self.modify_other_keys = Some(level[0].min(u8::MAX as u16) as u8);
                    return;
                }
                [resource] if *resource == [4] => {
                    self.modify_other_keys = None;
                    return;
                }
                _ => {}
            }
        }

        // CSI ? 7 h / CSI ? 7 l toggle line wrapping. Ratty does not model the mode yet, but
        // treating it as known avoids noisy warnings for shells and TUIs that flip it frequently.
        if i1 == Some(b'?')
            && i2.is_none()
            && params.len() == 1
            && params[0] == [7]
            && matches!(c, 'h' | 'l')
        {
            return;
        }

        let mut sequence = String::from("\u{1b}[");
        if let Some(i1) = i1 {
            sequence.push(i1 as char);
        }
        if let Some(i2) = i2 {
            sequence.push(i2 as char);
        }
        for (idx, param) in params.iter().enumerate() {
            if idx > 0 {
                sequence.push(';');
            }
            for (j, value) in param.iter().enumerate() {
                if j > 0 {
                    sequence.push(':');
                }
                sequence.push_str(&value.to_string());
            }
        }
        sequence.push(c);

        if self.seen_csi.insert(sequence.clone()) {
            bevy::log::warn!("unhandled terminal CSI sequence: {sequence}");
        }
    }

    fn unhandled_escape(&mut self, _: &mut Screen, i1: Option<u8>, i2: Option<u8>, b: u8) {
        let mut sequence = String::from("\u{1b}");
        if let Some(i1) = i1 {
            sequence.push(i1 as char);
        }
        if let Some(i2) = i2 {
            sequence.push(i2 as char);
        }
        sequence.push(b as char);

        if self.seen_escape.insert(sequence.clone()) {
            bevy::log::warn!("unhandled terminal escape sequence: {sequence}");
        }
    }
}

/// Running PTY and parser state.
pub struct TerminalRuntime {
    /// PTY output channel.
    pub rx: Receiver<Vec<u8>>,
    /// PTY input writer.
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// PTY master handle.
    pub _master: Box<dyn MasterPty + Send>,
    /// Child process handle.
    pub _child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Terminal parser.
    pub parser: Parser<TerminalParserCallbacks>,
    /// Indicates PTY shutdown.
    pub pty_disconnected: bool,
}

/// Returns the default shell for the current platform.
///
/// On Windows this prefers Git for Windows' `bash.exe` when it can be found
/// (most users running terminal apps on Windows want a POSIX shell so the
/// Ratatui demos behave the same as on Linux/macOS), then `%COMSPEC%` (the
/// resolved command processor), and finally `cmd.exe`. On other platforms
/// it falls back to `/bin/sh`.
fn default_shell() -> String {
    #[cfg(windows)]
    {
        if let Some(bash) = find_git_bash() {
            return bash;
        }
        env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        "/bin/sh".to_string()
    }
}

/// Looks for a Git for Windows `bash.exe` in the well-known install
/// locations, then on `PATH`. Returns the first match.
///
/// `usr/bin/bash.exe` is the MSYS shell bundled with Git for Windows;
/// `bin/bash.exe` is the shim used by the Git Bash launcher. Either works
/// as a PTY shell.
#[cfg(windows)]
fn find_git_bash() -> Option<String> {
    use std::path::PathBuf;

    // Flat candidate table keeps every probe path on one footing: each entry
    // is `(env_var, subpath_under_that_directory)`. New install layouts (Git
    // via Scoop, Chocolatey, custom installers) only need another row here.
    const CANDIDATES: &[(&str, &str)] = &[
        ("ProgramW6432", "Git/bin/bash.exe"),
        ("ProgramW6432", "Git/usr/bin/bash.exe"),
        ("ProgramFiles", "Git/bin/bash.exe"),
        ("ProgramFiles", "Git/usr/bin/bash.exe"),
        ("ProgramFiles(x86)", "Git/bin/bash.exe"),
        ("ProgramFiles(x86)", "Git/usr/bin/bash.exe"),
        ("LOCALAPPDATA", "Programs/Git/bin/bash.exe"),
        ("LOCALAPPDATA", "Programs/Git/usr/bin/bash.exe"),
    ];

    for (env_var, sub) in CANDIDATES {
        let Ok(base) = env::var(env_var) else {
            continue;
        };
        let candidate = PathBuf::from(base).join(sub);
        if candidate.is_file() {
            return candidate.into_os_string().into_string().ok();
        }
    }

    // Final fallback: walk PATH so custom installs (Scoop shims, etc.) work.
    if let Ok(path) = env::var("PATH") {
        for entry in env::split_paths(&path) {
            let candidate = entry.join("bash.exe");
            if candidate.is_file() {
                return candidate.into_os_string().into_string().ok();
            }
        }
    }

    None
}

impl TerminalRuntime {
    /// Spawns the shell PTY runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the PTY cannot be created or the shell cannot be spawned.
    pub fn spawn(config: &AppConfig, options: &RuntimeOptions) -> anyhow::Result<Self> {
        let cols = config.terminal.default_cols;
        let rows = config.terminal.default_rows;
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to create PTY pair")?;

        let mut cmd = if let Some(command) = &options.command {
            let mut command = command.iter();
            let program = command
                .next()
                .context("command override must contain at least one argument")?;
            let mut cmd = CommandBuilder::new(program);
            cmd.args(command);
            cmd
        } else {
            let shell = config
                .shell
                .program
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .or_else(|| env::var("SHELL").ok())
                .unwrap_or_else(default_shell);
            let mut cmd = CommandBuilder::new(shell);
            cmd.args(&config.shell.args);
            cmd
        };

        if let Some(working_dir) = &options.working_dir {
            cmd.cwd(working_dir);
        }
        if !config.env.contains_key("TERM") {
            cmd.env("TERM", "xterm-256color");
        }
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell")?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("failed to create PTY writer")?;

        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(16);
        thread::spawn(move || {
            let mut buf = [0_u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(size) => {
                        if tx.send(buf[..size].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            rx,
            writer: Arc::new(Mutex::new(writer)),
            _master: pair.master,
            _child: child,
            parser: Parser::new_with_callbacks(
                rows,
                cols,
                config.terminal.scrollback,
                TerminalParserCallbacks::default(),
            ),
            pty_disconnected: false,
        })
    }

    /// Writes input bytes to the PTY.
    pub fn write_input(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    /// Resizes the PTY and parser screen.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }

        let _ = self._master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// Returns the active kitty keyboard enhancement flags.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.parser.callbacks().kitty_keyboard_flags()
    }

    /// Returns the active xterm `modifyOtherKeys` level.
    pub fn modify_other_keys(&self) -> Option<u8> {
        self.parser.callbacks().modify_other_keys()
    }
}

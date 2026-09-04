//! PTY runtime and terminal state.

use std::collections::HashSet;
use std::env;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::Context;
use bevy::platform::cell::SyncCell;
use bevy::prelude::Resource;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::config::AppConfig;
use crate::ratty_vt::{Callbacks, Parser, Screen};

/// Command-line runtime overrides.
#[derive(Debug, Clone, Default)]
pub struct RuntimeOptions {
    /// Command and arguments to execute instead of the configured shell.
    pub command: Option<Vec<String>>,
    /// Working directory used for the spawned PTY command.
    pub working_dir: Option<PathBuf>,
}

/// DA1 capabilities ratty advertises: VT220 class (`62`) with ANSI colour
/// (`22`). Nothing else listed by xterm (sixel, ReGIS, OSC 52 clipboard, ...)
/// is implemented, and advertising it would make applications emit payloads
/// that go nowhere.
const PRIMARY_DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?62;22c";

/// Callback state for the sequences the engine leaves to its embedder.
///
/// The engine models the screen; everything that identifies or answers for
/// the *terminal* lives here, so the replies describe ratty by construction:
/// device attributes, status and cursor reports, the terminal version, and the
/// kitty keyboard flag query. Unhandled sequences are logged once each.
#[derive(Default)]
pub struct TerminalParserCallbacks {
    seen_csi: HashSet<String>,
    seen_escape: HashSet<String>,
    pending_replies: Vec<Vec<u8>>,
}

impl TerminalParserCallbacks {
    /// Drains any terminal replies queued by parser callbacks.
    pub fn take_replies(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_replies)
    }
}

/// Encodes ratty's version the way the DA2 firmware field expects: each
/// semver component weighted by a power of 100, pre-release suffix dropped.
fn encoded_version() -> usize {
    let version = env!("CARGO_PKG_VERSION");
    let version = version
        .rsplit_once('-')
        .map_or(version, |(release, _prerelease)| release);

    version
        .split('.')
        .rev()
        .enumerate()
        .map(|(index, component)| {
            let scale = u32::try_from(index)
                .ok()
                .and_then(|index| 100_usize.checked_pow(index))
                .unwrap_or(0);
            scale.saturating_mul(component.parse::<usize>().unwrap_or(0))
        })
        .sum()
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
        let first = params.first().and_then(|param| param.first()).copied();
        let single = params.len() <= 1 && params.first().is_none_or(|param| param.len() <= 1);

        match (i1, i2, c) {
            // CSI 0 c = primary device attributes request.
            (None, None, 'c') if single && first.unwrap_or(0) == 0 => {
                self.pending_replies
                    .push(PRIMARY_DEVICE_ATTRIBUTES.to_vec());
            }
            // CSI > 0 c = secondary device attributes: terminal type, firmware
            // version, ROM cartridge. Type 0 is "VT100" in xterm's table; the
            // firmware field carries ratty's version.
            (Some(b'>'), None, 'c') if single && first.unwrap_or(0) == 0 => {
                self.pending_replies
                    .push(format!("\x1b[>0;{};1c", encoded_version()).into_bytes());
            }
            // CSI 5 n = device status report request.
            (None, None, 'n') if single && first == Some(5) => {
                self.pending_replies.push(b"\x1b[0n".to_vec());
            }
            // CSI 6 n = cursor position report request. Reported at the cell
            // the cursor is drawn in, so a cursor past the last column after
            // a full row reports that column rather than one beyond it.
            (None, None, 'n') if single && first == Some(6) => {
                let (row, col) = screen.display_cursor_position();
                self.pending_replies
                    .push(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
            }
            // CSI > 0 q = XTVERSION: the terminal name and version.
            (Some(b'>'), None, 'q') if single && first.unwrap_or(0) == 0 => {
                self.pending_replies
                    .push(format!("\x1bP>|ratty {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes());
            }
            // CSI ? u = kitty keyboard protocol flag query. The engine tracks
            // the flag stack; ratty answers so applications can detect whether
            // enhanced key reporting is enabled.
            (Some(b'?'), None, 'u') if single && first.unwrap_or(0) == 0 => {
                self.pending_replies
                    .push(format!("\x1b[?{}u", screen.kitty_keyboard_flags()).into_bytes());
            }
            // CSI ? 7 h / CSI ? 7 l toggle line wrapping. Ratty does not model
            // the mode yet, but treating it as known avoids noisy warnings
            // for shells and TUIs that flip it frequently.
            (Some(b'?'), None, 'h' | 'l') if single && first == Some(7) => {}
            _ => {
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
///
/// The `!Sync` PTY handles (the output channel receiver and the master) live
/// in [`SyncCell`]s so the runtime qualifies as a regular [`Resource`] and
/// systems using it are not pinned to the main thread.
#[derive(Resource)]
pub struct TerminalRuntime {
    /// PTY output channel.
    rx: SyncCell<Receiver<Vec<u8>>>,
    /// PTY input writer.
    pub writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    /// PTY master handle.
    master: SyncCell<Option<Box<dyn MasterPty + Send>>>,
    /// Child process handle.
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// PTY reader thread.
    reader_thread: Option<JoinHandle<()>>,
    /// Terminal parser: the VT state machine plus the screen it drives.
    pub parser: Parser<TerminalParserCallbacks>,
    /// Indicates PTY shutdown.
    pub pty_disconnected: bool,
    shutdown_started: bool,
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
        let reader_thread = thread::spawn(move || {
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

        let parser = Parser::new_with_callbacks(
            rows.max(1),
            cols.max(1),
            config.terminal.scrollback,
            TerminalParserCallbacks::default(),
        );

        Ok(Self {
            rx: SyncCell::new(rx),
            writer: Arc::new(Mutex::new(Some(writer))),
            master: SyncCell::new(Some(pair.master)),
            child: Some(child),
            reader_thread: Some(reader_thread),
            parser,
            pty_disconnected: false,
            shutdown_started: false,
        })
    }

    /// Feeds bytes from the PTY into the VT state machine.
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Returns the terminal screen.
    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    /// Returns the terminal screen for mutation (scrollback, resize).
    pub fn screen_mut(&mut self) -> &mut Screen {
        self.parser.screen_mut()
    }

    /// Returns each visible row as a string with trailing blanks trimmed.
    ///
    /// Allocates, so it is only worth calling when something actually diffs
    /// rows; today that is inline-object scroll tracking.
    pub fn visible_row_texts(&self) -> Vec<String> {
        let (_, cols) = self.screen().size();
        self.screen().rows(0, cols).collect()
    }

    /// Drains the replies the parser callbacks have queued for write-back to
    /// the PTY.
    pub fn take_replies(&mut self) -> Vec<Vec<u8>> {
        self.parser.callbacks_mut().take_replies()
    }

    /// Receives pending PTY output without blocking.
    pub fn try_recv(&mut self) -> Result<Vec<u8>, TryRecvError> {
        self.rx.get().try_recv()
    }

    /// Writes input bytes to the PTY.
    pub fn write_input(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        if let Ok(mut writer) = self.writer.lock()
            && let Some(writer) = writer.as_mut()
        {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    /// Resizes the PTY and parser screen.
    pub fn resize(&mut self, cols: u16, rows: u16, pw: u16, ph: u16) {
        if cols == 0 || rows == 0 {
            return;
        }

        if let Some(master) = self.master.get().as_ref() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: pw,
                pixel_height: ph,
            });
        }

        // The engine reflows content and resets the scrolling region itself,
        // so the grid resize is the whole operation: no snapshot and replay.
        self.parser.screen_mut().set_size_reflow(rows, cols);
    }

    /// Returns the active kitty keyboard enhancement flags.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.parser.screen().kitty_keyboard_flags()
    }

    /// Returns the active xterm `modifyOtherKeys` level.
    pub fn modify_other_keys(&self) -> Option<u8> {
        self.parser.screen().modify_other_keys()
    }

    /// Shuts down the PTY runtime without blocking the Bevy main thread indefinitely.
    pub fn shutdown(&mut self) {
        if self.shutdown_started {
            return;
        }
        self.shutdown_started = true;
        self.pty_disconnected = true;

        if let Ok(mut writer) = self.writer.lock() {
            writer.take();
        }

        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
        self.child.take();
        self.master.get().take();

        if self
            .reader_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(reader_thread) = self.reader_thread.take()
        {
            let _ = reader_thread.join();
        }
    }
}

impl Drop for TerminalRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(rows: u16, cols: u16) -> Parser<TerminalParserCallbacks> {
        Parser::new_with_callbacks(rows, cols, 100, TerminalParserCallbacks::default())
    }

    fn replies(parser: &mut Parser<TerminalParserCallbacks>) -> Vec<String> {
        parser
            .callbacks_mut()
            .take_replies()
            .into_iter()
            .map(|reply| String::from_utf8(reply).expect("utf-8 reply"))
            .collect()
    }

    #[test]
    fn replies_are_queued_for_write_back() {
        let mut parser = parser(5, 20);
        parser.process(b"\x1b[0c");
        parser.process(b"\x1b[5n");
        parser.process(b"\x1b[3;7H\x1b[6n");

        let got = replies(&mut parser);
        assert_eq!(got, vec!["\x1b[?62;22c", "\x1b[0n", "\x1b[3;7R"]);
        assert!(replies(&mut parser).is_empty(), "replies must drain");
    }

    /// DA1 must not advertise sixel (`4`) or OSC 52 (`52`), or applications
    /// feature-detect support that does not exist.
    #[test]
    fn primary_device_attributes_advertise_only_what_ratty_implements() {
        let mut parser = parser(5, 20);
        parser.process(b"\x1b[c");
        let reply = replies(&mut parser).remove(0);
        let params: Vec<&str> = reply
            .strip_prefix("\x1b[?")
            .and_then(|rest| rest.strip_suffix('c'))
            .expect("DA1 shape")
            .split(';')
            .collect();
        assert!(params.contains(&"62"));
        assert!(params.contains(&"22"));
        assert!(!params.contains(&"4"));
        assert!(!params.contains(&"52"));
    }

    #[test]
    fn secondary_device_attributes_and_xtversion_report_ratty() {
        let mut parser = parser(5, 20);
        parser.process(b"\x1b[>0c\x1b[>0q\x1b[>q");
        let got = replies(&mut parser);
        assert_eq!(got[0], format!("\x1b[>0;{};1c", encoded_version()));
        assert_eq!(
            got[1],
            format!("\x1bP>|ratty {}\x1b\\", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(got[2], got[1], "a missing parameter defaults to 0");
        assert!(!got[1].to_lowercase().contains("rio"));
    }

    #[test]
    fn encoded_version_matches_the_da2_weighting() {
        // patch + minor*100 + major*10000
        let expected = env!("CARGO_PKG_VERSION")
            .split('.')
            .rev()
            .enumerate()
            .map(|(index, part)| 100_usize.pow(index as u32) * part.parse::<usize>().unwrap_or(0))
            .sum::<usize>();
        assert_eq!(encoded_version(), expected);
    }

    #[test]
    fn cursor_position_report_uses_the_drawn_cell() {
        let mut parser = parser(2, 4);
        parser.process(b"abcd\x1b[6n");
        assert_eq!(replies(&mut parser), vec!["\x1b[1;4R"]);
    }

    #[test]
    fn kitty_keyboard_query_reports_the_active_flags() {
        let mut parser = parser(5, 20);
        parser.process(b"\x1b[?u\x1b[>5u\x1b[?u\x1b[<u\x1b[?u");
        assert_eq!(
            replies(&mut parser),
            vec!["\x1b[?0u", "\x1b[?5u", "\x1b[?0u"]
        );
    }

    #[test]
    fn known_but_unmodelled_sequences_do_not_reply() {
        let mut parser = parser(5, 20);
        parser.process(b"\x1b[?7h\x1b[?7l\x1b[>1;2m");
        assert!(replies(&mut parser).is_empty());
    }
}

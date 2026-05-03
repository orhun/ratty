//! PTY runtime and parser state.

use std::collections::HashSet;
use std::env;
use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Context;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use vt100::{Callbacks, Parser, Screen};

use crate::config::AppConfig;

/// Callback state for unhandled parser sequences.
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

impl TerminalRuntime {
    /// Spawns the shell PTY runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the PTY cannot be created or the shell cannot be spawned.
    pub fn spawn(config: &AppConfig) -> anyhow::Result<Self> {
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

        let shell = config
            .shell
            .program
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(shell);
        cmd.args(&config.shell.args);
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
}

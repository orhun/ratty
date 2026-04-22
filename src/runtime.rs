use std::env;
use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Context;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use vt100::Parser;

use crate::config::TERMINAL_SCROLLBACK;

pub struct TerminalRuntime {
    pub rx: Receiver<Vec<u8>>,
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub _master: Box<dyn MasterPty + Send>,
    pub _child: Box<dyn portable_pty::Child + Send + Sync>,
    pub parser: Parser,
    pub pty_disconnected: bool,
}

impl TerminalRuntime {
    pub fn spawn(cols: u16, rows: u16) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to create PTY pair")?;

        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");

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

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = [0_u8; 4096];
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
            parser: Parser::new(rows, cols, TERMINAL_SCROLLBACK),
            pty_disconnected: false,
        })
    }

    pub fn write_input(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

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

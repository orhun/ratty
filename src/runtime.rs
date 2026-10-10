//! PTY runtime and terminal state.

use std::collections::HashSet;
use std::env;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context;
use bevy::platform::cell::SyncCell;
use bevy::prelude::Resource;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::config::AppConfig;
use fux_vt::{Identity, Options, Parser, Sink, Unhandled};

use crate::screen::ScreenView;

/// Command-line runtime overrides.
#[derive(Debug, Clone, Default)]
pub struct RuntimeOptions {
    /// Command and arguments to execute instead of the configured shell.
    pub command: Option<Vec<String>>,
    /// Working directory used for the spawned PTY command.
    pub working_dir: Option<PathBuf>,
}

/// Who ratty says it is. With an identity, fux-vt answers primary device
/// attributes as VT220 class (`62`) with ANSI colour (`22`) and nothing else
/// listed by xterm (sixel, ReGIS, OSC 52 clipboard, ...), which ratty does
/// not implement and applications would otherwise send payloads for; it
/// answers secondary device attributes and XTVERSION with ratty's version,
/// and reports a cursor waiting to wrap at the last column, as xterm does.
const IDENTITY: Identity = Identity {
    name: "ratty",
    version: env!("CARGO_PKG_VERSION"),
};

/// What ratty asks of the engine beyond a bare VT100: the kitty keyboard
/// protocol and modifyOtherKeys (ratty encodes keys by them, see
/// `keyboard.rs`, and answers the flag query), reflow on resize, and ratty's
/// identity in device replies.
pub const PARSER_OPTIONS: Options = Options {
    events: false,
    extended_replies: false,
    kitty_keyboard: true,
    reflow: true,
    identity: Some(IDENTITY),
};

/// Receives what the engine leaves to its host: replies to queries, queued
/// for write-back to the PTY, and sequences the engine does not implement,
/// logged once each.
#[derive(Default)]
pub struct TerminalParserSink {
    seen: HashSet<String>,
    pending_replies: Vec<Vec<u8>>,
}

impl TerminalParserSink {
    /// Drains any terminal replies the parser queued.
    pub fn take_replies(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_replies)
    }
}

impl Sink for TerminalParserSink {
    fn reply(&mut self, bytes: &[u8]) {
        self.pending_replies.push(bytes.to_vec());
    }

    fn unhandled(&mut self, sequence: Unhandled<'_>) {
        let (kind, sequence) = match sequence {
            Unhandled::Csi {
                params,
                intermediates,
                action,
            } => {
                let mut sequence = String::from("\u{1b}[");
                sequence.extend(intermediates.iter().map(|&byte| char::from(byte)));
                for (idx, param) in params.groups().enumerate() {
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
                sequence.push(char::from(action));
                ("CSI", sequence)
            }
            Unhandled::Escape {
                intermediates,
                action,
            } => {
                let mut sequence = String::from("\u{1b}");
                sequence.extend(intermediates.iter().map(|&byte| char::from(byte)));
                sequence.push(char::from(action));
                ("escape", sequence)
            }
            _ => return,
        };
        if self.seen.insert(sequence.clone()) {
            bevy::log::warn!("unhandled terminal {kind} sequence: {sequence}");
        }
    }
}

/// Runs `change` on the parser, keeping a view scrolled back into history on
/// the row at its top: rows scrolled into history or re-wrapped by a reflow
/// would otherwise slide under it. A row that leaves history altogether
/// leaves the offset as it was, clamped to the history left.
fn keep_scrollback_anchored(
    parser: &mut Parser,
    scrollback: &mut usize,
    change: impl FnOnce(&mut Parser),
) {
    let anchor = (*scrollback > 0)
        .then(|| ScreenView::new(parser.screen(), *scrollback).visible_row(0))
        .flatten()
        .map(|row| row.id);
    change(parser);
    let screen = parser.screen();
    *scrollback = anchor
        .and_then(|id| screen.offset_for_row(id))
        .unwrap_or(*scrollback)
        .min(screen.history_len());
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
    pub parser: Parser,
    /// Replies and unhandled sequences the parser hands back.
    sink: TerminalParserSink,
    /// How many rows the view is scrolled back into history.
    scrollback: usize,
    /// Indicates PTY shutdown.
    pub pty_disconnected: bool,
    output_sequence: u64,
    human_input_sequence: u64,
    agent_input_sequence: u64,
    last_output_at: Option<Instant>,
    child_exit_code: Option<u32>,
    shutdown_started: bool,
    /// Last dimensions successfully applied to the PTY.
    last_pty_size: PtyDimensions,
    /// Last column and row dimensions applied to the VT parser.
    last_parser_size: ParserDimensions,
    /// Desired dimensions retained until both the PTY and parser accept them.
    pending_resize: Option<PtyDimensions>,
}

/// `(cols, rows, pixel_width, pixel_height)` as sent to the PTY.
type PtyDimensions = (u16, u16, u16, u16);
/// `(cols, rows)` as applied to the parser grid.
type ParserDimensions = (u16, u16);

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

        let parser = Parser::with_options(
            rows.max(1),
            cols.max(1),
            config.terminal.scrollback,
            PARSER_OPTIONS,
        )
        .context("failed to create the terminal screen")?;

        Ok(Self {
            rx: SyncCell::new(rx),
            writer: Arc::new(Mutex::new(Some(writer))),
            master: SyncCell::new(Some(pair.master)),
            child: Some(child),
            reader_thread: Some(reader_thread),
            parser,
            sink: TerminalParserSink::default(),
            scrollback: 0,
            pty_disconnected: false,
            output_sequence: 0,
            human_input_sequence: 0,
            agent_input_sequence: 0,
            last_output_at: None,
            child_exit_code: None,
            shutdown_started: false,
            last_pty_size: (cols, rows, 0, 0),
            last_parser_size: (cols, rows),
            pending_resize: None,
        })
    }

    /// Feeds bytes from the PTY into the VT state machine.
    ///
    /// While the view is scrolled back it stays on the rows it shows as new
    /// output scrolls more rows into history.
    pub fn process(&mut self, bytes: &[u8]) {
        let Self {
            parser,
            sink,
            scrollback,
            ..
        } = self;
        keep_scrollback_anchored(parser, scrollback, |parser| {
            if let Err(err) = parser.process_with(bytes, sink) {
                bevy::log::warn!("terminal output dropped: {err}");
            }
        });
    }

    /// Records one PTY output batch, including batches containing only
    /// graphics or control protocol data that do not change visible cells.
    pub fn record_output_batch(&mut self) {
        self.output_sequence = self.output_sequence.saturating_add(1);
        self.last_output_at = Some(Instant::now());
    }

    /// Returns the terminal screen.
    /// Returns the terminal screen as the user sees it, scrolled back by
    /// [`scrollback`](Self::scrollback) rows.
    pub fn screen(&self) -> ScreenView<'_> {
        ScreenView::new(self.parser.screen(), self.scrollback)
    }

    /// Returns how many rows the view is scrolled back into history.
    pub fn scrollback(&self) -> usize {
        self.screen().scrollback()
    }

    /// Scrolls the view `rows` back into history, clamped to what it holds;
    /// `0` shows the live screen.
    pub fn set_scrollback(&mut self, rows: usize) {
        self.scrollback = rows.min(self.parser.screen().history_len());
    }

    /// Returns each visible row as a string with trailing blanks trimmed.
    ///
    /// Allocates, so it is only worth calling when something actually diffs
    /// rows; today that is inline-object scroll tracking.
    pub fn visible_row_texts(&self) -> Vec<String> {
        self.screen().row_texts()
    }

    /// Drains the replies the parser has queued for write-back to the PTY.
    pub fn take_replies(&mut self) -> Vec<Vec<u8>> {
        self.sink.take_replies()
    }

    /// Receives pending PTY output without blocking.
    pub fn try_recv(&mut self) -> Result<Vec<u8>, TryRecvError> {
        self.rx.get().try_recv()
    }

    /// Writes input bytes to the PTY.
    pub fn write_input(&self, bytes: &[u8]) {
        self.write_input_bytes(bytes);
    }

    /// Writes input accepted from Ratty's local keyboard or mouse.
    pub fn write_human_input(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.human_input_sequence = self.human_input_sequence.saturating_add(1);
        }
        self.write_input_bytes(bytes);
    }

    /// Writes input accepted from the authenticated control endpoint.
    pub fn write_agent_input(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.agent_input_sequence = self.agent_input_sequence.saturating_add(1);
        }
        self.write_input_bytes(bytes);
    }

    fn write_input_bytes(&self, bytes: &[u8]) {
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

    /// Returns the number of PTY output batches processed by Ratty.
    pub const fn output_sequence(&self) -> u64 {
        self.output_sequence
    }

    /// Returns the number of accepted local keyboard or mouse inputs.
    pub const fn human_input_sequence(&self) -> u64 {
        self.human_input_sequence
    }

    /// Returns the number of accepted authenticated remote inputs.
    pub const fn agent_input_sequence(&self) -> u64 {
        self.agent_input_sequence
    }

    /// Returns the elapsed time since Ratty processed PTY output.
    pub fn elapsed_since_output(&self) -> Option<Duration> {
        self.last_output_at.map(|instant| instant.elapsed())
    }

    /// Polls and returns the child process exit code when available.
    pub fn child_exit_code(&mut self) -> Option<u32> {
        if self.child_exit_code.is_none()
            && let Some(child) = self.child.as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            self.child_exit_code = Some(status.exit_code());
        }
        self.child_exit_code
    }

    /// Resizes the PTY and parser screen.
    ///
    /// The parser adopts the new grid immediately so local rendering, the
    /// viewport, and mouse mapping always track the committed layout. Only
    /// the operating-system PTY resize is cached for retry after a failure,
    /// so until it succeeds the child still holds the previous geometry.
    /// Dimensions equal to the last applied ones are not resent.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system fails to resize the PTY.
    pub fn resize(&mut self, cols: u16, rows: u16, pw: u16, ph: u16) -> anyhow::Result<()> {
        if cols == 0 || rows == 0 {
            return Ok(());
        }

        self.pending_resize = Some((cols, rows, pw, ph));
        self.retry_pending_resize().map(drop)
    }

    /// Retries the most recently requested resize, if one remains pending.
    ///
    /// Returns `true` when a pending resize was applied.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system fails to resize the PTY;
    /// the request stays pending for the next attempt.
    pub(crate) fn retry_pending_resize(&mut self) -> anyhow::Result<bool> {
        let Some(pty_size @ (cols, rows, pw, ph)) = self.pending_resize else {
            return Ok(false);
        };

        // The caller has already committed the surface, viewport, and mouse
        // mapping to the new grid, so the local parser must follow even when
        // the OS notification below fails. Only the child-facing ioctl is
        // retried.
        let parser_size = (cols, rows);
        if self.last_parser_size != parser_size {
            // The engine reflows content and resets the scrolling region
            // itself, so the grid resize is the whole operation: no snapshot
            // and replay. A failed resize (the size past the engine's
            // allocation limit) leaves the old grid intact.
            let Self {
                parser, scrollback, ..
            } = self;
            keep_scrollback_anchored(parser, scrollback, |parser| {
                if let Err(err) = parser.resize(rows, cols) {
                    bevy::log::warn!("terminal resize to {cols}x{rows} failed: {err}");
                }
            });
            self.last_parser_size = parser_size;
        }

        if self.last_pty_size != pty_size {
            if let Some(master) = self.master.get().as_ref() {
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: pw,
                        pixel_height: ph,
                    })
                    .context("failed to resize PTY")?;
            }
            self.last_pty_size = pty_size;
        }

        self.pending_resize = None;
        Ok(true)
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

    fn parser(rows: u16, cols: u16) -> (Parser, TerminalParserSink) {
        let parser = Parser::with_options(rows, cols, 100, PARSER_OPTIONS).expect("parser");
        (parser, TerminalParserSink::default())
    }

    fn process(parser: &mut Parser, sink: &mut TerminalParserSink, input: &[u8]) {
        parser.process_with(input, sink).expect("process");
    }

    fn replies(sink: &mut TerminalParserSink) -> Vec<String> {
        sink.take_replies()
            .into_iter()
            .map(|reply| String::from_utf8(reply).expect("utf-8 reply"))
            .collect()
    }

    #[test]
    fn replies_are_queued_for_write_back() {
        let (mut parser, mut sink) = parser(5, 20);
        process(&mut parser, &mut sink, b"\x1b[0c");
        process(&mut parser, &mut sink, b"\x1b[5n");
        process(&mut parser, &mut sink, b"\x1b[3;7H\x1b[6n");

        let got = replies(&mut sink);
        assert_eq!(got, vec!["\x1b[?62;22c", "\x1b[0n", "\x1b[3;7R"]);
        assert!(replies(&mut sink).is_empty(), "replies must drain");
    }

    /// DA1 must not advertise sixel (`4`) or OSC 52 (`52`), or applications
    /// feature-detect support that does not exist.
    #[test]
    fn primary_device_attributes_advertise_only_what_ratty_implements() {
        let (mut parser, mut sink) = parser(5, 20);
        process(&mut parser, &mut sink, b"\x1b[c");
        let reply = replies(&mut sink).remove(0);
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
        let (mut parser, mut sink) = parser(5, 20);
        process(&mut parser, &mut sink, b"\x1b[>0c\x1b[>0q\x1b[>q");
        let got = replies(&mut sink);
        // patch + minor*100 + major*10000
        let version = env!("CARGO_PKG_VERSION")
            .split('.')
            .take(3)
            .fold(0, |sum, part| sum * 100 + part.parse::<u32>().unwrap_or(0));
        assert_eq!(got[0], format!("\x1b[>1;{version};0c"));
        assert_eq!(
            got[1],
            format!("\x1bP>|ratty {}\x1b\\", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(got[2], got[1], "a missing parameter defaults to 0");
    }

    #[test]
    fn cursor_position_report_uses_the_drawn_cell() {
        let (mut parser, mut sink) = parser(2, 4);
        process(&mut parser, &mut sink, b"abcd\x1b[6n");
        assert_eq!(replies(&mut sink), vec!["\x1b[1;4R"]);
    }

    #[test]
    fn kitty_keyboard_query_reports_the_active_flags() {
        let (mut parser, mut sink) = parser(5, 20);
        process(
            &mut parser,
            &mut sink,
            b"\x1b[?u\x1b[>5u\x1b[?u\x1b[<u\x1b[?u",
        );
        assert_eq!(replies(&mut sink), vec!["\x1b[?0u", "\x1b[?5u", "\x1b[?0u"]);
    }

    #[test]
    fn known_but_unmodelled_sequences_do_not_reply() {
        let (mut parser, mut sink) = parser(5, 20);
        process(
            &mut parser,
            &mut sink,
            b"\x1b[?7h\x1b[?7l\x1b[>1;2m\x1b[3J\x1b(B",
        );
        assert!(replies(&mut sink).is_empty());
        // Unhandled sequences are logged once each.
        assert!(sink.seen.contains("\u{1b}[>1;2m"));
        assert!(sink.seen.contains("\u{1b}[3J"));
        assert!(sink.seen.contains("\u{1b}(B"));
        assert_eq!(sink.seen.len(), 3);
    }

    fn scrolled(rows: u16, cols: u16, lines: usize, back: usize) -> (Parser, usize) {
        let (mut parser, mut sink) = parser(rows, cols);
        for line in 0..lines {
            process(&mut parser, &mut sink, format!("row{line}\r\n").as_bytes());
        }
        (parser, back)
    }

    #[test]
    fn a_scrolled_back_view_stays_on_its_rows_as_output_arrives() {
        let (mut parser, mut scrollback) = scrolled(3, 10, 12, 2);
        let mut sink = TerminalParserSink::default();
        let before = ScreenView::new(parser.screen(), scrollback).row_texts();
        keep_scrollback_anchored(&mut parser, &mut scrollback, |parser| {
            parser
                .process_with(b"more\r\nand more\r\n", &mut sink)
                .expect("process");
        });
        assert_eq!(scrollback, 4);
        assert_eq!(
            ScreenView::new(parser.screen(), scrollback).row_texts(),
            before
        );

        // At the live screen the view follows the output.
        let mut live = 0;
        keep_scrollback_anchored(&mut parser, &mut live, |parser| {
            parser
                .process_with(b"last\r\n", &mut sink)
                .expect("process");
        });
        assert_eq!(live, 0);
    }

    #[test]
    fn a_scrolled_back_view_stays_on_its_rows_through_a_reflow() {
        let (mut parser, mut scrollback) = scrolled(3, 10, 12, 4);
        let top = ScreenView::new(parser.screen(), scrollback).row_texts()[0].clone();
        keep_scrollback_anchored(&mut parser, &mut scrollback, |parser| {
            parser.resize(5, 4).expect("resize");
        });
        assert_eq!(
            ScreenView::new(parser.screen(), scrollback).row_texts()[0],
            top
        );
    }
}

#[cfg(test)]
mod resize_tests {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct FailOnceMaster {
        attempts: Arc<AtomicUsize>,
        applied_size: Arc<Mutex<Option<PtyDimensions>>>,
    }

    impl MasterPty for FailOnceMaster {
        fn resize(&self, size: PtySize) -> anyhow::Result<()> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("injected resize failure");
            }
            *self.applied_size.lock().expect("resize state lock") =
                Some((size.cols, size.rows, size.pixel_width, size.pixel_height));
            Ok(())
        }

        fn get_size(&self) -> anyhow::Result<PtySize> {
            Ok(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
        }

        fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
            Ok(Box::new(io::empty()))
        }

        fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
            Ok(Box::new(io::sink()))
        }

        #[cfg(unix)]
        fn process_group_leader(&self) -> Option<i32> {
            None
        }

        #[cfg(unix)]
        fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
            None
        }
    }

    fn test_runtime(master: Box<dyn MasterPty + Send>) -> TerminalRuntime {
        let (_tx, rx) = mpsc::sync_channel(1);
        TerminalRuntime {
            rx: SyncCell::new(rx),
            writer: Arc::new(Mutex::new(None)),
            master: SyncCell::new(Some(master)),
            child: None,
            reader_thread: None,
            parser: Parser::with_options(24, 80, 100, PARSER_OPTIONS).expect("parser"),
            sink: TerminalParserSink::default(),
            scrollback: 0,
            pty_disconnected: false,
            output_sequence: 0,
            human_input_sequence: 0,
            agent_input_sequence: 0,
            last_output_at: None,
            child_exit_code: None,
            shutdown_started: false,
            last_pty_size: (80, 24, 0, 0),
            last_parser_size: (80, 24),
            pending_resize: None,
        }
    }

    #[test]
    fn scheduled_retry_applies_a_retained_resize_after_the_parser_reflowed() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let applied_size = Arc::new(Mutex::new(None));
        let master = FailOnceMaster {
            attempts: attempts.clone(),
            applied_size: applied_size.clone(),
        };
        let mut runtime = test_runtime(Box::new(master));

        let first = runtime.resize(100, 30, 800, 600);
        assert!(first.is_err());
        // The parser follows the committed layout immediately; only the
        // child-facing PTY notification is retried.
        assert_eq!(runtime.last_pty_size, (80, 24, 0, 0));
        assert_eq!(runtime.last_parser_size, (100, 30));
        assert_eq!(runtime.screen().size(), (30, 100));
        assert_eq!(runtime.pending_resize, Some((100, 30, 800, 600)));

        let mut app = bevy::prelude::App::new();
        app.init_resource::<crate::terminal::TerminalRedrawState>();
        app.insert_resource(runtime).add_systems(
            bevy::prelude::Update,
            crate::systems::retry_pending_terminal_resize,
        );
        app.update();

        let mut runtime = app.world_mut().resource_mut::<TerminalRuntime>();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.last_pty_size, (100, 30, 800, 600));
        assert_eq!(runtime.pending_resize, None);
        assert_eq!(
            *applied_size.lock().expect("resize state lock"),
            Some((100, 30, 800, 600))
        );

        // Identical dimensions are not resent to the PTY.
        runtime
            .resize(100, 30, 800, 600)
            .expect("no-op resize succeeds");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        runtime
            .resize(100, 30, 900, 600)
            .expect("pixel-only resize should reach the PTY");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(runtime.last_pty_size, (100, 30, 900, 600));
        assert_eq!(runtime.last_parser_size, (100, 30));
    }

    #[test]
    fn activity_sequences_count_output_and_input_sources_independently() {
        let attempts = Arc::new(AtomicUsize::new(1));
        let applied_size = Arc::new(Mutex::new(None));
        let mut runtime = test_runtime(Box::new(FailOnceMaster {
            attempts,
            applied_size,
        }));
        assert!(runtime.elapsed_since_output().is_none());
        runtime.record_output_batch();
        runtime.record_output_batch();
        runtime.write_human_input(b"a");
        runtime.write_human_input(b"");
        runtime.write_agent_input("界".as_bytes());
        assert_eq!(runtime.output_sequence(), 2);
        assert!(runtime.elapsed_since_output().is_some());
        assert_eq!(runtime.human_input_sequence(), 1);
        assert_eq!(runtime.agent_input_sequence(), 1);
    }
}

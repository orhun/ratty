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
use ratty_vt::{Callbacks, Parser, Screen};

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
    /// Last dimensions successfully applied to the PTY.
    last_pty_size: PtyDimensions,
    /// Last column and row dimensions applied to the VT parser.
    last_parser_size: ParserDimensions,
    /// Desired dimensions retained until both the PTY and parser accept them.
    pending_resize: Option<PtyDimensions>,
    #[cfg(feature = "performance")]
    processed_bytes: u64,
    #[cfg(feature = "performance")]
    processed_digest: Option<u64>,
    #[cfg(feature = "performance")]
    queue_metrics: Arc<QueueMetrics>,
}

/// Capacity of the bounded PTY reader channel, in chunks.
const PTY_CHANNEL_CHUNKS: usize = 16;
/// Size of each PTY reader chunk, in bytes.
const PTY_CHUNK_BYTES: usize = 16 * 1024;

/// Upper bound on bytes reported by [`TerminalRuntime::queued_bytes`]: the channel,
/// one blocked reader send, and one chunk between receipt and decrement.
#[cfg(feature = "performance")]
pub const PTY_QUEUE_ACCOUNTING_BOUND: usize = (PTY_CHANNEL_CHUNKS + 2) * PTY_CHUNK_BYTES;

#[cfg(feature = "performance")]
#[derive(Default)]
struct QueueMetrics {
    outstanding: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
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

        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(PTY_CHANNEL_CHUNKS);
        #[cfg(feature = "performance")]
        let queue_metrics = Arc::new(QueueMetrics::default());
        #[cfg(feature = "performance")]
        let reader_metrics = Arc::clone(&queue_metrics);
        let reader_thread = thread::spawn(move || {
            let mut buf = [0_u8; PTY_CHUNK_BYTES];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(size) => {
                        #[cfg(feature = "performance")]
                        {
                            use std::sync::atomic::Ordering::Relaxed;
                            let outstanding =
                                reader_metrics.outstanding.fetch_add(size, Relaxed) + size;
                            reader_metrics.peak.fetch_max(outstanding, Relaxed);
                        }
                        if tx.send(buf[..size].to_vec()).is_err() {
                            #[cfg(feature = "performance")]
                            reader_metrics
                                .outstanding
                                .fetch_sub(size, std::sync::atomic::Ordering::Relaxed);
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
            last_pty_size: (cols, rows, 0, 0),
            last_parser_size: (cols, rows),
            pending_resize: None,
            #[cfg(feature = "performance")]
            processed_bytes: 0,
            #[cfg(feature = "performance")]
            processed_digest: None,
            #[cfg(feature = "performance")]
            queue_metrics,
        })
    }

    /// Feeds bytes from the PTY into the VT state machine.
    pub fn process(&mut self, bytes: &[u8]) {
        #[cfg(feature = "performance")]
        {
            self.processed_bytes = self.processed_bytes.saturating_add(bytes.len() as u64);
            if let Some(digest) = &mut self.processed_digest {
                for byte in bytes {
                    *digest = (*digest ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        self.parser.process(bytes);
    }

    /// Bytes delivered to the VT parser, excluding filtered graphics sequences.
    #[cfg(feature = "performance")]
    pub fn processed_bytes(&self) -> u64 {
        self.processed_bytes
    }

    /// Enables a streaming FNV-1a digest for an untimed fidelity validation pass.
    #[cfg(feature = "performance")]
    pub fn start_processed_digest(&mut self) {
        self.processed_digest = Some(0xcbf29ce484222325);
    }

    /// Returns the optional digest of bytes delivered since validation started.
    #[cfg(feature = "performance")]
    pub fn processed_digest(&self) -> Option<u64> {
        self.processed_digest
    }

    /// Polls the PTY child's exit status independently of output EOF.
    #[cfg(feature = "performance")]
    pub fn child_exit_success(&mut self) -> std::io::Result<Option<bool>> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotConnected, "no PTY child"))?;
        let result = child.try_wait()?.map(|status| status.success());
        if result.is_some() {
            // portable-pty's Unix kill sends SIGHUP before checking status.
            // Once reaped, retaining this handle could signal a reused PID
            // during shutdown.
            self.child.take();
        }
        Ok(result)
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
        let bytes = self.rx.get().try_recv()?;
        #[cfg(feature = "performance")]
        self.queue_metrics
            .outstanding
            .fetch_sub(bytes.len(), std::sync::atomic::Ordering::Relaxed);
        Ok(bytes)
    }

    /// Current and peak PTY bytes awaiting receipt, including a blocked send.
    ///
    /// Excludes the reader's fixed buffer and chunks already returned by
    /// `try_recv`. The channel and pending send hold at most 17 chunks of
    /// 16 KiB; accounting can briefly include an eighteenth chunk between
    /// receipt and decrement. This is an upper bound, not an exact channel
    /// occupancy measurement. Unread kernel PTY and producer bytes are not
    /// counted. The peak includes startup.
    #[cfg(feature = "performance")]
    pub fn queued_bytes(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let current = self.queue_metrics.outstanding.load(Relaxed);
        (current, self.queue_metrics.peak.load(Relaxed).max(current))
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
            // and replay.
            self.parser.screen_mut().set_size_reflow(rows, cols);
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

#[cfg(test)]
mod resize_tests {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[cfg(feature = "performance")]
    #[derive(Debug, Clone)]
    struct ObservedChild {
        kills: Arc<AtomicUsize>,
        status: Option<u32>,
    }

    #[cfg(feature = "performance")]
    impl portable_pty::ChildKiller for ObservedChild {
        fn kill(&mut self) -> io::Result<()> {
            self.kills.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    #[cfg(feature = "performance")]
    impl portable_pty::Child for ObservedChild {
        fn try_wait(&mut self) -> io::Result<Option<portable_pty::ExitStatus>> {
            Ok(self.status.map(portable_pty::ExitStatus::with_exit_code))
        }

        fn wait(&mut self) -> io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(
                self.status.unwrap_or(0),
            ))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    #[cfg(feature = "performance")]
    #[test]
    fn polling_exit_discards_reaped_children_but_preserves_live_child_cleanup() {
        for status in [None, Some(0), Some(7)] {
            let kills = Arc::new(AtomicUsize::new(0));
            let mut runtime = test_runtime(Box::new(FailOnceMaster {
                attempts: Arc::new(AtomicUsize::new(0)),
                applied_size: Arc::new(Mutex::new(None)),
            }));
            runtime.child = Some(Box::new(ObservedChild {
                kills: kills.clone(),
                status,
            }));
            assert_eq!(
                runtime.child_exit_success().expect("poll child status"),
                status.map(|code| code == 0)
            );
            assert_eq!(runtime.child.is_some(), status.is_none());
            runtime.shutdown();
            assert_eq!(kills.load(Ordering::SeqCst), usize::from(status.is_none()));
        }
    }

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
            parser: Parser::new_with_callbacks(24, 80, 100, TerminalParserCallbacks::default()),
            pty_disconnected: false,
            shutdown_started: false,
            last_pty_size: (80, 24, 0, 0),
            last_parser_size: (80, 24),
            pending_resize: None,
            #[cfg(feature = "performance")]
            processed_bytes: 0,
            #[cfg(feature = "performance")]
            processed_digest: None,
            #[cfg(feature = "performance")]
            queue_metrics: Arc::new(QueueMetrics::default()),
        }
    }

    #[cfg(feature = "performance")]
    #[test]
    fn queue_accounting_decrements_only_on_successful_receipt() {
        let mut runtime = test_runtime(Box::new(FailOnceMaster {
            attempts: Arc::new(AtomicUsize::new(0)),
            applied_size: Arc::new(Mutex::new(None)),
        }));
        let (tx, rx) = mpsc::sync_channel(1);
        runtime.rx = SyncCell::new(rx);
        runtime
            .queue_metrics
            .outstanding
            .store(3, Ordering::Relaxed);
        runtime.queue_metrics.peak.store(3, Ordering::Relaxed);
        tx.send(vec![1, 2, 3]).expect("send output");
        assert_eq!(runtime.queued_bytes(), (3, 3));
        assert_eq!(runtime.try_recv().expect("receive output"), vec![1, 2, 3]);
        assert_eq!(runtime.queued_bytes(), (0, 3));
        assert_eq!(runtime.try_recv(), Err(TryRecvError::Empty));
        drop(tx);
        assert_eq!(runtime.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(runtime.queued_bytes(), (0, 3));
    }

    #[cfg(feature = "performance")]
    #[test]
    fn fidelity_digest_is_opt_in_and_independent_of_chunk_boundaries() {
        let make_runtime = || {
            test_runtime(Box::new(FailOnceMaster {
                attempts: Arc::new(AtomicUsize::new(0)),
                applied_size: Arc::new(Mutex::new(None)),
            }))
        };
        let mut whole = make_runtime();
        whole.process(b"READY");
        assert_eq!(whole.processed_digest(), None);
        whole.start_processed_digest();
        whole.process(b"hello");
        assert_eq!(whole.processed_digest(), Some(0xa430d84680aabd0b));
        let mut split = make_runtime();
        split.start_processed_digest();
        for byte in b"hello" {
            split.process(&[*byte]);
        }
        assert_eq!(split.processed_digest(), whole.processed_digest());
        split.start_processed_digest();
        split.process(b"jello");
        assert_ne!(split.processed_digest(), whole.processed_digest());
        assert_eq!(whole.processed_bytes(), 10);
        assert_eq!(
            whole
                .child_exit_success()
                .expect_err("no child handle")
                .kind(),
            io::ErrorKind::NotConnected
        );
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
}

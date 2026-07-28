//! vt100-shaped shim over rio-vt's terminal engine.
//!
//! ratty was written against the `vt100` crate. This module re-exposes the
//! subset of that API surface it relied on (`Parser`, `Screen`, `Cell`,
//! `Color`, and the mouse-protocol enums), backed by `rio_vt::Crosswords`, so
//! the rest of the codebase changes only its import paths.
//!
//! Notable behavioral mappings vs. vt100:
//! - Device attribute / status / cursor-position replies are produced by
//!   rio-vt itself as `RioEvent::PtyWrite` events, captured here and drained
//!   via [`Parser::take_replies`] (vt100 required the app to synthesize them).
//! - Kitty keyboard flags are derived from rio-vt's public `Mode` bits.
//! - xterm `modifyOtherKeys` has no rio-vt equivalent, so it is tracked with a
//!   small byte sniffer over the input stream.
//! - Resize reflow is native in rio-vt, so [`ScreenMut::set_size`] just resizes
//!   the grid (vt100 required a snapshot/replay dance).

use std::sync::{Arc, Mutex};

use rio_vt::ansi::CursorShape;
use rio_vt::config::colors::{AnsiColor, NamedColor};
use rio_vt::crosswords::grid::row::Row;
use rio_vt::crosswords::grid::Scroll;
use rio_vt::crosswords::pos::Column;
use rio_vt::crosswords::square::{ContentTag, Square, Wide};
use rio_vt::crosswords::style::{Style, StyleFlags};
use rio_vt::crosswords::{Crosswords, CrosswordsSize, Mode};
use rio_vt::event::{EventListener, RioEvent, WindowId};
use rio_vt::performer::handler::Processor;

/// Terminal cell color, mirroring `vt100::Color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// The terminal's configured default color.
    Default,
    /// A palette index (0-255).
    Idx(u8),
    /// A 24-bit color.
    Rgb(u8, u8, u8),
}

/// The xterm mouse handling mode currently in use, mirroring
/// `vt100::MouseProtocolMode`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolMode {
    /// Mouse handling is disabled.
    #[default]
    None,
    /// Report on button press only (X10).
    Press,
    /// Report on button press and release (VT200).
    PressRelease,
    /// Report press/release and motion while a button is held.
    ButtonMotion,
    /// Report press/release and all motion.
    AnyMotion,
}

/// The encoding used for mouse reports, mirroring
/// `vt100::MouseProtocolEncoding`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolEncoding {
    /// Default single-printable-byte encoding.
    #[default]
    Default,
    /// UTF-8-based encoding.
    Utf8,
    /// SGR-like encoding.
    Sgr,
}

/// Shared state populated by rio-vt events while parsing.
#[derive(Clone, Default)]
struct Listener {
    /// Bytes rio-vt wants written back to the PTY (DA/DSR/CPR/kitty replies).
    replies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl EventListener for Listener {
    fn event(&self) -> (Option<RioEvent>, bool) {
        (None, false)
    }

    fn send_event(&self, event: RioEvent, _id: WindowId) {
        if let RioEvent::PtyWrite(_route, text) = event
            && let Ok(mut replies) = self.replies.lock()
        {
            replies.push(text.into_bytes());
        }
    }
}

/// A single terminal cell, mirroring the `vt100::Cell` accessors ratty uses.
pub struct Cell {
    contents: String,
    wide_continuation: bool,
    fg: Color,
    bg: Color,
    flags: StyleFlags,
}

impl Cell {
    fn new(square: Square, styles: &[Style]) -> Self {
        let wide_continuation = matches!(square.wide(), Wide::Spacer);
        let ch = square.c();
        let contents = if wide_continuation || ch == '\0' {
            String::new()
        } else {
            ch.to_string()
        };

        let (fg, bg, flags) = match square.content_tag() {
            ContentTag::Codepoint => {
                let style = styles
                    .get(square.style_id() as usize)
                    .copied()
                    .unwrap_or_default();
                (map_color(style.fg), map_color(style.bg), style.flags)
            }
            ContentTag::BgPalette => (
                Color::Default,
                Color::Idx(square.bg_palette_index()),
                StyleFlags::empty(),
            ),
            ContentTag::BgRgb => {
                let (r, g, b) = square.bg_rgb();
                (Color::Default, Color::Rgb(r, g, b), StyleFlags::empty())
            }
        };

        Self {
            contents,
            wide_continuation,
            fg,
            bg,
            flags,
        }
    }

    /// Returns the cell's textual contents (empty for blank/continuation cells).
    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// Returns whether the cell holds any text.
    pub fn has_contents(&self) -> bool {
        !self.contents.is_empty()
    }

    /// Returns whether this is the trailing cell of a wide character.
    pub fn is_wide_continuation(&self) -> bool {
        self.wide_continuation
    }

    /// Returns the cell's foreground color.
    pub fn fgcolor(&self) -> Color {
        self.fg
    }

    /// Returns the cell's background color.
    pub fn bgcolor(&self) -> Color {
        self.bg
    }

    /// Returns whether the bold attribute is set.
    pub fn bold(&self) -> bool {
        self.flags.contains(StyleFlags::BOLD)
    }

    /// Returns whether the dim attribute is set.
    pub fn dim(&self) -> bool {
        self.flags.contains(StyleFlags::DIM)
    }

    /// Returns whether the italic attribute is set.
    pub fn italic(&self) -> bool {
        self.flags.contains(StyleFlags::ITALIC)
    }

    /// Returns whether any underline attribute is set.
    pub fn underline(&self) -> bool {
        self.flags.intersects(StyleFlags::ALL_UNDERLINES)
    }

    /// Returns whether the inverse attribute is set.
    pub fn inverse(&self) -> bool {
        self.flags.contains(StyleFlags::INVERSE)
    }
}

/// Immutable view over the terminal screen, mirroring `&vt100::Screen`.
///
/// Snapshots the visible grid on construction so cell/row lookups are cheap and
/// consistent for the lifetime of the view.
pub struct Screen<'a> {
    term: &'a Crosswords<Listener>,
    rows: Vec<Row<Square>>,
}

impl<'a> Screen<'a> {
    fn new(term: &'a Crosswords<Listener>) -> Self {
        Self {
            rows: term.visible_rows(),
            term,
        }
    }

    /// Returns the screen size as `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        (
            self.term.screen_lines() as u16,
            self.term.columns() as u16,
        )
    }

    /// Returns the cursor position as `(row, col)`.
    pub fn cursor_position(&self) -> (u16, u16) {
        let pos = self.term.cursor().pos;
        (
            u16::try_from(pos.row.0.max(0)).unwrap_or(u16::MAX),
            u16::try_from(pos.col.0).unwrap_or(u16::MAX),
        )
    }

    /// Returns whether the alternate screen is active.
    pub fn alternate_screen(&self) -> bool {
        self.term.mode().contains(Mode::ALT_SCREEN)
    }

    /// Returns whether the cursor is hidden.
    pub fn hide_cursor(&self) -> bool {
        !self.term.mode().contains(Mode::SHOW_CURSOR)
    }

    /// Returns whether bracketed paste mode is active.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(Mode::BRACKETED_PASTE)
    }

    /// Returns whether application cursor keys mode is active.
    pub fn application_cursor(&self) -> bool {
        self.term.mode().contains(Mode::APP_CURSOR)
    }

    /// Returns the active mouse protocol mode.
    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        let mode = self.term.mode();
        if mode.contains(Mode::MOUSE_MOTION) {
            MouseProtocolMode::AnyMotion
        } else if mode.contains(Mode::MOUSE_DRAG) {
            MouseProtocolMode::ButtonMotion
        } else if mode.contains(Mode::MOUSE_REPORT_CLICK) {
            MouseProtocolMode::PressRelease
        } else {
            MouseProtocolMode::None
        }
    }

    /// Returns the active mouse protocol encoding.
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        let mode = self.term.mode();
        if mode.contains(Mode::SGR_MOUSE) {
            MouseProtocolEncoding::Sgr
        } else if mode.contains(Mode::UTF8_MOUSE) {
            MouseProtocolEncoding::Utf8
        } else {
            MouseProtocolEncoding::Default
        }
    }

    /// Returns the cell at `(row, col)`, or `None` if out of range.
    pub fn cell(&self, row: u16, col: u16) -> Option<Cell> {
        let row = self.rows.get(usize::from(row))?;
        if usize::from(col) >= self.term.columns() {
            return None;
        }
        Some(Cell::new(row[Column(usize::from(col))], self.styles()))
    }

    /// Returns the visible rows as strings, starting at column `start` and
    /// spanning `width` columns.
    pub fn rows(&self, start: u16, width: u16) -> impl Iterator<Item = String> + '_ {
        let cols = self.term.columns();
        let start = usize::from(start);
        let end = start.saturating_add(usize::from(width)).min(cols);
        self.rows.iter().map(move |row| {
            let mut out = String::with_capacity(end.saturating_sub(start));
            for col in start..end {
                let square = row[Column(col)];
                if matches!(square.wide(), Wide::Spacer) {
                    continue;
                }
                let ch = square.c();
                out.push(if ch == '\0' { ' ' } else { ch });
            }
            out.trim_end().to_string()
        })
    }

    fn styles(&self) -> &[Style] {
        self.term.grid.style_set.styles()
    }
}

/// Mutable view over the terminal screen, mirroring `&mut vt100::Screen`.
pub struct ScreenMut<'a> {
    term: &'a mut Crosswords<Listener>,
}

impl ScreenMut<'_> {
    /// Resizes the terminal grid; rio-vt reflows content natively.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        self.term.resize(CrosswordsSize::new(
            usize::from(cols.max(1)),
            usize::from(rows.max(1)),
        ));
    }

    /// Returns the current scrollback offset (rows scrolled into history).
    pub fn scrollback(&self) -> usize {
        self.term.display_offset()
    }

    /// Sets the scrollback offset to `rows` rows into history.
    pub fn set_scrollback(&mut self, rows: usize) {
        let current = self.term.display_offset() as i64;
        let delta = rows as i64 - current;
        if delta != 0 {
            self.term
                .scroll_display(Scroll::Delta(delta.clamp(i32::MIN as i64, i32::MAX as i64) as i32));
        }
    }
}

/// Terminal parser owning the rio-vt state machine, mirroring `vt100::Parser`.
pub struct Parser {
    term: Crosswords<Listener>,
    processor: Processor,
    replies: Arc<Mutex<Vec<Vec<u8>>>>,
    modify_other_keys: Option<u8>,
}

impl Parser {
    /// Creates a parser with the given grid size and scrollback history limit.
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        let replies = Arc::new(Mutex::new(Vec::new()));
        let listener = Listener {
            replies: Arc::clone(&replies),
        };
        let size = CrosswordsSize::new(usize::from(cols.max(1)), usize::from(rows.max(1)));
        let term = Crosswords::new(
            size,
            CursorShape::Block,
            listener,
            WindowId::from(0),
            0,
            scrollback,
        );
        Self {
            term,
            processor: Processor::default(),
            replies,
            modify_other_keys: None,
        }
    }

    /// Feeds bytes from the PTY into the parser.
    pub fn process(&mut self, bytes: &[u8]) {
        self.track_modify_other_keys(bytes);
        self.processor.advance(&mut self.term, bytes);
    }

    /// Returns an immutable view of the current screen.
    pub fn screen(&self) -> Screen<'_> {
        Screen::new(&self.term)
    }

    /// Returns a mutable view of the current screen.
    pub fn screen_mut(&mut self) -> ScreenMut<'_> {
        ScreenMut {
            term: &mut self.term,
        }
    }

    /// Drains bytes queued for write-back to the PTY.
    pub fn take_replies(&mut self) -> Vec<Vec<u8>> {
        self.replies
            .lock()
            .map(|mut replies| std::mem::take(&mut *replies))
            .unwrap_or_default()
    }

    /// Returns the active kitty keyboard enhancement flags, derived from the
    /// terminal's current mode bits.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        let mode = self.term.mode();
        let mut flags = 0u8;
        if mode.contains(Mode::DISAMBIGUATE_ESC_CODES) {
            flags |= 0b0_0001;
        }
        if mode.contains(Mode::REPORT_EVENT_TYPES) {
            flags |= 0b0_0010;
        }
        if mode.contains(Mode::REPORT_ALTERNATE_KEYS) {
            flags |= 0b0_0100;
        }
        if mode.contains(Mode::REPORT_ALL_KEYS_AS_ESC) {
            flags |= 0b0_1000;
        }
        if mode.contains(Mode::REPORT_ASSOCIATED_TEXT) {
            flags |= 0b1_0000;
        }
        flags
    }

    /// Returns the active xterm `modifyOtherKeys` level.
    pub fn modify_other_keys(&self) -> Option<u8> {
        self.modify_other_keys
    }

    /// Sniffs `CSI > 4 [; level] m` sequences to track xterm modifyOtherKeys,
    /// which rio-vt does not model.
    fn track_modify_other_keys(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while i + 3 < bytes.len() {
            if &bytes[i..i + 3] != b"\x1b[>" {
                i += 1;
                continue;
            }
            let params_start = i + 3;
            let mut j = params_start;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'm' {
                let mut parts = bytes[params_start..j].split(|&b| b == b';');
                if parts.next() == Some(b"4") {
                    self.modify_other_keys = parts
                        .next()
                        .and_then(|level| std::str::from_utf8(level).ok())
                        .and_then(|level| level.parse().ok());
                }
                i = j + 1;
            } else {
                i = params_start;
            }
        }
    }
}

/// Maps a rio-vt `AnsiColor` to the vt100-shaped [`Color`].
fn map_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Named(NamedColor::Foreground | NamedColor::Background) => Color::Default,
        AnsiColor::Named(named) => {
            let index = named as u32;
            if index < 16 {
                Color::Idx(index as u8)
            } else {
                Color::Default
            }
        }
        AnsiColor::Indexed(index) => Color::Idx(index),
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

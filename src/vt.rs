//! rio-vt integration.
//!
//! Holds the [`EventListener`] ratty drives rio-vt with, the screen-reading
//! helpers its renderers share, and the small amount of terminal state rio-vt
//! does not model itself.
//!
//! Screen reads go through [`visible_row`] rather than rio-vt's `visible_rows`.
//! That function deep-copies every visible row on each call, and ratty reads the
//! screen several times per frame and per PTY chunk, so indexing the grid and
//! borrowing avoids the copy entirely.

use std::sync::{Arc, Mutex, PoisonError};

use rio_vt::ansi::CursorShape;
use rio_vt::ansi::graphics::UpdateQueues;
use rio_vt::config::colors::{AnsiColor, NamedColor};
use rio_vt::crosswords::grid::row::Row;
use rio_vt::crosswords::grid::{Grid, Scroll};
use rio_vt::crosswords::pos::{Column, Line, Pos};
use rio_vt::crosswords::square::{ContentTag, Square, Wide};
use rio_vt::crosswords::style::{Style, StyleFlags};
use rio_vt::crosswords::{Crosswords, Mode};
use rio_vt::event::{EventListener, RioEvent, WindowId};

/// The rio-vt state machine ratty drives.
pub type VtTerminal = Crosswords<TerminalEventSink>;

/// Sink for the events rio-vt raises while parsing.
///
/// Ratty acts on two variants: [`RioEvent::PtyWrite`] — the engine's DA, DSR,
/// CPR, XTVERSION, kitty-keyboard, and kitty-graphics replies, which have to
/// be written back to the PTY — and [`RioEvent::UpdateGraphics`], which
/// carries decoded kitty image pixels for upload to the GPU.
///
/// `send_event` takes `&self`, so the queues need interior mutability, and
/// `TerminalRuntime` is a Bevy resource that has to stay `Send + Sync`, which
/// rules out `RefCell`.
#[derive(Clone, Default)]
pub struct TerminalEventSink {
    replies: Arc<Mutex<Vec<Vec<u8>>>>,
    graphics: Arc<Mutex<Vec<UpdateQueues>>>,
}

impl TerminalEventSink {
    /// Drains the bytes rio-vt has queued for write-back to the PTY.
    pub fn take_replies(&self) -> Vec<Vec<u8>> {
        let mut replies = self.replies.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *replies)
    }

    /// Drains the graphics update queues rio-vt has emitted.
    pub fn take_graphics_updates(&self) -> Vec<UpdateQueues> {
        let mut updates = self.graphics.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *updates)
    }
}

impl EventListener for TerminalEventSink {
    fn send_event(&self, event: RioEvent, _window: WindowId) {
        match event {
            // Engine-generated replies: primary and secondary device
            // attributes, DSR, CPR, XTVERSION, kitty keyboard mode reports,
            // and XTSMGRAPHICS. Recover from a poisoned lock rather than
            // dropping these — an application that queried is waiting on them.
            RioEvent::PtyWrite(_route, text) => {
                let reply = rewrite_reply(&text).unwrap_or(text);
                let mut replies = self.replies.lock().unwrap_or_else(PoisonError::into_inner);
                replies.push(reply.into_bytes());
            }

            // Decoded image pixels the engine wants uploaded to the GPU, plus
            // texture keys whose images were deleted or evicted. Ratty's
            // kitty-graphics sync drains these each frame.
            RioEvent::UpdateGraphics { queues, .. } => {
                let mut updates = self.graphics.lock().unwrap_or_else(PoisonError::into_inner);
                updates.push(queues);
            }

            // Requests carrying a reply callback rio-vt expects the embedder to
            // invoke and write back. Ratty answers none of them yet, so an
            // application that queries with a timeout waits it out:
            //   ColorRequest        OSC 4/10/11/12 palette queries
            //   TextAreaSizeRequest CSI 14t/16t/18t pixel-geometry queries
            //   ClipboardLoad       OSC 52 clipboard read
            // Answering them needs, respectively, ratty's theme, its cell
            // metrics, and a clipboard-read policy.
            RioEvent::ColorRequest(..)
            | RioEvent::TextAreaSizeRequest(..)
            | RioEvent::ClipboardLoad(..) => {}

            // Features ratty does not implement yet, listed so that adding one
            // means filling in an arm rather than discovering the event exists.
            RioEvent::ClipboardStore(..)
            | RioEvent::Title(..)
            | RioEvent::TitleWithSubtitle(..)
            | RioEvent::ResetTitle
            | RioEvent::Bell
            | RioEvent::DesktopNotification { .. }
            | RioEvent::ProgressReport(..)
            | RioEvent::ColorChange(..) => {}

            // The rest is Rio's own window, tab, and config plumbing, plus
            // redraw bookkeeping ratty drives from Bevy instead. `RioEvent` is
            // not `#[non_exhaustive]` and gains variants across minor releases,
            // so this stays a catch-all rather than an exhaustive match that
            // would break the build on every upgrade.
            _ => {}
        }
    }
}

/// DA1 capability parameters ratty must not advertise.
///
/// rio-vt answers primary device attributes with a fixed list describing what
/// the *engine* can parse, not what the embedder renders. `4` is sixel
/// graphics — the engine decodes it, but ratty only renders kitty-protocol
/// placements — and `52` is OSC 52 clipboard access, whose events
/// [`TerminalEventSink`] drops. Leaving either in makes applications
/// feature-detect support that does not exist and emit payloads ratty
/// silently swallows.
const UNSUPPORTED_DA1_CAPABILITIES: &[&str] = &["4", "52"];

/// Rewrites an engine reply that would misreport ratty's capabilities or
/// identity, or returns `None` to send it through untouched.
///
/// rio-vt answers device-attribute and version queries on ratty's behalf, which
/// is the right split — it owns the protocol framing. But it answers them as
/// Rio: the DA1 capability list is hardcoded, and both the XTVERSION string and
/// the DA2 firmware field carry rio-vt's own name and crate version. Patching
/// the payloads on the way out keeps the engine's framing while reporting what
/// ratty actually is and does.
///
/// Parsing structurally rather than matching the exact strings means a rio-vt
/// release that changes its capability list still gets filtered.
fn rewrite_reply(text: &str) -> Option<String> {
    // Primary device attributes: `CSI ? <capabilities> c`.
    if let Some(params) = text
        .strip_prefix("\x1b[?")
        .and_then(|rest| rest.strip_suffix('c'))
    {
        let kept = params
            .split(';')
            .filter(|param| !UNSUPPORTED_DA1_CAPABILITIES.contains(param))
            .collect::<Vec<_>>();
        // Nothing to do when the engine already reported only what ratty
        // supports, and never answer with an empty capability list.
        if kept.is_empty() || kept.len() == params.split(';').count() {
            return None;
        }
        return Some(format!("\x1b[?{}c", kept.join(";")));
    }

    // Secondary device attributes: `CSI > <type> ; <firmware> ; <cartridge> c`,
    // where the firmware field carries rio-vt's crate version.
    if let Some(params) = text
        .strip_prefix("\x1b[>")
        .and_then(|rest| rest.strip_suffix('c'))
    {
        let mut fields = params.split(';').map(str::to_owned).collect::<Vec<_>>();
        if fields.len() != 3 {
            return None;
        }
        fields[1] = encoded_version().to_string();
        return Some(format!("\x1b[>{}c", fields.join(";")));
    }

    // XTVERSION: `DCS > | <name and version> ST`.
    if text.starts_with("\x1bP>|") && text.ends_with("\x1b\\") {
        return Some(format!("\x1bP>|ratty {}\x1b\\", env!("CARGO_PKG_VERSION")));
    }

    None
}

/// Encodes ratty's version the way the DA2 firmware field expects.
///
/// Mirrors rio-vt's own encoding — pre-release suffix dropped, then each semver
/// component weighted by a power of 100 — so the field keeps its established
/// shape and only the value changes.
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

/// Returns the visible row at `row`, or `None` when it is outside the grid.
///
/// Mirrors rio-vt's own `visible_line_bounds` offset math, but borrows instead
/// of copying. See the module docs.
pub fn visible_row(term: &VtTerminal, row: u16) -> Option<&Row<Square>> {
    if usize::from(row) >= term.screen_lines() {
        return None;
    }
    let offset = i32::try_from(term.display_offset()).unwrap_or(i32::MAX);
    Some(&term.grid[Line(i32::from(row) - offset)])
}

/// Appends a cell's full grapheme cluster to `out`.
///
/// Wide-character continuation cells contribute nothing: their codepoint is a
/// space, and emitting it would overwrite the left half of the glyph.
pub fn push_cell_text(out: &mut String, grid: &Grid<Square>, pos: Pos) {
    if matches!(grid[pos].wide(), Wide::Spacer) {
        return;
    }
    // `cell_text` yields the base codepoint followed by any zero-width marks,
    // so combining marks and ZWJ sequences survive.
    for character in grid.cell_text(pos) {
        if character != '\0' {
            out.push(character);
        }
    }
}

/// Returns whether the cell is the second half of a wide glyph.
///
/// Callers that substitute a space for blank cells must skip these outright:
/// a spacer is padding, not an empty cell, and emitting a space for it inserts
/// a gap after every wide character.
pub fn is_wide_spacer(grid: &Grid<Square>, pos: Pos) -> bool {
    matches!(grid[pos].wide(), Wide::Spacer)
}

/// Returns the grid position of a visible `(row, col)`.
pub fn visible_pos(term: &VtTerminal, row: u16, col: u16) -> Pos {
    let offset = i32::try_from(term.display_offset()).unwrap_or(i32::MAX);
    Pos::new(Line(i32::from(row) - offset), Column(usize::from(col)))
}

/// Returns each visible row as a trailing-trimmed string.
///
/// Allocates, so it is only worth calling when something actually diffs rows —
/// today that is inline-object scroll tracking.
pub fn visible_row_texts(term: &VtTerminal) -> Vec<String> {
    let columns = term.columns();
    (0..term.screen_lines())
        .map(|row| {
            let Some(row) = u16::try_from(row)
                .ok()
                .filter(|row| visible_row(term, *row).is_some())
            else {
                return String::new();
            };
            let mut out = String::with_capacity(columns);
            for col in 0..columns {
                let Ok(col) = u16::try_from(col) else {
                    break;
                };
                let pos = visible_pos(term, row, col);
                if is_wide_spacer(&term.grid, pos) {
                    continue;
                }
                let before = out.len();
                push_cell_text(&mut out, &term.grid, pos);
                if out.len() == before {
                    // Blank cells are stored as NUL rather than a space.
                    out.push(' ');
                }
            }
            let trimmed = out.trim_end();
            out.truncate(trimmed.len());
            out
        })
        .collect()
}

/// Returns whether the alternate screen is active.
pub fn alternate_screen(term: &VtTerminal) -> bool {
    term.mode().contains(Mode::ALT_SCREEN)
}

/// Returns whether bracketed paste mode is active.
pub fn bracketed_paste(term: &VtTerminal) -> bool {
    term.mode().contains(Mode::BRACKETED_PASTE)
}

/// Returns whether application cursor keys mode is active.
pub fn application_cursor(term: &VtTerminal) -> bool {
    term.mode().contains(Mode::APP_CURSOR)
}

/// A cell colour resolved to what a renderer has to draw.
///
/// rio-vt's [`AnsiColor::Named`] covers more than the sixteen ANSI slots: the
/// terminal's own default foreground and background, the cursor colour, and a
/// dim variant of each base colour. Resolving them once here keeps the ratatui
/// widget and the debug-image renderer from drifting apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellColor {
    /// Fall back to the theme's default foreground or background.
    Default,
    /// Palette index, 0-255.
    Indexed(u8),
    /// Direct 24-bit colour.
    Rgb(u8, u8, u8),
}

/// Resolves a rio-vt colour into a [`CellColor`].
pub fn resolve_color(color: AnsiColor) -> CellColor {
    match color {
        AnsiColor::Indexed(index) => CellColor::Indexed(index),
        AnsiColor::Spec(rgb) => CellColor::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Named(named) => match named {
            // The terminal's own defaults. `Cursor` lands here because ratty
            // draws its own cursor rather than recolouring the cell.
            NamedColor::Foreground
            | NamedColor::Background
            | NamedColor::Cursor
            | NamedColor::LightForeground
            | NamedColor::DimForeground => CellColor::Default,
            // Dim variants resolve to their base slot: the DIM style flag
            // already carries the dimming, so folding it into the colour too
            // would double up.
            NamedColor::DimBlack
            | NamedColor::DimRed
            | NamedColor::DimGreen
            | NamedColor::DimYellow
            | NamedColor::DimBlue
            | NamedColor::DimMagenta
            | NamedColor::DimCyan
            | NamedColor::DimWhite => {
                let base = named as u32 - NamedColor::DimBlack as u32;
                u8::try_from(base).map_or(CellColor::Default, CellColor::Indexed)
            }
            // Everything left is one of the sixteen ANSI slots.
            named => {
                let index = named as u32;
                debug_assert!(index < 16, "unhandled NamedColor variant: {named:?}");
                u8::try_from(index).map_or(CellColor::Default, CellColor::Indexed)
            }
        },
    }
}

/// Foreground colour, background colour, and attribute flags for a cell.
pub fn cell_attributes(styles: &[Style], square: Square) -> (CellColor, CellColor, StyleFlags) {
    match square.content_tag() {
        ContentTag::Codepoint => {
            let style = styles
                .get(usize::from(square.style_id()))
                .copied()
                .unwrap_or_default();
            (
                resolve_color(style.fg),
                resolve_color(style.bg),
                style.flags,
            )
        }
        ContentTag::BgPalette => (
            CellColor::Default,
            CellColor::Indexed(square.bg_palette_index()),
            StyleFlags::empty(),
        ),
        ContentTag::BgRgb => {
            let (r, g, b) = square.bg_rgb();
            (
                CellColor::Default,
                CellColor::Rgb(r, g, b),
                StyleFlags::empty(),
            )
        }
    }
}

/// Returns the grid's interned style table, indexed by `Square::style_id`.
pub fn styles(term: &VtTerminal) -> &[Style] {
    term.grid.style_set.styles()
}

/// The xterm mouse reporting mode an application has requested.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolMode {
    /// Mouse reporting is off.
    #[default]
    None,
    /// Report press only (X10, `CSI ? 9 h`).
    Press,
    /// Report press and release (1000).
    PressRelease,
    /// Report press, release, and motion while a button is held (1002).
    ButtonMotion,
    /// Report press, release, and all motion (1003).
    AnyMotion,
}

/// The encoding used for mouse reports.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolEncoding {
    /// Single-printable-byte encoding.
    #[default]
    Default,
    /// UTF-8 encoding (1005).
    Utf8,
    /// SGR encoding (1006).
    Sgr,
}

/// Returns the active mouse reporting mode.
///
/// Checked most-permissive first: 1003 supersedes 1002, which supersedes 1000.
pub fn mouse_protocol_mode(term: &VtTerminal) -> MouseProtocolMode {
    let mode = term.mode();
    if mode.contains(Mode::MOUSE_MOTION) {
        MouseProtocolMode::AnyMotion
    } else if mode.contains(Mode::MOUSE_DRAG) {
        MouseProtocolMode::ButtonMotion
    } else if mode.contains(Mode::MOUSE_REPORT_CLICK) {
        MouseProtocolMode::PressRelease
    } else if mode.contains(Mode::MOUSE_REPORT_X10) {
        MouseProtocolMode::Press
    } else {
        MouseProtocolMode::None
    }
}

/// Returns the active mouse report encoding.
pub fn mouse_protocol_encoding(term: &VtTerminal) -> MouseProtocolEncoding {
    let mode = term.mode();
    if mode.contains(Mode::SGR_MOUSE) {
        MouseProtocolEncoding::Sgr
    } else if mode.contains(Mode::UTF8_MOUSE) {
        MouseProtocolEncoding::Utf8
    } else {
        MouseProtocolEncoding::Default
    }
}

/// Returns the active kitty keyboard enhancement flags.
pub fn kitty_keyboard_flags(term: &VtTerminal) -> u8 {
    term.keyboard_mode().bits()
}

/// Returns the cursor position as `(row, col)` in grid coordinates.
///
/// Unaffected by the scrollback offset, which is what the inline-object anchors
/// and mouse-wheel encoding want.
pub fn cursor_position(term: &VtTerminal) -> (u16, u16) {
    let pos = term.cursor().pos;
    (
        u16::try_from(pos.row.0.max(0)).unwrap_or(u16::MAX),
        u16::try_from(pos.col.0).unwrap_or(u16::MAX),
    )
}

/// Returns whether the cursor should be drawn.
///
/// Uses rio-vt's resolved cursor state, which folds in DECTCEM, the scrollback
/// offset (the cursor hides while scrolled into history), and the wide-cell
/// column snap. The raw `Mode::SHOW_CURSOR` bit carries none of that.
pub fn cursor_hidden(term: &VtTerminal) -> bool {
    term.cursor().content == CursorShape::Hidden
}

/// Returns how many rows the view is scrolled into history.
pub fn scrollback(term: &VtTerminal) -> usize {
    term.display_offset()
}

/// Scrolls the view `rows` rows into history, clamped to the available history.
pub fn set_scrollback(term: &mut VtTerminal, rows: usize) {
    let current = term.display_offset();
    if rows == current {
        return;
    }
    let delta = rows as i64 - current as i64;
    let delta = delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    term.scroll_display(Scroll::Delta(delta));
}

#[cfg(test)]
mod tests {
    use super::*;

    use rio_vt::crosswords::CrosswordsSize;
    use rio_vt::event::WindowId;
    use rio_vt::performer::handler::Processor;

    struct Harness {
        term: VtTerminal,
        processor: Processor,
        sink: TerminalEventSink,
    }

    impl Harness {
        fn new(rows: u16, cols: u16) -> Self {
            let sink = TerminalEventSink::default();
            let term = Crosswords::new(
                CrosswordsSize::new(usize::from(cols), usize::from(rows)),
                CursorShape::Block,
                sink.clone(),
                WindowId::from(0),
                0,
                1000,
            );
            Self {
                term,
                processor: Processor::default(),
                sink,
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            self.processor.advance(&mut self.term, bytes);
        }

        fn row_text(&self, row: u16) -> String {
            let Some(grid_row) = visible_row(&self.term, row) else {
                return String::new();
            };
            let _ = grid_row;
            let mut out = String::new();
            for col in 0..u16::try_from(self.term.columns()).unwrap_or(u16::MAX) {
                push_cell_text(&mut out, &self.term.grid, visible_pos(&self.term, row, col));
            }
            out.trim_end().to_string()
        }
    }

    /// Older rio-vt releases made `visible_rows` iterate the DECSTBM scroll
    /// region, so a widget built on it rendered the grid shifted up and
    /// truncated the bottom rows as soon as an application narrowed the region.
    /// Keep every reported row reachable so that bug cannot return here.
    #[test]
    fn every_row_stays_reachable_with_a_scroll_region_set() {
        let mut harness = Harness::new(10, 20);
        for line in 0..10 {
            harness.feed(format!("\x1b[{};1Hline{line}", line + 1).as_bytes());
        }

        harness.feed(b"\x1b[2;8r");

        assert_eq!(harness.term.screen_lines(), 10);
        for line in 0..10_u16 {
            assert!(
                visible_row(&harness.term, line).is_some(),
                "row {line} unreachable with a scroll region set"
            );
            assert_eq!(
                harness.row_text(line),
                format!("line{line}"),
                "row {line} holds the wrong grid line"
            );
        }
        assert!(visible_row(&harness.term, 10).is_none());
    }

    #[test]
    fn combining_marks_survive_in_cell_text() {
        let mut harness = Harness::new(3, 20);
        harness.feed("e\u{0301}X".as_bytes());

        let mut first = String::new();
        push_cell_text(
            &mut first,
            &harness.term.grid,
            visible_pos(&harness.term, 0, 0),
        );

        assert_eq!(first, "e\u{0301}");
        assert_eq!(first.chars().count(), 2);
        assert_eq!(harness.row_text(0), "e\u{0301}X");
    }

    #[test]
    fn wide_characters_skip_their_spacer_cells() {
        let mut harness = Harness::new(3, 10);
        harness.feed("你好ab".as_bytes());

        let row = visible_row(&harness.term, 0).expect("row 0");
        assert_eq!(row[Column(0)].c(), '你');
        assert!(matches!(row[Column(1)].wide(), Wide::Spacer));

        let mut spacer = String::new();
        push_cell_text(
            &mut spacer,
            &harness.term.grid,
            visible_pos(&harness.term, 0, 1),
        );
        assert!(spacer.is_empty(), "spacer cells must contribute no text");

        assert_eq!(harness.row_text(0), "你好ab");
    }

    #[test]
    fn cursor_hides_while_scrolled_into_history() {
        let mut harness = Harness::new(3, 20);
        for line in 0..10 {
            harness.feed(format!("row{line}\r\n").as_bytes());
        }
        assert!(!cursor_hidden(&harness.term));

        set_scrollback(&mut harness.term, 3);
        assert_eq!(scrollback(&harness.term), 3);
        assert!(
            cursor_hidden(&harness.term),
            "cursor must not be drawn over scrollback"
        );

        set_scrollback(&mut harness.term, 0);
        assert_eq!(scrollback(&harness.term), 0);
        assert!(!cursor_hidden(&harness.term));
    }

    #[test]
    fn cursor_hides_under_dectcem() {
        let mut harness = Harness::new(3, 20);
        harness.feed(b"\x1b[?25l");
        assert!(cursor_hidden(&harness.term));
        harness.feed(b"\x1b[?25h");
        assert!(!cursor_hidden(&harness.term));
    }

    #[test]
    fn scrollback_clamps_to_available_history() {
        let mut harness = Harness::new(3, 20);
        for line in 0..10 {
            harness.feed(format!("row{line}\r\n").as_bytes());
        }
        let history = harness.term.history_size();

        set_scrollback(&mut harness.term, history + 100);
        assert_eq!(scrollback(&harness.term), history);
    }

    #[test]
    fn resize_reflows_and_keeps_scrollback() {
        let mut harness = Harness::new(4, 20);
        for line in 0..12 {
            harness.feed(format!("row{line}\r\n").as_bytes());
        }
        assert!(harness.term.history_size() > 0);

        harness.term.resize(CrosswordsSize::new(40, 6));

        assert_eq!(harness.term.screen_lines(), 6);
        assert_eq!(harness.term.columns(), 40);
        assert!(harness.term.history_size() > 0);
        for line in 0..6_u16 {
            assert!(visible_row(&harness.term, line).is_some());
        }
    }

    /// [`visible_row`] indexes the grid at `row - display_offset`, which is only
    /// in range while the offset stays within the available history. A resize
    /// that shrinks history must not leave a stale offset behind and push the
    /// index past the ring.
    #[test]
    fn rows_stay_readable_after_resizing_while_scrolled_back() {
        for (cols, rows) in [(40, 3), (10, 12), (80, 24), (20, 4)] {
            let mut harness = Harness::new(6, 40);
            for line in 0..60 {
                harness.feed(format!("row{line}\r\n").as_bytes());
            }
            let history = harness.term.history_size();
            set_scrollback(&mut harness.term, history);

            harness.term.resize(CrosswordsSize::new(cols, rows));

            let screen_lines = u16::try_from(harness.term.screen_lines()).expect("screen lines");
            for line in 0..screen_lines {
                assert!(
                    visible_row(&harness.term, line).is_some(),
                    "row {line} unreadable after resizing to {cols}x{rows} while scrolled back"
                );
            }
            // And reading every cell must not index out of the ring.
            let _ = visible_row_texts(&harness.term);
        }
    }

    #[test]
    fn resize_resets_a_narrowed_scroll_region() {
        let mut harness = Harness::new(10, 20);
        harness.feed(b"\x1b[2;5r");
        harness.term.resize(CrosswordsSize::new(21, 10));

        for line in 0..10_u16 {
            assert!(visible_row(&harness.term, line).is_some());
        }
    }

    #[test]
    fn engine_replies_are_queued_for_write_back() {
        let mut harness = Harness::new(5, 20);
        harness.feed(b"\x1b[0c");
        harness.feed(b"\x1b[5n");
        harness.feed(b"\x1b[3;7H\x1b[6n");

        let replies = harness.sink.take_replies();
        assert_eq!(replies.len(), 3, "expected DA, DSR, and CPR replies");
        assert_eq!(replies[1], b"\x1b[0n");
        assert_eq!(replies[2], b"\x1b[3;7R");
        assert!(harness.sink.take_replies().is_empty(), "replies must drain");
    }

    /// rio-vt reports the engine's capabilities, not ratty's. Sixel and OSC 52
    /// must not survive to the PTY, or applications will emit payloads that
    /// silently go nowhere.
    #[test]
    fn primary_device_attributes_drop_unimplemented_capabilities() {
        let mut harness = Harness::new(5, 20);
        harness.feed(b"\x1b[0c");

        let replies = harness.sink.take_replies();
        assert_eq!(replies.len(), 1);
        let reply = String::from_utf8(replies[0].clone()).expect("utf-8 DA1 reply");

        let params = reply
            .strip_prefix("\x1b[?")
            .and_then(|rest| rest.strip_suffix('c'))
            .expect("DA1 shape preserved")
            .split(';')
            .collect::<Vec<_>>();

        assert!(!params.contains(&"4"), "sixel must not be advertised");
        assert!(!params.contains(&"52"), "OSC 52 must not be advertised");
        assert!(
            params.contains(&"62"),
            "the VT220 terminal class must survive"
        );
        assert!(params.contains(&"22"), "ANSI colour must survive");
    }

    #[test]
    fn secondary_device_attributes_report_rattys_version() {
        let mut harness = Harness::new(5, 20);
        harness.feed(b"\x1b[>0c");

        let replies = harness.sink.take_replies();
        assert_eq!(replies.len(), 1);
        assert_eq!(
            replies[0],
            format!("\x1b[>0;{};1c", encoded_version()).into_bytes()
        );
    }

    #[test]
    fn xtversion_reports_ratty_rather_than_rio() {
        let mut harness = Harness::new(5, 20);
        harness.feed(b"\x1b[>0q");

        let replies = harness.sink.take_replies();
        assert_eq!(replies.len(), 1);
        let reply = String::from_utf8(replies[0].clone()).expect("utf-8 XTVERSION reply");

        assert_eq!(
            reply,
            format!("\x1bP>|ratty {}\x1b\\", env!("CARGO_PKG_VERSION"))
        );
        assert!(!reply.contains("Rio"));
    }

    #[test]
    fn unrelated_replies_pass_through_untouched() {
        for reply in [
            "\x1b[0n",        // DSR
            "\x1b[3;7R",      // CPR
            "\x1b[?1u",       // kitty keyboard mode report
            "\x1b[?62;22c",   // DA1 with nothing to strip
            "\x1b[>0;1;2;3c", // DA2 with an unexpected field count
        ] {
            assert_eq!(rewrite_reply(reply), None, "{reply:?} should be untouched");
        }
    }

    #[test]
    fn encoded_version_matches_the_da2_weighting() {
        // Mirrors rio-vt's scheme: patch + minor*100 + major*10000.
        let expected = env!("CARGO_PKG_VERSION")
            .split('.')
            .rev()
            .enumerate()
            .map(|(index, part)| 100_usize.pow(index as u32) * part.parse::<usize>().unwrap_or(0))
            .sum::<usize>();
        assert_eq!(encoded_version(), expected);
    }

    #[test]
    fn mouse_protocol_mode_prefers_the_most_permissive() {
        let mut harness = Harness::new(5, 20);
        assert_eq!(mouse_protocol_mode(&harness.term), MouseProtocolMode::None);

        harness.feed(b"\x1b[?1000h");
        assert_eq!(
            mouse_protocol_mode(&harness.term),
            MouseProtocolMode::PressRelease
        );
        harness.feed(b"\x1b[?1002h");
        assert_eq!(
            mouse_protocol_mode(&harness.term),
            MouseProtocolMode::ButtonMotion
        );
        harness.feed(b"\x1b[?1003h");
        assert_eq!(
            mouse_protocol_mode(&harness.term),
            MouseProtocolMode::AnyMotion
        );

        harness.feed(b"\x1b[?1003l\x1b[?1002l\x1b[?1000l");
        assert_eq!(mouse_protocol_mode(&harness.term), MouseProtocolMode::None);
    }

    #[test]
    fn mouse_protocol_encoding_prefers_sgr() {
        let mut harness = Harness::new(5, 20);
        assert_eq!(
            mouse_protocol_encoding(&harness.term),
            MouseProtocolEncoding::Default
        );

        harness.feed(b"\x1b[?1005h");
        assert_eq!(
            mouse_protocol_encoding(&harness.term),
            MouseProtocolEncoding::Utf8
        );
        harness.feed(b"\x1b[?1006h");
        assert_eq!(
            mouse_protocol_encoding(&harness.term),
            MouseProtocolEncoding::Sgr
        );
    }

    #[test]
    fn kitty_keyboard_flags_map_each_mode_bit() {
        let mut harness = Harness::new(5, 20);
        assert_eq!(kitty_keyboard_flags(&harness.term), 0);

        for (requested, expected) in [(1_u8, 1_u8), (3, 3), (31, 31)] {
            harness.feed(format!("\x1b[>{requested}u").as_bytes());
            assert_eq!(
                kitty_keyboard_flags(&harness.term),
                expected,
                "flags {requested} did not round-trip"
            );
            harness.feed(b"\x1b[<1u");
        }
    }

    #[test]
    fn default_style_resolves_to_theme_defaults() {
        assert_eq!(
            resolve_color(AnsiColor::Named(NamedColor::Foreground)),
            CellColor::Default
        );
        assert_eq!(
            resolve_color(AnsiColor::Named(NamedColor::Background)),
            CellColor::Default
        );
        assert_eq!(
            resolve_color(AnsiColor::Named(NamedColor::Cursor)),
            CellColor::Default
        );
    }

    #[test]
    fn named_colors_resolve_to_their_palette_slots() {
        assert_eq!(
            resolve_color(AnsiColor::Named(NamedColor::Black)),
            CellColor::Indexed(0)
        );
        assert_eq!(
            resolve_color(AnsiColor::Named(NamedColor::LightWhite)),
            CellColor::Indexed(15)
        );
        // Dim variants fall back to their base slot; the DIM flag does the rest.
        assert_eq!(
            resolve_color(AnsiColor::Named(NamedColor::DimBlack)),
            CellColor::Indexed(0)
        );
        assert_eq!(
            resolve_color(AnsiColor::Named(NamedColor::DimWhite)),
            CellColor::Indexed(7)
        );
    }

    #[test]
    fn indexed_and_rgb_colors_pass_through() {
        assert_eq!(
            resolve_color(AnsiColor::Indexed(200)),
            CellColor::Indexed(200)
        );
        assert_eq!(
            resolve_color(AnsiColor::Spec(rio_vt::config::colors::ColorRgb {
                r: 1,
                g: 2,
                b: 3
            })),
            CellColor::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn cell_attributes_read_truecolor_and_flags() {
        let mut harness = Harness::new(3, 20);
        harness.feed(b"\x1b[1;3;4;7;38;2;10;20;30;48;5;99mX");

        let row = visible_row(&harness.term, 0).expect("row 0");
        let (fg, bg, flags) = cell_attributes(styles(&harness.term), row[Column(0)]);

        assert_eq!(fg, CellColor::Rgb(10, 20, 30));
        assert_eq!(bg, CellColor::Indexed(99));
        assert!(flags.contains(StyleFlags::BOLD));
        assert!(flags.contains(StyleFlags::ITALIC));
        assert!(flags.intersects(StyleFlags::ALL_UNDERLINES));
        assert!(flags.contains(StyleFlags::INVERSE));
    }

    /// rio-vt models `modifyOtherKeys` itself now, so ratty reads it from the
    /// engine instead of sniffing PTY bytes. Level 0 means disabled and must
    /// report `None`, since the key encoder treats any `Some` as enabled.
    #[test]
    fn modify_other_keys_comes_from_the_engine() {
        let mut harness = Harness::new(5, 20);
        assert_eq!(harness.term.modify_other_keys(), None);

        harness.feed(b"\x1b[>4;2m");
        assert_eq!(harness.term.modify_other_keys(), Some(2));

        harness.feed(b"\x1b[>4;0m");
        assert_eq!(
            harness.term.modify_other_keys(),
            None,
            "level 0 disables the mode"
        );

        harness.feed(b"\x1b[>4;1m");
        assert_eq!(harness.term.modify_other_keys(), Some(1));
        harness.feed(b"\x1b[>4m");
        assert_eq!(harness.term.modify_other_keys(), None);
    }

    /// Split across PTY reads, which the old byte sniffer could not handle.
    #[test]
    fn modify_other_keys_survives_a_split_sequence() {
        let sequence = b"\x1b[>4;2m";
        for split in 1..sequence.len() {
            let mut harness = Harness::new(5, 20);
            harness.feed(&sequence[..split]);
            harness.feed(&sequence[split..]);
            assert_eq!(
                harness.term.modify_other_keys(),
                Some(2),
                "split at {split} was missed"
            );
        }
    }

    #[test]
    fn x10_mouse_reporting_is_recognized() {
        let mut harness = Harness::new(5, 20);
        harness.feed(b"\x1b[?9h");
        assert_eq!(mouse_protocol_mode(&harness.term), MouseProtocolMode::Press);
        harness.feed(b"\x1b[?9l");
        assert_eq!(mouse_protocol_mode(&harness.term), MouseProtocolMode::None);
    }
}

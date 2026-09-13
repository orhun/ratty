use compact_str::CompactString;
use unicode_width::UnicodeWidthChar as _;

const IS_WIDE: u8 = 0b1000_0000;
const IS_WIDE_CONTINUATION: u8 = 0b0100_0000;
// Bound both per-cell storage and contextual Unicode analysis. This leaves
// ample space for emoji and combining sequences while preventing arbitrary
// PTY output from accumulating unbounded work in one cell.
pub(crate) const MAX_CONTENT_BYTES: usize = 4096;

/// Represents a single terminal cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    // Keep short text inline while retaining clusters up to MAX_CONTENT_BYTES.
    // On 64-bit targets this makes a cell 40 bytes, with up to
    // 24 bytes of text stored without a heap allocation.
    contents: CompactString,
    flags: u8,
    attrs: crate::attrs::Attrs,
}

impl Cell {
    pub(crate) fn new() -> Self {
        Self {
            contents: Default::default(),
            flags: 0,
            attrs: crate::attrs::Attrs::default(),
        }
    }

    pub(crate) fn set(&mut self, c: char, a: crate::attrs::Attrs) {
        self.contents.clear();
        self.contents.push(c);
        self.flags = 0;
        // Start with the character's width. The screen updates this when
        // subsequent characters extend the grapheme cluster.
        self.set_wide(c.width().unwrap_or(1) > 1);
        self.attrs = a;
    }

    pub(crate) fn can_append(&self, c: char) -> bool {
        self.contents.len().max(1) + c.len_utf8() <= MAX_CONTENT_BYTES
    }

    pub(crate) fn append(&mut self, c: char) {
        if !self.can_append(c) {
            return;
        }
        if self.contents.is_empty() {
            self.contents.push(' ');
        }
        self.contents.push(c);
    }

    pub(crate) fn clear(&mut self, attrs: crate::attrs::Attrs) {
        self.contents.clear();
        self.flags = 0;
        self.attrs = attrs;
    }

    /// Returns the text contents of the cell.
    ///
    /// Includes all characters in the cell's grapheme cluster, such as
    /// combining marks, spacing vowel signs, and joined emoji sequences.
    /// Cell text is limited to 4096 UTF-8 bytes; excess zero-width marks
    /// are discarded, and a following spacing character starts a new cell.
    #[must_use]
    pub fn contents(&self) -> &str {
        self.contents.as_str()
    }

    /// Returns whether the cell contains any text data.
    #[must_use]
    pub fn has_contents(&self) -> bool {
        !self.contents.is_empty()
    }

    /// Returns whether the text data in the cell represents a wide character.
    #[must_use]
    pub fn is_wide(&self) -> bool {
        self.flags & IS_WIDE != 0
    }

    /// Returns whether the cell contains the second half of a wide character
    /// (in other words, whether the previous cell in the row contains a wide
    /// character)
    #[must_use]
    pub fn is_wide_continuation(&self) -> bool {
        self.flags & IS_WIDE_CONTINUATION != 0
    }

    pub(crate) fn set_wide(&mut self, wide: bool) {
        if wide {
            self.flags |= IS_WIDE;
        } else {
            self.flags &= !IS_WIDE;
        }
    }

    pub(crate) fn set_wide_continuation(&mut self, wide: bool) {
        if wide {
            self.flags |= IS_WIDE_CONTINUATION;
        } else {
            self.flags &= !IS_WIDE_CONTINUATION;
        }
    }

    pub(crate) fn attrs(&self) -> &crate::attrs::Attrs {
        &self.attrs
    }

    /// Returns the foreground color of the cell.
    #[must_use]
    pub fn fgcolor(&self) -> crate::Color {
        self.attrs.fgcolor
    }

    /// Returns the background color of the cell.
    #[must_use]
    pub fn bgcolor(&self) -> crate::Color {
        self.attrs.bgcolor
    }

    /// Returns whether the cell should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether the cell should be rendered with the dim text
    /// attribute.
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether the cell should be rendered with the italic text
    /// attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether the cell should be rendered with the underlined text
    /// attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether the cell should be rendered with the inverse text
    /// attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }

    /// Returns the blink attribute the cell should be rendered with.
    ///
    /// ratty-vt addition: upstream vt100 does not parse SGR 5/6/25.
    #[must_use]
    pub fn blink(&self) -> crate::Blink {
        self.attrs.blink()
    }

    /// Returns whether the cell should be concealed (SGR 8).
    ///
    /// ratty-vt addition.
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.attrs.hidden()
    }

    /// Returns whether the cell should be struck through (SGR 9).
    ///
    /// ratty-vt addition.
    #[must_use]
    pub fn strikeout(&self) -> bool {
        self.attrs.strikeout()
    }

    /// Returns the cell's underline color (SGR 58); `Color::Default` means
    /// the foreground color.
    ///
    /// ratty-vt addition.
    #[must_use]
    pub fn underline_color(&self) -> crate::Color {
        self.attrs.underline_color
    }
}

#[cfg(test)]
mod ratty_cell_tests {
    use super::Cell;
    use crate::{Parser, Screen};

    fn assert_same_screen(actual: &Screen, expected: &Screen) {
        assert_eq!(actual.contents(), expected.contents());
        assert_eq!(actual.cursor_position(), expected.cursor_position());
        let (rows, cols) = expected.size();
        for row in 0..rows {
            assert_eq!(actual.row_wrapped(row), expected.row_wrapped(row));
            for col in 0..cols {
                assert_eq!(actual.cell(row, col), expected.cell(row, col));
            }
        }
    }

    #[test]
    fn long_graphemes_preserve_text_cursor_alignment_and_formatted_output() {
        for (cluster, width) in [
            ("👨‍👩‍👧‍👦".to_owned(), 2),
            ("👩🏻‍❤️‍👨🏻".to_owned(), 2),
            (format!("e{}", "\u{301}".repeat(12)), 1),
            (format!("e{}", "\u{301}".repeat(128)), 1),
        ] {
            let input = format!("\x1b[1;38;2;12;34;56m{cluster}\x1b[mx");
            // PTY reads may split both UTF-8 code points and the cluster.
            for chunk_size in [input.len(), 1] {
                let mut parser = Parser::new(2, 10, 0);
                for chunk in input.as_bytes().chunks(chunk_size) {
                    parser.process(chunk);
                }
                let screen = parser.screen();
                let cell = screen.cell(0, 0).unwrap();
                assert_eq!(cell.contents(), cluster);
                assert_eq!(cell.is_wide(), width == 2);
                if width == 2 {
                    assert!(screen.cell(0, 1).unwrap().is_wide_continuation());
                }
                assert_eq!(screen.cell(0, width).unwrap().contents(), "x");
                assert_eq!(screen.cursor_position(), (0, width + 1));
                assert_eq!(screen.contents(), format!("{cluster}x"));

                let mut replay = Parser::new(2, 10, 0);
                replay.process(&screen.contents_formatted());
                assert_same_screen(replay.screen(), screen);
            }
        }
    }

    #[test]
    fn oversized_clusters_bound_storage_and_preserve_following_cells() {
        for (base, mark, width) in [
            ("a", '\u{301}', 1),
            ("a", '\u{fe0f}', 1),
            ("a", '\u{200d}', 1),
            ("a", '\u{fe01}', 1),
            ("👩", '\u{301}', 2),
        ] {
            let mut parser = Parser::new(2, 10, 0);
            parser.process(format!("{base}{}x", mark.to_string().repeat(20_000)).as_bytes());
            let screen = parser.screen();
            let expected_marks = (super::MAX_CONTENT_BYTES - base.len()) / mark.len_utf8();
            assert_eq!(
                screen.cell(0, 0).unwrap().contents(),
                format!("{base}{}", mark.to_string().repeat(expected_marks))
            );
            assert_eq!(screen.cell(0, width).unwrap().contents(), "x");
            assert_eq!(screen.cursor_position(), (0, width + 1));
            let mut replay = Parser::new(2, 10, 0);
            replay.process(&screen.contents_formatted());
            assert_same_screen(replay.screen(), screen);
        }

        // Once the stored prefix is full, a spacing emoji starts a new
        // cell even when an overflowing ZWJ would otherwise join it.
        let mut parser = Parser::new(2, 10, 0);
        parser.process(format!("👩{}\u{200d}🔬x", "\u{301}".repeat(20_000)).as_bytes());
        assert_eq!(parser.screen().cell(0, 2).unwrap().contents(), "🔬");
        assert_eq!(parser.screen().cell(0, 4).unwrap().contents(), "x");
        assert_eq!(parser.screen().cursor_position(), (0, 5));
    }

    #[test]
    fn boundary_probes_match_complete_grapheme_segmentation() {
        use unicode_segmentation::UnicodeSegmentation as _;

        for previous in [
            "a".to_owned(),
            "नि".to_owned(),
            "क्".to_owned(),
            "❤️".to_owned(),
            "🇯".to_owned(),
            "🇯🇵".to_owned(),
            "👩🏻‍❤️‍".to_owned(),
            format!("👩{}\u{200d}", "\u{301}".repeat(512)),
            format!("क्{}", "\u{301}".repeat(512)),
        ] {
            for next in ['x', '\u{301}', '\u{fe0f}', '\u{200d}', 'ि', 'क', '👨', '🇵'] {
                assert_eq!(
                    crate::screen::clusters_with(&previous, next),
                    format!("{previous}{next}").graphemes(true).count() == 1,
                    "boundary before {next:?} after {previous:?}"
                );
            }
        }
    }

    #[test]
    fn ascii_boundary_shortcut_matches_unicode_segmentation() {
        use unicode_segmentation::UnicodeSegmentation as _;

        for previous in b' '..=b'~' {
            for next in b' '..=b'~' {
                let text = String::from_utf8(vec![previous, next]).unwrap();
                assert!(!crate::screen::clusters_with(&text[..1], char::from(next)));
                assert_eq!(text.graphemes(true).count(), 2);
            }
        }
        // Non-ASCII prefixes can contain earlier boundaries; only the appended
        // boundary matters. Marks with ASCII-looking low bytes must not take
        // the shortcut, and CR/LF retains its special joining rule.
        for previous in ["e\u{301}a", "👩\u{200d}💻x", "\u{0600}a", "a", "\r"] {
            for next in ['x', ' ', '\u{034f}', '\u{301}', '\n', '界'] {
                let joined = format!("{previous}{next}");
                let boundary = joined
                    .grapheme_indices(true)
                    .any(|(index, _)| index == previous.len());
                assert_eq!(
                    crate::screen::clusters_with(previous, next),
                    !boundary,
                    "{previous:?} + {next:?}"
                );
            }
        }
    }

    #[test]
    fn zero_width_marks_after_cursor_moves_reconcile_cluster_width() {
        for (rows, cols) in [(1, 2), (2, 4)] {
            let mut parser = Parser::new(rows, cols, 0);
            parser.process("❤\x1b[2G\u{fe0f}".as_bytes());
            let screen = parser.screen();
            assert_eq!(screen.cell(0, 0).unwrap().contents(), "❤️");
            assert!(screen.cell(0, 0).unwrap().is_wide());
            assert!(screen.cell(0, 1).unwrap().is_wide_continuation());
            assert_eq!(screen.cursor_position(), (0, 2));
            let mut replay = Parser::new(rows, cols, 0);
            replay.process(&screen.contents_formatted());
            assert_same_screen(replay.screen(), screen);
            if cols > 2 {
                parser.process(b"x");
                assert_eq!(parser.screen().cell(0, 2).unwrap().contents(), "x");
                assert_eq!(parser.screen().cursor_position(), (0, 3));
            }
        }

        // Widening over an existing wide glyph clears its orphaned half.
        let mut parser = Parser::new(2, 4, 0);
        parser.process("❤界\x1b[2G\u{fe0f}".as_bytes());
        assert!(parser.screen().cell(0, 0).unwrap().is_wide());
        assert!(parser.screen().cell(0, 1).unwrap().is_wide_continuation());
        assert!(!parser.screen().cell(0, 2).unwrap().is_wide_continuation());
        assert!(!parser.screen().cell(0, 2).unwrap().has_contents());

        // A mark attached across a wrapped boundary cannot widen the last
        // column, and must not move the cursor or consume the following row.
        let mut parser = Parser::new(3, 2, 0);
        parser.process("a❤x\x08\u{fe0f}".as_bytes());
        assert_eq!(parser.screen().cell(0, 1).unwrap().contents(), "❤️");
        assert!(!parser.screen().cell(0, 1).unwrap().is_wide());
        assert_eq!(parser.screen().cell(1, 0).unwrap().contents(), "x");
        assert_eq!(parser.screen().cursor_position(), (1, 0));
        let mut replay = Parser::new(3, 2, 0);
        replay.process(&parser.screen().contents_formatted());
        assert_same_screen(replay.screen(), parser.screen());
    }

    #[test]
    fn replacing_long_graphemes_updates_diffs_and_clears_continuations() {
        let mut parser = Parser::new(2, 10, 0);
        parser.process("👨‍👩‍👧‍👦x".as_bytes());
        let before = parser.screen().clone();
        let mut replay = Parser::new(2, 10, 0);
        replay.process(&before.contents_formatted());

        // Only the end differs, past the old cell's fixed storage limit.
        parser.process("\r👨‍👩‍👧‍👧x".as_bytes());
        assert_eq!(before.cell(0, 0).unwrap().contents(), "👨‍👩‍👧‍👦");
        assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "👨‍👩‍👧‍👧");
        assert_ne!(parser.screen().cell(0, 0), before.cell(0, 0));
        replay.process(&parser.screen().contents_diff(&before));
        assert_same_screen(replay.screen(), parser.screen());

        // A narrow replacement must reset the old wide-cell state.
        parser.process(b"\rz");
        assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "z");
        assert!(!parser.screen().cell(0, 0).unwrap().is_wide());
        assert!(!parser.screen().cell(0, 1).unwrap().is_wide_continuation());
        parser.process(b"\r\x1b[2K");
        assert!(!parser.screen().cell(0, 0).unwrap().has_contents());
        assert!(!parser.screen().cell(0, 1).unwrap().has_contents());
    }

    #[test]
    fn short_cell_text_stays_inline() {
        let mut cell = Cell::new();
        cell.set('e', Default::default());
        cell.append('\u{301}');
        assert_eq!(cell.contents(), "e\u{301}");
        assert!(!cell.contents.is_heap_allocated());
    }
}

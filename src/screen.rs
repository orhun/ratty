//! Ratty's view of the terminal screen.
//!
//! [`fux_vt`] models the screen and its history but leaves the scrollback
//! offset to its host: reading history is a window onto it, never a change
//! to the terminal. [`ScreenView`] pairs the screen with the offset ratty
//! keeps, so renderers, selection and inline objects read the rows the user
//! is looking at.

use std::ops::Deref;

use fux_vt::{CellRef, Row, Screen};

/// Kitty graphics Unicode placeholder (U+10EEEE). A cell starting with it
/// marks where an image goes; its diacritics carry the image row and column.
pub const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// The terminal screen as the user sees it: scrolled back by
/// [`scrollback`](Self::scrollback) rows into history.
///
/// Dereferences to [`Screen`] for everything that does not depend on the
/// scrollback offset (size, modes, the cursor's logical position).
#[derive(Clone, Copy)]
pub struct ScreenView<'a> {
    screen: &'a Screen,
    scrollback: usize,
}

impl<'a> ScreenView<'a> {
    /// A view of `screen` scrolled back `scrollback` rows, clamped to the
    /// history the current screen holds (the alternate screen holds none).
    #[must_use]
    pub fn new(screen: &'a Screen, scrollback: usize) -> Self {
        Self {
            screen,
            scrollback: scrollback.min(screen.history_len()),
        }
    }

    /// Returns how many rows the view is scrolled back into history.
    #[must_use]
    pub fn scrollback(&self) -> usize {
        self.scrollback
    }

    /// Returns the row drawn at `row`, counting from the top of the view.
    #[must_use]
    pub fn visible_row(&self, row: u16) -> Option<Row<'a>> {
        let (rows, cols) = self.screen.size();
        self.screen.window(self.scrollback, rows, cols).row(row)
    }

    /// Returns the cell drawn at `row`, `col` of the view.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<CellRef<'a>> {
        self.visible_row(row)?.cell(usize::from(col))
    }

    /// Returns the cell a renderer should draw the cursor in, as `(row, col)`.
    ///
    /// [`Screen::cursor_position`] reports the logical position, whose column
    /// equals the width while a character drawn in the last column waits to
    /// wrap. This clamps that column onto the grid and, when the cursor sits
    /// on the second half of a wide character, moves it to the first half,
    /// so the cell always exists and owns its glyph.
    #[must_use]
    pub fn display_cursor_position(&self) -> (u16, u16) {
        let (row, col) = self.screen.cursor_position();
        let (_, cols) = self.screen.size();
        let mut col = col.min(cols.saturating_sub(1));
        if self
            .screen
            .cell(row, col)
            .is_some_and(|cell| cell.is_wide_continuation())
        {
            col = col.saturating_sub(1);
        }
        (row, col)
    }

    /// Returns whether a renderer should leave the cursor undrawn: hidden by
    /// DECTCEM, or the view scrolled back, as the cursor belongs to the live
    /// screen and must not be painted over history.
    #[must_use]
    pub fn cursor_hidden(&self) -> bool {
        self.screen.hide_cursor() || self.scrollback > 0
    }

    /// Returns each row of the view as text, trailing blanks trimmed.
    #[must_use]
    pub fn row_texts(&self) -> Vec<String> {
        let (rows, _) = self.screen.size();
        (0..rows)
            .map(|row| {
                let mut text = String::new();
                if let Some(row) = self.visible_row(row) {
                    for cell in row.cells().filter(|c| !c.is_wide_continuation()) {
                        text.push_str(if cell.has_contents() {
                            cell.contents()
                        } else {
                            " "
                        });
                    }
                }
                text.truncate(text.trim_end().len());
                text
            })
            .collect()
    }
}

impl Deref for ScreenView<'_> {
    type Target = Screen;

    fn deref(&self) -> &Screen {
        self.screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fux_vt::Parser;

    fn parse(rows: u16, cols: u16, input: &[u8]) -> Parser {
        let mut parser = Parser::new(rows, cols, 20).expect("parser");
        parser.process(input).expect("process");
        parser
    }

    #[test]
    fn scrolling_back_shows_history_and_clamps_to_it() {
        let mut input = Vec::new();
        for line in 0..12 {
            input.extend_from_slice(format!("row{line}\r\n").as_bytes());
        }
        let parser = parse(3, 10, &input);
        let screen = parser.screen();
        assert_eq!(
            ScreenView::new(screen, 0).row_texts(),
            ["row10", "row11", ""]
        );
        assert_eq!(
            ScreenView::new(screen, 2).row_texts(),
            ["row8", "row9", "row10"]
        );
        let top = ScreenView::new(screen, usize::MAX);
        assert_eq!(top.scrollback(), 10);
        assert_eq!(top.row_texts(), ["row0", "row1", "row2"]);
        assert!(top.visible_row(3).is_none());
        assert_eq!(top.cell(0, 0).map(|cell| cell.contents()), Some("r"));
    }

    #[test]
    fn display_cursor_clamps_a_pending_wrap_and_snaps_off_wide_halves() {
        let parser = parse(2, 4, b"abcd");
        let view = ScreenView::new(parser.screen(), 0);
        assert_eq!(view.cursor_position(), (0, 4));
        assert_eq!(view.display_cursor_position(), (0, 3));

        let parser = parse(2, 6, "\u{4f60}\x1b[1;2H".as_bytes());
        let view = ScreenView::new(parser.screen(), 0);
        assert_eq!(view.cursor_position(), (0, 1));
        assert_eq!(view.display_cursor_position(), (0, 0));
    }

    #[test]
    fn cursor_hides_under_dectcem_and_while_scrolled_back() {
        let mut input = b"\x1b[?25l".to_vec();
        let parser_hidden = parse(3, 20, &input);
        assert!(ScreenView::new(parser_hidden.screen(), 0).cursor_hidden());
        input.clear();
        for line in 0..10 {
            input.extend_from_slice(format!("row{line}\r\n").as_bytes());
        }
        let parser = parse(3, 20, &input);
        assert!(!ScreenView::new(parser.screen(), 0).cursor_hidden());
        assert!(ScreenView::new(parser.screen(), 3).cursor_hidden());
    }
}

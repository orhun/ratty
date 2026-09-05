use crate::ratty_vt::term::BufWrite as _;

/// A single row of the terminal grid.
///
/// ratty-vt addition: upstream keeps rows private and hands out cells one at a
/// time through `Screen::cell`. Renderers that walk the whole screen every
/// frame want to borrow a row once and index it.
#[derive(Clone, Debug)]
pub struct Row {
    cells: Vec<crate::ratty_vt::Cell>,
    wrapped: bool,
    // ratty-vt: see `has_kitty_placeholder`.
    kitty_placeholder: bool,
}

impl Row {
    pub(crate) fn new(cols: u16) -> Self {
        Self {
            cells: vec![crate::ratty_vt::Cell::new(); usize::from(cols)],
            wrapped: false,
            kitty_placeholder: false,
        }
    }

    fn cols(&self) -> u16 {
        self.cells
            .len()
            .try_into()
            // we limit the number of cols to a u16 (see Size)
            .unwrap()
    }

    pub(crate) fn clear(&mut self, attrs: crate::ratty_vt::attrs::Attrs) {
        for cell in &mut self.cells {
            cell.clear(attrs);
        }
        self.wrapped = false;
        self.kitty_placeholder = false;
    }

    /// Returns whether a kitty graphics Unicode placeholder (U+10EEEE) may be
    /// present in this row.
    ///
    /// ratty-vt addition. This is a conservative hint for renderers that map
    /// placeholders back to images: `false` means the row holds none, so the
    /// per-cell scan can be skipped; `true` means one was written since the
    /// row was last cleared, and may be stale if it has since been
    /// overwritten cell by cell.
    #[must_use]
    pub fn has_kitty_placeholder(&self) -> bool {
        self.kitty_placeholder
    }

    pub(crate) fn mark_kitty_placeholder(&mut self) {
        self.kitty_placeholder = true;
    }

    // ratty-vt: reflow support.
    pub(crate) fn from_cells(cells: Vec<crate::ratty_vt::Cell>, wrapped: bool) -> Self {
        Self {
            cells,
            wrapped,
            kitty_placeholder: false,
        }
    }

    pub(crate) fn is_blank(&self) -> bool {
        let blank = crate::ratty_vt::Cell::new();
        self.cells.iter().all(|cell| cell == &blank)
    }

    /// Iterates over the cells in the row, left to right.
    ///
    /// ratty-vt: public.
    pub fn cells(&self) -> impl Iterator<Item = &crate::ratty_vt::Cell> {
        self.cells.iter()
    }

    /// Returns the cell at `col`, if the row has that many columns.
    ///
    /// ratty-vt: public.
    #[must_use]
    pub fn get(&self, col: u16) -> Option<&crate::ratty_vt::Cell> {
        self.cells.get(usize::from(col))
    }

    pub(crate) fn get_mut(&mut self, col: u16) -> Option<&mut crate::ratty_vt::Cell> {
        self.cells.get_mut(usize::from(col))
    }

    pub(crate) fn insert(&mut self, i: u16, cell: crate::ratty_vt::Cell) {
        self.cells.insert(usize::from(i), cell);
        self.wrapped = false;
    }

    pub(crate) fn remove(&mut self, i: u16) {
        self.clear_wide(i);
        self.cells.remove(usize::from(i));
        self.wrapped = false;
    }

    pub(crate) fn erase(&mut self, i: u16, attrs: crate::ratty_vt::attrs::Attrs) {
        let wide = self.cells[usize::from(i)].is_wide();
        self.clear_wide(i);
        self.cells[usize::from(i)].clear(attrs);
        if i == self.cols() - if wide { 2 } else { 1 } {
            self.wrapped = false;
        }
    }

    pub(crate) fn truncate(&mut self, len: u16) {
        self.cells.truncate(usize::from(len));
        self.wrapped = false;
        let last_cell = &mut self.cells[usize::from(len) - 1];
        if last_cell.is_wide() {
            last_cell.clear(*last_cell.attrs());
        }
    }

    pub(crate) fn resize(&mut self, len: u16, cell: crate::ratty_vt::Cell) {
        self.cells.resize(usize::from(len), cell);
        self.wrapped = false;
    }

    pub(crate) fn wrap(&mut self, wrap: bool) {
        self.wrapped = wrap;
    }

    /// Returns whether the text in this row continues onto the next row.
    ///
    /// ratty-vt: public.
    #[must_use]
    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    pub(crate) fn clear_wide(&mut self, col: u16) {
        let cell = &self.cells[usize::from(col)];
        let other = if cell.is_wide() {
            &mut self.cells[usize::from(col + 1)]
        } else if cell.is_wide_continuation() {
            &mut self.cells[usize::from(col - 1)]
        } else {
            return;
        };
        other.clear(*other.attrs());
    }

    pub(crate) fn write_contents(
        &self,
        contents: &mut String,
        start: u16,
        width: u16,
        wrapping: bool,
    ) {
        let mut prev_was_wide = false;

        let mut prev_col = start;
        for (col, cell) in self
            .cells()
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            if cell.has_contents() {
                for _ in 0..(col - prev_col) {
                    contents.push(' ');
                }
                prev_col += col - prev_col;

                contents.push_str(cell.contents());
                prev_col += if cell.is_wide() { 2 } else { 1 };
            }
        }
        if prev_col == start && wrapping {
            contents.push('\n');
        }
    }

    pub(crate) fn write_contents_formatted(
        &self,
        contents: &mut Vec<u8>,
        start: u16,
        width: u16,
        row: u16,
        wrapping: bool,
        prev_pos: Option<crate::ratty_vt::grid::Pos>,
        prev_attrs: Option<crate::ratty_vt::attrs::Attrs>,
    ) -> (crate::ratty_vt::grid::Pos, crate::ratty_vt::attrs::Attrs) {
        let mut prev_was_wide = false;
        let default_cell = crate::ratty_vt::Cell::new();

        let mut prev_pos = prev_pos.unwrap_or_else(|| {
            if wrapping {
                crate::ratty_vt::grid::Pos {
                    row: row - 1,
                    col: self.cols(),
                }
            } else {
                crate::ratty_vt::grid::Pos { row, col: start }
            }
        });
        let mut prev_attrs = prev_attrs.unwrap_or_default();

        let first_cell = &self.cells[usize::from(start)];
        if wrapping && first_cell == &default_cell {
            let default_attrs = default_cell.attrs();
            if &prev_attrs != default_attrs {
                default_attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *default_attrs;
            }
            contents.push(b' ');
            crate::ratty_vt::term::Backspace.write_buf(contents);
            crate::ratty_vt::term::EraseChar::new(1).write_buf(contents);
            prev_pos = crate::ratty_vt::grid::Pos { row, col: 0 };
        }

        let mut erase: Option<(u16, &crate::ratty_vt::attrs::Attrs)> = None;
        let mut last_written: Option<&str> = None;
        for (col, cell) in self
            .cells()
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            let pos = crate::ratty_vt::grid::Pos { row, col };

            if let Some((prev_col, attrs)) = erase {
                if cell.has_contents() || cell.attrs() != attrs {
                    let new_pos = crate::ratty_vt::grid::Pos { row, col: prev_col };
                    if wrapping && prev_pos.row + 1 == new_pos.row && prev_pos.col >= self.cols() {
                        if new_pos.col > 0 {
                            contents.extend(" ".repeat(usize::from(new_pos.col)).as_bytes());
                        } else {
                            contents.extend(b" ");
                            crate::ratty_vt::term::Backspace.write_buf(contents);
                        }
                    } else {
                        crate::ratty_vt::term::MoveFromTo::new(prev_pos, new_pos)
                            .write_buf(contents);
                    }
                    prev_pos = new_pos;
                    if &prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = *attrs;
                    }
                    crate::ratty_vt::term::EraseChar::new(pos.col - prev_col).write_buf(contents);
                    erase = None;
                }
            }

            if cell != &default_cell {
                let attrs = cell.attrs();
                if cell.has_contents() {
                    if pos != prev_pos {
                        if !wrapping
                            || prev_pos.row + 1 != pos.row
                            || prev_pos.col < self.cols() - u16::from(cell.is_wide())
                            || pos.col != 0
                        {
                            crate::ratty_vt::term::MoveFromTo::new(prev_pos, pos)
                                .write_buf(contents);
                        }
                        prev_pos = pos;
                    }

                    if &prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = *attrs;
                    }

                    cluster_break(contents, &mut last_written, pos, prev_pos, cell.contents());
                    prev_pos.col += if cell.is_wide() { 2 } else { 1 };
                    let cell_contents = cell.contents();
                    contents.extend(cell_contents.as_bytes());
                } else if erase.is_none() {
                    erase = Some((pos.col, attrs));
                }
            }
        }
        if let Some((prev_col, attrs)) = erase {
            let new_pos = crate::ratty_vt::grid::Pos { row, col: prev_col };
            if wrapping && prev_pos.row + 1 == new_pos.row && prev_pos.col >= self.cols() {
                if new_pos.col > 0 {
                    contents.extend(" ".repeat(usize::from(new_pos.col)).as_bytes());
                } else {
                    contents.extend(b" ");
                    crate::ratty_vt::term::Backspace.write_buf(contents);
                }
            } else {
                crate::ratty_vt::term::MoveFromTo::new(prev_pos, new_pos).write_buf(contents);
            }
            prev_pos = new_pos;
            if &prev_attrs != attrs {
                attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *attrs;
            }
            crate::ratty_vt::term::ClearRowForward.write_buf(contents);
        }

        (prev_pos, prev_attrs)
    }

    // while it's true that most of the logic in this is identical to
    // write_contents_formatted, i can't figure out how to break out the
    // common parts without making things noticeably slower.
    pub(crate) fn write_contents_diff(
        &self,
        contents: &mut Vec<u8>,
        prev: &Self,
        start: u16,
        width: u16,
        row: u16,
        wrapping: bool,
        prev_wrapping: bool,
        mut prev_pos: crate::ratty_vt::grid::Pos,
        mut prev_attrs: crate::ratty_vt::attrs::Attrs,
    ) -> (crate::ratty_vt::grid::Pos, crate::ratty_vt::attrs::Attrs) {
        let mut prev_was_wide = false;

        let first_cell = &self.cells[usize::from(start)];
        let prev_first_cell = &prev.cells[usize::from(start)];
        if wrapping
            && !prev_wrapping
            && first_cell == prev_first_cell
            && prev_pos.row + 1 == row
            && prev_pos.col >= self.cols() - u16::from(prev_first_cell.is_wide())
        {
            let first_cell_attrs = first_cell.attrs();
            if &prev_attrs != first_cell_attrs {
                first_cell_attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *first_cell_attrs;
            }
            let mut cell_contents = prev_first_cell.contents();
            let need_erase = if cell_contents.is_empty() {
                cell_contents = " ";
                true
            } else {
                false
            };
            contents.extend(cell_contents.as_bytes());
            crate::ratty_vt::term::Backspace.write_buf(contents);
            if prev_first_cell.is_wide() {
                crate::ratty_vt::term::Backspace.write_buf(contents);
            }
            if need_erase {
                crate::ratty_vt::term::EraseChar::new(1).write_buf(contents);
            }
            prev_pos = crate::ratty_vt::grid::Pos { row, col: 0 };
        }

        let mut erase: Option<(u16, &crate::ratty_vt::attrs::Attrs)> = None;
        let mut last_written: Option<&str> = None;
        for (col, (cell, prev_cell)) in self
            .cells()
            .zip(prev.cells())
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            let pos = crate::ratty_vt::grid::Pos { row, col };

            if let Some((prev_col, attrs)) = erase {
                if cell.has_contents() || cell.attrs() != attrs {
                    let new_pos = crate::ratty_vt::grid::Pos { row, col: prev_col };
                    if wrapping && prev_pos.row + 1 == new_pos.row && prev_pos.col >= self.cols() {
                        if new_pos.col > 0 {
                            contents.extend(" ".repeat(usize::from(new_pos.col)).as_bytes());
                        } else {
                            contents.extend(b" ");
                            crate::ratty_vt::term::Backspace.write_buf(contents);
                        }
                    } else {
                        crate::ratty_vt::term::MoveFromTo::new(prev_pos, new_pos)
                            .write_buf(contents);
                    }
                    prev_pos = new_pos;
                    if &prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = *attrs;
                    }
                    crate::ratty_vt::term::EraseChar::new(pos.col - prev_col).write_buf(contents);
                    erase = None;
                }
            }

            if cell != prev_cell {
                let attrs = cell.attrs();
                if cell.has_contents() {
                    if pos != prev_pos {
                        if !wrapping
                            || prev_pos.row + 1 != pos.row
                            || prev_pos.col < self.cols() - u16::from(cell.is_wide())
                            || pos.col != 0
                        {
                            crate::ratty_vt::term::MoveFromTo::new(prev_pos, pos)
                                .write_buf(contents);
                        }
                        prev_pos = pos;
                    }

                    if &prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = *attrs;
                    }

                    cluster_break(contents, &mut last_written, pos, prev_pos, cell.contents());
                    prev_pos.col += if cell.is_wide() { 2 } else { 1 };
                    contents.extend(cell.contents().as_bytes());
                } else if erase.is_none() {
                    erase = Some((pos.col, attrs));
                }
            }
        }
        if let Some((prev_col, attrs)) = erase {
            let new_pos = crate::ratty_vt::grid::Pos { row, col: prev_col };
            if wrapping && prev_pos.row + 1 == new_pos.row && prev_pos.col >= self.cols() {
                if new_pos.col > 0 {
                    contents.extend(" ".repeat(usize::from(new_pos.col)).as_bytes());
                } else {
                    contents.extend(b" ");
                    crate::ratty_vt::term::Backspace.write_buf(contents);
                }
            } else {
                crate::ratty_vt::term::MoveFromTo::new(prev_pos, new_pos).write_buf(contents);
            }
            prev_pos = new_pos;
            if &prev_attrs != attrs {
                attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *attrs;
            }
            crate::ratty_vt::term::ClearRowForward.write_buf(contents);
        }

        // if this row is going from wrapped to not wrapped, we need to erase
        // and redraw the last character to break wrapping. if this row is
        // wrapped, we need to redraw the last character without erasing it to
        // position the cursor after the end of the line correctly so that
        // drawing the next line can just start writing and be wrapped.
        if (!self.wrapped && prev.wrapped) || (!prev.wrapped && self.wrapped) {
            let end_pos = if self.cells[usize::from(self.cols() - 1)].is_wide_continuation() {
                crate::ratty_vt::grid::Pos {
                    row,
                    col: self.cols() - 2,
                }
            } else {
                crate::ratty_vt::grid::Pos {
                    row,
                    col: self.cols() - 1,
                }
            };
            crate::ratty_vt::term::MoveFromTo::new(prev_pos, end_pos).write_buf(contents);
            prev_pos = end_pos;
            if !self.wrapped {
                crate::ratty_vt::term::EraseChar::new(1).write_buf(contents);
            }
            let end_cell = &self.cells[usize::from(end_pos.col)];
            if end_cell.has_contents() {
                let attrs = end_cell.attrs();
                if &prev_attrs != attrs {
                    attrs.write_escape_code_diff(contents, &prev_attrs);
                    prev_attrs = *attrs;
                }
                contents.extend(end_cell.contents().as_bytes());
                prev_pos.col += if end_cell.is_wide() { 2 } else { 1 };
            }
        }

        (prev_pos, prev_attrs)
    }
}

/// Emits an explicit column move before writing `cell_contents` when it
/// would otherwise join the grapheme cluster of the cell written just before
/// it (`Screen::extend_grapheme_cluster`), so replaying formatted output
/// reproduces two cells that were written separately. Records the contents
/// as the most recently written cell.
fn cluster_break<'a>(
    contents: &mut Vec<u8>,
    last_written: &mut Option<&'a str>,
    pos: crate::ratty_vt::grid::Pos,
    prev_pos: crate::ratty_vt::grid::Pos,
    cell_contents: &'a str,
) {
    if pos == prev_pos
        && let Some(previous) = *last_written
        && let Some(first) = cell_contents.chars().next()
        && crate::ratty_vt::screen::clusters_with(previous, first)
    {
        crate::ratty_vt::term::MoveTo::new(pos).write_buf(contents);
    }
    *last_written = Some(cell_contents);
}

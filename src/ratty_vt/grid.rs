use crate::ratty_vt::term::BufWrite as _;

#[derive(Clone, Debug)]
pub struct Grid {
    size: Size,
    pos: Pos,
    saved_pos: Pos,
    rows: Vec<crate::ratty_vt::row::Row>,
    scroll_top: u16,
    scroll_bottom: u16,
    origin_mode: bool,
    saved_origin_mode: bool,
    scrollback: std::collections::VecDeque<crate::ratty_vt::row::Row>,
    scrollback_len: usize,
    scrollback_offset: usize,
}

impl Grid {
    pub fn new(size: Size, scrollback_len: usize) -> Self {
        Self {
            size,
            pos: Pos::default(),
            saved_pos: Pos::default(),
            rows: vec![],
            scroll_top: 0,
            scroll_bottom: size.rows - 1,
            origin_mode: false,
            saved_origin_mode: false,
            scrollback: std::collections::VecDeque::new(),
            scrollback_len,
            scrollback_offset: 0,
        }
    }

    pub fn allocate_rows(&mut self) {
        if self.rows.is_empty() {
            self.rows.extend(
                std::iter::repeat_with(|| crate::ratty_vt::row::Row::new(self.size.cols))
                    .take(usize::from(self.size.rows)),
            );
        }
    }

    fn new_row(&self) -> crate::ratty_vt::row::Row {
        crate::ratty_vt::row::Row::new(self.size.cols)
    }

    pub fn clear(&mut self) {
        self.pos = Pos::default();
        self.saved_pos = Pos::default();
        for row in self.drawing_rows_mut() {
            row.clear(crate::ratty_vt::attrs::Attrs::default());
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows - 1;
        self.origin_mode = false;
        self.saved_origin_mode = false;
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn set_size(&mut self, size: Size) {
        if size.cols != self.size.cols {
            for row in &mut self.rows {
                row.wrap(false);
            }
        }

        if self.scroll_bottom == self.size.rows - 1 {
            self.scroll_bottom = size.rows - 1;
        }

        self.size = size;
        for row in &mut self.rows {
            row.resize(size.cols, crate::ratty_vt::Cell::new());
        }
        self.rows.resize(usize::from(size.rows), self.new_row());

        if self.scroll_bottom >= size.rows {
            self.scroll_bottom = size.rows - 1;
        }
        if self.scroll_bottom < self.scroll_top {
            self.scroll_top = 0;
        }

        self.row_clamp_top(false);
        self.row_clamp_bottom(false);
        self.col_clamp();

        if self.saved_pos.row > self.size.rows - 1 {
            self.saved_pos.row = self.size.rows - 1;
        }
        if self.saved_pos.col > self.size.cols - 1 {
            self.saved_pos.col = self.size.cols - 1;
        }
    }

    // ratty-vt: resize with reflow.
    //
    // Upstream `set_size` truncates or pads rows in place: narrowing the grid
    // cuts every line off at the new width and widening leaves the old wrap
    // points in place. This variant treats consecutive wrapped rows as one
    // logical line and re-wraps every line (scrollback included) at the new
    // width, moves rows between the screen and scrollback when the row count
    // changes, keeps the cursor on the character it was on, and resets the
    // DECSTBM scroll region, which is what xterm does on resize.
    pub fn set_size_reflow(&mut self, size: Size) {
        let size = Size {
            rows: size.rows.max(1),
            cols: size.cols.max(1),
        };
        if self.rows.is_empty() {
            // Never allocated (the alternate grid before first use): there is
            // nothing to reflow, and `allocate_rows` will use the new size.
            self.size = size;
            self.reset_after_resize();
            return;
        }
        if size.cols != self.size.cols {
            self.reflow_columns(size.cols);
        }
        self.set_row_count(size.rows);
        self.size = size;
        self.reset_after_resize();
    }

    // ratty-vt: upstream `set_size` semantics (no reflow) plus the scroll
    // region reset. Used for the alternate screen, whose applications redraw
    // from scratch on SIGWINCH and would only be confused by reflowed rows.
    pub fn set_size_plain(&mut self, size: Size) {
        let size = Size {
            rows: size.rows.max(1),
            cols: size.cols.max(1),
        };
        self.set_size(size);
        self.reset_after_resize();
    }

    fn reset_after_resize(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows - 1;
        self.scrollback_offset = self.scrollback_offset.min(self.scrollback.len());
        self.row_clamp();
        // The column may sit one past the last cell after a character was
        // drawn in the last column (pending wrap); keep that state.
        if self.pos.col > self.size.cols {
            self.pos.col = self.size.cols;
        }
        if self.saved_pos.row > self.size.rows - 1 {
            self.saved_pos.row = self.size.rows - 1;
        }
        if self.saved_pos.col > self.size.cols - 1 {
            self.saved_pos.col = self.size.cols - 1;
        }
    }

    fn push_scrollback(&mut self, row: crate::ratty_vt::row::Row) {
        if self.scrollback_len == 0 {
            return;
        }
        self.scrollback.push_back(row);
        while self.scrollback.len() > self.scrollback_len {
            self.scrollback.pop_front();
        }
    }

    // Changes the number of screen rows. Shrinking first drops blank rows
    // below the cursor, then moves rows off the top into scrollback so the
    // cursor stays on screen; growing pulls rows back out of scrollback
    // before appending blank ones. This mirrors what alacritty does.
    fn set_row_count(&mut self, rows: u16) {
        let target = usize::from(rows);
        while self.rows.len() > target {
            let last = self.rows.len() - 1;
            if last > usize::from(self.pos.row) && self.rows[last].is_blank() {
                self.rows.pop();
            } else {
                let removed = self.rows.remove(0);
                self.push_scrollback(removed);
                self.pos.row = self.pos.row.saturating_sub(1);
                self.saved_pos.row = self.saved_pos.row.saturating_sub(1);
            }
        }
        while self.rows.len() < target {
            if let Some(row) = self.scrollback.pop_back() {
                self.rows.insert(0, row);
                self.pos.row = self.pos.row.saturating_add(1);
                self.saved_pos.row = self.saved_pos.row.saturating_add(1);
            } else {
                self.rows
                    .push(crate::ratty_vt::row::Row::new(self.size.cols));
            }
        }
    }

    // Re-wraps every logical line at `cols` columns.
    fn reflow_columns(&mut self, cols: u16) {
        let width = usize::from(cols);
        let screen_rows = self.rows.len();
        let cursor_abs = self.scrollback.len() + usize::from(self.pos.row);
        let cursor_col = usize::from(self.pos.col);
        let blank = crate::ratty_vt::Cell::new();

        let all: Vec<crate::ratty_vt::row::Row> = self
            .scrollback
            .drain(..)
            .chain(self.rows.drain(..))
            .collect();

        let mut out: Vec<crate::ratty_vt::row::Row> = Vec::with_capacity(all.len());
        // (row index in `out`, column)
        let mut new_cursor: Option<(usize, u16)> = None;

        let mut start = 0;
        while start < all.len() {
            // A logical line is a run of wrapped rows plus the row that ends it.
            let mut end = start;
            while end + 1 < all.len() && all[end].wrapped() {
                end += 1;
            }

            // Flatten the line, trimming the blank tail of its last row.
            let mut cells: Vec<crate::ratty_vt::Cell> = Vec::new();
            let mut cursor_offset: Option<usize> = None;
            let mut has_placeholder = false;
            for (abs, row) in all.iter().enumerate().take(end + 1).skip(start) {
                if abs == cursor_abs {
                    cursor_offset = Some(cells.len() + cursor_col);
                }
                has_placeholder |= row.has_kitty_placeholder();
                cells.extend(row.cells().cloned());
                if abs == end {
                    while cells.last().is_some_and(|cell| cell == &blank) {
                        // Do not trim past the cursor's own row start; an
                        // all-blank line still yields one empty row below.
                        cells.pop();
                    }
                }
            }
            let content_len = cells.len();

            // Re-chunk at the new width without splitting wide glyphs.
            let mut line_rows: Vec<Vec<crate::ratty_vt::Cell>> = vec![Vec::with_capacity(width)];
            let mut cursor_in_line: Option<(usize, u16)> = None;
            for (n, cell) in cells.into_iter().enumerate() {
                if width < 2 && (cell.is_wide() || cell.is_wide_continuation()) {
                    // A wide glyph cannot exist in a one-column grid.
                    continue;
                }
                let mut at = line_rows.len() - 1;
                if line_rows[at].len() >= width {
                    line_rows.push(Vec::with_capacity(width));
                    at += 1;
                } else if cell.is_wide() && line_rows[at].len() == width - 1 {
                    line_rows[at].push(blank.clone());
                    line_rows.push(Vec::with_capacity(width));
                    at += 1;
                }
                if cursor_offset == Some(n) {
                    cursor_in_line = Some((at, u16::try_from(line_rows[at].len()).unwrap()));
                }
                line_rows[at].push(cell);
            }
            if let Some(offset) = cursor_offset
                && cursor_in_line.is_none()
            {
                // The cursor sat at or past the end of the line's content.
                // Keep it that far past the end on the last row, clamped so it
                // is at most one past the last column (pending wrap).
                let last = line_rows.len() - 1;
                let col = (line_rows[last].len() + (offset - content_len)).min(width);
                cursor_in_line = Some((last, u16::try_from(col).unwrap()));
            }

            let count = line_rows.len();
            for (k, mut row_cells) in line_rows.into_iter().enumerate() {
                row_cells.resize(width, blank.clone());
                let mut row = crate::ratty_vt::row::Row::from_cells(row_cells, k + 1 < count);
                if has_placeholder {
                    row.mark_kitty_placeholder();
                }
                if cursor_in_line.is_some_and(|(r, _)| r == k) {
                    new_cursor = Some((out.len(), cursor_in_line.unwrap().1));
                }
                out.push(row);
            }

            start = end + 1;
        }

        // Split the reflowed rows back into scrollback and screen, keeping
        // the cursor's row on screen. Blank rows below the cursor are dropped
        // first so that a mostly empty screen does not push its text into
        // scrollback just because its lines got longer.
        let (cursor_row, cursor_col) = new_cursor.unwrap_or((out.len().saturating_sub(1), 0));
        while out.len() > screen_rows
            && out.len() - 1 > cursor_row
            && out.last().is_some_and(crate::ratty_vt::row::Row::is_blank)
        {
            out.pop();
        }
        let total = out.len();
        let screen_start = total.saturating_sub(screen_rows).min(cursor_row);
        let mut screen = out.split_off(screen_start);
        screen.truncate(screen_rows);
        while screen.len() < screen_rows {
            screen.push(crate::ratty_vt::row::Row::new(cols));
        }
        for row in out {
            self.push_scrollback(row);
        }
        self.rows = screen;
        self.size.cols = cols;
        self.pos = Pos {
            row: u16::try_from(cursor_row - screen_start).unwrap(),
            col: cursor_col,
        };
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }

    pub fn set_pos(&mut self, mut pos: Pos) {
        if self.origin_mode {
            pos.row = pos.row.saturating_add(self.scroll_top);
        }
        self.pos = pos;
        self.row_clamp_top(self.origin_mode);
        self.row_clamp_bottom(self.origin_mode);
        self.col_clamp();
    }

    pub fn save_cursor(&mut self) {
        self.saved_pos = self.pos;
        self.saved_origin_mode = self.origin_mode;
    }

    pub fn restore_cursor(&mut self) {
        self.pos = self.saved_pos;
        self.origin_mode = self.saved_origin_mode;
    }

    pub fn visible_rows(&self) -> impl Iterator<Item = &crate::ratty_vt::row::Row> {
        let scrollback_len = self.scrollback.len();
        let rows_len = self.rows.len();
        self.scrollback
            .iter()
            .skip(scrollback_len - self.scrollback_offset)
            // when scrollback_offset > rows_len (e.g. rows = 3,
            // scrollback_len = 10, offset = 9) the skip(10 - 9)
            // will take 9 rows instead of 3. we need to set
            // the upper bound to rows_len (e.g. 3)
            .take(rows_len)
            // same for rows_len - scrollback_offset (e.g. 3 - 9).
            // it'll panic with overflow. we have to saturate the subtraction.
            .chain(
                self.rows
                    .iter()
                    .take(rows_len.saturating_sub(self.scrollback_offset)),
            )
    }

    pub fn drawing_rows(&self) -> impl Iterator<Item = &crate::ratty_vt::row::Row> {
        self.rows.iter()
    }

    pub fn drawing_rows_mut(&mut self) -> impl Iterator<Item = &mut crate::ratty_vt::row::Row> {
        self.rows.iter_mut()
    }

    // ratty-vt: index the scrollback ring and the drawing rows directly
    // instead of walking `visible_rows` with `nth`, which is O(row) and runs
    // once per row per frame in the renderers.
    pub fn visible_row(&self, row: u16) -> Option<&crate::ratty_vt::row::Row> {
        let row = usize::from(row);
        if row >= self.rows.len() {
            return None;
        }
        let offset = self.scrollback_offset;
        if row < offset {
            // `scrollback_offset` is clamped to `scrollback.len()`, so the
            // subtraction cannot underflow.
            self.scrollback.get(self.scrollback.len() - offset + row)
        } else {
            self.rows.get(row - offset)
        }
    }

    pub fn drawing_row(&self, row: u16) -> Option<&crate::ratty_vt::row::Row> {
        self.drawing_rows().nth(usize::from(row))
    }

    pub fn drawing_row_mut(&mut self, row: u16) -> Option<&mut crate::ratty_vt::row::Row> {
        self.drawing_rows_mut().nth(usize::from(row))
    }

    pub fn current_row_mut(&mut self) -> &mut crate::ratty_vt::row::Row {
        self.drawing_row_mut(self.pos.row)
            // we assume self.pos.row is always valid
            .unwrap()
    }

    pub fn visible_cell(&self, pos: Pos) -> Option<&crate::ratty_vt::Cell> {
        self.visible_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell(&self, pos: Pos) -> Option<&crate::ratty_vt::Cell> {
        self.drawing_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell_mut(&mut self, pos: Pos) -> Option<&mut crate::ratty_vt::Cell> {
        self.drawing_row_mut(pos.row)
            .and_then(|r| r.get_mut(pos.col))
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback_len
    }

    pub fn scrollback(&self) -> usize {
        self.scrollback_offset
    }

    pub fn set_scrollback(&mut self, rows: usize) {
        self.scrollback_offset = rows.min(self.scrollback.len());
    }

    pub fn write_contents(&self, contents: &mut String) {
        let mut wrapping = false;
        for row in self.visible_rows() {
            row.write_contents(contents, 0, self.size.cols, wrapping);
            if !row.wrapped() {
                contents.push('\n');
            }
            wrapping = row.wrapped();
        }

        while contents.ends_with('\n') {
            contents.truncate(contents.len() - 1);
        }
    }

    pub fn write_contents_formatted(
        &self,
        contents: &mut Vec<u8>,
    ) -> crate::ratty_vt::attrs::Attrs {
        crate::ratty_vt::term::ClearAttrs.write_buf(contents);
        crate::ratty_vt::term::ClearScreen.write_buf(contents);

        let mut prev_attrs = crate::ratty_vt::attrs::Attrs::default();
        let mut prev_pos = Pos::default();
        let mut wrapping = false;
        for (i, row) in self.visible_rows().enumerate() {
            // we limit the number of cols to a u16 (see Size), so
            // visible_rows() can never return more rows than will fit
            let i = i.try_into().unwrap();
            let (new_pos, new_attrs) = row.write_contents_formatted(
                contents,
                0,
                self.size.cols,
                i,
                wrapping,
                Some(prev_pos),
                Some(prev_attrs),
            );
            prev_pos = new_pos;
            prev_attrs = new_attrs;
            wrapping = row.wrapped();
        }

        self.write_cursor_position_formatted(contents, Some(prev_pos), Some(prev_attrs));

        prev_attrs
    }

    pub fn write_contents_diff(
        &self,
        contents: &mut Vec<u8>,
        prev: &Self,
        mut prev_attrs: crate::ratty_vt::attrs::Attrs,
    ) -> crate::ratty_vt::attrs::Attrs {
        let mut prev_pos = prev.pos;
        let mut wrapping = false;
        let mut prev_wrapping = false;
        for (i, (row, prev_row)) in self.visible_rows().zip(prev.visible_rows()).enumerate() {
            // we limit the number of cols to a u16 (see Size), so
            // visible_rows() can never return more rows than will fit
            let i = i.try_into().unwrap();
            let (new_pos, new_attrs) = row.write_contents_diff(
                contents,
                prev_row,
                0,
                self.size.cols,
                i,
                wrapping,
                prev_wrapping,
                prev_pos,
                prev_attrs,
            );
            prev_pos = new_pos;
            prev_attrs = new_attrs;
            wrapping = row.wrapped();
            prev_wrapping = prev_row.wrapped();
        }

        self.write_cursor_position_formatted(contents, Some(prev_pos), Some(prev_attrs));

        prev_attrs
    }

    pub fn write_cursor_position_formatted(
        &self,
        contents: &mut Vec<u8>,
        prev_pos: Option<Pos>,
        prev_attrs: Option<crate::ratty_vt::attrs::Attrs>,
    ) {
        let prev_attrs = prev_attrs.unwrap_or_default();
        // writing a character to the last column of a row doesn't wrap the
        // cursor immediately - it waits until the next character is actually
        // drawn. it is only possible for the cursor to have this kind of
        // position after drawing a character though, so if we end in this
        // position, we need to redraw the character at the end of the row.
        if prev_pos != Some(self.pos) && self.pos.col >= self.size.cols {
            let mut pos = Pos {
                row: self.pos.row,
                col: self.size.cols - 1,
            };
            if self
                .drawing_cell(pos)
                // we assume self.pos.row is always valid, and self.size.cols
                // - 1 is always a valid column
                .unwrap()
                .is_wide_continuation()
            {
                pos.col = self.size.cols - 2;
            }
            let cell =
                // we assume self.pos.row is always valid, and self.size.cols
                // - 2 must be a valid column because self.size.cols - 1 is
                // always valid and we just checked that the cell at
                // self.size.cols - 1 is a wide continuation character, which
                // means that the first half of the wide character must be
                // before it
                self.drawing_cell(pos).unwrap();
            if cell.has_contents() {
                if let Some(prev_pos) = prev_pos {
                    crate::ratty_vt::term::MoveFromTo::new(prev_pos, pos).write_buf(contents);
                } else {
                    crate::ratty_vt::term::MoveTo::new(pos).write_buf(contents);
                }
                cell.attrs().write_escape_code_diff(contents, &prev_attrs);
                contents.extend(cell.contents().as_bytes());
                prev_attrs.write_escape_code_diff(contents, cell.attrs());
            } else {
                // if the cell doesn't have contents, we can't have gotten
                // here by drawing a character in the last column. this means
                // that as far as i'm aware, we have to have reached here from
                // a newline when we were already after the end of an earlier
                // row. in the case where we are already after the end of an
                // earlier row, we can just write a few newlines, otherwise we
                // also need to do the same as above to get ourselves to after
                // the end of a row.
                let mut found = false;
                for i in (0..self.pos.row).rev() {
                    pos.row = i;
                    pos.col = self.size.cols - 1;
                    if self
                        .drawing_cell(pos)
                        // i is always less than self.pos.row, which we assume
                        // to be always valid, so it must also be valid.
                        // self.size.cols - 1 is always a valid col.
                        .unwrap()
                        .is_wide_continuation()
                    {
                        pos.col = self.size.cols - 2;
                    }
                    let cell = self
                        .drawing_cell(pos)
                        // i is always less than self.pos.row, which we assume
                        // to be always valid, so it must also be valid.
                        // self.size.cols - 2 is valid because self.size.cols
                        // - 1 is always valid, and col gets set to
                        // self.size.cols - 2 when the cell at self.size.cols
                        // - 1 is a wide continuation character, meaning that
                        // the first half of the wide character must be before
                        // it
                        .unwrap();
                    if cell.has_contents() {
                        if let Some(prev_pos) = prev_pos {
                            if prev_pos.row != i || prev_pos.col < self.size.cols {
                                crate::ratty_vt::term::MoveFromTo::new(prev_pos, pos)
                                    .write_buf(contents);
                                cell.attrs().write_escape_code_diff(contents, &prev_attrs);
                                contents.extend(cell.contents().as_bytes());
                                prev_attrs.write_escape_code_diff(contents, cell.attrs());
                            }
                        } else {
                            crate::ratty_vt::term::MoveTo::new(pos).write_buf(contents);
                            cell.attrs().write_escape_code_diff(contents, &prev_attrs);
                            contents.extend(cell.contents().as_bytes());
                            prev_attrs.write_escape_code_diff(contents, cell.attrs());
                        }
                        contents.extend("\n".repeat(usize::from(self.pos.row - i)).as_bytes());
                        found = true;
                        break;
                    }
                }

                // this can happen if you get the cursor off the end of a row,
                // and then do something to clear the end of the current row
                // without moving the cursor (IL, DL, ED, EL, etc). we know
                // there can't be something in the last column because we
                // would have caught that above, so it should be safe to
                // overwrite it.
                if !found {
                    pos = Pos {
                        row: self.pos.row,
                        col: self.size.cols - 1,
                    };
                    if let Some(prev_pos) = prev_pos {
                        crate::ratty_vt::term::MoveFromTo::new(prev_pos, pos).write_buf(contents);
                    } else {
                        crate::ratty_vt::term::MoveTo::new(pos).write_buf(contents);
                    }
                    contents.push(b' ');
                    // we know that the cell has no contents, but it still may
                    // have drawing attributes (background color, etc)
                    let end_cell = self
                        .drawing_cell(pos)
                        // we assume self.pos.row is always valid, and
                        // self.size.cols - 1 is always a valid column
                        .unwrap();
                    end_cell
                        .attrs()
                        .write_escape_code_diff(contents, &prev_attrs);
                    crate::ratty_vt::term::SaveCursor.write_buf(contents);
                    crate::ratty_vt::term::Backspace.write_buf(contents);
                    crate::ratty_vt::term::EraseChar::new(1).write_buf(contents);
                    crate::ratty_vt::term::RestoreCursor.write_buf(contents);
                    prev_attrs.write_escape_code_diff(contents, end_cell.attrs());
                }
            }
        } else if let Some(prev_pos) = prev_pos {
            crate::ratty_vt::term::MoveFromTo::new(prev_pos, self.pos).write_buf(contents);
        } else {
            crate::ratty_vt::term::MoveTo::new(self.pos).write_buf(contents);
        }
    }

    pub fn erase_all(&mut self, attrs: crate::ratty_vt::attrs::Attrs) {
        for row in self.drawing_rows_mut() {
            row.clear(attrs);
        }
    }

    pub fn erase_all_forward(&mut self, attrs: crate::ratty_vt::attrs::Attrs) {
        let pos = self.pos;
        for row in self.drawing_rows_mut().skip(usize::from(pos.row) + 1) {
            row.clear(attrs);
        }

        self.erase_row_forward(attrs);
    }

    pub fn erase_all_backward(&mut self, attrs: crate::ratty_vt::attrs::Attrs) {
        let pos = self.pos;
        for row in self.drawing_rows_mut().take(usize::from(pos.row)) {
            row.clear(attrs);
        }

        self.erase_row_backward(attrs);
    }

    pub fn erase_row(&mut self, attrs: crate::ratty_vt::attrs::Attrs) {
        self.current_row_mut().clear(attrs);
    }

    pub fn erase_row_forward(&mut self, attrs: crate::ratty_vt::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in pos.col..size.cols {
            row.erase(col, attrs);
        }
    }

    pub fn erase_row_backward(&mut self, attrs: crate::ratty_vt::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in 0..=pos.col.min(size.cols - 1) {
            row.erase(col, attrs);
        }
    }

    pub fn insert_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        let wide = pos.col < size.cols
            && self
                .drawing_cell(pos)
                // we assume self.pos.row is always valid, and we know we are
                // not off the end of a row because we just checked pos.col <
                // size.cols
                .unwrap()
                .is_wide_continuation();
        let row = self.current_row_mut();
        for _ in 0..count {
            if wide {
                row.get_mut(pos.col).unwrap().set_wide_continuation(false);
            }
            row.insert(pos.col, crate::ratty_vt::Cell::new());
            if wide {
                row.get_mut(pos.col).unwrap().set_wide_continuation(true);
            }
        }
        row.truncate(size.cols);
    }

    pub fn delete_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for _ in 0..(count.min(size.cols - pos.col)) {
            row.remove(pos.col);
        }
        row.resize(size.cols, crate::ratty_vt::Cell::new());
    }

    pub fn erase_cells(&mut self, count: u16, attrs: crate::ratty_vt::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in pos.col..((pos.col.saturating_add(count)).min(size.cols)) {
            row.erase(col, attrs);
        }
    }

    pub fn insert_lines(&mut self, count: u16) {
        for _ in 0..count {
            self.rows.remove(usize::from(self.scroll_bottom));
            self.rows.insert(usize::from(self.pos.row), self.new_row());
            // self.scroll_bottom is maintained to always be a valid row
            self.rows[usize::from(self.scroll_bottom)].wrap(false);
        }
    }

    pub fn delete_lines(&mut self, count: u16) {
        for _ in 0..(count.min(self.size.rows - self.pos.row)) {
            self.rows
                .insert(usize::from(self.scroll_bottom) + 1, self.new_row());
            self.rows.remove(usize::from(self.pos.row));
        }
    }

    pub fn scroll_up(&mut self, count: u16) {
        for _ in 0..(count.min(self.size.rows - self.scroll_top)) {
            self.rows
                .insert(usize::from(self.scroll_bottom) + 1, self.new_row());
            let removed = self.rows.remove(usize::from(self.scroll_top));
            if self.scrollback_len > 0 && !self.scroll_region_active() {
                self.scrollback.push_back(removed);
                while self.scrollback.len() > self.scrollback_len {
                    self.scrollback.pop_front();
                }
                if self.scrollback_offset > 0 {
                    self.scrollback_offset = self.scrollback.len().min(self.scrollback_offset + 1);
                }
            }
        }
    }

    pub fn scroll_down(&mut self, count: u16) {
        for _ in 0..count {
            self.rows.remove(usize::from(self.scroll_bottom));
            self.rows
                .insert(usize::from(self.scroll_top), self.new_row());
            // self.scroll_bottom is maintained to always be a valid row
            self.rows[usize::from(self.scroll_bottom)].wrap(false);
        }
    }

    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let bottom = bottom.min(self.size().rows - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.size().rows - 1;
        }
        self.pos.row = self.scroll_top;
        self.pos.col = 0;
    }

    fn in_scroll_region(&self) -> bool {
        self.pos.row >= self.scroll_top && self.pos.row <= self.scroll_bottom
    }

    fn scroll_region_active(&self) -> bool {
        self.scroll_top != 0 || self.scroll_bottom != self.size.rows - 1
    }

    pub fn set_origin_mode(&mut self, mode: bool) {
        self.origin_mode = mode;
        self.set_pos(Pos { row: 0, col: 0 });
    }

    pub fn row_inc_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        self.row_clamp_bottom(in_scroll_region);
    }

    pub fn row_inc_scroll(&mut self, count: u16) -> u16 {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        let lines = self.row_clamp_bottom(in_scroll_region);
        if in_scroll_region {
            self.scroll_up(lines);
            lines
        } else {
            0
        }
    }

    pub fn row_dec_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_sub(count);
        self.row_clamp_top(in_scroll_region);
    }

    pub fn row_dec_scroll(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        // need to account for clamping by both row_clamp_top and by
        // saturating_sub
        let extra_lines = count.saturating_sub(self.pos.row);
        self.pos.row = self.pos.row.saturating_sub(count);
        let lines = self.row_clamp_top(in_scroll_region);
        self.scroll_down(lines + extra_lines);
    }

    pub fn row_set(&mut self, i: u16) {
        self.pos.row = i;
        self.row_clamp();
    }

    pub fn col_inc(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
    }

    pub fn col_inc_clamp(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
        self.col_clamp();
    }

    pub fn col_dec(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_sub(count);
    }

    pub fn col_tab(&mut self) {
        self.pos.col -= self.pos.col % 8;
        self.pos.col += 8;
        self.col_clamp();
    }

    pub fn col_set(&mut self, i: u16) {
        self.pos.col = i;
        self.col_clamp();
    }

    pub fn col_wrap(&mut self, width: u16, wrap: bool) {
        if self.pos.col > self.size.cols - width {
            let mut prev_pos = self.pos;
            self.pos.col = 0;
            let scrolled = self.row_inc_scroll(1);
            // ratty-vt: a one-row grid scrolls its only row away here, so
            // the subtraction would underflow (upstream #29/#30).
            prev_pos.row = prev_pos.row.saturating_sub(scrolled);
            let new_pos = self.pos;
            self.drawing_row_mut(prev_pos.row)
                // we assume self.pos.row is always valid, and so prev_pos.row
                // must be valid because it is always less than or equal to
                // self.pos.row
                .unwrap()
                .wrap(wrap && prev_pos.row + 1 == new_pos.row);
        }
    }

    fn row_clamp_top(&mut self, limit_to_scroll_region: bool) -> u16 {
        if limit_to_scroll_region && self.pos.row < self.scroll_top {
            let rows = self.scroll_top - self.pos.row;
            self.pos.row = self.scroll_top;
            rows
        } else {
            0
        }
    }

    fn row_clamp_bottom(&mut self, limit_to_scroll_region: bool) -> u16 {
        let bottom = if limit_to_scroll_region {
            self.scroll_bottom
        } else {
            self.size.rows - 1
        };
        if self.pos.row > bottom {
            let rows = self.pos.row - bottom;
            self.pos.row = bottom;
            rows
        } else {
            0
        }
    }

    fn row_clamp(&mut self) {
        if self.pos.row > self.size.rows - 1 {
            self.pos.row = self.size.rows - 1;
        }
    }

    fn col_clamp(&mut self) {
        if self.pos.col > self.size.cols - 1 {
            self.pos.col = self.size.cols - 1;
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}

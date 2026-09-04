use crate::ratty_vt::term::BufWrite as _;
use unicode_width::UnicodeWidthChar as _;

const MODE_APPLICATION_KEYPAD: u8 = 0b0000_0001;
const MODE_APPLICATION_CURSOR: u8 = 0b0000_0010;
const MODE_HIDE_CURSOR: u8 = 0b0000_0100;
const MODE_ALTERNATE_SCREEN: u8 = 0b0000_1000;
const MODE_BRACKETED_PASTE: u8 = 0b0001_0000;

/// The kitty graphics protocol's Unicode placeholder character.
///
/// ratty-vt addition; see [`Row::has_kitty_placeholder`](crate::ratty_vt::Row::has_kitty_placeholder).
const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// The xterm mouse handling mode currently in use.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolMode {
    /// Mouse handling is disabled.
    #[default]
    None,

    /// Mouse button events should be reported on button press. Also known as
    /// X10 mouse mode.
    Press,

    /// Mouse button events should be reported on button press and release.
    /// Also known as VT200 mouse mode.
    PressRelease,

    // Highlight,
    /// Mouse button events should be reported on button press and release, as
    /// well as when the mouse moves between cells while a button is held
    /// down.
    ButtonMotion,

    /// Mouse button events should be reported on button press and release,
    /// and mouse motion events should be reported when the mouse moves
    /// between cells regardless of whether a button is held down or not.
    AnyMotion,
    // DecLocator,
}

/// The encoding to use for the enabled [`MouseProtocolMode`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolEncoding {
    /// Default single-printable-byte encoding.
    #[default]
    Default,

    /// UTF-8-based encoding.
    Utf8,

    /// SGR-like encoding.
    Sgr,
    // Urxvt,
}

/// Represents the overall terminal state.
#[derive(Clone, Debug)]
pub struct Screen {
    grid: crate::ratty_vt::grid::Grid,
    alternate_grid: crate::ratty_vt::grid::Grid,

    attrs: crate::ratty_vt::attrs::Attrs,
    saved_attrs: crate::ratty_vt::attrs::Attrs,

    modes: u8,
    mouse_protocol_mode: MouseProtocolMode,
    mouse_protocol_encoding: MouseProtocolEncoding,

    // ratty-vt: keyboard protocol state.
    //
    // The kitty keyboard protocol keeps one flag stack per screen (main and
    // alternate), so an application that pushes flags on the alternate
    // screen and exits without popping does not leave the shell with
    // enhanced key reporting.
    kitty_keyboard_flags: [Vec<u8>; 2],
    modify_other_keys: Option<u8>,
}

// ratty-vt: `CSI > u` pushes a flag set; kitty caps the stack so a runaway
// application cannot grow it without bound.
const KITTY_KEYBOARD_STACK_LIMIT: usize = 32;

impl Screen {
    pub(crate) fn new(size: crate::ratty_vt::grid::Size, scrollback_len: usize) -> Self {
        let mut grid = crate::ratty_vt::grid::Grid::new(size, scrollback_len);
        grid.allocate_rows();
        Self {
            grid,
            alternate_grid: crate::ratty_vt::grid::Grid::new(size, 0),

            attrs: crate::ratty_vt::attrs::Attrs::default(),
            saved_attrs: crate::ratty_vt::attrs::Attrs::default(),

            modes: 0,
            mouse_protocol_mode: MouseProtocolMode::default(),
            mouse_protocol_encoding: MouseProtocolEncoding::default(),

            kitty_keyboard_flags: [vec![], vec![]],
            modify_other_keys: None,
        }
    }

    /// Resizes the terminal.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        self.grid
            .set_size(crate::ratty_vt::grid::Size { rows, cols });
        self.alternate_grid
            .set_size(crate::ratty_vt::grid::Size { rows, cols });
    }

    /// Resizes the terminal, reflowing its contents.
    ///
    /// ratty-vt addition. Unlike [`set_size`](Self::set_size), which cuts
    /// lines off at the new width, this re-wraps every logical line of the
    /// main screen and its scrollback at the new width, moves rows between
    /// the screen and scrollback when the height changes, keeps the cursor on
    /// its character, and resets the DECSTBM scroll region. The alternate
    /// screen is resized without reflow, since full-screen applications
    /// redraw on resize anyway.
    pub fn set_size_reflow(&mut self, rows: u16, cols: u16) {
        let size = crate::ratty_vt::grid::Size { rows, cols };
        self.grid.set_size_reflow(size);
        self.alternate_grid.set_size_plain(size);
    }

    /// Returns the current size of the terminal.
    ///
    /// The return value will be (rows, cols).
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let size = self.grid().size();
        (size.rows, size.cols)
    }

    /// Scrolls to the given position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and
    /// should be `0` to put the normal screen in view.
    ///
    /// This affects the return values of methods called on the screen: for
    /// instance, `screen.cell(0, 0)` will return the top left corner of the
    /// screen after taking the scrollback offset into account.
    ///
    /// The value given will be clamped to the actual size of the scrollback.
    pub fn set_scrollback(&mut self, rows: usize) {
        self.grid_mut().set_scrollback(rows);
    }

    /// Returns the current position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and is
    /// `0` when the normal screen is in view.
    #[must_use]
    pub fn scrollback(&self) -> usize {
        self.grid().scrollback()
    }

    /// Returns the text contents of the terminal.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    #[must_use]
    pub fn contents(&self) -> String {
        let mut contents = String::new();
        self.write_contents(&mut contents);
        contents
    }

    fn write_contents(&self, contents: &mut String) {
        self.grid().write_contents(contents);
    }

    /// Returns the text contents of the terminal by row, restricted to the
    /// given subset of columns.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    ///
    /// Newlines will not be included.
    pub fn rows(&self, start: u16, width: u16) -> impl Iterator<Item = String> + '_ {
        self.grid().visible_rows().map(move |row| {
            let mut contents = String::new();
            row.write_contents(&mut contents, start, width, false);
            contents
        })
    }

    /// Returns the text contents of the terminal logically between two cells.
    /// This will include the remainder of the starting row after `start_col`,
    /// followed by the entire contents of the rows between `start_row` and
    /// `end_row`, followed by the beginning of the `end_row` up until
    /// `end_col`. This is useful for things like determining the contents of
    /// a clipboard selection.
    #[must_use]
    pub fn contents_between(
        &self,
        start_row: u16,
        start_col: u16,
        end_row: u16,
        end_col: u16,
    ) -> String {
        match start_row.cmp(&end_row) {
            std::cmp::Ordering::Less => {
                let (_, cols) = self.size();
                let mut contents = String::new();
                for (i, row) in self
                    .grid()
                    .visible_rows()
                    .enumerate()
                    .skip(usize::from(start_row))
                    .take(usize::from(end_row) - usize::from(start_row) + 1)
                {
                    if i == usize::from(start_row) {
                        row.write_contents(&mut contents, start_col, cols - start_col, false);
                        if !row.wrapped() {
                            contents.push('\n');
                        }
                    } else if i == usize::from(end_row) {
                        row.write_contents(&mut contents, 0, end_col, false);
                    } else {
                        row.write_contents(&mut contents, 0, cols, false);
                        if !row.wrapped() {
                            contents.push('\n');
                        }
                    }
                }
                contents
            }
            std::cmp::Ordering::Equal => {
                if start_col < end_col {
                    self.rows(start_col, end_col - start_col)
                        .nth(usize::from(start_row))
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            std::cmp::Ordering::Greater => String::new(),
        }
    }

    /// Return escape codes sufficient to reproduce the entire contents of the
    /// current terminal state. This is a convenience wrapper around
    /// [`contents_formatted`](Self::contents_formatted) and
    /// [`input_mode_formatted`](Self::input_mode_formatted).
    #[must_use]
    pub fn state_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_formatted(&mut contents);
        self.write_input_mode_formatted(&mut contents);
        contents
    }

    /// Return escape codes sufficient to turn the terminal state of the
    /// screen `prev` into the current terminal state. This is a convenience
    /// wrapper around [`contents_diff`](Self::contents_diff) and
    /// [`input_mode_diff`](Self::input_mode_diff).
    #[must_use]
    pub fn state_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_diff(&mut contents, prev);
        self.write_input_mode_diff(&mut contents, prev);
        contents
    }

    /// Returns the formatted visible contents of the terminal.
    ///
    /// Formatting information will be included inline as terminal escape
    /// codes. The result will be suitable for feeding directly to a raw
    /// terminal parser, and will result in the same visual output.
    #[must_use]
    pub fn contents_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_formatted(&mut contents);
        contents
    }

    fn write_contents_formatted(&self, contents: &mut Vec<u8>) {
        crate::ratty_vt::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        let prev_attrs = self.grid().write_contents_formatted(contents);
        self.attrs.write_escape_code_diff(contents, &prev_attrs);
    }

    /// Returns the formatted visible contents of the terminal by row,
    /// restricted to the given subset of columns.
    ///
    /// Formatting information will be included inline as terminal escape
    /// codes. The result will be suitable for feeding directly to a raw
    /// terminal parser, and will result in the same visual output.
    ///
    /// You are responsible for positioning the cursor before printing each
    /// row, and the final cursor position after displaying each row is
    /// unspecified.
    // the unwraps in this method shouldn't be reachable
    #[allow(clippy::missing_panics_doc)]
    pub fn rows_formatted(&self, start: u16, width: u16) -> impl Iterator<Item = Vec<u8>> + '_ {
        let mut wrapping = false;
        self.grid().visible_rows().enumerate().map(move |(i, row)| {
            // number of rows in a grid is stored in a u16 (see Size), so
            // visible_rows can never return enough rows to overflow here
            let i = i.try_into().unwrap();
            let mut contents = vec![];
            row.write_contents_formatted(&mut contents, start, width, i, wrapping, None, None);
            if start == 0 && width == self.grid.size().cols {
                wrapping = row.wrapped();
            }
            contents
        })
    }

    /// Returns a terminal byte stream sufficient to turn the visible contents
    /// of the screen described by `prev` into the visible contents of the
    /// screen described by `self`.
    ///
    /// The result of rendering `prev.contents_formatted()` followed by
    /// `self.contents_diff(prev)` should be equivalent to the result of
    /// rendering `self.contents_formatted()`. This is primarily useful when
    /// you already have a terminal parser whose state is described by `prev`,
    /// since the diff will likely require less memory and cause less
    /// flickering than redrawing the entire screen contents.
    #[must_use]
    pub fn contents_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_diff(&mut contents, prev);
        contents
    }

    fn write_contents_diff(&self, contents: &mut Vec<u8>, prev: &Self) {
        if self.hide_cursor() != prev.hide_cursor() {
            crate::ratty_vt::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        }
        let prev_attrs = self
            .grid()
            .write_contents_diff(contents, prev.grid(), prev.attrs);
        self.attrs.write_escape_code_diff(contents, &prev_attrs);
    }

    /// Returns a sequence of terminal byte streams sufficient to turn the
    /// visible contents of the subset of each row from `prev` (as described
    /// by `start` and `width`) into the visible contents of the corresponding
    /// row subset in `self`.
    ///
    /// You are responsible for positioning the cursor before printing each
    /// row, and the final cursor position after displaying each row is
    /// unspecified.
    // the unwraps in this method shouldn't be reachable
    #[allow(clippy::missing_panics_doc)]
    pub fn rows_diff<'a>(
        &'a self,
        prev: &'a Self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = Vec<u8>> + 'a {
        self.grid()
            .visible_rows()
            .zip(prev.grid().visible_rows())
            .enumerate()
            .map(move |(i, (row, prev_row))| {
                // number of rows in a grid is stored in a u16 (see Size), so
                // visible_rows can never return enough rows to overflow here
                let i = i.try_into().unwrap();
                let mut contents = vec![];
                row.write_contents_diff(
                    &mut contents,
                    prev_row,
                    start,
                    width,
                    i,
                    false,
                    false,
                    crate::ratty_vt::grid::Pos { row: i, col: start },
                    crate::ratty_vt::attrs::Attrs::default(),
                );
                contents
            })
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// terminal's input modes.
    ///
    /// Supported modes are:
    /// * application keypad
    /// * application cursor
    /// * bracketed paste
    /// * xterm mouse support
    #[must_use]
    pub fn input_mode_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_input_mode_formatted(&mut contents);
        contents
    }

    fn write_input_mode_formatted(&self, contents: &mut Vec<u8>) {
        crate::ratty_vt::term::ApplicationKeypad::new(self.mode(MODE_APPLICATION_KEYPAD))
            .write_buf(contents);
        crate::ratty_vt::term::ApplicationCursor::new(self.mode(MODE_APPLICATION_CURSOR))
            .write_buf(contents);
        crate::ratty_vt::term::BracketedPaste::new(self.mode(MODE_BRACKETED_PASTE))
            .write_buf(contents);
        crate::ratty_vt::term::MouseProtocolMode::new(
            self.mouse_protocol_mode,
            MouseProtocolMode::None,
        )
        .write_buf(contents);
        crate::ratty_vt::term::MouseProtocolEncoding::new(
            self.mouse_protocol_encoding,
            MouseProtocolEncoding::Default,
        )
        .write_buf(contents);
    }

    /// Returns terminal escape sequences sufficient to change the previous
    /// terminal's input modes to the input modes enabled in the current
    /// terminal.
    #[must_use]
    pub fn input_mode_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_input_mode_diff(&mut contents, prev);
        contents
    }

    fn write_input_mode_diff(&self, contents: &mut Vec<u8>, prev: &Self) {
        if self.mode(MODE_APPLICATION_KEYPAD) != prev.mode(MODE_APPLICATION_KEYPAD) {
            crate::ratty_vt::term::ApplicationKeypad::new(self.mode(MODE_APPLICATION_KEYPAD))
                .write_buf(contents);
        }
        if self.mode(MODE_APPLICATION_CURSOR) != prev.mode(MODE_APPLICATION_CURSOR) {
            crate::ratty_vt::term::ApplicationCursor::new(self.mode(MODE_APPLICATION_CURSOR))
                .write_buf(contents);
        }
        if self.mode(MODE_BRACKETED_PASTE) != prev.mode(MODE_BRACKETED_PASTE) {
            crate::ratty_vt::term::BracketedPaste::new(self.mode(MODE_BRACKETED_PASTE))
                .write_buf(contents);
        }
        crate::ratty_vt::term::MouseProtocolMode::new(
            self.mouse_protocol_mode,
            prev.mouse_protocol_mode,
        )
        .write_buf(contents);
        crate::ratty_vt::term::MouseProtocolEncoding::new(
            self.mouse_protocol_encoding,
            prev.mouse_protocol_encoding,
        )
        .write_buf(contents);
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// terminal's drawing attributes.
    ///
    /// Supported drawing attributes are:
    /// * fgcolor
    /// * bgcolor
    /// * bold
    /// * dim
    /// * italic
    /// * underline
    /// * inverse
    /// * blink
    ///
    /// This is not typically necessary, since
    /// [`contents_formatted`](Self::contents_formatted) will leave
    /// the current active drawing attributes in the correct state, but this
    /// can be useful in the case of drawing additional things on top of a
    /// terminal output, since you will need to restore the terminal state
    /// without the terminal contents necessarily being the same.
    #[must_use]
    pub fn attributes_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_attributes_formatted(&mut contents);
        contents
    }

    fn write_attributes_formatted(&self, contents: &mut Vec<u8>) {
        crate::ratty_vt::term::ClearAttrs.write_buf(contents);
        self.attrs
            .write_escape_code_diff(contents, &crate::ratty_vt::attrs::Attrs::default());
    }

    /// Returns the current cursor position of the terminal.
    ///
    /// The return value will be (row, col).
    #[must_use]
    pub fn cursor_position(&self) -> (u16, u16) {
        let pos = self.grid().pos();
        (pos.row, pos.col)
    }

    /// Returns the position a renderer should draw the cursor at, as
    /// `(row, col)`.
    ///
    /// ratty-vt addition. [`cursor_position`](Self::cursor_position) reports
    /// the logical position, whose column can equal the width after a
    /// character was drawn in the last column (pending wrap). This clamps
    /// that column onto the grid and, when the cursor sits on the second
    /// half of a wide character, snaps it to the first half, so the reported
    /// cell is always one that exists and owns its glyph.
    #[must_use]
    pub fn display_cursor_position(&self) -> (u16, u16) {
        let pos = self.grid().pos();
        let size = self.grid().size();
        let mut col = pos.col.min(size.cols.saturating_sub(1));
        if self
            .grid()
            .drawing_cell(crate::ratty_vt::grid::Pos { row: pos.row, col })
            .is_some_and(crate::ratty_vt::Cell::is_wide_continuation)
        {
            col = col.saturating_sub(1);
        }
        (pos.row, col)
    }

    /// Returns whether a renderer should leave the cursor undrawn.
    ///
    /// ratty-vt addition. Folds DECTCEM (`hide_cursor`) together with the
    /// scrollback offset: the cursor belongs to the live screen, so it must
    /// not be painted over history while the view is scrolled back.
    #[must_use]
    pub fn cursor_hidden(&self) -> bool {
        self.hide_cursor() || self.scrollback() > 0
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// cursor state of the terminal.
    ///
    /// This is not typically necessary, since
    /// [`contents_formatted`](Self::contents_formatted) will leave
    /// the cursor in the correct state, but this can be useful in the case of
    /// drawing additional things on top of a terminal output, since you will
    /// need to restore the terminal state without the terminal contents
    /// necessarily being the same.
    ///
    /// Note that the bytes returned by this function may alter the active
    /// drawing attributes, because it may require redrawing existing cells in
    /// order to position the cursor correctly (for instance, in the case
    /// where the cursor is past the end of a row). Therefore, you should
    /// ensure to reset the active drawing attributes if necessary after
    /// processing this data, for instance by using
    /// [`attributes_formatted`](Self::attributes_formatted).
    #[must_use]
    pub fn cursor_state_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_cursor_state_formatted(&mut contents);
        contents
    }

    fn write_cursor_state_formatted(&self, contents: &mut Vec<u8>) {
        crate::ratty_vt::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        self.grid()
            .write_cursor_position_formatted(contents, None, None);

        // we don't just call write_attributes_formatted here, because that
        // would still be confusing - consider the case where the user sets
        // their own unrelated drawing attributes (on a different parser
        // instance) and then calls cursor_state_formatted. just documenting
        // it and letting the user handle it on their own is more
        // straightforward.
    }

    /// Returns the visible row at `row`, if it exists, after taking the
    /// scrollback offset into account.
    ///
    /// ratty-vt addition. Borrows the row in O(1) so renderers can walk the
    /// screen every frame without copying it.
    #[must_use]
    pub fn visible_row(&self, row: u16) -> Option<&crate::ratty_vt::Row> {
        self.grid().visible_row(row)
    }

    /// Iterates over the visible rows, top to bottom, after taking the
    /// scrollback offset into account.
    ///
    /// ratty-vt addition.
    pub fn visible_rows(&self) -> impl Iterator<Item = &crate::ratty_vt::Row> {
        self.grid().visible_rows()
    }

    /// Returns the [`Cell`](crate::ratty_vt::Cell) object at the given location in the
    /// terminal, if it exists.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&crate::ratty_vt::Cell> {
        self.grid()
            .visible_cell(crate::ratty_vt::grid::Pos { row, col })
    }

    /// Returns whether the text in row `row` should wrap to the next line.
    #[must_use]
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.grid()
            .visible_row(row)
            .is_some_and(crate::ratty_vt::row::Row::wrapped)
    }

    /// Returns whether the alternate screen is currently in use.
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.mode(MODE_ALTERNATE_SCREEN)
    }

    /// Returns whether the terminal should be in application keypad mode.
    #[must_use]
    pub fn application_keypad(&self) -> bool {
        self.mode(MODE_APPLICATION_KEYPAD)
    }

    /// Returns whether the terminal should be in application cursor mode.
    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.mode(MODE_APPLICATION_CURSOR)
    }

    /// Returns whether the terminal should be in hide cursor mode.
    #[must_use]
    pub fn hide_cursor(&self) -> bool {
        self.mode(MODE_HIDE_CURSOR)
    }

    /// Returns whether the terminal should be in bracketed paste mode.
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.mode(MODE_BRACKETED_PASTE)
    }

    /// Returns the currently active [`MouseProtocolMode`].
    #[must_use]
    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        self.mouse_protocol_mode
    }

    /// Returns the currently active [`MouseProtocolEncoding`].
    #[must_use]
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        self.mouse_protocol_encoding
    }

    /// Returns the active kitty keyboard protocol enhancement flags.
    ///
    /// ratty-vt addition. The flags are the top of the current screen's
    /// stack, as set by `CSI > flags u` (push), `CSI = flags ; mode u` (set),
    /// and `CSI < n u` (pop); `0` means legacy key reporting.
    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.kitty_keyboard_stack().last().copied().unwrap_or(0)
    }

    /// Returns the active xterm `modifyOtherKeys` level, or `None` when the
    /// mode is disabled.
    ///
    /// ratty-vt addition. Set by `CSI > 4 ; level m`; level `0`, or a bare
    /// `CSI > 4 m`, disables the mode.
    #[must_use]
    pub fn modify_other_keys(&self) -> Option<u8> {
        self.modify_other_keys
    }

    fn kitty_keyboard_stack(&self) -> &Vec<u8> {
        &self.kitty_keyboard_flags[usize::from(self.mode(MODE_ALTERNATE_SCREEN))]
    }

    fn kitty_keyboard_stack_mut(&mut self) -> &mut Vec<u8> {
        &mut self.kitty_keyboard_flags[usize::from(self.mode(MODE_ALTERNATE_SCREEN))]
    }

    /// Returns the currently active foreground color.
    #[must_use]
    pub fn fgcolor(&self) -> crate::ratty_vt::Color {
        self.attrs.fgcolor
    }

    /// Returns the currently active background color.
    #[must_use]
    pub fn bgcolor(&self) -> crate::ratty_vt::Color {
        self.attrs.bgcolor
    }

    /// Returns whether newly drawn text should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether newly drawn text should be rendered with the dim text
    /// attribute.
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether newly drawn text should be rendered with the italic
    /// text attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether newly drawn text should be rendered with the
    /// underlined text attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether newly drawn text should be rendered with the inverse
    /// text attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }

    /// Returns the blink attribute newly drawn text should be rendered with.
    ///
    /// ratty-vt addition: upstream vt100 does not parse SGR 5/6/25.
    #[must_use]
    pub fn blink(&self) -> crate::ratty_vt::Blink {
        self.attrs.blink()
    }

    /// Returns whether newly drawn text should be concealed (SGR 8).
    ///
    /// ratty-vt addition.
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.attrs.hidden()
    }

    /// Returns whether newly drawn text should be struck through (SGR 9).
    ///
    /// ratty-vt addition.
    #[must_use]
    pub fn strikeout(&self) -> bool {
        self.attrs.strikeout()
    }

    /// Returns the underline color of newly drawn text (SGR 58);
    /// `Color::Default` means the foreground color.
    ///
    /// ratty-vt addition.
    #[must_use]
    pub fn underline_color(&self) -> crate::ratty_vt::Color {
        self.attrs.underline_color
    }

    pub(crate) fn grid(&self) -> &crate::ratty_vt::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &self.alternate_grid
        } else {
            &self.grid
        }
    }

    fn grid_mut(&mut self) -> &mut crate::ratty_vt::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &mut self.alternate_grid
        } else {
            &mut self.grid
        }
    }

    fn enter_alternate_grid(&mut self) {
        self.grid_mut().set_scrollback(0);
        self.set_mode(MODE_ALTERNATE_SCREEN);
        self.alternate_grid.allocate_rows();
    }

    fn exit_alternate_grid(&mut self) {
        self.clear_mode(MODE_ALTERNATE_SCREEN);
    }

    fn save_cursor(&mut self) {
        self.grid_mut().save_cursor();
        self.saved_attrs = self.attrs;
    }

    fn restore_cursor(&mut self) {
        self.grid_mut().restore_cursor();
        self.attrs = self.saved_attrs;
    }

    fn set_mode(&mut self, mode: u8) {
        self.modes |= mode;
    }

    fn clear_mode(&mut self, mode: u8) {
        self.modes &= !mode;
    }

    fn mode(&self, mode: u8) -> bool {
        self.modes & mode != 0
    }

    fn set_mouse_mode(&mut self, mode: MouseProtocolMode) {
        self.mouse_protocol_mode = mode;
    }

    fn clear_mouse_mode(&mut self, mode: MouseProtocolMode) {
        if self.mouse_protocol_mode == mode {
            self.mouse_protocol_mode = MouseProtocolMode::default();
        }
    }

    fn set_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        self.mouse_protocol_encoding = encoding;
    }

    fn clear_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        if self.mouse_protocol_encoding == encoding {
            self.mouse_protocol_encoding = MouseProtocolEncoding::default();
        }
    }
}

impl Screen {
    pub(crate) fn text(&mut self, c: char) {
        let pos = self.grid().pos();
        let size = self.grid().size();
        let attrs = self.attrs;

        let width = c.width();
        if width.is_none() && (u32::from(c)) < 256 {
            // don't even try to draw control characters
            return;
        }
        let width = width
            .unwrap_or(1)
            .try_into()
            // width() can only return 0, 1, or 2
            .unwrap();
        // ratty-vt: a wide glyph cannot be drawn in a grid narrower than
        // itself; upstream underflows below and panics in a one-column grid.
        if width > size.cols {
            return;
        }

        // it doesn't make any sense to wrap if the last column in a row
        // didn't already have contents. don't try to handle the case where a
        // character wraps because there was only one column left in the
        // previous row - literally everything handles this case differently,
        // and this is tmux behavior (and also the simplest). i'm open to
        // reconsidering this behavior, but only with a really good reason
        // (xterm handles this by introducing the concept of triple width
        // cells, which i really don't want to do).
        let mut wrap = false;
        if pos.col > size.cols - width {
            let last_cell = self
                .grid()
                .drawing_cell(crate::ratty_vt::grid::Pos {
                    row: pos.row,
                    col: size.cols - 1,
                })
                // pos.row is valid, since it comes directly from
                // self.grid().pos() which we assume to always have a valid
                // row value. size.cols - 1 is also always a valid column.
                .unwrap();
            if last_cell.has_contents() || last_cell.is_wide_continuation() {
                wrap = true;
            }
        }
        self.grid_mut().col_wrap(width, wrap);
        let pos = self.grid().pos();

        if width == 0 {
            if pos.col > 0 {
                let mut prev_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::ratty_vt::grid::Pos {
                        row: pos.row,
                        col: pos.col - 1,
                    })
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a
                    // valid row value. pos.col - 1 is valid because we just
                    // checked for pos.col > 0.
                    .unwrap();
                if prev_cell.is_wide_continuation() {
                    prev_cell = self
                        .grid_mut()
                        .drawing_cell_mut(crate::ratty_vt::grid::Pos {
                            row: pos.row,
                            col: pos.col - 2,
                        })
                        // pos.row is valid, since it comes directly from
                        // self.grid().pos() which we assume to always have a
                        // valid row value. we know pos.col - 2 is valid
                        // because the cell at pos.col - 1 is a wide
                        // continuation character, which means there must be
                        // the first half of the wide character before it.
                        .unwrap();
                }
                prev_cell.append(c);
            } else if pos.row > 0 {
                let prev_row = self
                    .grid()
                    .drawing_row(pos.row - 1)
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a
                    // valid row value. pos.row - 1 is valid because we just
                    // checked for pos.row > 0.
                    .unwrap();
                if prev_row.wrapped() {
                    let mut prev_cell = self
                        .grid_mut()
                        .drawing_cell_mut(crate::ratty_vt::grid::Pos {
                            row: pos.row - 1,
                            col: size.cols - 1,
                        })
                        // pos.row is valid, since it comes directly from
                        // self.grid().pos() which we assume to always have a
                        // valid row value. pos.row - 1 is valid because we
                        // just checked for pos.row > 0. col of size.cols - 1
                        // is always valid.
                        .unwrap();
                    if prev_cell.is_wide_continuation() {
                        prev_cell = self
                            .grid_mut()
                            .drawing_cell_mut(crate::ratty_vt::grid::Pos {
                                row: pos.row - 1,
                                col: size.cols - 2,
                            })
                            // pos.row is valid, since it comes directly from
                            // self.grid().pos() which we assume to always
                            // have a valid row value. pos.row - 1 is valid
                            // because we just checked for pos.row > 0. col of
                            // size.cols - 2 is valid because the cell at
                            // size.cols - 1 is a wide continuation character,
                            // so it must have the first half of the wide
                            // character before it.
                            .unwrap();
                    }
                    prev_cell.append(c);
                }
            }
        } else {
            if self
                .grid()
                .drawing_cell(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap()
                .is_wide_continuation()
            {
                let prev_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::ratty_vt::grid::Pos {
                        row: pos.row,
                        col: pos.col - 1,
                    })
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() immediately before this, which
                    // ensures that self.grid().pos().col has a valid value.
                    // pos.col - 1 is valid because the cell at pos.col is a
                    // wide continuation character, so it must have the first
                    // half of the wide character before it.
                    .unwrap();
                prev_cell.clear(attrs);
            }

            if self
                .grid()
                .drawing_cell(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap()
                .is_wide()
            {
                let next_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::ratty_vt::grid::Pos {
                        row: pos.row,
                        col: pos.col + 1,
                    })
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() immediately before this, which
                    // ensures that self.grid().pos().col has a valid value.
                    // pos.col + 1 is valid because the cell at pos.col is a
                    // wide character, so it must have the second half of the
                    // wide character after it.
                    .unwrap();
                next_cell.set(' ', attrs);
            }

            let cell = self
                .grid_mut()
                .drawing_cell_mut(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap();
            cell.set(c, attrs);
            // ratty-vt: remember that this row holds a kitty graphics
            // Unicode placeholder so renderers can skip rows without one.
            if c == KITTY_PLACEHOLDER {
                self.grid_mut().current_row_mut().mark_kitty_placeholder();
            }
            self.grid_mut().col_inc(1);
            if width > 1 {
                let pos = self.grid().pos();
                if self
                    .grid()
                    .drawing_cell(pos)
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() earlier, which ensures that
                    // self.grid().pos().col has a valid value. this is true
                    // even though we just called col_inc, because this branch
                    // only happens if width > 1, and col_wrap takes width
                    // into account.
                    .unwrap()
                    .is_wide()
                {
                    let next_next_pos = crate::ratty_vt::grid::Pos {
                        row: pos.row,
                        col: pos.col + 1,
                    };
                    let next_next_cell = self
                        .grid_mut()
                        .drawing_cell_mut(next_next_pos)
                        // pos.row is valid because we assume
                        // self.grid().pos() to always have a valid row value.
                        // pos.col is valid because we called col_wrap()
                        // earlier, which ensures that self.grid().pos().col
                        // has a valid value. this is true even though we just
                        // called col_inc, because this branch only happens if
                        // width > 1, and col_wrap takes width into account.
                        // pos.col + 1 is valid because the cell at pos.col is
                        // wide, and so it must have the second half of the
                        // wide character after it.
                        .unwrap();
                    next_next_cell.clear(attrs);
                    if next_next_pos.col == size.cols - 1 {
                        self.grid_mut()
                            .drawing_row_mut(pos.row)
                            // we assume self.grid().pos().row is always valid
                            .unwrap()
                            .wrap(false);
                    }
                }
                let next_cell = self
                    .grid_mut()
                    .drawing_cell_mut(pos)
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() earlier, which ensures that
                    // self.grid().pos().col has a valid value. this is true
                    // even though we just called col_inc, because this branch
                    // only happens if width > 1, and col_wrap takes width
                    // into account.
                    .unwrap();
                next_cell.clear(crate::ratty_vt::attrs::Attrs::default());
                next_cell.set_wide_continuation(true);
                self.grid_mut().col_inc(1);
            }
        }
    }

    // control codes

    pub(crate) fn bs(&mut self) {
        self.grid_mut().col_dec(1);
    }

    pub(crate) fn tab(&mut self) {
        self.grid_mut().col_tab();
    }

    pub(crate) fn lf(&mut self) {
        self.grid_mut().row_inc_scroll(1);
    }

    pub(crate) fn vt(&mut self) {
        self.lf();
    }

    pub(crate) fn ff(&mut self) {
        self.lf();
    }

    pub(crate) fn cr(&mut self) {
        self.grid_mut().col_set(0);
    }

    // escape codes

    // ESC 7
    pub(crate) fn decsc(&mut self) {
        self.save_cursor();
    }

    // ESC 8
    pub(crate) fn decrc(&mut self) {
        self.restore_cursor();
    }

    // CSI s
    //
    // ratty-vt: SCOSC saves only the cursor position, sharing the slot with
    // DECSC the way xterm does; text attributes are left alone.
    pub(crate) fn scosc(&mut self) {
        self.grid_mut().save_cursor();
    }

    // CSI u
    pub(crate) fn scorc(&mut self) {
        self.grid_mut().restore_cursor();
    }

    // ESC =
    pub(crate) fn deckpam(&mut self) {
        self.set_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC >
    pub(crate) fn deckpnm(&mut self) {
        self.clear_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC M
    pub(crate) fn ri(&mut self) {
        self.grid_mut().row_dec_scroll(1);
    }

    // ESC c
    pub(crate) fn ris(&mut self) {
        *self = Self::new(self.grid.size(), self.grid.scrollback_len());
    }

    // csi codes

    // CSI @
    pub(crate) fn ich(&mut self, count: u16) {
        self.grid_mut().insert_cells(count);
    }

    // CSI A
    pub(crate) fn cuu(&mut self, offset: u16) {
        self.grid_mut().row_dec_clamp(offset);
    }

    // CSI B
    pub(crate) fn cud(&mut self, offset: u16) {
        self.grid_mut().row_inc_clamp(offset);
    }

    // CSI C
    pub(crate) fn cuf(&mut self, offset: u16) {
        self.grid_mut().col_inc_clamp(offset);
    }

    // CSI D
    pub(crate) fn cub(&mut self, offset: u16) {
        self.grid_mut().col_dec(offset);
    }

    // CSI E
    pub(crate) fn cnl(&mut self, offset: u16) {
        self.grid_mut().col_set(0);
        self.grid_mut().row_inc_clamp(offset);
    }

    // CSI F
    pub(crate) fn cpl(&mut self, offset: u16) {
        self.grid_mut().col_set(0);
        self.grid_mut().row_dec_clamp(offset);
    }

    // CSI G
    pub(crate) fn cha(&mut self, col: u16) {
        self.grid_mut().col_set(col - 1);
    }

    // CSI H, and CSI f (HVP; ratty-vt)
    pub(crate) fn cup(&mut self, (row, col): (u16, u16)) {
        self.grid_mut().set_pos(crate::ratty_vt::grid::Pos {
            row: row - 1,
            col: col - 1,
        });
    }

    // CSI J
    pub(crate) fn ed(&mut self, mode: u16, mut unhandled: impl FnMut(&mut Self)) {
        let attrs = self.attrs;
        match mode {
            0 => self.grid_mut().erase_all_forward(attrs),
            1 => self.grid_mut().erase_all_backward(attrs),
            2 => self.grid_mut().erase_all(attrs),
            _ => unhandled(self),
        }
    }

    // CSI ? J
    pub(crate) fn decsed(&mut self, mode: u16, unhandled: impl FnMut(&mut Self)) {
        self.ed(mode, unhandled);
    }

    // CSI K
    pub(crate) fn el(&mut self, mode: u16, mut unhandled: impl FnMut(&mut Self)) {
        let attrs = self.attrs;
        match mode {
            0 => self.grid_mut().erase_row_forward(attrs),
            1 => self.grid_mut().erase_row_backward(attrs),
            2 => self.grid_mut().erase_row(attrs),
            _ => unhandled(self),
        }
    }

    // CSI ? K
    pub(crate) fn decsel(&mut self, mode: u16, unhandled: impl FnMut(&mut Self)) {
        self.el(mode, unhandled);
    }

    // CSI L
    pub(crate) fn il(&mut self, count: u16) {
        self.grid_mut().insert_lines(count);
    }

    // CSI M
    pub(crate) fn dl(&mut self, count: u16) {
        self.grid_mut().delete_lines(count);
    }

    // CSI P
    pub(crate) fn dch(&mut self, count: u16) {
        self.grid_mut().delete_cells(count);
    }

    // CSI S
    pub(crate) fn su(&mut self, count: u16) {
        self.grid_mut().scroll_up(count);
    }

    // CSI T
    pub(crate) fn sd(&mut self, count: u16) {
        self.grid_mut().scroll_down(count);
    }

    // CSI X
    pub(crate) fn ech(&mut self, count: u16) {
        let attrs = self.attrs;
        self.grid_mut().erase_cells(count, attrs);
    }

    // CSI d
    pub(crate) fn vpa(&mut self, row: u16) {
        self.grid_mut().row_set(row - 1);
    }

    // CSI ? h
    pub(crate) fn decset(&mut self, params: &vte::Params, mut unhandled: impl FnMut(&mut Self)) {
        for param in params {
            match param {
                [1] => self.set_mode(MODE_APPLICATION_CURSOR),
                [6] => self.grid_mut().set_origin_mode(true),
                [9] => self.set_mouse_mode(MouseProtocolMode::Press),
                [25] => self.clear_mode(MODE_HIDE_CURSOR),
                [47] => self.enter_alternate_grid(),
                [1000] => {
                    self.set_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    self.set_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => self.set_mouse_mode(MouseProtocolMode::AnyMotion),
                [1005] => {
                    self.set_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    self.set_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                [1049] => {
                    self.decsc();
                    self.alternate_grid.clear();
                    self.enter_alternate_grid();
                }
                [2004] => self.set_mode(MODE_BRACKETED_PASTE),
                _ => unhandled(self),
            }
        }
    }

    // CSI ? l
    pub(crate) fn decrst(&mut self, params: &vte::Params, mut unhandled: impl FnMut(&mut Self)) {
        for param in params {
            match param {
                [1] => self.clear_mode(MODE_APPLICATION_CURSOR),
                [6] => self.grid_mut().set_origin_mode(false),
                [9] => self.clear_mouse_mode(MouseProtocolMode::Press),
                [25] => self.set_mode(MODE_HIDE_CURSOR),
                [47] => {
                    self.exit_alternate_grid();
                }
                [1000] => {
                    self.clear_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    self.clear_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => {
                    self.clear_mouse_mode(MouseProtocolMode::AnyMotion);
                }
                [1005] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                [1049] => {
                    self.exit_alternate_grid();
                    self.decrc();
                }
                [2004] => self.clear_mode(MODE_BRACKETED_PASTE),
                _ => unhandled(self),
            }
        }
    }

    // CSI m
    pub(crate) fn sgr(&mut self, params: &vte::Params, mut unhandled: impl FnMut(&mut Self)) {
        // XXX really i want to just be able to pass in a default Params
        // instance with a 0 in it, but vte doesn't allow creating new Params
        // instances
        if params.is_empty() {
            self.attrs = crate::ratty_vt::attrs::Attrs::default();
            return;
        }

        let mut iter = params.iter();

        macro_rules! next_param {
            () => {
                match iter.next() {
                    Some(n) => n,
                    _ => return,
                }
            };
        }

        macro_rules! to_u8 {
            ($n:expr) => {
                if let Some(n) = u16_to_u8($n) {
                    n
                } else {
                    return;
                }
            };
        }

        macro_rules! next_param_u8 {
            () => {
                if let &[n] = next_param!() {
                    to_u8!(n)
                } else {
                    return;
                }
            };
        }

        loop {
            match next_param!() {
                [0] => self.attrs = crate::ratty_vt::attrs::Attrs::default(),
                [1] => self.attrs.set_bold(),
                [2] => self.attrs.set_dim(),
                [3] => self.attrs.set_italic(true),
                [4] => self.attrs.set_underline(true),
                // ratty-vt: blink.
                [5] => self.attrs.set_blink(crate::ratty_vt::Blink::Slow),
                [6] => self.attrs.set_blink(crate::ratty_vt::Blink::Rapid),
                [7] => self.attrs.set_inverse(true),
                // ratty-vt: hidden and strikeout.
                [8] => self.attrs.set_hidden(true),
                [9] => self.attrs.set_strikeout(true),
                [22] => self.attrs.set_normal_intensity(),
                [23] => self.attrs.set_italic(false),
                [24] => self.attrs.set_underline(false),
                // ratty-vt: blink.
                [25] => self.attrs.set_blink(crate::ratty_vt::Blink::None),
                [27] => self.attrs.set_inverse(false),
                // ratty-vt: hidden and strikeout.
                [28] => self.attrs.set_hidden(false),
                [29] => self.attrs.set_strikeout(false),
                [n] if (30..=37).contains(n) => {
                    self.attrs.fgcolor = crate::ratty_vt::Color::Idx(to_u8!(*n) - 30);
                }
                [38, 2, r, g, b] => {
                    self.attrs.fgcolor =
                        crate::ratty_vt::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [38, 5, i] => {
                    self.attrs.fgcolor = crate::ratty_vt::Color::Idx(to_u8!(*i));
                }
                [38] => match next_param!() {
                    [2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.fgcolor = crate::ratty_vt::Color::Rgb(r, g, b);
                    }
                    [5] => {
                        self.attrs.fgcolor = crate::ratty_vt::Color::Idx(next_param_u8!());
                    }
                    _ => {
                        unhandled(self);
                        return;
                    }
                },
                [39] => {
                    self.attrs.fgcolor = crate::ratty_vt::Color::Default;
                }
                [n] if (40..=47).contains(n) => {
                    self.attrs.bgcolor = crate::ratty_vt::Color::Idx(to_u8!(*n) - 40);
                }
                [48, 2, r, g, b] => {
                    self.attrs.bgcolor =
                        crate::ratty_vt::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [48, 5, i] => {
                    self.attrs.bgcolor = crate::ratty_vt::Color::Idx(to_u8!(*i));
                }
                [48] => match next_param!() {
                    [2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.bgcolor = crate::ratty_vt::Color::Rgb(r, g, b);
                    }
                    [5] => {
                        self.attrs.bgcolor = crate::ratty_vt::Color::Idx(next_param_u8!());
                    }
                    _ => {
                        unhandled(self);
                        return;
                    }
                },
                [49] => {
                    self.attrs.bgcolor = crate::ratty_vt::Color::Default;
                }
                // ratty-vt: underline color (SGR 58 / 59), same parameter
                // shapes as SGR 38 / 48.
                [58, 2, r, g, b] => {
                    self.attrs.underline_color =
                        crate::ratty_vt::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [58, 5, i] => {
                    self.attrs.underline_color = crate::ratty_vt::Color::Idx(to_u8!(*i));
                }
                [58] => match next_param!() {
                    [2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.underline_color = crate::ratty_vt::Color::Rgb(r, g, b);
                    }
                    [5] => {
                        self.attrs.underline_color = crate::ratty_vt::Color::Idx(next_param_u8!());
                    }
                    _ => {
                        unhandled(self);
                        return;
                    }
                },
                [59] => {
                    self.attrs.underline_color = crate::ratty_vt::Color::Default;
                }
                [n] if (90..=97).contains(n) => {
                    self.attrs.fgcolor = crate::ratty_vt::Color::Idx(to_u8!(*n) - 82);
                }
                [n] if (100..=107).contains(n) => {
                    self.attrs.bgcolor = crate::ratty_vt::Color::Idx(to_u8!(*n) - 92);
                }
                _ => unhandled(self),
            }
        }
    }

    // CSI r
    pub(crate) fn decstbm(&mut self, (top, bottom): (u16, u16)) {
        self.grid_mut().set_scroll_region(top - 1, bottom - 1);
    }

    // ratty-vt: kitty keyboard protocol.
    //
    // Only the first parameter carries flags; kitty defines the value as a
    // bit set below 32 but tolerates larger values, so saturate rather than
    // reject.

    // CSI > flags u
    pub(crate) fn kitty_keyboard_push(&mut self, flags: u16) {
        let flags = u8::try_from(flags).unwrap_or(u8::MAX);
        let stack = self.kitty_keyboard_stack_mut();
        if stack.len() >= KITTY_KEYBOARD_STACK_LIMIT {
            stack.remove(0);
        }
        stack.push(flags);
    }

    // CSI < n u
    pub(crate) fn kitty_keyboard_pop(&mut self, count: u16) {
        let stack = self.kitty_keyboard_stack_mut();
        for _ in 0..count.max(1) {
            if stack.pop().is_none() {
                break;
            }
        }
    }

    // CSI = flags ; mode u
    pub(crate) fn kitty_keyboard_set(&mut self, flags: u16, mode: u16) {
        let flags = u8::try_from(flags).unwrap_or(u8::MAX);
        let stack = self.kitty_keyboard_stack_mut();
        if stack.is_empty() {
            stack.push(0);
        }
        let current = stack.last_mut().unwrap();
        *current = match mode {
            2 => *current | flags,
            3 => *current & !flags,
            _ => flags,
        };
    }

    // CSI > 4 ; level m
    pub(crate) fn modify_other_keys_set(&mut self, level: Option<u16>) {
        self.modify_other_keys = match level {
            None | Some(0) => None,
            Some(level) => Some(u8::try_from(level).unwrap_or(u8::MAX)),
        };
    }
}

fn u16_to_u8(i: u16) -> Option<u8> {
    if i > u16::from(u8::MAX) {
        None
    } else {
        // safe because we just ensured that the value fits in a u8
        Some(i.try_into().unwrap())
    }
}

#[cfg(test)]
mod ratty_tests {
    //! ratty-vt engine patch tests. Upstream's suite lives in `super::super::tests`.
    use crate::ratty_vt::{Blink, Parser};

    #[test]
    fn visible_row_matches_the_iterator_at_every_scrollback_offset() {
        let mut parser = Parser::new(3, 10, 20);
        for line in 0..12 {
            parser.process(format!("row{line}\r\n").as_bytes());
        }
        for offset in 0..=parser.screen().scrollback() + 5 {
            parser.screen_mut().set_scrollback(offset);
            let expected: Vec<String> = parser
                .screen()
                .visible_rows()
                .map(|row| row.get(0).unwrap().contents().to_string())
                .collect();
            for (index, text) in expected.iter().enumerate() {
                let row = parser.screen().visible_row(index as u16).unwrap();
                assert_eq!(
                    row.get(0).unwrap().contents(),
                    text,
                    "offset {offset} row {index}"
                );
            }
            assert!(parser.screen().visible_row(3).is_none());
            assert_eq!(expected.len(), 3);
        }
        // The last screen row after scrolling all the way back is scrollback.
        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(parser.screen().scrollback(), 10);
        assert_eq!(
            parser
                .screen()
                .visible_row(0)
                .unwrap()
                .get(0)
                .unwrap()
                .contents(),
            "r"
        );
        assert_eq!(parser.screen().visible_row(0).unwrap().cells().count(), 10);
    }

    #[test]
    fn hvp_positions_the_cursor_like_cup() {
        let mut parser = Parser::new(5, 10, 0);
        parser.process(b"\x1b[3;4fX");
        assert_eq!(parser.screen().cell(2, 3).unwrap().contents(), "X");
        assert_eq!(parser.screen().cursor_position(), (2, 4));

        parser.process(b"\x1b[fY");
        assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "Y");

        // Origin mode applies to HVP exactly as it does to CUP.
        parser.process(b"\x1b[2;4r\x1b[?6h\x1b[1;1fZ");
        assert_eq!(parser.screen().cell(1, 0).unwrap().contents(), "Z");
    }

    #[test]
    fn sgr_blink_is_parsed_and_stored_on_cells() {
        let mut parser = Parser::new(2, 10, 0);
        parser.process(b"a\x1b[5mb\x1b[6mc\x1b[25md\x1b[5m\x1b[me");
        let cell = |col| parser.screen().cell(0, col).unwrap().blink();
        assert_eq!(cell(0), Blink::None);
        assert_eq!(cell(1), Blink::Slow);
        assert_eq!(cell(2), Blink::Rapid);
        assert_eq!(cell(3), Blink::None);
        assert_eq!(cell(4), Blink::None, "SGR 0 resets blink");
        assert_eq!(parser.screen().blink(), Blink::None);
    }

    #[test]
    fn scosc_and_scorc_save_and_restore_the_cursor_position() {
        let mut parser = Parser::new(5, 20, 0);
        parser.process(b"\x1b[2;3H\x1b[s\x1b[1mtext\x1b[4;10Hmore\x1b[u");
        assert_eq!(parser.screen().cursor_position(), (1, 2));
        // Unlike DECRC, SCORC leaves the attributes alone.
        assert!(parser.screen().bold());
        parser.process(b"X");
        assert_eq!(parser.screen().cell(1, 2).unwrap().contents(), "X");
    }

    #[test]
    fn sgr_hidden_strikeout_and_underline_color_are_stored_on_cells() {
        let mut parser = Parser::new(2, 12, 0);
        parser.process(b"a\x1b[8mb\x1b[28m\x1b[9mc\x1b[29m\x1b[58;2;1;2;3md\x1b[58;5;9me\x1b[59mf\x1b[8;9;58:2:4:5:6mg\x1b[mh");
        use crate::ratty_vt::Color;
        let cell = |col| parser.screen().cell(0, col).unwrap();
        assert!(!cell(0).hidden() && !cell(0).strikeout());
        assert!(cell(1).hidden());
        assert!(!cell(2).hidden() && cell(2).strikeout());
        assert!(!cell(3).strikeout());
        assert_eq!(cell(3).underline_color(), Color::Rgb(1, 2, 3));
        assert_eq!(cell(4).underline_color(), Color::Idx(9));
        assert_eq!(cell(5).underline_color(), Color::Default);
        assert!(cell(6).hidden() && cell(6).strikeout());
        assert_eq!(cell(6).underline_color(), Color::Rgb(4, 5, 6));
        assert!(!cell(7).hidden() && !cell(7).strikeout(), "SGR 0 resets");
        assert_eq!(cell(7).underline_color(), Color::Default);
        assert!(!parser.screen().hidden());
    }

    #[test]
    fn hidden_strikeout_and_underline_color_round_trip_through_contents_formatted() {
        let mut parser = Parser::new(2, 10, 0);
        parser.process(b"\x1b[8ma\x1b[28;9mb\x1b[29;58;2;1;2;3mc\x1b[59md");
        let formatted = parser.screen().contents_formatted();
        let mut replay = Parser::new(2, 10, 0);
        replay.process(&formatted);
        use crate::ratty_vt::Color;
        let cell = |col| replay.screen().cell(0, col).unwrap();
        assert!(cell(0).hidden());
        assert!(!cell(1).hidden() && cell(1).strikeout());
        assert!(!cell(2).strikeout());
        assert_eq!(cell(2).underline_color(), Color::Rgb(1, 2, 3));
        assert_eq!(cell(3).underline_color(), Color::Default);
        assert!(
            formatted.windows(4).any(|w| w == b"\x1b[8m"),
            "{formatted:?}"
        );
    }

    #[test]
    fn blink_round_trips_through_contents_formatted() {
        let mut parser = Parser::new(2, 10, 0);
        parser.process(b"\x1b[5mab\x1b[6mc\x1b[25md");
        let formatted = parser.screen().contents_formatted();
        assert!(
            formatted.windows(4).any(|w| w == b"\x1b[5m"),
            "{formatted:?}"
        );
        assert!(
            formatted.windows(4).any(|w| w == b"\x1b[6m"),
            "{formatted:?}"
        );

        let mut replay = Parser::new(2, 10, 0);
        replay.process(&formatted);
        for col in 0..5 {
            assert_eq!(
                replay.screen().cell(0, col).unwrap().blink(),
                parser.screen().cell(0, col).unwrap().blink(),
                "column {col}"
            );
        }
        assert_eq!(replay.screen().blink(), parser.screen().blink());
    }

    #[test]
    fn blink_diff_emits_reset_when_cleared() {
        let mut parser = Parser::new(2, 10, 0);
        parser.process(b"\x1b[5ma");
        let prev = parser.screen().clone();
        // Bold keeps the attributes off the all-default fast path, which
        // emits a bare SGR 0 instead of a per-attribute reset.
        parser.process(b"\x1b[1;25mb");
        let diff = parser.screen().contents_diff(&prev);
        let text = String::from_utf8_lossy(&diff);
        assert!(text.contains("\x1b[1;25m"), "{text:?}");
    }
}

#[cfg(test)]
mod ratty_keyboard_tests {
    use crate::ratty_vt::Parser;

    #[test]
    fn kitty_keyboard_flags_push_set_and_pop() {
        let mut parser = Parser::new(5, 20, 0);
        assert_eq!(parser.screen().kitty_keyboard_flags(), 0);

        for (requested, expected) in [(1_u16, 1_u8), (3, 3), (31, 31)] {
            parser.process(format!("\x1b[>{requested}u").as_bytes());
            assert_eq!(parser.screen().kitty_keyboard_flags(), expected);
            parser.process(b"\x1b[<1u");
        }
        assert_eq!(parser.screen().kitty_keyboard_flags(), 0);

        // Nested pushes pop back to the previous level, and `CSI < u` with no
        // parameter pops one entry.
        parser.process(b"\x1b[>1u\x1b[>5u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 5);
        parser.process(b"\x1b[<u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 1);
        parser.process(b"\x1b[<10u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 0);

        // `CSI = flags ; mode u`: 1 sets, 2 ors in, 3 masks out.
        parser.process(b"\x1b[=3;1u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 3);
        parser.process(b"\x1b[=4;2u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 7);
        parser.process(b"\x1b[=1;3u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 6);
        parser.process(b"\x1b[=8u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 8);
    }

    #[test]
    fn kitty_keyboard_stacks_are_per_screen() {
        let mut parser = Parser::new(5, 20, 0);
        parser.process(b"\x1b[>1u");
        parser.process(b"\x1b[?1049h\x1b[>15u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 15);
        parser.process(b"\x1b[?1049l");
        assert_eq!(
            parser.screen().kitty_keyboard_flags(),
            1,
            "leaving the alternate screen restores the main screen's flags"
        );
    }

    #[test]
    fn kitty_keyboard_query_reaches_the_callbacks() {
        #[derive(Default)]
        struct Seen(Vec<(Option<u8>, char, Vec<Vec<u16>>)>);
        impl crate::ratty_vt::Callbacks for Seen {
            fn unhandled_csi(
                &mut self,
                _: &mut crate::ratty_vt::Screen,
                i1: Option<u8>,
                _: Option<u8>,
                params: &[&[u16]],
                c: char,
            ) {
                self.0
                    .push((i1, c, params.iter().map(|p| p.to_vec()).collect()));
            }
        }
        let mut parser = Parser::new_with_callbacks(5, 20, 0, Seen::default());
        parser.process(b"\x1b[>3u\x1b[?u");
        assert_eq!(parser.screen().kitty_keyboard_flags(), 3);
        assert_eq!(parser.callbacks().0, vec![(Some(b'?'), 'u', vec![vec![0]])]);
    }

    /// Level 0 means disabled and must read as `None`; a key encoder that
    /// treats any `Some` as enabled would otherwise mis-encode Ctrl+Enter.
    #[test]
    fn modify_other_keys_levels() {
        let mut parser = Parser::new(5, 20, 0);
        assert_eq!(parser.screen().modify_other_keys(), None);
        parser.process(b"\x1b[>4;2m");
        assert_eq!(parser.screen().modify_other_keys(), Some(2));
        parser.process(b"\x1b[>4;0m");
        assert_eq!(parser.screen().modify_other_keys(), None);
        parser.process(b"\x1b[>4;1m");
        assert_eq!(parser.screen().modify_other_keys(), Some(1));
        parser.process(b"\x1b[>4m");
        assert_eq!(parser.screen().modify_other_keys(), None);
    }

    /// Split across PTY reads, which a byte sniffer could not handle.
    #[test]
    fn modify_other_keys_survives_a_split_sequence() {
        let sequence = b"\x1b[>4;2m";
        for split in 1..sequence.len() {
            let mut parser = Parser::new(5, 20, 0);
            parser.process(&sequence[..split]);
            parser.process(&sequence[split..]);
            assert_eq!(
                parser.screen().modify_other_keys(),
                Some(2),
                "split at {split} was missed"
            );
        }
    }

    /// Other `CSI > ... m` resources (e.g. xterm's `CSI > 1 m`) are not ours
    /// and must still reach the callbacks.
    #[test]
    fn other_private_sgr_resources_stay_unhandled() {
        #[derive(Default)]
        struct Count(usize);
        impl crate::ratty_vt::Callbacks for Count {
            fn unhandled_csi(
                &mut self,
                _: &mut crate::ratty_vt::Screen,
                _: Option<u8>,
                _: Option<u8>,
                _: &[&[u16]],
                _: char,
            ) {
                self.0 += 1;
            }
        }
        let mut parser = Parser::new_with_callbacks(5, 20, 0, Count::default());
        parser.process(b"\x1b[>1;2m\x1b[>4;2m");
        assert_eq!(parser.callbacks().0, 1);
        assert_eq!(parser.screen().modify_other_keys(), Some(2));
    }
}

#[cfg(test)]
mod ratty_cursor_tests {
    use crate::ratty_vt::Parser;

    #[test]
    fn display_cursor_clamps_the_pending_wrap_column() {
        let mut parser = Parser::new(2, 4, 0);
        parser.process(b"abcd");
        assert_eq!(parser.screen().cursor_position(), (0, 4));
        assert_eq!(parser.screen().display_cursor_position(), (0, 3));
    }

    #[test]
    fn display_cursor_snaps_off_a_wide_continuation_cell() {
        let mut parser = Parser::new(2, 6, 0);
        parser.process("\u{4f60}\x1b[1;2H".as_bytes());
        assert_eq!(parser.screen().cursor_position(), (0, 1));
        assert_eq!(parser.screen().display_cursor_position(), (0, 0));
    }

    #[test]
    fn cursor_hides_under_dectcem_and_while_scrolled_back() {
        let mut parser = Parser::new(3, 20, 100);
        assert!(!parser.screen().cursor_hidden());
        parser.process(b"\x1b[?25l");
        assert!(parser.screen().cursor_hidden());
        parser.process(b"\x1b[?25h");
        assert!(!parser.screen().cursor_hidden());

        for line in 0..10 {
            parser.process(format!("row{line}\r\n").as_bytes());
        }
        parser.screen_mut().set_scrollback(3);
        assert!(parser.screen().cursor_hidden());
        parser.screen_mut().set_scrollback(0);
        assert!(!parser.screen().cursor_hidden());
    }

    #[test]
    fn rows_remember_kitty_placeholders_until_cleared() {
        let mut parser = Parser::new(3, 10, 0);
        parser.process("ab\r\n\u{10EEEE}\r\n".as_bytes());
        assert!(
            !parser
                .screen()
                .visible_row(0)
                .unwrap()
                .has_kitty_placeholder()
        );
        assert!(
            parser
                .screen()
                .visible_row(1)
                .unwrap()
                .has_kitty_placeholder()
        );
        parser.process(b"\x1b[2;1H\x1b[2K");
        assert!(
            !parser
                .screen()
                .visible_row(1)
                .unwrap()
                .has_kitty_placeholder()
        );
    }
}

#[cfg(test)]
mod ratty_resize_tests {
    use crate::ratty_vt::Parser;

    fn rows(parser: &Parser) -> Vec<String> {
        let (_, cols) = parser.screen().size();
        parser.screen().rows(0, cols).collect()
    }

    #[test]
    fn narrowing_reflows_a_long_line() {
        let mut parser = Parser::new(4, 20, 100);
        parser.process(b"0123456789abcdefghij\r\nnext");
        parser.screen_mut().set_size_reflow(4, 8);
        assert_eq!(rows(&parser), vec!["01234567", "89abcdef", "ghij", "next"]);
        assert!(parser.screen().row_wrapped(0));
        assert!(parser.screen().row_wrapped(1));
        assert!(!parser.screen().row_wrapped(2));
        assert_eq!(parser.screen().cursor_position(), (3, 4));

        // Widening joins the pieces back together.
        parser.screen_mut().set_size_reflow(4, 30);
        assert_eq!(rows(&parser), vec!["0123456789abcdefghij", "next", "", ""]);
        assert_eq!(parser.screen().cursor_position(), (1, 4));
    }

    #[test]
    fn reflow_keeps_the_cursor_on_its_character() {
        let mut parser = Parser::new(3, 10, 100);
        parser.process(b"abcdefghij\x1b[1;7H");
        assert_eq!(parser.screen().cursor_position(), (0, 6));
        parser.screen_mut().set_size_reflow(3, 4);
        // "abcd" / "efgh" / "ij": 'g' is row 1, col 2.
        assert_eq!(parser.screen().cursor_position(), (1, 2));
        assert_eq!(parser.screen().cell(1, 2).unwrap().contents(), "g");
    }

    #[test]
    fn reflow_pushes_overflow_into_scrollback_and_pulls_it_back() {
        let mut parser = Parser::new(3, 12, 100);
        parser.process(b"aaaaaaaaaaaa\r\nbb\r\ncc");
        assert_eq!(rows(&parser), vec!["aaaaaaaaaaaa", "bb", "cc"]);

        parser.screen_mut().set_size_reflow(3, 6);
        assert_eq!(rows(&parser), vec!["aaaaaa", "bb", "cc"]);
        parser.screen_mut().set_scrollback(1);
        assert_eq!(rows(&parser), vec!["aaaaaa", "aaaaaa", "bb"]);
        parser.screen_mut().set_scrollback(0);

        parser.screen_mut().set_size_reflow(3, 12);
        assert_eq!(rows(&parser), vec!["aaaaaaaaaaaa", "bb", "cc"]);
        assert_eq!(parser.screen().cursor_position(), (2, 2));
    }

    #[test]
    fn shrinking_rows_scrolls_into_history_and_growing_recovers_it() {
        let mut parser = Parser::new(4, 10, 100);
        parser.process(b"one\r\ntwo\r\nthree\r\nfour");
        parser.screen_mut().set_size_reflow(2, 10);
        assert_eq!(rows(&parser), vec!["three", "four"]);
        assert_eq!(parser.screen().cursor_position(), (1, 4));

        parser.screen_mut().set_size_reflow(5, 10);
        assert_eq!(rows(&parser), vec!["one", "two", "three", "four", ""]);
        assert_eq!(parser.screen().cursor_position(), (3, 4));
    }

    #[test]
    fn shrinking_rows_drops_blank_rows_below_the_cursor_first() {
        let mut parser = Parser::new(6, 10, 100);
        parser.process(b"top\x1b[2;1Hmid");
        parser.screen_mut().set_size_reflow(3, 10);
        assert_eq!(rows(&parser), vec!["top", "mid", ""]);
        assert_eq!(parser.screen().cursor_position(), (1, 3));
    }

    #[test]
    fn resize_resets_the_scroll_region() {
        let mut parser = Parser::new(10, 20, 0);
        parser.process(b"\x1b[2;5r");
        parser.screen_mut().set_size_reflow(10, 21);
        // With the region reset, a linefeed on the last row scrolls the
        // whole screen rather than rows 2-5.
        parser.process(b"\x1b[10;1Hlast\r\nafter");
        assert_eq!(rows(&parser)[8], "last");
        assert_eq!(rows(&parser)[9], "after");
    }

    #[test]
    fn resizing_while_scrolled_back_keeps_every_row_readable() {
        for (rows_after, cols_after) in [(3_u16, 40_u16), (12, 10), (24, 80), (4, 20)] {
            let mut parser = Parser::new(6, 40, 100);
            for line in 0..60 {
                parser.process(format!("row{line}\r\n").as_bytes());
            }
            parser.screen_mut().set_scrollback(usize::MAX);
            let before = parser.screen().scrollback();
            assert!(before > 0);

            parser.screen_mut().set_size_reflow(rows_after, cols_after);

            let (rows_now, _) = parser.screen().size();
            assert_eq!(rows_now, rows_after);
            for row in 0..rows_now {
                assert!(
                    parser.screen().visible_row(row).is_some(),
                    "row {row} unreadable after resizing to {cols_after}x{rows_after}"
                );
            }
            let _ = rows(&parser);
        }
    }

    #[test]
    fn reflow_does_not_split_wide_glyphs() {
        let mut parser = Parser::new(2, 10, 100);
        parser.process("ab\u{4f60}\u{597d}cd".as_bytes());
        parser.screen_mut().set_size_reflow(4, 3);
        // "ab" + pad, "你", "好c", "d"
        assert_eq!(rows(&parser), vec!["ab", "\u{4f60}", "\u{597d}c", "d"]);
        assert!(parser.screen().cell(1, 0).unwrap().is_wide());
        assert!(parser.screen().cell(1, 1).unwrap().is_wide_continuation());
        assert_eq!(parser.screen().cursor_position(), (3, 1));
    }

    #[test]
    fn alternate_screen_resizes_without_reflow() {
        let mut parser = Parser::new(3, 10, 100);
        parser.process(b"main line\x1b[?1049h0123456789");
        parser.screen_mut().set_size_reflow(3, 5);
        assert_eq!(rows(&parser), vec!["01234", "", ""]);
        parser.process(b"\x1b[?1049l");
        assert_eq!(rows(&parser), vec!["main ", "line", ""]);
    }

    #[test]
    fn degenerate_grids_do_not_panic() {
        for (rows_n, cols_n) in [(1_u16, 1_u16), (1, 40), (40, 1), (2, 2)] {
            let mut parser = Parser::new(rows_n, cols_n, 10);
            parser.process("\u{4f60}\u{597d}ab\r\n\u{1f600}x".as_bytes());
            parser.screen_mut().set_size_reflow(1, 1);
            parser.process("\u{4f60}z".as_bytes());
            parser.screen_mut().set_size_reflow(rows_n, cols_n);
            let _ = parser.screen().contents_formatted();
            let _ = parser.screen().display_cursor_position();
        }
    }
}

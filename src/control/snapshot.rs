use std::collections::HashMap;

use fux_vt::{Blink, Color as VtColor, MouseProtocolEncoding, MouseProtocolMode};
use serde::{Deserialize, Serialize};

use crate::screen::ScreenView;

/// A terminal-cell rectangle used to restrict an observation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotRect {
    /// First absolute terminal row.
    pub row: u16,
    /// First absolute terminal column.
    pub column: u16,
    /// Number of rows to include.
    pub rows: u16,
    /// Number of columns to include.
    pub columns: u16,
}

/// Options for a canonical screen observation.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotRequest {
    /// Optional terminal-cell rectangle.
    pub rect: Option<SnapshotRect>,
    /// Include the style table used by run style indices.
    #[serde(default)]
    pub include_styles: bool,
    /// Include a convenience plain-text rendering on each returned row.
    #[serde(default)]
    pub include_plain: bool,
}

/// Exact text and terminal width at an absolute screen position.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotRun {
    /// Absolute starting terminal column.
    pub column: u16,
    /// Width in terminal cells.
    pub columns: u16,
    /// Exact UTF-8 stored by ratty-vt for the displayed cell or cells.
    pub text: String,
    /// Index into the snapshot style table.
    pub style: u32,
}

/// One visible physical terminal row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotRow {
    /// Absolute terminal row.
    pub row: u16,
    /// Whether this physical row wraps into the next row.
    pub wrapped: bool,
    /// Whether the row contains a Kitty graphics placeholder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub graphics_placeholder: bool,
    /// Positioned cell runs.
    pub runs: Vec<SnapshotRun>,
    /// Optional convenience view derived only from positioned runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain: Option<String>,
}

/// A deduplicated terminal cell style.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct SnapshotStyle {
    /// Foreground color (`default`, `indexed:N`, or `rgb:R,G,B`).
    pub foreground: String,
    /// Background color (`default`, `indexed:N`, or `rgb:R,G,B`).
    pub background: String,
    /// Bold intensity.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    /// Dim intensity.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
    /// Italic text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    /// Underlined text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    /// Underline color (`default`, `indexed:N`, or `rgb:R,G,B`).
    pub underline_color: String,
    /// Inverse foreground/background.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inverse: bool,
    /// Blink mode (`none`, `slow`, or `rapid`).
    pub blink: String,
    /// Concealed text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Struck-through text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strikeout: bool,
}

impl Default for SnapshotStyle {
    fn default() -> Self {
        Self {
            foreground: "default".into(),
            background: "default".into(),
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            underline_color: "default".into(),
            inverse: false,
            blink: "none".into(),
            hidden: false,
            strikeout: false,
        }
    }
}

/// Display cursor returned to agents.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotCursor {
    /// Absolute terminal row.
    pub row: u16,
    /// Absolute terminal column.
    pub column: u16,
    /// Whether the application text cursor is visible.
    pub visible: bool,
}

/// Input modes that affect keyboard, paste, and mouse behavior.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotModes {
    /// Alternate-screen buffer is active.
    pub alternate_screen: bool,
    /// Application cursor-key mode is active.
    pub application_cursor: bool,
    /// Application keypad mode is active.
    pub application_keypad: bool,
    /// Bracketed-paste mode is active.
    pub bracketed_paste: bool,
    /// Active terminal mouse protocol.
    pub mouse_protocol: String,
    /// Active terminal mouse encoding.
    pub mouse_encoding: String,
    /// Kitty keyboard protocol flags.
    pub kitty_keyboard_flags: u8,
    /// xterm modifyOtherKeys level.
    pub modify_other_keys: Option<u8>,
}

/// Canonical structured view of Ratty's visible terminal grid.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScreenSnapshot {
    /// Monotonic revision incremented when terminal output, size, or scrollback changes.
    pub screen_revision: u64,
    /// Full screen width in terminal columns.
    pub columns: u16,
    /// Full screen height in terminal rows.
    pub rows: u16,
    /// Requested visible rows containing content, visible styling, or wrapping.
    pub content: Vec<SnapshotRow>,
    /// Deduplicated styles when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub styles: Option<Vec<SnapshotStyle>>,
    /// Display cursor.
    pub cursor: SnapshotCursor,
    /// Input modes affecting interaction.
    pub modes: SnapshotModes,
}

impl ScreenSnapshot {
    pub(super) fn from_screen(
        screen: ScreenView<'_>,
        request: &SnapshotRequest,
        screen_revision: u64,
    ) -> Self {
        let (rows, columns) = screen.size();
        let (row_start, row_end, column_start, column_end) =
            request.rect.map_or((0, rows, 0, columns), |rect| {
                (
                    rect.row.min(rows),
                    rect.row.saturating_add(rect.rows).min(rows),
                    rect.column.min(columns),
                    rect.column.saturating_add(rect.columns).min(columns),
                )
            });
        let default_style = SnapshotStyle::default();
        let mut styles = vec![default_style.clone()];
        let mut style_indices = HashMap::from([(default_style.clone(), 0_u32)]);
        let mut content = Vec::new();

        for row_index in row_start..row_end {
            let Some(row) = screen.visible_row(row_index) else {
                continue;
            };
            let mut runs: Vec<SnapshotRun> = Vec::new();
            let mut graphics_placeholder = false;
            for column in 0..columns {
                let Some(cell) = row.cell(usize::from(column)) else {
                    break;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let width = if cell.is_wide() { 2 } else { 1 };
                if column >= column_end || column.saturating_add(width) <= column_start {
                    continue;
                }

                let placeholder = cell
                    .contents()
                    .starts_with(crate::screen::KITTY_PLACEHOLDER);
                graphics_placeholder |= placeholder;
                let style = snapshot_style(cell);
                let meaningful_style = style != default_style;
                if !cell.has_contents() && !meaningful_style {
                    continue;
                }
                if placeholder && !meaningful_style {
                    continue;
                }
                let text = if placeholder || !cell.has_contents() {
                    " ".to_owned()
                } else {
                    cell.contents().to_owned()
                };
                let style_index = *style_indices.entry(style.clone()).or_insert_with(|| {
                    let index = u32::try_from(styles.len()).unwrap_or(u32::MAX);
                    styles.push(style);
                    index
                });
                let printable_ascii = width == 1
                    && !text.is_empty()
                    && text
                        .as_bytes()
                        .iter()
                        .all(|byte| (0x20..=0x7e).contains(byte));
                if printable_ascii
                    && let Some(previous) = runs.last_mut()
                    && previous.style == style_index
                    && previous.column.saturating_add(previous.columns) == column
                    && previous
                        .text
                        .as_bytes()
                        .iter()
                        .all(|byte| (0x20..=0x7e).contains(byte))
                {
                    previous.text.push_str(&text);
                    previous.columns = previous.columns.saturating_add(1);
                    continue;
                }
                runs.push(SnapshotRun {
                    column,
                    columns: width,
                    text,
                    style: style_index,
                });
            }

            if runs.is_empty() && !row.wrapped && !graphics_placeholder {
                continue;
            }
            let plain = request.include_plain.then(|| plain_from_runs(&runs));
            content.push(SnapshotRow {
                row: row_index,
                wrapped: row.wrapped,
                graphics_placeholder,
                runs,
                plain,
            });
        }

        let (cursor_row, cursor_column) = screen.display_cursor_position();
        Self {
            screen_revision,
            columns,
            rows,
            content,
            styles: request.include_styles.then_some(styles),
            cursor: SnapshotCursor {
                row: cursor_row,
                column: cursor_column,
                visible: !screen.cursor_hidden(),
            },
            modes: SnapshotModes {
                alternate_screen: screen.alternate_screen(),
                application_cursor: screen.application_cursor(),
                application_keypad: screen.application_keypad(),
                bracketed_paste: screen.bracketed_paste(),
                mouse_protocol: mouse_protocol_name(screen.mouse_protocol_mode()).into(),
                mouse_encoding: mouse_encoding_name(screen.mouse_protocol_encoding()).into(),
                kitty_keyboard_flags: screen.kitty_keyboard_flags(),
                modify_other_keys: screen.modify_other_keys(),
            },
        }
    }

    /// Produces a rectangular/style/plain view from an already canonical full
    /// snapshot without inspecting or re-segmenting run text.
    pub fn into_view(
        mut self,
        rect: Option<SnapshotRect>,
        include_styles: bool,
        include_plain: bool,
    ) -> Self {
        if let Some(rect) = rect {
            let row_end = rect.row.saturating_add(rect.rows).min(self.rows);
            let column_end = rect.column.saturating_add(rect.columns).min(self.columns);
            self.content.retain_mut(|row| {
                if row.row < rect.row || row.row >= row_end {
                    return false;
                }
                row.runs = row
                    .runs
                    .drain(..)
                    .filter_map(|run| clip_run_to_columns(run, rect.column, column_end))
                    .collect();
                !row.runs.is_empty() || row.wrapped || row.graphics_placeholder
            });
        }
        for row in &mut self.content {
            row.plain = include_plain.then(|| plain_from_runs(&row.runs));
        }
        if !include_styles {
            self.styles = None;
        }
        self
    }
}

fn clip_run_to_columns(
    mut run: SnapshotRun,
    column_start: u16,
    column_end: u16,
) -> Option<SnapshotRun> {
    let run_end = run.column.saturating_add(run.columns);
    if run.column >= column_end || run_end <= column_start {
        return None;
    }
    if run.column >= column_start && run_end <= column_end {
        return Some(run);
    }

    // Only printable ASCII cells are coalesced. Those runs have a one-byte,
    // one-column mapping, so clipping them cannot split Unicode text. Wide or
    // non-ASCII cells stay whole when either half intersects the rectangle.
    if run.columns as usize == run.text.len()
        && run
            .text
            .as_bytes()
            .iter()
            .all(|byte| (0x20..=0x7e).contains(byte))
    {
        let start = usize::from(column_start.saturating_sub(run.column));
        let end = usize::from(column_end.min(run_end).saturating_sub(run.column));
        run.text = run.text[start..end].to_owned();
        run.column = run
            .column
            .saturating_add(u16::try_from(start).unwrap_or(u16::MAX));
        run.columns = u16::try_from(end.saturating_sub(start)).unwrap_or(u16::MAX);
    }
    Some(run)
}

fn snapshot_style(cell: fux_vt::CellRef<'_>) -> SnapshotStyle {
    SnapshotStyle {
        foreground: snapshot_color(cell.fgcolor()),
        background: snapshot_color(cell.bgcolor()),
        bold: cell.bold(),
        dim: cell.dim(),
        italic: cell.italic(),
        underline: cell.underline(),
        underline_color: snapshot_color(cell.underline_color()),
        inverse: cell.inverse(),
        blink: match cell.blink() {
            Blink::None => "none",
            Blink::Slow => "slow",
            Blink::Rapid => "rapid",
        }
        .into(),
        hidden: cell.hidden(),
        strikeout: cell.strikeout(),
    }
}

fn snapshot_color(color: VtColor) -> String {
    match color {
        VtColor::Default => "default".into(),
        VtColor::Idx(index) => format!("indexed:{index}"),
        VtColor::Rgb(red, green, blue) => format!("rgb:{red},{green},{blue}"),
    }
}

pub(super) const fn mouse_protocol_name(mode: MouseProtocolMode) -> &'static str {
    match mode {
        MouseProtocolMode::None => "none",
        MouseProtocolMode::Press => "press",
        MouseProtocolMode::PressRelease => "press_release",
        MouseProtocolMode::ButtonMotion => "button_motion",
        MouseProtocolMode::AnyMotion => "any_motion",
    }
}

pub(super) const fn mouse_encoding_name(encoding: MouseProtocolEncoding) -> &'static str {
    match encoding {
        MouseProtocolEncoding::Default => "default",
        MouseProtocolEncoding::Utf8 => "utf8",
        MouseProtocolEncoding::Sgr => "sgr",
    }
}

fn plain_from_runs(runs: &[SnapshotRun]) -> String {
    let mut plain = String::new();
    let mut column = 0_u16;
    for run in runs {
        plain.extend(std::iter::repeat_n(
            ' ',
            usize::from(run.column.saturating_sub(column)),
        ));
        plain.push_str(&run.text);
        column = run.column.saturating_add(run.columns);
    }
    plain
}

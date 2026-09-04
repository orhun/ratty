//! Terminal surface rendering and Ratatui integration.

use std::fs;
use std::num::NonZeroU16;
use std::path::Path;

use anyhow::Context;
use bevy::prelude::*;
use bevy_terminal_ratatui::RatatuiTerminal;
use bevy_terminal_ratatui::prelude::{
    BlinkConfig, CellSizing, CursorConfig, CursorStyle, FontFaces, FontSizing, FontSource,
    RasterConfig, TerminalRenderConfig, TerminalRenderScale, TerminalTexture, TerminalTheme,
    font_family,
};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color as TuiColor, Modifier, Style};
use ratatui::widgets::Widget;

use crate::config::{AppConfig, FontConfig, FontStyleConfig, ThemeConfig};
use crate::mouse::TerminalSelection;
use crate::ratty_vt::{Blink, Cell as VtCell, Color as VtColor, Screen};

/// Terminal grid and presentation dimensions.
#[derive(Clone, Copy, Debug)]
pub struct TerminalLayout {
    /// Terminal column count.
    pub cols: u16,
    /// Terminal row count.
    pub rows: u16,
    /// Physical texture size in pixels.
    pub texture_size: UVec2,
    /// Logical presentation size in Bevy world units.
    pub logical_size: Vec2,
    /// Physical render scale used for the terminal texture.
    pub render_scale: f32,
}

impl TerminalLayout {
    fn new(cols: u16, rows: u16, texture_size: UVec2, render_scale: f32) -> Self {
        Self {
            cols,
            rows,
            texture_size,
            logical_size: texture_logical_size(texture_size, render_scale),
            render_scale,
        }
    }

    /// Returns PTY pixel dimensions clamped to portable-pty's `u16` API.
    pub fn pty_pixels(self) -> UVec2 {
        self.texture_size.min(UVec2::splat(u16::MAX as u32))
    }
}

/// Marks the Bevy entity that renders the application's terminal surface.
#[derive(Component)]
pub(crate) struct TerminalRenderTarget;

/// Font faces resolved from the configured system family or explicit files.
#[derive(Resource, Clone)]
pub struct ConfiguredFontFaces {
    pub(crate) faces: FontFaces,
    /// The system family `faces` names (`None` for explicit font files).
    /// Retained so the availability check validates the family actually
    /// pushed to the renderer, which an embedder may build from a different
    /// `FontConfig` than the inserted `AppConfig`.
    pub(crate) system_family: Option<String>,
}

/// Loads explicit font files into Bevy, or retains the configured system family.
///
/// # Errors
///
/// Returns an error when an explicit font file cannot be read or is not a
/// font, or when style faces are configured without `font.regular`.
pub fn load_configured_font_faces(
    app: &mut App,
    font: &FontConfig,
) -> anyhow::Result<ConfiguredFontFaces> {
    let explicit = [
        font.regular.as_deref(),
        font.bold.as_deref(),
        font.italic.as_deref(),
        font.bold_italic.as_deref(),
    ];
    if explicit.iter().all(Option::is_none) {
        return Ok(ConfiguredFontFaces {
            faces: FontFaces::regular(font_family(&font.family)),
            system_family: Some(font.family.clone()),
        });
    }

    let regular = font
        .regular
        .as_deref()
        .context("font.regular is required when explicit font files are configured")?;
    let regular = read_font_face(regular)?;
    let bold = font.bold.as_deref().map(read_font_face).transpose()?;
    let italic = font.italic.as_deref().map(read_font_face).transpose()?;
    let bold_italic = font
        .bold_italic
        .as_deref()
        .map(read_font_face)
        .transpose()?;

    let mut fonts = app.world_mut().resource_mut::<Assets<Font>>();
    let regular = fonts.add(regular);
    let bold = bold.map(|font| fonts.add(font));
    let italic = italic.map(|font| fonts.add(font));
    let bold_italic = bold_italic.map(|font| fonts.add(font));

    Ok(ConfiguredFontFaces {
        faces: FontFaces {
            regular: FontSource::Handle(regular),
            bold: bold.map(FontSource::Handle),
            italic: italic.map(FontSource::Handle),
            bold_italic: bold_italic.map(FontSource::Handle),
            synthesize: true,
        },
        system_family: None,
    })
}

fn read_font_face(path: &Path) -> anyhow::Result<Font> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read font face {}", path.display()))?;
    validate_font_face(path, &bytes)?;
    Ok(Font::from_bytes(bytes))
}

/// Rejects files that are not an OpenType/TrueType font before handing them to
/// Bevy, whose font registration silently ignores unparsable data and would
/// otherwise leave the terminal on the generic fallback with no explanation.
fn validate_font_face(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    const SFNT_TAGS: [[u8; 4]; 4] = [
        [0x00, 0x01, 0x00, 0x00], // TrueType outlines
        *b"OTTO",                 // CFF outlines
        *b"true",                 // Apple TrueType
        *b"ttcf",                 // TrueType collection
    ];
    let is_font = bytes
        .get(..4)
        .is_some_and(|magic| SFNT_TAGS.iter().any(|tag| tag == magic));
    anyhow::ensure!(
        is_font,
        "font face {} is not a TrueType or OpenType font file",
        path.display()
    );
    Ok(())
}

/// Terminal redraw flag.
#[derive(Resource)]
pub struct TerminalRedrawState {
    needs_redraw: bool,
}

impl Default for TerminalRedrawState {
    fn default() -> Self {
        Self { needs_redraw: true }
    }
}

impl TerminalRedrawState {
    /// Requests a terminal redraw.
    pub fn request(&mut self) {
        self.needs_redraw = true;
    }

    /// Returns whether a redraw was pending.
    pub fn take(&mut self) -> bool {
        std::mem::take(&mut self.needs_redraw)
    }
}

/// Terminal surface and render state.
#[derive(Resource)]
pub struct TerminalSurface {
    /// Ratatui terminal backend.
    pub tui: RatatuiTerminal,
    /// Front texture image handle (the renderer-owned terminal texture,
    /// sampled by the plane material and the 2D present quad).
    pub image_handle: Option<Handle<Image>>,
    /// Back texture image handle.
    pub back_image_handle: Option<Handle<Image>>,
    /// Terminal column count.
    pub cols: u16,
    /// Terminal row count.
    pub rows: u16,
    cursor_model_visible: bool,
    font_size: i32,
    render_config: TerminalRenderConfig,
    render_scale: f32,
    cell_size: Vec2,
    rendered_texture_size: Option<UVec2>,
}

impl TerminalSurface {
    /// Creates a terminal surface from the application config.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal backend cannot be initialized.
    pub fn new(config: &AppConfig) -> anyhow::Result<Self> {
        let cols = config.terminal.default_cols;
        let rows = config.terminal.default_rows;
        let (mut tui, _) = RatatuiTerminal::new(cols, rows);
        let Ok(()) = tui.clear();
        if config.cursor.model.visible {
            let Ok(()) = tui.hide_cursor();
        } else {
            let Ok(()) = tui.show_cursor();
        }
        // The real scale arrives with the first `resize_to_fit` once the
        // window exists; an explicit override seeds it early.
        let render_scale = config.window.scale_factor.unwrap_or(1.0).max(1.0);
        let render_config = build_terminal_render_config(
            &config.font,
            &config.theme,
            config.window.opacity,
            render_scale,
        );

        Ok(Self {
            tui,
            image_handle: None,
            back_image_handle: None,
            cols,
            rows,
            cursor_model_visible: config.cursor.model.visible,
            font_size: config.font.size,
            render_config,
            render_scale,
            // No geometry is inferred before the renderer measures the loaded font.
            cell_size: Vec2::ONE,
            rendered_texture_size: None,
        })
    }

    /// Adjusts the font size.
    ///
    /// The renderer remeasures the cell from the new size and reports it
    /// through [`TerminalTexture`]; the PTY reflow follows that report.
    pub fn adjust_font_size(&mut self, delta: i32) -> bool {
        let new_size = self.font_size.saturating_add(delta).max(1);
        if new_size == self.font_size {
            return false;
        }

        self.render_config.font_size = FontSizing::Px(points_to_logical_pixels(new_size));
        self.font_size = new_size;
        true
    }

    /// Returns the current font size.
    pub fn font_size(&self) -> i32 {
        self.font_size
    }

    /// Updates the physical render scale; returns whether it changed.
    pub(crate) fn set_render_scale(&mut self, render_scale: f32) -> bool {
        let render_scale = render_scale.max(1.0);
        if (render_scale - self.render_scale).abs() < f32::EPSILON {
            return false;
        }

        self.render_scale = render_scale;
        self.render_config.raster.scale = TerminalRenderScale::Fixed(render_scale);
        true
    }

    /// Resizes the terminal grid to fit a logical window size.
    pub fn resize_to_fit(&mut self, logical_size: Vec2, render_scale: f32) -> TerminalLayout {
        self.set_render_scale(render_scale);

        // The renderer sizes its texture with the same exported helper, so
        // the PTY grid and the rendered grid cannot disagree by a cell.
        let grid =
            bevy_terminal_ratatui::render::grid_for(logical_size.max(Vec2::ONE), self.cell_size);
        let (cols, rows) = (grid.width, grid.height);
        if cols != self.cols || rows != self.rows {
            self.resize(cols, rows);
        }

        self.layout()
    }

    /// Resizes the terminal grid.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }

        self.tui.resize_grid(cols, rows);
        if self.cursor_model_visible {
            let Ok(()) = self.tui.hide_cursor();
        } else {
            let Ok(()) = self.tui.show_cursor();
        }
        self.cols = cols;
        self.rows = rows;
    }

    /// Returns the rendered cell size in logical pixels.
    ///
    /// Always at least 1x1: every writer of `cell_size` (the constructor and
    /// `update_render_output`) floors it, so consumers need no re-clamp.
    pub fn char_dimensions(&self) -> Vec2 {
        self.cell_size
    }

    /// Whether the renderer has supplied authoritative font and cell metrics.
    pub fn is_measured(&self) -> bool {
        self.rendered_texture_size.is_some()
    }

    /// Returns the terminal pixmap dimensions in pixels.
    pub fn pixmap_dimensions(&self) -> UVec2 {
        (Vec2::new(self.cols as f32, self.rows as f32) * self.cell_size * self.render_scale)
            .round()
            .max(Vec2::ONE)
            .as_uvec2()
    }

    /// Returns the current terminal layout.
    pub(crate) fn layout(&self) -> TerminalLayout {
        TerminalLayout::new(
            self.cols,
            self.rows,
            self.pixmap_dimensions(),
            self.render_scale,
        )
    }

    /// Returns the render configuration derived from Ratty's settings.
    ///
    /// This is the template pushed to the renderer entity; the font faces may
    /// be swapped for explicit files or a fallback family before the push.
    pub const fn render_config(&self) -> &TerminalRenderConfig {
        &self.render_config
    }

    /// Adopts the metrics and stable image handle produced by the Bevy
    /// renderer, comparing first and writing only on change; returns whether
    /// anything changed.
    pub fn update_render_output(&mut self, texture: &TerminalTexture) -> bool {
        let cell_size = texture.cell_size.max(Vec2::ONE);
        let render_scale = texture.raster_scale.max(1.0);
        let changed = self.image_handle.as_ref() != Some(&texture.image)
            || self.rendered_texture_size != Some(texture.size)
            || self.cell_size != cell_size
            || self.render_scale != render_scale;
        if changed {
            self.image_handle = Some(texture.image.clone());
            self.rendered_texture_size = Some(texture.size);
            self.cell_size = cell_size;
            self.render_scale = render_scale;
        }
        changed
    }
}

/// Computes the physical render scale for a Bevy window.
///
/// Delegates to the renderer's exported helper so the scale the PTY layout
/// uses is the exact scale the renderer rasterizes with. It derives from the
/// window's actual framebuffer ratio rather than the reported scale factor,
/// which keeps mixed-DPI setups from over-sizing the texture.
pub fn render_scale_for_window(window: &Window) -> f32 {
    bevy_terminal_ratatui::render::raster_scale_for_window(window)
}

/// Returns the logical size for a physical terminal texture.
pub fn texture_logical_size(texture_size: UVec2, render_scale: f32) -> Vec2 {
    texture_size.as_vec2() / render_scale.max(1.0)
}

fn build_terminal_render_config(
    font: &FontConfig,
    theme_config: &ThemeConfig,
    window_opacity: f32,
    render_scale: f32,
) -> TerminalRenderConfig {
    let [fg_r, fg_g, fg_b] = theme_config.foreground;
    let [bg_r, bg_g, bg_b] = theme_config.background;
    let [cursor_r, cursor_g, cursor_b] = theme_config.cursor;
    let theme = TerminalTheme {
        foreground: Color::srgb_u8(fg_r, fg_g, fg_b),
        background: Color::srgba_u8(
            bg_r,
            bg_g,
            bg_b,
            (window_opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
        ),
        ansi: theme_config
            .palette()
            .map(|[r, g, b]| Color::srgb_u8(r, g, b)),
    };

    TerminalRenderConfig {
        // Cell width and height come from the loaded face's measured advance
        // and line box; Ratty supplies no independent geometry estimate.
        cell_size: CellSizing::FROM_FONT,
        font: FontFaces::regular(font_family(&font.family)),
        font_size: FontSizing::Px(points_to_logical_pixels(font.size)),
        theme,
        cursor: CursorConfig {
            style: CursorStyle::Block,
            color: Color::srgb_u8(cursor_r, cursor_g, cursor_b),
            blink_hz: None,
        },
        blink: BlinkConfig {
            slow_hz: Some(1.0),
            rapid_hz: Some(2.0),
        },
        raster: RasterConfig {
            scale: TerminalRenderScale::Fixed(render_scale.max(1.0)),
            ..default()
        },
    }
}

fn points_to_logical_pixels(points: i32) -> f32 {
    // A typographic point is 1/72 inch and a logical (CSS) pixel is 1/96
    // inch, so this converts units only; it estimates nothing about the font.
    const POINTS_PER_INCH: f32 = 72.0;
    const LOGICAL_PIXELS_PER_INCH: f32 = 96.0;
    (points as f32 * LOGICAL_PIXELS_PER_INCH / POINTS_PER_INCH).max(1.0)
}

/// Leading character of a Kitty graphics Unicode placeholder cell.
const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// Ratatui widget backed by the terminal screen.
pub struct TerminalWidget<'a> {
    /// Terminal state to render.
    pub screen: &'a Screen,
    /// Active selection.
    pub selection: &'a TerminalSelection,
    /// Terminal theme.
    pub theme: &'a ThemeConfig,
    /// Base font style override.
    pub font_style: FontStyleConfig,
}

impl Widget for TerminalWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let [fg_r, fg_g, fg_b] = self.theme.foreground;
        let theme_fg = TuiColor::Rgb(fg_r, fg_g, fg_b);
        let theme_palette = self.theme.palette().map(|[r, g, b]| TuiColor::Rgb(r, g, b));
        buf.set_style(area, Style::default().fg(theme_fg));

        let selection = self.selection.normalized_bounds();
        let (rows, cols) = self.screen.size();
        let draw_rows = rows.min(area.height);
        let draw_cols = cols.min(area.width);

        for row in 0..draw_rows {
            let Some(grid_row) = self.screen.visible_row(row) else {
                continue;
            };
            for col in 0..draw_cols {
                let Some(vt_cell) = grid_row.get(col) else {
                    break;
                };
                let cell = &mut buf[(area.x + col, area.y + row)];

                // Ratatui skips a wide glyph's trailing cell when diffing and
                // the Bevy backend synthesizes that continuation cell from the
                // anchor, so the anchor branch below owns the glyph's style.
                // The engine stores the continuation half with default
                // attributes; when it does reach the diff (behind a narrow
                // predecessor after an edit) it borrows the owner's style so a
                // background covers both halves of the glyph.
                if vt_cell.is_wide_continuation() {
                    let owner = col
                        .checked_sub(1)
                        .and_then(|left| grid_row.get(left))
                        .filter(|left| left.is_wide())
                        .unwrap_or(vt_cell);
                    let mut style = cell_style(owner, &theme_palette, theme_fg, self.font_style);
                    if selection.is_some_and(|bounds| bounds.contains(row, col)) {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    cell.set_symbol(" ")
                        .set_style(style)
                        .set_diff_option(forced_width(1));
                    continue;
                }

                // Kitty Unicode placeholders (U+10EEEE plus row/column
                // diacritics) only mark where an image goes; the image is
                // drawn as a separate scene object, so the cell must stay a
                // blank instead of rendering the placeholder as a glyph.
                let symbol = if vt_cell.contents().starts_with(KITTY_PLACEHOLDER) {
                    " "
                } else {
                    vt_cell.contents()
                };
                let mut style = cell_style(vt_cell, &theme_palette, theme_fg, self.font_style);
                let is_wide = vt_cell.is_wide();
                // A wide glyph renders as one unit: a selection touching
                // either of its columns highlights the whole glyph, since the
                // trailing continuation cell cannot carry its own style.
                if selection.is_some_and(|bounds| {
                    bounds.contains(row, col)
                        || (is_wide && col + 1 < cols && bounds.contains(row, col + 1))
                }) {
                    style = style.add_modifier(Modifier::REVERSED);
                }

                cell.set_symbol(if symbol.is_empty() { " " } else { symbol })
                    .set_style(style)
                    .set_diff_option(forced_width(if is_wide { 2 } else { 1 }));
            }
        }
    }
}

fn forced_width(width: u16) -> CellDiffOption {
    CellDiffOption::ForcedWidth(NonZeroU16::new(width).unwrap_or(NonZeroU16::MIN))
}

fn cell_style(
    cell: &VtCell,
    theme_palette: &[TuiColor; 16],
    theme_fg: TuiColor,
    font_style: FontStyleConfig,
) -> Style {
    let mut style =
        Style::default().fg(cell_color_to_tui(cell.fgcolor(), theme_palette).unwrap_or(theme_fg));
    if let Some(bg) = cell_color_to_tui(cell.bgcolor(), theme_palette) {
        style = style.bg(bg);
    }
    if let Some(underline) = cell_color_to_tui(cell.underline_color(), theme_palette) {
        style = style.underline_color(underline);
    }

    let mut modifiers = match font_style {
        FontStyleConfig::Regular => Modifier::empty(),
        FontStyleConfig::Bold => Modifier::BOLD,
        FontStyleConfig::Italic => Modifier::ITALIC,
        FontStyleConfig::BoldItalic => Modifier::BOLD | Modifier::ITALIC,
    };
    if cell.bold() {
        modifiers |= Modifier::BOLD;
    }
    if cell.dim() {
        modifiers |= Modifier::DIM;
    }
    if cell.italic() {
        modifiers |= Modifier::ITALIC;
    }
    if cell.underline() {
        modifiers |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        modifiers |= Modifier::REVERSED;
    }
    if cell.hidden() {
        modifiers |= Modifier::HIDDEN;
    }
    if cell.strikeout() {
        modifiers |= Modifier::CROSSED_OUT;
    }
    match cell.blink() {
        Blink::None => {}
        Blink::Slow => modifiers |= Modifier::SLOW_BLINK,
        Blink::Rapid => modifiers |= Modifier::RAPID_BLINK,
    }

    style.add_modifier(modifiers)
}

fn cell_color_to_tui(color: VtColor, theme_palette: &[TuiColor; 16]) -> Option<TuiColor> {
    match color {
        VtColor::Default => None,
        VtColor::Idx(index) => Some(ansi_index_to_tui(index, theme_palette)),
        VtColor::Rgb(r, g, b) => Some(TuiColor::Rgb(r, g, b)),
    }
}

fn ansi_index_to_tui(index: u8, theme_palette: &[TuiColor; 16]) -> TuiColor {
    match index {
        0..=15 => theme_palette[index as usize],
        16..=231 => {
            let index = index - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            TuiColor::Rgb(component(r), component(g), component(b))
        }
        232..=255 => {
            let shade = 8 + (index - 232) * 10;
            TuiColor::Rgb(shade, shade, shade)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bevy_terminal_ratatui::prelude::{TerminalColor, TerminalSnapshot};
    use ratatui::buffer::{Cell, CellWidth};

    use crate::ratty_vt::Parser;

    fn parse(rows: u16, cols: u16, input: &[u8]) -> Parser {
        let mut parser = Parser::new(rows, cols, 1000);
        parser.process(input);
        parser
    }

    fn render_buffer(screen: &Screen) -> Buffer {
        let (rows, cols) = screen.size();
        let area = Rect::new(0, 0, cols, rows);
        let mut buffer = Buffer::empty(area);
        TerminalWidget {
            screen,
            selection: &TerminalSelection::default(),
            theme: &ThemeConfig::default(),
            font_style: FontStyleConfig::Regular,
        }
        .render(area, &mut buffer);
        buffer
    }

    /// Renders `input` through [`TerminalWidget`] and returns row 0's cells.
    fn render_cells(rows: u16, cols: u16, input: &[u8]) -> Vec<Cell> {
        let parser = parse(rows, cols, input);
        let buffer = render_buffer(parser.screen());
        (0..cols).map(|col| buffer[(col, 0)].clone()).collect()
    }

    /// Renders `input` through [`TerminalWidget`] and returns the drawn rows.
    fn render_rows(rows: u16, cols: u16, input: &[u8]) -> Vec<String> {
        let parser = parse(rows, cols, input);
        let buffer = render_buffer(parser.screen());
        (0..rows)
            .map(|row| {
                (0..cols)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// Draws a terminal state through Ratatui's differential update path into
    /// the retained Bevy terminal surface.
    fn draw_screen(tui: &mut RatatuiTerminal, screen: &Screen) {
        tui.draw(|frame| {
            frame.render_widget(
                TerminalWidget {
                    screen,
                    selection: &TerminalSelection::default(),
                    theme: &ThemeConfig::default(),
                    font_style: FontStyleConfig::Regular,
                },
                frame.area(),
            );
        });
    }

    /// Builds and draws a fresh terminal state through [`draw_screen`].
    fn draw_input(tui: &mut RatatuiTerminal, rows: u16, cols: u16, input: &[u8]) {
        let parser = parse(rows, cols, input);
        draw_screen(tui, parser.screen());
    }

    fn symbol(snapshot: &TerminalSnapshot, col: u16, row: u16) -> String {
        snapshot
            .cell((col, row))
            .map(|cell| cell.symbol().to_string())
            .unwrap_or_default()
    }

    fn background(snapshot: &TerminalSnapshot, col: u16, row: u16) -> TerminalColor {
        snapshot
            .cell((col, row))
            .map(|cell| cell.style.background)
            .unwrap_or(TerminalColor::Default)
    }

    /// A narrowed DECSTBM region must not shift or blank the drawn grid: the
    /// widget reads visible rows, never the scroll region.
    #[test]
    fn widget_draws_every_row_with_a_scroll_region_set() {
        let mut input = Vec::new();
        for line in 0..8 {
            input.extend_from_slice(format!("\x1b[{};1Hline{line}", line + 1).as_bytes());
        }
        let without_region = render_rows(8, 20, &input);

        input.extend_from_slice(b"\x1b[2;6r");
        let with_region = render_rows(8, 20, &input);

        let expected: Vec<String> = (0..8).map(|line| format!("line{line}")).collect();
        assert_eq!(without_region, expected);
        assert_eq!(
            with_region, expected,
            "a narrowed scroll region must not shift or blank the drawn grid"
        );
    }

    /// When a wide glyph does not fit at the end of a line it wraps, and the
    /// cell it skipped must stay an unstyled blank so the renderer does not
    /// paint a stray background there.
    #[test]
    fn wrapped_wide_characters_leave_an_unstyled_pad() {
        // Green background, then thirteen narrow cells so the wide glyph cannot
        // fit in the one remaining column.
        let rendered = render_cells(2, 14, b"\x1b[42mabcdefghijklm\xe4\xbd\xa0\x1b[0m");

        let pad = &rendered[13];
        assert_eq!(pad.symbol(), " ", "the pad cell must stay blank");
        assert_eq!(
            pad.bg,
            TuiColor::Reset,
            "the pad cell must not carry the active background"
        );
    }

    /// Ratatui skips a wide glyph's second cell when sending a diff. The Bevy
    /// backend synthesizes that continuation cell from the anchor, so the
    /// retained surface must show the glyph, its styled continuation, and no
    /// stale content from the previous frame.
    #[test]
    fn successive_draws_replace_wide_continuation_cells() {
        let (rows, cols) = (2, 8);
        let (mut tui, _) = RatatuiTerminal::new(cols, rows);

        draw_input(&mut tui, rows, cols, b"abcdefgh");
        draw_input(
            &mut tui,
            rows,
            cols,
            "\x1b[42m\u{4f60}\u{1f600}\x1b[0m".as_bytes(),
        );

        let snapshot = tui.snapshot();
        assert_eq!(symbol(&snapshot, 0, 0), "\u{4f60}");
        assert!(snapshot.cell((1, 0)).is_some_and(|c| c.is_continuation()));
        assert_eq!(symbol(&snapshot, 2, 0), "\u{1f600}");
        assert!(snapshot.cell((3, 0)).is_some_and(|c| c.is_continuation()));
        assert_eq!(background(&snapshot, 1, 0), background(&snapshot, 0, 0));
        assert_eq!(background(&snapshot, 3, 0), background(&snapshot, 2, 0));
        assert_ne!(background(&snapshot, 0, 0), TerminalColor::Default);
        for col in 4..cols {
            assert_eq!(
                symbol(&snapshot, col, 0),
                " ",
                "old content survived at column {col}"
            );
        }
    }

    /// Repeatedly moving wide graphemes into and out of the viewport must not
    /// leave their owners or continuation cells behind on unrelated rows.
    #[test]
    fn scrollback_redraws_wide_graphemes_without_artifacts() {
        let (rows, cols) = (2, 8);
        let (mut tui, _) = RatatuiTerminal::new(cols, rows);
        let mut parser = parse(
            rows,
            cols,
            "\x1b[42m\u{4f60}\u{1f600}\x1b[0m\r\nsecond\r\nthird\r\nfourth".as_bytes(),
        );

        for offset in [1, 2, 0, 2, 1, 2] {
            parser.screen_mut().set_scrollback(offset);
            draw_screen(&mut tui, parser.screen());

            let snapshot = tui.snapshot();
            if offset == 2 {
                assert_eq!(symbol(&snapshot, 0, 0), "\u{4f60}");
                assert!(snapshot.cell((1, 0)).is_some_and(|c| c.is_continuation()));
                assert_eq!(symbol(&snapshot, 2, 0), "\u{1f600}");
                assert!(snapshot.cell((3, 0)).is_some_and(|c| c.is_continuation()));
                assert_eq!(background(&snapshot, 1, 0), background(&snapshot, 0, 0));
                assert_eq!(background(&snapshot, 3, 0), background(&snapshot, 2, 0));
            } else {
                assert!(
                    snapshot
                        .cells()
                        .iter()
                        .all(|cell| !matches!(cell.symbol(), "\u{4f60}" | "\u{1f600}"))
                );
            }
        }
    }

    /// Window managers and drag-resize routinely produce very narrow grids;
    /// the engine used to underflow placing a double-width glyph in a
    /// single-column grid, which is why the grid floor was two columns.
    #[test]
    fn degenerate_grids_render_without_panicking() {
        for (rows, cols) in [(1_u16, 1_u16), (1, 40), (40, 1), (2, 2)] {
            let rendered = render_rows(rows, cols, "\u{4f60}\u{597d}ab".as_bytes());
            assert_eq!(rendered.len(), usize::from(rows));
        }
    }

    #[test]
    fn widget_draws_wide_characters_and_combining_marks() {
        let rendered = render_rows(2, 10, "你好e\u{0301}z".as_bytes());

        // The continuation cell after a wide glyph stays blank rather than
        // repeating it.
        assert_eq!(rendered[0], "你 好 e\u{0301}z");
    }

    #[test]
    fn widget_maps_attributes_to_ratatui_modifiers() {
        // Bold and dim are one exclusive intensity attribute, so they go on
        // separate cells.
        let rendered = render_cells(1, 8, b"\x1b[1;3;4;5;7mA\x1b[0;2;6mB\x1b[25mC");
        let a = rendered[0].modifier;
        for expected in [
            Modifier::BOLD,
            Modifier::ITALIC,
            Modifier::UNDERLINED,
            Modifier::SLOW_BLINK,
            Modifier::REVERSED,
        ] {
            assert!(a.contains(expected), "{expected:?} missing from {a:?}");
        }
        let b = rendered[1].modifier;
        assert!(b.contains(Modifier::DIM));
        assert!(b.contains(Modifier::RAPID_BLINK));
        assert!(!b.contains(Modifier::SLOW_BLINK));
        assert!(
            !rendered[2]
                .modifier
                .intersects(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK)
        );
    }

    #[test]
    fn widget_preserves_hidden_strikeout_and_underline_color() {
        let rendered = render_cells(1, 3, b"\x1b[4;8;9;58;2;1;2;3mX\x1b[28;29;59mY");
        let cell = &rendered[0];
        assert!(cell.modifier.contains(Modifier::UNDERLINED));
        assert!(cell.modifier.contains(Modifier::HIDDEN));
        assert!(cell.modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(cell.underline_color, TuiColor::Rgb(1, 2, 3));
        let cell = &rendered[1];
        assert!(
            !cell
                .modifier
                .intersects(Modifier::HIDDEN | Modifier::CROSSED_OUT)
        );
        assert_eq!(cell.underline_color, TuiColor::Reset);
    }

    /// Hidden text must reach the renderer concealed: the backend translates
    /// `Modifier::HIDDEN` to `StyleFlags::HIDDEN`, which the renderer skips.
    #[test]
    fn hidden_text_reaches_the_renderer_concealed() {
        let (mut tui, _) = RatatuiTerminal::new(20, 2);
        draw_input(&mut tui, 2, 20, b"ab\x1b[8mXY\x1b[28mcd");
        let snapshot = tui.snapshot();
        let hidden = snapshot.cell((2, 0)).expect("cell");
        assert!(
            hidden
                .style
                .has(bevy_terminal_ratatui::prelude::StyleFlags::HIDDEN)
        );
        let visible = snapshot.cell((4, 0)).expect("cell");
        assert!(
            !visible
                .style
                .has(bevy_terminal_ratatui::prelude::StyleFlags::HIDDEN)
        );
    }

    #[test]
    fn widget_blanks_kitty_placeholder_cells() {
        let rendered = render_cells(1, 4, "a\u{10EEEE}\u{0305}\u{0305}b".as_bytes());
        assert_eq!(rendered[0].symbol(), "a");
        assert_eq!(rendered[1].symbol(), " ");
        assert_eq!(rendered[2].symbol(), "b");
    }

    #[test]
    fn widget_resolves_colours_through_the_theme() {
        let rendered = render_cells(1, 8, b"\x1b[31;48;5;99mA\x1b[0;38;2;1;2;3mB");
        let theme = ThemeConfig::default();
        let [r, g, b] = theme.palette()[1];
        assert_eq!(rendered[0].fg, TuiColor::Rgb(r, g, b));
        assert_eq!(
            rendered[0].bg,
            ansi_index_to_tui(99, &[TuiColor::Reset; 16])
        );
        assert_eq!(rendered[1].fg, TuiColor::Rgb(1, 2, 3));
        let [fr, fg, fb] = theme.foreground;
        assert_eq!(rendered[2].fg, TuiColor::Rgb(fr, fg, fb));
    }

    /// Wide anchors declare their two-column span so the Bevy surface
    /// synthesizes the continuation cell; narrow cells declare one column.
    #[test]
    fn widget_declares_wide_glyph_widths() {
        let rendered = render_cells(1, 6, "\u{4f60}ab".as_bytes());
        assert_eq!(rendered[0].cell_width(), 2);
        assert_eq!(rendered[1].cell_width(), 1);
        assert_eq!(rendered[2].cell_width(), 1);
    }

    #[test]
    fn font_size_changes_update_the_render_config_only() {
        let mut surface = TerminalSurface::new(&AppConfig::default()).expect("surface");
        let before = surface.render_config().clone();
        assert!(surface.adjust_font_size(2));
        assert_eq!(surface.font_size(), AppConfig::default().font.size + 2);
        assert_ne!(surface.render_config().font_size, before.font_size);
        assert!(!surface.is_measured());
        // Zoom never moves the grid on its own: the renderer's measurement
        // reports the new cell size and the reflow follows that.
        assert_eq!(surface.cols, AppConfig::default().terminal.default_cols);
        assert!(!surface.adjust_font_size(0));
    }

    #[test]
    fn render_output_adoption_reports_changes_once() {
        let mut surface = TerminalSurface::new(&AppConfig::default()).expect("surface");
        let texture = TerminalTexture {
            image: Handle::default(),
            size: UVec2::new(800, 600),
            logical_size: Vec2::new(400.0, 300.0),
            raster_scale: 2.0,
            cell_size: Vec2::new(8.0, 16.0),
            font_size: 16.0,
        };
        assert!(surface.update_render_output(&texture));
        assert!(!surface.update_render_output(&texture));
        assert!(surface.is_measured());
        assert_eq!(surface.char_dimensions(), Vec2::new(8.0, 16.0));

        let layout = surface.resize_to_fit(Vec2::new(400.0, 300.0), 2.0);
        assert_eq!((layout.cols, layout.rows), (50, 18));
        assert_eq!(layout.texture_size, UVec2::new(800, 576));
        assert_eq!(layout.logical_size, Vec2::new(400.0, 288.0));
    }

    #[test]
    fn non_font_files_are_rejected() {
        let dir = std::env::temp_dir().join(format!("ratty-font-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("not-a-font.ttf");
        fs::write(&path, b"hello").expect("write");
        assert!(read_font_face(&path).is_err());
        assert!(read_font_face(&dir.join("missing.ttf")).is_err());
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// Builds a headless Bevy text app that measures Ratty's renderer config.
    ///
    /// The family is deliberately absent so the fallback path is exercised
    /// without depending on which fonts the host has installed.
    fn measured_terminal_app(font_size: i32, render_scale: f32) -> (App, Entity) {
        let app_config = AppConfig {
            font: FontConfig {
                family: "Ratty Definitely Missing Mono".to_string(),
                size: font_size,
                ..default()
            },
            window: crate::config::WindowConfig {
                scale_factor: Some(render_scale),
                ..default()
            },
            ..default()
        };
        let terminal = TerminalSurface::new(&app_config).expect("terminal");
        let render_surface = terminal.tui.surface();
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::text::TextPlugin,
            bevy_terminal_ratatui::prelude::TerminalPlugin,
        ))
        .init_asset::<Image>()
        .insert_resource(app_config)
        .insert_resource(terminal)
        .add_systems(
            Update,
            crate::systems::sync_terminal_renderer_config
                .before(bevy_terminal_ratatui::prelude::TerminalSystems::Sync),
        );
        let entity = app
            .world_mut()
            .spawn((
                TerminalRenderTarget,
                bevy_terminal_ratatui::TerminalRenderer::new(render_surface),
            ))
            .id();
        for _ in 0..4 {
            app.update();
        }
        (app, entity)
    }

    /// Bevy snaps cell dimensions to whole physical pixels. Adjacent requested
    /// sizes can therefore share one effective font/cell at low DPI, but the
    /// sequence may never shrink and the full zoom range must grow both axes.
    #[test]
    fn font_size_steps_change_measured_cells_without_shrinking() {
        for render_scale in [1.0, 2.0] {
            let (mut app, entity) = measured_terminal_app(8, render_scale);
            let mut previous = app
                .world()
                .get::<TerminalTexture>(entity)
                .expect("initial measured texture")
                .cell_size;
            let initial = previous;
            for size in 9..=24 {
                assert!(
                    app.world_mut()
                        .resource_mut::<TerminalSurface>()
                        .adjust_font_size(1)
                );
                app.update();
                let texture = app
                    .world()
                    .get::<TerminalTexture>(entity)
                    .expect("remeasured texture")
                    .clone();
                let requested = points_to_logical_pixels(size);
                assert_eq!(
                    app.world()
                        .resource::<TerminalSurface>()
                        .render_config()
                        .font_size,
                    FontSizing::Px(requested)
                );
                assert!(texture.font_size.is_finite() && texture.font_size >= 1.0);
                let measured = texture.cell_size;
                assert!(
                    measured.cmpge(previous).all(),
                    "cell shrank at size {size} (scale {render_scale}): \
                     {previous:?} -> {measured:?}"
                );
                previous = measured;
            }
            assert!(
                previous.cmpgt(initial).all(),
                "zoom range did not grow both axes at scale {render_scale}: \
                 {initial:?} -> {previous:?}"
            );
        }
    }

    #[test]
    fn unavailable_font_falls_back_and_adopts_measured_metrics() {
        let (mut app, entity) = measured_terminal_app(12, 1.0);

        let render_config = app
            .world()
            .get::<TerminalRenderConfig>(entity)
            .expect("render config");
        assert_eq!(render_config.font.regular, FontSource::Monospace);
        assert_eq!(render_config.cell_size, CellSizing::FROM_FONT);
        assert!(matches!(render_config.font_size, FontSizing::Px(_)));
        let texture = app
            .world()
            .get::<TerminalTexture>(entity)
            .expect("measured terminal texture")
            .clone();
        assert!(texture.cell_size.cmpgt(Vec2::ONE).all());
        assert!(texture.cell_size.y >= points_to_logical_pixels(12));

        let cell_size = texture.cell_size;
        let mut terminal = app.world_mut().resource_mut::<TerminalSurface>();
        assert!(terminal.update_render_output(&texture));
        let layout = terminal.resize_to_fit(cell_size * Vec2::new(4.9, 3.9), 1.0);
        assert_eq!((layout.cols, layout.rows), (4, 3));
    }

    #[test]
    fn font_config_always_uses_measured_cell_metrics() {
        let config = AppConfig {
            font: FontConfig {
                size: 20,
                ..default()
            },
            window: crate::config::WindowConfig {
                scale_factor: Some(2.0),
                ..default()
            },
            ..default()
        };
        let mut terminal = TerminalSurface::new(&config).expect("measured terminal");
        assert_eq!(terminal.render_config().cell_size, CellSizing::FROM_FONT);
        assert_eq!(
            terminal.render_config().font_size,
            FontSizing::Px(points_to_logical_pixels(20))
        );
        assert_eq!(
            terminal.render_config().raster.scale,
            TerminalRenderScale::Fixed(2.0)
        );

        let unmeasured_cell = terminal.char_dimensions();
        assert!(terminal.adjust_font_size(2));
        assert_eq!(
            terminal.char_dimensions(),
            unmeasured_cell,
            "zoom must retain the last authoritative metrics until remeasurement"
        );
    }

    #[test]
    fn explicit_face_configuration_requires_a_regular_face() {
        let font = FontConfig {
            bold: Some("Bold.ttf".into()),
            ..default()
        };
        let Err(error) = load_configured_font_faces(&mut App::new(), &font) else {
            panic!("bold without regular must fail");
        };

        assert!(error.to_string().contains("font.regular is required"));
    }

    #[test]
    fn invalid_explicit_font_data_is_rejected_before_asset_loading() {
        let font = FontConfig {
            regular: Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")),
            ..default()
        };
        let Err(error) = load_configured_font_faces(&mut App::new(), &font) else {
            panic!("non-font data must fail");
        };

        assert!(
            error
                .to_string()
                .contains("is not a TrueType or OpenType font")
        );
    }
}

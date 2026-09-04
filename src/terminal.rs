//! Terminal surface rendering and Ratatui integration.

use std::num::NonZeroU16;

use bevy::prelude::*;
use parley_ratatui::ratatui::Terminal;
use parley_ratatui::ratatui::buffer::{Buffer, CellDiffOption};
use parley_ratatui::ratatui::layout::Rect;
use parley_ratatui::ratatui::style::{Color as TuiColor, Modifier, Style};
use parley_ratatui::ratatui::widgets::Widget;
use parley_ratatui::{
    CellQuantization, FontOptions, ParleyBackend, TerminalRenderer, TexturePresentation,
};

use crate::config::{AppConfig, FontConfig, FontStyleConfig, ThemeConfig};
use crate::direct_render::{
    DirectTerminalSceneExchange, TerminalImages, resize_terminal_image,
    update_direct_terminal_frame,
};
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
    pub tui: Terminal<ParleyBackend>,
    /// Front texture image handle (sampled by the plane material and sprite).
    pub image_handle: Option<Handle<Image>>,
    /// Vello render-target handle. Vello rasterizes into this storage texture
    /// and it is copied into [`Self::image_handle`] each frame.
    pub render_image_handle: Option<Handle<Image>>,
    /// Back texture image handle.
    pub back_image_handle: Option<Handle<Image>>,
    /// Terminal column count.
    pub cols: u16,
    /// Terminal row count.
    pub rows: u16,
    cursor_model_visible: bool,
    window_opacity: f32,
    font: FontConfig,
    theme: ThemeConfig,
    render_scale: f32,
    renderer: TerminalRenderer,
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
        let backend = ParleyBackend::new(cols, rows);
        let mut tui = Terminal::new(backend)?;
        let _ = tui.clear();
        if config.cursor.model.visible {
            tui.hide_cursor()?;
        } else {
            tui.show_cursor()?;
        }
        // The real scale arrives with the first `resize_to_fit` once the
        // window exists; an explicit override seeds it early.
        let render_scale = config.window.scale_factor.unwrap_or(1.0).max(1.0);
        let renderer = build_terminal_renderer(
            &config.font,
            &config.theme,
            config.window.opacity,
            render_scale,
        );

        Ok(Self {
            tui,
            image_handle: None,
            render_image_handle: None,
            back_image_handle: None,
            cols,
            rows,
            cursor_model_visible: config.cursor.model.visible,
            window_opacity: config.window.opacity.clamp(0.0, 1.0),
            font: config.font.clone(),
            theme: config.theme.clone(),
            render_scale,
            renderer,
        })
    }

    /// Adjusts the font size.
    pub fn adjust_font_size(&mut self, delta: i32) -> bool {
        let new_size = self.font.size + delta;
        if new_size == self.font.size {
            return false;
        }

        self.font.size = new_size;
        self.rebuild_renderer();
        true
    }

    /// Returns the current font size.
    pub fn font_size(&self) -> i32 {
        self.font.size
    }

    /// Updates the physical render scale.
    fn set_render_scale(&mut self, render_scale: f32) -> bool {
        let render_scale = render_scale.max(1.0);
        if (render_scale - self.render_scale).abs() < f32::EPSILON {
            return false;
        }

        self.render_scale = render_scale;
        self.rebuild_renderer();
        true
    }

    /// Resizes the terminal grid to fit a logical window size.
    pub fn resize_to_fit(&mut self, logical_size: Vec2, render_scale: f32) -> TerminalLayout {
        self.set_render_scale(render_scale);

        let metrics = self.renderer.logical_metrics(self.render_scale);
        let logical_size = logical_size.max(Vec2::ONE);
        let cols = (logical_size.x / metrics.cell_width)
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16;
        let rows = (logical_size.y / metrics.cell_height)
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16;

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

        self.tui.backend_mut().resize(cols, rows);
        let _ = self.tui.resize(Rect::new(0, 0, cols, rows));
        if self.cursor_model_visible {
            let _ = self.tui.hide_cursor();
        } else {
            let _ = self.tui.show_cursor();
        }
        self.cols = cols;
        self.rows = rows;
    }

    /// Returns the rendered cell size in logical pixels.
    pub fn char_dimensions(&self) -> Vec2 {
        let metrics = self.renderer.logical_metrics(self.render_scale);
        Vec2::new(metrics.cell_width.max(1.0), metrics.cell_height.max(1.0))
    }

    /// Returns the terminal pixmap dimensions in pixels.
    pub fn pixmap_dimensions(&self) -> UVec2 {
        let (width, height) = self
            .renderer
            .texture_size_for_buffer(self.tui.backend().buffer());
        UVec2::new(width, height)
    }

    /// Returns the current terminal layout.
    fn layout(&self) -> TerminalLayout {
        TerminalLayout::new(
            self.cols,
            self.rows,
            self.pixmap_dimensions(),
            self.render_scale,
        )
    }

    /// Synchronizes the rendered terminal image.
    ///
    /// # Errors
    ///
    /// Returns an error if the offscreen renderer cannot be initialized or rendered.
    pub(crate) fn sync_image(
        &mut self,
        images: &mut Assets<Image>,
        exchange: &DirectTerminalSceneExchange,
        elapsed_secs: f32,
    ) -> anyhow::Result<()> {
        let (Some(render_handle), Some(present_handle)) =
            (self.render_image_handle.clone(), self.image_handle.clone())
        else {
            return Ok(());
        };
        let (width, height) = self
            .renderer
            .texture_size_for_buffer(self.tui.backend().buffer());
        // The render and present textures are kept the same size so the copy is
        // a plain texel copy. `get_mut` marks the asset modified, which makes
        // Bevy re-extract and re-upload the CPU-side buffer; only take it when
        // the size changes.
        for handle in [&render_handle, &present_handle] {
            let Some(image) = images.get(handle) else {
                continue;
            };
            let size = image.texture_descriptor.size;
            if (size.width != width || size.height != height)
                && let Some(mut image) = images.get_mut(handle)
            {
                resize_terminal_image(&mut image, width, height);
            }
        }

        let buffer = self.tui.backend().buffer();
        let cursor = Some(self.tui.backend().cursor_position());
        let cursor_visible = self.tui.backend().cursor_visible();
        update_direct_terminal_frame(
            exchange,
            TerminalImages {
                render: render_handle,
                present: present_handle,
            },
            &mut self.renderer,
            buffer,
            cursor,
            cursor_visible,
            elapsed_secs,
        );

        Ok(())
    }

    fn rebuild_renderer(&mut self) {
        self.renderer = build_terminal_renderer(
            &self.font,
            &self.theme,
            self.window_opacity,
            self.render_scale,
        );
    }
}

/// Computes the physical render scale for a Bevy window.
pub fn render_scale_for_window(window: &Window) -> f32 {
    // The presenting window's *actual* framebuffer ratio (physical / logical), so the
    // terminal texture is rasterized at exactly the framebuffer resolution and can be
    // presented 1:1 with physical pixels. Deriving it from the real physical size —
    // rather than the reported scale factor — keeps it correct when they disagree.
    //
    // The previous version took the max with the backend's base scale factor; on a
    // mixed-DPI multi-monitor setup that leaked a higher-DPI monitor's scale, over-sizing
    // the texture so it had to be resampled onto the low-DPI window.
    let logical = window.resolution.size().max(Vec2::ONE);
    let physical = window.resolution.physical_size().as_vec2();
    (physical.x / logical.x)
        .min(physical.y / logical.y)
        .max(1.0)
}

/// Returns the logical size for a physical terminal texture.
pub fn texture_logical_size(texture_size: UVec2, render_scale: f32) -> Vec2 {
    let [width, height] =
        TexturePresentation::new([texture_size.x, texture_size.y], render_scale).logical_size();
    Vec2::new(width, height)
}

fn build_terminal_renderer(
    font: &FontConfig,
    theme_config: &ThemeConfig,
    window_opacity: f32,
    render_scale: f32,
) -> TerminalRenderer {
    let palette = theme_config
        .palette()
        .map(|[r, g, b]| parley_ratatui::Rgba::rgb(r, g, b));
    let theme = parley_ratatui::Theme {
        foreground: parley_ratatui::Rgba::rgb(
            theme_config.foreground[0],
            theme_config.foreground[1],
            theme_config.foreground[2],
        ),
        background: parley_ratatui::Rgba::rgba(
            theme_config.background[0],
            theme_config.background[1],
            theme_config.background[2],
            (window_opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
        ),
        cursor: parley_ratatui::Rgba::rgb(
            theme_config.cursor[0],
            theme_config.cursor[1],
            theme_config.cursor[2],
        ),
        palette,
    };
    // Config font sizes are points; Parley takes pixels (1pt = 4/3px at 96dpi).
    const PT_TO_PX: f32 = 96.0 / 72.0;
    let font_options = FontOptions::default()
        .with_family(font.family.clone())
        // Fractional cells keep font-size zoom proportional on both axes even
        // when a single step moves the glyph advance by less than one pixel.
        .with_cell_quantization(CellQuantization::Fractional);
    TerminalRenderer::new_scaled(
        FontOptions {
            size: font.size as f32 * PT_TO_PX,
            ..font_options
        },
        theme,
        render_scale,
    )
}

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

                // Ratatui normally skips the trailing buffer cell when diffing
                // a width-2 symbol. That is correct for a real terminal, where
                // printing the glyph updates both columns, but ParleyBackend is
                // an in-memory cell buffer: skipping the update leaves whatever
                // symbol occupied the continuation cell in an earlier frame.
                // Keep the owner visually wide for Parley while forcing Ratatui
                // to diff it as one cell, which lets its continuation cell
                // participate in the normal diff instead of being skipped.
                //
                // The engine stores the continuation half with default
                // attributes, so it borrows the owner's style: a background
                // must cover both halves of the glyph.
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
                    cell.set_symbol(" ").set_style(style);
                    continue;
                }

                let symbol = vt_cell.contents();
                let mut style = cell_style(vt_cell, &theme_palette, theme_fg, self.font_style);
                if selection.is_some_and(|bounds| bounds.contains(row, col)) {
                    style = style.add_modifier(Modifier::REVERSED);
                }

                cell.set_symbol(if symbol.is_empty() { " " } else { symbol })
                    .set_style(style);
                if vt_cell.is_wide() {
                    cell.set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
                }
            }
        }
    }
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

    use parley_ratatui::ratatui::buffer::Cell;

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
    /// the persistent Parley backend buffer.
    fn draw_screen(tui: &mut Terminal<ParleyBackend>, screen: &Screen) {
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
        })
        .expect("draw terminal frame");
    }

    /// Builds and draws a fresh terminal state through [`draw_screen`].
    fn draw_input(tui: &mut Terminal<ParleyBackend>, rows: u16, cols: u16, input: &[u8]) {
        let parser = parse(rows, cols, input);
        draw_screen(tui, parser.screen());
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
    /// cell it skipped must stay an unstyled blank. Painting the active
    /// background there breaks the renderer's wide-cell heuristic, which only
    /// treats a trailing space as a continuation when it has no background.
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

    /// Ratatui skips a wide glyph's second cell when sending a diff to a real
    /// terminal. ParleyBackend stores those diffs as cells, so the skipped
    /// update used to retain the old symbol and background. Scrolling then
    /// repeated those stale cells anywhere a CJK glyph or emoji moved.
    #[test]
    fn successive_draws_replace_wide_continuation_cells() {
        let (rows, cols) = (2, 8);
        let mut tui = Terminal::new(ParleyBackend::new(cols, rows)).expect("terminal");

        draw_input(&mut tui, rows, cols, b"abcdefgh");
        draw_input(
            &mut tui,
            rows,
            cols,
            "\x1b[42m\u{4f60}\u{1f600}\x1b[0m".as_bytes(),
        );

        let buffer = tui.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "\u{4f60}");
        assert_eq!(buffer[(1, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 0)].symbol(), "\u{1f600}");
        assert_eq!(buffer[(3, 0)].symbol(), " ");
        assert_eq!(buffer[(1, 0)].bg, buffer[(0, 0)].bg);
        assert_eq!(buffer[(3, 0)].bg, buffer[(2, 0)].bg);
        assert_ne!(buffer[(0, 0)].bg, TuiColor::Reset);
        for col in 4..cols {
            assert_eq!(
                buffer[(col, 0)].symbol(),
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
        let mut tui = Terminal::new(ParleyBackend::new(cols, rows)).expect("terminal");
        let mut parser = parse(
            rows,
            cols,
            "\x1b[42m\u{4f60}\u{1f600}\x1b[0m\r\nsecond\r\nthird\r\nfourth".as_bytes(),
        );

        for offset in [1, 2, 0, 2, 1, 2] {
            parser.screen_mut().set_scrollback(offset);
            draw_screen(&mut tui, parser.screen());

            let buffer = tui.backend().buffer();
            if offset == 2 {
                assert_eq!(buffer[(0, 0)].symbol(), "\u{4f60}");
                assert_eq!(buffer[(1, 0)].symbol(), " ");
                assert_eq!(buffer[(2, 0)].symbol(), "\u{1f600}");
                assert_eq!(buffer[(3, 0)].symbol(), " ");
                assert_eq!(buffer[(1, 0)].bg, buffer[(0, 0)].bg);
                assert_eq!(buffer[(3, 0)].bg, buffer[(2, 0)].bg);
            } else {
                assert!(
                    buffer
                        .content()
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

    /// Regression test for vertical-only zoom steps (#97): with fractional
    /// cell quantization, every font-size step must grow both axes.
    #[test]
    fn font_size_steps_scale_cells_on_both_axes() {
        for render_scale in [1.0, 2.0] {
            let mut previous: Option<(f32, f32)> = None;
            for size in 8..=24 {
                let font = FontConfig {
                    size,
                    ..FontConfig::default()
                };
                let renderer =
                    build_terminal_renderer(&font, &ThemeConfig::default(), 1.0, render_scale);
                let metrics = renderer.logical_metrics(render_scale);
                if let Some((width, height)) = previous {
                    assert!(
                        metrics.cell_width > width,
                        "cell width must grow at size {size} (scale {render_scale}): \
                         {width} -> {}",
                        metrics.cell_width
                    );
                    assert!(
                        metrics.cell_height > height,
                        "cell height must grow at size {size} (scale {render_scale}): \
                         {height} -> {}",
                        metrics.cell_height
                    );
                }
                previous = Some((metrics.cell_width, metrics.cell_height));
            }
        }
    }
}

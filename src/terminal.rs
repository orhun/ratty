//! Terminal surface rendering and Ratatui integration.

use bevy::prelude::*;
use parley_ratatui::ratatui::Terminal;
use parley_ratatui::ratatui::buffer::Buffer;
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
        // A single-column grid makes vt100's wide-character wrap logic
        // underflow (`cols - width` for a 2-cell glyph), so keep at least two.
        let cols = (logical_size.x / metrics.cell_width)
            .floor()
            .clamp(2.0, u16::MAX as f32) as u16;
        // Likewise, a single-row grid makes vt100's wrap/scroll bookkeeping
        // underflow (`prev_row - scrolled`), so keep at least two rows.
        let rows = (logical_size.y / metrics.cell_height)
            .floor()
            .clamp(2.0, u16::MAX as f32) as u16;

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

/// Ratatui widget backed by a VT100 screen.
pub struct TerminalWidget<'a> {
    /// Screen to render.
    pub screen: &'a vt100::Screen,
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
            for col in 0..draw_cols {
                let Some(vt_cell) = self.screen.cell(row, col) else {
                    continue;
                };
                if vt_cell.is_wide_continuation() {
                    continue;
                }

                let mut style =
                    vt100_cell_style(vt_cell, &theme_palette, theme_fg, self.font_style);
                let symbol = if vt_cell.has_contents() {
                    vt_cell.contents()
                } else {
                    " "
                };

                if selection.is_some_and(|bounds| bounds.contains(row, col)) {
                    style = style.add_modifier(Modifier::REVERSED);
                }

                buf[(area.x + col, area.y + row)]
                    .set_symbol(symbol)
                    .set_style(style);
            }
        }
    }
}

fn vt100_cell_style(
    cell: &vt100::Cell,
    theme_palette: &[TuiColor; 16],
    theme_fg: TuiColor,
    font_style: FontStyleConfig,
) -> Style {
    let mut style =
        Style::default().fg(vt100_color_to_tui(cell.fgcolor(), theme_palette).unwrap_or(theme_fg));

    if let Some(bg) = vt100_color_to_tui(cell.bgcolor(), theme_palette) {
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

    style = style.add_modifier(modifiers);
    style
}

fn vt100_color_to_tui(color: vt100::Color, theme_palette: &[TuiColor; 16]) -> Option<TuiColor> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(index) => Some(ansi_index_to_tui(index, theme_palette)),
        vt100::Color::Rgb(r, g, b) => Some(TuiColor::Rgb(r, g, b)),
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

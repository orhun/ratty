//! Terminal surface rendering and Ratatui integration.

use std::time::{Duration, Instant};

use bevy::prelude::*;
use parley_ratatui::ratatui::Terminal;
use parley_ratatui::ratatui::buffer::Buffer;
use parley_ratatui::ratatui::layout::Rect;
use parley_ratatui::ratatui::style::{Color as TuiColor, Modifier, Style};
use parley_ratatui::ratatui::widgets::Widget;
use parley_ratatui::vello::wgpu;
use parley_ratatui::{
    FontOptions, GpuRenderer, ParleyBackend, TerminalRenderer, TextureReadback, TextureTarget,
};

use crate::config::{AppConfig, FontConfig, FontStyleConfig, TERMINAL_TEXTURE_LABEL, ThemeConfig};
use crate::mouse::TerminalSelection;

/// Minimum interval between terminal redraws.
const REDRAW_THROTTLE: Duration = Duration::from_millis(16);

/// Terminal redraw flag.
#[derive(Resource)]
pub struct TerminalRedrawState {
    needs_redraw: bool,
    last_redraw: Instant,
}

impl Default for TerminalRedrawState {
    fn default() -> Self {
        Self {
            needs_redraw: true,
            last_redraw: Instant::now() - REDRAW_THROTTLE,
        }
    }
}

impl TerminalRedrawState {
    /// Requests a terminal redraw.
    pub fn request(&mut self) {
        self.needs_redraw = true;
    }

    /// Returns whether a redraw was pending.
    pub fn take(&mut self) -> bool {
        if !self.needs_redraw || self.last_redraw.elapsed() < REDRAW_THROTTLE {
            return false;
        }
        self.needs_redraw = false;
        self.last_redraw = Instant::now();
        true
    }
}

/// Terminal surface and render state.
pub struct TerminalSurface {
    /// Ratatui terminal backend.
    pub tui: Terminal<ParleyBackend>,
    /// Front texture image handle.
    pub image_handle: Option<Handle<Image>>,
    /// Back texture image handle.
    pub back_image_handle: Option<Handle<Image>>,
    /// Terminal column count.
    pub cols: u16,
    /// Terminal row count.
    pub rows: u16,
    cursor_model_visible: bool,
    font: FontConfig,
    theme: ThemeConfig,
    renderer: TerminalRenderer,
    gpu: Option<OffscreenGpu>,
}

struct OffscreenGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: GpuRenderer,
    target: TextureTarget,
    readback: TextureReadback,
    rgba: Vec<u8>,
}

impl OffscreenGpu {
    async fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .map_err(|_| anyhow::anyhow!("failed to request wgpu adapter for parley_ratatui"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;
        let target = TextureTarget::new(
            &device,
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            Some(TERMINAL_TEXTURE_LABEL),
        );
        let renderer = GpuRenderer::new(&device)?;
        Ok(Self {
            device,
            queue,
            renderer,
            target,
            readback: TextureReadback::new(),
            rgba: Vec::new(),
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if self.target.width == width && self.target.height == height {
            return;
        }

        self.target = TextureTarget::new(
            &self.device,
            width,
            height,
            self.target.format,
            Some(TERMINAL_TEXTURE_LABEL),
        );
    }
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
        let renderer = build_terminal_renderer(&config.font, &config.theme);

        Ok(Self {
            tui,
            image_handle: None,
            back_image_handle: None,
            cols,
            rows,
            cursor_model_visible: config.cursor.model.visible,
            font: config.font.clone(),
            theme: config.theme.clone(),
            renderer,
            gpu: None,
        })
    }

    /// Adjusts the font size.
    pub fn adjust_font_size(&mut self, delta: i32) -> bool {
        let new_size = self.font.size + delta;
        if new_size == self.font.size {
            return false;
        }

        self.font.size = new_size;
        self.renderer = build_terminal_renderer(&self.font, &self.theme);
        if let Some(gpu) = self.gpu.as_mut() {
            let (width, height) = self
                .renderer
                .texture_size_for_buffer(self.tui.backend().buffer());
            gpu.resize(width, height);
        }
        true
    }

    /// Returns the current font size.
    pub fn font_size(&self) -> i32 {
        self.font.size
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

        if let Some(gpu) = self.gpu.as_mut() {
            let (width, height) = self
                .renderer
                .texture_size_for_buffer(self.tui.backend().buffer());
            gpu.resize(width, height);
        }
    }

    /// Returns the rendered cell size in pixels.
    pub fn char_dimensions(&self) -> UVec2 {
        let metrics = self.renderer.metrics();
        UVec2::new(
            metrics.cell_width.ceil().max(1.0) as u32,
            metrics.cell_height.ceil().max(1.0) as u32,
        )
    }

    /// Returns the terminal pixmap dimensions in pixels.
    pub fn pixmap_dimensions(&self) -> UVec2 {
        let (width, height) = self
            .renderer
            .texture_size_for_buffer(self.tui.backend().buffer());
        UVec2::new(width, height)
    }

    /// Synchronizes the rendered terminal image.
    ///
    /// # Errors
    ///
    /// Returns an error if the offscreen renderer cannot be initialized or rendered.
    pub fn sync_image(
        &mut self,
        images: &mut Assets<Image>,
        elapsed_secs: f32,
    ) -> anyhow::Result<()> {
        let Some(handle) = self.image_handle.as_ref() else {
            return Ok(());
        };
        let Some(image) = images.get_mut(handle) else {
            return Ok(());
        };

        let (width, height) = self
            .renderer
            .texture_size_for_buffer(self.tui.backend().buffer());
        if self.gpu.is_none() {
            self.gpu = Some(pollster::block_on(OffscreenGpu::new(width, height))?);
        }
        let Some(gpu) = self.gpu.as_mut() else {
            anyhow::bail!("offscreen GPU renderer should be initialized");
        };
        gpu.resize(width, height);

        let buffer = self.tui.backend().buffer();
        let cursor = Some(self.tui.backend().cursor_position());
        let cursor_visible = self.tui.backend().cursor_visible();

        gpu.renderer.render_to_rgba8_with_elapsed_into(
            &mut self.renderer,
            &mut gpu.readback,
            &gpu.device,
            &gpu.queue,
            &gpu.target,
            buffer,
            cursor,
            cursor_visible,
            elapsed_secs,
            &mut gpu.rgba,
        )?;

        image.resize(bevy::render::render_resource::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        });
        let data = image.data.get_or_insert_with(Vec::new);
        let target_len = width as usize * height as usize * 4;
        if data.len() != target_len {
            data.resize(target_len, 0);
        }
        if gpu.rgba.len() == target_len {
            data.copy_from_slice(&gpu.rgba);
        }

        Ok(())
    }
}

fn build_terminal_renderer(font: &FontConfig, theme_config: &ThemeConfig) -> TerminalRenderer {
    let palette = theme_config
        .palette()
        .map(|[r, g, b]| parley_ratatui::Rgba::rgb(r, g, b));
    let theme = parley_ratatui::Theme {
        foreground: parley_ratatui::Rgba::rgb(
            theme_config.foreground[0],
            theme_config.foreground[1],
            theme_config.foreground[2],
        ),
        background: parley_ratatui::Rgba::rgb(
            theme_config.background[0],
            theme_config.background[1],
            theme_config.background[2],
        ),
        cursor: parley_ratatui::Rgba::rgb(
            theme_config.cursor[0],
            theme_config.cursor[1],
            theme_config.cursor[2],
        ),
        palette,
    };
    let font_options = FontOptions::default().with_family(font.family.clone());
    TerminalRenderer::new(
        FontOptions {
            size: font.size as f32,
            ..font_options
        },
        theme,
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
        let [bg_r, bg_g, bg_b] = self.theme.background;
        let theme_fg = TuiColor::Rgb(fg_r, fg_g, fg_b);
        let theme_bg = TuiColor::Rgb(bg_r, bg_g, bg_b);
        let theme_palette = self.theme.palette().map(|[r, g, b]| TuiColor::Rgb(r, g, b));
        buf.set_style(area, Style::default().fg(theme_fg).bg(theme_bg));

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
                    vt100_cell_style(vt_cell, &theme_palette, theme_fg, theme_bg, self.font_style);
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
    theme_bg: TuiColor,
    font_style: FontStyleConfig,
) -> Style {
    let mut style = Style::default()
        .fg(vt100_color_to_tui(cell.fgcolor(), theme_palette).unwrap_or(theme_fg))
        .bg(vt100_color_to_tui(cell.bgcolor(), theme_palette).unwrap_or(theme_bg));

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

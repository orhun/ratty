//! Helpers for terminal image synchronization.

use bevy::prelude::*;
use bevy::render::render_resource::Extent3d;

use crate::terminal::TerminalSurface;

type Rgba = [u8; 4];
const DEBUG_BG: Rgba = [18, 20, 28, 255];
const DEBUG_GRID: Rgba = [33, 36, 48, 255];
const DEBUG_GRID_OUTLINE: Rgba = [51, 57, 72, 255];
const DEBUG_CURSOR: Rgba = [126, 156, 216, 255];
const DEBUG_FG_FALLBACK: Rgba = [220, 215, 186, 255];
const DEBUG_BG_FALLBACK: Rgba = [31, 31, 40, 255];

/// Synchronizes the terminal debug image.
pub fn sync_terminal_debug_image(
    terminal: &TerminalSurface,
    images: &mut Assets<Image>,
    screen: &vt100::Screen,
) {
    let Some(handle) = terminal.back_image_handle.as_ref() else {
        return;
    };
    let Some(mut image) = images.get_mut(handle) else {
        return;
    };

    let pixmap = terminal.pixmap_dimensions();
    let width = pixmap.x;
    let height = pixmap.y;
    let rgba_len = width as usize * height as usize * 4;

    image.resize(Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    });

    let data = image.data.get_or_insert_with(Vec::new);
    if data.len() != rgba_len {
        data.resize(rgba_len, 0);
    }

    CellDebugImageRenderer::new(data, width, height, terminal.cols, terminal.rows).render(screen);
}

/// Synchronizes an image handle across plane materials.
///
/// Re-binds the texture on every call rather than only when the handle changes.
/// The terminal plane textures are rebuilt on the GPU as the terminal updates:
/// the back debug texture is re-uploaded each redraw, and the front present
/// texture's GPU image is recreated whenever the terminal resizes. Either rebuild
/// leaves a material's cached bind group pointing at a stale texture, so the plane
/// freezes (front) or blanks (back) until the material is re-prepared.
/// Unconditionally re-binding re-prepares the bind group so the planes always
/// sample the current texture. The caller only runs this on redraw frames (gated
/// by the frame-dirty flag), so it is not per-frame churn.
pub fn sync_plane_texture<'a>(
    image_handle: Option<&Handle<Image>>,
    material_handles: impl IntoIterator<Item = &'a MeshMaterial3d<StandardMaterial>>,
    materials: &mut Assets<StandardMaterial>,
) {
    let Some(image_handle) = image_handle else {
        return;
    };

    for material_handle in material_handles {
        if let Some(mut material) = materials.get_mut(&material_handle.0) {
            material.base_color_texture = Some(image_handle.clone());
        }
    }
}

struct CellDebugImageRenderer<'a> {
    data: &'a mut [u8],
    width: u32,
    height: u32,
    cols: u32,
    rows: u32,
    cell_width: u32,
    cell_height: u32,
}

impl<'a> CellDebugImageRenderer<'a> {
    fn new(data: &'a mut [u8], width: u32, height: u32, cols: u16, rows: u16) -> Self {
        let cols = cols.max(1) as u32;
        let rows = rows.max(1) as u32;
        let cell_width = (width / cols).max(1);
        let cell_height = (height / rows).max(1);
        Self {
            data,
            width,
            height,
            cols,
            rows,
            cell_width,
            cell_height,
        }
    }

    fn render(&mut self, screen: &vt100::Screen) {
        self.fill(DEBUG_BG);

        for row in 0..self.rows {
            for col in 0..self.cols {
                let rect = self.cell_rect(row, col);
                self.draw_rect(rect, DEBUG_GRID);
                self.draw_rect_outline(rect, DEBUG_GRID_OUTLINE);

                let Some(cell) = screen.cell(row as u16, col as u16) else {
                    continue;
                };

                let bg = vt100_debug_color(cell.bgcolor()).unwrap_or(DEBUG_BG_FALLBACK);
                let fg = vt100_debug_color(cell.fgcolor()).unwrap_or(DEBUG_FG_FALLBACK);
                let active = cell.has_contents() && !cell.is_wide_continuation();
                let fill = if active {
                    bg
                } else {
                    blend_rgba(bg, DEBUG_BG, 0.55)
                };

                self.draw_rect(rect.inset(1), fill);

                if active {
                    let indicator = rect
                        .centered_subrect((rect.width() / 2).max(2), (rect.height() / 2).max(2));
                    self.draw_rect(indicator, fg);
                }

                if cell.underline() {
                    let underline = CellRect {
                        x0: rect.x0.saturating_add(2),
                        y0: rect.y1.saturating_sub(2),
                        x1: rect.x1.saturating_sub(2),
                        y1: rect.y1.saturating_sub(1),
                    };
                    self.draw_rect(underline, fg);
                }

                if cell.bold() {
                    self.draw_rect_outline(rect.inset(1), [255, 255, 255, 90]);
                }
            }
        }

        if !screen.hide_cursor() {
            let (cursor_row, cursor_col) = screen.cursor_position();
            self.draw_rect_outline(
                self.cell_rect(cursor_row as u32, cursor_col as u32),
                DEBUG_CURSOR,
            );
        }
    }

    fn cell_rect(&self, row: u32, col: u32) -> CellRect {
        let row = row.min(self.rows.saturating_sub(1));
        let col = col.min(self.cols.saturating_sub(1));
        let draw_col = self.cols.saturating_sub(1).saturating_sub(col);
        let x0 = draw_col * self.cell_width;
        let y0 = row * self.cell_height;
        let x1 = if draw_col + 1 == self.cols {
            self.width
        } else {
            ((draw_col + 1) * self.cell_width).min(self.width)
        };
        let y1 = if row + 1 == self.rows {
            self.height
        } else {
            ((row + 1) * self.cell_height).min(self.height)
        };
        CellRect { x0, y0, x1, y1 }
    }

    fn fill(&mut self, color: Rgba) {
        for pixel in self.data.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
    }

    fn draw_rect(&mut self, rect: CellRect, color: Rgba) {
        if rect.x0 >= rect.x1 || rect.y0 >= rect.y1 {
            return;
        }

        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                let idx = ((y * self.width + x) * 4) as usize;
                self.data[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }

    fn draw_rect_outline(&mut self, rect: CellRect, color: Rgba) {
        if rect.x0 >= rect.x1 || rect.y0 >= rect.y1 {
            return;
        }

        self.draw_rect(
            CellRect {
                x0: rect.x0,
                y0: rect.y0,
                x1: rect.x1,
                y1: (rect.y0 + 1).min(rect.y1),
            },
            color,
        );
        self.draw_rect(
            CellRect {
                x0: rect.x0,
                y0: rect.y1.saturating_sub(1),
                x1: rect.x1,
                y1: rect.y1,
            },
            color,
        );
        self.draw_rect(
            CellRect {
                x0: rect.x0,
                y0: rect.y0,
                x1: (rect.x0 + 1).min(rect.x1),
                y1: rect.y1,
            },
            color,
        );
        self.draw_rect(
            CellRect {
                x0: rect.x1.saturating_sub(1),
                y0: rect.y0,
                x1: rect.x1,
                y1: rect.y1,
            },
            color,
        );
    }
}

#[derive(Clone, Copy)]
struct CellRect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl CellRect {
    fn inset(self, amount: u32) -> Self {
        Self {
            x0: self.x0.saturating_add(amount),
            y0: self.y0.saturating_add(amount),
            x1: self.x1.saturating_sub(amount),
            y1: self.y1.saturating_sub(amount),
        }
    }

    fn width(self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    fn height(self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    fn centered_subrect(self, width: u32, height: u32) -> Self {
        let x0 = self.x0 + self.width().saturating_sub(width) / 2;
        let y0 = self.y0 + self.height().saturating_sub(height) / 2;
        Self {
            x0,
            y0,
            x1: (x0 + width).min(self.x1),
            y1: (y0 + height).min(self.y1),
        }
    }
}

fn blend_rgba(top: Rgba, bottom: Rgba, top_mix: f32) -> Rgba {
    let bottom_mix = 1.0 - top_mix;
    [
        (top[0] as f32 * top_mix + bottom[0] as f32 * bottom_mix) as u8,
        (top[1] as f32 * top_mix + bottom[1] as f32 * bottom_mix) as u8,
        (top[2] as f32 * top_mix + bottom[2] as f32 * bottom_mix) as u8,
        255,
    ]
}

fn vt100_debug_color(color: vt100::Color) -> Option<Rgba> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(index) => Some(ansi_index_to_rgba(index)),
        vt100::Color::Rgb(r, g, b) => Some([r, g, b, 255]),
    }
}

fn ansi_index_to_rgba(index: u8) -> Rgba {
    match index {
        0 => [0, 0, 0, 255],
        1 => [128, 0, 0, 255],
        2 => [0, 128, 0, 255],
        3 => [128, 128, 0, 255],
        4 => [0, 0, 128, 255],
        5 => [128, 0, 128, 255],
        6 => [0, 128, 128, 255],
        7 => [192, 192, 192, 255],
        8 => [128, 128, 128, 255],
        9 => [255, 0, 0, 255],
        10 => [0, 255, 0, 255],
        11 => [255, 255, 0, 255],
        12 => [0, 0, 255, 255],
        13 => [255, 0, 255, 255],
        14 => [0, 255, 255, 255],
        15 => [255, 255, 255, 255],
        16..=231 => {
            let index = index - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            [component(r), component(g), component(b), 255]
        }
        232..=255 => {
            let shade = 8 + (index - 232) * 10;
            [shade, shade, shade, 255]
        }
    }
}

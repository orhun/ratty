//! Kitty graphics rendering state.
//!
//! rio-vt owns the kitty graphics *protocol*: it parses the APC stream,
//! decodes PNG, RGB, and RGBA payloads (chunked or not), stores images and
//! placements per screen, answers queries, applies deletes, and evicts
//! images over the spec's memory budget. This module owns what is ratty's
//! to draw: it drains the pixel data the engine queues for upload and turns
//! the engine's placement state into the flat list of textured quads the
//! Bevy scene renders each frame.
//!
//! Two placement kinds reach the screen:
//!
//! - *Direct* placements (`a=T`/`a=p`) live in the engine as absolute,
//!   scrollback-aware grid positions. Their on-screen position is derived
//!   per refresh from the terminal's history and display offset, so they
//!   stay glued to the text they were placed next to — including while the
//!   user scrolls into history.
//! - *Virtual* placements (`U=1`, what `kitten icat --unicode-placeholder`
//!   emits) are anchored by U+10EEEE placeholder cells the application
//!   prints itself. The visible cells are scanned and the image is fitted
//!   to their bounding box, so the image moves, clips, and disappears with
//!   the text that carries it.

use std::collections::HashMap;

use rio_graphics::{ColorType, GraphicData};
use rio_vt::ansi::graphics::{kitty_display_size, resolve_source_rect};
use rio_vt::ansi::kitty_virtual::PLACEHOLDER;
use rio_vt::crosswords::pos::Column;

use crate::inline::RasterObject;
use crate::vt::{self, CellColor, VtTerminal};

/// A kitty placement key: `(image_id, placement_id)`.
pub type PlacementKey = (u32, u32);

/// Kitty images and the placements to draw, derived from engine state.
#[derive(Default)]
pub struct KittyGraphics {
    /// Decoded images by kitty image id. Pixels are held until the render
    /// sync uploads them, then only the texture handle remains.
    images: HashMap<u32, KittyImage>,
    /// Placements to draw, sorted back-to-front.
    placements: Vec<KittyPlacementView>,
}

/// A decoded kitty image and its GPU upload state.
pub struct KittyImage {
    /// Pixel payload and texture handle.
    pub raster: RasterObject,
    /// Engine transmission timestamp, used to skip redundant re-uploads
    /// when the engine re-queues the same pixels for another placement.
    transmit_time: std::time::Instant,
}

/// One kitty placement resolved to visible-grid coordinates.
///
/// Spans are fractional cells: a placement shown at native pixel size
/// rarely lands on a cell boundary, and rounding it up would stretch the
/// image. `row` is signed so a placement partially scrolled off the top
/// keeps its true origin instead of clamping to the first row.
#[derive(Clone, Debug, PartialEq)]
pub struct KittyPlacementView {
    /// Kitty image id (`i=`).
    pub image_id: u32,
    /// Kitty placement id (`p=`, or engine-allocated).
    pub placement_id: u32,
    /// Top-left corner in visible-grid cells.
    pub row: f32,
    /// Top-left corner in visible-grid cells.
    pub col: f32,
    /// Width in cells.
    pub columns: f32,
    /// Height in cells.
    pub rows: f32,
    /// Normalized source crop within the image, `[u0, v0, u1, v1]`.
    pub source_rect: [f32; 4],
    /// Kitty z-index, used for draw order among images.
    pub z: i32,
}

impl KittyGraphics {
    /// Returns the placements to draw, sorted back-to-front.
    pub fn placements(&self) -> &[KittyPlacementView] {
        &self.placements
    }

    /// Returns the image backing a placement.
    pub fn image_mut(&mut self, image_id: u32) -> Option<&mut KittyImage> {
        self.images.get_mut(&image_id)
    }

    /// Synchronizes with the engine: applies queued pixel uploads and
    /// removals, then rebuilds the placement list from the terminal's
    /// kitty state. Returns whether anything the renderer draws changed.
    pub fn refresh(
        &mut self,
        term: &VtTerminal,
        updates: Vec<rio_vt::ansi::graphics::UpdateQueues>,
    ) -> bool {
        let mut changed = false;
        for queues in updates {
            changed |= self.apply_queues(queues);
        }

        // Deletes clear the engine's image store without queueing texture
        // removals, so drop whatever the engine no longer holds. Images on
        // the *inactive* screen stay: the engine does not re-send pixels
        // when the terminal swaps back from the alternate screen.
        let graphics = &term.graphics;
        let before = self.images.len();
        self.images.retain(|image_id, _| {
            graphics.get_kitty_image(*image_id).is_some()
                || graphics
                    .kitty_inactive_screen
                    .kitty_images
                    .contains_key(image_id)
        });
        changed |= self.images.len() != before;

        let mut views = direct_placement_views(term);
        views.extend(virtual_placement_views(term));
        // Back-to-front by kitty z-index, then by ids so equal layers keep
        // a deterministic order across refreshes.
        views.sort_by_key(|view| (view.z, view.image_id, view.placement_id));
        if views != self.placements {
            self.placements = views;
            changed = true;
        }
        changed
    }

    /// Applies one engine update batch: new image pixels in, deleted or
    /// evicted textures out. Atlas graphics (sixel/iTerm2) are dropped —
    /// ratty renders kitty placements only and does not advertise sixel.
    fn apply_queues(&mut self, queues: rio_vt::ansi::graphics::UpdateQueues) -> bool {
        let mut changed = false;

        for key in queues.remove_queue {
            // Kitty texture keys are the protocol image id verbatim; atlas
            // keys live above the u32 range (see rio-graphics).
            if let Ok(image_id) = u32::try_from(key) {
                changed |= self.images.remove(&image_id).is_some();
            }
        }

        for (image_id, data) in queues.pending_images {
            // The engine re-queues the stored pixels for every new
            // placement of an already-transmitted image; only a genuine
            // retransmission carries a new timestamp.
            if self
                .images
                .get(&image_id)
                .is_some_and(|image| image.transmit_time == data.transmit_time)
            {
                continue;
            }
            let transmit_time = data.transmit_time;
            let Some(raster) = rasterize(data) else {
                continue;
            };
            self.images.insert(
                image_id,
                KittyImage {
                    raster,
                    transmit_time,
                },
            );
            changed = true;
        }

        changed
    }
}

/// Converts engine pixel data into ratty's RGBA raster form.
fn rasterize(data: GraphicData) -> Option<RasterObject> {
    let width = u32::try_from(data.width).ok()?;
    let height = u32::try_from(data.height).ok()?;
    let pixel_count = data.width.checked_mul(data.height)?;
    let rgba = match data.color_type {
        ColorType::Rgba => {
            if data.pixels.len() != pixel_count.checked_mul(4)? {
                return None;
            }
            data.pixels
        }
        ColorType::Rgb => {
            if data.pixels.len() != pixel_count.checked_mul(3)? {
                return None;
            }
            let mut rgba = Vec::with_capacity(pixel_count * 4);
            for rgb in data.pixels.chunks_exact(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            rgba
        }
    };
    Some(RasterObject {
        width,
        height,
        rgba,
        handle: None,
    })
}

/// Resolves the engine's direct placements against the current viewport.
fn direct_placement_views(term: &VtTerminal) -> Vec<KittyPlacementView> {
    let graphics = &term.graphics;
    let cell_width = graphics.cell_width;
    let cell_height = graphics.cell_height;
    if cell_width < 1.0 || cell_height < 1.0 {
        return Vec::new();
    }

    // The visible viewport's top row in the engine's absolute row space.
    let viewport_top = term.grid.lines_evicted() as i64 + term.history_size() as i64
        - term.display_offset() as i64;
    let screen_lines = term.screen_lines() as i64;

    let mut views = Vec::new();
    for (&(image_id, placement_id), placement) in &graphics.kitty_placements {
        let Some(stored) = graphics.get_kitty_image(image_id) else {
            continue;
        };
        let image_width = stored.data.width;
        let image_height = stored.data.height;
        let Some((source_x, source_y, source_width, source_height)) = resolve_source_rect(
            placement.source_x,
            placement.source_y,
            placement.source_width,
            placement.source_height,
            image_width,
            image_height,
        ) else {
            continue;
        };

        let (display_width, display_height) = kitty_display_size(
            source_width,
            source_height,
            placement.requested_columns,
            placement.requested_rows,
            cell_width.round() as usize,
            cell_height.round() as usize,
        );
        if display_width == 0 || display_height == 0 {
            continue;
        }

        // Per the kitty spec the sub-cell offset stays inside the cell box;
        // the engine stores the raw request, so clamp at read time.
        let x_offset = (placement.cell_x_offset as f32).min(cell_width - 1.0) / cell_width;
        let y_offset = (placement.cell_y_offset as f32).min(cell_height - 1.0) / cell_height;

        let row = (placement.dest_row - viewport_top) as f32 + y_offset;
        let col = placement.dest_col as f32 + x_offset;
        let columns = display_width as f32 / cell_width;
        let rows = display_height as f32 / cell_height;
        if row + rows <= 0.0 || row >= screen_lines as f32 {
            continue;
        }

        views.push(KittyPlacementView {
            image_id,
            placement_id,
            row,
            col,
            columns,
            rows,
            source_rect: [
                source_x as f32 / image_width as f32,
                source_y as f32 / image_height as f32,
                (source_x + source_width) as f32 / image_width as f32,
                (source_y + source_height) as f32 / image_height as f32,
            ],
            z: placement.z_index,
        });
    }
    views
}

/// Resolves virtual (`U=1`) placements from their placeholder cells.
///
/// The engine registers the placement metadata; the application prints the
/// U+10EEEE cells whose foreground color carries the image id. The image is
/// fitted to the visible placeholder bounding box, which keeps it welded to
/// the text while scrolling, splitting, and erasing — kitty's reason for
/// the placeholder mode to exist.
fn virtual_placement_views(term: &VtTerminal) -> Vec<KittyPlacementView> {
    let virtual_placements = &term.graphics.kitty_virtual_placements;
    if virtual_placements.is_empty() {
        return Vec::new();
    }

    // Placeholder cells carry the image id's low 24 bits in the foreground
    // color; the optional high byte lives in a third combining mark that
    // real emitters (kitten icat) leave unused.
    let lookup: HashMap<u32, PlacementKey> = virtual_placements
        .keys()
        .map(|&(image_id, placement_id)| (image_id & 0x00ff_ffff, (image_id, placement_id)))
        .collect();

    let mut bounds = HashMap::<PlacementKey, (u16, u16, u16, u16)>::new();
    let rows = u16::try_from(term.screen_lines()).unwrap_or(u16::MAX);
    let cols = u16::try_from(term.columns()).unwrap_or(u16::MAX);
    let styles = vt::styles(term);
    for row in 0..rows {
        let Some(grid_row) = vt::visible_row(term, row) else {
            continue;
        };
        // rio-vt flags rows holding a placeholder, so rows without one skip
        // the per-cell scan entirely.
        if !grid_row.kitty_virtual_placeholder {
            continue;
        }
        for col in 0..cols {
            let square = grid_row[Column(usize::from(col))];
            if square.c() != PLACEHOLDER {
                continue;
            }
            let (fg, _, _) = vt::cell_attributes(styles, square);
            let CellColor::Rgb(r, g, b) = fg else {
                continue;
            };
            let encoded_id = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
            let Some(&key) = lookup.get(&encoded_id) else {
                continue;
            };
            bounds
                .entry(key)
                .and_modify(|(top, left, bottom, right)| {
                    *top = (*top).min(row);
                    *left = (*left).min(col);
                    *bottom = (*bottom).max(row);
                    *right = (*right).max(col);
                })
                .or_insert((row, col, row, col));
        }
    }

    bounds
        .into_iter()
        .map(|((image_id, placement_id), (top, left, bottom, right))| {
            let placement = &virtual_placements[&(image_id, placement_id)];
            let source_rect = term
                .graphics
                .get_kitty_image(image_id)
                .and_then(|stored| {
                    let width = stored.data.width;
                    let height = stored.data.height;
                    let (x, y, w, h) = resolve_source_rect(
                        placement.x,
                        placement.y,
                        placement.width,
                        placement.height,
                        width,
                        height,
                    )?;
                    Some([
                        x as f32 / width as f32,
                        y as f32 / height as f32,
                        (x + w) as f32 / width as f32,
                        (y + h) as f32 / height as f32,
                    ])
                })
                .unwrap_or([0.0, 0.0, 1.0, 1.0]);
            KittyPlacementView {
                image_id,
                placement_id,
                row: f32::from(top),
                col: f32::from(left),
                columns: f32::from(right - left + 1),
                rows: f32::from(bottom - top + 1),
                source_rect,
                z: 0,
            }
        })
        .collect()
}

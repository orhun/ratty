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

#[cfg(test)]
mod tests {
    use super::*;

    use base64::Engine as _;
    use rio_vt::ansi::CursorShape;
    use rio_vt::crosswords::{Crosswords, CrosswordsSize};
    use rio_vt::event::WindowId;
    use rio_vt::performer::handler::Processor;

    use crate::vt::TerminalEventSink;

    const CELL_WIDTH: u32 = 10;
    const CELL_HEIGHT: u32 = 20;

    struct Harness {
        term: VtTerminal,
        processor: Processor,
        sink: TerminalEventSink,
        kitty: KittyGraphics,
    }

    impl Harness {
        fn new(rows: u16, cols: u16) -> Self {
            let sink = TerminalEventSink::default();
            let term = Crosswords::new(
                CrosswordsSize::new_with_dimensions(
                    usize::from(cols),
                    usize::from(rows),
                    u32::from(cols) * CELL_WIDTH,
                    u32::from(rows) * CELL_HEIGHT,
                    CELL_WIDTH,
                    CELL_HEIGHT,
                ),
                CursorShape::Block,
                sink.clone(),
                WindowId::from(0),
                0,
                1000,
            );
            Self {
                term,
                processor: Processor::default(),
                sink,
                kitty: KittyGraphics::default(),
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            self.processor.advance(&mut self.term, bytes);
        }

        fn refresh(&mut self) -> bool {
            let updates = self.sink.take_graphics_updates();
            self.kitty.refresh(&self.term, updates)
        }

        /// Transmits and places an opaque RGBA image at the cursor.
        fn transmit_and_place(&mut self, image_id: u32, width: u32, height: u32, extra: &str) {
            let pixels = vec![255_u8; (width * height * 4) as usize];
            let payload = base64::engine::general_purpose::STANDARD.encode(&pixels);
            self.feed(
                format!("\x1b_Ga=T,f=32,s={width},v={height},i={image_id}{extra};{payload}\x1b\\")
                    .as_bytes(),
            );
        }

        fn reply_text(&mut self) -> String {
            self.sink
                .take_replies()
                .into_iter()
                .map(|reply| String::from_utf8_lossy(&reply).into_owned())
                .collect()
        }
    }

    #[test]
    fn transmit_and_place_lands_at_the_cursor() {
        let mut harness = Harness::new(5, 20);
        harness.feed(b"\x1b[2;3H");
        harness.transmit_and_place(7, 2 * CELL_WIDTH, 2 * CELL_HEIGHT, "");

        assert!(harness.refresh(), "a new placement must report a change");
        let views = harness.kitty.placements();
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.image_id, 7);
        assert_eq!((view.row, view.col), (1.0, 2.0));
        assert_eq!((view.columns, view.rows), (2.0, 2.0));
        assert_eq!(view.source_rect, [0.0, 0.0, 1.0, 1.0]);

        let image = harness.kitty.image_mut(7).expect("decoded image");
        assert_eq!(image.raster.width, 2 * CELL_WIDTH);
        assert_eq!(image.raster.height, 2 * CELL_HEIGHT);
        assert_eq!(
            image.raster.rgba.len(),
            (2 * CELL_WIDTH * 2 * CELL_HEIGHT * 4) as usize
        );

        assert!(
            harness.reply_text().contains("\x1b_Gi=7;OK\x1b\\"),
            "the engine must acknowledge the transfer"
        );

        assert!(!harness.refresh(), "an unchanged frame must not re-sync");
    }

    #[test]
    fn rgb_payloads_expand_to_rgba() {
        let mut harness = Harness::new(5, 20);
        let payload = base64::engine::general_purpose::STANDARD.encode([9_u8, 8, 7]);
        harness.feed(format!("\x1b_Ga=T,f=24,s=1,v=1,i=3;{payload}\x1b\\").as_bytes());

        harness.refresh();
        let image = harness.kitty.image_mut(3).expect("decoded image");
        assert_eq!(image.raster.rgba, vec![9, 8, 7, 255]);
    }

    #[test]
    fn chunked_png_transfers_decode_once_complete() {
        let mut harness = Harness::new(5, 20);
        let png = {
            let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(image)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .expect("png encode");
            bytes
        };
        let payload = base64::engine::general_purpose::STANDARD.encode(&png);
        // Chunk boundaries must fall on base64 quantum edges, as kitten
        // icat's 4096-byte chunks do.
        let (first, second) = payload.split_at((payload.len() / 2) & !3);

        harness.feed(format!("\x1b_Ga=T,f=100,i=9,m=1;{first}\x1b\\").as_bytes());
        harness.refresh();
        assert!(
            harness.kitty.placements().is_empty(),
            "an incomplete transfer must not place anything"
        );

        harness.feed(format!("\x1b_Gm=0;{second}\x1b\\").as_bytes());
        harness.refresh();
        assert_eq!(harness.kitty.placements().len(), 1);
        let image = harness.kitty.image_mut(9).expect("decoded image");
        assert_eq!((image.raster.width, image.raster.height), (4, 4));
        assert_eq!(&image.raster.rgba[..4], &[1, 2, 3, 255]);
    }

    #[test]
    fn placements_scroll_with_content_and_history() {
        let mut harness = Harness::new(3, 20);
        harness.transmit_and_place(5, CELL_WIDTH, 2 * CELL_HEIGHT, "");
        harness.refresh();
        assert_eq!(harness.kitty.placements()[0].row, 0.0);

        // Scroll the content up one row: the placement follows the text.
        harness.feed(b"\x1b[3;1H\r\n");
        harness.refresh();
        assert_eq!(harness.kitty.placements()[0].row, -1.0);

        // And another: fully above the viewport, so nothing to draw.
        harness.feed(b"\r\n");
        harness.refresh();
        assert!(harness.kitty.placements().is_empty());

        // Scrolling back into history brings it back.
        vt::set_scrollback(&mut harness.term, 2);
        harness.refresh();
        assert_eq!(harness.kitty.placements()[0].row, 0.0);
    }

    #[test]
    fn delete_all_drops_placements_and_image_data() {
        let mut harness = Harness::new(5, 20);
        harness.transmit_and_place(4, CELL_WIDTH, CELL_HEIGHT, "");
        harness.refresh();
        assert_eq!(harness.kitty.placements().len(), 1);

        harness.feed(b"\x1b_Ga=d,d=A\x1b\\");
        assert!(harness.refresh());
        assert!(harness.kitty.placements().is_empty());
        assert!(
            harness.kitty.image_mut(4).is_none(),
            "deleted image data must not keep a texture alive"
        );
    }

    #[test]
    fn source_crops_resolve_to_normalized_rects() {
        let mut harness = Harness::new(5, 20);
        // Show only the right half of a 2-cell-wide image.
        harness.transmit_and_place(
            11,
            2 * CELL_WIDTH,
            CELL_HEIGHT,
            &format!(",x={CELL_WIDTH},w={CELL_WIDTH}"),
        );

        harness.refresh();
        let view = &harness.kitty.placements()[0];
        assert_eq!(view.source_rect, [0.5, 0.0, 1.0, 1.0]);
        assert_eq!(view.columns, 1.0);
    }

    #[test]
    fn virtual_placements_follow_their_placeholder_cells() {
        let mut harness = Harness::new(5, 20);
        harness.transmit_and_place(42, 2 * CELL_WIDTH, CELL_HEIGHT, ",U=1,c=2,r=1");
        harness.refresh();
        assert!(
            harness.kitty.placements().is_empty(),
            "a virtual placement must not draw before placeholders exist"
        );

        // The application prints the placeholder cells itself, image id in
        // the foreground color and row/column in combining diacritics.
        let encode = rio_vt::ansi::kitty_virtual::encode_placeholder;
        harness.feed(b"\x1b[3;4H\x1b[38;2;0;0;42m");
        harness.feed(encode(0, 0, None).as_bytes());
        harness.feed(encode(0, 1, None).as_bytes());
        harness.feed(b"\x1b[m");

        assert!(harness.refresh());
        let views = harness.kitty.placements();
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.image_id, 42);
        assert_eq!((view.row, view.col), (2.0, 3.0));
        assert_eq!((view.columns, view.rows), (2.0, 1.0));
    }

    #[test]
    fn queries_answer_without_placing() {
        let mut harness = Harness::new(5, 20);
        let payload = base64::engine::general_purpose::STANDARD.encode([0_u8, 0, 0, 0]);
        harness.feed(format!("\x1b_Ga=q,f=32,s=1,v=1,i=6;{payload}\x1b\\").as_bytes());

        let replies = harness.reply_text();
        assert!(
            replies.contains("\x1b_Gi=6;OK\x1b\\"),
            "queries must be acknowledged, got {replies:?}"
        );
        harness.refresh();
        assert!(harness.kitty.placements().is_empty());
    }
}

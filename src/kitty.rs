//! Kitty graphics rendering state.
//!
//! rio-vt owns the kitty graphics *protocol*: it parses the APC stream,
//! decodes PNG, RGB, and RGBA payloads (chunked or not), stores images and
//! placements per screen, answers queries, applies deletes, and evicts
//! images over the spec's memory budget. This module owns what is ratty's
//! to draw: it turns the engine's placement state into the flat list of
//! textured quads the Bevy scene renders each frame, converting pixels
//! from the engine's image store into GPU textures on demand.
//!
//! The engine store is the single source of truth for pixels. Textures are
//! cached per image id and validated against the store's transmission
//! timestamp, so retransmissions, alternate-screen id collisions, and
//! evictions all resolve to whatever the engine currently holds.
//!
//! Two placement kinds reach the screen:
//!
//! - *Direct* placements (`a=T`/`a=p`) live in the engine as absolute,
//!   scrollback-aware grid positions. Their on-screen quad comes from the
//!   engine's own [`kitty_overlay_geometry`], clipped to the terminal
//!   surface with a proportional source-rect shrink, so partially scrolled
//!   images show the correct slice instead of overhanging the grid.
//! - *Virtual* placements (`U=1`, what `kitten icat --unicode-placeholder`
//!   emits) are anchored by U+10EEEE placeholder cells the application
//!   prints itself. Each row-run of placeholder cells is decoded with the
//!   engine's [`IncompletePlacement`] rules (image id in the foreground
//!   color — RGB or indexed — placement id in the underline color, row,
//!   column, and id high byte in combining diacritics) and rendered as its
//!   own slice via [`compute_run_geometry`], so scrolled or split
//!   placeholder regions show the right image tiles.

use std::collections::{HashMap, HashSet};

use rio_graphics::{ColorType, GraphicData, GraphicOverlay};
use rio_vt::ansi::graphics::{OverlayViewport, clip_overlay_to_rect, kitty_overlay_geometry};
use rio_vt::ansi::kitty_virtual::{IncompletePlacement, PLACEHOLDER, compute_run_geometry};
use rio_vt::crosswords::pos::Column;

use crate::inline::RasterObject;
use crate::vt::{self, VtTerminal};

/// A kitty placement view key: `(image_id, placement_id, run)`.
///
/// Direct placements use run `0`; virtual placements get one view per
/// placeholder row-run, keyed by the run's screen position.
pub type PlacementKey = (u32, u32, u32);

/// Kitty images and the placements to draw, derived from engine state.
#[derive(Default)]
pub struct KittyGraphics {
    /// Decoded images by kitty image id. Pixels are held until the render
    /// sync uploads them, then only the texture handle remains.
    images: HashMap<u32, KittyImage>,
    /// Placements to draw, sorted back-to-front.
    placements: Vec<KittyPlacementView>,
    /// Scroll state the current placement views were derived from.
    snapshot: Option<Snapshot>,
}

/// A decoded kitty image and its GPU upload state.
pub struct KittyImage {
    /// Pixel payload and texture handle.
    pub raster: RasterObject,
    /// Engine transmission timestamp. A mismatch with the engine store
    /// means the pixels changed under this id — a retransmission, or an
    /// alternate-screen swap where both screens use the same id — and the
    /// texture is rebuilt from the store.
    transmit_time: std::time::Instant,
}

/// The terminal state the placement views depend on besides the grid
/// content and the engine's dirty flag: any change here moves placements
/// without either of those signals firing.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    display_offset: usize,
    history_size: usize,
    lines_evicted: u64,
    direct: usize,
    r#virtual: usize,
}

impl Snapshot {
    fn of(term: &VtTerminal) -> Self {
        Self {
            display_offset: term.display_offset(),
            history_size: term.history_size(),
            lines_evicted: term.grid.lines_evicted(),
            direct: term.graphics.kitty_placements.len(),
            r#virtual: term.graphics.kitty_virtual_placements.len(),
        }
    }
}

/// One kitty placement resolved to visible-grid coordinates.
///
/// Spans are fractional cells: a placement shown at native pixel size
/// rarely lands on a cell boundary, and rounding it up would stretch the
/// image.
#[derive(Clone, Debug, PartialEq)]
pub struct KittyPlacementView {
    /// Kitty image id (`i=`).
    pub image_id: u32,
    /// Kitty placement id (`p=`, or engine-allocated).
    pub placement_id: u32,
    /// Distinguishes the row-runs of a virtual placement; `0` for direct.
    pub run: u32,
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
    /// Kitty z-index. Negative layers render behind the terminal surface.
    pub z: i32,
}

impl KittyPlacementView {
    /// Returns the plane-cache key for this view.
    pub fn key(&self) -> PlacementKey {
        (self.image_id, self.placement_id, self.run)
    }
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

    /// Synchronizes with the engine: re-derives the placement views and
    /// keeps the texture cache aligned with the engine's image store.
    /// Returns whether anything the renderer draws changed.
    ///
    /// `terminal_changed` marks frames where grid content changed (PTY
    /// output, resize); together with the engine's dirty flag and the
    /// scroll-state snapshot it gates the rebuild, so idle frames — and
    /// all frames on a terminal with no graphics — cost a few comparisons.
    pub fn refresh(&mut self, term: &mut VtTerminal, terminal_changed: bool) -> bool {
        let engine_dirty = std::mem::take(&mut term.graphics.kitty_graphics_dirty);
        let snapshot = Snapshot::of(term);
        if !engine_dirty && !terminal_changed && self.snapshot == Some(snapshot) {
            return false;
        }
        self.snapshot = Some(snapshot);

        let mut views = direct_placement_views(term);
        views.extend(virtual_placement_views(term));
        // Back-to-front by kitty z-index, then by keys so equal layers
        // keep a deterministic order across refreshes.
        views.sort_by_key(|view| (view.z, view.key()));

        let mut changed = self.ensure_textures(term, &views);
        changed |= self.prune_textures(term);
        if views != self.placements {
            self.placements = views;
            changed = true;
        }
        changed
    }

    /// Builds or refreshes textures for every image the views reference,
    /// pulling pixels from the engine's store.
    fn ensure_textures(&mut self, term: &VtTerminal, views: &[KittyPlacementView]) -> bool {
        let mut changed = false;
        let mut seen = HashSet::new();
        for view in views {
            if !seen.insert(view.image_id) {
                continue;
            }
            let Some(stored) = term.graphics.get_kitty_image(view.image_id) else {
                continue;
            };
            if self
                .images
                .get(&view.image_id)
                .is_some_and(|image| image.transmit_time == stored.transmission_time)
            {
                continue;
            }
            let Some(raster) = rasterize(&stored.data) else {
                continue;
            };
            self.images.insert(
                view.image_id,
                KittyImage {
                    raster,
                    transmit_time: stored.transmission_time,
                },
            );
            changed = true;
        }
        changed
    }

    /// Drops textures for images the engine no longer holds. Images on
    /// the *inactive* screen stay cached: the engine does not re-send
    /// pixels when the terminal swaps back from the alternate screen.
    fn prune_textures(&mut self, term: &VtTerminal) -> bool {
        let graphics = &term.graphics;
        let before = self.images.len();
        self.images.retain(|image_id, _| {
            graphics.get_kitty_image(*image_id).is_some()
                || graphics
                    .kitty_inactive_screen
                    .kitty_images
                    .contains_key(image_id)
        });
        before != self.images.len()
    }
}

/// Converts engine pixel data into ratty's RGBA raster form.
fn rasterize(data: &GraphicData) -> Option<RasterObject> {
    let width = u32::try_from(data.width).ok()?;
    let height = u32::try_from(data.height).ok()?;
    let pixel_count = data.width.checked_mul(data.height)?;
    let rgba = match data.color_type {
        ColorType::Rgba => {
            if data.pixels.len() != pixel_count.checked_mul(4)? {
                return None;
            }
            data.pixels.clone()
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

/// Resolves the engine's direct placements against the current viewport,
/// using the engine's own geometry and clipping helpers so ratty draws
/// exactly the slice rio's renderer would.
fn direct_placement_views(term: &VtTerminal) -> Vec<KittyPlacementView> {
    let graphics = &term.graphics;
    let cell_width = graphics.cell_width;
    let cell_height = graphics.cell_height;
    if cell_width < 1.0 || cell_height < 1.0 {
        return Vec::new();
    }

    let viewport = OverlayViewport {
        cell_width,
        cell_height,
        origin_x: 0.0,
        origin_y: 0.0,
        // `dest_row` is anchored in the stable row space that includes
        // rows evicted off the ring; fold them into the history so the
        // viewport top lines up.
        history_size: term.grid.lines_evicted() as i64 + term.history_size() as i64,
        display_offset: term.display_offset() as i64,
        screen_lines: term.screen_lines() as i64,
    };
    let surface_width = term.columns() as f32 * cell_width;
    let surface_height = term.screen_lines() as f32 * cell_height;

    let mut views = Vec::new();
    for (&(image_id, placement_id), placement) in &graphics.kitty_placements {
        let Some(stored) = graphics.get_kitty_image(image_id) else {
            continue;
        };
        let Some(geometry) =
            kitty_overlay_geometry(placement, stored.data.width, stored.data.height, &viewport)
        else {
            continue;
        };
        // Clip to the terminal surface: a partially scrolled placement
        // keeps its position and shows the covered part of the image,
        // instead of overhanging the grid.
        let mut overlay = GraphicOverlay {
            image_id: u64::from(image_id),
            x: geometry.x,
            y: geometry.y,
            width: geometry.width,
            height: geometry.height,
            z_index: placement.z_index,
            source_rect: geometry.source_rect,
        };
        if !clip_overlay_to_rect(&mut overlay, 0.0, 0.0, surface_width, surface_height) {
            continue;
        }
        views.push(KittyPlacementView {
            image_id,
            placement_id,
            run: 0,
            row: overlay.y / cell_height,
            col: overlay.x / cell_width,
            columns: overlay.width / cell_width,
            rows: overlay.height / cell_height,
            source_rect: overlay.source_rect,
            z: placement.z_index,
        });
    }
    views
}

/// Resolves virtual (`U=1`) placements from their placeholder cells.
///
/// The engine registers the placement metadata; the application prints the
/// U+10EEEE cells itself. Each row-run of consecutive placeholder cells is
/// decoded with the engine's continuation rules and rendered as one slice,
/// which keeps the image welded to the text while scrolling, splitting,
/// and erasing — kitty's reason for the placeholder mode to exist.
fn virtual_placement_views(term: &VtTerminal) -> Vec<KittyPlacementView> {
    if term.graphics.kitty_virtual_placements.is_empty() {
        return Vec::new();
    }
    let cell_width = term.graphics.cell_width;
    let cell_height = term.graphics.cell_height;
    if cell_width < 1.0 || cell_height < 1.0 {
        return Vec::new();
    }

    let rows = u16::try_from(term.screen_lines()).unwrap_or(u16::MAX);
    let cols = u16::try_from(term.columns()).unwrap_or(u16::MAX);
    let styles = vt::styles(term);
    let mut views = Vec::new();
    for row in 0..rows {
        let Some(grid_row) = vt::visible_row(term, row) else {
            continue;
        };
        // rio-vt flags rows holding a placeholder, so rows without one
        // skip the per-cell scan entirely.
        if !grid_row.kitty_virtual_placeholder {
            continue;
        }
        let mut current: Option<(IncompletePlacement, u16)> = None;
        for col in 0..cols {
            let square = grid_row[Column(usize::from(col))];
            let cell = (square.c() == PLACEHOLDER).then(|| {
                let style = styles
                    .get(usize::from(square.style_id()))
                    .copied()
                    .unwrap_or_default();
                // Combining marks carry the row/column/id-high diacritics.
                let combining = term
                    .grid
                    .cell_text(vt::visible_pos(term, row, col))
                    .skip(1)
                    .collect::<Vec<_>>();
                IncompletePlacement::from_cell(style.fg, style.underline_color, &combining)
            });
            current = match (current.take(), cell) {
                (Some((mut run, start)), Some(cell)) if run.can_append(&cell) => {
                    run.append();
                    Some((run, start))
                }
                (previous, cell) => {
                    if let Some((run, start)) = previous {
                        views.extend(run_view(term, &run, row, start, cell_width, cell_height));
                    }
                    cell.map(|cell| (cell, col))
                }
            };
        }
        if let Some((run, start)) = current {
            views.extend(run_view(term, &run, row, start, cell_width, cell_height));
        }
    }
    views
}

/// Resolves one placeholder row-run into a placement view.
fn run_view(
    term: &VtTerminal,
    run: &IncompletePlacement,
    screen_row: u16,
    start_col: u16,
    cell_width: f32,
    cell_height: f32,
) -> Option<KittyPlacementView> {
    let run = run.complete();
    let virtual_placements = &term.graphics.kitty_virtual_placements;
    // Exact key first; placeholder streams that omit the underline color
    // (placement id 0) fall back to any placement of the image.
    let (&(image_id, placement_id), placement) = virtual_placements
        .get_key_value(&(run.image_id, run.placement_id))
        .or_else(|| {
            virtual_placements
                .iter()
                .find(|((id, _), _)| *id == run.image_id)
        })?;
    let stored = term.graphics.get_kitty_image(image_id)?;
    let geometry = compute_run_geometry(
        &run,
        placement.columns,
        placement.rows,
        u32::try_from(stored.data.width).ok()?,
        u32::try_from(stored.data.height).ok()?,
        (placement.x, placement.y, placement.width, placement.height),
        cell_width,
        cell_height,
        0.0,
        0.0,
        usize::from(screen_row),
        usize::from(start_col),
    )?;
    Some(KittyPlacementView {
        image_id,
        placement_id,
        run: (u32::from(screen_row) << 16) | u32::from(start_col),
        row: geometry.y / cell_height,
        col: geometry.x / cell_width,
        columns: geometry.width / cell_width,
        rows: geometry.height / cell_height,
        source_rect: geometry.source_rect,
        z: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::Engine as _;
    use rio_vt::ansi::CursorShape;
    use rio_vt::ansi::kitty_virtual::encode_placeholder;
    use rio_vt::crosswords::{Crosswords, CrosswordsSize};
    use rio_vt::event::WindowId;
    use rio_vt::performer::handler::Processor;

    use crate::vt::TerminalEventSink;

    const CELL_WIDTH: u32 = 10;
    const CELL_HEIGHT: u32 = 20;
    const FULL: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

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

        /// Refreshes as a frame with terminal changes would.
        fn refresh(&mut self) -> bool {
            self.kitty.refresh(&mut self.term, true)
        }

        /// Refreshes as a quiet frame would: only the engine dirty flag
        /// and the scroll snapshot can trigger a rebuild.
        fn refresh_quiet(&mut self) -> bool {
            self.kitty.refresh(&mut self.term, false)
        }

        /// Transmits and places a solid RGBA image at the cursor.
        fn transmit_and_place(&mut self, image_id: u32, width: u32, height: u32, extra: &str) {
            self.transmit_pixels(image_id, width, height, [255, 255, 255, 255], extra);
        }

        fn transmit_pixels(
            &mut self,
            image_id: u32,
            width: u32,
            height: u32,
            pixel: [u8; 4],
            extra: &str,
        ) {
            let pixels = pixel.repeat((width * height) as usize);
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
        assert_eq!(view.source_rect, FULL);

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
    }

    #[test]
    fn quiet_frames_skip_the_rebuild() {
        let mut harness = Harness::new(5, 20);
        for line in 0..10 {
            harness.feed(format!("row{line}\r\n").as_bytes());
        }
        harness.transmit_and_place(3, CELL_WIDTH, CELL_HEIGHT, "");

        assert!(
            harness.refresh_quiet(),
            "the engine dirty flag must trigger a rebuild without a grid-change hint"
        );
        assert!(!harness.refresh_quiet(), "an idle frame must be a no-op");

        vt::set_scrollback(&mut harness.term, 2);
        assert!(
            harness.refresh_quiet(),
            "a scrollback change must rebuild the views"
        );
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
    fn scrolled_placements_clip_to_the_visible_slice() {
        let mut harness = Harness::new(3, 20);
        harness.transmit_and_place(5, CELL_WIDTH, 2 * CELL_HEIGHT, "");
        harness.refresh();
        assert_eq!(harness.kitty.placements()[0].row, 0.0);

        // Scroll the content up one row: the top half leaves the screen,
        // and the visible remainder shows the bottom half of the image.
        harness.feed(b"\x1b[3;1H\r\n");
        harness.refresh();
        let view = &harness.kitty.placements()[0];
        assert_eq!((view.row, view.rows), (0.0, 1.0));
        assert_eq!(view.source_rect, [0.0, 0.5, 1.0, 1.0]);

        // And another: fully above the viewport, so nothing to draw.
        harness.feed(b"\r\n");
        harness.refresh();
        assert!(harness.kitty.placements().is_empty());

        // Scrolling back into history brings the whole image back.
        vt::set_scrollback(&mut harness.term, 2);
        harness.refresh();
        let view = &harness.kitty.placements()[0];
        assert_eq!((view.row, view.rows), (0.0, 2.0));
        assert_eq!(view.source_rect, FULL);
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
    fn negative_z_layers_sort_first_and_stay_negative() {
        let mut harness = Harness::new(5, 20);
        harness.transmit_and_place(1, CELL_WIDTH, CELL_HEIGHT, ",z=1");
        harness.feed(b"\x1b[1;1H");
        harness.transmit_and_place(2, CELL_WIDTH, CELL_HEIGHT, ",z=-1");

        harness.refresh();
        let views = harness.kitty.placements();
        assert_eq!(views.len(), 2);
        assert_eq!((views[0].image_id, views[0].z), (2, -1));
        assert_eq!((views[1].image_id, views[1].z), (1, 1));
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
        harness.feed(b"\x1b[3;4H\x1b[38;2;0;0;42m");
        harness.feed(encode_placeholder(0, 0, None).as_bytes());
        harness.feed(encode_placeholder(0, 1, None).as_bytes());
        harness.feed(b"\x1b[m");

        assert!(harness.refresh());
        let views = harness.kitty.placements();
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.image_id, 42);
        assert_eq!((view.row, view.col), (2.0, 3.0));
        assert_eq!((view.columns, view.rows), (2.0, 1.0));
        assert_eq!(view.source_rect, FULL);
    }

    #[test]
    fn indexed_color_placeholders_render() {
        let mut harness = Harness::new(5, 20);
        harness.transmit_and_place(42, 2 * CELL_WIDTH, CELL_HEIGHT, ",U=1,c=2,r=1");

        // Ids up to 255 may arrive as an indexed foreground color.
        harness.feed(b"\x1b[1;1H\x1b[38;5;42m");
        harness.feed(encode_placeholder(0, 0, None).as_bytes());
        harness.feed(encode_placeholder(0, 1, None).as_bytes());
        harness.feed(b"\x1b[m");

        harness.refresh();
        let views = harness.kitty.placements();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].image_id, 42);
    }

    #[test]
    fn partially_shown_virtual_placements_slice_by_diacritics() {
        let mut harness = Harness::new(5, 20);
        // A 2x2-cell virtual placement, but the application only shows the
        // second image row: kitty semantics say those cells display the
        // bottom slice, never a squashed whole image.
        harness.transmit_and_place(9, 2 * CELL_WIDTH, 2 * CELL_HEIGHT, ",U=1,c=2,r=2");
        harness.feed(b"\x1b[1;1H\x1b[38;2;0;0;9m");
        harness.feed(encode_placeholder(1, 0, None).as_bytes());
        harness.feed(encode_placeholder(1, 1, None).as_bytes());
        harness.feed(b"\x1b[m");

        harness.refresh();
        let views = harness.kitty.placements();
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!((view.row, view.col), (0.0, 0.0));
        assert_eq!((view.columns, view.rows), (2.0, 1.0));
        assert_eq!(
            view.source_rect,
            [0.0, 0.5, 1.0, 1.0],
            "the run must show the image row its diacritics name"
        );
    }

    #[test]
    fn alt_screen_id_collisions_rebuild_from_the_engine_store() {
        let mut harness = Harness::new(5, 20);
        harness.transmit_pixels(1, 1, 1, [255, 0, 0, 255], "");
        harness.refresh();
        assert_eq!(
            &harness.kitty.image_mut(1).expect("image 1").raster.rgba,
            &[255, 0, 0, 255]
        );

        // An alt-screen app reuses id 1 for a different image.
        harness.feed(b"\x1b[?1049h");
        harness.transmit_pixels(1, 1, 1, [0, 0, 255, 255], "");
        harness.refresh();
        assert_eq!(
            &harness.kitty.image_mut(1).expect("image 1").raster.rgba,
            &[0, 0, 255, 255]
        );

        // Swapping back must restore the main screen's pixels even though
        // the engine sends no new UpdateGraphics event for them.
        harness.feed(b"\x1b[?1049l");
        harness.refresh();
        let views = harness.kitty.placements();
        assert_eq!(views.len(), 1);
        assert_eq!(
            &harness.kitty.image_mut(1).expect("image 1").raster.rgba,
            &[255, 0, 0, 255]
        );
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

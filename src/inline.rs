//! Inline object state and APC handling.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

use bevy::prelude::*;
use vt100::Callbacks;

use crate::kitty::{KittyOperation, KittyParserState, refresh_kitty_placeholder_anchors};
use crate::model::{ObjectSource, load_object_source, load_object_source_from_bytes};
use crate::rgp::{
    RGP_OSC_START, RgpOperation, RgpPlacementStyle, RgpPlacementUpdate, RgpRegisterSource,
    consume_sequence as consume_rgp_sequence, support_reply,
};
const APC_START: &[u8] = b"\x1b_";
const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;
const BEL: u8 = 0x07;

/// Envelope variants we slice out of the PTY byte stream for RGP/Kitty
/// dispatch. APC catches any APC sequence (Kitty graphics, RGP); the OSC
/// variant is specifically pre-filtered to our RGP prefix so unrelated OSC
/// traffic (window titles, hyperlinks, prompt marks) passes straight to
/// vt100.
#[derive(Clone, Copy)]
enum EnvelopeKind {
    Apc,
    RgpOsc,
}

/// Marker for 2D inline object sprites.
#[derive(Component)]
pub struct TerminalInlineObjectSprite;

/// Marker for 3D inline object planes.
#[derive(Component)]
pub struct TerminalInlineObjectPlane;

/// Marker for RGP-backed inline objects.
#[derive(Component)]
pub struct TerminalRgpObject {
    /// Registered object identifier.
    pub object_id: u32,
}

/// Inline object registry and anchor state.
#[derive(Resource, Default)]
pub struct TerminalInlineObjects {
    pending_bytes: Vec<u8>,
    pending_rgp_payloads: HashMap<u32, PendingRgpPayload>,
    kitty: KittyParserState,
    dirty: bool,
    last_viewport_size: Vec2,
    last_cols: u16,
    last_rows: u16,
    pub(crate) objects: HashMap<u32, InlineObject>,
    pub(crate) anchors: HashMap<u32, InlineAnchor>,
    /// Object ids whose `Place` arrived inside the current PTY chunk.
    /// Drained by [`Self::take_placed_this_chunk`] after each chunk so
    /// the per-chunk scroll-tracking pass can exempt them — their `row`
    /// is already in the post-chunk screen frame, not the pre-chunk one.
    placed_this_chunk: std::collections::HashSet<u32>,
}

impl TerminalInlineObjects {
    /// Consumes PTY output and extracts inline object control sequences.
    pub fn consume_pty_output<CB: Callbacks>(
        &mut self,
        chunk: &[u8],
        parser: &mut vt100::Parser<CB>,
    ) -> Vec<Vec<u8>> {
        self.pending_bytes.extend_from_slice(chunk);
        let mut replies = Vec::new();

        let mut cursor = 0;
        loop {
            let Some((start_offset, kind)) = find_next_envelope(&self.pending_bytes[cursor..])
            else {
                if cursor < self.pending_bytes.len() {
                    parser.process(&normalize_hvp_sequences(&self.pending_bytes[cursor..]));
                }
                self.pending_bytes.clear();
                return replies;
            };
            let start = cursor + start_offset;
            if cursor < start {
                parser.process(&normalize_hvp_sequences(&self.pending_bytes[cursor..start]));
            }

            let header_len = match kind {
                EnvelopeKind::Apc => APC_START.len(),
                EnvelopeKind::RgpOsc => RGP_OSC_START.len(),
            };
            let payload_start = start + header_len;
            let end_opt = match kind {
                EnvelopeKind::Apc => apc_end(&self.pending_bytes, payload_start),
                EnvelopeKind::RgpOsc => osc_end(&self.pending_bytes, payload_start),
            };
            let Some(end) = end_opt else {
                // Envelope spans the chunk boundary — keep these bytes
                // pending and wait for the next read.
                self.pending_bytes.drain(..start);
                return replies;
            };
            let sequence = self.pending_bytes[start..end].to_vec();
            let (handled, reply) = match kind {
                EnvelopeKind::Apc => {
                    self.handle_apc_sequence(&sequence, parser.screen().cursor_position())
                }
                EnvelopeKind::RgpOsc => match self.handle_rgp_sequence(&sequence) {
                    Some(reply) => (true, reply),
                    None => (false, None),
                },
            };
            if let Some(reply) = reply {
                replies.push(reply);
            }
            if !handled {
                parser.process(&sequence);
            }
            cursor = end;
        }
    }

    /// Returns whether inline objects need synchronization.
    pub fn needs_sync(&self, viewport_size: Vec2, cols: u16, rows: u16) -> bool {
        self.dirty
            || self.last_viewport_size != viewport_size
            || self.last_cols != cols
            || self.last_rows != rows
    }

    /// Marks synchronization as complete.
    pub fn finish_sync(&mut self, viewport_size: Vec2, cols: u16, rows: u16) {
        self.dirty = false;
        self.last_viewport_size = viewport_size;
        self.last_cols = cols;
        self.last_rows = rows;
    }

    /// Applies upward scroll to anchored objects.
    ///
    /// This unconditionally scrolls every anchor. Prefer
    /// [`apply_scroll_to_existing`] when the caller can identify which
    /// anchors existed *before* the chunk that triggered the scroll —
    /// otherwise a `Place` arriving in the same PTY chunk as a clear +
    /// repaint will be evicted by the very scroll that chunk produced.
    pub fn apply_scroll(&mut self, rows_scrolled: u16) {
        self.apply_scroll_to_existing(rows_scrolled, None);
    }

    /// Like [`apply_scroll`], but only mutates anchors whose object id is
    /// in `existing_ids`. Anchors added during the chunk that caused the
    /// scroll (typically by an in-chunk `Place`) are left untouched —
    /// their row was emitted relative to the post-scroll screen state.
    pub fn apply_scroll_to_existing(
        &mut self,
        rows_scrolled: u16,
        existing_ids: Option<&std::collections::HashSet<u32>>,
    ) {
        if rows_scrolled == 0 || self.anchors.is_empty() {
            return;
        }

        self.anchors.retain(|object_id, anchor| {
            if let Some(existing) = existing_ids
                && !existing.contains(object_id)
            {
                // Anchor was added during the chunk that just produced
                // this scroll diff — its row is already in the new
                // coordinate space, do not double-apply.
                return true;
            }
            if self
                .objects
                .get(object_id)
                .is_some_and(|object| !object.scrolls_with_text())
            {
                return true;
            }
            let new_row = anchor.row as i32 - rows_scrolled as i32;
            if new_row + anchor.rows as i32 <= 0 {
                return false;
            }
            anchor.row = new_row.max(0) as u16;
            true
        });
        self.dirty = true;
    }

    /// Returns whether any anchors need scroll tracking.
    pub fn has_scroll_tracked_anchors(&self) -> bool {
        self.anchors.keys().any(|object_id| {
            self.objects
                .get(object_id)
                .is_some_and(InlineObject::scrolls_with_text)
        })
    }

    /// Refreshes placeholder-derived Kitty anchors.
    pub fn refresh_placeholder_anchors(&mut self, screen: &vt100::Screen) {
        if refresh_kitty_placeholder_anchors(&self.objects, &mut self.anchors, screen) {
            self.dirty = true;
        }
    }

    fn set_anchor(&mut self, object_id: u32, anchor: InlineAnchor) {
        self.anchors.insert(object_id, anchor);
        self.placed_this_chunk.insert(object_id);
        self.dirty = true;
    }

    /// Drains the set of object ids whose `Place` arrived inside the
    /// chunk currently being consumed. The caller (`pump_pty_output`)
    /// uses this to exempt freshly-placed anchors from the chunk's
    /// scroll diff — their `row` is emitted in post-chunk screen
    /// coordinates, so applying the same chunk's scroll to them would
    /// be a double-count.
    pub fn take_placed_this_chunk(&mut self) -> std::collections::HashSet<u32> {
        std::mem::take(&mut self.placed_this_chunk)
    }

    fn remove_object(&mut self, object_id: u32) {
        self.objects.remove(&object_id);
        self.anchors.remove(&object_id);
        self.pending_rgp_payloads.remove(&object_id);
        self.dirty = true;
    }

    fn clear_objects(&mut self) {
        self.objects.clear();
        self.anchors.clear();
        self.pending_rgp_payloads.clear();
        self.dirty = true;
    }

    fn handle_apc_sequence(
        &mut self,
        sequence: &[u8],
        cursor_position: (u16, u16),
    ) -> (bool, Option<Vec<u8>>) {
        if let Some(reply) = self.handle_rgp_sequence(sequence) {
            return (true, reply);
        }

        let Some(operation) = self.kitty.consume_sequence(sequence, cursor_position) else {
            return (false, None);
        };

        match operation {
            KittyOperation::Pending | KittyOperation::Ignored => (true, None),
            KittyOperation::TransmitOnly { object_id, image } => {
                self.objects
                    .insert(object_id, InlineObject::KittyImage(image.rasterize()));
                self.dirty = true;
                (true, None)
            }
            KittyOperation::TransmitAndPlace {
                object_id,
                image,
                anchor,
            } => {
                self.remove_objects_at(&InlineAnchor {
                    row: anchor.row,
                    col: anchor.col,
                    columns: anchor.columns,
                    rows: anchor.rows,
                    style: InlineStyle::default(),
                });
                self.objects
                    .insert(object_id, InlineObject::KittyImage(image.rasterize()));
                self.set_anchor(
                    object_id,
                    InlineAnchor {
                        row: anchor.row,
                        col: anchor.col,
                        columns: anchor.columns,
                        rows: anchor.rows,
                        style: InlineStyle::default(),
                    },
                );
                (true, None)
            }
            KittyOperation::PlaceExisting { object_id, anchor } => {
                if self.objects.contains_key(&object_id) {
                    self.set_anchor(
                        object_id,
                        InlineAnchor {
                            row: anchor.row,
                            col: anchor.col,
                            columns: anchor.columns,
                            rows: anchor.rows,
                            style: InlineStyle::default(),
                        },
                    );
                }
                (true, None)
            }
            KittyOperation::Delete { object_id } => {
                if let Some(object_id) = object_id {
                    self.remove_object(object_id);
                } else {
                    self.clear_objects();
                }
                (true, None)
            }
        }
    }

    fn handle_rgp_sequence(&mut self, sequence: &[u8]) -> Option<Option<Vec<u8>>> {
        let operation = consume_rgp_sequence(sequence)?;
        Some(match operation {
            RgpOperation::SupportQuery => Some(support_reply()),
            RgpOperation::Register {
                object_id,
                format,
                source,
            } => {
                if format != "obj" && format != "glb" {
                    warn!("unsupported RGP object format `{format}` for object {object_id}");
                    None
                } else {
                    match source {
                        RgpRegisterSource::Path { path } => {
                            self.pending_rgp_payloads.remove(&object_id);
                            match load_object_source(Path::new(&path)) {
                                Ok((source, source_data)) => {
                                    info!("registered RGP object {} from {}", object_id, source);
                                    self.objects.insert(
                                        object_id,
                                        InlineObject::RgpObject(match source_data {
                                            ObjectSource::Obj(meshes) => RgpInlineObject::Obj {
                                                meshes,
                                                handles: None,
                                            },
                                            ObjectSource::Gltf(asset_path) => {
                                                RgpInlineObject::Gltf {
                                                    asset_path,
                                                    handle: None,
                                                }
                                            }
                                        }),
                                    );
                                    self.dirty = true;
                                    None
                                }
                                Err(error) => {
                                    warn!("failed to load RGP object {object_id}: {error:#}");
                                    None
                                }
                            }
                        }
                        RgpRegisterSource::Payload { name, more, data } => {
                            self.handle_rgp_payload_chunk(object_id, &format, name, more, data)
                        }
                    }
                }
            }
            RgpOperation::Place { object_id, anchor } => {
                if self.objects.contains_key(&object_id) {
                    let row = anchor
                        .row
                        .saturating_sub(anchor.rows.saturating_sub(1).div_ceil(2) as u16);
                    let col = anchor
                        .col
                        .saturating_sub(anchor.columns.saturating_sub(1).div_ceil(2) as u16);
                    self.set_anchor(
                        object_id,
                        InlineAnchor {
                            row,
                            col,
                            columns: anchor.columns,
                            rows: anchor.rows,
                            style: anchor.style.into(),
                        },
                    );
                }
                None
            }
            RgpOperation::Update { object_id, update } => {
                if let Some(anchor) = self.anchors.get_mut(&object_id) {
                    apply_rgp_update(&mut anchor.style, update);
                    self.dirty = true;
                }
                None
            }
            RgpOperation::Delete { object_id } => {
                if let Some(object_id) = object_id {
                    self.remove_object(object_id);
                } else {
                    self.clear_objects();
                }
                None
            }
            RgpOperation::Ignored => None,
        })
    }

    fn remove_objects_at(&mut self, new_anchor: &InlineAnchor) {
        let row_start = new_anchor.row as i32;
        let row_end = row_start + new_anchor.rows as i32;
        let col_start = new_anchor.col as i32;
        let col_end = col_start + new_anchor.columns as i32;

        let overlapping_ids = self
            .anchors
            .iter()
            .filter_map(|(object_id, anchor)| {
                let anchor_row_start = anchor.row as i32;
                let anchor_row_end = anchor_row_start + anchor.rows as i32;
                let anchor_col_start = anchor.col as i32;
                let anchor_col_end = anchor_col_start + anchor.columns as i32;

                (anchor_row_start < row_end
                    && anchor_row_end > row_start
                    && anchor_col_start < col_end
                    && anchor_col_end > col_start)
                    .then_some(*object_id)
            })
            .collect::<Vec<_>>();

        for object_id in overlapping_ids {
            self.objects.remove(&object_id);
            self.anchors.remove(&object_id);
        }
    }

    // Buffers chunked payload registrations until the final chunk arrives, then loads and registers the object.
    fn handle_rgp_payload_chunk(
        &mut self,
        object_id: u32,
        format: &str,
        name: Option<String>,
        more: bool,
        data: Vec<u8>,
    ) -> Option<Vec<u8>> {
        let pending = self
            .pending_rgp_payloads
            .entry(object_id)
            .or_insert_with(|| PendingRgpPayload {
                format: format.to_string(),
                name: name.clone(),
                data: Vec::new(),
            });
        if pending.format != format {
            warn!(
                "ignoring RGP payload chunk for object {} due to format mismatch ({} vs {})",
                object_id, pending.format, format
            );
            return None;
        }
        if pending.name.is_none() {
            pending.name = name;
        }
        pending.data.extend_from_slice(&data);
        info!(
            "received RGP payload chunk for object {} (format={}, accumulated={} bytes, more={})",
            object_id,
            pending.format,
            pending.data.len(),
            more
        );
        if more {
            return None;
        }

        let pending = self.pending_rgp_payloads.remove(&object_id)?;
        info!(
            "finalizing RGP payload for object {} (format={}, total={} bytes)",
            object_id,
            pending.format,
            pending.data.len()
        );
        match load_object_source_from_bytes(&pending.format, pending.name.as_deref(), &pending.data)
        {
            Ok((source, source_data)) => {
                info!("registered RGP object {} from {}", object_id, source);
                self.objects.insert(
                    object_id,
                    InlineObject::RgpObject(match source_data {
                        ObjectSource::Obj(meshes) => RgpInlineObject::Obj {
                            meshes,
                            handles: None,
                        },
                        ObjectSource::Gltf(asset_path) => RgpInlineObject::Gltf {
                            asset_path,
                            handle: None,
                        },
                    }),
                );
                self.dirty = true;
                None
            }
            Err(error) => {
                warn!("failed to load RGP object {object_id}: {error:#}");
                None
            }
        }
    }
}

struct PendingRgpPayload {
    format: String,
    name: Option<String>,
    data: Vec<u8>,
}

fn normalize_hvp_sequences(bytes: &[u8]) -> Cow<'_, [u8]> {
    // vt100 handles CUP (`H`) but not HVP (`f`), so normalize cursor-positioning sequences.
    let mut normalized = None;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 2 < bytes.len() && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && matches!(bytes[j], b'0'..=b'9' | b';') {
                j += 1;
            }

            if j < bytes.len() && bytes[j] == b'f' && j > i + 2 {
                let out = normalized.get_or_insert_with(|| {
                    let mut out = Vec::with_capacity(bytes.len());
                    out.extend_from_slice(&bytes[..i]);
                    out
                });
                out.extend_from_slice(&bytes[i..j]);
                out.push(b'H');
                i = j + 1;
                continue;
            }
        }

        if let Some(out) = normalized.as_mut() {
            out.push(bytes[i]);
        }
        i += 1;
    }

    match normalized {
        Some(bytes) => Cow::Owned(bytes),
        None => Cow::Borrowed(bytes),
    }
}

fn apc_end(bytes: &[u8], payload_start: usize) -> Option<usize> {
    let mut index = payload_start;
    loop {
        if index >= bytes.len() {
            return None;
        }
        if bytes[index] == C1_ST {
            return Some(index + 1);
        }
        if index + 1 < bytes.len() && bytes[index] == ST[0] && bytes[index + 1] == ST[1] {
            return Some(index + 2);
        }
        index += 1;
    }
}

/// Scans for the OSC string terminator. xterm spec allows BEL, ST (`ESC \`),
/// or C1 ST (`0x9c`). All three survive ConPTY in our tests.
fn osc_end(bytes: &[u8], payload_start: usize) -> Option<usize> {
    let mut index = payload_start;
    loop {
        if index >= bytes.len() {
            return None;
        }
        if bytes[index] == BEL || bytes[index] == C1_ST {
            return Some(index + 1);
        }
        if index + 1 < bytes.len() && bytes[index] == ST[0] && bytes[index + 1] == ST[1] {
            return Some(index + 2);
        }
        index += 1;
    }
}

/// Returns the byte offset of the next RGP envelope (APC or OSC RGP), and
/// which envelope kind it is. Picks whichever appears first in the chunk;
/// arbitrary OSC traffic that isn't RGP-prefixed is left for vt100.
fn find_next_envelope(bytes: &[u8]) -> Option<(usize, EnvelopeKind)> {
    let apc = bytes.windows(APC_START.len()).position(|w| w == APC_START);
    let osc = bytes
        .windows(RGP_OSC_START.len())
        .position(|w| w == RGP_OSC_START);
    match (apc, osc) {
        (None, None) => None,
        (Some(a), None) => Some((a, EnvelopeKind::Apc)),
        (None, Some(o)) => Some((o, EnvelopeKind::RgpOsc)),
        (Some(a), Some(o)) if a <= o => Some((a, EnvelopeKind::Apc)),
        (Some(_), Some(o)) => Some((o, EnvelopeKind::RgpOsc)),
    }
}

/// Registered inline object.
pub enum InlineObject {
    /// Kitty image object.
    KittyImage(KittyInlineObject),
    /// Ratty graphics object.
    RgpObject(RgpInlineObject),
}

/// Raster image payload.
pub struct RasterObject {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// RGBA image bytes.
    pub rgba: Vec<u8>,
    /// Uploaded image handle.
    pub handle: Option<Handle<Image>>,
}

/// Kitty-backed inline object.
pub struct KittyInlineObject {
    /// Raster image payload.
    pub raster: RasterObject,
    /// Indicates placeholder-driven placement.
    pub uses_placeholders: bool,
}

/// RGP-backed inline object.
pub enum RgpInlineObject {
    /// OBJ mesh payload.
    Obj {
        /// Loaded mesh parts.
        meshes: Vec<Mesh>,
        /// Cached mesh handles keyed by depth.
        handles: Option<(u32, Vec<Handle<Mesh>>)>,
    },
    /// glTF scene payload.
    Gltf {
        /// Scene asset path.
        asset_path: String,
        /// Cached scene handle.
        handle: Option<Handle<Scene>>,
    },
}

impl InlineObject {
    fn scrolls_with_text(&self) -> bool {
        match self {
            InlineObject::KittyImage(object) => !object.uses_placeholders,
            InlineObject::RgpObject(_) => true,
        }
    }
}

/// Inline object anchor.
pub struct InlineAnchor {
    /// Anchor row.
    pub row: u16,
    /// Anchor column.
    pub col: u16,
    /// Object width in cells.
    pub columns: u32,
    /// Object height in cells.
    pub rows: u32,
    /// Inline styling.
    pub style: InlineStyle,
}

/// Inline object style.
#[derive(Clone, Copy, Default)]
pub struct InlineStyle {
    /// Enables default animation.
    pub animate: bool,
    /// Scale multiplier.
    pub scale: f32,
    /// Extrusion depth.
    pub depth: f32,
    /// Optional object color.
    pub color: Option<[u8; 3]>,
    /// Brightness multiplier.
    pub brightness: f32,
    /// Translation offset relative to the anchor.
    pub offset: Vec3,
    /// Rotation in degrees.
    pub rotation: Vec3,
    /// Non-uniform scale multiplier.
    pub scale3: Vec3,
}

impl From<RgpPlacementStyle> for InlineStyle {
    fn from(value: RgpPlacementStyle) -> Self {
        Self {
            animate: value.animate,
            scale: value.scale,
            depth: value.depth,
            color: value.color,
            brightness: value.brightness,
            offset: Vec3::from_array(value.offset),
            rotation: Vec3::from_array(value.rotation),
            scale3: Vec3::from_array(value.scale3),
        }
    }
}

fn apply_rgp_update(style: &mut InlineStyle, update: RgpPlacementUpdate) {
    if let Some(animate) = update.animate {
        style.animate = animate;
    }
    if let Some(scale) = update.scale {
        style.scale = scale;
    }
    if let Some(depth) = update.depth {
        style.depth = depth;
    }
    if let Some(color) = update.color {
        style.color = Some(color);
    }
    if let Some(brightness) = update.brightness {
        style.brightness = brightness;
    }
    apply_vec3_update(&mut style.offset, update.offset);
    apply_vec3_update(&mut style.rotation, update.rotation);
    apply_vec3_update(&mut style.scale3, update.scale3);
}

fn apply_vec3_update(target: &mut Vec3, update: [Option<f32>; 3]) {
    if let Some(x) = update[0] {
        target.x = x;
    }
    if let Some(y) = update[1] {
        target.y = y;
    }
    if let Some(z) = update[2] {
        target.z = z;
    }
}

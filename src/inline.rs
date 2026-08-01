//! Inline object state and APC handling.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

use bevy::prelude::*;
use vt100::Callbacks;

use crate::kitty::{KittyOperation, KittyParserState, refresh_kitty_placeholder_anchors};
use crate::model::{ObjectSource, load_object_source, load_object_source_from_bytes};
use crate::rgp::{
    RgpAnchorMode, RgpOperation, RgpPlacementStyle, RgpPlacementUpdate, RgpRegisterSource,
    consume_sequence as consume_rgp_sequence, support_reply,
};
const APC_START: &[u8] = b"\x1b_";
const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;

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
    next_rgp_marker_id: u32,
    pub(crate) objects: HashMap<u32, InlineObject>,
    pub(crate) anchors: HashMap<u32, InlineAnchor>,
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
            let Some(start_offset) = self.pending_bytes[cursor..]
                .windows(APC_START.len())
                .position(|window| window == APC_START)
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

            let payload_start = start + APC_START.len();
            let Some(end) = apc_end(&self.pending_bytes, payload_start) else {
                self.pending_bytes.drain(..start);
                return replies;
            };
            let sequence = self.pending_bytes[start..end].to_vec();
            let (handled, reply) = self.handle_apc_sequence(&sequence, parser);
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

    /// Applies inferred upward scroll to legacy scroll-coupled objects.
    pub fn apply_scroll(&mut self, rows_scrolled: u16) {
        if rows_scrolled == 0 || self.anchors.is_empty() {
            return;
        }

        for (object_id, anchor) in &mut self.anchors {
            let scrolls_with_text = match self.objects.get(object_id) {
                Some(InlineObject::KittyImage(object)) => !object.uses_placeholders,
                Some(InlineObject::RgpObject(_)) => anchor.marker_id.is_some(),
                None => false,
            };
            if scrolls_with_text {
                anchor.row -= i32::from(rows_scrolled);
                anchor.visible = anchor.row + anchor.rows as i32 > 0;
            }
        }
        self.dirty = true;
    }

    /// Returns whether any anchors need scroll tracking.
    pub fn has_scroll_tracked_anchors(&self) -> bool {
        self.anchors
            .iter()
            .any(|(object_id, anchor)| match self.objects.get(object_id) {
                Some(InlineObject::KittyImage(object)) => !object.uses_placeholders,
                Some(InlineObject::RgpObject(_)) => anchor.marker_id.is_some(),
                None => false,
            })
    }

    /// Refreshes anchors derived from markers in the terminal buffer.
    pub fn refresh_placeholder_anchors(&mut self, screen: &vt100::Screen) {
        self.refresh_anchors(screen, false);
    }

    /// Moves text anchors with a scrollback viewport change, then resolves visible markers.
    pub fn apply_scrollback_change(&mut self, previous: usize, screen: &vt100::Screen) {
        let delta = scrollback_row_delta(previous, screen.scrollback());
        if delta != 0 {
            let viewport_rows = screen.size().0;
            for anchor in self.anchors.values_mut() {
                if anchor.mode == InlineAnchorMode::Text && anchor.marker_id.is_some() {
                    anchor.row = anchor.row.saturating_add(delta);
                    anchor.visible =
                        anchor_intersects_viewport(anchor.row, anchor.rows, viewport_rows);
                }
            }
            self.dirty = true;
        }
        self.refresh_anchors(screen, true);
    }

    /// Refreshes anchors after PTY output, retaining objects that are still scrolling offscreen.
    pub fn refresh_anchors_after_output(&mut self, screen: &vt100::Screen) {
        self.refresh_anchors(screen, true);
    }

    fn refresh_anchors(&mut self, screen: &vt100::Screen, preserve_exiting: bool) {
        if refresh_kitty_placeholder_anchors(&self.objects, &mut self.anchors, screen)
            | self.refresh_rgp_text_anchors(screen, preserve_exiting)
        {
            self.dirty = true;
        }
    }

    fn refresh_rgp_text_anchors(&mut self, screen: &vt100::Screen, preserve_exiting: bool) -> bool {
        let tracked_anchors = self
            .anchors
            .iter()
            .filter_map(|(object_id, anchor)| {
                (anchor.mode == InlineAnchorMode::Text).then_some((*object_id, anchor.marker_id?))
            })
            .collect::<Vec<_>>();
        if tracked_anchors.is_empty() {
            return false;
        }

        let mut positions = HashMap::new();
        let (rows, cols) = screen.size();
        for row in 0..rows {
            for col in 0..cols {
                let Some(object_id) = screen
                    .cell(row, col)
                    .and_then(|cell| rgp_text_marker_id(cell.contents()))
                else {
                    continue;
                };
                positions.insert(object_id, (row, col));
            }
        }

        let mut changed = false;
        for (object_id, marker_id) in tracked_anchors {
            let Some(anchor) = self.anchors.get_mut(&object_id) else {
                continue;
            };
            if let Some((row, col)) = positions.get(&marker_id).copied() {
                let top = text_anchor_top(row, anchor.marker_row_offset);
                let left = col.saturating_sub(anchor.marker_col_offset);
                changed |= anchor.row != top || anchor.col != left || !anchor.visible;
                anchor.row = top;
                anchor.col = left;
                anchor.visible = true;
            } else if preserve_exiting && anchor.row < 0 {
                let visible = anchor_intersects_viewport_top(anchor.row, anchor.rows);
                changed |= anchor.visible != visible;
                anchor.visible = visible;
            } else {
                changed |= anchor.visible;
                anchor.visible = false;
            }
        }
        changed
    }

    fn set_anchor(&mut self, object_id: u32, anchor: InlineAnchor) {
        self.anchors.insert(object_id, anchor);
        self.dirty = true;
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

    fn handle_apc_sequence<CB: Callbacks>(
        &mut self,
        sequence: &[u8],
        parser: &mut vt100::Parser<CB>,
    ) -> (bool, Option<Vec<u8>>) {
        if let Some(reply) = self.handle_rgp_sequence(sequence, parser) {
            return (true, reply);
        }

        let cursor_position = parser.screen().cursor_position();
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
                    row: i32::from(anchor.row),
                    col: anchor.col,
                    columns: anchor.columns,
                    rows: anchor.rows,
                    mode: InlineAnchorMode::Screen,
                    marker_id: None,
                    marker_row_offset: 0,
                    marker_col_offset: 0,
                    visible: true,
                    style: InlineStyle::default(),
                });
                self.objects
                    .insert(object_id, InlineObject::KittyImage(image.rasterize()));
                self.set_anchor(
                    object_id,
                    InlineAnchor {
                        row: i32::from(anchor.row),
                        col: anchor.col,
                        columns: anchor.columns,
                        rows: anchor.rows,
                        mode: InlineAnchorMode::Screen,
                        marker_id: None,
                        marker_row_offset: 0,
                        marker_col_offset: 0,
                        visible: true,
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
                            row: i32::from(anchor.row),
                            col: anchor.col,
                            columns: anchor.columns,
                            rows: anchor.rows,
                            mode: InlineAnchorMode::Screen,
                            marker_id: None,
                            marker_row_offset: 0,
                            marker_col_offset: 0,
                            visible: true,
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

    fn handle_rgp_sequence<CB: Callbacks>(
        &mut self,
        sequence: &[u8],
        parser: &mut vt100::Parser<CB>,
    ) -> Option<Option<Vec<u8>>> {
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
                    let marker_row_offset = anchor.rows.saturating_sub(1).div_ceil(2) as u16;
                    let row = i32::from(anchor.row) - i32::from(marker_row_offset);
                    let col = anchor
                        .col
                        .saturating_sub(anchor.columns.saturating_sub(1).div_ceil(2) as u16);
                    let marker_col_offset = anchor.col.saturating_sub(col);
                    let marker_id =
                        (anchor.mode == RgpAnchorMode::Text).then(|| self.allocate_rgp_marker_id());
                    self.set_anchor(
                        object_id,
                        InlineAnchor {
                            row,
                            col,
                            columns: anchor.columns,
                            rows: anchor.rows,
                            mode: anchor.mode.into(),
                            marker_id,
                            marker_row_offset,
                            marker_col_offset,
                            visible: true,
                            style: anchor.style.into(),
                        },
                    );
                    if let Some(marker_id) = marker_id {
                        parser.process(rgp_text_marker(marker_id).as_bytes());
                    }
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

    fn allocate_rgp_marker_id(&mut self) -> u32 {
        let marker_id = self.next_rgp_marker_id;
        self.next_rgp_marker_id = self.next_rgp_marker_id.wrapping_add(1);
        marker_id
    }

    fn remove_objects_at(&mut self, new_anchor: &InlineAnchor) {
        let row_start = new_anchor.row;
        let row_end = row_start + new_anchor.rows as i32;
        let col_start = new_anchor.col as i32;
        let col_end = col_start + new_anchor.columns as i32;

        let overlapping_ids = self
            .anchors
            .iter()
            .filter_map(|(object_id, anchor)| {
                let anchor_row_start = anchor.row;
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

/// How an inline anchor follows terminal content.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InlineAnchorMode {
    /// Remain at fixed screen coordinates.
    #[default]
    Screen,
    /// Follow a marker attached to a terminal cell.
    Text,
}

impl From<RgpAnchorMode> for InlineAnchorMode {
    fn from(value: RgpAnchorMode) -> Self {
        match value {
            RgpAnchorMode::Screen => Self::Screen,
            RgpAnchorMode::Text => Self::Text,
        }
    }
}

/// Inline object anchor.
pub struct InlineAnchor {
    /// Anchor row.
    pub row: i32,
    /// Anchor column.
    pub col: u16,
    /// Object width in cells.
    pub columns: u32,
    /// Object height in cells.
    pub rows: u32,
    /// Anchor tracking mode.
    pub mode: InlineAnchorMode,
    /// Unique marker token for a text-tracked placement.
    pub marker_id: Option<u32>,
    /// Marker row offset from the placement's top-left cell.
    pub marker_row_offset: u16,
    /// Marker column offset from the placement's top-left cell.
    pub marker_col_offset: u16,
    /// Whether the anchor marker is currently visible.
    pub visible: bool,
    /// Inline styling.
    pub style: InlineStyle,
}

const VARIATION_SELECTOR_1: u32 = 0xfe00;
const VARIATION_SELECTOR_17: u32 = 0xe0100;

/// Encodes an object id as four zero-width Unicode variation selectors.
pub fn rgp_text_marker(object_id: u32) -> String {
    object_id
        .to_be_bytes()
        .into_iter()
        .map(|byte| {
            let codepoint = if byte < 16 {
                VARIATION_SELECTOR_1 + u32::from(byte)
            } else {
                VARIATION_SELECTOR_17 + u32::from(byte - 16)
            };
            char::from_u32(codepoint).expect("variation selector is a valid scalar")
        })
        .collect()
}

/// Removes an RGP text marker suffix before a cell is rendered.
pub fn strip_rgp_text_marker(contents: &str) -> &str {
    let Some((start, _)) = decode_rgp_text_marker(contents) else {
        return contents;
    };
    &contents[..start]
}

fn rgp_text_marker_id(contents: &str) -> Option<u32> {
    decode_rgp_text_marker(contents).map(|(_, object_id)| object_id)
}

fn decode_rgp_text_marker(contents: &str) -> Option<(usize, u32)> {
    let chars = contents.char_indices().collect::<Vec<_>>();
    if chars.len() < 4 {
        return None;
    }
    let marker = &chars[chars.len() - 4..];
    let mut bytes = [0; 4];
    for (index, (_, ch)) in marker.iter().enumerate() {
        let codepoint = u32::from(*ch);
        bytes[index] = if (VARIATION_SELECTOR_1..VARIATION_SELECTOR_1 + 16).contains(&codepoint) {
            (codepoint - VARIATION_SELECTOR_1) as u8
        } else if (VARIATION_SELECTOR_17..VARIATION_SELECTOR_17 + 240).contains(&codepoint) {
            (codepoint - VARIATION_SELECTOR_17 + 16) as u8
        } else {
            return None;
        };
    }
    Some((marker[0].0, u32::from_be_bytes(bytes)))
}

fn text_anchor_top(marker_row: u16, marker_row_offset: u16) -> i32 {
    i32::from(marker_row) - i32::from(marker_row_offset)
}

fn anchor_intersects_viewport_top(row: i32, rows: u32) -> bool {
    row + rows as i32 > 0
}

fn anchor_intersects_viewport(row: i32, rows: u32, viewport_rows: u16) -> bool {
    row < i32::from(viewport_rows) && anchor_intersects_viewport_top(row, rows)
}

fn scrollback_row_delta(previous: usize, current: usize) -> i32 {
    let delta = current as i128 - previous as i128;
    delta.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32
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

#[cfg(test)]
mod tests {
    use super::{
        anchor_intersects_viewport_top, rgp_text_marker, rgp_text_marker_id, scrollback_row_delta,
        strip_rgp_text_marker, text_anchor_top,
    };

    #[test]
    fn text_marker_round_trips_full_object_id() {
        for object_id in [0, 1, 0x00ff_ffff, u32::MAX] {
            let contents = format!("x{}", rgp_text_marker(object_id));
            assert_eq!(rgp_text_marker_id(&contents), Some(object_id));
            assert_eq!(strip_rgp_text_marker(&contents), "x");
        }
    }

    #[test]
    fn ordinary_variation_selector_is_not_a_marker() {
        let contents = "text\u{fe0f}";
        assert_eq!(rgp_text_marker_id(contents), None);
        assert_eq!(strip_rgp_text_marker(contents), contents);
    }

    #[test]
    fn vt_parser_attaches_marker_to_preceding_character() {
        let mut parser = vt100::Parser::new(2, 10, 0);
        let marker = rgp_text_marker(u32::MAX);
        parser.process(format!("x{marker}").as_bytes());

        let contents = parser.screen().cell(0, 0).unwrap().contents();
        assert_eq!(rgp_text_marker_id(contents), Some(u32::MAX));
        assert_eq!(strip_rgp_text_marker(contents), "x");
    }

    #[test]
    fn text_anchor_can_scroll_partially_above_viewport() {
        assert_eq!(text_anchor_top(0, 3), -3);
        assert!(anchor_intersects_viewport_top(-3, 8));
        assert!(anchor_intersects_viewport_top(-7, 8));
        assert!(!anchor_intersects_viewport_top(-8, 8));
    }

    #[test]
    fn scrollback_delta_moves_anchor_before_marker_is_visible() {
        assert_eq!(scrollback_row_delta(0, 1), 1);
        assert_eq!(scrollback_row_delta(4, 1), -3);
    }
}

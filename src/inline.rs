//! Inline object state and APC handling.

use std::collections::HashMap;
use std::path::Path;

use bevy::prelude::*;

use crate::camera::{OptionalVec3, TerminalCameraUpdate};
use crate::kitty::{KittyGraphics, PlacementKey};
use crate::model::{
    ObjectLoadOptions, load_object_source_from_bytes_with_options, load_object_source_with_options,
};
use crate::rgp::{
    RGP_APC_START, RgpOperation, RgpPlacementStyle, RgpPlacementUpdate, RgpRegisterSource,
    consume_sequence as consume_rgp_sequence, support_reply,
};
use crate::runtime::TerminalRuntime;

const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;

/// Longest unterminated RGP sequence held back from the engine. Past this,
/// the bytes are almost certainly not a real RGP message (chunked payloads
/// stay far smaller), so they flow through rather than freezing the screen.
const RGP_MAX_PENDING: usize = 8 * 1024 * 1024;

/// Marker for 2D inline object sprites.
#[derive(Component)]
pub struct TerminalInlineObjectSprite;

/// Marker for 3D inline object planes.
#[derive(Component)]
pub struct TerminalInlineObjectPlane;

/// Layout data used to animate Kitty image planes on the warped terminal surface.
#[derive(Component, Clone, Copy)]
pub(crate) struct InlineKittyPlaneLayout {
    /// Normalized horizontal center within the terminal plane.
    pub local_x: f32,
    /// Normalized vertical center within the terminal plane.
    pub local_y: f32,
    /// Normalized width within the terminal plane.
    pub local_width: f32,
    /// Normalized height within the terminal plane.
    pub local_height: f32,
    /// Horizontal mesh subdivision count.
    pub x_segments: u32,
    /// Vertical mesh subdivision count.
    pub y_segments: u32,
    /// Normalized source crop mapped onto the mesh UVs.
    pub source_rect: [f32; 4],
}

/// Cached GPU assets for a Kitty image plane attached to the terminal surface.
pub(crate) struct KittyPlaneCache {
    /// Cached horizontal mesh subdivision count.
    pub x_segments: u32,
    /// Cached vertical mesh subdivision count.
    pub y_segments: u32,
    /// Cached normalized source crop baked into the mesh UVs.
    pub source_rect: [f32; 4],
    /// Cached plane mesh handle.
    pub mesh: Handle<Mesh>,
    /// Cached plane material handle.
    pub material: Handle<StandardMaterial>,
}

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
    dirty: bool,
    last_viewport_size: Vec2,
    last_cols: u16,
    last_rows: u16,
    pub(crate) objects: HashMap<u32, RgpInlineObject>,
    pub(crate) anchors: HashMap<u32, InlineAnchor>,
    /// Kitty graphics derived from rio-vt's engine state.
    pub(crate) kitty: KittyGraphics,
    /// Cached plane assets per kitty placement (3D presentation).
    pub(crate) kitty_planes: HashMap<PlacementKey, KittyPlaneCache>,
}

impl TerminalInlineObjects {
    /// Consumes PTY output, extracting RGP control sequences and streaming
    /// everything else — kitty graphics APCs included — straight through to
    /// the engine, which parses APC statefully itself.
    ///
    /// Only bytes that could still become an RGP sequence are withheld: a
    /// partial `ESC _ratty;g;` prefix at the tail of a chunk, or a matched
    /// prefix whose terminator has not arrived yet (capped, so a runaway
    /// stream cannot freeze the display). Everything else flows to the
    /// engine immediately, so a multi-megabyte kitty transfer is never
    /// buffered or rescanned here.
    pub fn consume_pty_output(
        &mut self,
        chunk: &[u8],
        runtime: &mut TerminalRuntime,
        camera_updates: &mut Vec<TerminalCameraUpdate>,
        terminal_output: &mut bool,
    ) -> Vec<Vec<u8>> {
        self.pending_bytes.extend_from_slice(chunk);
        let mut replies = Vec::new();

        let mut cursor = 0;
        loop {
            match next_rgp_candidate(&self.pending_bytes, cursor) {
                RgpScan::None => {
                    if cursor < self.pending_bytes.len() {
                        *terminal_output = true;
                        runtime.process(&self.pending_bytes[cursor..]);
                    }
                    self.pending_bytes.clear();
                    return replies;
                }
                RgpScan::Partial(start) => {
                    if cursor < start {
                        *terminal_output = true;
                        runtime.process(&self.pending_bytes[cursor..start]);
                    }
                    self.pending_bytes.drain(..start);
                    return replies;
                }
                RgpScan::Complete(start) => {
                    if cursor < start {
                        *terminal_output = true;
                        runtime.process(&self.pending_bytes[cursor..start]);
                    }
                    let Some(end) = apc_end(&self.pending_bytes, start + RGP_APC_START.len())
                    else {
                        if self.pending_bytes.len() - start > RGP_MAX_PENDING {
                            // Runaway unterminated sequence: stop withholding
                            // and let the engine's APC parser deal with it.
                            *terminal_output = true;
                            runtime.process(&self.pending_bytes[start..]);
                            self.pending_bytes.clear();
                            return replies;
                        }
                        self.pending_bytes.drain(..start);
                        return replies;
                    };
                    let sequence = self.pending_bytes[start..end].to_vec();
                    let (handled, reply) = self.handle_rgp_apc(&sequence, camera_updates);
                    if let Some(reply) = reply {
                        replies.push(reply);
                    }
                    if !handled {
                        *terminal_output = true;
                        runtime.process(&sequence);
                    }
                    cursor = end;
                }
            }
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
    pub fn apply_scroll(&mut self, rows_scrolled: u16) {
        if rows_scrolled == 0 || self.anchors.is_empty() {
            return;
        }

        self.anchors.retain(|_, anchor| {
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
        !self.anchors.is_empty()
    }

    /// Synchronizes kitty graphics with the engine's state.
    ///
    /// Re-derives the visible placements and keeps textures aligned with
    /// the engine's image store. Runs every frame — placements move with
    /// scrollback without any PTY traffic — but the refresh gates itself
    /// on the engine's dirty flag, `terminal_changed`, and the scroll
    /// state, so quiet frames cost a few comparisons.
    pub fn refresh_kitty(&mut self, runtime: &mut TerminalRuntime, terminal_changed: bool) {
        if self.kitty.refresh(&mut runtime.term, terminal_changed) {
            self.dirty = true;
        }
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

    fn handle_rgp_apc(
        &mut self,
        sequence: &[u8],
        camera_updates: &mut Vec<TerminalCameraUpdate>,
    ) -> (bool, Option<Vec<u8>>) {
        // Only RGP is ratty's to parse. Kitty graphics APCs flow through to
        // rio-vt, which owns that protocol end to end. A sequence that
        // matched the RGP prefix but fails to parse also flows through, so
        // no bytes are ever swallowed silently.
        if let Some(reply) = self.handle_rgp_sequence(sequence, camera_updates) {
            return (true, reply);
        }
        (false, None)
    }

    fn handle_rgp_sequence(
        &mut self,
        sequence: &[u8],
        camera_updates: &mut Vec<TerminalCameraUpdate>,
    ) -> Option<Option<Vec<u8>>> {
        let operation = consume_rgp_sequence(sequence)?;
        Some(match operation {
            RgpOperation::SupportQuery => Some(support_reply()),
            RgpOperation::Camera {
                camera_slot,
                switch_immediately,
                settings,
            } => {
                camera_updates.push(TerminalCameraUpdate {
                    slot: camera_slot as usize,
                    activate: switch_immediately,
                    mode: settings.camera_type,
                    scale: settings.scale,
                    fov: settings.fov,
                    translation: OptionalVec3::from(settings.offset),
                    rotation_degrees: OptionalVec3::from(settings.rotation),
                });
                None
            }
            RgpOperation::Register {
                object_id,
                format,
                options,
                source,
            } => {
                let load_options = ObjectLoadOptions {
                    normalize: options.normalize,
                };
                if format != "obj" && format != "glb" && format != "stl" {
                    warn!("unsupported RGP object format `{format}` for object {object_id}");
                    None
                } else {
                    match source {
                        RgpRegisterSource::Path { path } => {
                            self.pending_rgp_payloads.remove(&object_id);
                            match load_object_source_with_options(Path::new(&path), load_options) {
                                Ok((source, source_data)) => {
                                    info!("registered RGP object {} from {}", object_id, source);
                                    self.objects.insert(object_id, source_data.into());
                                    self.dirty = true;
                                    None
                                }
                                Err(error) => {
                                    warn!("failed to load RGP object {object_id}: {error:#}");
                                    None
                                }
                            }
                        }
                        RgpRegisterSource::Payload { name, more, data } => self
                            .handle_rgp_payload_chunk(
                                object_id,
                                &format,
                                name,
                                more,
                                data,
                                load_options,
                            ),
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
                    let needs_respawn = update.depth.is_some()
                        || update.color.is_some()
                        || update.brightness.is_some();
                    apply_rgp_update(&mut anchor.style, update);
                    if needs_respawn {
                        self.dirty = true;
                    }
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

    // Buffers chunked payload registrations until the final chunk arrives, then loads and registers the object.
    fn handle_rgp_payload_chunk(
        &mut self,
        object_id: u32,
        format: &str,
        name: Option<String>,
        more: bool,
        data: Vec<u8>,
        options: ObjectLoadOptions,
    ) -> Option<Vec<u8>> {
        let pending = self
            .pending_rgp_payloads
            .entry(object_id)
            .or_insert_with(|| PendingRgpPayload {
                format: format.to_string(),
                name: name.clone(),
                data: Vec::new(),
                options,
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
        match load_object_source_from_bytes_with_options(
            &pending.format,
            pending.name.as_deref(),
            &pending.data,
            pending.options,
        ) {
            Ok((source, source_data)) => {
                info!("registered RGP object {} from {}", object_id, source);
                self.objects.insert(object_id, source_data.into());
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
    options: ObjectLoadOptions,
}

/// Where the next possible RGP sequence starts, relative to `bytes`.
enum RgpScan {
    /// No RGP prefix anywhere after `from` — everything can flow through.
    None,
    /// The buffer ends mid-prefix; bytes from here must wait for more input.
    Partial(usize),
    /// A full `ESC _ratty;g;` prefix starts here.
    Complete(usize),
}

/// Scans for the next byte position that matches the RGP APC prefix as far
/// as the buffer reaches. APC payloads cannot contain `ESC` (it terminates
/// them), so a match inside another sequence's payload is impossible.
fn next_rgp_candidate(bytes: &[u8], from: usize) -> RgpScan {
    let mut index = from;
    while let Some(offset) = bytes[index..].iter().position(|byte| *byte == 0x1b) {
        let start = index + offset;
        let available = bytes.len() - start;
        let compare = available.min(RGP_APC_START.len());
        if bytes[start..start + compare] == RGP_APC_START[..compare] {
            if compare < RGP_APC_START.len() {
                return RgpScan::Partial(start);
            }
            return RgpScan::Complete(start);
        }
        index = start + 1;
    }
    RgpScan::None
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

/// RGP-backed inline object.
pub enum RgpInlineObject {
    /// STL mesh payload.
    Stl {
        /// The loaded mesh
        mesh: Mesh,
        /// Cached extruded mesh handle keyed by extrusion depth.
        handle: Option<(u32, Handle<Mesh>)>,
    },
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
        handle: Option<Handle<WorldAsset>>,
    },
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

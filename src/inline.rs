use std::collections::HashMap;
use std::path::Path;

use bevy::prelude::*;

use crate::kitty::{KittyOperation, KittyParserState, refresh_kitty_placeholder_anchors};
use crate::model::{ObjectSource, load_object_source};
use crate::rgp::{
    RgpOperation,
    RgpPlacementStyle,
    consume_sequence as consume_rgp_sequence,
    support_reply,
};
const APC_START: &[u8] = b"\x1b_";
const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;

#[derive(Component)]
pub struct TerminalInlineObjectSprite;

#[derive(Component)]
pub struct TerminalInlineObjectPlane;

#[derive(Component)]
pub struct TerminalRgpObject {
    pub object_id: u32,
}

#[derive(Resource, Default)]
pub struct TerminalInlineObjects {
    pending_bytes: Vec<u8>,
    kitty: KittyParserState,
    dirty: bool,
    last_viewport_size: Vec2,
    last_cols: u16,
    last_rows: u16,
    pub(crate) objects: HashMap<u32, InlineObject>,
    pub(crate) anchors: HashMap<u32, InlineAnchor>,
}

impl TerminalInlineObjects {
    pub fn consume_pty_output(&mut self, chunk: &[u8], parser: &mut vt100::Parser) -> Vec<Vec<u8>> {
        self.pending_bytes.extend_from_slice(chunk);
        let mut replies = Vec::new();

        let mut cursor = 0;
        loop {
            let Some(start_offset) = self.pending_bytes[cursor..]
                .windows(APC_START.len())
                .position(|window| window == APC_START)
            else {
                if cursor < self.pending_bytes.len() {
                    parser.process(&self.pending_bytes[cursor..]);
                }
                self.pending_bytes.clear();
                return replies;
            };
            let start = cursor + start_offset;
            if cursor < start {
                parser.process(&self.pending_bytes[cursor..start]);
            }

            let payload_start = start + APC_START.len();
            let Some(end) = apc_end(&self.pending_bytes, payload_start) else {
                self.pending_bytes.drain(..start);
                return replies;
            };
            let sequence = self.pending_bytes[start..end].to_vec();
            let (handled, reply) = self.handle_apc_sequence(&sequence, parser.screen().cursor_position());
            if let Some(reply) = reply {
                replies.push(reply);
            }
            if !handled {
                parser.process(&sequence);
            }
            cursor = end;
        }
    }

    pub fn needs_sync(&self, viewport_size: Vec2, cols: u16, rows: u16) -> bool {
        self.dirty
            || self.last_viewport_size != viewport_size
            || self.last_cols != cols
            || self.last_rows != rows
    }

    pub fn finish_sync(&mut self, viewport_size: Vec2, cols: u16, rows: u16) {
        self.dirty = false;
        self.last_viewport_size = viewport_size;
        self.last_cols = cols;
        self.last_rows = rows;
    }

    pub fn apply_scroll(&mut self, rows_scrolled: u16) {
        if rows_scrolled == 0 || self.anchors.is_empty() {
            return;
        }

        self.anchors.retain(|object_id, anchor| {
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

    pub fn refresh_placeholder_anchors(&mut self, screen: &vt100::Screen) {
        if refresh_kitty_placeholder_anchors(&self.objects, &mut self.anchors, screen) {
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
        self.dirty = true;
    }

    fn clear_objects(&mut self) {
        self.objects.clear();
        self.anchors.clear();
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

        let Some(operation) = self
            .kitty
            .consume_sequence(sequence, cursor_position)
        else {
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
                path,
            } => {
                if format != "obj" && format != "glb" {
                    warn!("unsupported RGP object format `{format}` for object {object_id}");
                    None
                } else {
                    match load_object_source(Path::new(&path)) {
                        Ok((source, source_data)) => {
                            info!(
                                "registered RGP object {} from {}",
                                object_id,
                                source,
                            );
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

pub enum InlineObject {
    KittyImage(KittyInlineObject),
    RgpObject(RgpInlineObject),
}

pub struct RasterObject {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub handle: Option<Handle<Image>>,
}

pub struct KittyInlineObject {
    pub raster: RasterObject,
    pub uses_placeholders: bool,
}

pub enum RgpInlineObject {
    Obj {
        meshes: Vec<Mesh>,
        handles: Option<(u32, Vec<Handle<Mesh>>)>,
    },
    Gltf {
        asset_path: String,
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

pub struct InlineAnchor {
    pub row: u16,
    pub col: u16,
    pub columns: u32,
    pub rows: u32,
    pub style: InlineStyle,
}

#[derive(Clone, Copy, Default)]
pub struct InlineStyle {
    pub animate: bool,
    pub scale: f32,
    pub depth: f32,
    pub color: Option<[u8; 3]>,
    pub brightness: f32,
}

impl From<RgpPlacementStyle> for InlineStyle {
    fn from(value: RgpPlacementStyle) -> Self {
        Self {
            animate: value.animate,
            scale: value.scale,
            depth: value.depth,
            color: value.color,
            brightness: value.brightness,
        }
    }
}

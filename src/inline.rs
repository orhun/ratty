use std::collections::HashMap;

use bevy::prelude::*;

use crate::kitty::{
    KittyAnchor, KittyOperation, KittyParserState, KITTY_APC_START,
    refresh_kitty_placeholder_anchors,
};

#[derive(Component)]
pub struct TerminalInlineObjectSprite;

#[derive(Component)]
pub struct TerminalInlineObjectPlane;

#[derive(Resource, Default)]
pub struct TerminalInlineObjects {
    pending_bytes: Vec<u8>,
    kitty: KittyParserState,
    next_object_id: u32,
    dirty: bool,
    last_viewport_size: Vec2,
    last_cols: u16,
    last_rows: u16,
    pub(crate) objects: HashMap<u32, InlineObject>,
    pub(crate) anchors: HashMap<u32, InlineAnchor>,
}

impl TerminalInlineObjects {
    pub fn consume_pty_output(&mut self, chunk: &[u8], parser: &mut vt100::Parser) {
        self.pending_bytes.extend_from_slice(chunk);

        let mut cursor = 0;
        loop {
            let Some(start_offset) = self.pending_bytes[cursor..]
                .windows(KITTY_APC_START.len())
                .position(|window| window == KITTY_APC_START)
            else {
                if cursor < self.pending_bytes.len() {
                    parser.process(&self.pending_bytes[cursor..]);
                }
                self.pending_bytes.clear();
                return;
            };
            let start = cursor + start_offset;
            if cursor < start {
                parser.process(&self.pending_bytes[cursor..start]);
            }

            let payload_start = start + KITTY_APC_START.len();
            let Some(end) = ({
                let mut index = payload_start;
                loop {
                    if index >= self.pending_bytes.len() {
                        break None;
                    }
                    if self.pending_bytes[index] == 0x9c {
                        break Some(index + 1);
                    }
                    if index + 1 < self.pending_bytes.len()
                        && self.pending_bytes[index] == b'\x1b'
                        && self.pending_bytes[index + 1] == b'\\'
                    {
                        break Some(index + 2);
                    }
                    index += 1;
                }
            }) else {
                self.pending_bytes.drain(..start);
                return;
            };
            let sequence = self.pending_bytes[start..end].to_vec();
            if !self.handle_kitty_sequence(&sequence, parser.screen().cursor_position()) {
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

    fn handle_kitty_sequence(
        &mut self,
        sequence: &[u8],
        cursor_position: (u16, u16),
    ) -> bool {
        let Some(operation) = self
            .kitty
            .consume_sequence(sequence, cursor_position, self.next_object_id.max(1))
        else {
            return false;
        };

        match operation {
            KittyOperation::Pending | KittyOperation::Ignored => true,
            KittyOperation::TransmitOnly { object_id, image } => {
                self.next_object_id = self.next_object_id.max(object_id + 1);
                self.objects
                    .insert(object_id, InlineObject::KittyImage(image.rasterize()));
                self.dirty = true;
                true
            }
            KittyOperation::TransmitAndPlace {
                object_id,
                image,
                anchor,
            } => {
                self.next_object_id = self.next_object_id.max(object_id + 1);
                self.remove_objects_at(&InlineAnchor::from(anchor));
                self.objects
                    .insert(object_id, InlineObject::KittyImage(image.rasterize()));
                self.anchors.insert(object_id, InlineAnchor::from(anchor));
                self.dirty = true;
                true
            }
            KittyOperation::PlaceExisting { object_id, anchor } => {
                if self.objects.contains_key(&object_id) {
                    self.anchors.insert(object_id, InlineAnchor::from(anchor));
                    self.dirty = true;
                }
                true
            }
            KittyOperation::Delete { object_id } => {
                if let Some(object_id) = object_id {
                    self.objects.remove(&object_id);
                    self.anchors.remove(&object_id);
                } else {
                    self.objects.clear();
                    self.anchors.clear();
                }
                self.dirty = true;
                true
            }
        }
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

pub enum InlineObject {
    KittyImage(KittyInlineObject),
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

impl InlineObject {
    fn scrolls_with_text(&self) -> bool {
        match self {
            InlineObject::KittyImage(object) => !object.uses_placeholders,
        }
    }
}

pub struct InlineAnchor {
    pub row: u16,
    pub col: u16,
    pub columns: u32,
    pub rows: u32,
}

impl From<KittyAnchor> for InlineAnchor {
    fn from(anchor: KittyAnchor) -> Self {
        Self {
            row: anchor.row,
            col: anchor.col,
            columns: anchor.columns,
            rows: anchor.rows,
        }
    }
}

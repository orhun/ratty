use std::collections::HashMap;

use base64::Engine as _;

use crate::inline::{InlineAnchor, InlineObject, InlineStyle, KittyInlineObject, RasterObject};

pub const KITTY_APC_START: &[u8] = b"\x1b_G";
const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;

#[derive(Default)]
pub struct KittyParserState {
    transfer: Option<KittyTransfer>,
    next_object_id: u32,
}

impl KittyParserState {
    pub fn consume_sequence(
        &mut self,
        sequence: &[u8],
        cursor_position: (u16, u16),
    ) -> Option<KittyOperation> {
        if !sequence.starts_with(KITTY_APC_START) {
            return None;
        }

        let content_end = if sequence.ends_with(&[C1_ST]) {
            sequence.len() - 1
        } else if sequence.ends_with(ST) {
            sequence.len() - 2
        } else {
            return None;
        };
        let content = &sequence[KITTY_APC_START.len()..content_end];
        let separator = content.iter().position(|byte| *byte == b';')?;
        let header = std::str::from_utf8(&content[..separator]).ok()?;
        let payload = &content[separator + 1..];

        let mut params = HashMap::new();
        for part in header.split(',').filter(|part| !part.is_empty()) {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            params.insert(key, value);
        }

        let action = params.get("a").copied().unwrap_or("T");
        match action {
            "T" | "t" => {
                let starts_new_transfer = self.transfer.is_none()
                    || params.contains_key("a")
                    || params.contains_key("f")
                    || params.contains_key("s")
                    || params.contains_key("v")
                    || params.contains_key("i");
                if starts_new_transfer {
                    let object_id = params
                        .get("i")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(self.next_object_id.max(1));
                    self.next_object_id = self.next_object_id.max(object_id + 1);
                    self.transfer = Some(KittyTransfer {
                        action: action.to_owned(),
                        object_id,
                        format: params
                            .get("f")
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(100),
                        width: params.get("s").and_then(|value| value.parse().ok()),
                        height: params.get("v").and_then(|value| value.parse().ok()),
                        columns: params.get("c").and_then(|value| value.parse().ok()),
                        rows: params.get("r").and_then(|value| value.parse().ok()),
                        uses_placeholders: params.get("U").copied() == Some("1"),
                        anchor_row: cursor_position.0,
                        anchor_col: cursor_position.1,
                        bytes: Vec::new(),
                    });
                }

                let transfer = self.transfer.as_mut()?;
                let chunk = base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .ok()?;
                transfer.bytes.extend_from_slice(&chunk);

                if params.get("m").copied().unwrap_or("0") == "1" {
                    return Some(KittyOperation::Pending);
                }

                let transfer = self.transfer.take()?;
                let image = transfer.finalize()?;
                if transfer.action == "T" {
                    return Some(KittyOperation::TransmitAndPlace {
                        object_id: transfer.object_id,
                        image,
                        anchor: KittyAnchor {
                            row: transfer.anchor_row,
                            col: transfer.anchor_col,
                            columns: transfer.columns.unwrap_or(1),
                            rows: transfer.rows.unwrap_or(1),
                        },
                    });
                }
                Some(KittyOperation::TransmitOnly {
                    object_id: transfer.object_id,
                    image,
                })
            }
            "p" => Some(KittyOperation::PlaceExisting {
                object_id: params.get("i")?.parse().ok()?,
                anchor: KittyAnchor {
                    row: cursor_position.0,
                    col: cursor_position.1,
                    columns: params
                        .get("c")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1),
                    rows: params
                        .get("r")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1),
                },
            }),
            "d" => Some(match params.get("i").and_then(|value| value.parse().ok()) {
                Some(object_id) => KittyOperation::Delete {
                    object_id: Some(object_id),
                },
                None => KittyOperation::Delete { object_id: None },
            }),
            _ => Some(KittyOperation::Ignored),
        }
    }
}

#[derive(Default)]
pub struct KittyImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub uses_placeholders: bool,
}

impl KittyImage {
    pub fn rasterize(self) -> KittyInlineObject {
        KittyInlineObject {
            raster: RasterObject {
                width: self.width,
                height: self.height,
                rgba: self.rgba,
                handle: None,
            },
            uses_placeholders: self.uses_placeholders,
        }
    }
}

#[derive(Clone, Copy)]
pub struct KittyAnchor {
    pub row: u16,
    pub col: u16,
    pub columns: u32,
    pub rows: u32,
}

pub enum KittyOperation {
    Pending,
    Ignored,
    TransmitOnly {
        object_id: u32,
        image: KittyImage,
    },
    TransmitAndPlace {
        object_id: u32,
        image: KittyImage,
        anchor: KittyAnchor,
    },
    PlaceExisting {
        object_id: u32,
        anchor: KittyAnchor,
    },
    Delete {
        object_id: Option<u32>,
    },
}

struct KittyTransfer {
    action: String,
    object_id: u32,
    format: u32,
    width: Option<u32>,
    height: Option<u32>,
    columns: Option<u32>,
    rows: Option<u32>,
    uses_placeholders: bool,
    anchor_row: u16,
    anchor_col: u16,
    bytes: Vec<u8>,
}

impl KittyTransfer {
    fn finalize(&self) -> Option<KittyImage> {
        let (width, height, rgba) = match self.format {
            100 => {
                let image =
                    image::load_from_memory_with_format(&self.bytes, image::ImageFormat::Png)
                        .ok()?;
                let rgba = image.to_rgba8();
                (rgba.width(), rgba.height(), rgba.into_raw())
            }
            24 => {
                let width = self.width?;
                let height = self.height?;
                let expected = width as usize * height as usize * 3;
                if self.bytes.len() != expected {
                    return None;
                }
                let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
                for rgb in self.bytes.chunks_exact(3) {
                    rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
                (width, height, rgba)
            }
            32 => {
                let width = self.width?;
                let height = self.height?;
                let expected = width as usize * height as usize * 4;
                if self.bytes.len() != expected {
                    return None;
                }
                (width, height, self.bytes.clone())
            }
            _ => return None,
        };

        Some(KittyImage {
            width,
            height,
            rgba,
            uses_placeholders: self.uses_placeholders,
        })
    }
}

pub fn refresh_kitty_placeholder_anchors(
    objects: &HashMap<u32, InlineObject>,
    anchors: &mut HashMap<u32, InlineAnchor>,
    screen: &vt100::Screen,
) -> bool {
    let placeholder_ids = objects
        .iter()
        .filter_map(|(object_id, object)| match object {
            InlineObject::KittyImage(object) => object.uses_placeholders.then_some(*object_id),
            InlineObject::RgpObject(_) => None,
        })
        .collect::<Vec<_>>();
    if placeholder_ids.is_empty() {
        return false;
    }
    let placeholder_lookup = placeholder_ids
        .iter()
        .map(|object_id| (object_id & 0x00ff_ffff, *object_id))
        .collect::<HashMap<_, _>>();

    let mut bounds = HashMap::<u32, (u16, u16, u16, u16)>::new();
    let (rows, cols) = screen.size();
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if !cell.contents().starts_with('\u{10EEEE}') {
                continue;
            }
            let vt100::Color::Rgb(r, g, b) = cell.fgcolor() else {
                continue;
            };
            let placeholder_id = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
            let Some(object_id) = placeholder_lookup.get(&placeholder_id).copied() else {
                continue;
            };
            bounds
                .entry(object_id)
                .and_modify(|(top, left, bottom, right)| {
                    *top = (*top).min(row);
                    *left = (*left).min(col);
                    *bottom = (*bottom).max(row);
                    *right = (*right).max(col);
                })
                .or_insert((row, col, row, col));
        }
    }

    let mut changed = false;
    for object_id in placeholder_ids {
        if let Some((top, left, bottom, right)) = bounds.get(&object_id).copied() {
            let columns = u32::from(right - left + 1);
            let rows = u32::from(bottom - top + 1);
            let new_anchor = InlineAnchor {
                row: top,
                col: left,
                columns,
                rows,
                style: InlineStyle::default(),
            };
            changed |= anchors
                .insert(object_id, new_anchor)
                .is_none_or(|old_anchor| {
                    old_anchor.row != top
                        || old_anchor.col != left
                        || old_anchor.columns != columns
                        || old_anchor.rows != rows
                });
        } else {
            changed |= anchors.remove(&object_id).is_some();
        }
    }

    changed
}

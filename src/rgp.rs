pub const RGP_APC_START: &[u8] = b"\x1b_ratty;g;";
const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;

#[derive(Clone, Copy, Default)]
pub struct RgpPlacementStyle {
    pub animate: bool,
    pub scale: f32,
    pub depth: f32,
    pub color: Option<[u8; 3]>,
    pub brightness: f32,
}

pub fn consume_sequence(sequence: &[u8]) -> Option<RgpOperation> {
    if !sequence.starts_with(RGP_APC_START) {
        return None;
    }

    let content_end = if sequence.ends_with(&[C1_ST]) {
        sequence.len() - 1
    } else if sequence.ends_with(ST) {
        sequence.len() - 2
    } else {
        return None;
    };
    let content = std::str::from_utf8(&sequence[RGP_APC_START.len()..content_end]).ok()?;
    let mut parts = content.split(';');
    let verb = parts.next()?;
    let mut id = None;
    let mut format = None;
    let mut path = None;
    let mut row = None;
    let mut col = None;
    let mut width = None;
    let mut height = None;
    let mut animate = false;
    let mut scale = None;
    let mut depth = None;
    let mut color = None;
    let mut brightness = None;
    for part in parts.filter(|part| !part.is_empty()) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key {
            "id" => id = value.parse().ok(),
            "fmt" => format = Some(value.to_string()),
            "path" => path = Some(value.to_string()),
            "row" => row = value.parse().ok(),
            "col" => col = value.parse().ok(),
            "w" => width = value.parse().ok(),
            "h" => height = value.parse().ok(),
            "animate" => animate = value == "1",
            "scale" => scale = value.parse().ok(),
            "depth" => depth = value.parse().ok(),
            "color" | "tint" => color = parse_color(value),
            "brightness" => brightness = value.parse().ok(),
            _ => {}
        }
    }

    match verb {
        "s" => Some(RgpOperation::SupportQuery),
        "r" => Some(RgpOperation::Register {
            object_id: id?,
            format: format?,
            path: path?,
        }),
        "p" => Some(RgpOperation::Place {
            object_id: id?,
            anchor: RgpAnchor {
                row: row?,
                col: col?,
                columns: width?,
                rows: height?,
                style: RgpPlacementStyle {
                    animate,
                    scale: scale.unwrap_or(1.0),
                    depth: depth.unwrap_or(0.0),
                    color,
                    brightness: brightness.unwrap_or(1.0),
                },
            },
        }),
        "d" => Some(RgpOperation::Delete { object_id: id }),
        _ => Some(RgpOperation::Ignored),
    }
}

#[derive(Clone, Copy)]
pub struct RgpAnchor {
    pub row: u16,
    pub col: u16,
    pub columns: u32,
    pub rows: u32,
    pub style: RgpPlacementStyle,
}

pub enum RgpOperation {
    SupportQuery,
    Register {
        object_id: u32,
        format: String,
        path: String,
    },
    Place {
        object_id: u32,
        anchor: RgpAnchor,
    },
    Delete {
        object_id: Option<u32>,
    },
    Ignored,
}

pub fn support_reply() -> Vec<u8> {
    b"\x1b_ratty;g;s;v=1;fmt=obj|glb;path=1;anim=1;depth=1;color=1;brightness=1\x1b\\".to_vec()
}

fn parse_color(value: &str) -> Option<[u8; 3]> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        return None;
    }

    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}

pub const RGP_APC_START: &[u8] = b"\x1b_ratty;g;";
const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;

#[derive(Clone, Copy, Default)]
pub struct RgpPlacementStyle {
    pub animate: bool,
    pub scale: f32,
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
    b"\x1b_ratty;g;s;v=1;fmt=obj|glb;path=1;anim=1\x1b\\".to_vec()
}

pub fn register_reply(object_id: u32, status: u8) -> Vec<u8> {
    format!("\x1b_ratty;g;r;id={object_id};status={status}\x1b\\").into_bytes()
}

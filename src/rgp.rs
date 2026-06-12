//! Ratty Graphics Protocol parsing.

use base64::Engine as _;

/// Ratty Graphics Protocol APC envelope prefix (`ESC _ ratty;g;`).
pub const RGP_APC_START: &[u8] = b"\x1b_ratty;g;";

/// Ratty Graphics Protocol OSC envelope prefix (`ESC ] 8901 ; ratty ; g ;`).
///
/// The OSC variant is used on platforms where the parent terminal can't
/// transit APC sequences end-to-end — notably Windows ConPTY, which
/// consumes APC silently regardless of the new "passthrough" plumbing in
/// `microsoft/terminal` 1.22+.
///
/// OSC 8901 was chosen as a 4-digit private number with no known
/// assignment in any surveyed ecosystem. Concrete numbers to avoid and
/// why:
///
/// - 0,1,2,4,7,8,9,10,11,12,52,104,110-119 — recognised by conhost itself.
/// - 99 — kitty notifications.
/// - 133 — FinalTerm / Warp prompt marks.
/// - 633 — VS Code shell integration.
/// - 777 — urxvt.
/// - 1337 — iTerm2 image protocol.
/// - 9001 — `WTAction` in Microsoft Terminal's `OscActionCodes` enum
///   (`src/terminal/parser/OutputStateMachineEngine.hpp`); dispatching is
///   numeric-first, so `;ratty;` after the number does *not* prevent
///   conhost from consuming the whole sequence as a WT action.
pub const RGP_OSC_START: &[u8] = b"\x1b]8901;ratty;g;";

const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;
const BEL: u8 = 0x07;

/// Placement style for an RGP object.
#[derive(Clone, Copy, Default)]
pub struct RgpPlacementStyle {
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
    pub offset: [f32; 3],
    /// Rotation in degrees.
    pub rotation: [f32; 3],
    /// Non-uniform scale.
    pub scale3: [f32; 3],
}

/// Partial update for an RGP object placement.
#[derive(Clone, Copy, Default)]
pub struct RgpPlacementUpdate {
    /// Updates the default animation flag.
    pub animate: Option<bool>,
    /// Updates the uniform scale multiplier.
    pub scale: Option<f32>,
    /// Updates the extrusion depth.
    pub depth: Option<f32>,
    /// Updates the object color.
    pub color: Option<[u8; 3]>,
    /// Updates the brightness multiplier.
    pub brightness: Option<f32>,
    /// Updates the translation offset relative to the anchor.
    pub offset: [Option<f32>; 3],
    /// Updates the rotation in degrees.
    pub rotation: [Option<f32>; 3],
    /// Updates the non-uniform scale.
    pub scale3: [Option<f32>; 3],
}

/// Register source for an RGP object.
pub enum RgpRegisterSource {
    /// Path-based object registration.
    Path {
        /// Asset path known to Ratty.
        path: String,
    },
    /// Payload-based object registration.
    Payload {
        /// Optional payload name.
        name: Option<String>,
        /// Whether more chunks follow.
        more: bool,
        /// Decoded payload bytes.
        data: Vec<u8>,
    },
}

/// Consumes an RGP envelope (APC or OSC) and parses the inner body.
///
/// Both envelopes carry the same body grammar (`<verb>;<key=value>;...`)
/// so they share the parser below.
pub fn consume_sequence(sequence: &[u8]) -> Option<RgpOperation> {
    let (prefix_len, terminator_len) = if sequence.starts_with(RGP_APC_START) {
        // APC body terminates with ST or C1 ST. BEL is not a legal APC
        // terminator, so we deliberately don't accept it on the APC path.
        let trailing = if sequence.ends_with(&[C1_ST]) {
            1
        } else if sequence.ends_with(ST) {
            2
        } else {
            return None;
        };
        (RGP_APC_START.len(), trailing)
    } else if sequence.starts_with(RGP_OSC_START) {
        // OSC body can terminate with BEL, ST, or C1 ST. xterm's
        // `ctlseqs` calls out BEL as the historic terminator; modern
        // emitters tend to use ST. Accept all three.
        let trailing = if sequence.ends_with(&[BEL]) || sequence.ends_with(&[C1_ST]) {
            1
        } else if sequence.ends_with(ST) {
            2
        } else {
            return None;
        };
        (RGP_OSC_START.len(), trailing)
    } else {
        return None;
    };

    let content_end = sequence.len() - terminator_len;
    let content = std::str::from_utf8(&sequence[prefix_len..content_end]).ok()?;
    let mut parts = content.split(';');
    let verb = parts.next()?;
    let mut id = None;
    let mut format = None;
    let mut path = None;
    let mut source = None;
    let mut more = None;
    let mut name = None;
    let mut row = None;
    let mut col = None;
    let mut width = None;
    let mut height = None;
    let mut animate = None;
    let mut scale = None;
    let mut depth = None;
    let mut color = None;
    let mut brightness = None;
    let mut px = None;
    let mut py = None;
    let mut pz = None;
    let mut rx = None;
    let mut ry = None;
    let mut rz = None;
    let mut sx = None;
    let mut sy = None;
    let mut sz = None;
    let mut payload = None;
    for part in parts.filter(|part| !part.is_empty()) {
        let Some((key, value)) = part.split_once('=') else {
            if verb == "r" && source.as_deref() == Some("payload") {
                payload = Some(part.to_string());
                break;
            }
            continue;
        };
        match key {
            "id" => id = value.parse().ok(),
            "fmt" => format = Some(value.to_string()),
            "path" => path = Some(value.to_string()),
            "source" => source = Some(value.to_string()),
            "more" => more = parse_bool(value),
            "name" => name = Some(value.to_string()),
            "row" => row = value.parse().ok(),
            "col" => col = value.parse().ok(),
            "w" => width = value.parse().ok(),
            "h" => height = value.parse().ok(),
            "animate" => animate = parse_bool(value),
            "scale" => scale = value.parse().ok(),
            "depth" => depth = value.parse().ok(),
            "color" | "tint" => color = parse_color(value),
            "brightness" => brightness = value.parse().ok(),
            "px" => px = value.parse().ok(),
            "py" => py = value.parse().ok(),
            "pz" => pz = value.parse().ok(),
            "rx" => rx = value.parse().ok(),
            "ry" => ry = value.parse().ok(),
            "rz" => rz = value.parse().ok(),
            "sx" => sx = value.parse().ok(),
            "sy" => sy = value.parse().ok(),
            "sz" => sz = value.parse().ok(),
            _ if verb == "r" && source.as_deref() == Some("payload") => {
                payload = Some(part.to_string());
                break;
            }
            _ => {}
        }
    }

    match verb {
        "s" => Some(RgpOperation::SupportQuery),
        "r" => Some(RgpOperation::Register {
            object_id: id?,
            format: format?,
            source: if let Some(path) = path {
                RgpRegisterSource::Path { path }
            } else {
                if source.as_deref() != Some("payload") {
                    return None;
                }
                let data = base64::engine::general_purpose::STANDARD
                    .decode(payload.unwrap_or_default())
                    .ok()?;
                RgpRegisterSource::Payload {
                    name,
                    more: more.unwrap_or(false),
                    data,
                }
            },
        }),
        "p" => Some(RgpOperation::Place {
            object_id: id?,
            anchor: RgpAnchor {
                row: row?,
                col: col?,
                columns: width?,
                rows: height?,
                style: RgpPlacementStyle {
                    animate: animate.unwrap_or(false),
                    scale: scale.unwrap_or(1.0),
                    depth: depth.unwrap_or(0.0),
                    color,
                    brightness: brightness.unwrap_or(1.0),
                    offset: [px.unwrap_or(0.0), py.unwrap_or(0.0), pz.unwrap_or(0.0)],
                    rotation: [rx.unwrap_or(0.0), ry.unwrap_or(0.0), rz.unwrap_or(0.0)],
                    scale3: [sx.unwrap_or(1.0), sy.unwrap_or(1.0), sz.unwrap_or(1.0)],
                },
            },
        }),
        "u" => Some(RgpOperation::Update {
            object_id: id?,
            update: RgpPlacementUpdate {
                animate,
                scale,
                depth,
                color,
                brightness,
                offset: [px, py, pz],
                rotation: [rx, ry, rz],
                scale3: [sx, sy, sz],
            },
        }),
        "d" => Some(RgpOperation::Delete { object_id: id }),
        _ => Some(RgpOperation::Ignored),
    }
}

/// RGP anchor placement.
#[derive(Clone, Copy)]
pub struct RgpAnchor {
    /// Anchor row.
    pub row: u16,
    /// Anchor column.
    pub col: u16,
    /// Object width in cells.
    pub columns: u32,
    /// Object height in cells.
    pub rows: u32,
    /// Placement style.
    pub style: RgpPlacementStyle,
}

/// Parsed RGP operation.
pub enum RgpOperation {
    /// Support query.
    SupportQuery,
    /// Object registration.
    Register {
        /// Object identifier.
        object_id: u32,
        /// Declared object format.
        format: String,
        /// Register source.
        source: RgpRegisterSource,
    },
    /// Object placement.
    Place {
        /// Object identifier.
        object_id: u32,
        /// Placement anchor.
        anchor: RgpAnchor,
    },
    /// Object update.
    Update {
        /// Object identifier.
        object_id: u32,
        /// Partial style/transform update.
        update: RgpPlacementUpdate,
    },
    /// Object deletion.
    Delete {
        /// Optional object identifier.
        object_id: Option<u32>,
    },
    /// Ignored operation.
    Ignored,
}

/// Returns the RGP support reply sequence.
pub fn support_reply() -> Vec<u8> {
    b"\x1b_ratty;g;s;v=1;fmt=obj|glb;path=1;payload=1;chunk=1;anim=1;depth=1;color=1;brightness=1;transform=1;update=1\x1b\\".to_vec()
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

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

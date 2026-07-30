//! Ratty Graphics Protocol parsing.

use base64::Engine as _;
use bevy::prelude::*;

use crate::scene::TerminalPresentationMode;

/// Ratty Graphics Protocol APC prefix.
pub const RGP_APC_START: &[u8] = b"\x1b_ratty;g;";
const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;

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

/// Partial camera settings parsed from a `c` command.
///
/// `None` fields leave the slot's stored value untouched. Every angle on the
/// wire is degrees; `fov` is converted to radians during parsing, so nothing
/// downstream ever handles a degree value.
#[derive(Clone, Copy, Default)]
pub struct RgpCameraSettings {
    /// Camera type: flat, orthographic, perspective, or Mobius.
    pub camera_type: Option<TerminalPresentationMode>,
    /// Orthographic projection scale.
    pub scale: Option<f32>,
    /// Vertical perspective FOV in radians. The wire value is degrees and is
    /// converted in [`consume_sequence`]; never assign a degree value here.
    pub fov: Option<f32>,
    /// Translation offset relative to the terminal plane.
    pub offset: [Option<f32>; 3],
    /// Rotation in degrees, ordered as pitch, yaw, and roll.
    pub rotation: [Option<f32>; 3],
}

/// Partial update for an RGP object placement.
#[derive(Clone, Copy)]
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

/// Registration-time object loading options.
#[derive(Clone, Copy)]
pub struct RgpRegisterOptions {
    /// Normalizes OBJ meshes into a centered unit-size coordinate space.
    pub normalize: bool,
}

impl Default for RgpRegisterOptions {
    fn default() -> Self {
        Self { normalize: true }
    }
}

/// Consumes an RGP APC sequence.
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
    let mut ctype = None;
    let mut source = None;
    let mut more = None;
    let mut name = None;
    let mut set = None;
    let mut invalid_camera_field = false;
    let mut row = None;
    let mut col = None;
    let mut width = None;
    let mut height = None;
    let mut animate = None;
    let mut scale = None;
    let mut fov = None;
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
    let mut normalize = None;
    let mut payload = None;
    for part in parts.filter(|part| !part.is_empty()) {
        let Some((key, value)) = part.split_once('=') else {
            if verb == "r" && source.as_deref() == Some("payload") {
                payload = Some(part.to_string());
                break;
            }
            // A value-less token in a camera command is a truncated or broken
            // emitter; reject the whole command instead of partially applying.
            invalid_camera_field |= verb == "c";
            continue;
        };
        match key {
            "id" => {
                id = value.parse().ok();
                invalid_camera_field |= verb == "c" && id.is_none();
            }
            "fmt" => format = Some(value.to_string()),
            "path" => path = Some(value.to_string()),
            "type" => ctype = Some(value.to_string()),
            "source" => source = Some(value.to_string()),
            "more" => more = parse_bool(value),
            "name" => name = Some(value.to_string()),
            "set" => {
                set = parse_bool(value);
                invalid_camera_field |= verb == "c" && set.is_none();
            }
            "row" => row = value.parse().ok(),
            "col" => col = value.parse().ok(),
            "w" => width = value.parse().ok(),
            "h" => height = value.parse().ok(),
            "animate" => animate = parse_bool(value),
            "scale" => {
                scale = parse_finite_f32(value);
                invalid_camera_field |= verb == "c" && scale.is_none();
            }
            "fov" => {
                // Degrees on the wire; radians everywhere past this point.
                fov = parse_finite_f32(value).map(f32::to_radians);
                invalid_camera_field |= verb == "c" && fov.is_none();
            }
            "depth" => depth = value.parse().ok(),
            "color" | "tint" => color = parse_color(value),
            "brightness" => brightness = value.parse().ok(),
            "px" => {
                px = parse_finite_f32(value);
                invalid_camera_field |= verb == "c" && px.is_none();
            }
            "py" => {
                py = parse_finite_f32(value);
                invalid_camera_field |= verb == "c" && py.is_none();
            }
            "pz" => {
                pz = parse_finite_f32(value);
                invalid_camera_field |= verb == "c" && pz.is_none();
            }
            "rx" => {
                rx = parse_finite_f32(value);
                invalid_camera_field |= verb == "c" && rx.is_none();
            }
            "ry" => {
                ry = parse_finite_f32(value);
                invalid_camera_field |= verb == "c" && ry.is_none();
            }
            "rz" => {
                rz = parse_finite_f32(value);
                invalid_camera_field |= verb == "c" && rz.is_none();
            }
            "sx" => sx = value.parse().ok(),
            "sy" => sy = value.parse().ok(),
            "sz" => sz = value.parse().ok(),
            "normalize" => normalize = parse_bool(value),
            _ if verb == "r" && source.as_deref() == Some("payload") => {
                payload = Some(part.to_string());
                break;
            }
            _ => {}
        }
    }

    match verb {
        "s" => Some(RgpOperation::SupportQuery),
        "c" => {
            let Some(camera_slot) = id.filter(|slot| *slot < 10) else {
                return Some(RgpOperation::Ignored);
            };
            let camera_type = match ctype.as_deref() {
                Some("Flat") => Some(TerminalPresentationMode::Flat2d),
                Some("Ortho") => Some(TerminalPresentationMode::Plane3d),
                Some("Persp") => Some(TerminalPresentationMode::Perspective3d),
                Some("Mobius") => Some(TerminalPresentationMode::Mobius3d),
                Some(_) => return Some(RgpOperation::Ignored),
                None => None,
            };
            if invalid_camera_field {
                return Some(RgpOperation::Ignored);
            }
            Some(RgpOperation::Camera {
                camera_slot,
                switch_immediately: set.unwrap_or(false),
                settings: RgpCameraSettings {
                    camera_type,
                    scale,
                    fov,
                    offset: [px, py, pz],
                    rotation: [rx, ry, rz],
                },
            })
        }
        "r" => Some(RgpOperation::Register {
            object_id: id?,
            format: format?,
            options: RgpRegisterOptions {
                normalize: normalize.unwrap_or(true),
            },
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
        /// Registration-time object loading options.
        options: RgpRegisterOptions,
        /// Register source.
        source: RgpRegisterSource,
    },
    /// Camera manipulation.
    Camera {
        /// Camera preset slot, `0` through `9`.
        camera_slot: u32,
        /// Activates the slot after applying the update (`set=1`, default `0`).
        switch_immediately: bool,
        /// The partial settings: mode, scale, FOV, rotation, and offset.
        settings: RgpCameraSettings,
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
    b"\x1b_ratty;g;s;v=1;fmt=obj|glb|stl;path=1;payload=1;chunk=1;anim=1;depth=1;color=1;brightness=1;transform=1;update=1;normalize=1;camera=1\x1b\\".to_vec()
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

fn parse_finite_f32(value: &str) -> Option<f32> {
    value.parse::<f32>().ok().filter(|value| value.is_finite())
}

#[cfg(test)]
mod camera_tests {
    use super::*;

    fn parse(fields: &str) -> RgpOperation {
        let sequence = format!("\x1b_ratty;g;c;{fields}\x1b\\");
        consume_sequence(sequence.as_bytes()).expect("RGP sequence")
    }

    #[test]
    fn parses_complete_camera_update() {
        let operation =
            parse("id=9;set=1;type=Persp;scale=1.2;fov=90;px=1;py=2;pz=3;rx=10;ry=20;rz=30");
        let RgpOperation::Camera {
            camera_slot,
            switch_immediately,
            settings,
        } = operation
        else {
            panic!("expected camera update");
        };
        assert_eq!(camera_slot, 9);
        assert!(switch_immediately);
        assert_eq!(
            settings.camera_type,
            Some(TerminalPresentationMode::Perspective3d)
        );
        assert_eq!(settings.scale, Some(1.2));
        assert_eq!(settings.fov, Some(90.0_f32.to_radians()));
        assert_eq!(settings.offset, [Some(1.0), Some(2.0), Some(3.0)]);
        assert_eq!(settings.rotation, [Some(10.0), Some(20.0), Some(30.0)]);
    }

    #[test]
    fn preserves_partial_camera_updates() {
        let RgpOperation::Camera { settings, .. } = parse("id=2;py=-4") else {
            panic!("expected camera update");
        };
        assert_eq!(settings.camera_type, None);
        assert_eq!(settings.scale, None);
        assert_eq!(settings.fov, None);
        assert_eq!(settings.offset, [None, Some(-4.0), None]);
        assert_eq!(settings.rotation, [None, None, None]);
    }

    #[test]
    fn rejects_invalid_slot_mode_boolean_and_numbers() {
        for fields in [
            "id=10",
            "id=bad",
            "id=0;type=FishEye",
            "id=0;set=2",
            "id=0;scale=nope",
            "id=0;fov=nope",
            "id=0;fov=NaN",
            "id=0;fov",
            "id=0;px=NaN",
            "id=0;ry=inf",
            "id=0;px",
            "id=0;px;py=2",
        ] {
            assert!(matches!(parse(fields), RgpOperation::Ignored), "{fields}");
        }
    }

    #[test]
    fn accepts_all_camera_modes() {
        for (name, expected) in [
            ("Flat", TerminalPresentationMode::Flat2d),
            ("Ortho", TerminalPresentationMode::Plane3d),
            ("Persp", TerminalPresentationMode::Perspective3d),
            ("Mobius", TerminalPresentationMode::Mobius3d),
        ] {
            let RgpOperation::Camera { settings, .. } = parse(&format!("id=0;type={name}")) else {
                panic!("expected camera update");
            };
            assert_eq!(settings.camera_type, Some(expected));
        }
    }
}

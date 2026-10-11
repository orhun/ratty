//! MCP stdio facade for a live Ratty terminal.

use ratty::control::{
    ControlClient, ControlCommand, ControlSurfaceKind, ControlViewMode, CursorUpdate,
    PressKeysRequest, ScreenSnapshot, ShapeUpdate, SnapshotRect, SnapshotRequest, TerminalUpdate,
    ViewUpdate, WindowUpdate, WriteTextRequest,
};
use ratty::keyboard::NormalizedKey;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use std::time::Instant;

#[derive(Clone, Debug, Default)]
struct RattyMcp;

fn call_value(command: ControlCommand) -> Result<serde_json::Value, String> {
    let command_name = match &command {
        ControlCommand::GetState {} => "terminal_state",
        ControlCommand::Snapshot(_) => "observe",
        ControlCommand::WriteText(_) => "type_text",
        ControlCommand::PressKeys(_) => "press",
        ControlCommand::Resize(_) => "resize",
        ControlCommand::Close {} => "close",
        ControlCommand::SetView(_) => "set_view",
        ControlCommand::SetCursor(_) => "set_cursor",
        ControlCommand::SetWindow(_) => "set_window",
        ControlCommand::SetTerminal(_) => "set_terminal",
        ControlCommand::SetShape(_) => "set_shape",
    };
    let started = Instant::now();
    eprintln!("ratty-mcp: {command_name} request started");
    let response = match ControlClient::request(command) {
        Ok(response) => response,
        Err(error) => {
            eprintln!(
                "ratty-mcp: {command_name} request failed after {} ms: {error:#}",
                started.elapsed().as_millis()
            );
            return Err(error.to_string());
        }
    };
    eprintln!(
        "ratty-mcp: {command_name} request completed in {} ms",
        started.elapsed().as_millis()
    );
    Ok(response.result.unwrap_or(serde_json::Value::Null))
}

fn call(command: ControlCommand) -> Result<String, String> {
    serde_json::to_string_pretty(&call_value(command)?).map_err(|error| error.to_string())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ObserveRect {
    /// First absolute terminal row.
    row: u16,
    /// First absolute terminal column.
    column: u16,
    /// Number of terminal rows.
    #[schemars(range(min = 1, max = 1000))]
    rows: u16,
    /// Number of terminal columns.
    #[schemars(range(min = 1, max = 1000))]
    columns: u16,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ObserveParams {
    /// Restrict returned content while preserving absolute cell coordinates.
    rect: Option<ObserveRect>,
    /// Include the deduplicated style table. Run style indices are always present.
    #[serde(default)]
    include_styles: bool,
    /// Include a convenience plain-text value derived from positioned runs.
    #[serde(default = "default_true")]
    include_plain: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TypeTextParams {
    /// Exact UTF-8 text to write. Enter is never appended.
    #[schemars(length(max = 65536))]
    text: String,
    /// Add bracketed-paste delimiters when the application enabled that mode.
    #[serde(default = "default_true")]
    paste: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PressParams {
    /// Ordered normalized key presses.
    #[schemars(length(max = 256))]
    keys: Vec<NormalizedKey>,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ViewMode {
    Flat,
    Ortho,
    Perspective,
    Mobius,
}

impl From<ViewMode> for ControlViewMode {
    fn from(value: ViewMode) -> Self {
        match value {
            ViewMode::Flat => Self::Flat,
            ViewMode::Ortho => Self::Ortho,
            ViewMode::Perspective => Self::Perspective,
            ViewMode::Mobius => Self::Mobius,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetViewParams {
    /// Persistent camera preset slot to edit, from 0 to 9. Defaults to the active slot.
    #[schemars(range(min = 0, max = 9))]
    slot: Option<u8>,
    /// Activate the edited camera slot. Defaults to true when slot is supplied.
    activate: Option<bool>,
    /// Surface/camera mode. Mobius wraps the terminal into a live 3D strip.
    mode: Option<ViewMode>,
    /// Animated surface deformation strength from 0 (still) to 1 (maximum).
    #[schemars(range(min = 0.0, max = 1.0))]
    warp: Option<f32>,
    /// Camera yaw in degrees.
    yaw_degrees: Option<f32>,
    /// Camera pitch in degrees.
    pitch_degrees: Option<f32>,
    /// Camera roll in degrees.
    roll_degrees: Option<f32>,
    /// Orthographic zoom scale from 0.01 to 20.
    #[schemars(range(min = 0.01, max = 20.0))]
    zoom: Option<f32>,
    /// Perspective vertical field of view in degrees, from 3 to 177.
    #[schemars(range(min = 3.0, max = 177.0))]
    fov_degrees: Option<f32>,
    /// Horizontal camera translation in terminal world units.
    x: Option<f32>,
    /// Vertical camera translation in terminal world units.
    y: Option<f32>,
    /// Depth camera translation in terminal world units.
    z: Option<f32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetCursorParams {
    /// Show or hide the 3D rat cursor.
    visible: Option<bool>,
    /// Rat size multiplier, from 0.001 to 100.
    #[schemars(range(min = 0.001, max = 100.0))]
    scale: Option<f32>,
    /// Horizontal offset from the text cursor, in cells.
    x_offset: Option<f32>,
    /// Distance from the warped terminal surface in 3D modes.
    depth: Option<f32>,
    /// Cursor material brightness multiplier, from 0 to 20.
    #[schemars(range(min = 0.0, max = 20.0))]
    brightness: Option<f32>,
    /// Rat rotation speed in radians per second. Negative values reverse it.
    spin_speed: Option<f32>,
    /// Rat jump/bob angular speed. Negative values reverse phase travel.
    jump_speed: Option<f32>,
    /// Rat jump height as a fraction of cell height, from -10 to 10.
    #[schemars(range(min = -10.0, max = 10.0))]
    jump_height: Option<f32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetWindowParams {
    /// Window width in logical pixels, from 64 to 16384.
    #[schemars(range(min = 64, max = 16384))]
    width: Option<u32>,
    /// Window height in logical pixels, from 64 to 16384.
    #[schemars(range(min = 64, max = 16384))]
    height: Option<u32>,
    /// Window left coordinate in physical desktop pixels.
    x: Option<i32>,
    /// Window top coordinate in physical desktop pixels.
    y: Option<i32>,
    /// Native window title.
    #[schemars(length(max = 1024))]
    title: Option<String>,
    /// Background as three integer RGB channels, each 0..255.
    background_rgb: Option<[u8; 3]>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetTerminalParams {
    /// Exact terminal grid width, from 1 to 1000 cells.
    #[schemars(range(min = 1, max = 1000))]
    columns: Option<u16>,
    /// Exact terminal grid height, from 1 to 1000 cells.
    #[schemars(range(min = 1, max = 1000))]
    rows: Option<u16>,
    /// Font size in points, from 4 to 256.
    #[schemars(range(min = 4, max = 256))]
    font_size: Option<i32>,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SurfaceKind {
    Automatic,
    Plane,
    Mobius,
    PaperPlane,
    Gomboc,
    Cylinder,
    Sphere,
    Wave,
    Custom,
}

impl From<SurfaceKind> for ControlSurfaceKind {
    fn from(value: SurfaceKind) -> Self {
        match value {
            SurfaceKind::Automatic => Self::Automatic,
            SurfaceKind::Plane => Self::Plane,
            SurfaceKind::Mobius => Self::Mobius,
            SurfaceKind::PaperPlane => Self::PaperPlane,
            SurfaceKind::Gomboc => Self::Gomboc,
            SurfaceKind::Cylinder => Self::Cylinder,
            SurfaceKind::Sphere => Self::Sphere,
            SurfaceKind::Wave => Self::Wave,
            SurfaceKind::Custom => Self::Custom,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetShapeParams {
    /// Surface family: automatic, plane, mobius, paper_plane, gomboc, cylinder, sphere, wave, or custom.
    kind: SurfaceKind,
    /// Wave amplitude or custom surface depth multiplier, from 0 to 10.
    #[schemars(range(min = 0.0, max = 10.0))]
    amplitude: Option<f32>,
    /// Spatial repetition count for animated wave or custom surfaces, from 0.01 to 32.
    #[schemars(range(min = 0.01, max = 32.0))]
    frequency: Option<f32>,
    /// Animation speed for wave or custom surfaces; custom surfaces are static by default and zero disables motion, from -16 to 16.
    #[schemars(range(min = -16.0, max = 16.0))]
    twist: Option<f32>,
    /// Custom lattice width, from 2 to 8. A lattice is one continuous rectangular sheet.
    #[schemars(range(min = 2, max = 8))]
    control_columns: Option<u8>,
    /// Custom lattice height, from 2 to 8.
    #[schemars(range(min = 2, max = 8))]
    control_rows: Option<u8>,
    /// Row-major XYZ control points for one continuous sheet. X/Y are normalized to the terminal; Z is in terminal depth units (hundreds for a clearly visible fold).
    #[schemars(length(min = 4, max = 64))]
    control_points: Option<Vec<[f32; 3]>>,
    /// Custom mesh vertices as [x, y, z, u, v]; X/Y are normalized to the terminal, Z is in terminal depth units (hundreds for a clearly visible fold), and UV values are 0..1. Use a mesh for separately angled faces or closed shapes. Duplicate vertices at texture seams.
    #[schemars(length(min = 3, max = 256))]
    custom_vertices: Option<Vec<[f32; 5]>>,
    /// Triangle vertex indices into custom_vertices. Each entry is one face; use these with custom_vertices for folds, multiple faces, and closed shapes that a single lattice cannot represent.
    #[schemars(length(min = 1, max = 512))]
    custom_triangles: Option<Vec<[u32; 3]>>,
}

#[tool_router]
impl RattyMcp {
    /// Inspect terminal dimensions, connection status, and current camera/warp values.
    #[tool(
        description = "Inspect the live Ratty terminal's dimensions, PTY status, and camera/warp state"
    )]
    fn terminal_state(&self) -> Result<String, String> {
        call(ControlCommand::GetState {})
    }

    /// Observe the exact positioned runs currently visible in Ratty.
    #[tool(
        description = "Observe Ratty's visible terminal grid as Unicode-safe positioned runs with cursor and input modes"
    )]
    fn observe(&self, Parameters(params): Parameters<ObserveParams>) -> Result<String, String> {
        if params
            .rect
            .as_ref()
            .is_some_and(|rect| rect.rows == 0 || rect.columns == 0)
        {
            return Err("observation rectangle dimensions must be non-zero".into());
        }
        let rect = params.rect.map(|rect| SnapshotRect {
            row: rect.row,
            column: rect.column,
            rows: rect.rows,
            columns: rect.columns,
        });
        let value = call_value(ControlCommand::Snapshot(SnapshotRequest {
            rect,
            include_styles: true,
            include_plain: false,
        }))?;
        let snapshot: ScreenSnapshot =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        let snapshot = snapshot.into_view(None, params.include_styles, params.include_plain);
        serde_json::to_string_pretty(&snapshot).map_err(|error| error.to_string())
    }

    /// Write exact UTF-8 text without appending Enter.
    #[tool(
        description = "Write exact UTF-8 text into Ratty; optionally use bracketed paste when the application enabled it; never appends Enter"
    )]
    fn type_text(&self, Parameters(params): Parameters<TypeTextParams>) -> Result<String, String> {
        call(ControlCommand::WriteText(WriteTextRequest {
            text: params.text,
            paste: params.paste,
        }))
    }

    /// Send normalized special or printable keys through Ratty's keyboard encoder.
    #[tool(
        description = "Press normalized terminal keys using Ratty's active application-cursor, Kitty keyboard, and modifyOtherKeys modes"
    )]
    fn press(&self, Parameters(params): Parameters<PressParams>) -> Result<String, String> {
        call(ControlCommand::PressKeys(PressKeysRequest {
            keys: params.keys,
        }))
    }

    /// Change any subset of the camera and surface warp parameters in real time.
    #[tool(
        description = "Warp Ratty or change its flat, orthographic, perspective, or Mobius camera in real time"
    )]
    fn set_view(&self, Parameters(params): Parameters<SetViewParams>) -> Result<String, String> {
        call(ControlCommand::SetView(ViewUpdate {
            slot: params.slot,
            activate: params.activate,
            mode: params.mode.map(Into::into),
            warp: params.warp,
            yaw_degrees: params.yaw_degrees,
            pitch_degrees: params.pitch_degrees,
            roll_degrees: params.roll_degrees,
            zoom: params.zoom,
            fov_degrees: params.fov_degrees,
            x: params.x,
            y: params.y,
            z: params.z,
        }))
    }

    /// Reconfigure the rat cursor model and its motion live.
    #[tool(
        description = "Configure the live rat cursor: visibility, size, offsets, depth, brightness, spin, and jump motion"
    )]
    fn set_cursor(
        &self,
        Parameters(params): Parameters<SetCursorParams>,
    ) -> Result<String, String> {
        call(ControlCommand::SetCursor(CursorUpdate {
            visible: params.visible,
            scale: params.scale,
            x_offset: params.x_offset,
            depth: params.depth,
            brightness: params.brightness,
            spin_speed: params.spin_speed,
            jump_speed: params.jump_speed,
            jump_height: params.jump_height,
        }))
    }

    /// Reconfigure the native window live.
    #[tool(
        description = "Configure the live Ratty window's pixel dimensions, desktop coordinates, title, and RGB background"
    )]
    fn set_window(
        &self,
        Parameters(params): Parameters<SetWindowParams>,
    ) -> Result<String, String> {
        call(ControlCommand::SetWindow(WindowUpdate {
            width: params.width,
            height: params.height,
            x: params.x,
            y: params.y,
            title: params.title,
            background_rgb: params.background_rgb,
        }))
    }

    /// Reconfigure the terminal grid and typography live.
    #[tool(
        description = "Set the live terminal's exact row/column dimensions and font size; the PTY is resized too"
    )]
    fn set_terminal(
        &self,
        Parameters(params): Parameters<SetTerminalParams>,
    ) -> Result<String, String> {
        call(ControlCommand::SetTerminal(TerminalUpdate {
            columns: params.columns,
            rows: params.rows,
            font_size: params.font_size,
        }))
    }

    /// Define a custom terminal surface, or restore the normal view surface. A control lattice is one continuous rectangular sheet; use a triangle mesh for multiple angled faces or closed shapes. For kind=custom, provide complete geometry. X/Y are normalized and Z uses terminal depth units, so values like 0.5 are nearly flat; use a few hundred units for a visible fold. If mesh fields are missing from the available tool schema, the MCP server needs to be reloaded.
    #[tool(
        description = "Define a custom terminal surface or restore the normal plane/Mobius surface. A control lattice makes one continuous rectangular sheet; use a UV-mapped triangle mesh for multiple angled faces or closed shapes. For kind=custom, pass all geometry in this call: control_columns, control_rows, and row-major control_points, or custom_vertices and custom_triangles. X/Y are normalized; Z uses terminal depth units, so values like 0.5 are nearly flat and a few hundred units make a visible fold. Mesh vertices use [x, y, z, u, v]; each triangle has three vertex indices. If mesh fields are absent from this tool schema, reload the MCP server."
    )]
    fn set_shape(&self, Parameters(params): Parameters<SetShapeParams>) -> Result<String, String> {
        call(ControlCommand::SetShape(ShapeUpdate {
            kind: params.kind.into(),
            amplitude: params.amplitude,
            frequency: params.frequency,
            twist: params.twist,
            control_columns: params.control_columns,
            control_rows: params.control_rows,
            control_points: params.control_points,
            custom_vertices: params.custom_vertices,
            custom_triangles: params.custom_triangles,
        }))
    }
}

#[tool_handler]
impl ServerHandler for RattyMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ratty", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Observe and control the live Ratty terminal. Use observe for exact positioned \
                 terminal content, type_text for UTF-8 text without an implicit Enter, and press \
                 for structured keys. Input reaches the active PTY and may execute commands, so \
                 send it only when the user intends terminal interaction. Visual changes are \
                 applied on Ratty's next frame. For custom shapes, a control lattice is one \
                 continuous rectangular sheet; use custom_vertices/custom_triangles for multiple \
                 angled faces or closed shapes. Send complete geometry with set_shape and keep \
                 twist at zero for a static shape. Coordinates use normalized X/Y and terminal \
                 depth units for Z; Z values below a few units are nearly flat, while a few \
                 hundred units make a clearly visible fold. Do not claim a shape was applied unless \
                 set_shape succeeds and its response reports the geometry type and counts. If mesh \
                 fields are missing from the tool schema, the MCP server must be reloaded before \
                 mesh geometry can be sent.",
            )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    eprintln!(
        "ratty-mcp {} starting (MCP over stdin/stdout)",
        env!("CARGO_PKG_VERSION")
    );
    let server = RattyMcp.serve(rmcp::transport::stdio()).await?;
    eprintln!("ratty-mcp: MCP client initialized; waiting for tool requests");
    server.waiting().await?;
    eprintln!("ratty-mcp stopped");
    Ok(())
}

//! MCP stdio facade for a live Ratty terminal.

use ratty::control::{
    ControlClient, ControlCommand, ControlSurfaceKind, ControlViewMode, CursorUpdate, ShapeUpdate,
    TerminalUpdate, ViewUpdate, WindowUpdate,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;

#[derive(Clone, Debug)]
struct RattyMcp;

fn call(command: ControlCommand) -> Result<String, String> {
    let response = ControlClient::request(command).map_err(|error| error.to_string())?;
    serde_json::to_string_pretty(&response.data.unwrap_or(serde_json::Value::Null))
        .map_err(|error| error.to_string())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadTerminalParams {
    /// Return only this many rows from the bottom of the visible screen.
    #[schemars(range(min = 1, max = 1000))]
    last_lines: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendInputParams {
    /// Exact UTF-8 text to type into the active terminal application.
    #[schemars(length(max = 65536))]
    text: String,
    /// Press Enter after typing the text.
    #[serde(default)]
    submit: bool,
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
    Custom,
}

impl From<SurfaceKind> for ControlSurfaceKind {
    fn from(value: SurfaceKind) -> Self {
        match value {
            SurfaceKind::Automatic => Self::Automatic,
            SurfaceKind::Custom => Self::Custom,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetShapeParams {
    /// Use custom for a control lattice or automatic to restore the normal view surface.
    kind: SurfaceKind,
    /// Depth multiplier for custom control points, from 0 to 10.
    #[schemars(range(min = 0.0, max = 10.0))]
    amplitude: Option<f32>,
    /// Custom lattice width, from 2 to 8.
    #[schemars(range(min = 2, max = 8))]
    control_columns: Option<u8>,
    /// Custom lattice height, from 2 to 8.
    #[schemars(range(min = 2, max = 8))]
    control_rows: Option<u8>,
    /// Row-major XYZ control points. X/Y use normalized sheet coordinates and Z is depth.
    #[schemars(length(min = 4, max = 64))]
    control_points: Option<Vec<[f32; 3]>>,
}

#[tool_router]
impl RattyMcp {
    /// Inspect terminal dimensions, connection status, and current camera/warp values.
    #[tool(
        description = "Inspect the live Ratty terminal's dimensions, PTY status, and camera/warp state"
    )]
    fn terminal_state(&self) -> Result<String, String> {
        call(ControlCommand::GetState)
    }

    /// Read text currently visible in the terminal window (not full scrollback).
    #[tool(description = "Read the text currently visible in the live Ratty terminal window")]
    fn read_terminal(
        &self,
        Parameters(params): Parameters<ReadTerminalParams>,
    ) -> Result<String, String> {
        call(ControlCommand::ReadScreen {
            last_lines: params.last_lines,
        })
    }

    /// Type into the active terminal application, optionally followed by Enter.
    #[tool(
        description = "Type exact text into the active Ratty PTY; submit=true also presses Enter and may execute it"
    )]
    fn send_input(
        &self,
        Parameters(params): Parameters<SendInputParams>,
    ) -> Result<String, String> {
        call(ControlCommand::SendInput {
            text: params.text,
            submit: params.submit,
        })
    }

    /// Change any subset of the camera and surface warp parameters in real time.
    #[tool(
        description = "Warp Ratty or change its flat, orthographic, perspective, or Mobius camera in real time"
    )]
    fn set_view(&self, Parameters(params): Parameters<SetViewParams>) -> Result<String, String> {
        call(ControlCommand::SetView {
            update: ViewUpdate {
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
            },
        })
    }

    /// Reconfigure the rat cursor model and its motion live.
    #[tool(
        description = "Configure the live rat cursor: visibility, size, offsets, depth, brightness, spin, and jump motion"
    )]
    fn set_cursor(
        &self,
        Parameters(params): Parameters<SetCursorParams>,
    ) -> Result<String, String> {
        call(ControlCommand::SetCursor {
            update: CursorUpdate {
                visible: params.visible,
                scale: params.scale,
                x_offset: params.x_offset,
                depth: params.depth,
                brightness: params.brightness,
                spin_speed: params.spin_speed,
                jump_speed: params.jump_speed,
                jump_height: params.jump_height,
            },
        })
    }

    /// Reconfigure the native window live.
    #[tool(
        description = "Configure the live Ratty window's pixel dimensions, desktop coordinates, title, and RGB background"
    )]
    fn set_window(
        &self,
        Parameters(params): Parameters<SetWindowParams>,
    ) -> Result<String, String> {
        call(ControlCommand::SetWindow {
            update: WindowUpdate {
                width: params.width,
                height: params.height,
                x: params.x,
                y: params.y,
                title: params.title,
                background_rgb: params.background_rgb,
            },
        })
    }

    /// Reconfigure the terminal grid and typography live.
    #[tool(
        description = "Set the live terminal's exact row/column dimensions and font size; the PTY is resized too"
    )]
    fn set_terminal(
        &self,
        Parameters(params): Parameters<SetTerminalParams>,
    ) -> Result<String, String> {
        call(ControlCommand::SetTerminal {
            update: TerminalUpdate {
                columns: params.columns,
                rows: params.rows,
                font_size: params.font_size,
            },
        })
    }

    /// Define an agent-controlled terminal surface or restore the normal view surface.
    #[tool(
        description = "Define the terminal's 3D surface with a custom control-point lattice, or restore the normal plane/Mobius view surface"
    )]
    fn set_shape(&self, Parameters(params): Parameters<SetShapeParams>) -> Result<String, String> {
        call(ControlCommand::SetShape {
            update: ShapeUpdate {
                kind: params.kind.into(),
                amplitude: params.amplitude,
                control_columns: params.control_columns,
                control_rows: params.control_rows,
                control_points: params.control_points,
            },
        })
    }
}

#[tool_handler]
impl ServerHandler for RattyMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ratty", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Control the live Ratty terminal. Inspect state before making visual changes. \
                 send_input types into the active PTY and can execute commands when submit=true, \
                 so use it only when the user intends terminal interaction. Visual changes are \
                 applied on Ratty's next frame.",
            )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = RattyMcp.serve(rmcp::transport::stdio()).await?;
    server.waiting().await?;
    Ok(())
}

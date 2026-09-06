//! Authenticated, local-only control bridge used by the `ratty-mcp` server.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, bail};
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use etcetera::{BaseStrategy, choose_base_strategy};
use serde::{Deserialize, Serialize};

use crate::camera::{OptionalVec3, TerminalCameraSlots, TerminalCameraUpdate};
use crate::config::AppConfig;
use crate::runtime::TerminalRuntime;
use crate::scene::{
    TerminalPlane, TerminalPlaneBack, TerminalPlaneWarp, TerminalPresentationMode, TerminalSprite,
    TerminalSurfaceKind, TerminalSurfaceShape, TerminalViewport, sync_terminal_layout,
};
use crate::terminal::{TerminalRedrawState, TerminalSurface};
use crate::vt;

const MAX_REQUEST_BYTES: u64 = 128 * 1024;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const DISCOVERY_ENV: &str = "RATTY_MCP_ENDPOINT_FILE";

/// Camera modes accepted by the local control protocol.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ControlViewMode {
    Flat,
    Ortho,
    Perspective,
    Mobius,
}

impl From<ControlViewMode> for TerminalPresentationMode {
    fn from(value: ControlViewMode) -> Self {
        match value {
            ControlViewMode::Flat => Self::Flat2d,
            ControlViewMode::Ortho => Self::Plane3d,
            ControlViewMode::Perspective => Self::Perspective3d,
            ControlViewMode::Mobius => Self::Mobius3d,
        }
    }
}

impl From<TerminalPresentationMode> for ControlViewMode {
    fn from(value: TerminalPresentationMode) -> Self {
        match value {
            TerminalPresentationMode::Flat2d => Self::Flat,
            TerminalPresentationMode::Plane3d => Self::Ortho,
            TerminalPresentationMode::Perspective3d => Self::Perspective,
            TerminalPresentationMode::Mobius3d => Self::Mobius,
        }
    }
}

/// A partial, frame-safe camera update.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct ViewUpdate {
    pub slot: Option<u8>,
    pub activate: Option<bool>,
    pub mode: Option<ControlViewMode>,
    pub warp: Option<f32>,
    pub yaw_degrees: Option<f32>,
    pub pitch_degrees: Option<f32>,
    pub roll_degrees: Option<f32>,
    pub zoom: Option<f32>,
    pub fov_degrees: Option<f32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub z: Option<f32>,
}

/// Live cursor model and animation settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct CursorUpdate {
    pub visible: Option<bool>,
    pub scale: Option<f32>,
    pub x_offset: Option<f32>,
    pub depth: Option<f32>,
    pub brightness: Option<f32>,
    pub spin_speed: Option<f32>,
    pub jump_speed: Option<f32>,
    pub jump_height: Option<f32>,
}

/// Live native-window settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct WindowUpdate {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub title: Option<String>,
    pub background_rgb: Option<[u8; 3]>,
}

/// Live terminal grid and typography settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct TerminalUpdate {
    pub columns: Option<u16>,
    pub rows: Option<u16>,
    pub font_size: Option<i32>,
}

/// Parametric terminal-surface families accepted by MCP.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ControlSurfaceKind {
    Automatic,
    Custom,
}

impl From<TerminalSurfaceKind> for ControlSurfaceKind {
    fn from(value: TerminalSurfaceKind) -> Self {
        match value {
            TerminalSurfaceKind::ModeDefault => Self::Automatic,
            TerminalSurfaceKind::Custom => Self::Custom,
        }
    }
}

/// A complete surface definition, with a lattice for custom surfaces.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct ShapeUpdate {
    pub kind: ControlSurfaceKind,
    pub amplitude: Option<f32>,
    pub control_columns: Option<u8>,
    pub control_rows: Option<u8>,
    pub control_points: Option<Vec<[f32; 3]>>,
}

/// Commands sent across the private GUI control socket.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ControlCommand {
    GetState,
    ReadScreen { last_lines: Option<u16> },
    SendInput { text: String, submit: bool },
    SetView { update: ViewUpdate },
    SetCursor { update: CursorUpdate },
    SetWindow { update: WindowUpdate },
    SetTerminal { update: TerminalUpdate },
    SetShape { update: ShapeUpdate },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WireRequest {
    token: String,
    #[serde(flatten)]
    command: ControlCommand,
}

/// Successful response from the GUI.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    fn data(value: impl Serialize) -> Self {
        Self {
            ok: true,
            data: serde_json::to_value(value).ok(),
            error: None,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Discovery {
    pid: u32,
    address: String,
    token: String,
}

struct PendingCommand {
    command: ControlCommand,
    reply: SyncSender<ControlResponse>,
}

/// GUI-side endpoint. Its receiver is drained once per Bevy update.
#[derive(Resource)]
pub struct TerminalControl {
    rx: SyncCell<Receiver<PendingCommand>>,
    shutdown: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
    discovery_path: PathBuf,
    token: String,
    address: String,
}

impl TerminalControl {
    /// Starts a loopback listener and publishes its authenticated endpoint.
    pub fn spawn() -> anyhow::Result<Self> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .context("failed to bind Ratty MCP control socket")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?.to_string();
        let token = random_token()?;
        let discovery_path = discovery_path();
        write_discovery(
            &discovery_path,
            &Discovery {
                pid: std::process::id(),
                address: address.clone(),
                token: token.clone(),
            },
        )?;

        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_token = token.clone();
        let listener_thread = thread::Builder::new()
            .name("ratty-mcp-control".into())
            .spawn(move || listener_loop(listener, tx, &thread_token, &thread_shutdown))?;

        eprintln!("ratty MCP control enabled ({discovery_path:?})");
        Ok(Self {
            rx: SyncCell::new(rx),
            shutdown,
            listener_thread: Some(listener_thread),
            discovery_path,
            token,
            address,
        })
    }
}

impl Drop for TerminalControl {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(&self.address);
        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }
        if fs::read(&self.discovery_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Discovery>(&bytes).ok())
            .is_some_and(|record| record.token == self.token)
        {
            let _ = fs::remove_file(&self.discovery_path);
        }
    }
}

fn listener_loop(
    listener: TcpListener,
    tx: mpsc::Sender<PendingCommand>,
    token: &str,
    shutdown: &AtomicBool,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => handle_connection(stream, &tx, token),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(mut stream: TcpStream, tx: &mpsc::Sender<PendingCommand>, token: &str) {
    let _ = stream.set_read_timeout(Some(RESPONSE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RESPONSE_TIMEOUT));
    let parsed = {
        let mut line = String::new();
        let mut reader = BufReader::new((&stream).take(MAX_REQUEST_BYTES));
        reader.read_line(&mut line).and_then(|_| {
            serde_json::from_str::<WireRequest>(&line)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
    };
    let response = match parsed {
        Ok(request) if constant_time_eq(request.token.as_bytes(), token.as_bytes()) => {
            let (reply, rx) = mpsc::sync_channel(1);
            if tx
                .send(PendingCommand {
                    command: request.command,
                    reply,
                })
                .is_err()
            {
                ControlResponse::error("Ratty is shutting down")
            } else {
                rx.recv_timeout(RESPONSE_TIMEOUT)
                    .unwrap_or_else(|_| ControlResponse::error("Ratty did not answer in time"))
            }
        }
        Ok(_) => ControlResponse::error("authentication failed"),
        Err(error) => ControlResponse::error(format!("invalid request: {error}")),
    };
    if serde_json::to_writer(&mut stream, &response).is_ok() {
        let _ = stream.write_all(b"\n");
        let _ = stream.flush();
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn random_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("failed to generate MCP session token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn discovery_path() -> PathBuf {
    std::env::var_os(DISCOVERY_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            choose_base_strategy()
                .ok()
                // Codex intentionally launches MCP children with a restricted
                // environment that may omit XDG_RUNTIME_DIR. The cache path is
                // stable in both the GUI's desktop environment and the MCP
                // child's sanitized environment.
                .map(|strategy| strategy.cache_dir())
                .unwrap_or_else(std::env::temp_dir)
                .join("ratty")
                .join("mcp.json")
        })
}

fn write_discovery(path: &Path, discovery: &Discovery) -> anyhow::Result<()> {
    let parent = path.parent().context("MCP discovery path has no parent")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let bytes = serde_json::to_vec(discovery)?;
    let temporary = parent.join(format!(".mcp-{}.tmp", discovery.token));
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    fs::write(&temporary, bytes)?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to publish {}", path.display()))?;
    Ok(())
}

/// Applies queued MCP requests on Bevy's main world.
pub fn pump_control_commands(
    mut control: ResMut<TerminalControl>,
    mut runtime: ResMut<TerminalRuntime>,
    mut terminal: ResMut<TerminalSurface>,
    slots: Res<TerminalCameraSlots>,
    mut warp: ResMut<TerminalPlaneWarp>,
    mut camera_updates: MessageWriter<TerminalCameraUpdate>,
    mut app_config: ResMut<AppConfig>,
    mut redraw: ResMut<TerminalRedrawState>,
    mut window: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    mut viewport: ResMut<TerminalViewport>,
    mut plane_query: Query<&'static mut Transform, (With<TerminalPlane>, Without<TerminalSprite>)>,
    mut plane_back_query: Query<
        &'static mut Transform,
        (
            With<TerminalPlaneBack>,
            Without<TerminalPlane>,
            Without<TerminalSprite>,
        ),
    >,
    mut clear_color: ResMut<ClearColor>,
) {
    while let Ok(pending) = control.rx.get().try_recv() {
        let response = apply_command(
            pending.command,
            &mut runtime,
            &mut terminal,
            &slots,
            &mut warp,
            &mut camera_updates,
            &mut app_config,
            &mut redraw,
            &mut window,
            &mut viewport,
            &mut plane_query,
            &mut plane_back_query,
            &mut clear_color,
        );
        let _ = pending.reply.send(response);
    }
}

fn apply_command(
    command: ControlCommand,
    runtime: &mut TerminalRuntime,
    terminal: &mut TerminalSurface,
    slots: &TerminalCameraSlots,
    warp: &mut TerminalPlaneWarp,
    camera_updates: &mut MessageWriter<TerminalCameraUpdate>,
    app_config: &mut AppConfig,
    redraw: &mut TerminalRedrawState,
    window_query: &mut Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    viewport: &mut TerminalViewport,
    plane_query: &mut Query<&'static mut Transform, (With<TerminalPlane>, Without<TerminalSprite>)>,
    plane_back_query: &mut Query<
        &'static mut Transform,
        (
            With<TerminalPlaneBack>,
            Without<TerminalPlane>,
            Without<TerminalSprite>,
        ),
    >,
    clear_color: &mut ClearColor,
) -> ControlResponse {
    match command {
        ControlCommand::GetState => {
            let preset = slots.active();
            ControlResponse::data(serde_json::json!({
                "connected": !runtime.pty_disconnected,
                "columns": terminal.cols,
                "rows": terminal.rows,
                "font_size": terminal.font_size(),
                "window": window_query.single().ok().map(|window| serde_json::json!({
                    "width": window.resolution.width(),
                    "height": window.resolution.height(),
                    "title": window.title,
                    "position": format!("{:?}", window.position),
                    "background_rgb": app_config.theme.background,
                })),
                "cursor": {
                    "visible": app_config.cursor.model.visible,
                    "scale": app_config.cursor.model.scale_factor,
                    "x_offset": app_config.cursor.model.x_offset,
                    "depth": app_config.cursor.model.plane_offset,
                    "brightness": app_config.cursor.model.brightness,
                    "spin_speed": app_config.cursor.animation.spin_speed,
                    "jump_speed": app_config.cursor.animation.bob_speed,
                    "jump_height": app_config.cursor.animation.bob_amplitude,
                },
                "camera": {
                    "mode": ControlViewMode::from(preset.mode),
                    "warp": warp.amount,
                    "yaw_degrees": preset.pose.yaw.to_degrees(),
                    "pitch_degrees": preset.pose.pitch.to_degrees(),
                    "roll_degrees": preset.pose.roll.to_degrees(),
                    "zoom": preset.pose.orthographic_scale,
                    "fov_degrees": preset.pose.perspective_fov.to_degrees(),
                },
                "surface": {
                    "kind": ControlSurfaceKind::from(warp.shape.kind),
                    "amplitude": warp.shape.amplitude,
                    "control_columns": warp.shape.control_columns,
                    "control_rows": warp.shape.control_rows,
                    "control_points": warp.shape.control_points.iter().map(|point| point.to_array()).collect::<Vec<_>>(),
                }
            }))
        }
        ControlCommand::ReadScreen { last_lines } => {
            let mut rows = vt::visible_row_texts(&runtime.term);
            if let Some(count) = last_lines {
                let keep_from = rows
                    .len()
                    .saturating_sub(usize::from(count.min(terminal.rows)));
                rows.drain(..keep_from);
            }
            ControlResponse::data(serde_json::json!({
                "columns": terminal.cols,
                "rows": terminal.rows,
                "text": rows.join("\n"),
            }))
        }
        ControlCommand::SendInput { text, submit } => {
            if text.len() > 64 * 1024 {
                return ControlResponse::error("input exceeds 64 KiB");
            }
            runtime.write_input(text.as_bytes());
            if submit {
                runtime.write_input(b"\r");
            }
            ControlResponse::data(
                serde_json::json!({ "bytes_written": text.len(), "submitted": submit }),
            )
        }
        ControlCommand::SetView { update } => {
            let slot = usize::from(update.slot.unwrap_or(slots.active_slot() as u8));
            if slot >= 10 {
                return ControlResponse::error("camera slot must be between 0 and 9");
            }
            let values = [
                update.warp,
                update.yaw_degrees,
                update.pitch_degrees,
                update.roll_degrees,
                update.zoom,
                update.fov_degrees,
                update.x,
                update.y,
                update.z,
            ];
            if values.into_iter().flatten().any(|value| !value.is_finite()) {
                return ControlResponse::error("view values must be finite numbers");
            }
            if let Some(amount) = update.warp {
                if !(0.0..=1.0).contains(&amount) {
                    return ControlResponse::error("warp must be between 0 and 1");
                }
                warp.amount = amount;
            }
            if update
                .zoom
                .is_some_and(|value| !(0.01..=20.0).contains(&value))
            {
                return ControlResponse::error("zoom must be between 0.01 and 20");
            }
            if update
                .fov_degrees
                .is_some_and(|value| !(3.0..=177.0).contains(&value))
            {
                return ControlResponse::error("fov_degrees must be between 3 and 177");
            }
            camera_updates.write(TerminalCameraUpdate {
                slot,
                activate: update.activate.unwrap_or(update.slot.is_some()),
                mode: update.mode.map(Into::into),
                scale: update.zoom,
                fov: update.fov_degrees.map(f32::to_radians),
                translation: OptionalVec3 {
                    x: update.x,
                    y: update.y,
                    z: update.z,
                },
                rotation_degrees: OptionalVec3 {
                    x: update.pitch_degrees,
                    y: update.yaw_degrees,
                    z: update.roll_degrees,
                },
            });
            ControlResponse::data(serde_json::json!({ "accepted": true }))
        }
        ControlCommand::SetCursor { update } => {
            let numeric = [
                update.scale,
                update.x_offset,
                update.depth,
                update.brightness,
                update.spin_speed,
                update.jump_speed,
                update.jump_height,
            ];
            if numeric
                .into_iter()
                .flatten()
                .any(|value| !value.is_finite())
            {
                return ControlResponse::error("cursor values must be finite numbers");
            }
            if update
                .scale
                .is_some_and(|value| !(0.001..=100.0).contains(&value))
            {
                return ControlResponse::error("cursor scale must be between 0.001 and 100");
            }
            if update
                .brightness
                .is_some_and(|value| !(0.0..=20.0).contains(&value))
            {
                return ControlResponse::error("cursor brightness must be between 0 and 20");
            }
            if update
                .jump_height
                .is_some_and(|value| !(-10.0..=10.0).contains(&value))
            {
                return ControlResponse::error("jump height must be between -10 and 10");
            }
            if let Some(value) = update.visible {
                app_config.cursor.model.visible = value;
            }
            if let Some(value) = update.scale {
                app_config.cursor.model.scale_factor = value;
            }
            if let Some(value) = update.x_offset {
                app_config.cursor.model.x_offset = value;
            }
            if let Some(value) = update.depth {
                app_config.cursor.model.plane_offset = value;
            }
            if let Some(value) = update.brightness {
                app_config.cursor.model.brightness = value;
            }
            if let Some(value) = update.spin_speed {
                app_config.cursor.animation.spin_speed = value;
            }
            if let Some(value) = update.jump_speed {
                app_config.cursor.animation.bob_speed = value;
            }
            if let Some(value) = update.jump_height {
                app_config.cursor.animation.bob_amplitude = value;
            }
            redraw.request();
            ControlResponse::data(serde_json::json!({ "accepted": true }))
        }
        ControlCommand::SetWindow { update } => {
            let Ok(mut window) = window_query.single_mut() else {
                return ControlResponse::error("primary window is not ready");
            };
            let width = update.width.unwrap_or(window.resolution.width() as u32);
            let height = update.height.unwrap_or(window.resolution.height() as u32);
            if !(64..=16_384).contains(&width) || !(64..=16_384).contains(&height) {
                return ControlResponse::error(
                    "window width and height must be between 64 and 16384",
                );
            }
            if update.width.is_some() || update.height.is_some() {
                window.resolution.set(width as f32, height as f32);
            }
            if update.x.is_some() || update.y.is_some() {
                use bevy::window::WindowPosition;
                let current = match window.position {
                    WindowPosition::At(pos) => pos,
                    _ => IVec2::ZERO,
                };
                window.position = WindowPosition::At(IVec2::new(
                    update.x.unwrap_or(current.x),
                    update.y.unwrap_or(current.y),
                ));
            }
            if let Some(title) = update.title {
                if title.len() > 1024 {
                    return ControlResponse::error("title exceeds 1024 bytes");
                }
                window.title = title;
            }
            if let Some(rgb) = update.background_rgb {
                app_config.theme.background = rgb;
                terminal.set_background(rgb);
                clear_color.0 = Color::srgb_u8(rgb[0], rgb[1], rgb[2]);
                redraw.request();
            }
            ControlResponse::data(serde_json::json!({ "accepted": true }))
        }
        ControlCommand::SetTerminal { update } => {
            let columns = update.columns.unwrap_or(terminal.cols);
            let rows = update.rows.unwrap_or(terminal.rows);
            if !(1..=1000).contains(&columns) || !(1..=1000).contains(&rows) {
                return ControlResponse::error(
                    "terminal columns and rows must be between 1 and 1000",
                );
            }
            if let Some(size) = update.font_size {
                if !(4..=256).contains(&size) {
                    return ControlResponse::error("font size must be between 4 and 256");
                }
                terminal.set_font_size(size);
                app_config.font.size = size;
            }
            terminal.resize(columns, rows);
            let layout = terminal.layout();
            let pixels = layout.pty_pixels();
            runtime.resize(columns, rows, pixels.x as u16, pixels.y as u16);
            sync_terminal_layout(layout, viewport, plane_query, plane_back_query);
            redraw.request();
            ControlResponse::data(
                serde_json::json!({ "columns": columns, "rows": rows, "font_size": terminal.font_size() }),
            )
        }
        ControlCommand::SetShape { update } => {
            let amplitude = update.amplitude.unwrap_or(1.0);
            if !amplitude.is_finite() || !(0.0..=10.0).contains(&amplitude) {
                return ControlResponse::error(
                    "shape amplitude must be finite and between 0 and 10",
                );
            }
            let kind = match update.kind {
                ControlSurfaceKind::Automatic => TerminalSurfaceKind::ModeDefault,
                ControlSurfaceKind::Custom => TerminalSurfaceKind::Custom,
            };
            let (control_columns, control_rows, control_points) = if kind
                == TerminalSurfaceKind::Custom
            {
                let columns = update.control_columns.unwrap_or(0);
                let rows = update.control_rows.unwrap_or(0);
                if !(2..=8).contains(&columns) || !(2..=8).contains(&rows) {
                    return ControlResponse::error(
                        "custom surfaces require 2..8 control columns and rows",
                    );
                }
                let points = update.control_points.unwrap_or_default();
                if points.len() != usize::from(columns) * usize::from(rows) {
                    return ControlResponse::error(
                        "custom control_points length must equal control_columns * control_rows",
                    );
                }
                if points
                    .iter()
                    .flatten()
                    .any(|value| !value.is_finite() || !(-10_000.0..=10_000.0).contains(value))
                {
                    return ControlResponse::error(
                        "custom control-point coordinates must be finite and between -10000 and 10000",
                    );
                }
                (
                    columns,
                    rows,
                    points.into_iter().map(Vec3::from_array).collect(),
                )
            } else {
                (0, 0, Vec::new())
            };
            warp.shape = TerminalSurfaceShape {
                kind,
                amplitude,
                control_columns,
                control_rows,
                control_points,
            };
            if kind == TerminalSurfaceKind::Custom
                && (!slots.active().mode.is_3d()
                    || slots.active().mode == TerminalPresentationMode::Mobius3d)
            {
                camera_updates.write(TerminalCameraUpdate {
                    slot: slots.active_slot(),
                    activate: false,
                    mode: Some(TerminalPresentationMode::Perspective3d),
                    scale: None,
                    fov: None,
                    translation: OptionalVec3::default(),
                    rotation_degrees: OptionalVec3::default(),
                });
            }
            ControlResponse::data(
                serde_json::json!({ "accepted": true, "kind": format!("{kind:?}") }),
            )
        }
    }
}

/// Client used by the stdio MCP facade.
pub struct ControlClient;

impl ControlClient {
    /// Sends one authenticated command to the running Ratty instance.
    pub fn request(command: ControlCommand) -> anyhow::Result<ControlResponse> {
        let path = discovery_path();
        let discovery: Discovery =
            serde_json::from_slice(&fs::read(&path).with_context(|| {
                format!(
                    "Ratty is not available; start it with `ratty --mcp` (looked for {})",
                    path.display()
                )
            })?)?;
        let mut stream = TcpStream::connect(&discovery.address)
            .with_context(|| format!("cannot connect to Ratty at {}", discovery.address))?;
        stream.set_read_timeout(Some(RESPONSE_TIMEOUT))?;
        stream.set_write_timeout(Some(RESPONSE_TIMEOUT))?;
        serde_json::to_writer(
            &mut stream,
            &WireRequest {
                token: discovery.token,
                command,
            },
        )?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let mut line = String::new();
        BufReader::new(stream)
            .take(MAX_REQUEST_BYTES)
            .read_line(&mut line)?;
        if line.is_empty() {
            bail!("Ratty closed the control connection without a response");
        }
        let response: ControlResponse = serde_json::from_str(&line)?;
        if !response.ok {
            bail!(
                response
                    .error
                    .clone()
                    .unwrap_or_else(|| "Ratty rejected the request".into())
            );
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_rejects_wrong_values() {
        assert!(constant_time_eq(b"rat", b"rat"));
        assert!(!constant_time_eq(b"rat", b"bat"));
        assert!(!constant_time_eq(b"rat", b"rats"));
    }

    #[test]
    fn commands_round_trip_as_tagged_json() {
        let command = ControlCommand::SetView {
            update: ViewUpdate {
                mode: Some(ControlViewMode::Mobius),
                warp: Some(0.8),
                ..default()
            },
        };
        let json = serde_json::to_string(&command).unwrap();
        assert!(json.contains("\"command\":\"set_view\""));
        assert!(json.contains("\"mode\":\"mobius\""));
        assert!(serde_json::from_str::<ControlCommand>(&json).is_ok());
    }
}

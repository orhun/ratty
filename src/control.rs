//! Authenticated, local-only control bridge used by the `ratty-mcp` server.

mod snapshot;
use snapshot::{mouse_encoding_name, mouse_protocol_name};

use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, bail};
use bevy::ecs::system::SystemParam;
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use etcetera::{BaseStrategy, choose_base_strategy};
use serde::{Deserialize, Serialize};

use crate::camera::{OptionalVec3, TerminalCameraSlots, TerminalCameraUpdate};
use crate::config::AppConfig;
use crate::keyboard::{NormalizedKey, encode_agent_text, encode_normalized_key};
use crate::runtime::TerminalRuntime;
use crate::scene::{
    TerminalPlane, TerminalPlaneBack, TerminalPlaneWarp, TerminalPresentationMode, TerminalSprite,
    TerminalSurfaceKind, TerminalSurfaceShape, TerminalViewport, sync_terminal_layout,
};
use crate::terminal::{TerminalRedrawState, TerminalSurface};

// The 256 KiB input limit can expand to about 1.5 MiB when JSON escapes controls.
const MAX_REQUEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const DISCOVERY_ENV: &str = "RATTY_MCP_ENDPOINT_FILE";
/// Version of Ratty's private local control protocol.
pub const CONTROL_PROTOCOL_VERSION: u16 = 1;
const MAX_INPUT_BYTES: usize = 256 * 1024;
const CONNECTION_WORKERS: usize = 4;
const CONNECTION_QUEUE: usize = 4;
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

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
    Plane,
    Mobius,
    PaperPlane,
    Gomboc,
    Cylinder,
    Sphere,
    Wave,
    Custom,
}

impl From<TerminalSurfaceKind> for ControlSurfaceKind {
    fn from(value: TerminalSurfaceKind) -> Self {
        match value {
            TerminalSurfaceKind::ModeDefault => Self::Automatic,
            TerminalSurfaceKind::Plane => Self::Plane,
            TerminalSurfaceKind::Mobius => Self::Mobius,
            TerminalSurfaceKind::PaperPlane => Self::PaperPlane,
            TerminalSurfaceKind::Gomboc => Self::Gomboc,
            TerminalSurfaceKind::Cylinder => Self::Cylinder,
            TerminalSurfaceKind::Sphere => Self::Sphere,
            TerminalSurfaceKind::Wave => Self::Wave,
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
    pub frequency: Option<f32>,
    pub twist: Option<f32>,
    pub control_columns: Option<u8>,
    pub control_rows: Option<u8>,
    pub control_points: Option<Vec<[f32; 3]>>,
    pub custom_vertices: Option<Vec<[f32; 5]>>,
    pub custom_triangles: Option<Vec<[u32; 3]>>,
}

pub use snapshot::{
    ScreenSnapshot, SnapshotCursor, SnapshotModes, SnapshotRect, SnapshotRequest, SnapshotRow,
    SnapshotRun, SnapshotStyle,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct WriteTextRequest {
    pub text: String,
    #[serde(default)]
    pub paste: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct PressKeysRequest {
    pub keys: Vec<NormalizedKey>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[allow(missing_docs)]
pub struct ResizeRequest {
    pub columns: u16,
    pub rows: u16,
}

/// Commands sent across the private GUI control socket.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", content = "parameters", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ControlCommand {
    GetState {},
    Snapshot(SnapshotRequest),
    WriteText(WriteTextRequest),
    PressKeys(PressKeysRequest),
    Resize(ResizeRequest),
    Close {},
    SetView(ViewUpdate),
    SetCursor(CursorUpdate),
    SetWindow(WindowUpdate),
    SetTerminal(TerminalUpdate),
    SetShape(ShapeUpdate),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WireRequest {
    protocol_version: u16,
    request_id: u64,
    token: String,
    #[serde(flatten)]
    command: ControlCommand,
}

/// Structured local-control protocol error.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ControlError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

/// Response from the GUI control endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ControlResponse {
    /// Local protocol version.
    pub protocol_version: u16,
    /// Echoed request ID.
    pub request_id: u64,
    /// Successful command result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Structured command or protocol error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
}

impl ControlResponse {
    fn data(value: impl Serialize) -> Self {
        match serde_json::to_value(value) {
            Ok(result) => Self {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                request_id: 0,
                result: Some(result),
                error: None,
            },
            Err(error) => Self::coded_error(
                "serialization_failed",
                format!("failed to serialize command result: {error}"),
            ),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self::coded_error("command_failed", message)
    }

    fn coded_error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: 0,
            result: None,
            error: Some(ControlError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    fn for_request(mut self, request_id: u64) -> Self {
        self.request_id = request_id;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Discovery {
    protocol_version: u16,
    pid: u32,
    address: String,
    token: String,
}

struct PendingCommand {
    command: ControlCommand,
    reply: SyncSender<ControlResponse>,
}

struct AcceptedConnections {
    queue: Mutex<(VecDeque<TcpStream>, bool)>,
    ready: Condvar,
}

impl AcceptedConnections {
    fn push(&self, stream: TcpStream) {
        if let Ok(mut state) = self.queue.lock() {
            if !state.1 && state.0.len() < CONNECTION_QUEUE {
                state.0.push_back(stream);
                self.ready.notify_one();
            }
        }
    }

    fn pop(&self) -> Option<TcpStream> {
        let Ok(mut state) = self.queue.lock() else {
            return None;
        };
        while state.0.is_empty() && !state.1 {
            let Ok(next) = self.ready.wait(state) else {
                return None;
            };
            state = next;
        }
        state.0.pop_front()
    }

    fn close(&self) {
        if let Ok(mut state) = self.queue.lock() {
            state.1 = true;
            self.ready.notify_all();
        }
    }
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
                protocol_version: CONTROL_PROTOCOL_VERSION,
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
    let connections = Arc::new(AcceptedConnections {
        queue: Mutex::new((VecDeque::new(), false)),
        ready: Condvar::new(),
    });
    let mut workers = Vec::with_capacity(CONNECTION_WORKERS);
    for index in 0..CONNECTION_WORKERS {
        let connections = Arc::clone(&connections);
        let tx = tx.clone();
        let token = token.to_owned();
        if let Ok(worker) = thread::Builder::new()
            .name(format!("ratty-mcp-client-{index}"))
            .spawn(move || connection_worker(connections, tx, token))
        {
            workers.push(worker);
        }
    }

    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) if workers.is_empty() => handle_connection(stream, &tx, token),
            Ok((stream, _)) => connections.push(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break,
        }
    }
    connections.close();
    for worker in workers {
        let _ = worker.join();
    }
}

fn connection_worker(
    connections: Arc<AcceptedConnections>,
    commands: mpsc::Sender<PendingCommand>,
    token: String,
) {
    while let Some(stream) = connections.pop() {
        handle_connection(stream, &commands, &token);
    }
}

fn handle_connection(mut stream: TcpStream, tx: &mpsc::Sender<PendingCommand>, token: &str) {
    let _ = stream.set_read_timeout(Some(RESPONSE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RESPONSE_TIMEOUT));
    let parsed = read_wire_request(&stream);
    let response = match parsed {
        Ok(request) => match validate_wire_request(request, token) {
            Ok(request) => {
                let (reply, rx) = mpsc::sync_channel(1);
                let request_id = request.request_id;
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
                .for_request(request_id)
            }
            Err(response) => response,
        },
        Err(error) => {
            ControlResponse::coded_error("invalid_request", format!("invalid request: {error}"))
        }
    };
    let mut bytes = serde_json::to_vec(&response).unwrap_or_else(|_| {
        br#"{"protocol_version":1,"request_id":0,"error":{"code":"serialization_failed","message":"failed to serialize response"}}"#.to_vec()
    });
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        bytes = serde_json::to_vec(
            &ControlResponse::coded_error("response_too_large", "response exceeds 4 MiB")
                .for_request(response.request_id),
        )
        .unwrap_or_default();
    }
    bytes.push(b'\n');
    let _ = stream.write_all(&bytes);
    let _ = stream.flush();
}

fn read_wire_request(reader: impl Read) -> io::Result<WireRequest> {
    let mut bytes = Vec::new();
    BufReader::new(reader)
        .take(MAX_REQUEST_BYTES + 1)
        .read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "request is empty",
        ));
    }
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request exceeds 2 MiB",
        ));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "request must end with a newline",
        ));
    }
    serde_json::from_slice(&bytes[..bytes.len() - 1])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn validate_wire_request(
    request: WireRequest,
    token: &str,
) -> Result<WireRequest, ControlResponse> {
    if request.protocol_version != CONTROL_PROTOCOL_VERSION {
        return Err(ControlResponse::coded_error(
            "unsupported_protocol_version",
            format!(
                "protocol version {} is unsupported; expected {}",
                request.protocol_version, CONTROL_PROTOCOL_VERSION
            ),
        )
        .for_request(request.request_id));
    }
    if !constant_time_eq(request.token.as_bytes(), token.as_bytes()) {
        return Err(
            ControlResponse::coded_error("authentication_failed", "authentication failed")
                .for_request(request.request_id),
        );
    }
    Ok(request)
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
    let bytes = serde_json::to_vec(discovery)?;
    let temporary = parent.join(format!(
        ".mcp-{}-{}.tmp",
        discovery.pid,
        discovery.address.replace(':', "-")
    ));
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
#[derive(SystemParam)]
pub(crate) struct ControlApplyParams<'w, 's> {
    runtime: ResMut<'w, TerminalRuntime>,
    terminal: ResMut<'w, TerminalSurface>,
    slots: Res<'w, TerminalCameraSlots>,
    warp: ResMut<'w, TerminalPlaneWarp>,
    camera_updates: MessageWriter<'w, TerminalCameraUpdate>,
    app_config: ResMut<'w, AppConfig>,
    redraw: ResMut<'w, TerminalRedrawState>,
    window: Query<'w, 's, &'static mut Window, With<bevy::window::PrimaryWindow>>,
    viewport: ResMut<'w, TerminalViewport>,
    plane_query:
        Query<'w, 's, &'static mut Transform, (With<TerminalPlane>, Without<TerminalSprite>)>,
    plane_back_query: Query<
        'w,
        's,
        &'static mut Transform,
        (
            With<TerminalPlaneBack>,
            Without<TerminalPlane>,
            Without<TerminalSprite>,
        ),
    >,
    clear_color: ResMut<'w, ClearColor>,
}

pub(crate) fn pump_control_commands(
    mut control: ResMut<TerminalControl>,
    mut params: ControlApplyParams,
) {
    while let Ok(pending) = control.rx.get().try_recv() {
        let response = apply_command(pending.command, &mut params);
        let _ = pending.reply.send(response);
    }
}

fn apply_command(command: ControlCommand, params: &mut ControlApplyParams) -> ControlResponse {
    let ControlApplyParams {
        runtime,
        terminal,
        slots,
        warp,
        camera_updates,
        app_config,
        redraw,
        window: window_query,
        viewport,
        plane_query,
        plane_back_query,
        clear_color,
    } = params;
    match command {
        ControlCommand::GetState {} => {
            let preset = slots.active();
            let child_exit_code = runtime.child_exit_code();
            let screen = runtime.screen();
            let (cursor_row, cursor_column) = screen.display_cursor_position();
            ControlResponse::data(serde_json::json!({
                "connected": !runtime.pty_disconnected,
                "child": {
                    "running": child_exit_code.is_none() && !runtime.pty_disconnected,
                    "exit_code": child_exit_code,
                },
                "columns": terminal.cols,
                "rows": terminal.rows,
                "font_size": terminal.font_size(),
                "display_cursor": {
                    "row": cursor_row,
                    "column": cursor_column,
                    "visible": !screen.cursor_hidden(),
                },
                "modes": {
                    "alternate_screen": screen.alternate_screen(),
                    "application_cursor": screen.application_cursor(),
                    "application_keypad": screen.application_keypad(),
                    "bracketed_paste": screen.bracketed_paste(),
                    "mouse_protocol": mouse_protocol_name(screen.mouse_protocol_mode()),
                    "mouse_encoding": mouse_encoding_name(screen.mouse_protocol_encoding()),
                    "kitty_keyboard_flags": screen.kitty_keyboard_flags(),
                    "modify_other_keys": screen.modify_other_keys(),
                },
                "activity": {
                    "output_sequence": runtime.output_sequence(),
                    "human_input_sequence": runtime.human_input_sequence(),
                    "agent_input_sequence": runtime.agent_input_sequence(),
                    "last_output_elapsed_ms": runtime.elapsed_since_output().map(|elapsed| {
                        elapsed.as_millis().min(u128::from(u64::MAX)) as u64
                    }),
                },
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
                    "frequency": warp.shape.frequency,
                    "twist": warp.shape.twist,
                    "control_columns": warp.shape.control_columns,
                    "control_rows": warp.shape.control_rows,
                    "control_points": warp.shape.control_points.iter().map(|point| point.to_array()).collect::<Vec<_>>(),
                    "custom_vertices": warp.shape.custom_vertices.iter().map(|vertex| [vertex.position.x, vertex.position.y, vertex.position.z, vertex.uv.x, vertex.uv.y]).collect::<Vec<_>>(),
                    "custom_triangles": warp.shape.custom_triangles,
                }
            }))
        }
        ControlCommand::Snapshot(request) => {
            if request
                .rect
                .is_some_and(|rect| rect.rows == 0 || rect.columns == 0)
            {
                return ControlResponse::error("snapshot rectangle dimensions must be non-zero");
            }
            ControlResponse::data(ScreenSnapshot::from_screen(
                runtime.screen(),
                &request,
                runtime.screen_revision(),
            ))
        }
        ControlCommand::WriteText(request) => {
            if runtime.pty_disconnected {
                return ControlResponse::coded_error(
                    "terminal_disconnected",
                    "cannot write text because the terminal child is not running",
                );
            }
            if request.text.len() > MAX_INPUT_BYTES {
                return ControlResponse::coded_error("input_too_large", "input exceeds 256 KiB");
            }
            let bracketed = request.paste && runtime.screen().bracketed_paste();
            let bytes = encode_agent_text(&request.text, request.paste, &runtime.screen());
            let bytes_written = bytes.len();
            runtime.write_agent_input(&bytes);
            ControlResponse::data(serde_json::json!({
                "text_bytes": request.text.len(),
                "bytes_written": bytes_written,
                "paste_requested": request.paste,
                "bracketed": bracketed,
            }))
        }
        ControlCommand::PressKeys(request) => {
            if runtime.pty_disconnected {
                return ControlResponse::coded_error(
                    "terminal_disconnected",
                    "cannot press keys because the terminal child is not running",
                );
            }
            if request.keys.len() > 256 {
                return ControlResponse::coded_error(
                    "input_too_large",
                    "at most 256 keys may be pressed per request",
                );
            }
            let key_count = request.keys.len();
            let mut encoded = Vec::new();
            for key in &request.keys {
                let bytes = match encode_normalized_key(key, &runtime.screen()) {
                    Ok(bytes) => bytes,
                    Err(error) => return ControlResponse::error(error),
                };
                if encoded.len().saturating_add(bytes.len()) > MAX_INPUT_BYTES {
                    return ControlResponse::coded_error(
                        "input_too_large",
                        "encoded key input exceeds 256 KiB",
                    );
                }
                encoded.extend_from_slice(&bytes);
            }
            let bytes_written = encoded.len();
            runtime.write_agent_input(&encoded);
            ControlResponse::data(serde_json::json!({
                "keys_pressed": key_count,
                "bytes_written": bytes_written,
            }))
        }
        ControlCommand::Resize(request) => {
            if !(1..=1000).contains(&request.columns) || !(1..=1000).contains(&request.rows) {
                return ControlResponse::error(
                    "terminal columns and rows must be between 1 and 1000",
                );
            }
            terminal.resize(request.columns, request.rows);
            let layout = terminal.layout();
            let pixels = layout.pty_pixels();
            if let Err(error) = runtime.resize(
                request.columns,
                request.rows,
                pixels.x as u16,
                pixels.y as u16,
            ) {
                warn!("terminal resize remains pending: {error:#}");
            }
            sync_terminal_layout(layout, viewport, plane_query, plane_back_query);
            redraw.request();
            ControlResponse::data(
                serde_json::json!({ "columns": request.columns, "rows": request.rows }),
            )
        }
        ControlCommand::Close {} => {
            runtime.shutdown();
            ControlResponse::data(serde_json::json!({ "accepted": true }))
        }
        ControlCommand::SetView(update) => {
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
            if update
                .warp
                .is_some_and(|amount| !(0.0..=1.0).contains(&amount))
            {
                return ControlResponse::error("warp must be between 0 and 1");
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
            if let Some(amount) = update.warp {
                warp.amount = amount;
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
        ControlCommand::SetCursor(update) => {
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
        ControlCommand::SetWindow(update) => {
            if update
                .title
                .as_ref()
                .is_some_and(|title| title.len() > 1024)
            {
                return ControlResponse::error("title exceeds 1024 bytes");
            }
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
                window.title = title;
            }
            if let Some(rgb) = update.background_rgb {
                app_config.theme.background = rgb;
                terminal.set_background(rgb);
                clear_color.0 = Color::srgba_u8(
                    rgb[0],
                    rgb[1],
                    rgb[2],
                    (app_config.window.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
                );
                redraw.request();
            }
            ControlResponse::data(serde_json::json!({ "accepted": true }))
        }
        ControlCommand::SetTerminal(update) => {
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
            if let Err(error) = runtime.resize(columns, rows, pixels.x as u16, pixels.y as u16) {
                warn!("terminal resize remains pending: {error:#}");
            }
            sync_terminal_layout(layout, viewport, plane_query, plane_back_query);
            redraw.request();
            ControlResponse::data(
                serde_json::json!({ "columns": columns, "rows": rows, "font_size": terminal.font_size() }),
            )
        }
        ControlCommand::SetShape(update) => {
            let amplitude = update.amplitude.unwrap_or(1.0);
            if !amplitude.is_finite() || !(0.0..=10.0).contains(&amplitude) {
                return ControlResponse::error(
                    "shape amplitude must be finite and between 0 and 10",
                );
            }
            let frequency = update.frequency.unwrap_or(2.0);
            if !frequency.is_finite() || !(0.01..=32.0).contains(&frequency) {
                return ControlResponse::error(
                    "shape frequency must be finite and between 0.01 and 32",
                );
            }
            let kind = match update.kind {
                ControlSurfaceKind::Automatic => TerminalSurfaceKind::ModeDefault,
                ControlSurfaceKind::Plane => TerminalSurfaceKind::Plane,
                ControlSurfaceKind::Mobius => TerminalSurfaceKind::Mobius,
                ControlSurfaceKind::PaperPlane => TerminalSurfaceKind::PaperPlane,
                ControlSurfaceKind::Gomboc => TerminalSurfaceKind::Gomboc,
                ControlSurfaceKind::Cylinder => TerminalSurfaceKind::Cylinder,
                ControlSurfaceKind::Sphere => TerminalSurfaceKind::Sphere,
                ControlSurfaceKind::Wave => TerminalSurfaceKind::Wave,
                ControlSurfaceKind::Custom => TerminalSurfaceKind::Custom,
            };
            let twist = update
                .twist
                .unwrap_or(if kind == TerminalSurfaceKind::Custom {
                    0.0
                } else {
                    1.0
                });
            if !twist.is_finite() || !(-16.0..=16.0).contains(&twist) {
                return ControlResponse::error("shape twist must be finite and between -16 and 16");
            }
            let (control_columns, control_rows, control_points, custom_vertices, custom_triangles) =
                if kind == TerminalSurfaceKind::Custom {
                    let vertices = update.custom_vertices.unwrap_or_default();
                    let triangles = update.custom_triangles.unwrap_or_default();
                    if !vertices.is_empty() || !triangles.is_empty() {
                        if update.control_columns.is_some()
                            || update.control_rows.is_some()
                            || update.control_points.is_some()
                        {
                            return ControlResponse::error(
                                "custom mesh vertices/triangles cannot be combined with a control lattice",
                            );
                        }
                        if !(3..=256).contains(&vertices.len())
                            || !(1..=512).contains(&triangles.len())
                        {
                            return ControlResponse::error(
                                "custom meshes require 3..256 vertices and 1..512 triangles",
                            );
                        }
                        if vertices.iter().flatten().any(|value| !value.is_finite())
                            || vertices.iter().any(|vertex| {
                                vertex[..3]
                                    .iter()
                                    .any(|value| !(-10_000.0..=10_000.0).contains(value))
                                    || vertex[3..].iter().any(|value| !(0.0..=1.0).contains(value))
                            })
                        {
                            return ControlResponse::error(
                                "custom mesh positions must be finite and between -10000 and 10000; UVs must be between 0 and 1",
                            );
                        }
                        if triangles.iter().any(|triangle| {
                            triangle
                                .iter()
                                .any(|index| *index as usize >= vertices.len())
                                || triangle[0] == triangle[1]
                                || triangle[1] == triangle[2]
                                || triangle[2] == triangle[0]
                        }) {
                            return ControlResponse::error(
                                "custom mesh triangles must reference three distinct valid vertex indices",
                            );
                        }
                        let vertices = vertices
                            .into_iter()
                            .map(|[x, y, z, u, v]| crate::scene::CustomSurfaceVertex {
                                position: Vec3::new(x, y, z),
                                uv: Vec2::new(u, v),
                            })
                            .collect();
                        (0, 0, Vec::new(), vertices, triangles)
                    } else {
                        let columns = update.control_columns.unwrap_or(0);
                        let rows = update.control_rows.unwrap_or(0);
                        if !(2..=8).contains(&columns) || !(2..=8).contains(&rows) {
                            return ControlResponse::error(
                                "no custom geometry reached Ratty: provide control_columns, control_rows, and control_points, or custom_vertices and custom_triangles. A lattice is one continuous sheet; reload ratty-mcp if its schema omits the mesh fields",
                            );
                        }
                        let points = update.control_points.unwrap_or_default();
                        if points.len() != usize::from(columns) * usize::from(rows) {
                            return ControlResponse::error(
                                "custom control_points length must equal control_columns * control_rows",
                            );
                        }
                        if points.iter().flatten().any(|value| {
                            !value.is_finite() || !(-10_000.0..=10_000.0).contains(value)
                        }) {
                            return ControlResponse::error(
                                "custom control-point coordinates must be finite and between -10000 and 10000",
                            );
                        }
                        (
                            columns,
                            rows,
                            points.into_iter().map(Vec3::from_array).collect(),
                            Vec::new(),
                            Vec::new(),
                        )
                    }
                } else {
                    (0, 0, Vec::new(), Vec::new(), Vec::new())
                };
            warp.shape = TerminalSurfaceShape {
                kind,
                amplitude,
                frequency,
                twist,
                control_columns,
                control_rows,
                control_points,
                custom_vertices,
                custom_triangles,
            };
            let active_mode = slots.active().mode;
            let needs_camera_mode = match kind {
                TerminalSurfaceKind::ModeDefault => false,
                TerminalSurfaceKind::Mobius => active_mode != TerminalPresentationMode::Mobius3d,
                _ => !active_mode.is_3d() || active_mode == TerminalPresentationMode::Mobius3d,
            };
            if needs_camera_mode {
                camera_updates.write(TerminalCameraUpdate {
                    slot: slots.active_slot(),
                    activate: false,
                    mode: Some(if kind == TerminalSurfaceKind::Mobius {
                        TerminalPresentationMode::Mobius3d
                    } else {
                        TerminalPresentationMode::Perspective3d
                    }),
                    scale: None,
                    fov: None,
                    translation: OptionalVec3::default(),
                    rotation_degrees: OptionalVec3::default(),
                });
            }
            ControlResponse::data(serde_json::json!({
                "accepted": true,
                "kind": ControlSurfaceKind::from(kind),
                "amplitude": warp.shape.amplitude,
                "frequency": warp.shape.frequency,
                "twist": warp.shape.twist,
                "geometry": if !warp.shape.custom_vertices.is_empty() {
                    "triangle_mesh"
                } else if !warp.shape.control_points.is_empty() {
                    "control_lattice"
                } else {
                    "built_in_surface"
                },
                "control_columns": warp.shape.control_columns,
                "control_rows": warp.shape.control_rows,
                "control_point_count": warp.shape.control_points.len(),
                "vertex_count": warp.shape.custom_vertices.len(),
                "triangle_count": warp.shape.custom_triangles.len(),
            }))
        }
    }
}

/// Client used by the stdio MCP facade.
pub struct ControlClient;

impl ControlClient {
    /// Sends one authenticated command to the running Ratty instance.
    pub fn request(command: ControlCommand) -> anyhow::Result<ControlResponse> {
        let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = discovery_path();
        let discovery: Discovery =
            serde_json::from_slice(&fs::read(&path).with_context(|| {
                format!(
                    "Ratty is not available; start it with `ratty --mcp` (looked for {})",
                    path.display()
                )
            })?)?;
        if discovery.protocol_version != CONTROL_PROTOCOL_VERSION {
            bail!(
                "Ratty discovery record uses protocol version {}; expected {}",
                discovery.protocol_version,
                CONTROL_PROTOCOL_VERSION
            );
        }
        let mut stream = TcpStream::connect(&discovery.address)
            .with_context(|| {
                format!(
                    "cannot connect to Ratty at {}; the discovery record may be stale—start a fresh `ratty --mcp` window",
                    discovery.address
                )
            })?;
        stream.set_read_timeout(Some(RESPONSE_TIMEOUT))?;
        stream.set_write_timeout(Some(RESPONSE_TIMEOUT))?;
        serde_json::to_writer(
            &mut stream,
            &WireRequest {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                request_id,
                token: discovery.token,
                command,
            },
        )?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let mut line = String::new();
        BufReader::new(stream)
            .take(MAX_RESPONSE_BYTES + 1)
            .read_line(&mut line)?;
        if line.is_empty() {
            bail!("Ratty closed the control connection without a response");
        }
        if line.len() as u64 > MAX_RESPONSE_BYTES {
            bail!("Ratty control response exceeds 4 MiB");
        }
        let response: ControlResponse = serde_json::from_str(&line)?;
        if response.protocol_version != CONTROL_PROTOCOL_VERSION {
            bail!("Ratty returned an unsupported control protocol version");
        }
        if response.request_id != request_id {
            bail!("Ratty returned a mismatched control request ID");
        }
        if let Some(error) = &response.error {
            bail!("{}: {}", error.code, error.message);
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fux_vt::Parser;

    use crate::screen::ScreenView;

    fn snapshot(input: &str, rows: u16, columns: u16) -> ScreenSnapshot {
        let mut parser = Parser::new(rows, columns, 0).expect("parser");
        for chunk in input.as_bytes().chunks(1) {
            parser.process(chunk).expect("process");
        }
        ScreenSnapshot::from_screen(
            ScreenView::new(parser.screen(), 0),
            &SnapshotRequest {
                rect: None,
                include_styles: true,
                include_plain: true,
            },
            0,
        )
    }

    #[test]
    fn token_comparison_rejects_wrong_values() {
        assert!(constant_time_eq(b"rat", b"rat"));
        assert!(!constant_time_eq(b"rat", b"bat"));
        assert!(!constant_time_eq(b"rat", b"rats"));
    }

    #[test]
    fn wire_authentication_and_version_errors_echo_request_ids() {
        let wrong_token = WireRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: 17,
            token: "wrong".into(),
            command: ControlCommand::GetState {},
        };
        let response = validate_wire_request(wrong_token, "right")
            .expect_err("an incorrect token must be rejected");
        assert_eq!(response.request_id, 17);
        assert_eq!(
            response
                .error
                .expect("authentication failure must include an error")
                .code,
            "authentication_failed"
        );

        let wrong_version = WireRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION + 1,
            request_id: 23,
            token: "right".into(),
            command: ControlCommand::GetState {},
        };
        let response = validate_wire_request(wrong_version, "right")
            .expect_err("an unsupported protocol version must be rejected");
        assert_eq!(response.request_id, 23);
        assert_eq!(
            response
                .error
                .expect("version failure must include an error")
                .code,
            "unsupported_protocol_version"
        );
    }

    #[test]
    fn malformed_wire_requests_are_rejected() {
        for json in [
            "not json",
            r#"{"protocol_version":1,"request_id":1,"token":"x"}"#,
            r#"{"protocol_version":"one","request_id":1,"token":"x","command":"get_state"}"#,
        ] {
            assert!(serde_json::from_str::<WireRequest>(json).is_err());
        }
    }

    #[test]
    fn wire_reader_requires_a_bounded_newline_delimited_request() {
        let request = WireRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: 31,
            token: "secret".into(),
            command: ControlCommand::GetState {},
        };
        let mut bytes = serde_json::to_vec(&request).expect("request must serialize");
        assert_eq!(
            read_wire_request(bytes.as_slice())
                .expect_err("a request without its delimiter must fail")
                .kind(),
            io::ErrorKind::UnexpectedEof
        );

        bytes.push(b'\n');
        let parsed = read_wire_request(bytes.as_slice()).expect("delimited request must parse");
        assert_eq!(parsed.request_id, 31);

        let oversized = vec![b' '; usize::try_from(MAX_REQUEST_BYTES).expect("limit fits") + 1];
        assert_eq!(
            read_wire_request(oversized.as_slice())
                .expect_err("oversized request must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_write_does_not_change_custom_parent_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "ratty-control-test-{}-{}",
            std::process::id(),
            NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).expect("test directory must be created");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .expect("test permissions must be set");
        let path = directory.join("endpoint.json");
        write_discovery(
            &path,
            &Discovery {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                pid: std::process::id(),
                address: "127.0.0.1:1".into(),
                token: "secret".into(),
            },
        )
        .expect("discovery record must be written");

        assert_eq!(
            fs::metadata(&directory)
                .expect("directory metadata must exist")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("record metadata must exist")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_file(path).expect("record cleanup must succeed");
        fs::remove_dir(directory).expect("directory cleanup must succeed");
    }

    #[test]
    fn commands_round_trip_as_tagged_json() {
        let command = ControlCommand::SetView(ViewUpdate {
            mode: Some(ControlViewMode::Mobius),
            warp: Some(0.8),
            ..default()
        });
        let json = serde_json::to_string(&command).expect("command must serialize");
        assert!(json.contains("\"command\":\"set_view\""));
        assert!(json.contains("\"mode\":\"mobius\""));
        assert!(serde_json::from_str::<ControlCommand>(&json).is_ok());

        let request = WireRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: 9,
            token: "secret".into(),
            command: ControlCommand::GetState {},
        };
        let value = serde_json::to_value(request).expect("request must serialize");
        assert_eq!(value["protocol_version"], 1);
        assert_eq!(value["request_id"], 9);
        assert_eq!(value["command"], "get_state");
        assert_eq!(value["parameters"], serde_json::json!({}));
    }

    #[test]
    fn snapshot_preserves_unicode_clusters_and_terminal_columns() {
        let snapshot = snapshot("A界e\u{301}👩🏽‍💻", 2, 12);
        let runs = &snapshot.content[0].runs;
        assert_eq!(runs[0].text, "A");
        assert_eq!((runs[0].column, runs[0].columns), (0, 1));
        assert_eq!(runs[1].text, "界");
        assert_eq!((runs[1].column, runs[1].columns), (1, 2));
        assert_eq!(runs[2].text, "e\u{301}");
        assert_eq!((runs[2].column, runs[2].columns), (3, 1));
        assert_eq!(runs[3].text, "👩🏽‍💻");
        assert_eq!((runs[3].column, runs[3].columns), (4, 2));
        assert_eq!(snapshot.content[0].plain.as_deref(), Some("A界e\u{301}👩🏽‍💻"));

        let json = serde_json::to_string(&snapshot).expect("snapshot must serialize");
        assert_eq!(
            serde_json::from_str::<ScreenSnapshot>(&json).expect("snapshot must deserialize"),
            snapshot
        );
    }

    #[test]
    fn snapshot_preserves_diverse_unicode_without_normalization() {
        for text in ["┌─┐", "日本語", "é e\u{301}", "✈️", "👩🏽‍💻", "👨‍👩‍👧‍👦", "🇩🇪", "कि"]
        {
            let snapshot = snapshot(text, 2, 40);
            let emitted = snapshot.content[0]
                .runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>();
            assert_eq!(emitted, text);
            assert!(snapshot.content[0].runs.iter().all(|run| run.columns >= 1));
        }
    }

    #[test]
    fn snapshot_never_starts_a_run_on_a_continuation_cell() {
        let mut parser = Parser::new(2, 5, 0).expect("parser");
        parser.process("abc界界".as_bytes()).expect("process");
        let screen = parser.screen();
        let snapshot = ScreenSnapshot::from_screen(
            ScreenView::new(screen, 0),
            &SnapshotRequest {
                rect: None,
                include_styles: true,
                include_plain: false,
            },
            0,
        );
        for row in &snapshot.content {
            for run in &row.runs {
                let cell = screen
                    .cell(row.row, run.column)
                    .expect("snapshot run must reference a visible cell");
                assert!(!cell.is_wide_continuation());
                if !run
                    .text
                    .as_bytes()
                    .iter()
                    .all(|byte| (0x20..=0x7e).contains(byte))
                {
                    assert_eq!(run.columns, if cell.is_wide() { 2 } else { 1 });
                }
            }
        }
    }

    #[test]
    fn snapshot_coalesces_only_adjacent_ascii_with_equal_styles() {
        let snapshot = snapshot("ab\x1b[31mc\x1b[m界d", 2, 12);
        let runs = &snapshot.content[0].runs;
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[0].columns, 2);
        assert_eq!(runs[1].text, "c");
        assert_ne!(runs[0].style, runs[1].style);
        assert_eq!(runs[2].text, "界");
        assert_eq!(runs[3].text, "d");
    }

    #[test]
    fn snapshot_keeps_styled_blanks_wrapping_and_graphics_metadata() {
        let placeholder = crate::screen::KITTY_PLACEHOLDER;
        let snapshot = snapshot(&format!("\x1b[44m \x1b[mab{placeholder}c"), 3, 3);
        assert!(snapshot.content.iter().any(|row| row.wrapped));
        assert!(snapshot.content.iter().any(|row| row.graphics_placeholder));
        assert!(
            snapshot
                .content
                .iter()
                .flat_map(|row| &row.runs)
                .any(|run| {
                    run.text == " "
                        && snapshot.styles.as_ref().expect("styles were requested")
                            [run.style as usize]
                            .background
                            == "indexed:4"
                })
        );
        assert!(
            !snapshot
                .content
                .iter()
                .flat_map(|row| &row.runs)
                .any(|run| run.text.contains(placeholder))
        );
    }

    #[test]
    fn rectangular_snapshot_never_splits_a_wide_cell() {
        let mut snapshot = snapshot("界x", 1, 4);
        snapshot = snapshot.into_view(
            Some(SnapshotRect {
                row: 0,
                column: 1,
                rows: 1,
                columns: 1,
            }),
            true,
            false,
        );
        assert_eq!(snapshot.content[0].runs.len(), 1);
        assert_eq!(snapshot.content[0].runs[0].column, 0);
        assert_eq!(snapshot.content[0].runs[0].columns, 2);
        assert_eq!(snapshot.content[0].runs[0].text, "界");
    }

    #[test]
    fn rectangular_snapshot_clips_coalesced_ascii_runs() {
        let snapshot = snapshot("abcdef", 1, 8).into_view(
            Some(SnapshotRect {
                row: 0,
                column: 2,
                rows: 1,
                columns: 2,
            }),
            true,
            true,
        );
        assert_eq!(snapshot.content[0].runs.len(), 1);
        assert_eq!(snapshot.content[0].runs[0].column, 2);
        assert_eq!(snapshot.content[0].runs[0].columns, 2);
        assert_eq!(snapshot.content[0].runs[0].text, "cd");
        assert_eq!(snapshot.content[0].plain.as_deref(), Some("  cd"));
    }

    #[test]
    fn snapshot_uses_display_cursor_for_pending_wrap() {
        let snapshot = snapshot("abc", 2, 3);
        assert_eq!((snapshot.cursor.row, snapshot.cursor.column), (0, 2));
    }
}

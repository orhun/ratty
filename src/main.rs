use std::env;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, ensure};
use bevy::app::AppExit;
use bevy::asset::RenderAssetUsages;
use bevy::camera::ClearColorConfig;
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::image::ImageSampler;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::Terminal;
use ratatui::style::{Color as TuiColor, Style};
use ratatui::widgets::Paragraph;
use soft_ratatui::embedded_graphics_unicodefonts::{
    mono_8x13_atlas, mono_8x13_bold_atlas, mono_8x13_italic_atlas,
};
use soft_ratatui::{EmbeddedGraphics, SoftBackend};
use vt100::Parser;

const WINDOW_WIDTH: f32 = 1180.0;
const WINDOW_HEIGHT: f32 = 760.0;
const DEFAULT_COLS: u16 = 104;
const DEFAULT_ROWS: u16 = 32;
const TERMINAL_SCROLLBACK: usize = 10_000;
const VIEW_PADDING: f32 = 64.0;
const CURSOR_DEPTH: f32 = 10.0;
const CURSOR_SCALE_FACTOR: f32 = 5.2;

const THEME_BG: TuiColor = TuiColor::Rgb(244, 240, 231);
const THEME_FG: TuiColor = TuiColor::Rgb(32, 37, 44);

#[derive(Component)]
struct AssetShowcase;

#[derive(Resource, Clone, Copy)]
struct TerminalViewport {
    size: Vec2,
    center: Vec2,
}

fn main() -> anyhow::Result<()> {
    let runtime = TerminalRuntime::spawn(DEFAULT_COLS, DEFAULT_ROWS)?;
    let soft_terminal = SoftTerminal::new(DEFAULT_COLS, DEFAULT_ROWS);

    App::new()
        .insert_resource(ClearColor(Color::srgb(0.94, 0.92, 0.88)))
        .insert_non_send_resource(runtime)
        .insert_non_send_resource(soft_terminal)
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "ratterm".into(),
                resolution: (WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(TerminalPlugin)
        .run();

    Ok(())
}

struct TerminalPlugin;

impl Plugin for TerminalPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_scene)
            .add_systems(Update, pump_pty_output)
            .add_systems(Update, handle_keyboard_input)
            .add_systems(Update, redraw_soft_terminal.after(pump_pty_output))
            .add_systems(
                Update,
                sync_asset_to_terminal_cursor.after(redraw_soft_terminal),
            );
    }
}

struct TerminalRuntime {
    rx: Receiver<Vec<u8>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    parser: Parser,
    pty_disconnected: bool,
}

impl TerminalRuntime {
    fn spawn(cols: u16, rows: u16) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to create PTY pair")?;

        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell")?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("failed to create PTY writer")?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(size) => {
                        if tx.send(buf[..size].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            rx,
            writer: Arc::new(Mutex::new(writer)),
            _master: pair.master,
            _child: child,
            parser: Parser::new(rows, cols, TERMINAL_SCROLLBACK),
            pty_disconnected: false,
        })
    }

    fn write_input(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }
}

struct SoftTerminal {
    terminal: Terminal<SoftBackend<EmbeddedGraphics>>,
    image_handle: Option<Handle<Image>>,
    cols: u16,
    rows: u16,
}

impl SoftTerminal {
    fn new(cols: u16, rows: u16) -> Self {
        let backend = SoftBackend::<EmbeddedGraphics>::new(
            cols,
            rows,
            mono_8x13_atlas(),
            Some(mono_8x13_bold_atlas()),
            Some(mono_8x13_italic_atlas()),
        );

        let mut terminal =
            Terminal::new(backend).expect("soft_ratatui backend is infallible for Terminal::new");
        let _ = terminal.clear();
        terminal.backend_mut().cursor = false;

        Self {
            terminal,
            image_handle: None,
            cols,
            rows,
        }
    }
}

fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut soft_terminal: NonSendMut<SoftTerminal>,
) {
    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            near: -2000.0,
            far: 2000.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 800.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    let pixmap_width = soft_terminal.terminal.backend().get_pixmap_width() as u32;
    let pixmap_height = soft_terminal.terminal.backend().get_pixmap_height() as u32;

    let mut image = Image::new_fill(
        Extent3d {
            width: pixmap_width,
            height: pixmap_height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.data = Some(soft_terminal.terminal.backend().get_pixmap_data_as_rgba());
    image.sampler = ImageSampler::nearest();

    let image_handle = images.add(image);
    soft_terminal.image_handle = Some(image_handle.clone());

    let viewport_size = Vec2::new(
        WINDOW_WIDTH - 2.0 * VIEW_PADDING,
        WINDOW_HEIGHT - 2.0 * VIEW_PADDING,
    );
    let viewport_center = Vec2::ZERO;
    commands.insert_resource(TerminalViewport {
        size: viewport_size,
        center: viewport_center,
    });

    let mut sprite = Sprite::from_image(image_handle);
    sprite.custom_size = Some(viewport_size);
    commands.spawn((
        sprite,
        Transform::from_translation(Vec3::new(viewport_center.x, viewport_center.y, 0.0)),
    ));

    commands.spawn((
        PointLight {
            intensity: 90_000.0,
            range: 1600.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(0.0, 260.0, 900.0),
    ));

    spawn_3d_asset_showcase(&mut commands, &mut meshes, &mut materials);
}

fn spawn_3d_asset_showcase(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let root = commands
        .spawn((
            AssetShowcase,
            Transform::from_xyz(0.0, 0.0, CURSOR_DEPTH),
            Visibility::Visible,
        ))
        .id();
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.86, 0.52, 0.22),
        metallic: 0.1,
        perceptual_roughness: 0.55,
        ..default()
    });

    let maybe_obj_path = discover_obj_model_path();
    let maybe_meshes = maybe_obj_path
        .as_ref()
        .map(|path| load_obj_meshes(path).map(|loaded| (path, loaded)));

    match maybe_meshes {
        Some(Ok((path, loaded_meshes))) if !loaded_meshes.is_empty() => {
            info!(
                "loaded showcase model from {} ({} mesh parts)",
                path.display(),
                loaded_meshes.len()
            );
            commands.entity(root).with_children(|parent| {
                for mesh in loaded_meshes {
                    parent.spawn((
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(material.clone()),
                        Transform::default(),
                    ));
                }
            });
        }
        Some(Err(error)) => {
            warn!("failed to load OBJ model from model/: {error:#}");
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
                    MeshMaterial3d(material),
                ));
            });
        }
        _ => {
            warn!("no OBJ model found in model/; using cube cursor fallback");
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
                    MeshMaterial3d(material),
                ));
            });
        }
    }
}

fn discover_obj_model_path() -> Option<PathBuf> {
    let entries = fs::read_dir("model").ok()?;
    let mut candidates = Vec::new();

    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();
        let is_obj = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("obj"))
            .unwrap_or(false);
        if is_obj {
            let modified = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            candidates.push((path, modified));
        }
    }

    candidates
        .into_iter()
        .max_by_key(|(_, modified)| *modified)
        .map(|(path, _)| path)
}

fn load_obj_meshes(path: &Path) -> anyhow::Result<Vec<Mesh>> {
    let options = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ignore_lines: true,
        ignore_points: true,
        ..default()
    };
    let (models, _) = tobj::load_obj(path, &options)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let mut output = Vec::new();
    for model in models {
        let source_mesh = model.mesh;
        if source_mesh.positions.is_empty() {
            continue;
        }

        let mut positions = Vec::<[f32; 3]>::with_capacity(source_mesh.positions.len() / 3);
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for pos in source_mesh.positions.chunks_exact(3) {
            let point = Vec3::new(pos[0], pos[1], pos[2]);
            min = min.min(point);
            max = max.max(point);
            positions.push([point.x, point.y, point.z]);
        }

        let center = (min + max) * 0.5;
        let extent = max - min;
        let max_extent = extent.max_element().max(1e-6);
        for p in &mut positions {
            p[0] = (p[0] - center.x) / max_extent;
            p[1] = (p[1] - center.y) / max_extent;
            p[2] = (p[2] - center.z) / max_extent;
        }

        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);

        if !source_mesh.normals.is_empty() {
            let normals = source_mesh
                .normals
                .chunks_exact(3)
                .map(|normal| [normal[0], normal[1], normal[2]])
                .collect::<Vec<[f32; 3]>>();
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        }

        mesh.insert_indices(Indices::U32(source_mesh.indices));
        output.push(mesh);
    }

    ensure!(
        !output.is_empty(),
        "no mesh content inside {}",
        path.display()
    );
    Ok(output)
}

fn sync_asset_to_terminal_cursor(
    runtime: NonSend<TerminalRuntime>,
    soft_terminal: NonSend<SoftTerminal>,
    viewport: Res<TerminalViewport>,
    time: Res<Time>,
    mut query: Query<(&mut Transform, &mut Visibility), With<AssetShowcase>>,
) {
    let cols = soft_terminal.cols.max(1) as f32;
    let rows = soft_terminal.rows.max(1) as f32;
    let cell_width = viewport.size.x / cols;
    let cell_height = viewport.size.y / rows;
    let scale = cell_width.min(cell_height) * CURSOR_SCALE_FACTOR;

    let screen = runtime.parser.screen();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let cursor_col = cursor_col.min(soft_terminal.cols.saturating_sub(1)) as f32;
    let cursor_row = cursor_row.min(soft_terminal.rows.saturating_sub(1)) as f32;

    let world_x = viewport.center.x - viewport.size.x * 0.5 + (cursor_col + 0.5) * cell_width;
    let world_y = viewport.center.y + viewport.size.y * 0.5 - (cursor_row + 0.5) * cell_height;
    let spin = time.elapsed_secs() * 1.4;
    let bob = (time.elapsed_secs() * 2.2).sin() * cell_height * 0.08;

    for (mut transform, mut visibility) in &mut query {
        transform.translation = Vec3::new(world_x, world_y + bob, CURSOR_DEPTH);
        transform.rotation = Quat::from_rotation_y(spin) * Quat::from_rotation_x(-0.25);
        transform.scale = Vec3::splat(scale.max(0.001));
        *visibility = if screen.hide_cursor() {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
}

fn pump_pty_output(mut runtime: NonSendMut<TerminalRuntime>, mut app_exit: MessageWriter<AppExit>) {
    loop {
        match runtime.rx.try_recv() {
            Ok(chunk) => runtime.parser.process(&chunk),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                if !runtime.pty_disconnected {
                    runtime.pty_disconnected = true;
                    app_exit.write(AppExit::Success);
                }
                break;
            }
        }
    }
}

fn handle_keyboard_input(
    mut keyboard_events: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    runtime: NonSend<TerminalRuntime>,
) {
    let ctrl_pressed = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let alt_pressed = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);

    for event in keyboard_events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }

        let mut input = Vec::new();
        if ctrl_pressed {
            if let Some(ctrl) = ctrl_keycode_byte(event.key_code) {
                input.push(ctrl);
                runtime.write_input(&input);
                continue;
            }
        }

        match event.key_code {
            KeyCode::Enter | KeyCode::NumpadEnter => input.push(b'\r'),
            KeyCode::Tab => input.push(b'\t'),
            KeyCode::Backspace => input.push(0x7f),
            KeyCode::Escape => input.push(0x1b),
            KeyCode::ArrowUp => input.extend_from_slice(b"\x1b[A"),
            KeyCode::ArrowDown => input.extend_from_slice(b"\x1b[B"),
            KeyCode::ArrowRight => input.extend_from_slice(b"\x1b[C"),
            KeyCode::ArrowLeft => input.extend_from_slice(b"\x1b[D"),
            KeyCode::Delete => input.extend_from_slice(b"\x1b[3~"),
            KeyCode::Home => input.extend_from_slice(b"\x1b[H"),
            KeyCode::End => input.extend_from_slice(b"\x1b[F"),
            KeyCode::PageUp => input.extend_from_slice(b"\x1b[5~"),
            KeyCode::PageDown => input.extend_from_slice(b"\x1b[6~"),
            _ => {
                if let Key::Character(chars) = &event.logical_key {
                    if alt_pressed {
                        input.push(0x1b);
                    }
                    input.extend_from_slice(chars.as_bytes());
                }
            }
        }

        runtime.write_input(&input);
    }
}

fn ctrl_keycode_byte(key: KeyCode) -> Option<u8> {
    match key {
        KeyCode::KeyA => Some(0x01),
        KeyCode::KeyB => Some(0x02),
        KeyCode::KeyC => Some(0x03),
        KeyCode::KeyD => Some(0x04),
        KeyCode::KeyE => Some(0x05),
        KeyCode::KeyF => Some(0x06),
        KeyCode::KeyG => Some(0x07),
        KeyCode::KeyH => Some(0x08),
        KeyCode::KeyI => Some(0x09),
        KeyCode::KeyJ => Some(0x0a),
        KeyCode::KeyK => Some(0x0b),
        KeyCode::KeyL => Some(0x0c),
        KeyCode::KeyM => Some(0x0d),
        KeyCode::KeyN => Some(0x0e),
        KeyCode::KeyO => Some(0x0f),
        KeyCode::KeyP => Some(0x10),
        KeyCode::KeyQ => Some(0x11),
        KeyCode::KeyR => Some(0x12),
        KeyCode::KeyS => Some(0x13),
        KeyCode::KeyT => Some(0x14),
        KeyCode::KeyU => Some(0x15),
        KeyCode::KeyV => Some(0x16),
        KeyCode::KeyW => Some(0x17),
        KeyCode::KeyX => Some(0x18),
        KeyCode::KeyY => Some(0x19),
        KeyCode::KeyZ => Some(0x1a),
        _ => None,
    }
}

fn redraw_soft_terminal(
    runtime: NonSend<TerminalRuntime>,
    mut soft_terminal: NonSendMut<SoftTerminal>,
    mut images: ResMut<Assets<Image>>,
) {
    let screen = runtime.parser.screen();
    let text = screen.contents();

    let _ = soft_terminal.terminal.draw(|frame| {
        let area = frame.area();
        frame.render_widget(
            Paragraph::new(text.as_str()).style(Style::default().fg(THEME_FG).bg(THEME_BG)),
            area,
        );
    });

    if let Some(handle) = soft_terminal.image_handle.as_ref()
        && let Some(image) = images.get_mut(handle)
    {
        image.data = Some(soft_terminal.terminal.backend().get_pixmap_data_as_rgba());
    }
}

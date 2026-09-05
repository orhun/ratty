//! Renders a PTY session with no window and writes the result to a PNG.
//!
//! Two modes exercise the PTY -> ratty-vt -> Ratatui -> `bevy_terminal` path
//! on the GPU without opening a window, for visual checks in headless
//! environments:
//!
//! - default: only the renderer-owned terminal texture is captured;
//! - `--scene`: Ratty's full presentation (2D present quad, inline images,
//!   RGP objects, cursor model) is rendered into an off-screen target sized
//!   like the window would be.
//!
//! ```text
//! cargo run --example headless_snapshot -- --out shot.png --after 3 -- sh -c 'ls --color; sleep 5'
//! cargo run --example headless_snapshot -- --scene --out shot.png -- ./widget/target/debug/examples/document
//! ```
//!
//! Requires a GPU (Metal/Vulkan/DX12).

use std::path::PathBuf;
use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::settings::RenderCreation;
use bevy::window::{PrimaryWindow, WindowResolution};
use bevy::winit::WinitPlugin;
use bevy_terminal_ratatui::TerminalRenderer;
use bevy_terminal_ratatui::prelude::{TerminalPlugin, TerminalReady, TerminalTexture};
use clap::Parser;

use ratty::config::AppConfig;
use ratty::inline::TerminalInlineObjects;
use ratty::mouse::TerminalSelection;
use ratty::runtime::{RuntimeOptions, TerminalRuntime};
use ratty::systems::drain_pty_output;
use ratty::terminal::{TerminalSurface, TerminalWidget, load_configured_font_faces};

#[derive(Parser)]
struct Args {
    /// Output PNG path.
    #[arg(long, default_value = "headless_snapshot.png")]
    out: PathBuf,
    /// Seconds of PTY output to collect before capturing.
    #[arg(long, default_value_t = 3.0)]
    after: f32,
    /// Render Ratty's full scene into a window-sized off-screen target
    /// instead of capturing only the terminal texture.
    #[arg(long)]
    scene: bool,
    /// Terminal columns (texture mode) or window width in pixels (scene mode).
    #[arg(long)]
    width: Option<u32>,
    /// Terminal rows (texture mode) or window height in pixels (scene mode).
    #[arg(long)]
    height: Option<u32>,
    /// Bytes to write to the PTY before capturing (`\x1b`, `\n`, `\r`,
    /// `\t` and `\\` escapes are decoded). May be repeated; each value is
    /// sent `--send-interval` seconds after the previous one, starting one
    /// second into the session, so intermediate frames are rendered.
    #[arg(long)]
    send: Vec<String>,
    /// Seconds between successive `--send` values.
    #[arg(long, default_value_t = 0.25)]
    send_interval: f32,
    /// At capture time, report cells whose Ratatui surface content differs
    /// from the VT screen (stale or missing cells in the retained surface).
    #[arg(long)]
    check_stale: bool,
    /// Config file to load (defaults to Ratty's normal lookup).
    #[arg(short = 'c', long)]
    config_file: Option<PathBuf>,
    /// Command to run in the PTY.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(Resource)]
struct Options {
    out: PathBuf,
    after: f32,
    send: Vec<Vec<u8>>,
    send_interval: f32,
    check_stale: bool,
}

/// Compares the retained Ratatui surface with the VT screen and logs every
/// cell whose visible symbol differs.
fn report_stale_cells(terminal: &TerminalSurface, runtime: &TerminalRuntime) {
    let screen = runtime.screen();
    let snapshot = terminal.tui.snapshot();
    let (rows, cols) = screen.size();
    let mut stale = 0;
    for row in 0..rows {
        for col in 0..cols {
            let expected = screen
                .cell(row, col)
                .map(|cell| {
                    if cell.is_wide_continuation() || !cell.has_contents() {
                        " ".to_string()
                    } else {
                        cell.contents().to_string()
                    }
                })
                .unwrap_or_else(|| " ".to_string());
            let actual = snapshot
                .cell((col, row))
                .map(|cell| {
                    if cell.is_continuation() {
                        " ".to_string()
                    } else {
                        cell.symbol().to_string()
                    }
                })
                .unwrap_or_default();
            let expected = if expected.starts_with('\u{10EEEE}') {
                " ".to_string()
            } else {
                expected
            };
            if expected.trim_end() != actual.trim_end() {
                stale += 1;
                if stale <= 40 {
                    warn!("stale cell row {row} col {col}: screen {expected:?} surface {actual:?}");
                }
            }
        }
    }
    info!("stale-cell check: {stale} mismatches");
    if std::env::var_os("RATTY_TRACE_FG").is_some() {
        for row in 0..rows {
            let mut runs = Vec::new();
            let mut current: Option<(u16, ratty::ratty_vt::Color, String)> = None;
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let color = cell.fgcolor();
                match &mut current {
                    Some((_, c, text)) if *c == color => text.push_str(cell.contents()),
                    _ => {
                        if let Some(run) = current.take() {
                            runs.push(run);
                        }
                        current = Some((col, color, cell.contents().to_string()));
                    }
                }
            }
            if let Some(run) = current.take() {
                runs.push(run);
            }
            let described: Vec<String> = runs
                .iter()
                .map(|(col, color, text)| {
                    format!(
                        "{col}:{color:?}:{:?}",
                        text.trim_end().chars().take(12).collect::<String>()
                    )
                })
                .collect();
            info!("row {row}: {}", described.join(" | "));
        }
    }
}

/// Decodes the `--send` escapes into raw PTY input bytes.
fn decode_input(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                out.push(u8::from_str_radix(&hex, 16).unwrap_or(b'?'));
            }
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            Some(other) => {
                let mut buf = [0; 4];
                out.push(b'\\');
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

/// Writes the `--send` values to the PTY in order, spaced by the interval.
fn send_input(
    time: Res<Time<Real>>,
    options: Res<Options>,
    runtime: Res<TerminalRuntime>,
    mut next: Local<usize>,
) {
    let Some(bytes) = options.send.get(*next) else {
        return;
    };
    let due = 1.0 + *next as f32 * options.send_interval;
    if time.elapsed_secs() < due {
        return;
    }
    *next += 1;
    runtime.write_input(bytes);
}

#[derive(Resource, Default)]
struct CaptureState {
    requested: bool,
    done: bool,
}

/// The image every camera renders into in scene mode.
#[derive(Resource)]
struct SceneTarget(Handle<Image>, UVec2);

#[derive(Component)]
struct Target;

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut app_config = AppConfig::load_from_path(args.config_file.as_deref())?;
    app_config.window.scale_factor = Some(1.0);
    if args.scene {
        app_config.window.width = args.width.unwrap_or(1200);
        app_config.window.height = args.height.unwrap_or(800);
    } else {
        app_config.terminal.default_cols = args.width.unwrap_or(100) as u16;
        app_config.terminal.default_rows = args.height.unwrap_or(30) as u16;
    }
    let runtime = TerminalRuntime::spawn(
        &app_config,
        &RuntimeOptions {
            command: Some(args.command.clone()),
            working_dir: Some(std::env::current_dir()?),
        },
    )?;
    let terminal = TerminalSurface::new(&app_config)?;
    let render_surface = terminal.tui.surface();
    let render_config = terminal.render_config().clone();

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: bevy::window::ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::default()),
                synchronous_pipeline_compilation: true,
                ..default()
            })
            .disable::<WinitPlugin>(),
    )
    .add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(8)))
    .insert_resource(ClearColor(Color::srgba_u8(
        app_config.theme.background[0],
        app_config.theme.background[1],
        app_config.theme.background[2],
        (app_config.window.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
    )))
    .init_resource::<CaptureState>()
    .insert_resource(Options {
        out: args.out,
        after: args.after,
        send: args.send.iter().map(|text| decode_input(text)).collect(),
        send_interval: args.send_interval,
        check_stale: args.check_stale,
    })
    .add_systems(Update, send_input);

    if args.scene {
        let size = UVec2::new(app_config.window.width, app_config.window.height);
        let font_faces = load_configured_font_faces(&mut app, &app_config.font)?;
        app.insert_resource(app_config)
            .insert_resource(runtime)
            .insert_resource(terminal)
            .insert_resource(font_faces)
            // Ratty's plugin expects a primary window. A `Window` entity with
            // no OS handle is ignored by the renderer but still provides the
            // logical size and scale the layout systems read.
            .add_systems(PreStartup, move |mut commands: Commands| {
                commands.spawn((
                    Window {
                        resolution: WindowResolution::new(size.x, size.y)
                            .with_scale_factor_override(1.0),
                        visible: false,
                        ..default()
                    },
                    PrimaryWindow,
                ));
            })
            .add_plugins(ratty::plugin::TerminalPlugin)
            .add_systems(PostStartup, retarget_cameras)
            .add_systems(Update, request_scene_capture);
    } else {
        app.add_plugins(TerminalPlugin)
            .insert_resource(app_config)
            .insert_resource(runtime)
            .insert_resource(terminal)
            .init_resource::<TerminalInlineObjects>()
            .init_resource::<TerminalSelection>()
            .add_systems(Startup, move |mut commands: Commands| {
                commands.spawn((
                    Target,
                    TerminalRenderer::new(render_surface.clone()),
                    render_config.clone(),
                ));
            })
            .add_observer(on_ready)
            .add_systems(Update, (pump_and_draw, request_texture_capture).chain());
    }
    app.add_systems(Update, exit_when_done);
    app.run();
    Ok(())
}

/// Points every camera at one off-screen image so the scene can be read back.
fn retarget_cameras(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    window: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<Entity, With<Camera>>,
) {
    let window = window.single().expect("primary window");
    let size = window.resolution.physical_size();
    let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let handle = images.add(image);
    for camera in &cameras {
        commands
            .entity(camera)
            .insert(RenderTarget::Image(handle.clone().into()));
    }
    commands.insert_resource(SceneTarget(handle, size));
}

fn on_ready(
    ready: On<TerminalReady>,
    textures: Query<&TerminalTexture>,
    mut terminal: ResMut<TerminalSurface>,
    mut runtime: ResMut<TerminalRuntime>,
) {
    let Ok(texture) = textures.get(ready.entity) else {
        return;
    };
    // Adopt measured metrics; keep the configured grid, just tell the child
    // its pixel size.
    terminal.update_render_output(texture);
    let logical =
        Vec2::new(terminal.cols as f32, terminal.rows as f32) * terminal.char_dimensions();
    let layout = terminal.resize_to_fit(logical, 1.0);
    let pixels = layout.pty_pixels();
    if let Err(error) = runtime.resize(layout.cols, layout.rows, pixels.x as u16, pixels.y as u16) {
        warn!("pty resize failed: {error:#}");
    }
    info!(
        "terminal ready: {}x{} cells, texture {:?}, cell {:?}",
        layout.cols, layout.rows, texture.size, texture.cell_size
    );
}

fn pump_and_draw(
    app_config: Res<AppConfig>,
    mut runtime: ResMut<TerminalRuntime>,
    mut inline_objects: ResMut<TerminalInlineObjects>,
    mut terminal: ResMut<TerminalSurface>,
    selection: Res<TerminalSelection>,
) {
    // Camera updates have no camera to go to in texture mode.
    let mut camera_updates = Vec::new();
    drain_pty_output(&mut runtime, &mut inline_objects, &mut camera_updates);

    let screen = runtime.screen();
    terminal.tui.draw(|frame| {
        frame.render_widget(
            TerminalWidget {
                screen,
                selection: &selection,
                theme: &app_config.theme,
                font_style: app_config.font.style,
            },
            frame.area(),
        );
        if !screen.cursor_hidden() {
            let (row, col) = screen.display_cursor_position();
            frame.set_cursor_position((col, row));
        }
    });
}

fn request_texture_capture(
    time: Res<Time<Real>>,
    options: Res<Options>,
    mut state: ResMut<CaptureState>,
    textures: Query<&TerminalTexture, With<Target>>,
    commands: Commands,
) {
    if state.requested || time.elapsed_secs() < options.after {
        return;
    }
    let Ok(texture) = textures.single() else {
        return;
    };
    state.requested = true;
    schedule_readback(commands, texture.image.clone(), texture.size);
}

fn request_scene_capture(
    time: Res<Time<Real>>,
    options: Res<Options>,
    mut state: ResMut<CaptureState>,
    target: Option<Res<SceneTarget>>,
    terminal: Res<TerminalSurface>,
    runtime: Res<TerminalRuntime>,
    commands: Commands,
) {
    if state.requested || time.elapsed_secs() < options.after {
        return;
    }
    let Some(target) = target else {
        return;
    };
    state.requested = true;
    if options.check_stale {
        report_stale_cells(&terminal, &runtime);
    }
    schedule_readback(commands, target.0.clone(), target.1);
}

fn schedule_readback(mut commands: Commands, image: Handle<Image>, size: UVec2) {
    commands.spawn(Readback::texture(image)).observe(
        move |done: On<ReadbackComplete>,
              options: Res<Options>,
              mut state: ResMut<CaptureState>,
              mut frames: Local<u32>| {
            // The scene reaches the GPU a couple of frames after the
            // readback is scheduled.
            *frames += 1;
            if *frames < 4 || state.done {
                return;
            }
            state.done = true;
            write_png(&options.out, &done.data, size);
        },
    );
}

fn exit_when_done(state: Res<CaptureState>, mut exit: MessageWriter<AppExit>) {
    if state.done {
        exit.write(AppExit::Success);
    }
}

/// Writes padded RGBA8 readback rows as a PNG.
fn write_png(path: &PathBuf, data: &[u8], size: UVec2) {
    let unpadded = size.x as usize * 4;
    let stride = data.len() / size.y as usize;
    let mut pixels = Vec::with_capacity(unpadded * size.y as usize);
    for row in 0..size.y as usize {
        pixels.extend_from_slice(&data[row * stride..row * stride + unpadded]);
    }
    match image::RgbaImage::from_raw(size.x, size.y, pixels) {
        Some(image) => match image.save(path) {
            Ok(()) => info!("wrote {}", path.display()),
            Err(error) => error!("failed to write {}: {error}", path.display()),
        },
        None => error!("readback size mismatch for {:?}", size),
    }
}

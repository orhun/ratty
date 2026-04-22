use std::sync::mpsc::TryRecvError;

use bevy::app::AppExit;
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::prelude::*;
use bevy::render::render_resource::Extent3d;
use bevy::window::{PrimaryWindow, WindowResized};

use crate::config::CURSOR_DEPTH;
use crate::config::CURSOR_SCALE_FACTOR;
use crate::model::CursorModel;
use crate::model::spawn_cursor_model;
use crate::mouse::TerminalSelection;
use crate::runtime::TerminalRuntime;
use crate::scene::{ModelLoadState, TerminalSprite, TerminalViewport};
use crate::terminal::{TerminalSurface, TerminalWidget};

pub fn pump_pty_output(
    mut runtime: NonSendMut<TerminalRuntime>,
    mut app_exit: MessageWriter<AppExit>,
) {
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

pub fn redraw_soft_terminal(
    runtime: NonSend<TerminalRuntime>,
    mut terminal: NonSendMut<TerminalSurface>,
    selection: Res<TerminalSelection>,
    mut images: ResMut<Assets<Image>>,
    mut model_load_state: ResMut<ModelLoadState>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let screen = runtime.parser.screen();

    let _ = terminal.tui.draw(|frame| {
        frame.render_widget(
            TerminalWidget {
                screen,
                selection: &selection,
            },
            frame.area(),
        );
    });

    if let Some(handle) = terminal.image_handle.as_ref()
        && let Some(image) = images.get_mut(handle)
    {
        image.data = Some(terminal.tui.backend().get_pixmap_data_as_rgba());

        if !model_load_state.loaded {
            spawn_cursor_model(&mut commands, &mut meshes, &mut materials);
            model_load_state.loaded = true;
        }
    }
}

pub fn handle_window_resize(
    mut resize_events: MessageReader<WindowResized>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    mut runtime: NonSendMut<TerminalRuntime>,
    mut terminal: NonSendMut<TerminalSurface>,
    mut viewport: ResMut<TerminalViewport>,
    mut sprite_query: Query<&mut Sprite, With<TerminalSprite>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Ok(primary_window) = primary_window.single() else {
        return;
    };

    let mut latest_size = None;
    for event in resize_events.read() {
        if event.window == primary_window {
            latest_size = Some(Vec2::new(event.width, event.height));
        }
    }

    let Some(window_size) = latest_size else {
        return;
    };

    let viewport_size = Vec2::new(window_size.x.max(1.0), window_size.y.max(1.0));
    viewport.size = viewport_size;
    viewport.center = Vec2::ZERO;

    let char_dims = UVec2::new(
        terminal.tui.backend().char_width as u32,
        terminal.tui.backend().char_height as u32,
    )
    .max(UVec2::ONE);
    let cols = ((viewport_size.x / char_dims.x as f32).floor() as u16).max(1);
    let rows = ((viewport_size.y / char_dims.y as f32).floor() as u16).max(1);

    runtime.resize(cols, rows);
    terminal.resize(cols, rows);

    if let Some(handle) = terminal.image_handle.as_ref()
        && let Some(image) = images.get_mut(handle)
    {
        image.resize(Extent3d {
            width: terminal.tui.backend().get_pixmap_width() as u32,
            height: terminal.tui.backend().get_pixmap_height() as u32,
            depth_or_array_layers: 1,
        });
        image.data = Some(terminal.tui.backend().get_pixmap_data_as_rgba());
    }

    for mut sprite in &mut sprite_query {
        sprite.custom_size = Some(viewport_size);
    }
}

pub fn sync_asset_to_terminal_cursor(
    runtime: NonSend<TerminalRuntime>,
    terminal: NonSend<TerminalSurface>,
    viewport: Res<TerminalViewport>,
    time: Res<Time>,
    mut query: Query<(&mut Transform, &mut Visibility), With<CursorModel>>,
) {
    let cols = terminal.cols.max(1) as f32;
    let rows = terminal.rows.max(1) as f32;
    let cell_width = viewport.size.x / cols;
    let cell_height = viewport.size.y / rows;
    let scale = cell_width.min(cell_height) * CURSOR_SCALE_FACTOR;

    let screen = runtime.parser.screen();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let cursor_col = cursor_col.min(terminal.cols.saturating_sub(1)) as f32;
    let cursor_row = cursor_row.min(terminal.rows.saturating_sub(1)) as f32;

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

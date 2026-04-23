use std::sync::mpsc::TryRecvError;

use bevy::app::AppExit;
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowResized};

use crate::config::CURSOR_DEPTH;
use crate::config::CURSOR_PLANE_OFFSET;
use crate::config::CURSOR_SCALE_FACTOR;
use crate::model::CursorModel;
use crate::model::spawn_cursor_model;
use crate::mouse::TerminalSelection;
use crate::scene::{
    ModelLoadState, TerminalPlane, TerminalPlaneBack, TerminalPresentation,
    TerminalPresentationMode, TerminalSprite, TerminalViewport,
};
use crate::runtime::TerminalRuntime;
use crate::terminal::{TerminalRedrawState, TerminalSurface, TerminalWidget};

pub fn pump_pty_output(
    mut runtime: NonSendMut<TerminalRuntime>,
    mut app_exit: MessageWriter<AppExit>,
    mut redraw: ResMut<TerminalRedrawState>,
) {
    let mut processed_output = false;
    loop {
        match runtime.rx.try_recv() {
            Ok(chunk) => {
                runtime.parser.process(&chunk);
                processed_output = true;
            }
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

    if processed_output {
        redraw.request();
    }
}

pub fn redraw_soft_terminal(
    runtime: NonSend<TerminalRuntime>,
    mut terminal: NonSendMut<TerminalSurface>,
    selection: Res<TerminalSelection>,
    presentation: Res<TerminalPresentation>,
    mut redraw: ResMut<TerminalRedrawState>,
    mut images: ResMut<Assets<Image>>,
    mut model_load_state: ResMut<ModelLoadState>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    plane_materials: Query<&MeshMaterial3d<StandardMaterial>, With<TerminalPlane>>,
) {
    let needs_redraw = redraw.take();
    let force_live_redraw = presentation.mode == TerminalPresentationMode::Plane3d;
    if !needs_redraw && !force_live_redraw && model_load_state.loaded {
        return;
    }

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

    terminal.sync_image(&mut images);

    if let Some(image_handle) = terminal.image_handle.as_ref() {
        for material_handle in &plane_materials {
            if let Some(material) = materials.get_mut(&material_handle.0) {
                material.base_color_texture = Some(image_handle.clone());
            }
        }
    }

    if !model_load_state.loaded {
        spawn_cursor_model(&mut commands, &mut meshes, &mut materials);
        model_load_state.loaded = true;
    }
}

pub fn handle_window_resize(
    mut resize_events: MessageReader<WindowResized>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    mut runtime: NonSendMut<TerminalRuntime>,
    mut terminal: NonSendMut<TerminalSurface>,
    mut redraw: ResMut<TerminalRedrawState>,
    mut viewport: ResMut<TerminalViewport>,
    mut sprite_query: Query<&mut Sprite, With<TerminalSprite>>,
    mut plane_query: Query<&mut Transform, (With<TerminalPlane>, Without<TerminalSprite>)>,
    mut plane_back_query: Query<
        &mut Transform,
        (
            With<TerminalPlaneBack>,
            Without<TerminalPlane>,
            Without<TerminalSprite>,
        ),
    >,
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
    terminal.sync_image(&mut images);
    redraw.request();

    for mut sprite in &mut sprite_query {
        sprite.custom_size = Some(viewport_size);
    }

    for mut transform in &mut plane_query {
        transform.scale = viewport_size.extend(1.0);
    }

    for mut transform in &mut plane_back_query {
        transform.scale = viewport_size.extend(1.0);
    }
}

pub fn sync_asset_to_terminal_cursor(
    runtime: NonSend<TerminalRuntime>,
    terminal: NonSend<TerminalSurface>,
    viewport: Res<TerminalViewport>,
    presentation: Res<TerminalPresentation>,
    time: Res<Time>,
    plane_query: Query<&Transform, (With<TerminalPlane>, Without<CursorModel>)>,
    mut query: Query<
        (&mut Transform, &mut Visibility),
        (With<CursorModel>, Without<TerminalPlane>),
    >,
) {
    let (translation, rotation, scale, cursor_visibility) =
        cursor_pose(&runtime, &terminal, &viewport, presentation.mode, time.elapsed_secs(), &plane_query);
    for (mut transform, mut visibility) in &mut query {
        transform.translation = translation;
        transform.rotation = rotation;
        transform.scale = Vec3::splat(scale.max(0.001));
        *visibility = cursor_visibility;
    }
}

fn cursor_pose(
    runtime: &TerminalRuntime,
    terminal: &TerminalSurface,
    viewport: &TerminalViewport,
    mode: TerminalPresentationMode,
    elapsed_secs: f32,
    plane_query: &Query<&Transform, (With<TerminalPlane>, Without<CursorModel>)>,
) -> (Vec3, Quat, f32, Visibility) {
    let cols = terminal.cols.max(1) as f32;
    let rows = terminal.rows.max(1) as f32;
    let cell_width = viewport.size.x / cols;
    let cell_height = viewport.size.y / rows;
    let scale = cell_width.min(cell_height) * CURSOR_SCALE_FACTOR;

    let screen = runtime.parser.screen();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let cursor_col = cursor_col.min(terminal.cols.saturating_sub(1)) as f32;
    let cursor_row = cursor_row.min(terminal.rows.saturating_sub(1)) as f32;

    let local_x = viewport.center.x - viewport.size.x * 0.5 + (cursor_col + 0.5) * cell_width;
    let local_y = viewport.center.y + viewport.size.y * 0.5 - (cursor_row + 0.5) * cell_height;
    let spin = elapsed_secs * 1.4;
    let bob = (elapsed_secs * 2.2).sin() * cell_height * 0.08;

    let (translation, rotation, visibility) = match mode {
        TerminalPresentationMode::Flat2d => (
            Vec3::new(local_x, local_y + bob, CURSOR_DEPTH),
            Quat::from_rotation_y(spin) * Quat::from_rotation_x(-0.25),
            if screen.hide_cursor() {
                Visibility::Hidden
            } else {
                Visibility::Visible
            },
        ),
        TerminalPresentationMode::Plane3d => {
            let plane_transform = plane_query
                .single()
                .expect("terminal plane should exist while app is running");
            let plane_local_x = (cursor_col + 0.5) / cols - 0.5;
            let plane_local_y = 0.5 - (cursor_row + 0.5) / rows;
            let local_position = Vec3::new(plane_local_x, plane_local_y, CURSOR_PLANE_OFFSET);
            (
                plane_transform.transform_point(local_position),
                plane_transform.rotation
                    * (Quat::from_rotation_y(spin) * Quat::from_rotation_x(-0.25)),
                Visibility::Visible,
            )
        }
    };

    (translation, rotation, scale, visibility)
}

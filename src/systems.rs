use std::sync::mpsc::TryRecvError;

use bevy::app::AppExit;
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::PrimitiveTopology;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use ratatui::style::Color as TuiColor;
use bevy::window::{PrimaryWindow, WindowResized};

use crate::config::{AppConfig, CURSOR_DEPTH};
use crate::inline::{
    InlineObject, TerminalInlineObjectPlane, TerminalInlineObjectSprite, TerminalInlineObjects,
};
use crate::model::CursorModel;
use crate::model::spawn_cursor_model;
use crate::mouse::TerminalSelection;
use crate::rendering::{sync_plane_texture, sync_terminal_debug_image};
use crate::runtime::TerminalRuntime;
use crate::scene::{
    ModelLoadState, TerminalPlane, TerminalPlaneBack, TerminalPlaneMeshes, TerminalPlaneWarp,
    TerminalPresentation, TerminalPresentationMode, TerminalSprite, TerminalViewport,
};
use crate::terminal::{TerminalRedrawState, TerminalSurface, TerminalWidget};

pub fn pump_pty_output(
    mut runtime: NonSendMut<TerminalRuntime>,
    mut inline_objects: ResMut<TerminalInlineObjects>,
    mut app_exit: MessageWriter<AppExit>,
    mut redraw: ResMut<TerminalRedrawState>,
) {
    let mut processed_output = false;
    loop {
        match runtime.rx.try_recv() {
            Ok(chunk) => {
                let prev_rows: Option<Vec<String>> = if !inline_objects.anchors.is_empty() {
                    let (_, cols) = runtime.parser.screen().size();
                    Some(
                        runtime
                            .parser
                            .screen()
                            .rows(0, cols)
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                };
                inline_objects.consume_pty_output(&chunk, &mut runtime.parser);
                if let Some(prev_rows) = prev_rows {
                    let (_, cols) = runtime.parser.screen().size();
                    let next_rows = runtime.parser.screen().rows(0, cols).collect::<Vec<_>>();
                    let scrolled = infer_upward_scroll(&prev_rows, &next_rows);
                    inline_objects.apply_scroll(scrolled);
                }
                inline_objects.refresh_placeholder_anchors(runtime.parser.screen());
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

fn infer_upward_scroll(prev_rows: &[String], next_rows: &[String]) -> u16 {
    let max_shift = prev_rows.len().min(next_rows.len());
    for shift in (1..max_shift).rev() {
        if prev_rows
            .iter()
            .skip(shift)
            .zip(next_rows.iter())
            .all(|(prev, next)| prev == next)
        {
            return shift as u16;
        }
    }
    0
}

pub fn sync_inline_objects(
    mut commands: Commands,
    mut inline_objects: ResMut<TerminalInlineObjects>,
    terminal: NonSend<TerminalSurface>,
    viewport: Res<TerminalViewport>,
    presentation: Res<TerminalPresentation>,
    plane_warp: Res<TerminalPlaneWarp>,
    time: Res<Time>,
    plane_query: Query<Entity, With<TerminalPlane>>,
    sprite_query: Query<Entity, With<TerminalInlineObjectSprite>>,
    plane_image_query: Query<Entity, With<TerminalInlineObjectPlane>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let force_warp_sync = presentation.mode == TerminalPresentationMode::Plane3d
        && plane_warp.amount > 0.0
        && !inline_objects.anchors.is_empty();
    if !force_warp_sync && !inline_objects.needs_sync(viewport.size, terminal.cols, terminal.rows) {
        return;
    }

    for entity in &sprite_query {
        commands.entity(entity).despawn();
    }
    for entity in &plane_image_query {
        commands.entity(entity).despawn();
    }

    let Ok(plane_entity) = plane_query.single() else {
        return;
    };

    let cell_width = viewport.size.x / terminal.cols.max(1) as f32;
    let cell_height = viewport.size.y / terminal.rows.max(1) as f32;
    let elapsed_secs = time.elapsed_secs();
    let renderable_ids = inline_objects
        .anchors
        .iter()
        .filter_map(|(object_id, anchor)| {
            inline_objects.objects.get(object_id)?;
            let start = anchor.row as i32;
            let end = start + anchor.rows as i32;
            (start < terminal.rows as i32 && end > 0).then_some(*object_id)
        })
        .collect::<Vec<_>>();

    let mut plane_children = Vec::new();
    for object_id in renderable_ids {
        let image_handle = {
            let object = inline_objects
                .objects
                .get_mut(&object_id)
                .expect("inline object should exist");
            match object {
                InlineObject::KittyImage(object) => {
                    if let Some(handle) = object.raster.handle.as_ref() {
                        handle.clone()
                    } else {
                        let mut image = Image::new_fill(
                            Extent3d {
                                width: object.raster.width,
                                height: object.raster.height,
                                depth_or_array_layers: 1,
                            },
                            TextureDimension::D2,
                            &[0, 0, 0, 0],
                            TextureFormat::Rgba8UnormSrgb,
                            bevy::asset::RenderAssetUsages::default(),
                        );
                        image.sampler = ImageSampler::nearest();
                        image.data = Some(object.raster.rgba.clone());
                        let handle = images.add(image);
                        object.raster.handle = Some(handle.clone());
                        handle
                    }
                }
            }
        };
        let anchor = inline_objects
            .anchors
            .get(&object_id)
            .expect("inline object anchor should exist");
        let columns = anchor.columns;
        let rows = anchor.rows;
        let sprite_size = Vec2::new(columns as f32 * cell_width, rows as f32 * cell_height);
        let center_x = viewport.center.x - viewport.size.x * 0.5
            + (anchor.col as f32 + columns as f32 * 0.5) * cell_width;
        let center_y = viewport.center.y + viewport.size.y * 0.5
            - (anchor.row as f32 + rows as f32 * 0.5) * cell_height;

        let mut sprite = Sprite::from_image(image_handle.clone());
        sprite.custom_size = Some(sprite_size);
        commands.spawn((
            TerminalInlineObjectSprite,
            sprite,
            Transform::from_translation(Vec3::new(center_x, center_y, 5.0)),
            match presentation.mode {
                TerminalPresentationMode::Flat2d => Visibility::Visible,
                TerminalPresentationMode::Plane3d => Visibility::Hidden,
            },
        ));

        let local_width = columns as f32 / terminal.cols.max(1) as f32;
        let local_height = rows as f32 / terminal.rows.max(1) as f32;
        let local_x = (anchor.col as f32 + columns as f32 * 0.5) / terminal.cols.max(1) as f32
            - 0.5;
        let local_y = 0.5 - (anchor.row as f32 + rows as f32 * 0.5) / terminal.rows.max(1) as f32;
        let x_segments = columns.clamp(2, 24);
        let y_segments = rows.clamp(2, 24);
        let vertex_count = ((x_segments + 1) * (y_segments + 1)) as usize;
        let mut positions = Vec::with_capacity(vertex_count);
        let mut normals = Vec::with_capacity(vertex_count);
        let mut uvs = Vec::with_capacity(vertex_count);
        let mut indices = Vec::with_capacity((x_segments * y_segments * 6) as usize);

        for y in 0..=y_segments {
            let v = y as f32 / y_segments as f32;
            let py = local_y + (0.5 - v) * local_height;
            for x in 0..=x_segments {
                let u = x as f32 / x_segments as f32;
                let px = local_x + (u - 0.5) * local_width;
                positions.push([
                    px,
                    py,
                    plane_surface_z(px, py, plane_warp.amount, elapsed_secs) + 1.5,
                ]);
                normals.push([0.0, 0.0, 1.0]);
                uvs.push([u, v]);
            }
        }

        for y in 0..y_segments {
            for x in 0..x_segments {
                let row = y * (x_segments + 1);
                let next_row = (y + 1) * (x_segments + 1);
                let i0 = row + x;
                let i1 = i0 + 1;
                let i2 = next_row + x;
                let i3 = i2 + 1;
                indices.extend_from_slice(&[i0, i2, i1, i1, i2, i3]);
            }
        }

        let mesh = meshes.add(
            Mesh::new(PrimitiveTopology::TriangleList, bevy::asset::RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
                .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
                .with_inserted_indices(Indices::U32(indices)),
        );
        let plane_child = commands
            .spawn((
                TerminalInlineObjectPlane,
                Mesh3d(mesh),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    base_color_texture: Some(image_handle),
                    alpha_mode: AlphaMode::Blend,
                    unlit: true,
                    ..default()
                })),
                Transform::default(),
            ))
            .id();
        plane_children.push(plane_child);
    }

    if !plane_children.is_empty() {
        commands.entity(plane_entity).add_children(&plane_children);
    }

    inline_objects.finish_sync(viewport.size, terminal.cols, terminal.rows);
}

pub fn apply_inline_objects(
    presentation: Res<TerminalPresentation>,
    mut sprite_query: Query<&mut Visibility, With<TerminalInlineObjectSprite>>,
) {
    let visibility = match presentation.mode {
        TerminalPresentationMode::Flat2d => Visibility::Visible,
        TerminalPresentationMode::Plane3d => Visibility::Hidden,
    };

    for mut sprite_visibility in &mut sprite_query {
        *sprite_visibility = visibility;
    }
}

pub fn redraw_soft_terminal(
    app_config: Res<AppConfig>,
    runtime: NonSend<TerminalRuntime>,
    mut terminal: NonSendMut<TerminalSurface>,
    selection: Res<TerminalSelection>,
    presentation: Res<TerminalPresentation>,
    time: Res<Time>,
    mut redraw: ResMut<TerminalRedrawState>,
    mut images: ResMut<Assets<Image>>,
    mut model_load_state: ResMut<ModelLoadState>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    plane_materials: Query<&MeshMaterial3d<StandardMaterial>, With<TerminalPlane>>,
    plane_back_materials: Query<&MeshMaterial3d<StandardMaterial>, With<TerminalPlaneBack>>,
) {
    let needs_redraw = redraw.take();
    let force_live_redraw = presentation.mode == TerminalPresentationMode::Plane3d;
    if !needs_redraw && !force_live_redraw && model_load_state.loaded {
        return;
    }

    let screen = runtime.parser.screen();
    let theme_fg = TuiColor::Rgb(
        app_config.theme.foreground[0],
        app_config.theme.foreground[1],
        app_config.theme.foreground[2],
    );
    let theme_bg = TuiColor::Rgb(
        app_config.theme.background[0],
        app_config.theme.background[1],
        app_config.theme.background[2],
    );
    let _ = terminal.tui.draw(|frame| {
        frame.render_widget(
            TerminalWidget {
                screen,
                selection: &selection,
                theme_fg,
                theme_bg,
                font_style: app_config.font.style,
            },
            frame.area(),
        );

        if !app_config.cursor.model.visible && !screen.hide_cursor() {
            let (cursor_row, cursor_col) = screen.cursor_position();
            frame.set_cursor_position((cursor_col, cursor_row));
        }
    });

    let _ = terminal.sync_image(&mut images, time.elapsed_secs());
    sync_terminal_debug_image(&terminal, &mut images, screen);

    sync_plane_texture(
        terminal.image_handle.as_ref(),
        &plane_materials,
        &mut materials,
    );
    sync_plane_texture(
        terminal.back_image_handle.as_ref(),
        &plane_back_materials,
        &mut materials,
    );

    if !model_load_state.first_frame_uploaded {
        model_load_state.first_frame_uploaded = true;
        redraw.request();
        return;
    }

    if !model_load_state.loaded {
        spawn_cursor_model(&mut commands, &mut meshes, &mut materials, &app_config);
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

    let char_dims = terminal.char_dimensions().max(UVec2::ONE);
    let cols = ((viewport_size.x / char_dims.x as f32).floor() as u16).max(1);
    let rows = ((viewport_size.y / char_dims.y as f32).floor() as u16).max(1);

    runtime.resize(cols, rows);
    terminal.resize(cols, rows);
    let _ = terminal.sync_image(&mut images, 0.0);
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
    app_config: Res<AppConfig>,
    runtime: NonSend<TerminalRuntime>,
    terminal: NonSend<TerminalSurface>,
    viewport: Res<TerminalViewport>,
    presentation: Res<TerminalPresentation>,
    plane_warp: Res<TerminalPlaneWarp>,
    time: Res<Time>,
    plane_query: Query<&Transform, (With<TerminalPlane>, Without<CursorModel>)>,
    mut query: Query<
        (&mut Transform, &mut Visibility),
        (With<CursorModel>, Without<TerminalPlane>),
    >,
) {
    let (translation, rotation, scale, cursor_visibility) = cursor_pose(
        &app_config,
        &runtime,
        &terminal,
        &viewport,
        presentation.mode,
        plane_warp.amount,
        time.elapsed_secs(),
        &plane_query,
    );
    for (mut transform, mut visibility) in &mut query {
        transform.translation = translation;
        transform.rotation = rotation;
        transform.scale = Vec3::splat(scale.max(0.001));
        *visibility = cursor_visibility;
    }
}

fn cursor_pose(
    app_config: &AppConfig,
    runtime: &TerminalRuntime,
    terminal: &TerminalSurface,
    viewport: &TerminalViewport,
    mode: TerminalPresentationMode,
    plane_warp_amount: f32,
    elapsed_secs: f32,
    plane_query: &Query<&Transform, (With<TerminalPlane>, Without<CursorModel>)>,
) -> (Vec3, Quat, f32, Visibility) {
    let cols = terminal.cols.max(1) as f32;
    let rows = terminal.rows.max(1) as f32;
    let cell_width = viewport.size.x / cols;
    let cell_height = viewport.size.y / rows;
    let scale = cell_width.min(cell_height) * app_config.cursor.model.scale_factor;

    let screen = runtime.parser.screen();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let cursor_col = cursor_col.min(terminal.cols.saturating_sub(1)) as f32;
    let cursor_row = cursor_row.min(terminal.rows.saturating_sub(1)) as f32;

    let cursor_x = cursor_col + 0.5 + app_config.cursor.model.x_offset;
    let local_x = viewport.center.x - viewport.size.x * 0.5 + cursor_x * cell_width;
    let local_y = viewport.center.y + viewport.size.y * 0.5 - (cursor_row + 0.5) * cell_height;
    let spin = elapsed_secs * app_config.cursor.animation.spin_speed;
    let bob = (elapsed_secs * app_config.cursor.animation.bob_speed).sin()
        * cell_height
        * app_config.cursor.animation.bob_amplitude;

    let (translation, rotation, visibility) = match mode {
        TerminalPresentationMode::Flat2d => (
            Vec3::new(local_x, local_y + bob, CURSOR_DEPTH),
            Quat::from_rotation_y(spin) * Quat::from_rotation_x(-0.25),
            if !app_config.cursor.model.visible || screen.hide_cursor() {
                Visibility::Hidden
            } else {
                Visibility::Visible
            },
        ),
        TerminalPresentationMode::Plane3d => {
            let plane_transform = plane_query
                .single()
                .expect("terminal plane should exist while app is running");
            let plane_local_x = cursor_x / cols - 0.5;
            let plane_local_y = 0.5 - (cursor_row + 0.5) / rows;
            let surface_z =
                plane_surface_z(plane_local_x, plane_local_y, plane_warp_amount, elapsed_secs);
            let local_position = Vec3::new(
                plane_local_x,
                plane_local_y,
                surface_z + app_config.cursor.model.plane_offset,
            );
            (
                plane_transform.transform_point(local_position),
                plane_transform.rotation
                    * (Quat::from_rotation_y(spin) * Quat::from_rotation_x(-0.25)),
                if app_config.cursor.model.visible {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                },
            )
        }
    };

    (translation, rotation, scale, visibility)
}

fn plane_surface_z(local_x: f32, local_y: f32, warp_amount: f32, elapsed_secs: f32) -> f32 {
    if warp_amount <= 0.0 {
        return 0.0;
    }

    let pulse = warp_amount * (0.96 + 0.04 * (elapsed_secs * 2.2).sin());
    let radius = (local_x * local_x + local_y * local_y).sqrt();
    let core = (-radius * 9.0).exp();
    let ring = (-(radius - 0.22).powi(2) * 18.0).exp();
    -(core * 360.0 + ring * 72.0) * pulse
}

pub fn animate_terminal_plane_warp(
    time: Res<Time>,
    warp: Res<TerminalPlaneWarp>,
    plane_meshes: Res<TerminalPlaneMeshes>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !warp.is_changed() && warp.amount == 0.0 {
        return;
    }

    let pulse = warp.amount * (0.96 + 0.04 * (time.elapsed_secs() * 2.2).sin());
    apply_plane_warp(meshes.get_mut(&plane_meshes.front), pulse, -1.0);
    apply_plane_warp(meshes.get_mut(&plane_meshes.back), pulse, 1.0);
}

fn apply_plane_warp(mesh: Option<&mut Mesh>, pulse: f32, direction: f32) {
    let Some(mesh) = mesh else {
        return;
    };
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    else {
        return;
    };

    for position in positions.iter_mut() {
        let x = position[0];
        let y = position[1];
        let radius = (x * x + y * y).sqrt();
        let displacement = if pulse > 0.0 {
            let core = (-radius * 9.0).exp();
            let ring = (-(radius - 0.22).powi(2) * 18.0).exp();
            (core * 360.0 + ring * 72.0) * pulse
        } else {
            0.0
        };
        position[2] = displacement * direction;
    }
}

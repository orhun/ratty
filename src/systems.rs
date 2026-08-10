//! Runtime Bevy systems for terminal presentation.
//!
//! These systems are scheduled from [`crate::plugin::TerminalPlugin`] in a mostly linear flow:
//!
//! - `pump_pty_output`
//! - `keyboard::handle_keyboard_input`
//! - `mouse::handle_mouse_input`
//! - `handle_window_resize`
//! - `scene::apply_terminal_presentation`
//! - `apply_inline_objects`
//! - `render_terminal_widget`
//! - `sync_inline_objects`
//! - `animate_inline_kitty_planes`
//! - `sync_rgp_objects`
//! - `apply_instance_brightness`
//! - `animate_terminal_plane_warp`
//! - `sync_asset_to_terminal_cursor`
//!
//! The redraw path updates the terminal texture and presentation state first, then the inline
//! object systems rebuild or reposition scene entities that depend on the terminal grid.

use std::collections::HashMap;
use std::sync::mpsc::TryRecvError;

use crate::camera::{
    MIN_ORTHOGRAPHIC_SCALE, TerminalCameraSlots, TerminalCameraUpdate, TerminalMobiusSource,
};
use crate::config::{AppConfig, CURSOR_DEPTH};
use crate::direct_render::DirectTerminalSceneExchange;
use crate::inline::{
    InlineKittyPlaneLayout, KittyPlaneCache, TerminalInlineObjectPlane, TerminalInlineObjectSprite,
    TerminalInlineObjects, TerminalRgpObject,
};
use crate::kitty::{KittyPlacementView, PlacementKey};
use crate::model::CursorModel;
use crate::model::spawn_cursor_model;
use crate::mouse::TerminalSelection;
use crate::present::TerminalPresentMaterial;
use crate::rendering::{sync_plane_texture, sync_terminal_debug_image};
use crate::runtime::TerminalRuntime;
use crate::scene::{
    MobiusTransition, ModelLoadState, TerminalPlane, TerminalPlaneBack,
    TerminalPlaneBackLayoutQuery, TerminalPlaneLayoutQuery, TerminalPlaneMeshes, TerminalPlaneWarp,
    TerminalPresentationMode, TerminalViewport, sync_terminal_layout,
};
use crate::terminal::{
    TerminalRedrawState, TerminalSurface, TerminalWidget, render_scale_for_window,
};
use crate::vt;
use bevy::app::AppExit;
use bevy::asset::AssetMut;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::ecs::system::SystemParam;
use bevy::gltf::GltfAssetLabel;
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::PrimitiveTopology;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::{PrimaryWindow, Window, WindowCloseRequested, WindowResized};

struct InlineLayout {
    columns: f32,
    rows: f32,
    center_x: f32,
    center_y: f32,
    local_x: f32,
    local_y: f32,
    local_width: f32,
    local_height: f32,
    pixel_width: f32,
    pixel_height: f32,
}

struct KittyRenderContext<'a> {
    mode: TerminalPresentationMode,
    mobius_progress: f32,
    warp_amount: f32,
    elapsed_secs: f32,
    materials: &'a mut Assets<StandardMaterial>,
    images: &'a mut Assets<Image>,
    meshes: &'a mut Assets<Mesh>,
    plane_children: &'a mut Vec<Entity>,
}

struct CursorPoseContext<'a, 'w, 's> {
    runtime: &'a TerminalRuntime,
    terminal: &'a TerminalSurface,
    viewport: &'a TerminalViewport,
    mode: TerminalPresentationMode,
    plane_warp_amount: f32,
    mobius_progress: f32,
    elapsed_secs: f32,
    plane_query: &'a Query<'w, 's, &'static Transform, (With<TerminalPlane>, Without<CursorModel>)>,
}

/// Marker for objects that already had instance brightness applied.
#[derive(Component)]
pub struct BrightnessAdjusted;

type PlaneTransformQuery<'w, 's> =
    Query<'w, 's, &'static Transform, (With<TerminalPlane>, Without<TerminalRgpObject>)>;
type CursorTransformQuery<'w, 's> = Query<
    'w,
    's,
    (&'static mut Transform, &'static mut Visibility),
    (With<CursorModel>, Without<TerminalPlane>),
>;

/// Requests application exit as soon as the primary window is asked to close.
pub(crate) fn request_exit_on_primary_window_close(
    mut close_events: MessageReader<WindowCloseRequested>,
    primary_window: Query<Entity, With<PrimaryWindow>>,
    mut app_exit: MessageWriter<AppExit>,
    mut exit_requested: Local<bool>,
) {
    if *exit_requested {
        close_events.clear();
        return;
    }

    let Ok(primary_window) = primary_window.single() else {
        return;
    };

    if close_events
        .read()
        .any(|event| event.window == primary_window)
    {
        *exit_requested = true;
        app_exit.write(AppExit::Success);
    }
}

/// Shuts down the PTY runtime when Bevy begins exiting.
pub(crate) fn shutdown_terminal_runtime_on_exit(
    mut app_exit: MessageReader<AppExit>,
    mut runtime: ResMut<TerminalRuntime>,
    mut shutdown_started: Local<bool>,
) {
    if *shutdown_started {
        app_exit.clear();
        return;
    }

    if app_exit.read().next().is_some() {
        *shutdown_started = true;
        runtime.shutdown();
    }
}

/// Pumps PTY output into the terminal parser.
///
/// This runs early in the update schedule, before `render_terminal_widget`. It drains PTY output
/// from [`TerminalRuntime`], feeds it through [`TerminalInlineObjects::consume_pty_output`] and
/// requests a redraw through [`TerminalRedrawState`] when terminal state changed.
///
/// It also updates scroll-coupled inline anchors before the redraw and sync passes rebuild the
/// scene.
pub fn pump_pty_output(
    mut runtime: ResMut<TerminalRuntime>,
    mut inline_objects: ResMut<TerminalInlineObjects>,
    mut camera_update_writer: MessageWriter<TerminalCameraUpdate>,
    mut app_exit: MessageWriter<AppExit>,
    mut redraw: ResMut<TerminalRedrawState>,
) {
    let mut processed_output = false;
    let mut camera_updates = Vec::new();
    loop {
        match runtime.try_recv() {
            Ok(chunk) => {
                let track_scroll = inline_objects.has_scroll_tracked_anchors();
                let prev_rows = track_scroll.then(|| vt::visible_row_texts(&runtime.term));
                let mut replies = inline_objects.consume_pty_output(
                    &chunk,
                    &mut runtime,
                    &mut camera_updates,
                    &mut processed_output,
                );
                for update in camera_updates.drain(..) {
                    camera_update_writer.write(update);
                }
                replies.extend(runtime.take_replies());
                for reply in replies {
                    runtime.write_input(&reply);
                }
                if let Some(prev_rows) = prev_rows {
                    let next_rows = vt::visible_row_texts(&runtime.term);
                    let scrolled = infer_upward_scroll(&prev_rows, &next_rows);
                    inline_objects.apply_scroll(scrolled);
                }
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

/// Synchronizes kitty graphics state with the rio-vt engine.
///
/// Runs every frame after [`pump_pty_output`], [`handle_window_resize`],
/// and the mouse input set, and before the redraw set: placements depend on
/// the scrollback display offset, which mouse input changes without any PTY
/// traffic, and on the post-resize grid. The pending-redraw flag is peeked
/// (the redraw set consumes it later in the frame) as the grid-changed
/// signal that gates the placeholder rescan.
pub(crate) fn refresh_kitty_graphics(
    mut runtime: ResMut<TerminalRuntime>,
    mut inline_objects: ResMut<TerminalInlineObjects>,
    redraw: Res<TerminalRedrawState>,
) {
    let terminal_changed = redraw.pending();
    inline_objects.refresh_kitty(&mut runtime, terminal_changed);
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

#[derive(SystemParam)]
pub(crate) struct ResizeParams<'w, 's> {
    primary_window: Query<'w, 's, (Entity, &'static Window), With<PrimaryWindow>>,
    runtime: ResMut<'w, TerminalRuntime>,
    terminal: ResMut<'w, TerminalSurface>,
    redraw: ResMut<'w, TerminalRedrawState>,
    viewport: ResMut<'w, TerminalViewport>,
    plane_query: TerminalPlaneLayoutQuery<'w, 's>,
    plane_back_query: TerminalPlaneBackLayoutQuery<'w, 's>,
}

/// Handles primary window resize events.
///
/// This updates both the PTY grid and the rendered scene dimensions. It resizes
/// [`TerminalRuntime`], [`TerminalSurface`], [`TerminalViewport`], the 2D terminal sprite and the
/// front and back terminal plane transforms.
///
/// The redraw system runs after this system and uploads the resized terminal image in the same
/// frame.
pub(crate) fn handle_window_resize(
    mut resize_events: MessageReader<WindowResized>,
    mut params: ResizeParams,
) {
    let ResizeParams {
        primary_window,
        runtime,
        terminal,
        redraw,
        viewport,
        plane_query,
        plane_back_query,
    } = &mut params;
    let Ok((primary_window, window)) = primary_window.single() else {
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

    // Minimizing the window reports a 0x0 size. Skip it so the terminal keeps
    // its last good grid instead of collapsing to a degenerate size the VT
    // engine can't safely process.
    if window_size.x < 1.0 || window_size.y < 1.0 {
        return;
    }

    let window_size = window_size.max(Vec2::ONE);
    let layout = terminal.resize_to_fit(window_size, render_scale_for_window(window));
    let pty_pixels = layout.pty_pixels();
    let cell_px = terminal.char_dimensions() * layout.render_scale;
    runtime.resize(
        layout.cols,
        layout.rows,
        pty_pixels.x as u16,
        pty_pixels.y as u16,
        cell_px.x,
        cell_px.y,
    );
    sync_terminal_layout(layout, viewport, plane_query, plane_back_query);
    redraw.request();
}

/// Applies inline object visibility for the current presentation mode.
///
/// This runs after `scene::apply_terminal_presentation` and only flips scene visibility.
/// [`TerminalInlineObjectSprite`] entities are shown in [`TerminalPresentationMode::Flat2d`], while
/// [`TerminalInlineObjectPlane`] entities are shown in the 3D presentation modes.
pub fn apply_inline_objects(
    camera_slots: Res<TerminalCameraSlots>,
    mut sprite_query: Query<&mut Visibility, With<TerminalInlineObjectSprite>>,
    mut plane_query: Query<
        &mut Visibility,
        (
            With<TerminalInlineObjectPlane>,
            Without<TerminalInlineObjectSprite>,
        ),
    >,
) {
    let sprite_visibility = if camera_slots.active().mode.is_3d() {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
    let plane_visibility = if camera_slots.active().mode.is_3d() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };

    for mut visibility in &mut sprite_query {
        if *visibility != sprite_visibility {
            *visibility = sprite_visibility;
        }
    }
    for mut visibility in &mut plane_query {
        if *visibility != plane_visibility {
            *visibility = plane_visibility;
        }
    }
}

/// Redraw system parameters.
/// Tracks whether the terminal frame was redrawn during the current update.
#[derive(Resource, Default)]
pub(crate) struct TerminalFrameDirty(pub bool);

/// Ordered terminal redraw pipeline:
/// [`render_terminal_widget`] → [`sync_terminal_materials`] →
/// [`finish_terminal_model_load`].
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TerminalRedrawSet;

/// Half-period of the fastest blink cadence the renderer supports (rapid
/// blink); slow blink (0.5s) is a multiple of it.
const BLINK_TICK_SECS: f32 = 0.25;

#[derive(SystemParam)]
pub(crate) struct RenderWidgetParams<'w, 's> {
    app_config: Res<'w, AppConfig>,
    runtime: Res<'w, TerminalRuntime>,
    terminal: ResMut<'w, TerminalSurface>,
    selection: Res<'w, TerminalSelection>,
    time: Res<'w, Time>,
    redraw: ResMut<'w, TerminalRedrawState>,
    images: ResMut<'w, Assets<Image>>,
    direct_render: Res<'w, DirectTerminalSceneExchange>,
    model_load_state: Res<'w, ModelLoadState>,
    frame_dirty: ResMut<'w, TerminalFrameDirty>,
    blink_phase: Local<'s, u64>,
}

/// Redraws the Ratatui buffer and publishes the rendered terminal frame.
///
/// This runs after [`pump_pty_output`] and [`crate::mouse::handle_mouse_input`]. It records
/// whether the frame changed in [`TerminalFrameDirty`] so the rest of [`TerminalRedrawSet`]
/// can skip its work on clean frames.
pub(crate) fn render_terminal_widget(mut params: RenderWidgetParams) {
    let RenderWidgetParams {
        app_config,
        runtime,
        terminal,
        selection,
        time,
        redraw,
        images,
        direct_render,
        model_load_state,
        frame_dirty,
        blink_phase,
    } = &mut params;
    let needs_redraw = redraw.take();
    // The texture content only changes with terminal state or blink phase;
    // warp and camera animations are mesh- and camera-side. Rebuilding on
    // blink ticks instead of every frame keeps idle scene builds at 4Hz.
    let phase = (time.elapsed_secs() / BLINK_TICK_SECS) as u64;
    let blink_ticked = **blink_phase != phase;
    **blink_phase = phase;
    frame_dirty.0 = needs_redraw || blink_ticked || !model_load_state.loaded;
    if !frame_dirty.0 {
        return;
    }

    let term = &runtime.term;
    let _ = terminal.tui.draw(|frame| {
        frame.render_widget(
            TerminalWidget {
                term,
                selection,
                theme: &app_config.theme,
                font_style: app_config.font.style,
            },
            frame.area(),
        );

        if !app_config.cursor.model.visible && !vt::cursor_hidden(term) {
            let (cursor_row, cursor_col) = vt::cursor_position(term);
            frame.set_cursor_position((cursor_col, cursor_row));
        }
    });

    let _ = terminal.sync_image(images, direct_render, time.elapsed_secs());
}

#[derive(SystemParam)]
pub(crate) struct SyncMaterialsParams<'w, 's> {
    runtime: Res<'w, TerminalRuntime>,
    terminal: Res<'w, TerminalSurface>,
    camera_slots: Res<'w, TerminalCameraSlots>,
    images: ResMut<'w, Assets<Image>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    plane_materials: Query<'w, 's, &'static MeshMaterial3d<StandardMaterial>, With<TerminalPlane>>,
    plane_back_materials:
        Query<'w, 's, &'static MeshMaterial3d<StandardMaterial>, With<TerminalPlaneBack>>,
    present_materials: ResMut<'w, Assets<TerminalPresentMaterial>>,
    present_query: Query<'w, 's, &'static MeshMaterial2d<TerminalPresentMaterial>>,
    frame_dirty: Res<'w, TerminalFrameDirty>,
}

/// Refreshes the debug back texture and plane materials after a redraw.
pub(crate) fn sync_terminal_materials(mut params: SyncMaterialsParams) {
    let SyncMaterialsParams {
        runtime,
        terminal,
        camera_slots,
        images,
        materials,
        plane_materials,
        plane_back_materials,
        present_materials,
        present_query,
        frame_dirty,
    } = &mut params;
    if !frame_dirty.0 {
        return;
    }

    // The present texture's GpuImage is recreated when the terminal resizes (window
    // resize / font zoom), which invalidates the 2D present material's cached bind
    // group. Writing the texture handle — not merely touching the asset with
    // `get_mut` — advances the material's change tick so Bevy re-prepares the bind
    // group against the current GpuImage; a no-op touch leaves the quad sampling a
    // stale texture and the flat view freezes. Matches the plane handling.
    if let Some(present_image) = terminal.image_handle.as_ref() {
        for present_handle in present_query.iter() {
            if let Some(mut material) = present_materials.get_mut(&present_handle.0) {
                material.texture = present_image.clone();
            }
        }
    }

    let in_3d = camera_slots.active().mode.is_3d();
    if in_3d {
        sync_terminal_debug_image(terminal, images, &runtime.term);
    }

    sync_plane_texture(terminal.image_handle.as_ref(), plane_materials, materials);
    if in_3d {
        sync_plane_texture(
            terminal.back_image_handle.as_ref(),
            plane_back_materials,
            materials,
        );
    }
}

#[derive(SystemParam)]
pub(crate) struct ModelLoadParams<'w, 's> {
    app_config: Res<'w, AppConfig>,
    model_load_state: ResMut<'w, ModelLoadState>,
    redraw: ResMut<'w, TerminalRedrawState>,
    commands: Commands<'w, 's>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    asset_server: Res<'w, AssetServer>,
    frame_dirty: Res<'w, TerminalFrameDirty>,
}

/// Completes deferred cursor-model loading once the first frame is uploaded.
///
/// The first successful upload defers cursor-model spawning to the next frame. After that, it
/// ensures the cursor model exists so [`sync_asset_to_terminal_cursor`] can position it.
pub(crate) fn finish_terminal_model_load(mut params: ModelLoadParams) {
    let ModelLoadParams {
        app_config,
        model_load_state,
        redraw,
        commands,
        meshes,
        materials,
        images,
        asset_server,
        frame_dirty,
    } = &mut params;
    if !frame_dirty.0 {
        return;
    }

    if !model_load_state.first_frame_uploaded {
        model_load_state.first_frame_uploaded = true;
        redraw.request();
        return;
    }

    if !model_load_state.loaded {
        if app_config.cursor.model.visible {
            spawn_cursor_model(
                commands,
                meshes,
                materials,
                images,
                asset_server,
                app_config,
            );
        }
        model_load_state.loaded = true;
    }
}

/// Synchronizes Kitty inline objects.
#[derive(SystemParam)]
pub(crate) struct SyncInlineParams<'w, 's> {
    commands: Commands<'w, 's>,
    inline_objects: ResMut<'w, TerminalInlineObjects>,
    terminal: Res<'w, TerminalSurface>,
    viewport: Res<'w, TerminalViewport>,
    camera_slots: Res<'w, TerminalCameraSlots>,
    mobius_transition: Res<'w, MobiusTransition>,
    plane_warp: Res<'w, TerminalPlaneWarp>,
    time: Res<'w, Time>,
    plane_query: Query<'w, 's, (Entity, &'static Transform), With<TerminalPlane>>,
    sprite_query: Query<'w, 's, Entity, With<TerminalInlineObjectSprite>>,
    plane_image_query: Query<'w, 's, Entity, With<TerminalInlineObjectPlane>>,
    rgp_query: Query<'w, 's, Entity, With<TerminalRgpObject>>,
    asset_server: Res<'w, AssetServer>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    meshes: ResMut<'w, Assets<Mesh>>,
}

/// Synchronizes inline object entities.
///
/// This runs after [`render_terminal_widget`]. It rebuilds the scene entities for kitty graphics
/// placements and registered RGP objects, clearing stale inline entities first so the scene
/// matches the latest terminal state exactly.
///
/// In 2D mode it spawns [`TerminalInlineObjectSprite`] entities. In 3D mode it also generates
/// plane-attached meshes under [`TerminalPlane`] so images follow the warped terminal surface.
/// Warp motion is handled in place by [`animate_inline_kitty_planes`].
pub(crate) fn sync_inline_objects(mut params: SyncInlineParams) {
    let SyncInlineParams {
        commands,
        inline_objects,
        terminal,
        viewport,
        camera_slots,
        mobius_transition,
        plane_warp,
        time,
        plane_query,
        sprite_query,
        plane_image_query,
        rgp_query,
        asset_server,
        materials,
        images,
        meshes,
    } = &mut params;
    if !inline_objects.needs_sync(viewport.size, terminal.cols, terminal.rows) {
        return;
    }

    for entity in sprite_query.iter() {
        commands.entity(entity).despawn();
    }
    for entity in plane_image_query.iter() {
        commands.entity(entity).despawn();
    }
    for entity in rgp_query.iter() {
        commands.entity(entity).despawn();
    }

    let Ok((plane_entity, _plane_transform)) = plane_query.single() else {
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

    for object_id in renderable_ids {
        let Some(anchor) = inline_objects.anchors.get(&object_id) else {
            continue;
        };
        let style = anchor.style;
        let Some(object) = inline_objects.objects.get_mut(&object_id) else {
            continue;
        };
        spawn_rgp_object(
            commands,
            object_id,
            object,
            style,
            materials,
            meshes,
            asset_server,
        );
    }

    // Kitty placements come resolved from the engine; the views are cloned
    // out so the image store can be borrowed mutably for texture uploads.
    let mut plane_children = Vec::new();
    let views = inline_objects.kitty.placements().to_vec();
    let inline: &mut TerminalInlineObjects = inline_objects;
    for (draw_order, view) in views.iter().enumerate() {
        let Some(image) = inline.kitty.image_mut(view.image_id) else {
            continue;
        };
        let layout = inline_layout(
            view.row,
            view.col,
            view.columns,
            view.rows,
            terminal,
            viewport,
            cell_width,
            cell_height,
        );
        let mut ctx = KittyRenderContext {
            mode: camera_slots.active().mode,
            mobius_progress: active_mobius_progress(camera_slots.active().mode, mobius_transition),
            warp_amount: plane_warp.amount,
            elapsed_secs,
            materials,
            images,
            meshes,
            plane_children: &mut plane_children,
        };
        sync_kitty_placement(
            commands,
            view,
            &mut image.raster,
            &mut inline.kitty_planes,
            &layout,
            draw_order,
            &mut ctx,
        );
    }

    // Placements that disappeared leave their cached plane assets behind;
    // free them so `Assets` does not accumulate dead meshes and materials.
    let live_keys = views
        .iter()
        .map(KittyPlacementView::key)
        .collect::<std::collections::HashSet<PlacementKey>>();
    inline.kitty_planes.retain(|key, cache| {
        if live_keys.contains(key) {
            return true;
        }
        meshes.remove(&cache.mesh);
        materials.remove(&cache.material);
        false
    });

    if !plane_children.is_empty() {
        commands.entity(plane_entity).add_children(&plane_children);
    }

    inline_objects.finish_sync(viewport.size, terminal.cols, terminal.rows);
}

#[allow(clippy::too_many_arguments)]
fn inline_layout(
    row: f32,
    col: f32,
    columns: f32,
    rows: f32,
    terminal: &TerminalSurface,
    viewport: &TerminalViewport,
    cell_width: f32,
    cell_height: f32,
) -> InlineLayout {
    let grid_cols = terminal.cols.max(1) as f32;
    let grid_rows = terminal.rows.max(1) as f32;
    let center_x = viewport.center.x - viewport.size.x * 0.5 + (col + columns * 0.5) * cell_width;
    let center_y = viewport.center.y + viewport.size.y * 0.5 - (row + rows * 0.5) * cell_height;

    InlineLayout {
        columns,
        rows,
        center_x,
        center_y,
        local_x: (col + columns * 0.5) / grid_cols - 0.5,
        local_y: 0.5 - (row + rows * 0.5) / grid_rows,
        local_width: columns / grid_cols,
        local_height: rows / grid_rows,
        pixel_width: columns * cell_width,
        pixel_height: rows * cell_height,
    }
}

#[allow(clippy::too_many_arguments)]
fn sync_kitty_placement(
    commands: &mut Commands,
    view: &KittyPlacementView,
    raster: &mut crate::inline::RasterObject,
    plane_caches: &mut HashMap<PlacementKey, KittyPlaneCache>,
    layout: &InlineLayout,
    draw_order: usize,
    ctx: &mut KittyRenderContext<'_>,
) {
    let image_handle = if let Some(handle) = raster.handle.as_ref() {
        handle.clone()
    } else {
        let mut image = Image::new_fill(
            Extent3d {
                width: raster.width,
                height: raster.height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 0],
            TextureFormat::Rgba8UnormSrgb,
            bevy::asset::RenderAssetUsages::default(),
        );
        image.sampler = ImageSampler::nearest();
        image.data = Some(std::mem::take(&mut raster.rgba));
        let handle = ctx.images.add(image);
        raster.handle = Some(handle.clone());
        handle
    };

    let mut sprite = Sprite::from_image(image_handle.clone());
    sprite.custom_size = Some(Vec2::new(layout.pixel_width, layout.pixel_height));
    if view.source_rect != [0.0, 0.0, 1.0, 1.0] {
        let [u0, v0, u1, v1] = view.source_rect;
        sprite.rect = Some(Rect::new(
            u0 * raster.width as f32,
            v0 * raster.height as f32,
            u1 * raster.width as f32,
            v1 * raster.height as f32,
        ));
    }
    // Negative kitty z-indices render behind the terminal per the spec: the
    // sprite goes under the text quad (z = 0), and the plane sinks behind
    // the terminal surface. The epsilon per placement keeps overlapping
    // images stacked back-to-front on both paths — the views arrive sorted
    // by kitty z-index.
    let layer = draw_order as f32 * 0.001;
    let sprite_z = if view.z < 0 {
        -5.0 + layer
    } else {
        5.0 + layer
    };
    commands.spawn((
        TerminalInlineObjectSprite,
        sprite,
        Transform::from_translation(Vec3::new(layout.center_x, layout.center_y, sprite_z)),
        if ctx.mode.is_3d() {
            Visibility::Hidden
        } else {
            Visibility::Visible
        },
    ));

    let plane_layout = inline_kitty_plane_layout(layout, view.source_rect);
    let (mesh_handle, material_handle) =
        ensure_kitty_plane_assets(plane_caches, view.key(), &plane_layout, &image_handle, ctx);
    let plane_bias = if view.z < 0 {
        -0.002 - draw_order as f32 * 0.0005
    } else {
        0.002 + draw_order as f32 * 0.0005
    };
    ctx.plane_children.push(
        commands
            .spawn((
                TerminalInlineObjectPlane,
                plane_layout,
                Mesh3d(mesh_handle),
                MeshMaterial3d(material_handle),
                Transform::from_xyz(0.0, 0.0, plane_bias),
                // Warp and Mobius morphing move the vertices far outside the
                // AABB cached at spawn, like the terminal planes.
                NoFrustumCulling,
            ))
            .id(),
    );
}

fn inline_kitty_plane_layout(
    layout: &InlineLayout,
    source_rect: [f32; 4],
) -> InlineKittyPlaneLayout {
    InlineKittyPlaneLayout {
        local_x: layout.local_x,
        local_y: layout.local_y,
        local_width: layout.local_width,
        local_height: layout.local_height,
        x_segments: (layout.columns.ceil() as u32).clamp(2, 24),
        y_segments: (layout.rows.ceil() as u32).clamp(2, 24),
        source_rect,
    }
}

fn ensure_kitty_plane_assets(
    plane_caches: &mut HashMap<PlacementKey, KittyPlaneCache>,
    key: PlacementKey,
    layout: &InlineKittyPlaneLayout,
    image_handle: &Handle<Image>,
    ctx: &mut KittyRenderContext<'_>,
) -> (Handle<Mesh>, Handle<StandardMaterial>) {
    let needs_rebuild = plane_caches.get(&key).is_none_or(|cache| {
        cache.x_segments != layout.x_segments
            || cache.y_segments != layout.y_segments
            || cache.source_rect != layout.source_rect
    });
    if needs_rebuild {
        if let Some(cache) = plane_caches.remove(&key) {
            ctx.meshes.remove(&cache.mesh);
            ctx.materials.remove(&cache.material);
        }
        let mesh = build_kitty_plane_mesh(
            layout,
            ctx.mode,
            ctx.warp_amount,
            ctx.elapsed_secs,
            ctx.mobius_progress,
        );
        let mesh_handle = ctx.meshes.add(mesh);
        let material_handle = ctx.materials.add(kitty_plane_material(image_handle));
        plane_caches.insert(
            key,
            KittyPlaneCache {
                x_segments: layout.x_segments,
                y_segments: layout.y_segments,
                source_rect: layout.source_rect,
                mesh: mesh_handle.clone(),
                material: material_handle.clone(),
            },
        );
        return (mesh_handle, material_handle);
    }

    let cache = plane_caches
        .get_mut(&key)
        .expect("plane cache should exist");
    if let Some(mut mesh) = ctx.meshes.get_mut(&cache.mesh) {
        write_kitty_plane_positions(
            &mut mesh,
            layout,
            ctx.mode,
            ctx.warp_amount,
            ctx.elapsed_secs,
            ctx.mobius_progress,
        );
    }
    if let Some(mut material) = ctx.materials.get_mut(&cache.material) {
        material.base_color_texture = Some(image_handle.clone());
    }
    (cache.mesh.clone(), cache.material.clone())
}

fn kitty_plane_material(image_handle: &Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(image_handle.clone()),
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        unlit: true,
        ..default()
    }
}

fn build_kitty_plane_mesh(
    layout: &InlineKittyPlaneLayout,
    mode: TerminalPresentationMode,
    warp_amount: f32,
    elapsed_secs: f32,
    mobius_progress: f32,
) -> Mesh {
    let vertex_count = ((layout.x_segments + 1) * (layout.y_segments + 1)) as usize;
    let mut positions = Vec::with_capacity(vertex_count);
    let normals = vec![[0.0, 0.0, 1.0]; vertex_count];
    let mut uvs = Vec::with_capacity(vertex_count);
    let mut indices = Vec::with_capacity((layout.x_segments * layout.y_segments * 6) as usize);

    let [u0, v0, u1, v1] = layout.source_rect;
    for y in 0..=layout.y_segments {
        let v = y as f32 / layout.y_segments as f32;
        let py = layout.local_y + (0.5 - v) * layout.local_height;
        for x in 0..=layout.x_segments {
            let u = x as f32 / layout.x_segments as f32;
            let px = layout.local_x + (u - 0.5) * layout.local_width;
            positions.push([px, py, 0.0]);
            uvs.push([u0 + u * (u1 - u0), v0 + v * (v1 - v0)]);
        }
    }

    for y in 0..layout.y_segments {
        for x in 0..layout.x_segments {
            let row = y * (layout.x_segments + 1);
            let next_row = (y + 1) * (layout.x_segments + 1);
            let i0 = row + x;
            let i1 = i0 + 1;
            let i2 = next_row + x;
            let i3 = i2 + 1;
            indices.extend_from_slice(&[i0, i2, i1, i1, i2, i3]);
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices));
    write_kitty_plane_positions(
        &mut mesh,
        layout,
        mode,
        warp_amount,
        elapsed_secs,
        mobius_progress,
    );
    mesh
}

fn write_kitty_plane_positions(
    mesh: &mut Mesh,
    layout: &InlineKittyPlaneLayout,
    mode: TerminalPresentationMode,
    warp_amount: f32,
    elapsed_secs: f32,
    mobius_progress: f32,
) {
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    else {
        return;
    };

    let mut index = 0;
    for y in 0..=layout.y_segments {
        let v = y as f32 / layout.y_segments as f32;
        let py = layout.local_y + (0.5 - v) * layout.local_height;
        for x in 0..=layout.x_segments {
            let u = x as f32 / layout.x_segments as f32;
            let px = layout.local_x + (u - 0.5) * layout.local_width;
            if index < positions.len() {
                let point = plane_surface_point(
                    mode,
                    px,
                    py,
                    warp_amount,
                    elapsed_secs,
                    1.5,
                    mobius_progress,
                );
                positions[index] = point.to_array();
            }
            index += 1;
        }
    }
}

/// Animates Kitty image planes attached to the warped terminal surface.
///
/// This runs after [`sync_inline_objects`] and updates cached plane mesh positions in place when
/// warp is active, instead of rebuilding inline entities every frame.
pub(crate) fn animate_inline_kitty_planes(
    camera_slots: Res<TerminalCameraSlots>,
    mobius_transition: Res<MobiusTransition>,
    warp: Res<TerminalPlaneWarp>,
    time: Res<Time>,
    mut previous_mode: Local<Option<TerminalPresentationMode>>,
    query: Query<(&InlineKittyPlaneLayout, &Mesh3d), With<TerminalInlineObjectPlane>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let mode = camera_slots.active().mode;
    let mode_changed = previous_mode.as_ref() != Some(&mode);
    *previous_mode = Some(mode);
    if !should_animate_inline_kitty_planes(
        mode,
        warp.amount,
        warp.is_changed(),
        mode_changed,
        mobius_transition.active,
        mobius_transition.is_changed(),
    ) {
        return;
    }

    let elapsed_secs = time.elapsed_secs();
    let mobius_progress = active_mobius_progress(mode, &mobius_transition);
    for (layout, mesh3d) in query.iter() {
        let Some(mut mesh) = meshes.get_mut(&mesh3d.0) else {
            continue;
        };
        write_kitty_plane_positions(
            &mut mesh,
            layout,
            mode,
            warp.amount,
            elapsed_secs,
            mobius_progress,
        );
    }
}

fn should_animate_inline_kitty_planes(
    mode: TerminalPresentationMode,
    warp_amount: f32,
    warp_changed: bool,
    mode_changed: bool,
    mobius_transition_active: bool,
    mobius_transition_changed: bool,
) -> bool {
    match mode {
        TerminalPresentationMode::Flat2d => false,
        TerminalPresentationMode::Plane3d | TerminalPresentationMode::Perspective3d => {
            mode_changed || warp_changed || warp_amount > 0.0
        }
        TerminalPresentationMode::Mobius3d => {
            mode_changed
                || mobius_transition_active
                || mobius_transition_changed
                || warp_changed
                || warp_amount > 0.0
        }
    }
}

fn spawn_rgp_object(
    commands: &mut Commands,
    object_id: u32,
    object: &mut crate::inline::RgpInlineObject,
    style: crate::inline::InlineStyle,
    materials: &mut Assets<StandardMaterial>,
    meshes: &mut Assets<Mesh>,
    asset_server: &AssetServer,
) {
    match object {
        crate::inline::RgpInlineObject::Obj {
            meshes: source_meshes,
            handles,
        } => {
            let depth_key = (style.depth.max(0.0) * 100.0).round() as u32;
            let mesh_handles = if let Some((existing_key, existing_handles)) = handles.as_ref() {
                if *existing_key == depth_key {
                    existing_handles.clone()
                } else {
                    for handle in existing_handles {
                        meshes.remove(handle);
                    }
                    let mesh_handles = source_meshes
                        .iter()
                        .cloned()
                        .map(|mesh| meshes.add(extrude_mesh(mesh, style.depth)))
                        .collect::<Vec<_>>();
                    *handles = Some((depth_key, mesh_handles.clone()));
                    mesh_handles
                }
            } else {
                let mesh_handles = source_meshes
                    .iter()
                    .cloned()
                    .map(|mesh| meshes.add(extrude_mesh(mesh, style.depth)))
                    .collect::<Vec<_>>();
                *handles = Some((depth_key, mesh_handles.clone()));
                mesh_handles
            };
            let use_lighting = true;
            let [r, g, b] = match style.color {
                Some([r, g, b]) => [r, g, b],
                None => [255, 255, 255],
            };
            let material = materials.add(StandardMaterial {
                base_color: Color::srgb_u8(r, g, b),
                emissive: if use_lighting {
                    LinearRgba::rgb(0.02, 0.02, 0.02)
                } else {
                    LinearRgba::rgb(0.0, 0.0, 0.0)
                },
                metallic: 0.0,
                perceptual_roughness: if use_lighting { 0.88 } else { 1.0 },
                reflectance: if use_lighting { 0.18 } else { 0.0 },
                cull_mode: None,
                unlit: !use_lighting,
                ..default()
            });
            let root = commands
                .spawn((
                    TerminalRgpObject { object_id },
                    Transform::default(),
                    Visibility::Visible,
                ))
                .id();
            let children = mesh_handles
                .into_iter()
                .map(|handle| {
                    commands
                        .spawn((
                            Mesh3d(handle),
                            MeshMaterial3d(material.clone()),
                            Transform::default(),
                        ))
                        .id()
                })
                .collect::<Vec<_>>();
            commands.entity(root).add_children(&children);
        }
        crate::inline::RgpInlineObject::Gltf { asset_path, handle } => {
            let handle = if let Some(handle) = handle.as_ref() {
                handle.clone()
            } else {
                let scene =
                    asset_server.load(GltfAssetLabel::Scene(0).from_asset(asset_path.clone()));
                *handle = Some(scene.clone());
                scene
            };
            commands.spawn((
                TerminalRgpObject { object_id },
                Transform::default(),
                Visibility::Visible,
                WorldAssetRoot(handle),
            ));
        }
        crate::inline::RgpInlineObject::Stl { mesh, handle } => {
            let depth_key = (style.depth.max(0.0) * 100.0).round() as u32;
            let mesh_handle = match handle.as_ref() {
                Some((existing_key, existing_handle)) if *existing_key == depth_key => {
                    existing_handle.clone()
                }
                Some((_, existing_handle)) => {
                    meshes.remove(existing_handle);
                    let mesh_handle = meshes.add(extrude_mesh(mesh.clone(), style.depth));
                    *handle = Some((depth_key, mesh_handle.clone()));
                    mesh_handle
                }
                None => {
                    let mesh_handle = meshes.add(extrude_mesh(mesh.clone(), style.depth));
                    *handle = Some((depth_key, mesh_handle.clone()));
                    mesh_handle
                }
            };
            let use_lighting = true;
            let [r, g, b] = match style.color {
                Some([r, g, b]) => [r, g, b],
                None => [255, 255, 255],
            };
            let material = materials.add(StandardMaterial {
                base_color: Color::srgb_u8(r, g, b),
                emissive: if use_lighting {
                    LinearRgba::rgb(0.02, 0.02, 0.02)
                } else {
                    LinearRgba::rgb(0.0, 0.0, 0.0)
                },
                metallic: 0.0,
                perceptual_roughness: if use_lighting { 0.88 } else { 1.0 },
                reflectance: if use_lighting { 0.18 } else { 0.0 },
                cull_mode: None,
                unlit: !use_lighting,
                ..default()
            });
            let root = commands
                .spawn((
                    TerminalRgpObject { object_id },
                    Transform::default(),
                    Visibility::Visible,
                ))
                .id();

            let child = commands
                .spawn((
                    Mesh3d(mesh_handle),
                    MeshMaterial3d(material.clone()),
                    Transform::default(),
                ))
                .id();
            commands.entity(root).add_child(child);
        }
    }
}

/// Synchronizes RGP inline objects.
#[derive(SystemParam)]
pub(crate) struct RgpSyncParams<'w, 's> {
    app_config: Res<'w, AppConfig>,
    terminal: Res<'w, TerminalSurface>,
    viewport: Res<'w, TerminalViewport>,
    camera_slots: Res<'w, TerminalCameraSlots>,
    mobius_transition: Res<'w, MobiusTransition>,
    plane_warp: Res<'w, TerminalPlaneWarp>,
    time: Res<'w, Time>,
    plane_query: PlaneTransformQuery<'w, 's>,
    inline_objects: Res<'w, TerminalInlineObjects>,
    query: Query<
        'w,
        's,
        (
            &'static TerminalRgpObject,
            &'static mut Transform,
            &'static mut Visibility,
        ),
    >,
}

/// Synchronizes RGP object entities.
///
/// This runs after [`sync_inline_objects`]. It does not create registrations itself; instead, it
/// positions existing [`TerminalRgpObject`] roots from [`TerminalInlineObjects`] anchor data.
///
/// In [`TerminalPresentationMode::Flat2d`] objects are placed in screen space above the terminal
/// surface. In the 3D modes they are projected onto the active terminal surface using the current
/// [`TerminalPlane`] transform.
pub(crate) fn sync_rgp_objects(mut params: RgpSyncParams) {
    let RgpSyncParams {
        app_config,
        terminal,
        viewport,
        camera_slots,
        mobius_transition,
        plane_warp,
        time,
        plane_query,
        inline_objects,
        query,
    } = &mut params;
    let cell_width = viewport.size.x / terminal.cols.max(1) as f32;
    let cell_height = viewport.size.y / terminal.rows.max(1) as f32;
    let elapsed_secs = time.elapsed_secs();
    let mode = camera_slots.active().mode;
    let mobius_progress = active_mobius_progress(mode, mobius_transition);

    for (object, mut transform, mut visibility) in query.iter_mut() {
        let Some(anchor) = inline_objects.anchors.get(&object.object_id) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let layout = inline_layout(
            f32::from(anchor.row),
            f32::from(anchor.col),
            anchor.columns as f32,
            anchor.rows as f32,
            terminal,
            viewport,
            cell_width,
            cell_height,
        );
        let base_scale = layout.pixel_width.max(layout.pixel_height).max(1.0) * 0.9;
        let scale = base_scale * anchor.style.scale.max(0.001);
        let scale3 = Vec3::new(
            anchor.style.scale3.x.max(0.001),
            anchor.style.scale3.y.max(0.001),
            anchor.style.scale3.z.max(0.001),
        );
        let base_oblique = if anchor.style.depth > 0.0 {
            Quat::from_rotation_y(0.75) * Quat::from_rotation_x(0.35)
        } else {
            Quat::IDENTITY
        };
        let explicit_rotation = Quat::from_euler(
            EulerRot::XYZ,
            anchor.style.rotation.x.to_radians(),
            anchor.style.rotation.y.to_radians(),
            anchor.style.rotation.z.to_radians(),
        );
        let (spin, tilt, bob) = if anchor.style.animate {
            (
                elapsed_secs * app_config.cursor.animation.spin_speed,
                elapsed_secs * app_config.cursor.animation.spin_speed * 0.7,
                (elapsed_secs * app_config.cursor.animation.bob_speed).sin()
                    * cell_height
                    * app_config.cursor.animation.bob_amplitude,
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        let animated_rotation = Quat::from_rotation_y(spin) * Quat::from_rotation_x(tilt);
        let object_rotation = base_oblique * explicit_rotation * animated_rotation;
        let object_scale = Vec3::splat(scale) * scale3;

        if mode.is_3d() {
            let Ok(plane_transform) = plane_query.single() else {
                *visibility = Visibility::Hidden;
                continue;
            };
            let local_position = plane_surface_point(
                mode,
                layout.local_x,
                layout.local_y,
                plane_warp.amount,
                elapsed_secs,
                8.0 + anchor.style.depth * 1.5,
                mobius_progress,
            ) + anchor.style.offset;
            transform.translation = plane_transform.transform_point(local_position);
            transform.rotation = plane_transform.rotation * object_rotation;
            transform.scale = object_scale;
            *visibility = Visibility::Visible;
        } else {
            transform.translation = Vec3::new(
                layout.center_x + anchor.style.offset.x * (terminal.pixmap_dimensions().x as f32),
                layout.center_y
                    + bob
                    + anchor.style.offset.y * (terminal.pixmap_dimensions().y as f32),
                CURSOR_DEPTH + anchor.style.depth * 4.0 + anchor.style.offset.z,
            );
            transform.rotation = object_rotation;
            transform.scale = object_scale;
            *visibility = Visibility::Visible;
        }
    }
}

/// Brightness application parameters.
#[derive(SystemParam)]
pub(crate) struct BrightnessParams<'w, 's> {
    app_config: Res<'w, AppConfig>,
    inline_objects: Res<'w, TerminalInlineObjects>,
    rgp_roots: Query<'w, 's, (Entity, &'static TerminalRgpObject)>,
    cursor_roots: Query<'w, 's, Entity, With<CursorModel>>,
    parent_query: Query<'w, 's, &'static ChildOf>,
    material_query: Query<
        'w,
        's,
        (
            Entity,
            &'static mut MeshMaterial3d<StandardMaterial>,
            &'static ChildOf,
        ),
        Without<BrightnessAdjusted>,
    >,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    commands: Commands<'w, 's>,
}

/// Applies per-instance brightness to spawned materials.
///
/// This runs after [`sync_rgp_objects`] so newly spawned object descendants already exist. It walks
/// up each material-bearing entity through [`ChildOf`] relationships, finds either an
/// [`TerminalRgpObject`] root or a [`CursorModel`] root and clones the referenced material with
/// the effective brightness applied.
///
/// Adjusted entities receive [`BrightnessAdjusted`] so the same material branch is not processed
/// again every frame.
pub(crate) fn apply_instance_brightness(mut params: BrightnessParams) {
    let BrightnessParams {
        app_config,
        inline_objects,
        rgp_roots,
        cursor_roots,
        parent_query,
        material_query,
        materials,
        commands,
    } = &mut params;

    if material_query.is_empty() {
        return;
    }

    let rgp_brightness = rgp_roots
        .iter()
        .filter_map(|(entity, object)| {
            let brightness = inline_objects
                .anchors
                .get(&object.object_id)
                .map(|anchor| anchor.style.brightness)?;
            Some((entity, brightness))
        })
        .collect::<HashMap<_, _>>();
    let cursor_roots = cursor_roots.iter().collect::<Vec<_>>();

    for (entity, mut material_handle, parent) in material_query.iter_mut() {
        let mut current = parent.parent();
        let mut brightness = None;

        loop {
            if let Some(value) = rgp_brightness.get(&current) {
                brightness = Some(*value);
                break;
            }
            if cursor_roots.contains(&current) {
                brightness = Some(app_config.cursor.model.brightness);
                break;
            }
            let Ok(next) = parent_query.get(current) else {
                break;
            };
            current = next.parent();
        }

        let Some(brightness) = brightness else {
            continue;
        };

        let Some(source_material) = materials.get(&material_handle.0).cloned() else {
            continue;
        };
        let mut adjusted = source_material;
        let linear = adjusted.base_color.to_linear();
        adjusted.base_color = Color::linear_rgba(
            linear.red * brightness,
            linear.green * brightness,
            linear.blue * brightness,
            linear.alpha,
        );
        adjusted.emissive = LinearRgba::new(
            adjusted.emissive.red * brightness,
            adjusted.emissive.green * brightness,
            adjusted.emissive.blue * brightness,
            adjusted.emissive.alpha,
        );
        material_handle.0 = materials.add(adjusted);
        commands.entity(entity).insert(BrightnessAdjusted);
    }
}

fn extrude_mesh(mesh: Mesh, depth: f32) -> Mesh {
    if depth <= 0.0 {
        return mesh;
    }

    let Some(VertexAttributeValues::Float32x3(source_positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        return mesh;
    };
    // `depth` is meant to give thickness to flat artwork. Applying the same extrusion to meshes
    // that already have volume creates overlapping surfaces and unstable depth ordering.
    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for &[_, _, z] in source_positions {
        min_z = min_z.min(z);
        max_z = max_z.max(z);
    }
    if (max_z - min_z).abs() > 1e-4 {
        return mesh;
    }
    let Some(indices) = mesh.indices() else {
        return mesh;
    };

    let indices = match indices {
        Indices::U16(values) => values.iter().map(|&value| value as u32).collect::<Vec<_>>(),
        Indices::U32(values) => values.clone(),
    };
    if indices.len() < 3 {
        return mesh;
    }

    let thickness = depth * 0.03;
    let half = thickness * 0.5;
    let source_len = source_positions.len() as u32;

    let mut positions = Vec::<[f32; 3]>::with_capacity(source_positions.len() * 2);
    let mut normals = Vec::<[f32; 3]>::with_capacity(source_positions.len() * 2);

    for &[x, y, z] in source_positions {
        positions.push([x, y, z + half]);
        normals.push([0.0, 0.0, 1.0]);
    }
    for &[x, y, z] in source_positions {
        positions.push([x, y, z - half]);
        normals.push([0.0, 0.0, -1.0]);
    }

    let mut out_indices = Vec::<u32>::with_capacity(indices.len() * 4);
    for triangle in indices.chunks_exact(3) {
        out_indices.extend_from_slice(triangle);
        out_indices.extend_from_slice(&[
            triangle[2] + source_len,
            triangle[1] + source_len,
            triangle[0] + source_len,
        ]);
    }

    let mut edge_counts = HashMap::<(u32, u32), u32>::new();
    for triangle in indices.chunks_exact(3) {
        for edge in [
            (triangle[0], triangle[1]),
            (triangle[1], triangle[2]),
            (triangle[2], triangle[0]),
        ] {
            let key = if edge.0 < edge.1 {
                edge
            } else {
                (edge.1, edge.0)
            };
            *edge_counts.entry(key).or_insert(0) += 1;
        }
    }

    for ((a, b), count) in edge_counts {
        if count != 1 {
            continue;
        }

        let front_a = source_positions[a as usize];
        let front_b = source_positions[b as usize];
        let edge = Vec3::new(
            front_b[0] - front_a[0],
            front_b[1] - front_a[1],
            front_b[2] - front_a[2],
        );
        let side_normal = Vec3::new(edge.y, -edge.x, 0.0).normalize_or_zero();

        let base = positions.len() as u32;
        positions.extend_from_slice(&[
            [front_a[0], front_a[1], front_a[2] + half],
            [front_b[0], front_b[1], front_b[2] + half],
            [front_b[0], front_b[1], front_b[2] - half],
            [front_a[0], front_a[1], front_a[2] - half],
        ]);
        for _ in 0..4 {
            normals.push([side_normal.x, side_normal.y, side_normal.z]);
        }
        out_indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    Mesh::new(PrimitiveTopology::TriangleList, Default::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_indices(Indices::U32(out_indices))
}

/// Animates the terminal plane warp.
///
/// This updates the front and back meshes stored in [`TerminalPlaneMeshes`]. It is independent of
/// the redraw path and only mutates mesh vertex positions, so plane presentation can keep moving
/// even when the terminal contents are otherwise static.
pub fn animate_terminal_plane_warp(
    time: Res<Time>,
    camera_slots: Res<TerminalCameraSlots>,
    mobius_transition: Res<MobiusTransition>,
    warp: Res<TerminalPlaneWarp>,
    plane_meshes: Res<TerminalPlaneMeshes>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let mode = camera_slots.active().mode;
    if mode == TerminalPresentationMode::Flat2d {
        return;
    }

    let needs_update = should_animate_terminal_plane_warp(
        mode,
        warp.amount,
        camera_slots.is_changed() || warp.is_changed(),
        mobius_transition.active,
        mobius_transition.is_changed(),
    );
    if !needs_update {
        return;
    }

    let mobius_progress = active_mobius_progress(mode, &mobius_transition);
    apply_plane_warp(
        meshes.get_mut(&plane_meshes.front),
        mode,
        warp.amount,
        time.elapsed_secs(),
        1.0,
        mobius_progress,
    );
    // The back sheet is hidden in Mobius mode; skipping it avoids uploading a
    // mesh nothing renders.
    if !mode.is_mobius() {
        apply_plane_warp(
            meshes.get_mut(&plane_meshes.back),
            mode,
            warp.amount,
            time.elapsed_secs(),
            -1.0,
            mobius_progress,
        );
    }
}

fn should_animate_terminal_plane_warp(
    mode: TerminalPresentationMode,
    warp_amount: f32,
    state_changed: bool,
    mobius_transition_active: bool,
    mobius_transition_changed: bool,
) -> bool {
    match mode {
        TerminalPresentationMode::Flat2d => false,
        TerminalPresentationMode::Plane3d | TerminalPresentationMode::Perspective3d => {
            state_changed || warp_amount > 0.0
        }
        // A settled strip with no warp is time-invariant; transition frames
        // (including the finish frame, via change detection) and mode or pose
        // changes still reapply it.
        TerminalPresentationMode::Mobius3d => {
            state_changed
                || warp_amount > 0.0
                || mobius_transition_active
                || mobius_transition_changed
        }
    }
}

/// Advances the Mobius transition and restores normal 3D interaction when it completes.
pub fn animate_mobius_transition(
    time: Res<Time>,
    mut camera_slots: ResMut<TerminalCameraSlots>,
    mut mobius_transition: ResMut<MobiusTransition>,
) {
    if camera_slots.active().mode != TerminalPresentationMode::Mobius3d {
        mobius_transition.stop();
        return;
    }

    if !mobius_transition.active {
        return;
    }

    mobius_transition.elapsed_secs += time.delta_secs();

    if mobius_transition.finished() {
        if mobius_transition.direction == crate::scene::MobiusTransitionDirection::Exiting {
            let preset = camera_slots.active_mut();
            let mut fallback_pose = preset.pose;
            fallback_pose.orthographic_scale = mobius_transition.source_zoom;
            fallback_pose.yaw = mobius_transition.source_yaw;
            fallback_pose.pitch = mobius_transition.source_pitch;
            fallback_pose.roll = mobius_transition.source_roll;
            fallback_pose.translation = mobius_transition.source_translation;
            let source = preset.mobius_source.unwrap_or(TerminalMobiusSource {
                mode: mobius_transition.source_mode,
                pose: fallback_pose,
            });
            preset.mode = source.mode;
            preset.pose = source.pose;
            preset.mobius_source = None;
        } else {
            camera_slots.active_mut().pose.orthographic_scale =
                mobius_transition.end_zoom.max(MIN_ORTHOGRAPHIC_SCALE);
        }
        mobius_transition.stop();
    }
}

fn active_mobius_progress(
    mode: TerminalPresentationMode,
    mobius_transition: &MobiusTransition,
) -> f32 {
    if mode != TerminalPresentationMode::Mobius3d {
        return 0.0;
    }

    if mobius_transition.active {
        mobius_transition.morph_progress()
    } else {
        1.0
    }
}

fn apply_plane_warp(
    mesh: Option<AssetMut<'_, Mesh>>,
    mode: TerminalPresentationMode,
    warp_amount: f32,
    elapsed_secs: f32,
    direction: f32,
    mobius_progress: f32,
) {
    let Some(mut mesh) = mesh else {
        return;
    };
    let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0) else {
        return;
    };
    let uvs = uvs.clone();
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    else {
        return;
    };

    for (position, uv) in positions.iter_mut().zip(uvs.iter()) {
        let x = uv[0] - 0.5;
        let y = 0.5 - uv[1];
        let point =
            plane_surface_point(mode, x, y, warp_amount, elapsed_secs, 0.0, mobius_progress);
        position[0] = point.x;
        position[1] = point.y;
        position[2] = oriented_plane_depth(mode, point.z, direction);
    }
}

fn oriented_plane_depth(mode: TerminalPresentationMode, depth: f32, direction: f32) -> f32 {
    match mode {
        TerminalPresentationMode::Plane3d | TerminalPresentationMode::Perspective3d => {
            depth * direction
        }
        TerminalPresentationMode::Flat2d | TerminalPresentationMode::Mobius3d => depth,
    }
}

/// Cursor synchronization parameters.
#[derive(SystemParam)]
pub(crate) struct CursorSyncParams<'w, 's> {
    app_config: Res<'w, AppConfig>,
    runtime: Res<'w, TerminalRuntime>,
    terminal: Res<'w, TerminalSurface>,
    viewport: Res<'w, TerminalViewport>,
    camera_slots: Res<'w, TerminalCameraSlots>,
    mobius_transition: Res<'w, MobiusTransition>,
    plane_warp: Res<'w, TerminalPlaneWarp>,
    time: Res<'w, Time>,
    plane_query: Query<'w, 's, &'static Transform, (With<TerminalPlane>, Without<CursorModel>)>,
    query: CursorTransformQuery<'w, 's>,
}

/// Synchronizes the 3D cursor model with the terminal cursor.
///
/// This runs after [`render_terminal_widget`], once the cursor model has been spawned and the latest
/// terminal cursor position is available from [`TerminalRuntime`]. It updates the [`CursorModel`]
/// transform and visibility for both 2D and 3D presentation modes.
///
/// In 3D mode the cursor model is positioned relative to the current [`TerminalPlane`] transform
/// and warp amount.
pub(crate) fn sync_asset_to_terminal_cursor(mut params: CursorSyncParams) {
    let CursorSyncParams {
        app_config,
        runtime,
        terminal,
        viewport,
        camera_slots,
        mobius_transition,
        plane_warp,
        time,
        plane_query,
        query,
    } = &mut params;
    if query.is_empty() {
        return;
    }

    let pose_ctx = CursorPoseContext {
        runtime,
        terminal,
        viewport,
        mode: camera_slots.active().mode,
        plane_warp_amount: plane_warp.amount,
        mobius_progress: active_mobius_progress(camera_slots.active().mode, mobius_transition),
        elapsed_secs: time.elapsed_secs(),
        plane_query,
    };
    let (translation, rotation, scale, cursor_visibility) = cursor_pose(app_config, &pose_ctx);
    for (mut transform, mut visibility) in query.iter_mut() {
        transform.translation = translation;
        transform.rotation = rotation;
        transform.scale = Vec3::splat(scale.max(0.001));
        *visibility = cursor_visibility;
    }
}

fn cursor_pose(
    app_config: &AppConfig,
    ctx: &CursorPoseContext<'_, '_, '_>,
) -> (Vec3, Quat, f32, Visibility) {
    let cols = ctx.terminal.cols.max(1) as f32;
    let rows = ctx.terminal.rows.max(1) as f32;
    let cell_width = ctx.viewport.size.x / cols;
    let cell_height = ctx.viewport.size.y / rows;
    let scale = cell_width.min(cell_height) * app_config.cursor.model.scale_factor;

    let (cursor_row, cursor_col) = vt::cursor_position(&ctx.runtime.term);
    let cursor_col = cursor_col.min(ctx.terminal.cols.saturating_sub(1)) as f32;
    let cursor_row = cursor_row.min(ctx.terminal.rows.saturating_sub(1)) as f32;

    let cursor_x = cursor_col + 0.5 + app_config.cursor.model.x_offset;
    let local_x = ctx.viewport.center.x - ctx.viewport.size.x * 0.5 + cursor_x * cell_width;
    let local_y =
        ctx.viewport.center.y + ctx.viewport.size.y * 0.5 - (cursor_row + 0.5) * cell_height;
    let spin = ctx.elapsed_secs * app_config.cursor.animation.spin_speed;
    let bob = (ctx.elapsed_secs * app_config.cursor.animation.bob_speed).sin()
        * cell_height
        * app_config.cursor.animation.bob_amplitude;
    let plane_bob = if ctx.viewport.size.y > 0.0 {
        bob / ctx.viewport.size.y
    } else {
        0.0
    };

    let (translation, rotation, visibility) = if !ctx.mode.is_3d() {
        (
            Vec3::new(local_x, local_y + bob, CURSOR_DEPTH),
            Quat::from_rotation_y(spin) * Quat::from_rotation_x(-0.25),
            if !app_config.cursor.model.visible || vt::cursor_hidden(&ctx.runtime.term) {
                Visibility::Hidden
            } else {
                Visibility::Visible
            },
        )
    } else {
        let Ok(plane_transform) = ctx.plane_query.single() else {
            return (Vec3::ZERO, Quat::IDENTITY, scale, Visibility::Hidden);
        };
        let plane_local_x = cursor_x / cols - 0.5;
        let plane_local_y = 0.5 - (cursor_row + 0.5) / rows + plane_bob;
        let local_position = plane_surface_point(
            ctx.mode,
            plane_local_x,
            plane_local_y,
            ctx.plane_warp_amount,
            ctx.elapsed_secs,
            app_config.cursor.model.plane_offset,
            ctx.mobius_progress,
        );
        (
            plane_transform.transform_point(local_position),
            plane_transform.rotation * (Quat::from_rotation_y(spin) * Quat::from_rotation_x(-0.25)),
            if app_config.cursor.model.visible {
                Visibility::Visible
            } else {
                Visibility::Hidden
            },
        )
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

fn plane_surface_point(
    mode: TerminalPresentationMode,
    local_x: f32,
    local_y: f32,
    warp_amount: f32,
    elapsed_secs: f32,
    depth_offset: f32,
    mobius_progress: f32,
) -> Vec3 {
    match mode {
        TerminalPresentationMode::Flat2d => Vec3::new(local_x, local_y, depth_offset),
        TerminalPresentationMode::Plane3d | TerminalPresentationMode::Perspective3d => Vec3::new(
            local_x,
            local_y,
            -plane_surface_z(local_x, local_y, warp_amount, elapsed_secs) + depth_offset,
        ),
        TerminalPresentationMode::Mobius3d => {
            let source_point = Vec3::new(local_x, local_y, depth_offset);
            let target_point =
                mobius_surface_point(local_x, local_y, warp_amount, elapsed_secs, depth_offset);
            source_point.lerp(target_point, mobius_progress)
        }
    }
}

fn mobius_surface_point(
    local_x: f32,
    local_y: f32,
    warp_amount: f32,
    elapsed_secs: f32,
    depth_offset: f32,
) -> Vec3 {
    let twist = 1.0 + warp_amount * 0.06 * (elapsed_secs * 0.7).sin();
    let angle = (local_x + 0.5) * std::f32::consts::TAU;
    let radius = 0.24 + warp_amount * 0.015;
    let width = local_y * (0.42 + warp_amount * 0.04);
    let half_angle = angle * 0.5 * twist;
    let cos_half = half_angle.cos();
    let sin_half = half_angle.sin();
    let ring = radius + width * cos_half;

    Vec3::new(
        ring * angle.cos(),
        ring * angle.sin(),
        width * sin_half * 320.0 + depth_offset,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::MobiusEnterZoomFloor;

    #[derive(Resource, Default)]
    struct CameraChangedProbe(bool);

    #[derive(Resource, Default)]
    struct VisibilityChangedProbe(usize);

    fn record_camera_change(
        camera_slots: Res<TerminalCameraSlots>,
        mut probe: ResMut<CameraChangedProbe>,
    ) {
        probe.0 = camera_slots.is_changed();
    }

    fn record_visibility_changes(
        visibility: Query<(), Changed<Visibility>>,
        mut probe: ResMut<VisibilityChangedProbe>,
    ) {
        probe.0 = visibility.iter().count();
    }

    #[test]
    fn brightness_system_does_not_mutate_camera_state() {
        let mut app = App::new();
        app.init_resource::<AppConfig>()
            .init_resource::<TerminalInlineObjects>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<TerminalCameraSlots>()
            .init_resource::<CameraChangedProbe>()
            .add_systems(
                Update,
                (
                    apply_instance_brightness,
                    record_camera_change.after(apply_instance_brightness),
                ),
            );

        app.update();
        app.world_mut().resource_mut::<CameraChangedProbe>().0 = false;
        app.update();

        assert!(!app.world().resource::<CameraChangedProbe>().0);
    }

    #[test]
    fn pose_updates_do_not_mark_inline_visibility_changed() {
        let mut app = App::new();
        app.init_resource::<TerminalCameraSlots>()
            .init_resource::<VisibilityChangedProbe>()
            .add_systems(
                Update,
                (apply_inline_objects, record_visibility_changes).chain(),
            );
        app.world_mut()
            .spawn((TerminalInlineObjectSprite, Visibility::Visible));
        app.world_mut()
            .spawn((TerminalInlineObjectPlane, Visibility::Hidden));
        app.update();
        app.world_mut().clear_trackers();
        app.world_mut()
            .resource_mut::<TerminalCameraSlots>()
            .active_mut()
            .pose
            .yaw += 0.25;

        app.update();

        assert_eq!(app.world().resource::<VisibilityChangedProbe>().0, 0);
    }

    #[test]
    fn perspective_uses_the_same_oriented_warp_as_orthographic_3d() {
        for mode in [
            TerminalPresentationMode::Plane3d,
            TerminalPresentationMode::Perspective3d,
        ] {
            let surface = plane_surface_point(mode, 0.0, 0.0, 0.75, 1.0, 0.0, 0.0);
            let object = plane_surface_point(mode, 0.0, 0.0, 0.75, 1.0, 8.0, 0.0);
            assert_eq!(
                oriented_plane_depth(mode, surface.z, 1.0),
                surface.z,
                "front mesh must use the shared object surface"
            );
            assert!((object.z - surface.z - 8.0).abs() < f32::EPSILON);
        }

        let orthographic = plane_surface_point(
            TerminalPresentationMode::Plane3d,
            0.0,
            0.0,
            0.75,
            1.0,
            0.0,
            0.0,
        );
        let perspective = plane_surface_point(
            TerminalPresentationMode::Perspective3d,
            0.0,
            0.0,
            0.75,
            1.0,
            0.0,
            0.0,
        );
        assert_eq!(perspective, orthographic);
    }

    #[test]
    fn every_3d_mode_animates_warped_kitty_planes() {
        for mode in [
            TerminalPresentationMode::Plane3d,
            TerminalPresentationMode::Perspective3d,
            TerminalPresentationMode::Mobius3d,
        ] {
            assert!(should_animate_inline_kitty_planes(
                mode, 0.5, false, false, false, false
            ));
        }
        assert!(!should_animate_inline_kitty_planes(
            TerminalPresentationMode::Flat2d,
            0.5,
            true,
            true,
            true,
            true
        ));
        assert!(should_animate_inline_kitty_planes(
            TerminalPresentationMode::Perspective3d,
            0.0,
            true,
            false,
            false,
            false
        ));
        assert!(should_animate_inline_kitty_planes(
            TerminalPresentationMode::Plane3d,
            0.0,
            false,
            true,
            false,
            false
        ));
        assert!(!should_animate_inline_kitty_planes(
            TerminalPresentationMode::Mobius3d,
            0.0,
            false,
            false,
            false,
            false
        ));
        assert!(should_animate_inline_kitty_planes(
            TerminalPresentationMode::Mobius3d,
            0.0,
            false,
            false,
            true,
            false
        ));
        assert!(should_animate_inline_kitty_planes(
            TerminalPresentationMode::Mobius3d,
            0.0,
            false,
            false,
            false,
            true
        ));
        assert!(!should_animate_inline_kitty_planes(
            TerminalPresentationMode::Perspective3d,
            0.0,
            false,
            false,
            false,
            false
        ));
    }

    #[test]
    fn settled_mobius_mode_stops_re_uploading_the_terminal_meshes() {
        // Settled strip, no warp, nothing changed: no idle upload.
        assert!(!should_animate_terminal_plane_warp(
            TerminalPresentationMode::Mobius3d,
            0.0,
            false,
            false,
            false
        ));
        // Transition frames, the finish frame, and state changes still update.
        assert!(should_animate_terminal_plane_warp(
            TerminalPresentationMode::Mobius3d,
            0.0,
            false,
            true,
            false
        ));
        assert!(should_animate_terminal_plane_warp(
            TerminalPresentationMode::Mobius3d,
            0.0,
            false,
            false,
            true
        ));
        assert!(should_animate_terminal_plane_warp(
            TerminalPresentationMode::Mobius3d,
            0.0,
            true,
            false,
            false
        ));
        assert!(should_animate_terminal_plane_warp(
            TerminalPresentationMode::Mobius3d,
            0.5,
            false,
            false,
            false
        ));
    }

    #[test]
    fn completed_mobius_transition_updates_the_final_kitty_frame() {
        let layout = InlineKittyPlaneLayout {
            local_x: 0.0,
            local_y: 0.0,
            local_width: 0.2,
            local_height: 0.2,
            x_segments: 2,
            y_segments: 2,
            source_rect: [0.0, 0.0, 1.0, 1.0],
        };
        let mesh =
            build_kitty_plane_mesh(&layout, TerminalPresentationMode::Mobius3d, 0.0, 0.0, 0.0);

        let mut slots = TerminalCameraSlots::default();
        slots.active_mut().mode = TerminalPresentationMode::Mobius3d;
        let pose = slots.active().pose;
        let mut transition = MobiusTransition::default();
        transition.begin_enter(
            0,
            &TerminalMobiusSource {
                mode: TerminalPresentationMode::Plane3d,
                pose,
            },
            &pose,
            MobiusEnterZoomFloor::KeyboardTarget,
        );
        transition.elapsed_secs = MobiusTransition::ZOOM_OUT_SECS;

        let mut app = App::new();
        app.insert_resource(slots)
            .insert_resource(transition)
            .init_resource::<TerminalPlaneWarp>()
            .init_resource::<Time>()
            .init_resource::<Assets<Mesh>>()
            .add_systems(
                Update,
                animate_inline_kitty_planes.after(animate_mobius_transition),
            )
            .add_systems(Update, animate_mobius_transition);
        let mesh_handle = app.world_mut().resource_mut::<Assets<Mesh>>().add(mesh);
        app.world_mut().spawn((
            TerminalInlineObjectPlane,
            layout,
            Mesh3d(mesh_handle.clone()),
        ));

        app.update();
        app.world_mut()
            .resource_mut::<MobiusTransition>()
            .elapsed_secs = MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS;
        app.update();

        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&mesh_handle).expect("Kitty mesh");
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("expected Kitty mesh positions");
        };
        let center = Vec3::from_array(positions[4]);
        let expected = plane_surface_point(
            TerminalPresentationMode::Mobius3d,
            0.0,
            0.0,
            0.0,
            0.0,
            1.5,
            1.0,
        );
        assert_eq!(center, expected);
    }

    #[test]
    fn kitty_plane_vertices_follow_the_mobius_surface() {
        let layout = InlineKittyPlaneLayout {
            local_x: 0.0,
            local_y: 0.0,
            local_width: 0.2,
            local_height: 0.2,
            x_segments: 2,
            y_segments: 2,
            source_rect: [0.0, 0.0, 1.0, 1.0],
        };
        let mesh =
            build_kitty_plane_mesh(&layout, TerminalPresentationMode::Mobius3d, 0.0, 0.0, 1.0);
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("expected Kitty mesh positions");
        };
        let center = Vec3::from_array(positions[4]);
        let expected = plane_surface_point(
            TerminalPresentationMode::Mobius3d,
            0.0,
            0.0,
            0.0,
            0.0,
            1.5,
            1.0,
        );

        assert_eq!(center, expected);
        assert_ne!(center.x, 0.0);
    }

    #[test]
    fn kitty_planes_are_visible_from_both_sides() {
        let image = Handle::<Image>::default();
        let material = kitty_plane_material(&image);

        assert_eq!(material.cull_mode, None);
    }

    #[test]
    fn keyboard_mobius_enter_finish_applies_the_target_zoom_floor() {
        let sub_unit_pose = crate::camera::TerminalCameraPose {
            orthographic_scale: 0.4,
            ..crate::camera::TerminalCameraPose::default()
        };
        let source = TerminalMobiusSource {
            mode: TerminalPresentationMode::Plane3d,
            pose: sub_unit_pose,
        };

        let mut slots = TerminalCameraSlots::default();
        slots.active_mut().mode = TerminalPresentationMode::Mobius3d;
        slots.active_mut().pose = sub_unit_pose;
        slots.active_mut().mobius_source = Some(source);

        let mut transition = MobiusTransition::default();
        transition.begin_enter(
            0,
            &source,
            &sub_unit_pose,
            MobiusEnterZoomFloor::KeyboardTarget,
        );
        transition.elapsed_secs = MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS;

        let mut app = App::new();
        app.insert_resource(slots)
            .insert_resource(transition)
            .init_resource::<Time>()
            .add_systems(Update, animate_mobius_transition);
        app.update();

        let preset = app.world().resource::<TerminalCameraSlots>().active();
        // The keyboard enter is the one finish write that is not a no-op: the
        // displayed pose is zoomed out to the target floor...
        assert_eq!(
            preset.pose.orthographic_scale,
            MobiusTransition::TARGET_ZOOM_MULTIPLIER
        );
        // ...while the saved source keeps the exact sub-unit scale so the
        // exit restores it.
        assert_eq!(
            preset
                .mobius_source
                .expect("Mobius source")
                .pose
                .orthographic_scale,
            0.4
        );
        assert!(!app.world().resource::<MobiusTransition>().active);
    }

    #[test]
    fn mobius_exit_restores_the_complete_per_slot_source_pose() {
        let source_pose = crate::camera::TerminalCameraPose {
            orthographic_scale: MIN_ORTHOGRAPHIC_SCALE,
            roll: 0.4,
            perspective_fov: 0.8,
            ..crate::camera::TerminalCameraPose::default()
        };
        let source = TerminalMobiusSource {
            mode: TerminalPresentationMode::Plane3d,
            pose: source_pose,
        };

        let mut slots = TerminalCameraSlots::default();
        slots.active_mut().mode = TerminalPresentationMode::Mobius3d;
        slots.active_mut().pose.orthographic_scale = 1.0;
        slots.active_mut().mobius_source = Some(source);

        let mut transition = MobiusTransition::default();
        transition.prepare_source(0, source.mode, &source.pose);
        let active_pose = slots.active().pose;
        transition.begin_exit(0, &active_pose, 1.0);
        transition.elapsed_secs = MobiusTransition::VIEW_RESET_SECS + MobiusTransition::MORPH_SECS;

        let mut app = App::new();
        app.insert_resource(slots)
            .insert_resource(transition)
            .init_resource::<Time>()
            .add_systems(Update, animate_mobius_transition);
        app.update();

        let preset = app.world().resource::<TerminalCameraSlots>().active();
        assert_eq!(preset.mode, source.mode);
        assert_eq!(preset.pose, source.pose);
        assert_eq!(preset.mobius_source, None);
    }
}

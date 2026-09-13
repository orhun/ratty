//! Bevy plugin wiring for the terminal application.

use bevy::prelude::*;
use bevy_terminal_ratatui::prelude::{
    TerminalPlugin as BevyTerminalPlugin, TerminalSystems as BevyTerminalSystems,
};

use crate::camera::{
    ActivateTerminalCameraPreset, TerminalCameraSlots, TerminalCameraSystemSet,
    TerminalCameraUpdate, activate_terminal_camera_presets, apply_terminal_camera_updates,
};
use crate::config::AppConfig;
use crate::inline::{
    TerminalInlineObjectPlane, TerminalInlineObjectSprite, TerminalInlineObjects, TerminalRgpObject,
};
use crate::keyboard::{TerminalClipboard, TerminalKeyBindings, handle_keyboard_input};
use crate::mouse::{TerminalSelection, handle_mouse_input};
use crate::present::TerminalPresentPlugin;
use crate::scene::{
    MobiusTransition, TerminalPresentationMode, apply_terminal_presentation, setup_scene,
    spawn_terminal_renderer,
};
use crate::systems::{
    TerminalFrameDirty, TerminalRedrawSet, animate_inline_kitty_planes, animate_mobius_transition,
    animate_terminal_plane_warp, apply_inline_objects, apply_instance_brightness,
    finish_terminal_model_load, handle_window_resize, pump_pty_output, render_terminal_widget,
    request_exit_on_primary_window_close, retry_pending_terminal_resize, reveal_window_fallback,
    shutdown_terminal_runtime_on_exit, sync_asset_to_terminal_cursor, sync_inline_objects,
    sync_rgp_objects, sync_terminal_materials, sync_terminal_render_output,
    sync_terminal_renderer_config,
};
use crate::terminal::TerminalRedrawState;

/// Inline object entities spawned since the visibility pass last ran.
type AddedInlineObjects<'w, 's> = Query<
    'w,
    's,
    (),
    Or<(
        Added<TerminalInlineObjectSprite>,
        Added<TerminalInlineObjectPlane>,
    )>,
>;

/// Main terminal plugin.
pub struct TerminalPlugin;

impl Plugin for TerminalPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerminalSelection>()
            .init_resource::<TerminalInlineObjects>()
            .init_resource::<TerminalRedrawState>()
            .init_resource::<TerminalKeyBindings>()
            .init_resource::<TerminalFrameDirty>()
            .init_non_send::<TerminalClipboard>()
            .add_message::<TerminalCameraUpdate>()
            .add_message::<ActivateTerminalCameraPreset>()
            .add_systems(Startup, setup_scene)
            .add_systems(
                PostUpdate,
                spawn_terminal_renderer.after(bevy::text::load_font_assets_into_font_collection),
            )
            .add_systems(Update, request_exit_on_primary_window_close)
            .add_systems(Update, reveal_window_fallback)
            .add_systems(Update, pump_pty_output)
            .configure_sets(
                Update,
                (
                    TerminalCameraSystemSet::ProtocolUpdates,
                    TerminalCameraSystemSet::KeyboardInput,
                    TerminalCameraSystemSet::Activation,
                    TerminalCameraSystemSet::MouseInput,
                    TerminalCameraSystemSet::Transition,
                    TerminalCameraSystemSet::Synchronize,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                apply_terminal_camera_updates
                    .after(pump_pty_output)
                    .in_set(TerminalCameraSystemSet::ProtocolUpdates),
            )
            .add_systems(
                Update,
                handle_keyboard_input.in_set(TerminalCameraSystemSet::KeyboardInput),
            )
            .add_systems(
                Update,
                activate_terminal_camera_presets.in_set(TerminalCameraSystemSet::Activation),
            )
            .add_systems(
                Update,
                handle_mouse_input.in_set(TerminalCameraSystemSet::MouseInput),
            )
            .add_systems(
                Update,
                animate_mobius_transition
                    .run_if(
                        |camera_slots: Res<TerminalCameraSlots>,
                         mobius_transition: Res<MobiusTransition>| {
                            camera_slots.active().mode == TerminalPresentationMode::Mobius3d
                                || mobius_transition.active
                        },
                    )
                    .in_set(TerminalCameraSystemSet::Transition),
            )
            .add_systems(Update, handle_window_resize)
            .add_systems(
                Update,
                retry_pending_terminal_resize.after(handle_window_resize),
            )
            .add_systems(
                Update,
                apply_terminal_presentation
                    .run_if(
                        |camera_slots: Res<TerminalCameraSlots>,
                         mobius_transition: Res<MobiusTransition>| {
                            camera_slots.is_changed() || mobius_transition.is_changed()
                        },
                    )
                    .in_set(TerminalCameraSystemSet::Synchronize),
            )
            .add_systems(
                Update,
                apply_inline_objects
                    .after(apply_terminal_presentation)
                    .run_if(
                        |camera_slots: Res<TerminalCameraSlots>, added: AddedInlineObjects| {
                            camera_slots.is_changed() || !added.is_empty()
                        },
                    ),
            )
            .add_systems(
                Update,
                render_terminal_widget
                    .after(TerminalCameraSystemSet::MouseInput)
                    .after(handle_window_resize)
                    .after(pump_pty_output)
                    .after(retry_pending_terminal_resize)
                    .before(BevyTerminalSystems::Sync),
            )
            .add_systems(
                Update,
                sync_terminal_renderer_config
                    .after(render_terminal_widget)
                    .before(BevyTerminalSystems::Sync),
            )
            .configure_sets(
                Update,
                TerminalRedrawSet
                    .after(BevyTerminalSystems::Sync)
                    .after(sync_terminal_renderer_config),
            )
            .add_systems(
                Update,
                (
                    sync_terminal_render_output,
                    sync_terminal_materials,
                    finish_terminal_model_load,
                )
                    .chain()
                    .in_set(TerminalRedrawSet),
            )
            .add_systems(
                Update,
                sync_inline_objects
                    .after(TerminalRedrawSet)
                    // Deterministic vs the Transition set: on the frame a
                    // Mobius exit finishes, spawned inline entities must see
                    // the restored mode, not race it.
                    .after(TerminalCameraSystemSet::Transition),
            )
            .add_systems(
                Update,
                animate_inline_kitty_planes
                    .after(sync_inline_objects)
                    .after(TerminalCameraSystemSet::Transition),
            )
            .add_systems(
                Update,
                sync_rgp_objects
                    .after(sync_inline_objects)
                    .after(TerminalCameraSystemSet::Synchronize)
                    .run_if(|objects: Query<(), With<TerminalRgpObject>>| !objects.is_empty()),
            )
            .add_systems(Update, apply_instance_brightness.after(sync_rgp_objects))
            .add_systems(
                Update,
                animate_terminal_plane_warp
                    .after(TerminalCameraSystemSet::Transition)
                    .run_if(|camera_slots: Res<TerminalCameraSlots>| {
                        camera_slots.active().mode != TerminalPresentationMode::Flat2d
                    }),
            )
            .add_systems(
                Update,
                sync_asset_to_terminal_cursor
                    .after(TerminalRedrawSet)
                    .after(TerminalCameraSystemSet::Synchronize)
                    .run_if(|config: Res<AppConfig>| config.cursor.model.visible),
            )
            .add_systems(Last, shutdown_terminal_runtime_on_exit)
            .add_plugins(BevyTerminalPlugin)
            .add_plugins(TerminalPresentPlugin);
    }
}

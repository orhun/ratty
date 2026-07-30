//! Terminal camera presets, protocol updates, and activation systems.

use bevy::prelude::*;

use crate::scene::{MobiusEnterZoomFloor, MobiusTransition, TerminalPresentationMode};

/// Number of addressable terminal camera presets.
pub const TERMINAL_CAMERA_SLOT_COUNT: usize = 10;
/// Smallest valid perspective field of view in radians (about 2.87 degrees).
/// The RGP `fov` field arrives in degrees and is converted during parsing, so
/// every value checked against this clamp is already radians.
pub const MIN_PERSPECTIVE_FOV: f32 = 0.05;
/// Largest valid perspective field of view in radians (about 177.13 degrees).
pub const MAX_PERSPECTIVE_FOV: f32 = std::f32::consts::PI - 0.05;
/// Smallest valid orthographic projection scale, shared by protocol updates
/// and interactive wheel zoom.
pub const MIN_ORTHOGRAPHIC_SCALE: f32 = 0.01;

/// Ordered camera pipeline stages with actual resource dependencies.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TerminalCameraSystemSet {
    /// Apply parsed RGP partial updates.
    ProtocolUpdates,
    /// Apply keyboard mode changes and queue slot activations.
    KeyboardInput,
    /// Activate presets after their updates and keyboard requests.
    Activation,
    /// Apply mouse interaction to the active pose.
    MouseInput,
    /// Advance explicit Mobius transitions.
    Transition,
    /// Synchronize the resulting state to Bevy entities.
    Synchronize,
}

/// Persistent pose for a terminal camera preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalCameraPose {
    /// Rotation around the vertical axis, in radians.
    pub yaw: f32,
    /// Rotation around the horizontal axis, in radians.
    pub pitch: f32,
    /// Rotation around the view axis, in radians.
    pub roll: f32,
    /// Orthographic projection scale.
    pub orthographic_scale: f32,
    /// Perspective vertical field of view in radians.
    pub perspective_fov: f32,
    /// Camera translation relative to its default position.
    pub translation: Vec3,
}

impl TerminalCameraPose {
    /// Returns a finite, positive orthographic scale.
    pub fn resolved_orthographic_scale(self) -> f32 {
        if self.orthographic_scale.is_finite() {
            self.orthographic_scale.max(MIN_ORTHOGRAPHIC_SCALE)
        } else {
            1.0
        }
    }

    /// Returns a finite perspective field of view away from zero and pi.
    pub fn resolved_perspective_fov(self) -> f32 {
        if self.perspective_fov.is_finite() {
            self.perspective_fov
                .clamp(MIN_PERSPECTIVE_FOV, MAX_PERSPECTIVE_FOV)
        } else {
            1.0
        }
    }
}

impl Default for TerminalCameraPose {
    fn default() -> Self {
        Self {
            yaw: 0.18,
            pitch: 0.08,
            roll: 0.0,
            orthographic_scale: 1.0,
            perspective_fov: 1.0,
            translation: Vec3::ZERO,
        }
    }
}

/// Presentation mode and pose stored in one camera slot.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct TerminalCameraPreset {
    /// Presentation mode selected by the preset.
    pub mode: TerminalPresentationMode,
    /// Persistent camera pose selected by the preset.
    pub pose: TerminalCameraPose,
    /// Presentation state restored when leaving Mobius mode.
    pub mobius_source: Option<TerminalMobiusSource>,
}

/// Per-slot presentation state saved before entering Mobius mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalMobiusSource {
    /// Presentation mode restored after the exit transition.
    pub mode: TerminalPresentationMode,
    /// Camera pose restored after the exit transition.
    pub pose: TerminalCameraPose,
}

/// The ten persistent camera presets and the active slot.
#[derive(Resource, Clone, Debug)]
pub struct TerminalCameraSlots {
    presets: [TerminalCameraPreset; TERMINAL_CAMERA_SLOT_COUNT],
    active_slot: usize,
}

impl TerminalCameraSlots {
    /// Returns the active slot index.
    pub fn active_slot(&self) -> usize {
        self.active_slot
    }

    /// Returns the active preset.
    pub fn active(&self) -> &TerminalCameraPreset {
        &self.presets[self.active_slot]
    }

    /// Returns the active preset for a user interaction update.
    pub fn active_mut(&mut self) -> &mut TerminalCameraPreset {
        &mut self.presets[self.active_slot]
    }

    /// Returns a preset by slot index.
    pub fn preset(&self, slot: usize) -> Option<&TerminalCameraPreset> {
        self.presets.get(slot)
    }

    /// Activates a preset, returning whether the active slot changed.
    pub fn activate(&mut self, slot: usize) -> bool {
        if slot >= TERMINAL_CAMERA_SLOT_COUNT || self.active_slot == slot {
            return false;
        }
        self.active_slot = slot;
        true
    }

    fn updated_preset(&self, update: TerminalCameraUpdate) -> Option<TerminalCameraPreset> {
        let current = self.presets.get(update.slot).copied()?;
        let mut next = current;

        if let Some(mode) = update.mode {
            next.mode = mode;
        }
        apply_pose_update(&mut next.pose, update);

        let mode_changed = next.mode != current.mode;
        let pose_changed = next.pose != current.pose;
        if next.mode == TerminalPresentationMode::Mobius3d {
            if mode_changed {
                next.mobius_source = Some(TerminalMobiusSource {
                    mode: current.mode,
                    pose: next.pose,
                });
            } else {
                let source = current.mobius_source.unwrap_or(TerminalMobiusSource {
                    mode: TerminalPresentationMode::Plane3d,
                    pose: current.pose,
                });
                let mut updated_source = source;
                apply_pose_update(&mut updated_source.pose, update);
                if pose_changed || updated_source != source {
                    next.mobius_source = Some(updated_source);
                }
            }
        } else {
            next.mobius_source = None;
        }

        if next == current {
            return None;
        }
        Some(next)
    }

    #[cfg(test)]
    fn apply_update(&mut self, update: TerminalCameraUpdate) -> bool {
        let Some(next) = self.updated_preset(update) else {
            return false;
        };
        self.presets[update.slot] = next;
        true
    }
}

fn apply_pose_update(pose: &mut TerminalCameraPose, update: TerminalCameraUpdate) {
    if let Some(value) = update.translation.x {
        pose.translation.x = value;
    }
    if let Some(value) = update.translation.y {
        pose.translation.y = value;
    }
    if let Some(value) = update.translation.z {
        pose.translation.z = value;
    }
    if let Some(value) = update.rotation_degrees.x {
        pose.pitch = value.to_radians();
    }
    if let Some(value) = update.rotation_degrees.y {
        pose.yaw = value.to_radians();
    }
    if let Some(value) = update.rotation_degrees.z {
        pose.roll = value.to_radians();
    }
    // Scale and FOV target their independently stored projection values
    // regardless of the update's or slot's mode; out-of-range values leave
    // the previous value in place without invalidating the rest.
    if let Some(value) = update.scale
        && value >= MIN_ORTHOGRAPHIC_SCALE
    {
        pose.orthographic_scale = value;
    }
    if let Some(value) = update.fov
        && (MIN_PERSPECTIVE_FOV..=MAX_PERSPECTIVE_FOV).contains(&value)
    {
        pose.perspective_fov = value;
    }
}

impl Default for TerminalCameraSlots {
    fn default() -> Self {
        Self {
            presets: [TerminalCameraPreset::default(); TERMINAL_CAMERA_SLOT_COUNT],
            active_slot: 0,
        }
    }
}

/// Transient mouse interaction state, kept outside persistent presets.
#[derive(Resource, Default)]
pub struct TerminalCameraInteraction {
    /// Indicates an active rotation drag.
    pub rotating: bool,
    /// Indicates an active panning drag.
    pub panning: bool,
    /// Previous cursor position during rotation.
    pub last_rotate_cursor: Option<Vec2>,
    /// Previous cursor position during panning.
    pub last_pan_cursor: Option<Vec2>,
}

impl TerminalCameraInteraction {
    /// Clears transient drag state after a mode or slot transition.
    pub fn reset(&mut self) {
        self.rotating = false;
        self.panning = false;
        self.last_rotate_cursor = None;
        self.last_pan_cursor = None;
    }
}

/// Optional vector components in a partial camera update.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OptionalVec3 {
    /// Optional X component.
    pub x: Option<f32>,
    /// Optional Y component.
    pub y: Option<f32>,
    /// Optional Z component.
    pub z: Option<f32>,
}

impl From<[Option<f32>; 3]> for OptionalVec3 {
    fn from(value: [Option<f32>; 3]) -> Self {
        Self {
            x: value[0],
            y: value[1],
            z: value[2],
        }
    }
}

/// Parsed partial camera update queued by the RGP input path.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct TerminalCameraUpdate {
    /// Slot to update.
    pub slot: usize,
    /// Whether to activate the slot after applying the update.
    pub activate: bool,
    /// Optional presentation mode.
    pub mode: Option<TerminalPresentationMode>,
    /// Optional orthographic scale; ignored below [`MIN_ORTHOGRAPHIC_SCALE`].
    pub scale: Option<f32>,
    /// Optional perspective FOV in radians — the RGP parser converts the
    /// wire's degrees before constructing this update. Ignored outside
    /// [`MIN_PERSPECTIVE_FOV`]`..=`[`MAX_PERSPECTIVE_FOV`].
    pub fov: Option<f32>,
    /// Optional translation components.
    pub translation: OptionalVec3,
    /// Optional rotation components in degrees, mapped as pitch, yaw, and roll.
    pub rotation_degrees: OptionalVec3,
}

/// Request to activate one of the ten camera presets.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivateTerminalCameraPreset {
    /// Slot to activate.
    pub slot: usize,
}

/// Applies queued RGP partial updates before any requested activation.
pub fn apply_terminal_camera_updates(
    mut updates: MessageReader<TerminalCameraUpdate>,
    mut activations: MessageWriter<ActivateTerminalCameraPreset>,
    mut slots: ResMut<TerminalCameraSlots>,
    mut interaction: ResMut<TerminalCameraInteraction>,
    mut mobius_transition: ResMut<MobiusTransition>,
) {
    for update in updates.read().copied() {
        let previous = slots.preset(update.slot).copied();
        let changed = if let Some(next) = slots.updated_preset(update) {
            slots.presets[update.slot] = next;
            true
        } else {
            false
        };
        if changed && let Some(previous) = previous {
            let next = slots.presets[update.slot];
            let mode_changed = next.mode != previous.mode;
            if update.slot == slots.active_slot() || update.activate {
                mobius_transition.stop();
                if next.mode == TerminalPresentationMode::Mobius3d {
                    let source = next.mobius_source.unwrap_or(TerminalMobiusSource {
                        mode: TerminalPresentationMode::Plane3d,
                        pose: next.pose,
                    });
                    if mode_changed && update.slot == slots.active_slot() {
                        // A protocol switch into Mobius on the displayed slot
                        // animates like the keyboard toggle, but honors the
                        // requested scale exactly: begin_enter recomputes the
                        // zoom floor from its argument on every call, so no
                        // earlier keyboard floor can persist. The stop()
                        // above only makes this a fresh enter whose start
                        // values snap to the live pose — the documented
                        // protocol-cancel semantics. Pose-only updates while
                        // already in Mobius stay immediate.
                        mobius_transition.begin_enter(
                            update.slot,
                            &source,
                            &next.pose,
                            MobiusEnterZoomFloor::ProtocolExact,
                        );
                    } else {
                        mobius_transition.prepare_source(update.slot, source.mode, &source.pose);
                    }
                }
            }
            if mode_changed && update.slot == slots.active_slot() {
                interaction.reset();
            }
        }
        if update.activate {
            activations.write(ActivateTerminalCameraPreset { slot: update.slot });
        }
    }
}

/// Activates requested presets after their partial RGP updates have been applied.
pub fn activate_terminal_camera_presets(
    mut activations: MessageReader<ActivateTerminalCameraPreset>,
    mut slots: ResMut<TerminalCameraSlots>,
    mut interaction: ResMut<TerminalCameraInteraction>,
    mut mobius_transition: ResMut<MobiusTransition>,
) {
    for activation in activations.read().copied() {
        let was_mobius = slots.active().mode == TerminalPresentationMode::Mobius3d;
        let strip_in_flight = was_mobius && mobius_transition.active;
        if slots.activate(activation.slot) {
            interaction.reset();
            if slots.active().mode != TerminalPresentationMode::Mobius3d {
                mobius_transition.stop();
                continue;
            }
            let source = slots
                .active()
                .mobius_source
                .unwrap_or(TerminalMobiusSource {
                    mode: TerminalPresentationMode::Plane3d,
                    pose: slots.active().pose,
                });
            if slots.active().mobius_source.is_none() {
                slots.active_mut().mobius_source = Some(source);
            }
            if was_mobius && !strip_in_flight {
                // A settled strip was displayed by the previous slot; keep it
                // settled and just re-point the exit source.
                mobius_transition.stop();
                mobius_transition.prepare_source(activation.slot, source.mode, &source.pose);
            } else if strip_in_flight {
                // The strip animation is mid-flight; re-own its source for
                // the new slot WITHOUT stopping so begin_enter resumes from
                // the currently displayed morph and camera values instead of
                // snapping the strip.
                mobius_transition.prepare_source(activation.slot, source.mode, &source.pose);
                mobius_transition.begin_enter(
                    activation.slot,
                    &source,
                    &slots.active().pose,
                    MobiusEnterZoomFloor::ProtocolExact,
                );
            } else {
                // Activating a Mobius slot from a non-Mobius view animates
                // the strip in like the keyboard toggle, but at the slot's
                // exact stored scale (recomputed from the zoom_floor argument
                // on every begin_enter) so activation never rewrites the
                // preset at finish. The stop() only forces a fresh enter
                // whose start values snap to the newly displayed pose.
                mobius_transition.stop();
                mobius_transition.begin_enter(
                    activation.slot,
                    &source,
                    &slots.active().pose,
                    MobiusEnterZoomFloor::ProtocolExact,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalRedrawState;

    fn update(slot: usize) -> TerminalCameraUpdate {
        TerminalCameraUpdate {
            slot,
            activate: false,
            mode: None,
            scale: None,
            fov: None,
            translation: OptionalVec3::default(),
            rotation_degrees: OptionalVec3::default(),
        }
    }

    #[test]
    fn partial_updates_preserve_omitted_pose_fields() {
        let mut slots = TerminalCameraSlots::default();
        let original = *slots.preset(3).expect("preset");
        let mut partial = update(3);
        partial.translation.x = Some(12.0);
        partial.rotation_degrees.y = Some(90.0);

        assert!(slots.apply_update(partial));
        let preset = slots.preset(3).expect("preset");
        assert_eq!(preset.mode, original.mode);
        assert_eq!(preset.pose.translation, Vec3::new(12.0, 0.0, 0.0));
        assert_eq!(preset.pose.yaw, std::f32::consts::FRAC_PI_2);
        assert_eq!(preset.pose.pitch, original.pose.pitch);
        assert_eq!(preset.pose.roll, original.pose.roll);
        assert_eq!(
            preset.pose.orthographic_scale,
            original.pose.orthographic_scale
        );
        assert_eq!(preset.pose.perspective_fov, original.pose.perspective_fov);
    }

    #[test]
    fn protocol_axes_map_to_pitch_yaw_and_roll_in_degrees() {
        let mut slots = TerminalCameraSlots::default();
        let mut partial = update(0);
        partial.rotation_degrees = OptionalVec3 {
            x: Some(10.0),
            y: Some(20.0),
            z: Some(30.0),
        };

        assert!(slots.apply_update(partial));
        let pose = slots.active().pose;
        assert_eq!(pose.pitch, 10.0_f32.to_radians());
        assert_eq!(pose.yaw, 20.0_f32.to_radians());
        assert_eq!(pose.roll, 30.0_f32.to_radians());
    }

    #[test]
    fn invalid_projection_values_do_not_replace_the_previous_value() {
        let mut slots = TerminalCameraSlots::default();
        let mut partial = update(0);
        partial.mode = Some(TerminalPresentationMode::Plane3d);
        partial.scale = Some(MIN_ORTHOGRAPHIC_SCALE / 2.0);

        assert!(slots.apply_update(partial));
        assert_eq!(slots.active().mode, TerminalPresentationMode::Plane3d);
        assert_eq!(slots.active().pose.orthographic_scale, 1.0);

        let mut partial = update(0);
        partial.mode = Some(TerminalPresentationMode::Perspective3d);
        partial.fov = Some(std::f32::consts::PI);

        assert!(slots.apply_update(partial));
        assert_eq!(slots.active().mode, TerminalPresentationMode::Perspective3d);
        assert_eq!(slots.active().pose.perspective_fov, 1.0);
    }

    #[test]
    fn scale_and_fov_update_their_stored_values_in_any_mode() {
        let mut slots = TerminalCameraSlots::default();
        let mut partial = update(0);
        partial.scale = Some(1.5);
        partial.fov = Some(0.9);

        assert!(slots.apply_update(partial));
        assert_eq!(slots.active().mode, TerminalPresentationMode::Flat2d);
        assert_eq!(slots.active().pose.orthographic_scale, 1.5);
        assert_eq!(slots.active().pose.perspective_fov, 0.9);

        let mut partial = update(0);
        partial.mode = Some(TerminalPresentationMode::Perspective3d);
        partial.scale = Some(2.0);

        assert!(slots.apply_update(partial));
        assert_eq!(slots.active().pose.orthographic_scale, 2.0);
        assert_eq!(slots.active().pose.perspective_fov, 0.9);
    }

    #[test]
    fn fov_applies_only_inside_the_radian_clamp() {
        let mut slots = TerminalCameraSlots::default();
        let mut partial = update(0);
        partial.fov = Some(MIN_PERSPECTIVE_FOV);
        assert!(slots.apply_update(partial));
        assert_eq!(slots.active().pose.perspective_fov, MIN_PERSPECTIVE_FOV);

        let mut partial = update(0);
        partial.fov = Some(MAX_PERSPECTIVE_FOV);
        assert!(slots.apply_update(partial));
        assert_eq!(slots.active().pose.perspective_fov, MAX_PERSPECTIVE_FOV);

        let mut partial = update(0);
        partial.fov = Some(MIN_PERSPECTIVE_FOV - 0.001);
        assert!(!slots.apply_update(partial));

        let mut partial = update(0);
        partial.fov = Some(MAX_PERSPECTIVE_FOV + 0.001);
        assert!(!slots.apply_update(partial));
        assert_eq!(slots.active().pose.perspective_fov, MAX_PERSPECTIVE_FOV);
    }

    #[test]
    fn activation_rejects_out_of_range_slots() {
        let mut slots = TerminalCameraSlots::default();
        assert!(!slots.activate(TERMINAL_CAMERA_SLOT_COUNT));
        assert_eq!(slots.active_slot(), 0);
    }

    #[test]
    fn projections_are_always_finite_and_valid() {
        let mut pose = TerminalCameraPose::default();
        for value in [f32::NAN, f32::INFINITY, -1.0, 0.0, std::f32::consts::PI] {
            pose.orthographic_scale = value;
            pose.perspective_fov = value;
            assert!(pose.resolved_orthographic_scale().is_finite());
            assert!(pose.resolved_orthographic_scale() > 0.0);
            assert!(pose.resolved_perspective_fov().is_finite());
            assert!(pose.resolved_perspective_fov() > 0.0);
            assert!(pose.resolved_perspective_fov() < std::f32::consts::PI);
        }
    }

    #[test]
    fn orthographic_zoom_does_not_change_perspective_fov() {
        let mut slots = TerminalCameraSlots::default();
        slots.active_mut().mode = TerminalPresentationMode::Plane3d;
        slots.active_mut().pose.orthographic_scale = 4.0;
        let expected_fov = slots.active().pose.perspective_fov;

        slots.active_mut().mode = TerminalPresentationMode::Perspective3d;

        assert_eq!(slots.active().pose.perspective_fov, expected_fov);
        assert_eq!(slots.active().pose.resolved_perspective_fov(), 1.0);
    }

    #[test]
    fn immediate_activation_happens_after_the_slot_update() {
        let mut app = App::new();
        app.init_resource::<TerminalCameraSlots>()
            .init_resource::<TerminalCameraInteraction>()
            .init_resource::<MobiusTransition>()
            .add_message::<TerminalCameraUpdate>()
            .add_message::<ActivateTerminalCameraPreset>()
            .add_systems(
                Update,
                (
                    apply_terminal_camera_updates,
                    activate_terminal_camera_presets,
                )
                    .chain(),
            );
        let mut command = update(4);
        command.activate = true;
        command.mode = Some(TerminalPresentationMode::Perspective3d);
        command.translation.z = Some(50.0);
        app.world_mut().write_message(command);

        app.update();

        let slots = app.world().resource::<TerminalCameraSlots>();
        assert_eq!(slots.active_slot(), 4);
        assert_eq!(slots.active().mode, TerminalPresentationMode::Perspective3d);
        assert_eq!(slots.active().pose.translation.z, 50.0);
    }

    #[test]
    fn mobius_exit_sources_are_persistent_per_slot() {
        let mut app = App::new();
        app.init_resource::<TerminalCameraSlots>()
            .init_resource::<TerminalCameraInteraction>()
            .init_resource::<MobiusTransition>()
            .add_message::<TerminalCameraUpdate>()
            .add_message::<ActivateTerminalCameraPreset>()
            .add_systems(
                Update,
                (
                    apply_terminal_camera_updates,
                    activate_terminal_camera_presets,
                )
                    .chain(),
            );

        let mut slot_zero = update(0);
        slot_zero.mode = Some(TerminalPresentationMode::Mobius3d);
        app.world_mut().write_message(slot_zero);
        app.update();

        let mut slot_one = update(1);
        slot_one.mode = Some(TerminalPresentationMode::Perspective3d);
        app.world_mut().write_message(slot_one);
        app.update();

        let mut slot_one = update(1);
        slot_one.activate = true;
        slot_one.mode = Some(TerminalPresentationMode::Mobius3d);
        app.world_mut().write_message(slot_one);
        app.update();

        app.world_mut()
            .write_message(ActivateTerminalCameraPreset { slot: 0 });
        app.update();

        let slots = app.world().resource::<TerminalCameraSlots>();
        assert_eq!(slots.active_slot(), 0);
        assert_eq!(
            slots.active().mobius_source.expect("slot source").mode,
            TerminalPresentationMode::Flat2d
        );
        let transition = app.world().resource::<MobiusTransition>();
        assert!(transition.source_is_for(0));
        assert_eq!(transition.source_mode, TerminalPresentationMode::Flat2d);
    }

    #[test]
    fn activating_a_mobius_slot_starts_the_enter_transition() {
        use crate::scene::MobiusTransitionDirection;

        let mut app = App::new();
        app.init_resource::<TerminalCameraSlots>()
            .init_resource::<TerminalCameraInteraction>()
            .init_resource::<MobiusTransition>()
            .add_message::<TerminalCameraUpdate>()
            .add_message::<ActivateTerminalCameraPreset>()
            .add_systems(
                Update,
                (
                    apply_terminal_camera_updates,
                    activate_terminal_camera_presets,
                )
                    .chain(),
            );
        let mut command = update(3);
        command.activate = true;
        command.mode = Some(TerminalPresentationMode::Mobius3d);
        app.world_mut().write_message(command);

        app.update();

        let transition = app.world().resource::<MobiusTransition>();
        assert!(transition.active);
        assert!(matches!(
            transition.direction,
            MobiusTransitionDirection::Entering
        ));
        assert_eq!(transition.morph_progress(), 0.0);
        assert!(transition.source_is_for(3));

        // Switching between two Mobius slots keeps the settled strip instead
        // of collapsing it flat and re-morphing.
        app.world_mut().resource_mut::<MobiusTransition>().stop();
        let mut other = update(5);
        other.activate = true;
        other.mode = Some(TerminalPresentationMode::Mobius3d);
        app.world_mut().write_message(other);
        app.update();

        let transition = app.world().resource::<MobiusTransition>();
        assert!(!transition.active);
        assert!(transition.source_is_for(5));

        // A mid-flight strip animation is handed to the next Mobius slot
        // continuously instead of snapping to the settled strip.
        app.world_mut()
            .write_message(ActivateTerminalCameraPreset { slot: 1 });
        app.update();
        app.world_mut()
            .write_message(ActivateTerminalCameraPreset { slot: 3 });
        app.update();
        {
            let mut transition = app.world_mut().resource_mut::<MobiusTransition>();
            assert!(transition.active);
            transition.elapsed_secs =
                MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS * 0.4;
        }
        let morph_before = app.world().resource::<MobiusTransition>().morph_progress();
        assert!((morph_before - 0.4).abs() < 1e-6);
        app.world_mut()
            .write_message(ActivateTerminalCameraPreset { slot: 5 });
        app.update();

        let transition = app.world().resource::<MobiusTransition>();
        assert!(transition.active);
        assert!(transition.source_is_for(5));
        assert!((transition.morph_progress() - morph_before).abs() < 1e-6);
    }

    #[test]
    fn protocol_updates_cancel_an_active_mobius_transition() {
        let mut app = App::new();
        app.init_resource::<TerminalCameraSlots>()
            .init_resource::<TerminalCameraInteraction>()
            .init_resource::<MobiusTransition>()
            .add_message::<TerminalCameraUpdate>()
            .add_message::<ActivateTerminalCameraPreset>()
            .add_systems(Update, apply_terminal_camera_updates);

        let mut enter_mobius = update(0);
        enter_mobius.mode = Some(TerminalPresentationMode::Mobius3d);
        app.world_mut().write_message(enter_mobius);
        app.update();

        let preset = *app.world().resource::<TerminalCameraSlots>().active();
        let source = preset.mobius_source.expect("Mobius source");
        app.world_mut()
            .resource_mut::<MobiusTransition>()
            .begin_enter(
                0,
                &source,
                &preset.pose,
                MobiusEnterZoomFloor::KeyboardTarget,
            );

        let mut camera_update = update(0);
        camera_update.scale = Some(2.0);
        app.world_mut().write_message(camera_update);
        app.update();

        let slots = app.world().resource::<TerminalCameraSlots>();
        assert_eq!(slots.active().pose.orthographic_scale, 2.0);
        assert_eq!(
            slots
                .active()
                .mobius_source
                .expect("Mobius source")
                .pose
                .orthographic_scale,
            2.0
        );
        assert!(!app.world().resource::<MobiusTransition>().active);
    }

    #[test]
    fn partial_mobius_updates_preserve_omitted_source_fields() {
        let mut slots = TerminalCameraSlots::default();
        let mut source = update(0);
        source.mode = Some(TerminalPresentationMode::Plane3d);
        source.scale = Some(0.05);
        assert!(slots.apply_update(source));

        let mut enter_mobius = update(0);
        enter_mobius.mode = Some(TerminalPresentationMode::Mobius3d);
        assert!(slots.apply_update(enter_mobius));
        slots.active_mut().pose.orthographic_scale = 1.0;

        let mut rotate = update(0);
        rotate.rotation_degrees.y = Some(45.0);
        assert!(slots.apply_update(rotate));

        let source = slots.active().mobius_source.expect("Mobius source");
        assert_eq!(source.pose.orthographic_scale, 0.05);
        assert_eq!(source.pose.yaw, 45.0_f32.to_radians());
    }

    #[test]
    fn mobius_updates_compare_against_the_saved_source_pose() {
        let mut slots = TerminalCameraSlots::default();
        let mut source = update(0);
        source.mode = Some(TerminalPresentationMode::Plane3d);
        source.scale = Some(0.05);
        assert!(slots.apply_update(source));

        let mut enter_mobius = update(0);
        enter_mobius.mode = Some(TerminalPresentationMode::Mobius3d);
        assert!(slots.apply_update(enter_mobius));
        slots.active_mut().pose.orthographic_scale = 1.0;

        let mut match_live_pose = update(0);
        match_live_pose.scale = Some(1.0);
        assert!(slots.apply_update(match_live_pose));

        let source = slots.active().mobius_source.expect("Mobius source");
        assert_eq!(source.pose.orthographic_scale, 1.0);
    }

    #[test]
    fn application_updates_remain_after_user_interaction_and_slot_switches() {
        let mut slots = TerminalCameraSlots::default();
        let mut command = update(2);
        command.mode = Some(TerminalPresentationMode::Plane3d);
        command.translation.x = Some(25.0);
        slots.apply_update(command);
        slots.activate(2);

        slots.active_mut().pose.yaw += 0.5;
        let expected = *slots.active();
        slots.activate(1);
        slots.activate(2);

        assert_eq!(*slots.active(), expected);
    }

    #[test]
    fn camera_protocol_updates_do_not_request_terminal_texture_redraws() {
        let mut app = App::new();
        app.init_resource::<TerminalCameraSlots>()
            .init_resource::<TerminalCameraInteraction>()
            .init_resource::<MobiusTransition>()
            .init_resource::<TerminalRedrawState>()
            .add_message::<TerminalCameraUpdate>()
            .add_message::<ActivateTerminalCameraPreset>()
            .add_systems(
                Update,
                (
                    apply_terminal_camera_updates,
                    activate_terminal_camera_presets,
                )
                    .chain(),
            );
        assert!(app.world_mut().resource_mut::<TerminalRedrawState>().take());
        {
            let mut interaction = app.world_mut().resource_mut::<TerminalCameraInteraction>();
            interaction.rotating = true;
            interaction.last_rotate_cursor = Some(Vec2::new(10.0, 20.0));
        }
        let mut command = update(0);
        command.mode = Some(TerminalPresentationMode::Perspective3d);
        app.world_mut().write_message(command);

        app.update();

        assert!(!app.world_mut().resource_mut::<TerminalRedrawState>().take());
        let interaction = app.world().resource::<TerminalCameraInteraction>();
        assert!(!interaction.rotating);
        assert_eq!(interaction.last_rotate_cursor, None);
    }
}

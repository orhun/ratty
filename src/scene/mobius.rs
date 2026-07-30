//! Mobius view transition state and timing.

use bevy::prelude::*;

use crate::camera::{MIN_ORTHOGRAPHIC_SCALE, TerminalCameraPose, TerminalMobiusSource};

use super::TerminalPresentationMode;

/// Zoom floor applied when an enter transition finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobiusEnterZoomFloor {
    /// Zoom out to at least [`MobiusTransition::TARGET_ZOOM_MULTIPLIER`] so
    /// the keyboard toggle always reveals the whole strip; the finish
    /// write-back may raise a sub-unit preset scale.
    KeyboardTarget,
    /// Honor the stored preset scale exactly, so the finish write-back is a
    /// value-level no-op and protocol or activation enters never mutate the
    /// preset.
    ProtocolExact,
}

impl MobiusEnterZoomFloor {
    fn min_end_zoom(self) -> f32 {
        match self {
            Self::KeyboardTarget => MobiusTransition::TARGET_ZOOM_MULTIPLIER,
            Self::ProtocolExact => MIN_ORTHOGRAPHIC_SCALE,
        }
    }
}

/// Animated transition into the Mobius-strip terminal view.
#[derive(Resource)]
pub struct MobiusTransition {
    /// Indicates the transition is active.
    pub active: bool,
    /// Elapsed transition time in seconds.
    pub elapsed_secs: f32,
    /// Current transition direction.
    pub direction: MobiusTransitionDirection,
    /// Source mode before entering the Mobius view.
    pub source_mode: TerminalPresentationMode,
    /// Camera slot that owns the saved source state.
    source_slot: Option<usize>,
    /// Source zoom before entering the Mobius view.
    pub source_zoom: f32,
    /// Source camera yaw before entering the Mobius view.
    pub source_yaw: f32,
    /// Source camera pitch before entering the Mobius view.
    pub source_pitch: f32,
    /// Source camera roll before entering the Mobius view.
    pub source_roll: f32,
    /// Source camera translation before entering the Mobius view.
    pub source_translation: Vec3,
    /// Strip morph progress at the start of the active transition.
    pub start_morph: f32,
    /// Camera zoom at the start of the active transition.
    pub start_zoom: f32,
    /// Camera zoom at the end of the active transition.
    pub end_zoom: f32,
    /// Camera yaw at the start of the active transition.
    pub start_yaw: f32,
    /// Camera pitch at the start of the active transition.
    pub start_pitch: f32,
    /// Camera roll at the start of the active transition.
    pub start_roll: f32,
    /// Camera translation at the start of the active transition.
    pub start_translation: Vec3,
    /// Camera yaw at the end of the active transition.
    pub end_yaw: f32,
    /// Camera pitch at the end of the active transition.
    pub end_pitch: f32,
    /// Camera roll at the end of the active transition.
    pub end_roll: f32,
    /// Camera translation at the end of the active transition.
    pub end_translation: Vec3,
}

/// Direction of the Mobius transition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MobiusTransitionDirection {
    /// Entering the Mobius view.
    Entering,
    /// Leaving the Mobius view.
    Exiting,
}

impl MobiusTransition {
    /// Zoom-out phase duration in seconds.
    pub const ZOOM_OUT_SECS: f32 = 0.2;
    /// View-reset phase duration in seconds while exiting.
    pub const VIEW_RESET_SECS: f32 = 0.2;
    /// Strip morph phase duration in seconds.
    pub const MORPH_SECS: f32 = 0.9;
    /// Final zoom multiplier applied when the transition completes.
    pub const TARGET_ZOOM_MULTIPLIER: f32 = 1.0;

    /// Starts the entry transition toward `live_pose`, restoring to `source`
    /// on a later exit.
    ///
    /// When a transition is already active for `slot` (e.g. re-entering
    /// during an exit), the animation resumes from the currently displayed
    /// values and keeps the stored source instead of overwriting it.
    pub fn begin_enter(
        &mut self,
        slot: usize,
        source: &TerminalMobiusSource,
        live_pose: &TerminalCameraPose,
        zoom_floor: MobiusEnterZoomFloor,
    ) {
        let resume = self.active && self.source_is_for(slot);
        if resume
            && self.direction == MobiusTransitionDirection::Entering
            && self.end_zoom == live_pose.orthographic_scale.max(zoom_floor.min_end_zoom())
            && self.end_yaw == live_pose.yaw
            && self.end_pitch == live_pose.pitch
            && self.end_roll == live_pose.roll
            && self.end_translation == live_pose.translation
        {
            // Re-entering toward identical targets is a no-op; restarting
            // would reset the clock, letting rapid repeated activations keep
            // the transition open (and input gated) indefinitely.
            return;
        }
        let (start_morph, start_zoom, start_yaw, start_pitch, start_roll, start_translation) =
            if resume {
                (
                    self.morph_progress(),
                    self.current_zoom(),
                    self.current_yaw(),
                    self.current_pitch(),
                    self.current_roll(),
                    self.current_translation(),
                )
            } else {
                (
                    0.0,
                    live_pose.orthographic_scale,
                    live_pose.yaw,
                    live_pose.pitch,
                    live_pose.roll,
                    live_pose.translation,
                )
            };
        if !resume {
            self.prepare_source(slot, source.mode, &source.pose);
        }
        self.active = true;
        self.elapsed_secs = 0.0;
        self.direction = MobiusTransitionDirection::Entering;
        self.start_morph = start_morph;
        self.start_zoom = start_zoom;
        self.end_zoom = live_pose.orthographic_scale.max(zoom_floor.min_end_zoom());
        self.start_yaw = start_yaw;
        self.start_pitch = start_pitch;
        self.start_roll = start_roll;
        self.start_translation = start_translation;
        self.end_yaw = live_pose.yaw;
        self.end_pitch = live_pose.pitch;
        self.end_roll = live_pose.roll;
        self.end_translation = live_pose.translation;
    }

    /// Starts the exit transition back to the source mode.
    ///
    /// When a transition is already active (e.g. exiting during an enter), the
    /// animation resumes from the currently displayed values instead of
    /// snapping to the fully formed strip.
    pub fn begin_exit(&mut self, slot: usize, pose: &TerminalCameraPose, current_zoom: f32) {
        // Resume only from a transition that belongs to this slot; another
        // slot's in-flight values are never valid start values here.
        let resume = self.active && self.source_is_for(slot);
        if !self.source_is_for(slot) {
            self.prepare_source(slot, TerminalPresentationMode::Plane3d, pose);
        }
        let (start_morph, start_yaw, start_pitch, start_roll, start_translation) = if resume {
            (
                self.morph_progress(),
                self.current_yaw(),
                self.current_pitch(),
                self.current_roll(),
                self.current_translation(),
            )
        } else {
            (1.0, pose.yaw, pose.pitch, pose.roll, pose.translation)
        };
        self.active = true;
        self.elapsed_secs = 0.0;
        self.direction = MobiusTransitionDirection::Exiting;
        self.start_morph = start_morph;
        self.start_zoom = current_zoom;
        self.end_zoom = self.source_zoom.max(MIN_ORTHOGRAPHIC_SCALE);
        self.start_yaw = start_yaw;
        self.start_pitch = start_pitch;
        self.start_roll = start_roll;
        self.start_translation = start_translation;
        self.end_yaw = self.source_yaw;
        self.end_pitch = self.source_pitch;
        self.end_roll = self.source_roll;
        self.end_translation = self.source_translation;
    }

    /// Stops the transition and resets its timer.
    pub fn stop(&mut self) {
        self.active = false;
        self.elapsed_secs = 0.0;
    }

    /// Saves a stable source pose for a later Mobius exit from `slot`.
    pub fn prepare_source(
        &mut self,
        slot: usize,
        source_mode: TerminalPresentationMode,
        pose: &TerminalCameraPose,
    ) {
        self.source_slot = Some(slot);
        self.source_mode = if source_mode == TerminalPresentationMode::Mobius3d {
            TerminalPresentationMode::Plane3d
        } else {
            source_mode
        };
        self.source_zoom = pose.orthographic_scale;
        self.source_yaw = pose.yaw;
        self.source_pitch = pose.pitch;
        self.source_roll = pose.roll;
        self.source_translation = pose.translation;
    }

    /// Returns whether the saved source state belongs to `slot`.
    pub fn source_is_for(&self, slot: usize) -> bool {
        self.source_slot == Some(slot)
    }

    /// Returns the current zoom-out progress from `0.0` to `1.0` while entering.
    pub fn enter_zoom_progress(&self) -> f32 {
        (self.elapsed_secs / Self::ZOOM_OUT_SECS).clamp(0.0, 1.0)
    }

    /// Returns the morph phase duration for the active transition.
    ///
    /// A resumed transition covers only the remaining morph distance, so the
    /// phase shrinks proportionally and morph velocity stays continuous
    /// across interruptions instead of stretching the remainder over the full
    /// [`Self::MORPH_SECS`].
    fn morph_phase_secs(&self) -> f32 {
        let remaining = match self.direction {
            MobiusTransitionDirection::Entering => 1.0 - self.start_morph,
            MobiusTransitionDirection::Exiting => self.start_morph,
        };
        Self::MORPH_SECS * remaining
    }

    /// Returns how long the morph waits for the camera phase.
    ///
    /// Only fresh transitions hold the morph while the camera settles; a
    /// resumed transition must keep morphing immediately, otherwise every
    /// restart (e.g. rapid slot hand-offs) would re-freeze the morph for the
    /// hold duration and the transition could be kept from ever finishing.
    fn morph_hold_secs(&self) -> f32 {
        match self.direction {
            MobiusTransitionDirection::Entering if self.start_morph > 0.0 => 0.0,
            MobiusTransitionDirection::Entering => Self::ZOOM_OUT_SECS,
            MobiusTransitionDirection::Exiting if self.start_morph < 1.0 => 0.0,
            MobiusTransitionDirection::Exiting => Self::VIEW_RESET_SECS,
        }
    }

    /// Returns the raw progress through the (possibly shortened) morph phase.
    fn morph_phase_progress(&self) -> f32 {
        let phase_secs = self.morph_phase_secs();
        if phase_secs <= f32::EPSILON {
            return 1.0;
        }
        ((self.elapsed_secs - self.morph_hold_secs()) / phase_secs).clamp(0.0, 1.0)
    }

    /// Returns the current Mobius morph progress for the active direction.
    ///
    /// The morph resumes from `start_morph` so interrupting a transition never
    /// snaps the strip geometry.
    pub fn morph_progress(&self) -> f32 {
        match self.direction {
            MobiusTransitionDirection::Entering => {
                self.start_morph + (1.0 - self.start_morph) * self.morph_phase_progress()
            }
            MobiusTransitionDirection::Exiting => {
                self.start_morph * (1.0 - self.morph_phase_progress())
            }
        }
    }

    /// Returns the current animated camera zoom.
    pub fn current_zoom(&self) -> f32 {
        match self.direction {
            MobiusTransitionDirection::Entering => {
                let t = ease_in_out(self.enter_zoom_progress());
                self.start_zoom + (self.end_zoom - self.start_zoom) * t
            }
            MobiusTransitionDirection::Exiting => {
                let t = (self.elapsed_secs / Self::VIEW_RESET_SECS).clamp(0.0, 1.0);
                let t = ease_in_out(t);
                self.start_zoom + (self.end_zoom - self.start_zoom) * t
            }
        }
    }

    /// Returns the eased camera interpolation factor for the active direction.
    ///
    /// Fresh enters keep `start == end` so the entering lerp is a no-op there;
    /// it only moves the camera when an interrupted exit resumes entering.
    fn camera_lerp_t(&self) -> f32 {
        let phase_secs = match self.direction {
            MobiusTransitionDirection::Entering => Self::ZOOM_OUT_SECS,
            MobiusTransitionDirection::Exiting => Self::VIEW_RESET_SECS,
        };
        ease_in_out((self.elapsed_secs / phase_secs).clamp(0.0, 1.0))
    }

    /// Returns the current animated camera yaw.
    pub fn current_yaw(&self) -> f32 {
        self.start_yaw + (self.end_yaw - self.start_yaw) * self.camera_lerp_t()
    }

    /// Returns the current animated camera pitch.
    pub fn current_pitch(&self) -> f32 {
        self.start_pitch + (self.end_pitch - self.start_pitch) * self.camera_lerp_t()
    }

    /// Returns the current animated camera roll.
    pub fn current_roll(&self) -> f32 {
        self.start_roll + (self.end_roll - self.start_roll) * self.camera_lerp_t()
    }

    /// Returns the current animated camera translation.
    pub fn current_translation(&self) -> Vec3 {
        self.start_translation
            .lerp(self.end_translation, self.camera_lerp_t())
    }

    /// Returns whether the full transition has finished.
    pub fn finished(&self) -> bool {
        // Wait for both the camera phase (always the full hold window) and
        // the morph, which may start immediately on a resume.
        let camera_secs = match self.direction {
            MobiusTransitionDirection::Entering => Self::ZOOM_OUT_SECS,
            MobiusTransitionDirection::Exiting => Self::VIEW_RESET_SECS,
        };
        let morph_secs = self.morph_hold_secs() + self.morph_phase_secs();
        self.elapsed_secs >= morph_secs.max(camera_secs)
    }
}

impl Default for MobiusTransition {
    fn default() -> Self {
        Self {
            active: false,
            elapsed_secs: 0.0,
            direction: MobiusTransitionDirection::Entering,
            source_mode: TerminalPresentationMode::Flat2d,
            source_slot: None,
            source_zoom: 1.0,
            source_yaw: 0.0,
            source_pitch: 0.0,
            source_roll: 0.0,
            source_translation: Vec3::ZERO,
            start_morph: 0.0,
            start_zoom: 0.0,
            end_zoom: 0.0,
            start_yaw: 0.0,
            start_pitch: 0.0,
            start_roll: 0.0,
            start_translation: Vec3::ZERO,
            end_yaw: 0.0,
            end_pitch: 0.0,
            end_roll: 0.0,
            end_translation: Vec3::ZERO,
        }
    }
}

fn ease_in_out(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_transition_preserves_the_minimum_protocol_zoom() {
        let pose = TerminalCameraPose {
            orthographic_scale: MIN_ORTHOGRAPHIC_SCALE,
            ..TerminalCameraPose::default()
        };
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
        transition.begin_exit(0, &pose, 1.0);

        assert_eq!(transition.end_zoom, MIN_ORTHOGRAPHIC_SCALE);
    }

    #[test]
    fn protocol_enters_display_sub_unit_scales_exactly() {
        let pose = TerminalCameraPose {
            orthographic_scale: 0.4,
            ..TerminalCameraPose::default()
        };
        let mut transition = MobiusTransition::default();
        transition.begin_enter(
            0,
            &TerminalMobiusSource {
                mode: TerminalPresentationMode::Plane3d,
                pose,
            },
            &pose,
            MobiusEnterZoomFloor::ProtocolExact,
        );

        assert_eq!(transition.end_zoom, 0.4);
        transition.begin_enter(
            0,
            &TerminalMobiusSource {
                mode: TerminalPresentationMode::Plane3d,
                pose,
            },
            &pose,
            MobiusEnterZoomFloor::KeyboardTarget,
        );
        assert_eq!(
            transition.end_zoom,
            MobiusTransition::TARGET_ZOOM_MULTIPLIER
        );
    }

    #[test]
    fn exiting_mid_enter_keeps_the_morph_and_zoom_continuous() {
        let pose = TerminalCameraPose::default();
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
        transition.elapsed_secs =
            MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS * 0.3;
        let morph_before = transition.morph_progress();
        let zoom_before = transition.current_zoom();
        assert!((morph_before - 0.3).abs() < 1e-6);

        transition.begin_exit(0, &pose, zoom_before);

        assert!((transition.morph_progress() - morph_before).abs() < 1e-6);
        assert_eq!(transition.current_zoom(), zoom_before);
        transition.elapsed_secs = MobiusTransition::VIEW_RESET_SECS + MobiusTransition::MORPH_SECS;
        assert_eq!(transition.morph_progress(), 0.0);
    }

    #[test]
    fn entering_mid_exit_resumes_from_the_current_state() {
        let pose = TerminalCameraPose::default();
        let mut transition = MobiusTransition::default();
        transition.begin_enter(
            0,
            &TerminalMobiusSource {
                mode: TerminalPresentationMode::Flat2d,
                pose,
            },
            &pose,
            MobiusEnterZoomFloor::KeyboardTarget,
        );
        transition.elapsed_secs = MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS;
        transition.begin_exit(0, &pose, transition.current_zoom());
        transition.elapsed_secs =
            MobiusTransition::VIEW_RESET_SECS + MobiusTransition::MORPH_SECS * 0.5;
        let morph_before = transition.morph_progress();
        assert!((morph_before - 0.5).abs() < 1e-6);

        transition.begin_enter(
            0,
            &TerminalMobiusSource {
                mode: TerminalPresentationMode::Flat2d,
                pose,
            },
            &pose,
            MobiusEnterZoomFloor::KeyboardTarget,
        );

        assert!(matches!(
            transition.direction,
            MobiusTransitionDirection::Entering
        ));
        assert!((transition.morph_progress() - morph_before).abs() < 1e-6);
        assert_eq!(transition.source_mode, TerminalPresentationMode::Flat2d);
        transition.elapsed_secs = MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS;
        assert_eq!(transition.morph_progress(), 1.0);
    }

    #[test]
    fn resumed_transitions_shrink_the_morph_phase_proportionally() {
        let pose = TerminalCameraPose::default();
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
        transition.elapsed_secs =
            MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS * 0.7;
        transition.begin_exit(0, &pose, transition.current_zoom());

        // The remaining 0.7 of morph unwinds in 0.7 * MORPH_SECS with no
        // hold: velocity is continuous, so halfway through the shortened
        // phase the morph has covered half the remaining distance.
        transition.elapsed_secs = MobiusTransition::MORPH_SECS * 0.35;
        assert!((transition.morph_progress() - 0.35).abs() < 1e-6);
        assert!(!transition.finished());
        transition.elapsed_secs = MobiusTransition::MORPH_SECS * 0.7;
        assert_eq!(transition.morph_progress(), 0.0);
        assert!(transition.finished());

        // A resume with no remaining morph distance completes right after the
        // camera hold phase instead of dividing by zero.
        let mut degenerate = MobiusTransition::default();
        degenerate.begin_exit(0, &pose, 1.0);
        degenerate.elapsed_secs = MobiusTransition::VIEW_RESET_SECS + MobiusTransition::MORPH_SECS;
        degenerate.begin_exit(0, &pose, 1.0);
        assert_eq!(degenerate.morph_progress(), 0.0);
        assert!(!degenerate.finished());
        degenerate.elapsed_secs = MobiusTransition::VIEW_RESET_SECS;
        assert!(degenerate.finished());
    }

    #[test]
    fn repeated_identical_handoffs_do_not_restart_the_clock() {
        let pose = TerminalCameraPose::default();
        let source = TerminalMobiusSource {
            mode: TerminalPresentationMode::Plane3d,
            pose,
        };
        let mut transition = MobiusTransition::default();
        transition.begin_enter(0, &source, &pose, MobiusEnterZoomFloor::ProtocolExact);
        transition.elapsed_secs = 0.15;

        // Rapid re-activations toward identical targets (e.g. alternating
        // identically-posed Mobius slots) must not reset the clock, or the
        // transition could be held open forever.
        transition.prepare_source(1, source.mode, &source.pose);
        transition.begin_enter(1, &source, &pose, MobiusEnterZoomFloor::ProtocolExact);

        assert_eq!(transition.elapsed_secs, 0.15);
        assert!(transition.source_is_for(1));
    }

    #[test]
    fn resumed_morph_advances_during_the_camera_phase() {
        let live_pose = TerminalCameraPose::default();
        let other_pose = TerminalCameraPose {
            yaw: 1.0,
            ..TerminalCameraPose::default()
        };
        let source = TerminalMobiusSource {
            mode: TerminalPresentationMode::Plane3d,
            pose: live_pose,
        };
        let mut transition = MobiusTransition::default();
        transition.begin_enter(0, &source, &live_pose, MobiusEnterZoomFloor::ProtocolExact);
        transition.elapsed_secs =
            MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS * 0.4;

        // A hand-off to a differently-posed slot restarts the camera lerp,
        // but the morph keeps advancing immediately instead of freezing for
        // the hold window.
        transition.prepare_source(1, source.mode, &source.pose);
        transition.begin_enter(1, &source, &other_pose, MobiusEnterZoomFloor::ProtocolExact);
        assert!((transition.morph_progress() - 0.4).abs() < 1e-6);
        transition.elapsed_secs = MobiusTransition::ZOOM_OUT_SECS / 2.0;
        assert!(transition.morph_progress() > 0.4);
    }

    #[test]
    fn handoff_resumes_continuously_from_an_exiting_transition() {
        let pose = TerminalCameraPose::default();
        let source = TerminalMobiusSource {
            mode: TerminalPresentationMode::Plane3d,
            pose,
        };
        let mut transition = MobiusTransition::default();
        transition.begin_enter(0, &source, &pose, MobiusEnterZoomFloor::KeyboardTarget);
        transition.elapsed_secs = MobiusTransition::ZOOM_OUT_SECS + MobiusTransition::MORPH_SECS;
        transition.begin_exit(0, &pose, transition.current_zoom());
        transition.elapsed_secs = MobiusTransition::MORPH_SECS * 0.5;
        let morph_before = transition.morph_progress();
        assert!(morph_before > 0.0 && morph_before < 1.0);

        transition.prepare_source(1, source.mode, &source.pose);
        transition.begin_enter(1, &source, &pose, MobiusEnterZoomFloor::ProtocolExact);

        assert!(matches!(
            transition.direction,
            MobiusTransitionDirection::Entering
        ));
        assert!((transition.morph_progress() - morph_before).abs() < 1e-6);
        assert!(transition.source_is_for(1));
    }

    #[test]
    fn exit_interpolates_roll_back_to_the_source() {
        let source_pose = TerminalCameraPose {
            roll: 0.1,
            ..TerminalCameraPose::default()
        };
        let mobius_pose = TerminalCameraPose {
            roll: 0.5,
            ..TerminalCameraPose::default()
        };
        let mut transition = MobiusTransition::default();
        transition.prepare_source(0, TerminalPresentationMode::Plane3d, &source_pose);
        transition.begin_exit(0, &mobius_pose, 1.0);

        assert_eq!(transition.current_roll(), 0.5);
        transition.elapsed_secs = MobiusTransition::VIEW_RESET_SECS;
        assert!((transition.current_roll() - 0.1).abs() < 1e-6);
    }
}

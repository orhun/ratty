//! Mouse input handling and selection state.

use bevy::ecs::message::MessageReader;
use bevy::ecs::system::SystemParam;
use bevy::input::ButtonState;
use bevy::input::mouse::{MouseButton, MouseButtonInput, MouseScrollUnit, MouseWheel};
use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::prelude::*;
use bevy::window::{CursorMoved, PrimaryWindow, Window, WindowFocused};

use crate::camera::{
    MAX_PERSPECTIVE_FOV, MIN_ORTHOGRAPHIC_SCALE, MIN_PERSPECTIVE_FOV, TerminalCameraInteraction,
    TerminalCameraSlots,
};
use crate::config::AppConfig;
use crate::keyboard::enter_mobius_presentation;
use crate::runtime::TerminalRuntime;
use crate::scene::{MobiusTransition, TerminalPresentationMode, TerminalViewport};
use crate::terminal::TerminalSurface;
use crate::vt::{self, MouseProtocolEncoding, MouseProtocolMode, VtTerminal};

/// Distance in pixels the pointer must move with a pending selection to start dragging.
const SELECTION_DRAG_THRESHOLD: f32 = 4.0;

/// Camera rotation applied per pixel of pointer movement.
const ROTATION_SENSITIVITY: f32 = 0.005;

/// Camera zoom applied per pixel of change between two fingers.
const PINCH_ZOOM_SENSITIVITY: f32 = 0.002;

/// Minimum corner-swipe travel as a fraction of the shorter window edge.
const MOBIUS_SWIPE_FRACTION: f32 = 0.18;

/// Active terminal text selection.
#[derive(Resource, Clone, Default)]
pub struct TerminalSelection {
    start: Option<UVec2>,
    end: Option<UVec2>,
    pending_start: Option<UVec2>,
    pending_position: Option<Vec2>,
    dragging: bool,
    cursor_position: Option<Vec2>,
}

#[derive(Default)]
pub(crate) struct ForwardedMouseState {
    left_pressed: bool,
    middle_pressed: bool,
    right_pressed: bool,
    last_cell: Option<UVec2>,
}

#[derive(Default)]
pub(crate) struct LocalScrollState {
    pixel_remainder: f32,
}

/// Camera movement produced by a touch event.
#[derive(Debug, PartialEq)]
enum TouchGesture {
    Rotate(Vec2),
    PanAndZoom { pan: Vec2, zoom: f32 },
    EnterMobius,
}

/// Tracks up to two fingers used for camera gestures.
#[derive(Default)]
pub(crate) struct TouchGestureState {
    primary: Option<(u64, Vec2)>,
    secondary: Option<(u64, Vec2)>,
    pinch_distance: Option<f32>,
    mobius_swipe_start: Option<Vec2>,
}

impl TouchGestureState {
    fn reset(&mut self) {
        self.primary = None;
        self.secondary = None;
        self.pinch_distance = None;
        self.mobius_swipe_start = None;
    }

    /// Updates the active gesture and returns its rotation or pinch movement.
    fn update(
        &mut self,
        id: u64,
        phase: TouchPhase,
        position: Vec2,
        window_size: Vec2,
    ) -> Option<TouchGesture> {
        match phase {
            TouchPhase::Started => {
                if self.primary.is_none() {
                    self.primary = Some((id, position));
                    let corner_size = mobius_swipe_threshold(window_size);
                    if position.x <= corner_size && position.y >= window_size.y - corner_size {
                        self.mobius_swipe_start = Some(position);
                    }
                } else if self.secondary.is_none()
                    && self.primary.is_some_and(|(primary_id, _)| primary_id != id)
                {
                    self.secondary = Some((id, position));
                    self.pinch_distance = self.finger_distance();
                    self.mobius_swipe_start = None;
                }
                None
            }
            TouchPhase::Moved => {
                let previous_primary = self.primary;
                let previous_center = self.finger_center();
                if self.primary.is_some_and(|(primary_id, _)| primary_id == id) {
                    self.primary = Some((id, position));
                } else if self
                    .secondary
                    .is_some_and(|(secondary_id, _)| secondary_id == id)
                {
                    self.secondary = Some((id, position));
                } else {
                    return None;
                }

                if self.secondary.is_none()
                    && let Some(start) = self.mobius_swipe_start
                {
                    let travel = position - start;
                    let threshold = mobius_swipe_threshold(window_size);
                    if travel.x >= threshold && travel.y <= -threshold {
                        self.reset();
                        return Some(TouchGesture::EnterMobius);
                    }

                    // Reserve a valid corner swipe for the mode gesture rather
                    // than rotating the camera underneath the user's finger.
                    if travel.x >= -24.0 && travel.y <= 24.0 {
                        return None;
                    }
                    self.mobius_swipe_start = None;
                }

                if let Some(distance) = self.finger_distance() {
                    let zoom = self.pinch_distance.map(|last| distance - last);
                    self.pinch_distance = Some(distance);
                    zoom.zip(previous_center).and_then(|(zoom, center)| {
                        self.finger_center().map(|next| TouchGesture::PanAndZoom {
                            pan: next - center,
                            zoom,
                        })
                    })
                } else {
                    previous_primary.map(|(_, last)| TouchGesture::Rotate(position - last))
                }
            }
            TouchPhase::Ended | TouchPhase::Canceled => {
                if self
                    .secondary
                    .is_some_and(|(secondary_id, _)| secondary_id == id)
                {
                    self.secondary = None;
                    self.pinch_distance = None;
                } else if self.primary.is_some_and(|(primary_id, _)| primary_id == id) {
                    self.primary = self.secondary.take();
                    self.pinch_distance = None;
                    self.mobius_swipe_start = None;
                }
                None
            }
        }
    }

    fn finger_distance(&self) -> Option<f32> {
        Some(self.primary?.1.distance(self.secondary?.1))
    }

    fn finger_center(&self) -> Option<Vec2> {
        Some((self.primary?.1 + self.secondary?.1) * 0.5)
    }
}

fn mobius_swipe_threshold(window_size: Vec2) -> f32 {
    window_size
        .min_element()
        .mul_add(MOBIUS_SWIPE_FRACTION, 0.0)
        .clamp(72.0, 180.0)
}

/// Normalized selection bounds.
#[derive(Copy, Clone)]
pub struct SelectionBounds {
    /// First selected row.
    pub start_row: u32,
    /// Last selected row.
    pub end_row: u32,
    /// First selected column.
    pub start_col: u32,
    /// Last selected column.
    pub end_col: u32,
}

impl SelectionBounds {
    /// Returns whether a cell is inside the bounds.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        let row = row as u32;
        let col = col as u32;

        if row < self.start_row || row > self.end_row {
            return false;
        }

        if self.start_row == self.end_row {
            return col >= self.start_col && col <= self.end_col;
        }

        if row == self.start_row {
            return col >= self.start_col;
        }

        if row == self.end_row {
            return col <= self.end_col;
        }

        true
    }
}

impl TerminalSelection {
    /// Returns normalized selection bounds.
    pub fn normalized_bounds(&self) -> Option<SelectionBounds> {
        let start = self.start?;
        let end = self.end.unwrap_or(start);
        Some(SelectionBounds {
            start_row: start.y.min(end.y),
            end_row: start.y.max(end.y),
            start_col: start.x.min(end.x),
            end_col: start.x.max(end.x),
        })
    }

    /// Starts a selection at a cell.
    pub fn begin(&mut self, cell: UVec2) -> bool {
        let changed = self.start != Some(cell) || self.end != Some(cell) || !self.dragging;
        self.start = Some(cell);
        self.end = Some(cell);
        self.pending_start = None;
        self.pending_position = None;
        self.dragging = true;
        changed
    }

    /// Arms a selection at a cell without making it visible until the pointer is dragged.
    pub fn begin_pending(&mut self, cell: UVec2, position: Vec2) -> bool {
        let changed = self.start.is_some() || self.end.is_some() || self.dragging;
        self.start = None;
        self.end = None;
        self.pending_start = Some(cell);
        self.pending_position = Some(position);
        self.dragging = false;
        changed
    }

    /// Updates the selection end cell.
    pub fn update(&mut self, cell: UVec2) -> bool {
        if self.dragging && self.end != Some(cell) {
            self.end = Some(cell);
            return true;
        }
        false
    }

    /// Updates the selection from a pointer position.
    pub fn update_from_cursor(&mut self, cell: UVec2, position: Vec2) -> bool {
        if self.dragging {
            return self.update(cell);
        }

        let Some(start) = self.pending_start else {
            return false;
        };
        let Some(origin) = self.pending_position else {
            return false;
        };

        if position.distance(origin) < SELECTION_DRAG_THRESHOLD {
            return false;
        }

        self.start = Some(start);
        self.end = Some(cell);
        self.pending_start = None;
        self.pending_position = None;
        self.dragging = true;
        true
    }

    /// Ends an in-progress selection.
    pub fn end(&mut self) -> bool {
        let changed = self.dragging;
        self.pending_start = None;
        self.pending_position = None;
        self.dragging = false;
        changed
    }

    /// Clears the selection.
    pub fn clear(&mut self) -> bool {
        let changed = self.start.is_some()
            || self.end.is_some()
            || self.pending_start.is_some()
            || self.pending_position.is_some()
            || self.dragging;
        self.start = None;
        self.end = None;
        self.pending_start = None;
        self.pending_position = None;
        self.dragging = false;
        self.cursor_position = None;
        changed
    }

    /// Stores the current pointer position.
    pub fn set_cursor_position(&mut self, position: Vec2) {
        self.cursor_position = Some(position);
    }

    /// Returns the current pointer position.
    pub fn cursor_position(&self) -> Option<Vec2> {
        self.cursor_position
    }

    /// Returns the selected screen text.
    ///
    /// Kept hand-rolled rather than delegating to rio-vt's `selection_to_string`:
    /// ratty's selection is a plain rectangular row/column range driven by the
    /// 3D viewport, not rio-vt's `Selection` (which models linewise and
    /// semantic modes over absolute grid positions).
    pub fn selected_text(&self, term: &VtTerminal) -> Option<String> {
        let bounds = self.normalized_bounds()?;

        let cols = u16::try_from(term.columns()).unwrap_or(u16::MAX);
        let mut out = String::new();

        let start_row = bounds.start_row as u16;
        let end_row = bounds.end_row as u16;
        let start_col = bounds.start_col as u16;
        let end_col = bounds.end_col as u16;

        for row in start_row..=end_row {
            let row_start = if row == start_row { start_col } else { 0 };
            let row_end = if row == end_row {
                end_col.min(cols.saturating_sub(1))
            } else {
                cols.saturating_sub(1)
            };

            if vt::visible_row(term, row).is_some() {
                for col in row_start..=row_end {
                    if usize::from(col) >= term.columns() {
                        break;
                    }
                    let pos = vt::visible_pos(term, row, col);
                    if vt::is_wide_spacer(&term.grid, pos) {
                        continue;
                    }

                    let before = out.len();
                    vt::push_cell_text(&mut out, &term.grid, pos);
                    if out.len() == before {
                        // Blank cell: rio-vt stores it as NUL, not a space.
                        out.push(' ');
                    }
                }
            }

            if row != end_row {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\n');
            }
        }

        Some(out)
    }
}

/// Mouse input system parameters.
#[derive(SystemParam)]
pub struct MouseSystemParams<'w, 's> {
    primary_window: Query<'w, 's, (Entity, &'static Window), With<PrimaryWindow>>,
    runtime: ResMut<'w, TerminalRuntime>,
    terminal: Res<'w, TerminalSurface>,
    viewport: Res<'w, TerminalViewport>,
    camera_slots: ResMut<'w, TerminalCameraSlots>,
    camera_interaction: ResMut<'w, TerminalCameraInteraction>,
    mobius_transition: ResMut<'w, MobiusTransition>,
    selection: ResMut<'w, TerminalSelection>,
    redraw: ResMut<'w, crate::terminal::TerminalRedrawState>,
    app_config: Res<'w, AppConfig>,
}

/// Handles terminal mouse input.
pub(crate) fn handle_mouse_input(
    mut cursor_events: MessageReader<CursorMoved>,
    mut button_events: MessageReader<MouseButtonInput>,
    mut wheel_events: MessageReader<MouseWheel>,
    mut touch_events: MessageReader<TouchInput>,
    mut focus_events: MessageReader<WindowFocused>,
    mut params: MouseSystemParams,
    mut forwarded_mouse: Local<ForwardedMouseState>,
    mut local_scroll: Local<LocalScrollState>,
    mut touch_gesture: Local<TouchGestureState>,
) {
    let MouseSystemParams {
        primary_window,
        runtime,
        terminal,
        viewport,
        camera_slots,
        camera_interaction,
        mobius_transition,
        selection,
        redraw,
        app_config,
    } = &mut params;
    let Ok((primary_window, window)) = primary_window.single() else {
        return;
    };

    let window_size = window.resolution.size().max(Vec2::ONE);
    let mouse_mode = vt::mouse_protocol_mode(&runtime.term);
    let mouse_encoding = vt::mouse_protocol_encoding(&runtime.term);

    // Button releases delivered while the window is unfocused never reach the
    // handlers below, so losing focus mid-drag would otherwise leave held
    // button state re-arming on bare cursor movement after refocus.
    for event in focus_events.read() {
        if event.window == primary_window && !event.focused {
            // The PTY application saw the press and will never see the real
            // release, so synthesize one per held button before dropping the
            // local state.
            if mouse_mode != MouseProtocolMode::None
                && let Some(cell) = forwarded_mouse.last_cell
            {
                for (pressed, code) in [
                    (forwarded_mouse.left_pressed, 0),
                    (forwarded_mouse.middle_pressed, 1),
                    (forwarded_mouse.right_pressed, 2),
                ] {
                    if pressed {
                        runtime.write_input(&encode_mouse_event(cell, code, true, mouse_encoding));
                    }
                }
            }
            release_pointer_drags(camera_interaction, selection, &mut forwarded_mouse);
            touch_gesture.reset();
        }
    }
    let mode = camera_slots.active().mode;
    let mobius_animating = mode == TerminalPresentationMode::Mobius3d && mobius_transition.active;
    let forward_mouse =
        mode == TerminalPresentationMode::Flat2d && mouse_mode != MouseProtocolMode::None;

    for event in cursor_events.read() {
        if event.window != primary_window {
            continue;
        }

        selection.set_cursor_position(event.position);
        if mobius_animating {
            continue;
        }

        if mode.is_3d() {
            if camera_interaction.rotating {
                if let Some(last) = camera_interaction.last_rotate_cursor {
                    let delta = event.position - last;
                    let pose = &mut camera_slots.active_mut().pose;
                    pose.yaw += delta.x * ROTATION_SENSITIVITY;
                    pose.pitch -= delta.y * ROTATION_SENSITIVITY;
                }
                camera_interaction.last_rotate_cursor = Some(event.position);
            } else if camera_interaction.panning {
                if let Some(last) = camera_interaction.last_pan_cursor {
                    let delta = event.position - last;
                    apply_pan(&mut camera_slots.active_mut().pose, mode, delta);
                }
                camera_interaction.last_pan_cursor = Some(event.position);
            }
        } else if forward_mouse {
            if let Some(cell) = position_to_cell(event.position, window_size, viewport, terminal)
                && forwarded_mouse.last_cell != Some(cell)
                && match mouse_mode {
                    MouseProtocolMode::ButtonMotion => {
                        forwarded_mouse.left_pressed
                            || forwarded_mouse.middle_pressed
                            || forwarded_mouse.right_pressed
                    }
                    MouseProtocolMode::AnyMotion => true,
                    _ => false,
                }
            {
                let button_code = if forwarded_mouse.left_pressed {
                    32
                } else if forwarded_mouse.middle_pressed {
                    33
                } else if forwarded_mouse.right_pressed {
                    34
                } else {
                    35
                };
                runtime.write_input(&encode_mouse_event(
                    cell,
                    button_code,
                    false,
                    mouse_encoding,
                ));
                forwarded_mouse.last_cell = Some(cell);
            }
        } else if (selection.dragging || selection.pending_start.is_some())
            && let Some(cell) = position_to_cell(event.position, window_size, viewport, terminal)
            && selection.update_from_cursor(cell, event.position)
        {
            redraw.request();
        }
    }

    for event in touch_events.read() {
        if event.window != primary_window {
            continue;
        }

        match touch_gesture.update(event.id, event.phase, event.position, window_size) {
            Some(TouchGesture::Rotate(delta)) if mode.is_3d() && !mobius_animating => {
                let pose = &mut camera_slots.active_mut().pose;
                pose.yaw += delta.x * ROTATION_SENSITIVITY;
                pose.pitch -= delta.y * ROTATION_SENSITIVITY;
            }
            Some(TouchGesture::PanAndZoom { pan, zoom }) if mode.is_3d() && !mobius_animating => {
                let pose = &mut camera_slots.active_mut().pose;
                apply_pan(pose, mode, pan);
                apply_wheel_zoom(pose, mode, mobius_animating, zoom * PINCH_ZOOM_SENSITIVITY);
            }
            Some(TouchGesture::EnterMobius) => {
                enter_mobius_presentation(camera_slots, camera_interaction, mobius_transition);
                selection.clear();
            }
            None => {}
            _ => {}
        }
    }

    for event in button_events.read() {
        if event.window != primary_window {
            continue;
        }

        if mobius_animating {
            continue;
        }

        match (event.button, event.state) {
            (MouseButton::Left, ButtonState::Pressed) => {
                if forward_mouse {
                    forwarded_mouse.left_pressed = true;
                    if let Some(cell) = window
                        .cursor_position()
                        .or(selection.cursor_position())
                        .and_then(|position| {
                            position_to_cell(position, window_size, viewport, terminal)
                        })
                    {
                        runtime.write_input(&encode_mouse_event(cell, 0, false, mouse_encoding));
                        forwarded_mouse.last_cell = Some(cell);
                    }
                } else if mode.is_3d() {
                    camera_interaction.rotating = true;
                    camera_interaction.last_rotate_cursor = selection.cursor_position();
                } else if let Some(pos) = selection.cursor_position()
                    && let Some(cell) = position_to_cell(pos, window_size, viewport, terminal)
                    && selection.begin_pending(cell, pos)
                {
                    redraw.request();
                }
            }
            (MouseButton::Left, ButtonState::Released) => {
                if forward_mouse {
                    forwarded_mouse.left_pressed = false;
                    if let Some(cell) = window
                        .cursor_position()
                        .or(selection.cursor_position())
                        .and_then(|position| {
                            position_to_cell(position, window_size, viewport, terminal)
                        })
                    {
                        runtime.write_input(&encode_mouse_event(cell, 0, true, mouse_encoding));
                        forwarded_mouse.last_cell = Some(cell);
                    }
                } else if mode.is_3d() {
                    camera_interaction.rotating = false;
                    camera_interaction.last_rotate_cursor = selection.cursor_position();
                } else {
                    let _ = selection.end();
                }
            }
            (MouseButton::Middle, ButtonState::Pressed) if forward_mouse => {
                forwarded_mouse.middle_pressed = true;
                if let Some(cell) = window
                    .cursor_position()
                    .or(selection.cursor_position())
                    .and_then(|position| {
                        position_to_cell(position, window_size, viewport, terminal)
                    })
                {
                    runtime.write_input(&encode_mouse_event(cell, 1, false, mouse_encoding));
                    forwarded_mouse.last_cell = Some(cell);
                }
            }
            (MouseButton::Middle, ButtonState::Released) if forward_mouse => {
                forwarded_mouse.middle_pressed = false;
                if let Some(cell) = window
                    .cursor_position()
                    .or(selection.cursor_position())
                    .and_then(|position| {
                        position_to_cell(position, window_size, viewport, terminal)
                    })
                {
                    runtime.write_input(&encode_mouse_event(cell, 1, true, mouse_encoding));
                    forwarded_mouse.last_cell = Some(cell);
                }
            }
            (MouseButton::Right, ButtonState::Pressed) if forward_mouse => {
                forwarded_mouse.right_pressed = true;
                if let Some(cell) = window
                    .cursor_position()
                    .or(selection.cursor_position())
                    .and_then(|position| {
                        position_to_cell(position, window_size, viewport, terminal)
                    })
                {
                    runtime.write_input(&encode_mouse_event(cell, 2, false, mouse_encoding));
                    forwarded_mouse.last_cell = Some(cell);
                }
            }
            (MouseButton::Right, ButtonState::Released) if forward_mouse => {
                forwarded_mouse.right_pressed = false;
                if let Some(cell) = window
                    .cursor_position()
                    .or(selection.cursor_position())
                    .and_then(|position| {
                        position_to_cell(position, window_size, viewport, terminal)
                    })
                {
                    runtime.write_input(&encode_mouse_event(cell, 2, true, mouse_encoding));
                    forwarded_mouse.last_cell = Some(cell);
                }
            }
            (MouseButton::Right, ButtonState::Pressed) if mode.is_3d() => {
                camera_interaction.panning = true;
                camera_interaction.last_pan_cursor = selection.cursor_position();
            }
            (MouseButton::Right, ButtonState::Released) if mode.is_3d() => {
                camera_interaction.panning = false;
                camera_interaction.last_pan_cursor = selection.cursor_position();
            }
            _ => {}
        }
    }

    for event in wheel_events.read() {
        let delta = match event.unit {
            MouseScrollUnit::Line => event.y * 0.1,
            MouseScrollUnit::Pixel => event.y * 0.001,
        };

        if forward_mouse && delta != 0.0 {
            if let Some(cell) = window
                .cursor_position()
                .or(selection.cursor_position())
                .and_then(|position| position_to_cell(position, window_size, viewport, terminal))
            {
                runtime.write_input(&encode_mouse_event(
                    cell,
                    if delta > 0.0 { 64 } else { 65 },
                    false,
                    mouse_encoding,
                ));
            }
        } else if mode == TerminalPresentationMode::Flat2d && !vt::alternate_screen(&runtime.term) {
            let amount = match event.unit {
                MouseScrollUnit::Line => {
                    app_config.terminal.mouse_scroll_lines as isize
                        * (if delta < 0.0 { -1 } else { 1 })
                }
                MouseScrollUnit::Pixel => {
                    let char_height = terminal.char_dimensions().y;
                    local_scroll.pixel_remainder += event.y / char_height;
                    let amount = local_scroll.pixel_remainder.trunc() as isize;
                    local_scroll.pixel_remainder -= amount as f32;
                    amount
                }
            };

            if amount != 0 {
                let current = vt::scrollback(&runtime.term) as isize;
                let next = (current + amount).max(0) as usize;
                vt::set_scrollback(&mut runtime.term, next);
                selection.clear();
                redraw.request();
            }
        } else if mode.is_3d() && delta != 0.0 {
            apply_wheel_zoom(
                &mut camera_slots.active_mut().pose,
                mode,
                mobius_animating,
                delta,
            );
        }
    }
}

fn encode_mouse_event(
    cell: UVec2,
    button_code: u16,
    release: bool,
    encoding: MouseProtocolEncoding,
) -> Vec<u8> {
    let col = cell.x + 1;
    let row = cell.y + 1;
    match encoding {
        MouseProtocolEncoding::Sgr => {
            let final_byte = if release { 'm' } else { 'M' };
            format!("\x1b[<{button_code};{col};{row}{final_byte}").into_bytes()
        }
        MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => {
            let code = if release { 3 } else { button_code }.saturating_add(32);
            let x = (col + 32).min(u8::MAX as u32) as u8;
            let y = (row + 32).min(u8::MAX as u32) as u8;
            vec![0x1b, b'[', b'M', code as u8, x, y]
        }
    }
}

pub(crate) fn encode_mouse_wheel(
    cell: UVec2,
    up: bool,
    encoding: MouseProtocolEncoding,
) -> Vec<u8> {
    encode_mouse_event(cell, if up { 64 } else { 65 }, false, encoding)
}

fn position_to_cell(
    position: Vec2,
    window_size: Vec2,
    viewport: &TerminalViewport,
    terminal: &TerminalSurface,
) -> Option<UVec2> {
    if viewport.size.x <= 0.0 || viewport.size.y <= 0.0 {
        return None;
    }

    let cols = terminal.cols.max(1) as f32;
    let rows = terminal.rows.max(1) as f32;
    let cell_width = viewport.size.x / cols;
    let cell_height = viewport.size.y / rows;
    if cell_width <= 0.0 || cell_height <= 0.0 {
        return None;
    }

    let margin = (window_size - viewport.size).max(Vec2::ZERO) * 0.5;
    let local_position = position - margin;
    if local_position.x < 0.0
        || local_position.y < 0.0
        || local_position.x >= viewport.size.x
        || local_position.y >= viewport.size.y
    {
        return None;
    }

    let x = local_position.x.clamp(0.0, viewport.size.x - 1.0);
    let y = local_position.y.clamp(0.0, viewport.size.y - 1.0);
    let col = (x / cell_width).floor() as u32;
    let row = (y / cell_height).floor() as u32;

    Some(UVec2::new(
        col.min(terminal.cols.saturating_sub(1) as u32),
        row.min(terminal.rows.saturating_sub(1) as u32),
    ))
}

/// Drops every drag-like state that waits on a button release.
///
/// Releases delivered while the window is unfocused go to the newly focused
/// window instead, so held-button state must be cleared on focus loss: camera
/// drags, an in-progress or pending text selection drag (the completed
/// selection itself is kept), and forwarded mouse-protocol button state.
/// This only clears local state; the caller synthesizes the release events
/// the PTY application is still owed before invoking it. `last_cell` is also
/// cleared so the first motion after refocus is never deduplicated away.
fn release_pointer_drags(
    camera_interaction: &mut TerminalCameraInteraction,
    selection: &mut TerminalSelection,
    forwarded_mouse: &mut ForwardedMouseState,
) {
    camera_interaction.reset();
    selection.end();
    forwarded_mouse.left_pressed = false;
    forwarded_mouse.middle_pressed = false;
    forwarded_mouse.right_pressed = false;
    forwarded_mouse.last_cell = None;
}

/// Applies camera translation using the same movement as a right-button drag.
fn apply_pan(
    pose: &mut crate::camera::TerminalCameraPose,
    mode: TerminalPresentationMode,
    delta: Vec2,
) {
    let movement_scale = if mode == TerminalPresentationMode::Perspective3d {
        pose.perspective_fov
    } else {
        pose.orthographic_scale
    };
    pose.translation.x -= delta.x * movement_scale;
    pose.translation.y += delta.y * movement_scale;
}

/// Largest orthographic scale reachable by wheel zoom alone.
const MAX_WHEEL_ORTHOGRAPHIC_SCALE: f32 = 4.0;

/// Applies one wheel zoom step to an orthographic scale.
///
/// The protocol accepts any scale of at least [`MIN_ORTHOGRAPHIC_SCALE`], so a
/// protocol-set scale above the interactive limit must not be yanked down to
/// it by the first wheel tick; the wheel can only zoom back toward the range.
fn wheel_zoomed_orthographic_scale(current: f32, delta: f32) -> f32 {
    let max_scale = current.max(MAX_WHEEL_ORTHOGRAPHIC_SCALE);
    (current - delta).clamp(MIN_ORTHOGRAPHIC_SCALE, max_scale)
}

/// Routes one wheel step to the projection value the mode displays.
///
/// Mobius shares the orthographic branch with Plane3d. Input is dropped while
/// a Mobius transition animates: the transition owns the strip zoom until it
/// finishes.
fn apply_wheel_zoom(
    pose: &mut crate::camera::TerminalCameraPose,
    mode: TerminalPresentationMode,
    mobius_animating: bool,
    delta: f32,
) {
    if !mode.is_3d() || mobius_animating || delta == 0.0 {
        return;
    }
    if mode == TerminalPresentationMode::Perspective3d {
        pose.perspective_fov =
            (pose.perspective_fov - delta).clamp(MIN_PERSPECTIVE_FOV, MAX_PERSPECTIVE_FOV);
    } else {
        pose.orthographic_scale = wheel_zoomed_orthographic_scale(pose.orthographic_scale, delta);
    }
}

#[cfg(test)]
mod wheel_zoom_tests {
    use super::*;
    use crate::camera::TerminalCameraPose;

    #[test]
    fn one_finger_rotates_and_two_fingers_pan_and_pinch() {
        let mut state = TouchGestureState::default();
        let window_size = Vec2::new(1000.0, 600.0);

        assert_eq!(
            state.update(7, TouchPhase::Started, Vec2::new(10.0, 20.0), window_size),
            None
        );
        assert_eq!(
            state.update(7, TouchPhase::Moved, Vec2::new(14.0, 17.0), window_size),
            Some(TouchGesture::Rotate(Vec2::new(4.0, -3.0)))
        );
        assert_eq!(
            state.update(8, TouchPhase::Started, Vec2::new(4.0, 17.0), window_size),
            None
        );
        assert_eq!(
            state.update(8, TouchPhase::Moved, Vec2::new(0.0, 17.0), window_size),
            Some(TouchGesture::PanAndZoom {
                pan: Vec2::new(-2.0, 0.0),
                zoom: 4.0,
            })
        );

        state.update(7, TouchPhase::Ended, Vec2::new(14.0, 17.0), window_size);
        assert_eq!(state.primary, Some((8, Vec2::new(0.0, 17.0))));
        assert_eq!(
            state.update(8, TouchPhase::Moved, Vec2::new(3.0, 19.0), window_size),
            Some(TouchGesture::Rotate(Vec2::new(3.0, 2.0)))
        );
    }

    #[test]
    fn bottom_left_swipe_enters_mobius() {
        let mut state = TouchGestureState::default();
        let window_size = Vec2::new(1000.0, 600.0);

        state.update(1, TouchPhase::Started, Vec2::new(40.0, 560.0), window_size);
        assert_eq!(
            state.update(1, TouchPhase::Moved, Vec2::new(160.0, 440.0), window_size),
            Some(TouchGesture::EnterMobius)
        );
        assert_eq!(state.primary, None);
    }

    #[test]
    fn canceled_touch_rotation_can_restart() {
        let mut state = TouchGestureState::default();
        let window_size = Vec2::splat(500.0);
        state.update(1, TouchPhase::Started, Vec2::ONE, window_size);
        state.update(1, TouchPhase::Canceled, Vec2::ONE, window_size);
        state.update(2, TouchPhase::Started, Vec2::new(3.0, 4.0), window_size);

        assert_eq!(state.primary, Some((2, Vec2::new(3.0, 4.0))));
        assert_eq!(state.secondary, None);
    }

    #[test]
    fn focus_loss_releases_every_pointer_drag() {
        let mut interaction = TerminalCameraInteraction {
            rotating: true,
            panning: true,
            last_rotate_cursor: Some(Vec2::ONE),
            last_pan_cursor: Some(Vec2::ONE),
        };
        let mut selection = TerminalSelection::default();
        selection.begin(UVec2::new(2, 3));
        let mut forwarded = ForwardedMouseState {
            left_pressed: true,
            middle_pressed: true,
            right_pressed: true,
            last_cell: Some(UVec2::ZERO),
        };

        release_pointer_drags(&mut interaction, &mut selection, &mut forwarded);

        assert!(!interaction.rotating);
        assert!(!interaction.panning);
        assert_eq!(interaction.last_rotate_cursor, None);
        assert!(!selection.dragging);
        // The completed selection itself survives; only the drag is released.
        assert!(selection.normalized_bounds().is_some());
        assert!(!forwarded.left_pressed);
        assert!(!forwarded.middle_pressed);
        assert!(!forwarded.right_pressed);
        assert_eq!(forwarded.last_cell, None);

        let mut pending = TerminalSelection::default();
        pending.begin_pending(UVec2::ZERO, Vec2::ZERO);
        release_pointer_drags(&mut interaction, &mut pending, &mut forwarded);
        assert_eq!(pending.pending_start, None);
        assert!(!pending.update_from_cursor(UVec2::new(5, 5), Vec2::new(100.0, 100.0)));
    }

    #[test]
    fn mobius_wheel_zoom_shares_the_orthographic_branch() {
        let mut pose = TerminalCameraPose::default();
        apply_wheel_zoom(&mut pose, TerminalPresentationMode::Mobius3d, false, 0.1);
        assert_eq!(pose.orthographic_scale, 0.9);
        assert_eq!(
            pose.perspective_fov,
            TerminalCameraPose::default().perspective_fov
        );

        let mut plane_pose = TerminalCameraPose::default();
        apply_wheel_zoom(
            &mut plane_pose,
            TerminalPresentationMode::Plane3d,
            false,
            0.1,
        );
        assert_eq!(plane_pose.orthographic_scale, pose.orthographic_scale);

        // While the Mobius transition animates, the wheel is ignored.
        let before = pose;
        apply_wheel_zoom(&mut pose, TerminalPresentationMode::Mobius3d, true, 0.1);
        assert_eq!(pose, before);
    }

    #[test]
    fn wheel_zoom_respects_the_protocol_scale_range() {
        // A protocol-set scale above the interactive cap is not snapped down.
        assert_eq!(wheel_zoomed_orthographic_scale(20.0, -0.1), 20.0);
        // Zooming in from a large scale moves toward the view, not to 4.0.
        assert_eq!(wheel_zoomed_orthographic_scale(20.0, 0.1), 19.9);
        // Zooming in near the protocol minimum never zooms out instead.
        let zoomed = wheel_zoomed_orthographic_scale(0.05, 0.1);
        assert!(zoomed <= 0.05);
        assert!(zoomed >= MIN_ORTHOGRAPHIC_SCALE);
        // The ordinary interactive range still behaves as before.
        assert_eq!(wheel_zoomed_orthographic_scale(1.0, 0.1), 0.9);
        assert_eq!(wheel_zoomed_orthographic_scale(3.95, -0.1), 4.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rio_vt::ansi::CursorShape;
    use rio_vt::crosswords::{Crosswords, CrosswordsSize};
    use rio_vt::event::WindowId;
    use rio_vt::performer::handler::Processor;

    use crate::vt::TerminalEventSink;

    fn terminal(rows: u16, cols: u16, input: &str) -> VtTerminal {
        let mut term = Crosswords::new(
            CrosswordsSize::new(usize::from(cols), usize::from(rows)),
            CursorShape::Block,
            TerminalEventSink::default(),
            WindowId::from(0),
            0,
            1000,
        );
        Processor::default().advance(&mut term, input.as_bytes());
        term
    }

    fn select(start: (u32, u32), end: (u32, u32)) -> TerminalSelection {
        let mut selection = TerminalSelection::default();
        selection.begin(UVec2::new(start.0, start.1));
        selection.update(UVec2::new(end.0, end.1));
        selection
    }

    #[test]
    fn selection_returns_the_drawn_text() {
        let term = terminal(3, 20, "hello world");
        let selection = select((0, 0), (10, 0));
        assert_eq!(
            selection.selected_text(&term).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn selection_spans_multiple_rows() {
        let term = terminal(3, 20, "first\r\nsecond");
        let selection = select((0, 0), (5, 1));
        assert_eq!(
            selection.selected_text(&term).as_deref(),
            Some("first\nsecond")
        );
    }

    #[test]
    fn selection_preserves_wide_characters_and_combining_marks() {
        let term = terminal(3, 20, "你好e\u{0301}z");
        let selection = select((0, 0), (5, 0));
        assert_eq!(
            selection.selected_text(&term).as_deref(),
            Some("你好e\u{0301}z")
        );
    }

    #[test]
    fn selection_without_a_drag_is_empty() {
        let term = terminal(3, 20, "hello");
        assert_eq!(TerminalSelection::default().selected_text(&term), None);
    }
}

use bevy::ecs::message::MessageReader;
use bevy::input::ButtonState;
use bevy::input::mouse::{MouseButton, MouseButtonInput, MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::window::{CursorMoved, PrimaryWindow, Window};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

use crate::runtime::TerminalRuntime;
use crate::scene::{
    TerminalPlaneView, TerminalPresentation, TerminalPresentationMode, TerminalViewport,
};
use crate::terminal::TerminalSurface;

#[derive(Resource, Clone, Default)]
pub struct TerminalSelection {
    start: Option<UVec2>,
    end: Option<UVec2>,
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

#[derive(Copy, Clone)]
pub struct SelectionBounds {
    pub start_row: u32,
    pub end_row: u32,
    pub start_col: u32,
    pub end_col: u32,
}

impl SelectionBounds {
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

    pub fn begin(&mut self, cell: UVec2) -> bool {
        let changed = self.start != Some(cell) || self.end != Some(cell) || !self.dragging;
        self.start = Some(cell);
        self.end = Some(cell);
        self.dragging = true;
        changed
    }

    pub fn update(&mut self, cell: UVec2) -> bool {
        if self.dragging && self.end != Some(cell) {
            self.end = Some(cell);
            return true;
        }
        false
    }

    pub fn end(&mut self) -> bool {
        let changed = self.dragging;
        self.dragging = false;
        changed
    }

    pub fn clear(&mut self) -> bool {
        let changed = self.start.is_some() || self.end.is_some() || self.dragging;
        self.start = None;
        self.end = None;
        self.dragging = false;
        self.cursor_position = None;
        changed
    }

    pub fn set_cursor_position(&mut self, position: Vec2) {
        self.cursor_position = Some(position);
    }

    pub fn cursor_position(&self) -> Option<Vec2> {
        self.cursor_position
    }

    pub fn selected_text(&self, screen: &vt100::Screen) -> Option<String> {
        let bounds = self.normalized_bounds()?;

        let (_, cols) = screen.size();
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

            for col in row_start..=row_end {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }

                let symbol = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                out.push_str(symbol);
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

pub fn handle_mouse_input(
    mut cursor_events: MessageReader<CursorMoved>,
    mut button_events: MessageReader<MouseButtonInput>,
    mut wheel_events: MessageReader<MouseWheel>,
    primary_window: Query<(Entity, &Window), With<PrimaryWindow>>,
    runtime: NonSend<TerminalRuntime>,
    terminal: NonSend<TerminalSurface>,
    viewport: Res<TerminalViewport>,
    presentation: Res<TerminalPresentation>,
    mut plane_view: ResMut<TerminalPlaneView>,
    mut selection: ResMut<TerminalSelection>,
    mut forwarded_mouse: Local<ForwardedMouseState>,
    mut redraw: ResMut<crate::terminal::TerminalRedrawState>,
) {
    let Ok((primary_window, window)) = primary_window.single() else {
        return;
    };
    let mouse_mode = runtime.parser.screen().mouse_protocol_mode();
    let mouse_encoding = runtime.parser.screen().mouse_protocol_encoding();
    let forward_mouse = presentation.mode == TerminalPresentationMode::Flat2d
        && mouse_mode != MouseProtocolMode::None;

    for event in cursor_events.read() {
        if event.window != primary_window {
            continue;
        }

        selection.set_cursor_position(event.position);
        if presentation.mode == TerminalPresentationMode::Plane3d {
            if plane_view.rotating {
                if let Some(last) = plane_view.last_rotate_cursor {
                    let delta = event.position - last;
                    plane_view.yaw += delta.x * 0.005;
                    plane_view.pitch -= delta.y * 0.005;
                    redraw.request();
                }
                plane_view.last_rotate_cursor = Some(event.position);
            } else if plane_view.panning {
                if let Some(last) = plane_view.last_pan_cursor {
                    let delta = event.position - last;
                    plane_view.camera_offset.x -= delta.x * plane_view.zoom;
                    plane_view.camera_offset.y += delta.y * plane_view.zoom;
                    redraw.request();
                }
                plane_view.last_pan_cursor = Some(event.position);
            }
        } else if forward_mouse {
            if let Some(cell) = position_to_cell(event.position, &viewport, &terminal) {
                if forwarded_mouse.last_cell != Some(cell)
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
            }
        } else if selection.dragging
            && let Some(cell) = position_to_cell(event.position, &viewport, &terminal)
            && selection.update(cell)
        {
            redraw.request();
        }
    }

    for event in button_events.read() {
        if event.window != primary_window {
            continue;
        }

        match (event.button, event.state) {
            (MouseButton::Left, ButtonState::Pressed) => {
                if forward_mouse {
                    forwarded_mouse.left_pressed = true;
                    if let Some(cell) = window
                        .cursor_position()
                        .or(selection.cursor_position())
                        .and_then(|position| position_to_cell(position, &viewport, &terminal))
                    {
                        runtime.write_input(&encode_mouse_event(cell, 0, false, mouse_encoding));
                        forwarded_mouse.last_cell = Some(cell);
                    }
                } else if presentation.mode == TerminalPresentationMode::Plane3d {
                    plane_view.rotating = true;
                    plane_view.last_rotate_cursor = selection.cursor_position();
                } else if let Some(pos) = selection.cursor_position()
                    && let Some(cell) = position_to_cell(pos, &viewport, &terminal)
                    && selection.begin(cell)
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
                        .and_then(|position| position_to_cell(position, &viewport, &terminal))
                    {
                        runtime.write_input(&encode_mouse_event(cell, 0, true, mouse_encoding));
                        forwarded_mouse.last_cell = Some(cell);
                    }
                } else if presentation.mode == TerminalPresentationMode::Plane3d {
                    plane_view.rotating = false;
                    plane_view.last_rotate_cursor = selection.cursor_position();
                } else {
                    let _ = selection.end();
                }
            }
            (MouseButton::Middle, ButtonState::Pressed) if forward_mouse => {
                forwarded_mouse.middle_pressed = true;
                if let Some(cell) = window
                    .cursor_position()
                    .or(selection.cursor_position())
                    .and_then(|position| position_to_cell(position, &viewport, &terminal))
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
                    .and_then(|position| position_to_cell(position, &viewport, &terminal))
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
                    .and_then(|position| position_to_cell(position, &viewport, &terminal))
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
                    .and_then(|position| position_to_cell(position, &viewport, &terminal))
                {
                    runtime.write_input(&encode_mouse_event(cell, 2, true, mouse_encoding));
                    forwarded_mouse.last_cell = Some(cell);
                }
            }
            (MouseButton::Right, ButtonState::Pressed)
                if presentation.mode == TerminalPresentationMode::Plane3d =>
            {
                plane_view.panning = true;
                plane_view.last_pan_cursor = selection.cursor_position();
            }
            (MouseButton::Right, ButtonState::Released)
                if presentation.mode == TerminalPresentationMode::Plane3d =>
            {
                plane_view.panning = false;
                plane_view.last_pan_cursor = selection.cursor_position();
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
                .and_then(|position| position_to_cell(position, &viewport, &terminal))
            {
                runtime.write_input(&encode_mouse_event(
                    cell,
                    if delta > 0.0 { 64 } else { 65 },
                    false,
                    mouse_encoding,
                ));
            }
        } else if presentation.mode == TerminalPresentationMode::Plane3d && delta != 0.0 {
            plane_view.zoom = (plane_view.zoom - delta).clamp(0.1, 4.0);
            redraw.request();
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

fn position_to_cell(
    position: Vec2,
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

    let x = position.x.clamp(0.0, viewport.size.x - 1.0);
    let y = position.y.clamp(0.0, viewport.size.y - 1.0);
    let col = (x / cell_width).floor() as u32;
    let row = (y / cell_height).floor() as u32;

    Some(UVec2::new(
        col.min(terminal.cols.saturating_sub(1) as u32),
        row.min(terminal.rows.saturating_sub(1) as u32),
    ))
}

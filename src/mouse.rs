use bevy::ecs::message::MessageReader;
use bevy::input::ButtonState;
use bevy::input::mouse::{MouseButton, MouseButtonInput, MouseWheel, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::{CursorMoved, PrimaryWindow};

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
    primary_window: Query<Entity, With<PrimaryWindow>>,
    terminal: NonSend<TerminalSurface>,
    viewport: Res<TerminalViewport>,
    presentation: Res<TerminalPresentation>,
    mut plane_view: ResMut<TerminalPlaneView>,
    mut selection: ResMut<TerminalSelection>,
    mut redraw: ResMut<crate::terminal::TerminalRedrawState>,
) {
    let Ok(primary_window) = primary_window.single() else {
        return;
    };

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
                if presentation.mode == TerminalPresentationMode::Plane3d {
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
                if presentation.mode == TerminalPresentationMode::Plane3d {
                    plane_view.rotating = false;
                    plane_view.last_rotate_cursor = selection.cursor_position();
                } else {
                    let _ = selection.end();
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

        if presentation.mode == TerminalPresentationMode::Plane3d && delta != 0.0 {
            plane_view.zoom = (plane_view.zoom - delta).clamp(0.1, 4.0);
            redraw.request();
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

use bevy::prelude::*;

use crate::keyboard::{TerminalClipboard, handle_keyboard_input};
use crate::mouse::{TerminalSelection, handle_mouse_input};
use crate::scene::{apply_terminal_presentation, setup_scene};
use crate::systems::{
    handle_window_resize, pump_pty_output, redraw_soft_terminal, sync_asset_to_terminal_cursor,
};
use crate::terminal::TerminalRedrawState;

pub struct TerminalPlugin;

impl Plugin for TerminalPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerminalSelection>()
            .init_resource::<TerminalRedrawState>()
            .init_non_send_resource::<TerminalClipboard>()
            .add_systems(Startup, setup_scene)
            .add_systems(Update, pump_pty_output)
            .add_systems(Update, handle_keyboard_input)
            .add_systems(Update, handle_mouse_input)
            .add_systems(Update, handle_window_resize)
            .add_systems(
                Update,
                apply_terminal_presentation
                    .after(handle_keyboard_input)
                    .after(handle_mouse_input),
            )
            .add_systems(
                Update,
                redraw_soft_terminal
                    .after(handle_mouse_input)
                    .after(pump_pty_output),
            )
            .add_systems(
                Update,
                sync_asset_to_terminal_cursor.after(redraw_soft_terminal),
            );
    }
}

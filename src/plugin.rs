use bevy::prelude::*;

use crate::scene::setup_scene;
use crate::systems::{
    handle_keyboard_input, handle_window_resize, pump_pty_output, redraw_soft_terminal,
    sync_asset_to_terminal_cursor,
};

pub struct TerminalPlugin;

impl Plugin for TerminalPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_scene)
            .add_systems(Update, pump_pty_output)
            .add_systems(Update, handle_keyboard_input)
            .add_systems(Update, handle_window_resize)
            .add_systems(Update, redraw_soft_terminal.after(pump_pty_output))
            .add_systems(
                Update,
                sync_asset_to_terminal_cursor.after(redraw_soft_terminal),
            );
    }
}

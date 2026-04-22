mod config;
mod model;
mod plugin;
mod runtime;
mod scene;
mod soft_terminal;
mod systems;

use bevy::prelude::*;

use crate::config::{DEFAULT_COLS, DEFAULT_ROWS, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::plugin::TerminalPlugin;
use crate::runtime::TerminalRuntime;
use crate::soft_terminal::SoftTerminal;

fn main() -> anyhow::Result<()> {
    let runtime = TerminalRuntime::spawn(DEFAULT_COLS, DEFAULT_ROWS)?;
    let soft_terminal = SoftTerminal::new(DEFAULT_COLS, DEFAULT_ROWS);

    App::new()
        .insert_resource(ClearColor(Color::srgb(0.94, 0.92, 0.88)))
        .insert_non_send_resource(runtime)
        .insert_non_send_resource(soft_terminal)
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: env!("CARGO_PKG_NAME").into(),
                resolution: (WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(TerminalPlugin)
        .run();

    Ok(())
}

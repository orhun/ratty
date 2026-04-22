mod config;
mod keyboard;
mod model;
mod mouse;
mod plugin;
mod runtime;
mod scene;
mod systems;
mod terminal;

use bevy::prelude::*;
use bevy::window::WindowResolution;

use crate::config::{DEFAULT_COLS, DEFAULT_ROWS, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::plugin::TerminalPlugin;
use crate::runtime::TerminalRuntime;
use crate::terminal::TerminalSurface;

fn main() -> anyhow::Result<()> {
    let runtime = TerminalRuntime::spawn(DEFAULT_COLS, DEFAULT_ROWS)?;
    let terminal = TerminalSurface::new(DEFAULT_COLS, DEFAULT_ROWS);

    App::new()
        .insert_resource(ClearColor(Color::srgb_u8(31, 31, 40)))
        .insert_non_send_resource(runtime)
        .insert_non_send_resource(terminal)
        .add_plugins(
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: env!("CARGO_PKG_NAME").into(),
                    resolution: WindowResolution::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32)
                        .with_scale_factor_override(1.0),
                    ..default()
                }),
                ..default()
            }),
        )
        .add_plugins(TerminalPlugin)
        .run();

    Ok(())
}

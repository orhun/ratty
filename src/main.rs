mod config;
mod inline;
mod keyboard;
mod kitty;
mod model;
mod mouse;
mod plugin;
mod rendering;
mod rgp;
mod runtime;
mod scene;
mod systems;
mod terminal;

use bevy::prelude::*;
use bevy::window::WindowResolution;

use crate::config::AppConfig;
use crate::plugin::TerminalPlugin;
use crate::runtime::TerminalRuntime;
use crate::terminal::TerminalSurface;

fn main() -> anyhow::Result<()> {
    let app_config = AppConfig::load()?;
    let runtime = TerminalRuntime::spawn(&app_config)?;
    let terminal = TerminalSurface::new(&app_config)?;

    App::new()
        .insert_resource(ClearColor(Color::srgb_u8(
            app_config.theme.background[0],
            app_config.theme.background[1],
            app_config.theme.background[2],
        )))
        .insert_resource(app_config.clone())
        .insert_non_send_resource(runtime)
        .insert_non_send_resource(terminal)
        .add_plugins(
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: env!("CARGO_PKG_NAME").into(),
                    resolution: WindowResolution::new(
                        app_config.window.width,
                        app_config.window.height,
                    )
                    .with_scale_factor_override(app_config.window.scale_factor),
                    ..default()
                }),
                ..default()
            }),
        )
        .add_plugins(TerminalPlugin)
        .run();

    Ok(())
}

use std::time::Duration;

use bevy::prelude::*;
use bevy::window::WindowResolution;
use bevy::winit::{UpdateMode, WinitSettings};

use ratty::config::AppConfig;
use ratty::plugin::TerminalPlugin;
use ratty::runtime::TerminalRuntime;
use ratty::terminal::TerminalSurface;

/// Focused-window update interval for low-power winit mode.
const FOCUSED_UPDATE_INTERVAL: Duration = Duration::from_millis(33);
/// Unfocused-window update interval for low-power winit mode.
const UNFOCUSED_UPDATE_INTERVAL: Duration = Duration::from_millis(250);

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
        .insert_resource(WinitSettings {
            focused_mode: UpdateMode::reactive_low_power(FOCUSED_UPDATE_INTERVAL),
            unfocused_mode: UpdateMode::reactive_low_power(UNFOCUSED_UPDATE_INTERVAL),
        })
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

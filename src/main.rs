use std::time::Duration;

use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::window::WindowResolution;
use bevy::winit::{UpdateMode, WinitSettings};
use clap::Parser;

use ratty::cli::Cli;
use ratty::config::AppConfig;
use ratty::paths::runtime_asset_root;
use ratty::plugin::TerminalPlugin;
use ratty::runtime::{RuntimeOptions, TerminalRuntime};
use ratty::terminal::TerminalSurface;

/// Focused-window update interval for low-power winit mode.
const FOCUSED_UPDATE_INTERVAL: Duration = Duration::from_millis(33);

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let app_config = AppConfig::load_from_path(cli.config_file.as_deref())?;
    let runtime = TerminalRuntime::spawn(
        &app_config,
        &RuntimeOptions {
            command: cli.command.clone(),
            working_dir: Some(std::env::current_dir()?),
        },
    )?;
    let terminal = TerminalSurface::new(&app_config)?;
    let window_title = cli.title;
    let asset_root = runtime_asset_root();
    std::fs::create_dir_all(&asset_root)?;

    App::new()
        .insert_resource(ClearColor(Color::srgba_u8(
            app_config.theme.background[0],
            app_config.theme.background[1],
            app_config.theme.background[2],
            (app_config.window.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
        )))
        .insert_resource(app_config.clone())
        .insert_non_send_resource(runtime)
        .insert_non_send_resource(terminal)
        .insert_resource(WinitSettings {
            focused_mode: UpdateMode::reactive_low_power(FOCUSED_UPDATE_INTERVAL),
            unfocused_mode: UpdateMode::Continuous,
        })
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: window_title,
                        resolution: WindowResolution::new(
                            app_config.window.width,
                            app_config.window.height,
                        )
                        .with_scale_factor_override(app_config.window.scale_factor),
                        transparent: app_config.window.opacity < 1.0,
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    file_path: asset_root.to_string_lossy().into_owned(),
                    ..default()
                }),
        )
        .add_plugins(TerminalPlugin)
        .run();

    Ok(())
}

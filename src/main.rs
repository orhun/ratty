use std::time::Duration;

use anyhow::anyhow;
use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowCreated, WindowResolution};
use bevy::winit::{UpdateMode, WINIT_WINDOWS, WinitSettings};
use clap::Parser;
use winit::window::Icon;

#[cfg(target_os = "windows")]
use winit::platform::windows::WindowExtWindows;

use ratty::cli::Cli;
use ratty::config::AppConfig;
use ratty::paths::runtime_asset_root;
use ratty::plugin::TerminalPlugin;
use ratty::runtime::{RuntimeOptions, TerminalRuntime};
use ratty::terminal::TerminalSurface;

/// Focused-window update interval for low-power winit mode.
const FOCUSED_UPDATE_INTERVAL: Duration = Duration::from_millis(33);
const WINDOW_ICON: &[u8] = include_bytes!("../assets/ratty.ico");

struct AppWindowIcon {
    icon: Icon,
}

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
    let window_icon = load_window_icon()?;

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
        .insert_non_send_resource(AppWindowIcon { icon: window_icon })
        .insert_resource(WinitSettings {
            focused_mode: UpdateMode::reactive_low_power(FOCUSED_UPDATE_INTERVAL),
            unfocused_mode: UpdateMode::Continuous,
        })
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: window_title.clone(),
                        name: Some(window_title),
                        resolution: WindowResolution::new(
                            app_config.window.width,
                            app_config.window.height,
                        )
                        .with_scale_factor_override(app_config.window.scale_factor),
                        transparent: app_config.window.opacity < 1.0,
                        visible: false,
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    file_path: asset_root.to_string_lossy().into_owned(),
                    ..default()
                }),
        )
        .add_systems(Update, apply_window_icon)
        .add_plugins(TerminalPlugin)
        .run();

    Ok(())
}

fn apply_window_icon(
    mut window_created_events: MessageReader<WindowCreated>,
    app_icon: NonSend<AppWindowIcon>,
    mut primary_windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    for event in window_created_events.read() {
        let Ok(mut primary_window) = primary_windows.get_mut(event.window) else {
            continue;
        };

        WINIT_WINDOWS.with(|winit_windows| {
            let winit_windows = winit_windows.borrow();
            let Some(window) = winit_windows.get_window(event.window) else {
                return;
            };

            window.set_window_icon(Some(app_icon.icon.clone()));

            #[cfg(target_os = "windows")]
            window.set_taskbar_icon(Some(app_icon.icon.clone()));

            if !primary_window.visible {
                window.set_visible(true);
                primary_window.visible = true;
            }
        });
    }
}

fn load_window_icon() -> anyhow::Result<Icon> {
    let image =
        image::load_from_memory_with_format(WINDOW_ICON, image::ImageFormat::Ico)?.into_rgba8();
    let (width, height) = image.dimensions();

    Icon::from_rgba(image.into_raw(), width, height)
        .map_err(|error| anyhow!("failed to create window icon: {error}"))
}

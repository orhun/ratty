//! Application configuration types.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use bevy::prelude::Resource;
use etcetera::{BaseStrategy, choose_base_strategy};
use serde::{Deserialize, Deserializer};

/// Application name used for config discovery.
pub const APP_NAME: &str = "ratty";
/// Local fallback config path.
pub const CONFIG_PATH: &str = "config/ratty.toml";
/// Label used for the terminal render target.
pub const TERMINAL_TEXTURE_LABEL: &str = "ratty.parley_ratatui";
/// Z depth used for the cursor model root.
pub const CURSOR_DEPTH: f32 = 10.0;

/// Application configuration.
#[derive(Resource, Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    /// Window settings.
    pub window: WindowConfig,
    /// Terminal grid settings.
    pub terminal: TerminalConfig,
    /// Shell spawning settings.
    pub shell: ShellConfig,
    /// Extra environment variables.
    pub env: BTreeMap<String, String>,
    /// User-defined key bindings.
    pub bindings: BindingsConfig,
    /// Font settings.
    pub font: FontConfig,
    /// Theme settings.
    pub theme: ThemeConfig,
    /// Cursor settings.
    pub cursor: CursorConfig,
}

impl AppConfig {
    /// Loads the application configuration.
    ///
    /// System config is preferred over the local fallback file when both exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected config file cannot be read or parsed.
    pub fn load() -> anyhow::Result<Self> {
        let strategy =
            choose_base_strategy().context("failed to determine system config directory")?;
        let system_path = strategy.config_dir().join(APP_NAME).join("ratty.toml");
        let local_path = PathBuf::from(CONFIG_PATH);
        let Some(path) = (if system_path.exists() {
            Some(system_path)
        } else if local_path.exists() {
            Some(local_path)
        } else {
            None
        }) else {
            return Ok(Self::default());
        };

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
        if config.cursor.model.path.is_relative()
            && config
                .cursor
                .model
                .path
                .parent()
                .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            config.cursor.model.path = config_dir.join(&config.cursor.model.path);
        }
        Ok(config)
    }
}

/// Window configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    /// Window width in logical pixels.
    pub width: u32,
    /// Window height in logical pixels.
    pub height: u32,
    /// Window scale-factor override.
    pub scale_factor: f32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 960,
            height: 620,
            scale_factor: 1.0,
        }
    }
}

/// Terminal grid configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    /// Default terminal column count.
    pub default_cols: u16,
    /// Default terminal row count.
    pub default_rows: u16,
    /// Scrollback line count.
    pub scrollback: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            default_cols: 104,
            default_rows: 32,
            scrollback: 2_000,
        }
    }
}

/// Shell configuration.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ShellConfig {
    /// Shell program path.
    pub program: Option<PathBuf>,
    /// Shell arguments.
    pub args: Vec<String>,
}

/// Key binding configuration.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct BindingsConfig {
    /// Configured key bindings.
    pub keys: Vec<KeyBindingConfig>,
}

/// Single key binding entry.
#[derive(Debug, Clone, Deserialize)]
pub struct KeyBindingConfig {
    /// Key name.
    pub key: String,
    /// Modifier expression.
    #[serde(default)]
    pub with: String,
    /// Bound action.
    pub action: BindingAction,
}

/// Terminal binding action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum BindingAction {
    /// Disables a binding.
    #[serde(rename = "none")]
    None,
    /// Toggles the presentation mode.
    #[serde(rename = "ToggleMode")]
    ToggleMode,
    /// Increases plane warp.
    #[serde(rename = "IncreaseWarp")]
    IncreaseWarp,
    /// Decreases plane warp.
    #[serde(rename = "DecreaseWarp")]
    DecreaseWarp,
    /// Copies the current selection.
    #[serde(rename = "Copy")]
    Copy,
    /// Pastes clipboard contents.
    #[serde(rename = "Paste")]
    Paste,
    /// Increases the font size.
    #[serde(rename = "IncreaseFontSize")]
    IncreaseFontSize,
    /// Decreases the font size.
    #[serde(rename = "DecreaseFontSize")]
    DecreaseFontSize,
    /// Resets the font size.
    #[serde(rename = "ResetFontSize")]
    ResetFontSize,
}

/// Font configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    /// Font family name.
    pub family: String,
    /// Font style override.
    pub style: FontStyleConfig,
    /// Font size in logical pixels.
    pub size: i32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "DejaVu Sans Mono".to_string(),
            style: FontStyleConfig::Regular,
            size: 18,
        }
    }
}

/// Font style override.
#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub enum FontStyleConfig {
    /// Regular font style.
    #[serde(rename = "Regular")]
    #[default]
    Regular,
    /// Bold font style.
    #[serde(rename = "Bold")]
    Bold,
    /// Italic font style.
    #[serde(rename = "Italic")]
    Italic,
    /// Bold italic font style.
    #[serde(rename = "BoldItalic")]
    BoldItalic,
}

/// Terminal theme configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// Default foreground color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub foreground: [u8; 3],
    /// Default background color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub background: [u8; 3],
    /// Cursor color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub cursor: [u8; 3],
    /// ANSI 0..7 colors.
    #[serde(default = "ThemePaletteConfig::default_normal")]
    pub normal: ThemePaletteConfig,
    /// ANSI 8..15 colors.
    #[serde(default = "ThemePaletteConfig::default_bright")]
    pub bright: ThemePaletteConfig,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            foreground: [220, 215, 186],
            background: [31, 31, 40],
            cursor: [126, 156, 216],
            normal: ThemePaletteConfig::default_normal(),
            bright: ThemePaletteConfig::default_bright(),
        }
    }
}

impl ThemeConfig {
    /// Returns the ANSI 0..15 palette.
    pub fn palette(&self) -> [[u8; 3]; 16] {
        [
            self.normal.black,
            self.normal.red,
            self.normal.green,
            self.normal.yellow,
            self.normal.blue,
            self.normal.magenta,
            self.normal.cyan,
            self.normal.white,
            self.bright.black,
            self.bright.red,
            self.bright.green,
            self.bright.yellow,
            self.bright.blue,
            self.bright.magenta,
            self.bright.cyan,
            self.bright.white,
        ]
    }
}

/// Eight-color theme palette.
#[derive(Debug, Clone, Deserialize)]
pub struct ThemePaletteConfig {
    /// Black color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub black: [u8; 3],
    /// Red color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub red: [u8; 3],
    /// Green color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub green: [u8; 3],
    /// Yellow color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub yellow: [u8; 3],
    /// Blue color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub blue: [u8; 3],
    /// Magenta color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub magenta: [u8; 3],
    /// Cyan color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub cyan: [u8; 3],
    /// White color.
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub white: [u8; 3],
}

impl ThemePaletteConfig {
    /// Returns the default ANSI 0..7 palette.
    pub fn default_normal() -> Self {
        Self {
            black: [0, 0, 0],
            red: [205, 49, 49],
            green: [13, 188, 121],
            yellow: [229, 229, 16],
            blue: [36, 114, 200],
            magenta: [188, 63, 188],
            cyan: [17, 168, 205],
            white: [229, 229, 229],
        }
    }

    /// Returns the default ANSI 8..15 palette.
    pub fn default_bright() -> Self {
        Self {
            black: [102, 102, 102],
            red: [241, 76, 76],
            green: [35, 209, 139],
            yellow: [245, 245, 67],
            blue: [59, 142, 234],
            magenta: [214, 112, 214],
            cyan: [41, 184, 219],
            white: [255, 255, 255],
        }
    }
}

/// Cursor configuration.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct CursorConfig {
    /// Cursor model settings.
    pub model: CursorModelConfig,
    /// Cursor animation settings.
    pub animation: CursorAnimationConfig,
}

/// Cursor model configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CursorModelConfig {
    /// Enables the custom cursor model.
    pub visible: bool,
    /// Model scale multiplier.
    pub scale_factor: f32,
    /// Horizontal model offset.
    pub x_offset: f32,
    /// Plane distance in 3D mode.
    pub plane_offset: f32,
    /// Cursor model brightness.
    pub brightness: f32,
    /// Cursor asset path.
    pub path: PathBuf,
}

impl Default for CursorModelConfig {
    fn default() -> Self {
        Self {
            visible: true,
            scale_factor: 6.0,
            x_offset: 0.1,
            plane_offset: 18.0,
            brightness: 1.0,
            path: PathBuf::from("CairoSpinyMouse.obj"),
        }
    }
}

/// Cursor animation configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CursorAnimationConfig {
    /// Spin speed.
    pub spin_speed: f32,
    /// Bob speed.
    pub bob_speed: f32,
    /// Bob amplitude.
    pub bob_amplitude: f32,
}

impl Default for CursorAnimationConfig {
    fn default() -> Self {
        Self {
            spin_speed: 1.4,
            bob_speed: 2.2,
            bob_amplitude: 0.08,
        }
    }
}

fn deserialize_hex_color<'de, D>(deserializer: D) -> Result<[u8; 3], D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    parse_hex_color(&value).map_err(serde::de::Error::custom)
}

fn parse_hex_color(value: &str) -> anyhow::Result<[u8; 3]> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 {
        anyhow::bail!("expected hex color in #RRGGBB format, got {value}");
    }

    let r = u8::from_str_radix(&hex[0..2], 16)
        .with_context(|| format!("invalid red component in {value}"))?;
    let g = u8::from_str_radix(&hex[2..4], 16)
        .with_context(|| format!("invalid green component in {value}"))?;
    let b = u8::from_str_radix(&hex[4..6], 16)
        .with_context(|| format!("invalid blue component in {value}"))?;
    Ok([r, g, b])
}

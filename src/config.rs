use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use bevy::prelude::Resource;
use etcetera::{BaseStrategy, choose_base_strategy};
use serde::{Deserialize, Deserializer};

pub const APP_NAME: &str = "ratty";
pub const CONFIG_PATH: &str = "config/ratty.toml";
pub const TERMINAL_TEXTURE_LABEL: &str = "ratty.parley_ratatui";
pub const CURSOR_DEPTH: f32 = 10.0;

#[derive(Resource, Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub window: WindowConfig,
    pub terminal: TerminalConfig,
    pub shell: ShellConfig,
    pub env: BTreeMap<String, String>,
    pub bindings: BindingsConfig,
    pub font: FontConfig,
    pub theme: ThemeConfig,
    pub cursor: CursorConfig,
}

impl AppConfig {
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

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            window: WindowConfig::default(),
            terminal: TerminalConfig::default(),
            shell: ShellConfig::default(),
            env: BTreeMap::new(),
            bindings: BindingsConfig::default(),
            font: FontConfig::default(),
            theme: ThemeConfig::default(),
            cursor: CursorConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub width: u32,
    pub height: u32,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    pub default_cols: u16,
    pub default_rows: u16,
    pub scrollback: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            default_cols: 104,
            default_rows: 32,
            scrollback: 10_000,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            program: None,
            args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct BindingsConfig {
    pub keys: Vec<KeyBindingConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeyBindingConfig {
    pub key: String,
    #[serde(default)]
    pub with: String,
    pub action: BindingAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum BindingAction {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "ToggleMode")]
    ToggleMode,
    #[serde(rename = "IncreaseWarp")]
    IncreaseWarp,
    #[serde(rename = "DecreaseWarp")]
    DecreaseWarp,
    #[serde(rename = "Copy")]
    Copy,
    #[serde(rename = "Paste")]
    Paste,
    #[serde(rename = "IncreaseFontSize")]
    IncreaseFontSize,
    #[serde(rename = "DecreaseFontSize")]
    DecreaseFontSize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub style: FontStyleConfig,
    pub size: i32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "JetBrainsMono Nerd Font Mono".to_string(),
            style: FontStyleConfig::Regular,
            size: 14,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum FontStyleConfig {
    #[serde(rename = "Regular")]
    Regular,
    #[serde(rename = "Bold")]
    Bold,
    #[serde(rename = "Italic")]
    Italic,
    #[serde(rename = "BoldItalic")]
    BoldItalic,
}

impl Default for FontStyleConfig {
    fn default() -> Self {
        Self::Regular
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub foreground: [u8; 3],
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub background: [u8; 3],
    #[serde(deserialize_with = "deserialize_hex_color")]
    pub cursor: [u8; 3],
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            foreground: [220, 215, 186],
            background: [31, 31, 40],
            cursor: [126, 156, 216],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CursorConfig {
    pub model: CursorModelConfig,
    pub animation: CursorAnimationConfig,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            model: CursorModelConfig::default(),
            animation: CursorAnimationConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CursorModelConfig {
    pub visible: bool,
    pub scale_factor: f32,
    pub x_offset: f32,
    pub plane_offset: f32,
    pub brightness: f32,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CursorAnimationConfig {
    pub spin_speed: f32,
    pub bob_speed: f32,
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

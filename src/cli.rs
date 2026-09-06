//! Command-line argument parsing.

use std::path::PathBuf;

use clap::Parser;

/// Default window title.
pub const DEFAULT_WINDOW_TITLE: &str = "Ratty";

/// Command-line arguments for Ratty.
#[derive(Debug, Parser)]
#[command(
    name = env!("CARGO_PKG_NAME"),
    version,
    about = "A GPU-rendered terminal emulator with inline 3D graphics",
    trailing_var_arg = true
)]
pub struct Cli {
    /// Enable the authenticated local control socket used by `ratty-mcp`.
    #[arg(long)]
    pub mcp: bool,

    /// Specify an alternative configuration file.
    #[arg(short = 'c', long = "config-file", value_name = "CONFIG_FILE")]
    pub config_file: Option<PathBuf>,

    /// Command and args to execute (must be last argument).
    #[arg(
        short = 'e',
        long = "command",
        value_name = "COMMAND",
        num_args = 1..,
        allow_hyphen_values = true
    )]
    pub command: Option<Vec<String>>,

    /// Defines the window title.
    #[arg(
        short = 'T',
        long = "title",
        value_name = "TITLE",
        default_value = DEFAULT_WINDOW_TITLE
    )]
    pub title: String,
}

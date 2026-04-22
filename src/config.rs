use ratatui::style::Color as TuiColor;

pub const WINDOW_WIDTH: f32 = 1180.0;
pub const WINDOW_HEIGHT: f32 = 760.0;
pub const DEFAULT_COLS: u16 = 104;
pub const DEFAULT_ROWS: u16 = 32;
pub const TERMINAL_SCROLLBACK: usize = 10_000;
pub const VIEW_PADDING: f32 = 64.0;
pub const CURSOR_DEPTH: f32 = 10.0;
pub const CURSOR_SCALE_FACTOR: f32 = 5.2;
pub const TERMINAL_FONT_SIZE: i32 = 18;

pub const THEME_BG: TuiColor = TuiColor::Rgb(244, 240, 231);
pub const THEME_FG: TuiColor = TuiColor::Rgb(32, 37, 44);

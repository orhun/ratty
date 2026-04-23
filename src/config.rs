use ratatui::style::Color as TuiColor;

pub const WINDOW_WIDTH: f32 = 960.0;
pub const WINDOW_HEIGHT: f32 = 620.0;
pub const DEFAULT_COLS: u16 = 104;
pub const DEFAULT_ROWS: u16 = 32;
pub const TERMINAL_SCROLLBACK: usize = 10_000;
pub const CURSOR_DEPTH: f32 = 10.0;
pub const CURSOR_SCALE_FACTOR: f32 = 6.;
pub const CURSOR_PLANE_OFFSET: f32 = 18.0;
pub const TERMINAL_FONT_SIZE: i32 = 14;

pub const THEME_BG: TuiColor = TuiColor::Rgb(31, 31, 40);
pub const THEME_FG: TuiColor = TuiColor::Rgb(220, 215, 186);

use bevy::prelude::*;
use ratatui::Terminal;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as TuiColor, Modifier, Style};
use ratatui::widgets::Widget;
use soft_ratatui::{EmbeddedTTF, SoftBackend};

use crate::config::{TERMINAL_FONT_SIZE, THEME_BG, THEME_FG};

static TERMINAL_FONT_DATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/fonts/UbuntuMonoLigaturized-Regular.ttf"
));
static TERMINAL_FONT: std::sync::OnceLock<soft_ratatui::rusttype::Font<'static>> =
    std::sync::OnceLock::new();

pub struct TerminalSurface {
    pub tui: Terminal<SoftBackend<EmbeddedTTF>>,
    pub image_handle: Option<Handle<Image>>,
    pub cols: u16,
    pub rows: u16,
}

impl TerminalSurface {
    pub fn new(cols: u16, rows: u16) -> Self {
        let font_regular = TERMINAL_FONT
            .get_or_init(|| {
                soft_ratatui::rusttype::Font::try_from_bytes(TERMINAL_FONT_DATA)
                    .expect("embedded terminal font failed to load")
            })
            .clone();
        let backend = SoftBackend::<EmbeddedTTF>::new(
            cols,
            rows,
            TERMINAL_FONT_SIZE as u32,
            font_regular,
            None,
            None,
        );

        let mut tui =
            Terminal::new(backend).expect("soft_ratatui backend is infallible for Terminal::new");
        let _ = tui.clear();
        tui.backend_mut().cursor = false;

        Self {
            tui,
            image_handle: None,
            cols,
            rows,
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }

        self.tui.backend_mut().resize(cols, rows);
        let _ = self.tui.resize(Rect::new(0, 0, cols, rows));
        self.tui.backend_mut().cursor = false;
        self.cols = cols;
        self.rows = rows;
    }
}

pub struct TerminalWidget<'a> {
    pub screen: &'a vt100::Screen,
}

impl Widget for TerminalWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        buf.set_style(area, Style::default().fg(THEME_FG).bg(THEME_BG));

        let (rows, cols) = self.screen.size();
        let draw_rows = rows.min(area.height);
        let draw_cols = cols.min(area.width);

        for row in 0..draw_rows {
            for col in 0..draw_cols {
                let Some(vt_cell) = self.screen.cell(row, col) else {
                    continue;
                };
                if vt_cell.is_wide_continuation() {
                    continue;
                }

                let style = vt100_cell_style(vt_cell);
                let symbol = if vt_cell.has_contents() {
                    vt_cell.contents()
                } else {
                    " "
                };

                buf[(area.x + col, area.y + row)]
                    .set_symbol(symbol)
                    .set_style(style);
            }
        }
    }
}

fn vt100_cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(vt100_color_to_tui(cell.fgcolor()).unwrap_or(THEME_FG))
        .bg(vt100_color_to_tui(cell.bgcolor()).unwrap_or(THEME_BG));

    let mut modifiers = Modifier::empty();
    if cell.bold() {
        modifiers |= Modifier::BOLD;
    }
    if cell.dim() {
        modifiers |= Modifier::DIM;
    }
    if cell.italic() {
        modifiers |= Modifier::ITALIC;
    }
    if cell.underline() {
        modifiers |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        modifiers |= Modifier::REVERSED;
    }

    style = style.add_modifier(modifiers);
    style
}

fn vt100_color_to_tui(color: vt100::Color) -> Option<TuiColor> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(index) => Some(ansi_index_to_tui(index)),
        vt100::Color::Rgb(r, g, b) => Some(TuiColor::Rgb(r, g, b)),
    }
}

fn ansi_index_to_tui(index: u8) -> TuiColor {
    match index {
        0 => TuiColor::Black,
        1 => TuiColor::Red,
        2 => TuiColor::Green,
        3 => TuiColor::Yellow,
        4 => TuiColor::Blue,
        5 => TuiColor::Magenta,
        6 => TuiColor::Cyan,
        7 => TuiColor::Gray,
        8 => TuiColor::DarkGray,
        9 => TuiColor::LightRed,
        10 => TuiColor::LightGreen,
        11 => TuiColor::LightYellow,
        12 => TuiColor::LightBlue,
        13 => TuiColor::LightMagenta,
        14 => TuiColor::LightCyan,
        15 => TuiColor::White,
        16..=231 => {
            let index = index - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            TuiColor::Rgb(component(r), component(g), component(b))
        }
        232..=255 => {
            let shade = 8 + (index - 232) * 10;
            TuiColor::Rgb(shade, shade, shade)
        }
    }
}

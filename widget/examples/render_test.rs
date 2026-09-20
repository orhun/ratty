//! Interactive visual coverage for terminal glyphs, colors, and text styles.
//!
//! Run this example inside Ratty to inspect the complete PTY -> ratty-vt ->
//! Ratatui -> renderer path:
//!
//! ```text
//! cargo run --manifest-path widget/Cargo.toml --example render_test
//! ```
//!
//! The "font" button in the header (or `f`) cycles through [`FONTS`] by
//! sending OSC 50 with a font file from the gitignored `fonts/` directory,
//! which Ratty loads in place of a system font.

use std::io::{self, Write};
use std::path::Path;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton, MouseEventKind,
    },
    execute,
};
use ratatui::{
    DefaultTerminal, Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget},
};

/// Fonts the font button cycles through, as `(label, file)` with `file`
/// relative to the gitignored `fonts/` directory at the repository root.
const FONTS: &[(&str, &str)] = &[
    ("DejaVu Sans Mono", "dejavu/DejaVuSansMono.ttf"),
    ("JetBrains Mono", "jetbrains-mono/JetBrainsMono-Regular.ttf"),
    ("Fira Code", "fira-code/FiraCode-Regular.ttf"),
    ("Iosevka", "iosevka/Iosevka-Regular.ttf"),
    ("Menlo", "menlo/Menlo.ttc"),
    ("SF Mono", "sf-mono/SFNSMono.ttf"),
    ("Hack", "hack/Hack-Regular.ttf"),
    (
        "Source Code Pro",
        "source-code-pro/SourceCodePro-Regular.ttf",
    ),
    ("Cascadia Code", "cascadia-code/CascadiaCode-Regular.ttf"),
];

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal);
    execute!(io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

/// Asks the terminal to load the font `file` from `fonts/` (OSC 50 with a path).
fn set_font(file: &str) -> io::Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fonts")
        .join(file);
    let mut stdout = io::stdout();
    write!(stdout, "\x1b]50;{}\x1b\\", path.display())?;
    stdout.flush()
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let lines = fidelity_lines();
    let mut scroll = 0_u16;
    let mut font = None::<usize>;

    loop {
        let mut page_height = 1;
        let mut font_button = Rect::default();
        terminal.draw(|frame| {
            (page_height, font_button) = render(frame, &lines, scroll, font);
        })?;

        let max_scroll = u16::try_from(lines.len().saturating_sub(usize::from(page_height.max(1))))
            .unwrap_or(u16::MAX);
        scroll = scroll.min(max_scroll);

        let mut cycle_font = false;
        match event::read()? {
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    cycle_font = font_button.contains(Position::new(mouse.column, mouse.row));
                }
                MouseEventKind::ScrollUp => scroll = scroll.saturating_sub(1),
                MouseEventKind::ScrollDown => scroll = scroll.saturating_add(1).min(max_scroll),
                _ => {}
            },
            Event::Key(key) if key.is_press() => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('f') => cycle_font = true,
                KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1).min(max_scroll);
                }
                KeyCode::PageUp => scroll = scroll.saturating_sub(page_height.max(1)),
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(page_height.max(1)).min(max_scroll);
                }
                KeyCode::Home => scroll = 0,
                KeyCode::End => scroll = max_scroll,
                _ => {}
            },
            _ => {}
        }
        if cycle_font {
            let next = font.map_or(0, |index| (index + 1) % FONTS.len());
            font = Some(next);
            set_font(FONTS[next].1)?;
        }
    }
}

/// Draws the matrix and returns the page height and the font button's area.
fn render(
    frame: &mut Frame<'_>,
    lines: &[Line<'static>],
    scroll: u16,
    font: Option<usize>,
) -> (u16, Rect) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    Paragraph::new(Line::from(vec![
        Span::styled(
            "Renderer fidelity matrix",
            Style::new().fg(Color::Yellow).bold(),
        ),
        Span::raw(" — inspect alignment, clipping, joins, fallback, and styling"),
    ]))
    .block(Block::bordered().title(" Ratty / Ratatui "))
    .render(header, frame.buffer_mut());

    let font_label = format!(
        " font: {} ▸ ",
        font.map_or("configured", |index| FONTS[index].0)
    );
    let button_width = u16::try_from(font_label.chars().count())
        .unwrap_or(u16::MAX)
        .min(header.width.saturating_sub(4));
    let font_button = Rect::new(
        header.right().saturating_sub(button_width + 2),
        header.y + 1,
        button_width,
        1,
    )
    .intersection(header);
    Paragraph::new(font_label)
        .style(Style::new().fg(Color::Black).bg(Color::Cyan).bold())
        .render(font_button, frame.buffer_mut());

    let page_height = body.height.saturating_sub(2).max(1);
    let max_scroll =
        u16::try_from(lines.len().saturating_sub(usize::from(page_height))).unwrap_or(u16::MAX);
    Paragraph::new(lines.to_vec())
        .scroll((scroll.min(max_scroll), 0))
        .block(Block::bordered().title(format!(
            " samples — line {}/{} ",
            scroll.min(max_scroll).saturating_add(1),
            max_scroll.saturating_add(1)
        )))
        .render(body, frame.buffer_mut());

    Paragraph::new("↑/↓ or j/k: line  PgUp/PgDn: page  Home/End  f/click: font  q/Esc: quit")
        .centered()
        .render(footer, frame.buffer_mut());

    emit_hidden_sequences(frame.buffer_mut());

    (page_height, font_button)
}

/// Wraps every run of [`Modifier::HIDDEN`] cells in SGR 8 / SGR 28.
///
/// Ratatui's crossterm backend does not translate `Modifier::HIDDEN` into an
/// escape sequence, so the terminal would otherwise never learn the text is
/// concealed. The sequences ride along in the cell symbols; a forced width of
/// one keeps the diff from counting the escape bytes as columns.
fn emit_hidden_sequences(buf: &mut Buffer) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut in_run = false;
        for x in area.left()..=area.right() {
            let hidden = x < area.right()
                && buf
                    .cell((x, y))
                    .is_some_and(|cell| cell.modifier.contains(Modifier::HIDDEN));
            match (in_run, hidden) {
                (false, true) => {
                    in_run = true;
                    wrap_symbol(buf, x, y, "\x1b[8m", "");
                }
                (true, false) => {
                    in_run = false;
                    wrap_symbol(buf, x - 1, y, "", "\x1b[28m");
                }
                _ => {}
            }
        }
    }
}

fn wrap_symbol(buf: &mut Buffer, x: u16, y: u16, prefix: &str, suffix: &str) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        let symbol = format!("{prefix}{}{suffix}", cell.symbol());
        cell.set_symbol(&symbol);
        cell.set_diff_option(ratatui::buffer::CellDiffOption::ForcedWidth(
            std::num::NonZeroU16::MIN,
        ));
    }
}

fn fidelity_lines() -> Vec<Line<'static>> {
    let mut lines = vec![
        section("Block, shade, and quadrant elements"),
        labeled(
            "vertical eighths",
            "|▁▂▃▄▅▆▇█| |█▇▆▅▄▃▂▁| edges should touch",
        ),
        labeled(
            "horizontal eighths",
            "|▏▎▍▌▋▊▉█| |█▉▊▋▌▍▎▏| edges should touch",
        ),
        labeled("halves", "|▀▄| |▌▐| |▀▄▀▄▀▄| |▌▐▌▐▌▐|"),
        labeled("shades", "|░░░░|▒▒▒▒|▓▓▓▓|████|"),
        labeled("quadrants", "|▖▗▘▝| |▙▚▛▜▞▟| |▖▗| complementary corners"),
        Line::from(vec![
            label("color tiles"),
            Span::styled("████", Style::new().fg(Color::Red)),
            Span::styled("▀▀▀▀", Style::new().fg(Color::Yellow).bg(Color::Blue)),
            Span::styled("▄▄▄▄", Style::new().fg(Color::Green).bg(Color::Magenta)),
            Span::styled("▌▐▌▐", Style::new().fg(Color::Cyan)),
            Span::raw(" no seams between cells"),
        ]),
        Line::default(),
        section("Box drawing — joins should meet cleanly"),
    ];
    lines.extend(box_samples());
    lines.push(labeled(
        "loose glyphs",
        "┄┅┈┉ ╌╍ ╴╵╶╷ ╸╹╺╻ ┌┐└┘ ┏┓┗┛ ╔╗╚╝ ╭╮╰╯",
    ));
    lines.push(Line::default());

    lines.push(section("Braille — every dot position and cell edge"));
    lines.push(labeled("single dots", "|⠁⠂⠄⡀⢀⠠⠐⠈| |⡁⡂⡄⢁⢂⢄|"));
    lines.push(labeled("density ramp", "|⠀⠁⠃⠇⡇⣇⣧⣷⣿| |⣿⣶⣤⣀⠀|"));
    lines.push(labeled("patterns", "|⣿⡿⠿⢿⣿| |⣀⣤⣶⣿⣶⣤⣀| |⠒⠤⢀⡀|"));
    lines.push(labeled("checker", "|⡪⢕⡪⢕⡪⢕| |⠛⣛⠛⣛⠛⣛|"));
    lines.push(Line::default());

    lines.push(section("Wide glyphs, scripts, emoji, and fallback"));
    lines.push(labeled("CJK guards", "|日本語|简体中文|繁體中文|한글|"));
    lines.push(labeled(
        "CJK punctuation",
        "|「端末」|【方格】|（全角）|１２３|",
    ));
    lines.push(labeled("emoji guards", "|😀|🚀|❤️|👍🏽|👩‍💻|🏳️‍🌈|🇯🇵|1️⃣|"));
    lines.push(labeled("symbols", "←↑→↓ ↔↕ ⇐⇑⇒⇓ ∀∂∑√∞≈≠≤≥ ◆◇○●★☆"));
    lines.push(labeled(
        "powerline/PUA",
        "|| || glyphs require a supporting font",
    ));
    lines.push(labeled("Greek/Cyrillic", "Καλημέρα κόσμε — Привет, мир"));
    lines.push(labeled("Arabic/Hebrew", "|مرحبا بالعالم| |שלום עולם|"));
    lines.push(labeled("Indic/Thai", "|नमस्ते दुनिया| |สวัสดีชาวโลก|"));
    lines.push(Line::default());

    lines.push(section("Combining marks and grapheme clusters"));
    lines.push(labeled("precomposed", "|café| |Ångström| |piñata|"));
    lines.push(labeled(
        "decomposed",
        "|cafe\u{301}| |A\u{30a}ngstro\u{308}m| |pin\u{303}ata|",
    ));
    lines.push(labeled(
        "stacked marks",
        "|a\u{301}\u{323}| |Z\u{302}\u{303}\u{304}| |x\u{336}| |q\u{307}\u{328}|",
    ));
    lines.push(labeled("ZWJ sequences", "|👨‍👩‍👧‍👦| |👩‍🚀| |🧑🏽‍💻| |🏴‍☠️|"));
    lines.push(Line::default());

    lines.push(section("Text modifiers and combinations"));
    lines.extend(style_samples());
    lines.push(Line::from(vec![
        label("visibility"),
        Span::raw("visible ["),
        Span::styled("hidden", Style::new().add_modifier(Modifier::HIDDEN)),
        Span::raw("] hidden text should disappear"),
    ]));
    lines.push(Line::from(vec![
        label("blink"),
        Span::styled(
            "slow blink",
            Style::new().add_modifier(Modifier::SLOW_BLINK),
        ),
        Span::raw(" | "),
        Span::styled(
            "rapid blink",
            Style::new().add_modifier(Modifier::RAPID_BLINK),
        ),
        Span::raw(" (PTY/parser support may vary)"),
    ]));
    lines.push(Line::from(vec![
        label("underline color"),
        Span::styled(
            "red underline on white text",
            Style::new()
                .fg(Color::White)
                .underline_color(Color::Red)
                .underlined(),
        ),
    ]));
    lines.push(Line::default());

    lines.push(section("Color models and backgrounds"));
    lines.push(named_colors());
    lines.push(indexed_colors());
    lines.push(truecolor_gradient());
    lines.push(background_colors());

    lines
}

fn box_samples() -> Vec<Line<'static>> {
    [
        "light         ┌────┬────┐   heavy         ┏━━━━┳━━━━┓",
        "              │    │    │                 ┃    ┃    ┃",
        "              ├────┼────┤                 ┣━━━━╋━━━━┫",
        "              └────┴────┘                 ┗━━━━┻━━━━┛",
        "double        ╔════╦════╗   mixed         ╒════╤════╕",
        "              ║    ║    ║                 │    │    │",
        "              ╠════╬════╣                 ╞════╪════╡",
        "              ╚════╩════╝                 ╘════╧════╛",
    ]
    .into_iter()
    .map(Line::from)
    .collect()
}

fn style_samples() -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            label("font faces"),
            Span::raw("normal  "),
            Span::styled("bold", Style::new().bold()),
            Span::raw("  "),
            Span::styled("dim", Style::new().dim()),
            Span::raw("  "),
            Span::styled("italic", Style::new().italic()),
            Span::raw("  "),
            Span::styled("bold italic", Style::new().bold().italic()),
        ]),
        Line::from(vec![
            label("decorations"),
            Span::styled("underline", Style::new().underlined()),
            Span::raw("  "),
            Span::styled("strike", Style::new().crossed_out()),
            Span::raw("  "),
            Span::styled("reverse", Style::new().reversed()),
        ]),
    ]
}

fn named_colors() -> Line<'static> {
    let colors = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    let mut spans = vec![label("ANSI 16")];
    spans.extend(
        colors
            .into_iter()
            .map(|color| Span::styled("██", Style::new().fg(color))),
    );
    Line::from(spans)
}

fn indexed_colors() -> Line<'static> {
    let mut spans = vec![label("indexed")];
    spans.extend(
        [
            16_u8, 22, 28, 34, 40, 46, 51, 45, 39, 33, 27, 21, 57, 93, 129, 201,
        ]
        .into_iter()
        .map(|index| Span::styled("██", Style::new().fg(Color::Indexed(index)))),
    );
    Line::from(spans)
}

fn truecolor_gradient() -> Line<'static> {
    let mut spans = vec![label("truecolor")];
    spans.extend((0_u16..=15).map(|step| {
        let red = u8::try_from(step * 17).unwrap_or(u8::MAX);
        let blue = u8::MAX.saturating_sub(red);
        Span::styled("██", Style::new().fg(Color::Rgb(red, 96, blue)))
    }));
    Line::from(spans)
}

fn background_colors() -> Line<'static> {
    Line::from(vec![
        label("backgrounds"),
        Span::styled(" red ", Style::new().fg(Color::White).bg(Color::Red)),
        Span::styled(" green ", Style::new().fg(Color::Black).bg(Color::Green)),
        Span::styled(" blue ", Style::new().fg(Color::White).bg(Color::Blue)),
        Span::styled(
            " rgb ",
            Style::new()
                .fg(Color::Rgb(255, 255, 255))
                .bg(Color::Rgb(96, 48, 160)),
        ),
    ])
}

fn section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {title} ──"),
        Style::new().fg(Color::Yellow).bold(),
    ))
}

fn labeled(label_text: &'static str, sample: &'static str) -> Line<'static> {
    Line::from(vec![label(label_text), Span::raw(sample)])
}

fn label(text: &'static str) -> Span<'static> {
    Span::styled(format!("{text:>18}: "), Style::new().fg(Color::Cyan))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn sample_renders_first_and_last_pages() {
        let lines = fidelity_lines();
        assert!(lines.len() > 40);

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("test terminal");
        let mut page_height = 0;
        terminal
            .draw(|frame| page_height = render(frame, &lines, 0, None).0)
            .expect("render first page");
        let first_page = rendered_text(&terminal);
        assert!(first_page.contains("Block, shade, and quadrant elements"));

        let last_page = u16::try_from(lines.len().saturating_sub(usize::from(page_height.max(1))))
            .unwrap_or(u16::MAX);
        terminal
            .draw(|frame| {
                render(frame, &lines, last_page, Some(0));
            })
            .expect("render last page");
        let last_page = rendered_text(&terminal);
        assert!(last_page.contains("truecolor"));
        assert!(last_page.contains("backgrounds"));
    }

    fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
}

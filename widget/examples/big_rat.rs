use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    DefaultTerminal,
    layout::Constraint,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget},
};
use ratatui_ratty::{RattyGraphic, RattyGraphicSettings};

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut graphic = RattyGraphic::new(
        RattyGraphicSettings::new("assets/objects/SpinyMouse.glb")
            .id(7)
            .animate(true)
            .scale(1.0),
    );
    graphic.register()?;
    let mut camera = CameraPreset::Ortho;
    camera.apply()?;
    let mut area = Rect::new(0, 0, 24, 10);
    let mut centered = false;

    loop {
        terminal.draw(|frame| {
            let screen = frame.area();
            Paragraph::new(Line::from(vec![
                Span::styled("arrows", Style::default().fg(Color::Cyan)),
                Span::raw(": move  "),
                Span::styled("+/-", Style::default().fg(Color::Cyan)),
                Span::raw(format!(": scale ({:.1})  ", graphic.settings().scale)),
                Span::styled("[/]", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    ": brightness ({:.1})  ",
                    graphic.settings().brightness
                )),
                Span::styled("1-6", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    ": rot [{:.0}, {:.0}, {:.0}]  ",
                    graphic.settings().rotation[0],
                    graphic.settings().rotation[1],
                    graphic.settings().rotation[2]
                )),
                Span::styled("a", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    ": animate ({})  ",
                    u8::from(graphic.settings().animate)
                )),
                Span::styled("v", Style::default().fg(Color::Cyan)),
                Span::raw(format!(": camera ({})  ", camera.label())),
                Span::styled("c", Style::default().fg(Color::Cyan)),
                Span::raw(": clear  "),
                Span::styled("r", Style::default().fg(Color::Cyan)),
                Span::raw(": reset  "),
                Span::styled("q", Style::default().fg(Color::Cyan)),
                Span::raw(": quit"),
            ]))
            .block(Block::bordered().title(Span::styled(
                "Ratty Graphics Protocol Demo",
                Style::default().fg(Color::Yellow),
            )))
            .render(Rect::new(0, 0, screen.width, 3), frame.buffer_mut());

            let viewport = Rect::new(0, 3, screen.width, screen.height.saturating_sub(3));
            Block::bordered().render(viewport, frame.buffer_mut());

            let inner = Rect::new(
                viewport.x.saturating_add(1),
                viewport.y.saturating_add(1),
                viewport.width.saturating_sub(2),
                viewport.height.saturating_sub(2),
            );
            if !centered {
                area = inner.centered(
                    Constraint::Length(area.width.min(inner.width.max(1))),
                    Constraint::Length(area.height.min(inner.height.max(1))),
                );
                centered = true;
            }
            fill_background(inner, frame.buffer_mut());
            let bounded = clamp_rect(area, inner);

            (&graphic).render(bounded, frame.buffer_mut());
        })?;

        if let Event::Key(key) = event::read()? {
            let mut send_update = false;
            match (key.code, key.modifiers) {
                (KeyCode::Char('q'), _) => {
                    graphic.clear()?;
                    return Ok(());
                }
                (KeyCode::Char('c'), _) => {
                    graphic.clear()?;
                }
                (KeyCode::Char('a'), _) => {
                    let animate = graphic.settings().animate;
                    graphic.settings_mut().animate = !animate;
                    send_update = true;
                }
                (KeyCode::Char('v'), _) => {
                    camera = camera.next();
                    camera.apply()?;
                }
                (KeyCode::Char('r'), _) => {
                    area = Rect::new(0, 0, 24, 10);
                    graphic.settings_mut().animate = true;
                    *graphic.settings_mut() =
                        RattyGraphicSettings::new("assets/objects/SpinyMouse.glb")
                            .id(7)
                            .animate(true)
                            .scale(1.0)
                            .brightness(0.9);
                    camera = CameraPreset::Ortho;
                    camera.apply()?;
                    centered = false;
                    send_update = true;
                }
                (KeyCode::Char('+'), _) | (KeyCode::Char('='), _) => {
                    graphic.settings_mut().scale += 0.1;
                    send_update = true;
                }
                (KeyCode::Char('-'), _) => {
                    graphic.settings_mut().scale = (graphic.settings().scale - 0.1).max(0.1);
                    send_update = true;
                }
                (KeyCode::Char(']'), _) => {
                    graphic.settings_mut().brightness += 0.1;
                    send_update = true;
                }
                (KeyCode::Char('['), _) => {
                    graphic.settings_mut().brightness =
                        (graphic.settings().brightness - 0.1).max(0.1);
                    send_update = true;
                }
                (KeyCode::Char('1'), _) => {
                    graphic.settings_mut().rotation[0] -= 15.0;
                    send_update = true;
                }
                (KeyCode::Char('2'), _) => {
                    graphic.settings_mut().rotation[0] += 15.0;
                    send_update = true;
                }
                (KeyCode::Char('3'), _) => {
                    graphic.settings_mut().rotation[1] -= 15.0;
                    send_update = true;
                }
                (KeyCode::Char('4'), _) => {
                    graphic.settings_mut().rotation[1] += 15.0;
                    send_update = true;
                }
                (KeyCode::Char('5'), _) => {
                    graphic.settings_mut().rotation[2] -= 15.0;
                    send_update = true;
                }
                (KeyCode::Char('6'), _) => {
                    graphic.settings_mut().rotation[2] += 15.0;
                    send_update = true;
                }
                (KeyCode::Left, _) => {
                    area.x = area.x.saturating_sub(1);
                }
                (KeyCode::Right, _) => {
                    area.x = area.x.saturating_add(1);
                }
                (KeyCode::Up, _) => {
                    area.y = area.y.saturating_sub(1);
                }
                (KeyCode::Down, _) => {
                    area.y = area.y.saturating_add(1);
                }
                _ => {}
            }

            if send_update {
                graphic.update()?;
            }
        }
    }
}

/// Camera presets cycled with the `v` key, sent as RGP `c` commands.
/// `scale` is orthographic zoom; `fov` and all rotations are degrees.
#[derive(Clone, Copy)]
enum CameraPreset {
    Flat,
    Ortho,
    Perspective,
    Mobius,
}

impl CameraPreset {
    fn next(self) -> Self {
        match self {
            Self::Flat => Self::Ortho,
            Self::Ortho => Self::Perspective,
            Self::Perspective => Self::Mobius,
            Self::Mobius => Self::Flat,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Ortho => "ortho",
            Self::Perspective => "persp",
            Self::Mobius => "mobius",
        }
    }

    fn apply(self) -> io::Result<()> {
        let sequence = match self {
            Self::Flat => "\x1b_ratty;g;c;id=0;set=1;type=Flat\x1b\\".to_string(),
            Self::Ortho => {
                "\x1b_ratty;g;c;id=0;set=1;type=Ortho;scale=0.95;rx=8;ry=18;rz=2\x1b\\".to_string()
            }
            Self::Perspective => {
                "\x1b_ratty;g;c;id=0;set=1;type=Persp;fov=50;pz=40;rx=10;ry=20;rz=2\x1b\\"
                    .to_string()
            }
            Self::Mobius => {
                "\x1b_ratty;g;c;id=0;set=1;type=Mobius;scale=0.9;rx=12;ry=24;rz=4\x1b\\".to_string()
            }
        };
        io::stdout().write_all(sequence.as_bytes())?;
        io::stdout().flush()
    }
}

fn clamp_rect(mut rect: Rect, bounds: Rect) -> Rect {
    rect.width = rect.width.min(bounds.width.max(1));
    rect.height = rect.height.min(bounds.height.max(1));

    let max_x = bounds
        .x
        .saturating_add(bounds.width.saturating_sub(rect.width));
    let max_y = bounds
        .y
        .saturating_add(bounds.height.saturating_sub(rect.height));

    rect.x = rect.x.clamp(bounds.x, max_x);
    rect.y = rect.y.clamp(bounds.y, max_y);
    rect
}

fn fill_background(area: Rect, buf: &mut ratatui::buffer::Buffer) {
    let pattern = ['·', ' ', '·', '·', '·', ' ', '·', '·'];
    let style = Style::default().fg(Color::Indexed(8));

    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let index = ((x - area.x) as usize + (y - area.y) as usize * 3) % pattern.len();
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(pattern[index]).set_style(style);
            }
        }
    }
}

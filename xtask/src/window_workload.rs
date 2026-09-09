use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, ValueEnum)]
enum Scene {
    Idle,
    Sparse,
    Full,
    Scroll,
}

#[derive(Parser)]
pub struct Args {
    #[arg(long, value_enum, default_value = "idle")]
    scene: Scene,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u16).range(2..=1000))]
    cols: u16,
    #[arg(long, default_value_t = 40, value_parser = clap::value_parser!(u16).range(2..=1000))]
    rows: u16,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=3600))]
    seconds: u32,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=240))]
    hz: u32,
    /// Preserve intentional cursor blinking instead of hiding the cursor.
    #[arg(long)]
    cursor: bool,
}

fn frame(scene: Scene, cols: u16, rows: u16, alternate: bool) -> String {
    let glyph = if alternate { "B" } else { "A" };
    let style = if alternate {
        "\x1b[38;2;80;180;240m"
    } else {
        "\x1b[38;2;240;180;80m"
    };
    match scene {
        Scene::Idle => String::new(),
        Scene::Sparse => format!("\x1b[2;2H{style}{glyph}\x1b[0m"),
        Scene::Scroll => format!("{style}{}\x1b[0m\r\n", glyph.repeat(usize::from(cols - 1))),
        Scene::Full => {
            let line = glyph.repeat(usize::from(cols - 1));
            let mut result = style.to_owned();
            for row in 1..=rows {
                result.push_str(&format!("\x1b[{row};1H{line}"));
            }
            result.push_str("\x1b[0m");
            result
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    // Construct both alternating frames before emission begins.
    let frames = [
        frame(args.scene, args.cols, args.rows, false),
        frame(args.scene, args.cols, args.rows, true),
    ];
    let mut out = io::stdout().lock();
    out.write_all(b"\x1b[2J\x1b[Hwindow-workload\r\n")?;
    if !args.cursor {
        out.write_all(b"\x1b[?25l")?;
    }
    out.flush()?;
    let start = Instant::now();
    let ticks = if matches!(args.scene, Scene::Idle) {
        0
    } else {
        u64::from(args.seconds) * u64::from(args.hz)
    };
    for tick in 0..ticks {
        let deadline = start + Duration::from_secs_f64(tick as f64 / f64::from(args.hz));
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
        out.write_all(frames[(tick % 2) as usize].as_bytes())?;
        out.flush()?;
    }
    thread::sleep(
        (start + Duration::from_secs(u64::from(args.seconds)))
            .saturating_duration_since(Instant::now()),
    );
    out.write_all(b"\x1b[0m\x1b[?25h")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_frames_replace_every_row_without_scrolling() {
        let mut parser = ratty_vt::Parser::new(4, 8, 10);
        parser.process(frame(Scene::Full, 8, 4, false).as_bytes());
        parser.process(frame(Scene::Full, 8, 4, true).as_bytes());
        assert_eq!(
            parser.screen().contents(),
            "BBBBBBB\nBBBBBBB\nBBBBBBB\nBBBBBBB"
        );
        assert_eq!(parser.screen().cursor_position(), (3, 7));
    }

    #[test]
    fn sparse_frame_preserves_surrounding_text() {
        let mut parser = ratty_vt::Parser::new(4, 8, 10);
        parser.process(frame(Scene::Full, 8, 4, false).as_bytes());
        parser.process(frame(Scene::Sparse, 8, 4, true).as_bytes());
        assert_eq!(
            parser.screen().contents(),
            "AAAAAAA\nABAAAAA\nAAAAAAA\nAAAAAAA"
        );
    }
}

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use anyhow::{Result, ensure};
use clap::Parser;

#[derive(Parser)]
pub struct Args {
    #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u16).range(24..=1000))]
    rows: u16,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=1000))]
    cycles: u32,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(10..=1000))]
    interval_ms: u64,
}

fn image_frames() -> [String; 2] {
    ["/wAA", "AP8A"].map(|pixel| {
        let payload = pixel.repeat(1024);
        let mut frame = String::from("\x1b[3;2H");
        for chunk in 0..16 {
            let header = if chunk == 0 {
                "a=T,f=24,s=128,v=128,i=710,c=8,r=4,q=2,m=1"
            } else if chunk == 15 {
                "m=0"
            } else {
                "m=1"
            };
            frame.push_str(&format!("\x1b_G{header};{payload}\x1b\\"));
        }
        frame
    })
}

fn safe_model_path(model: &str) -> bool {
    !model.contains([';', '\x1b', '\n', '\r']) && !model.as_bytes().contains(&0x9c)
}

fn update_frames() -> Vec<String> {
    (0..12)
        .map(|step| {
            format!(
                "\x1b_ratty;g;u;id=711;animate=1;ry={}\x1b\\\x1b[1S",
                step * 15
            )
        })
        .collect()
}

pub fn run(args: Args) -> Result<()> {
    let model = crate::root()
        .join("widget/assets/black.obj")
        .canonicalize()?;
    let model = model
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("model path is not UTF-8"))?;
    ensure!(
        safe_model_path(model),
        "model path contains protocol delimiters"
    );
    // Each RGB pixel is exactly one base64 group: no padding between pixels.
    let images = image_frames();
    // RGP register/place/update/delete fields match the widget's wire format.
    let register = format!("\x1b_ratty;g;r;id=711;fmt=obj;path={model};normalize=1\x1b\\");
    let place = format!(
        "\x1b_ratty;g;p;id=711;row={};col=24;w=8;h=4;animate=1;scale=1;depth=1;color=ffffff;brightness=1\x1b\\",
        args.rows - 6
    );
    let updates = update_frames();
    let prefill: String = (1..=args.rows)
        .map(|row| format!("\x1b[{row};1Hrow {row:04}\x1b[K"))
        .collect();
    let delete = b"\x1b_Ga=d,d=I,i=710,q=2;\x1b\\\x1b_ratty;g;d;id=711\x1b\\";
    let mut out = io::stdout().lock();
    out.write_all(b"\x1b[2J\x1b[H\x1b[?25lgraphics lifecycle\r\n")?;
    out.flush()?;
    thread::sleep(Duration::from_secs(2));
    for _ in 0..args.cycles {
        out.write_all(prefill.as_bytes())?;
        out.write_all(register.as_bytes())?;
        out.write_all(place.as_bytes())?;
        for (step, update) in updates.iter().enumerate() {
            out.write_all(images[step % 2].as_bytes())?;
            out.write_all(update.as_bytes())?;
            out.flush()?;
            thread::sleep(Duration::from_millis(args.interval_ms));
        }
        out.write_all(delete)?;
        out.flush()?;
        thread::sleep(Duration::from_millis(250));
    }
    out.write_all(b"\x1b[Hgraphics cleanup complete\x1b[K")?;
    out.flush()?;
    thread::sleep(Duration::from_secs(5));
    out.write_all(b"\x1b[?25h")?;
    out.flush()?;
    Ok(())
}

#[cfg(all(test, feature = "app"))]
mod tests {
    use super::*;

    #[test]
    fn images_decode_to_alternating_full_size_pixels() {
        let mut parser = ratty::kitty::KittyParserState::default();
        for (frame, pixel) in image_frames()
            .iter()
            .zip([[255, 0, 0, 255], [0, 255, 0, 255]])
        {
            let sequence = frame.strip_prefix("\x1b[3;2H").expect("cursor position");
            let parts: Vec<_> = sequence.split_terminator("\x1b\\").collect();
            assert_eq!(parts.len(), 16);
            let mut decoded = None;
            for (index, part) in parts.iter().enumerate() {
                assert_eq!(part.split_once(';').expect("payload").1.len(), 4096);
                decoded = parser.consume_sequence(format!("{part}\x1b\\").as_bytes(), (2, 1));
                if index < 15 {
                    assert!(matches!(
                        decoded,
                        Some(ratty::kitty::KittyOperation::Pending)
                    ));
                }
            }
            let Some(ratty::kitty::KittyOperation::TransmitAndPlace {
                object_id,
                image,
                anchor,
            }) = decoded
            else {
                panic!("image did not decode")
            };
            assert_eq!(object_id, 710);
            assert_eq!((image.width, image.height), (128, 128));
            assert_eq!(image.rgba.len(), 128 * 128 * 4);
            assert!(
                image
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|actual| actual == &pixel)
            );
            assert_eq!(
                (anchor.row, anchor.col, anchor.columns, anchor.rows),
                (2, 1, 8, 4)
            );
        }
    }

    #[test]
    fn rejects_path_bytes_that_terminate_or_split_the_protocol() {
        for path in [
            "/tmp/Ü/model.obj",
            "/tmp/a;b.obj",
            "/tmp/a\x1bb.obj",
            "/tmp/a\nb.obj",
        ] {
            assert!(!safe_model_path(path));
        }
        assert!(safe_model_path("/tmp/a directory/black.obj"));
    }

    #[test]
    fn all_rotations_decode_and_text_scrolls_at_both_grid_sizes() {
        for rows in [24, 80] {
            let mut parser = ratty_vt::Parser::new(rows, 80, 100);
            parser.process(b"top marker");
            for (step, frame) in update_frames().iter().enumerate() {
                let end = frame.find("\x1b\\").expect("APC terminator") + 2;
                let Some(ratty::rgp::RgpOperation::Update { object_id, update }) =
                    ratty::rgp::consume_sequence(&frame.as_bytes()[..end])
                else {
                    panic!("update did not decode")
                };
                assert_eq!(object_id, 711);
                assert_eq!(update.rotation[1], Some((step * 15) as f32));
                parser.process(&frame.as_bytes()[end..]);
            }
            assert!(!parser.screen().contents().contains("top marker"));
            assert_eq!(parser.screen().cursor_position(), (0, 10));
            parser.screen_mut().set_scrollback(100);
            assert_eq!(parser.screen().scrollback(), 12);
            assert!(parser.screen().contents().starts_with("top marker"));
        }
    }
}

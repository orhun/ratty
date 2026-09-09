use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, ensure};
use clap::Parser;
use ratty_vt::Screen;
use serde::Serialize;

#[derive(Parser)]
pub struct Args {
    /// JSON report containing every sample, workload size, and final-state hash.
    #[arg(long)]
    output: PathBuf,
    /// Optional substring filter for a workload name (also useful when profiling).
    #[arg(long, default_value = "")]
    filter: String,
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u32).range(1..))]
    samples: u32,
    /// Corpus repetitions per processing sample; reflow pairs per resize sample.
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..))]
    rounds: u32,
}

#[derive(Serialize)]
struct Measurement {
    workload: String,
    cols: u16,
    rows: u16,
    scrollback_capacity: usize,
    chunk_bytes: usize,
    rounds: u32,
    bytes_per_sample: usize,
    operations_per_sample: u32,
    final_state_hash: String,
    seconds: Vec<f64>,
    median_seconds: f64,
    min_seconds: f64,
    max_seconds: f64,
}

pub fn run(args: Args) -> Result<()> {
    let mut results = Vec::new();
    let mut validation_failures = Vec::new();
    for (cols, rows) in [(80, 24), (120, 40), (240, 80)] {
        for kind in [
            "ascii",
            "unicode",
            "styles",
            "cursor",
            "alternate",
            "scroll-region",
        ] {
            for chunk in [1, 7, 64, 4096, 16384] {
                let name = format!("{kind}/{cols}x{rows}/chunk-{chunk}");
                if !name.contains(&args.filter) {
                    continue;
                }
                let corpus = corpus(kind, cols, rows);
                let process = |parser: &mut ratty_vt::Parser, chunk| {
                    for _ in 0..args.rounds {
                        for bytes in corpus.chunks(chunk) {
                            parser.process(black_box(bytes));
                        }
                    }
                };
                let mut reference = ratty_vt::Parser::new(rows, cols, 2000);
                process(&mut reference, corpus.len());
                let expected = fingerprint(reference.screen_mut());
                let mut seconds = Vec::new();
                // The first run warms code/data paths and is deliberately omitted.
                for sample in 0..=args.samples {
                    let mut parser = ratty_vt::Parser::new(rows, cols, 2000);
                    let start = Instant::now();
                    process(&mut parser, chunk);
                    let elapsed = start.elapsed().as_secs_f64();
                    let actual = fingerprint(parser.screen_mut());
                    if actual != expected {
                        validation_failures.push(format!("{name}: chunk-dependent terminal state, expected {expected:016x}, actual {actual:016x}; timings excluded"));
                        seconds.clear();
                        break;
                    }
                    if sample > 0 {
                        seconds.push(elapsed);
                    }
                }
                if seconds.is_empty() {
                    continue;
                }
                results.push(measurement(
                    name,
                    cols,
                    rows,
                    2000,
                    chunk,
                    args.rounds,
                    corpus.len() * args.rounds as usize,
                    args.rounds,
                    expected,
                    seconds,
                ));
            }
        }
        for history in [2000, 20000] {
            let name = format!("reflow/{cols}x{rows}/history-{history}");
            if !name.contains(&args.filter) {
                continue;
            }
            let initial = reflow_screen(cols, rows, history);
            let reflow = |screen: &mut Screen| {
                for _ in 0..args.rounds {
                    screen.set_size_reflow(rows, cols / 2);
                    screen.set_size_reflow(rows, cols);
                }
            };
            let mut reference = initial.clone();
            reflow(&mut reference);
            let expected = fingerprint(&mut reference);
            let mut seconds = Vec::new();
            for sample in 0..=args.samples {
                let mut screen = initial.clone();
                let start = Instant::now();
                reflow(black_box(&mut screen));
                let elapsed = start.elapsed().as_secs_f64();
                ensure!(
                    fingerprint(&mut screen) == expected,
                    "inconsistent reflow state"
                );
                if sample > 0 {
                    seconds.push(elapsed);
                }
            }
            results.push(measurement(
                name,
                cols,
                rows,
                history,
                0,
                args.rounds,
                0,
                args.rounds * 2,
                expected,
                seconds,
            ));
        }
    }
    ensure!(
        !results.is_empty() || !validation_failures.is_empty(),
        "filter matched no workloads"
    );
    let report = serde_json::json!({
        "schema": 1,
        "kind": "vt-cpu",
        "profile": if cfg!(debug_assertions) { "debug-assertions-enabled" } else { "optimized-no-debug-assertions" },
        "note": "Generation, parser construction, reflow prefill/cloning, and state validation excluded; one warmup discarded. Record commit/toolchain/hardware alongside this report.",
        "measurements": results,
        "validation_failures": validation_failures,
    });
    fs::write(&args.output, serde_json::to_string_pretty(&report)? + "\n")?;
    ensure!(
        validation_failures.is_empty(),
        "{}",
        validation_failures.join("\n")
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn measurement(
    workload: String,
    cols: u16,
    rows: u16,
    scrollback_capacity: usize,
    chunk_bytes: usize,
    rounds: u32,
    bytes_per_sample: usize,
    operations_per_sample: u32,
    hash: u64,
    seconds: Vec<f64>,
) -> Measurement {
    let mut sorted = seconds.clone();
    sorted.sort_by(f64::total_cmp);
    let median_seconds = if sorted.len().is_multiple_of(2) {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };
    eprintln!(
        "{workload}: {median_seconds:.6}s median ({bytes_per_sample} bytes, {operations_per_sample} operations)"
    );
    Measurement {
        workload,
        cols,
        rows,
        scrollback_capacity,
        chunk_bytes,
        rounds,
        bytes_per_sample,
        operations_per_sample,
        final_state_hash: format!("{hash:016x}"),
        median_seconds,
        min_seconds: sorted[0],
        max_seconds: sorted[sorted.len() - 1],
        seconds,
    }
}

pub(crate) fn corpus(kind: &str, cols: u16, rows: u16) -> Vec<u8> {
    let mut text = String::new();
    for i in 0..512 {
        match kind {
            "ascii" => writeln!(text, "{i:06} The quick brown fox jumps over the lazy dog. 0123456789\r").unwrap(),
            "unicode" => writeln!(text, "{i:06} 你好 世界 café e\u{301} 👩\u{200d}💻 🇺🇳 क्षि\r").unwrap(),
            "styles" => writeln!(text, "\x1b[1;38;2;{r};{g};{b}m{i:06} styled output\x1b[0m \x1b[4;48;5;123mbackground\x1b[0m\r", r=i%256, g=(i*3)%256, b=(i*7)%256).unwrap(),
            "cursor" => write!(text, "\x1b[{};{}H\x1b[2K{i:06}\x1b[3Dabc\x1b[2@XY\x1b[P", i%usize::from(rows)+1, i%usize::from(cols/2)+1).unwrap(),
            "alternate" => write!(text, "\x1b[?1049h\x1b[H\x1b[2Jframe {i:06}\r\n┌───┐\r\n│界 │\r\n└───┘\x1b[?1049l").unwrap(),
            "scroll-region" => write!(text, "\x1b[2;{}r\x1b[{};1H{i:06}\r\n\x1b[1S\x1b[1T\x1b[r", rows-1, rows-1).unwrap(),
            _ => unreachable!("known corpus"),
        }
    }
    // Leave a populated alternate screen visible so validation detects lost
    // alternate-screen draws, rather than hashing only the restored main grid.
    if kind == "alternate" {
        text.push_str("\x1b[?1049h\x1b[Hfinal alternate frame\r\n┌───┐\r\n│界 │\r\n└───┘");
    }
    text.into_bytes()
}

fn reflow_screen(cols: u16, rows: u16, history: usize) -> Screen {
    let mut parser = ratty_vt::Parser::new(rows, cols, history);
    // Mixed hard lines: these fit the initial width but wrap when narrowed.
    // Include wide/combining text and enough lines to exercise history eviction.
    let long = "界e\u{301}x".repeat(usize::from(cols) / 6);
    for i in 0..history + usize::from(rows) + 2 {
        let line = if i % 2 == 0 {
            format!("{i:06} {long}\r\n")
        } else {
            format!("{i:06} short\r\n")
        };
        parser.process(line.as_bytes());
    }
    parser.screen().clone()
}

pub(crate) fn fingerprint(screen: &mut Screen) -> u64 {
    // Stable FNV-1a over visible state, input modes, and retained history rows.
    // Validation stays outside the timed region, including serialization.
    let mut hash = 0xcbf29ce484222325_u64;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
    };
    feed(&screen.state_formatted());
    feed(format!("{:?}", screen.cursor_position()).as_bytes());
    feed(
        format!(
            "{:?}/{:?}",
            screen.kitty_keyboard_flags(),
            screen.modify_other_keys()
        )
        .as_bytes(),
    );
    screen.set_scrollback(usize::MAX);
    let max = screen.scrollback();
    let (rows, cols) = screen.size();
    let mut offset = max;
    while offset > 0 {
        screen.set_scrollback(offset);
        for row in screen
            .rows_formatted(0, cols)
            .take(usize::from(rows).min(offset))
        {
            feed(&row);
        }
        offset = offset.saturating_sub(usize::from(rows));
    }
    screen.set_scrollback(0);
    feed(&max.to_le_bytes());
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_corpora_preserve_screen_and_history() {
        for kind in [
            "ascii",
            "unicode",
            "styles",
            "cursor",
            "alternate",
            "scroll-region",
        ] {
            let input = corpus(kind, 80, 24);
            let mut whole = ratty_vt::Parser::new(24, 80, 100);
            whole.process(&input);
            let expected = fingerprint(whole.screen_mut());
            for chunk in [1, 7, 64, 4096] {
                let mut split = ratty_vt::Parser::new(24, 80, 100);
                for bytes in input.chunks(chunk) {
                    split.process(bytes);
                }
                assert_eq!(
                    fingerprint(split.screen_mut()),
                    expected,
                    "{kind}, chunk={chunk}: whole first row {:?}; split first row {:?}",
                    whole.screen().rows(0, 80).next(),
                    split.screen().rows(0, 80).next()
                );
            }
        }
    }

    #[test]
    fn fingerprint_includes_scrollback() {
        let mut a = ratty_vt::Parser::new(2, 10, 10);
        let mut b = ratty_vt::Parser::new(2, 10, 10);
        a.process(b"old\r\nsame\r\nnow");
        b.process(b"new\r\nsame\r\nnow");
        assert_eq!(a.screen().contents(), b.screen().contents());
        assert_ne!(fingerprint(a.screen_mut()), fingerprint(b.screen_mut()));
    }

    #[test]
    fn alternate_corpus_leaves_drawn_content_for_validation() {
        let mut parser = ratty_vt::Parser::new(24, 80, 100);
        parser.process(&corpus("alternate", 80, 24));
        assert!(parser.screen().alternate_screen());
        assert!(parser.screen().contents().contains("final alternate frame"));
        assert!(parser.screen().contents().contains("界"));
    }

    #[test]
    fn reflow_workloads_split_and_rejoin_wrapped_lines() {
        for (cols, rows) in [(80, 24), (120, 40), (240, 80)] {
            let mut screen = reflow_screen(cols, rows, 100);
            assert!(!screen.visible_rows().any(|row| row.wrapped()));
            screen.set_size_reflow(rows, cols / 2);
            assert!(screen.visible_rows().any(|row| row.wrapped()));
            screen.set_size_reflow(rows, cols);
            assert!(!screen.visible_rows().any(|row| row.wrapped()));
        }
    }
}

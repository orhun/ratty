use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use bevy_math::UVec2;
use bevy_terminal_ratatui::prelude::{StyleFlags, TerminalColor};
use clap::Parser;
use ratty::config::{AppConfig, FontStyleConfig};
use ratty::inline::TerminalInlineObjects;
use ratty::mouse::TerminalSelection;
use ratty::runtime::{RuntimeOptions, TerminalRuntime};
use ratty::systems::drain_pty_output;
use ratty::terminal::{TerminalSurface, TerminalWidget};

#[derive(Parser)]
pub struct Args {
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "")]
    filter: String,
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u32).range(1..))]
    samples: u32,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..))]
    frames: u32,
    #[arg(long, default_value_t = 32768, value_parser = clap::value_parser!(u32).range(1..))]
    lines: u32,
}

pub fn run(args: Args) -> Result<()> {
    let parent = args.output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut results = Vec::new();
    for (cols, rows) in [(80, 24), (120, 40), (240, 80)] {
        let mut config = AppConfig::default();
        config.terminal.default_cols = cols;
        config.terminal.default_rows = rows;
        config.cursor.model.visible = false;
        for kind in [
            "forced-redraw-unchanged",
            "sparse",
            "full",
            "scroll",
            "selection",
            "resize",
        ] {
            let name = format!("draw/{kind}/{cols}x{rows}");
            if !name.contains(&args.filter) {
                continue;
            }
            let mut samples = Vec::new();
            for sample in 0..=args.samples {
                let mut terminal = TerminalSurface::new(&config)?;
                let mut parser = ratty_vt::Parser::new(rows, cols, 2000);
                parser.process(&crate::vt_bench::corpus("unicode", cols, rows));
                let mut selection = TerminalSelection::default();
                // Populate the retained buffer before measuring steady-state updates.
                terminal.tui.draw(|frame| {
                    frame.render_widget(
                        TerminalWidget {
                            screen: parser.screen(),
                            selection: &selection,
                            theme: &config.theme,
                            font_style: config.font.style,
                        },
                        frame.area(),
                    )
                });
                let updates: Vec<_> = (0..args.frames)
                    .map(|frame| match kind {
                        "sparse" => format!("\x1b[{};1H{frame:06}", frame % u32::from(rows) + 1)
                            .into_bytes(),
                        "full" => {
                            let style = if frame % 2 == 0 {
                                "\x1b[0;1;38;2;240;80;30;48;2;10;20;30m"
                            } else {
                                "\x1b[0;3;4;38;2;30;180;240;48;2;30;20;10m"
                            };
                            let mut text = format!("{style}\x1b[Hframe {frame:06}");
                            for row in 1..rows {
                                text.push_str(&format!(
                                    "\x1b[{};1H{}",
                                    row + 1,
                                    (if frame % 2 == 0 { "界x" } else { "語y" })
                                        .repeat(usize::from(cols) / 3)
                                ));
                            }
                            text.into_bytes()
                        }
                        "scroll" => format!("\x1b[{};1Hline {frame:06}\r\n", rows).into_bytes(),
                        _ => Vec::new(),
                    })
                    .collect();
                let mut seconds = Vec::new();
                for (frame, bytes) in updates.iter().enumerate() {
                    let start = Instant::now();
                    parser.process(bytes);
                    if kind == "selection" {
                        selection.clear();
                        selection.begin(UVec2::ZERO);
                        selection.update(UVec2::new(
                            (frame % usize::from(cols)) as u32,
                            u32::from(rows / 2),
                        ));
                    }
                    if kind == "resize" {
                        let width = if frame % 2 == 0 { cols / 2 } else { cols };
                        parser.screen_mut().set_size_reflow(rows, width);
                        terminal.resize(width, rows);
                    }
                    terminal.tui.draw(|frame| {
                        frame.render_widget(
                            TerminalWidget {
                                screen: parser.screen(),
                                selection: &selection,
                                theme: &config.theme,
                                font_style: config.font.style,
                            },
                            frame.area(),
                        )
                    });
                    seconds.push(start.elapsed().as_secs_f64());
                    if sample == 0 {
                        validate_surface(&terminal, parser.screen(), &selection, &config)?;
                    }
                }
                validate_surface(&terminal, parser.screen(), &selection, &config)?;
                if sample > 0 {
                    samples.push(seconds);
                }
            }
            eprintln!("{name}: {} validated samples", samples.len());
            results.push(serde_json::json!({"workload":name,"metric":"parse-and-draw-cpu-seconds-per-update","cols":cols,"rows":rows,"seconds":samples,"frames_per_sample":args.frames}));
        }
        let name = format!("pty/bulk/{cols}x{rows}");
        if !name.contains(&args.filter) {
            continue;
        }
        let data = bulk_corpus(args.lines);
        let path = parent.join(format!("pty-corpus-{cols}x{rows}.bin"));
        fs::write(&path, &data)?;
        let path = path.canonicalize()?;
        let mut expected = ratty_vt::Parser::new(rows, cols, config.terminal.scrollback);
        expected.process(b"READY");
        expected.process(&data);
        let expected = crate::vt_bench::fingerprint(expected.screen_mut());
        let mut samples = Vec::new();
        for sample in 0..=args.samples {
            let startup = Instant::now();
            let mut runtime = TerminalRuntime::spawn(
                &config,
                &RuntimeOptions {
                    command: Some(vec![
                        std::env::current_exe()?.to_string_lossy().into_owned(),
                        "replay".into(),
                        path.to_string_lossy().into_owned(),
                    ]),
                    working_dir: Some(crate::root()),
                },
            )?;
            let mut inline = TerminalInlineObjects::default();
            let mut camera = Vec::new();
            loop {
                let drained = drain_pty_output(&mut runtime, &mut inline, &mut camera);
                if runtime.screen().contents() == "READY" {
                    break;
                }
                ensure!(
                    !drained.disconnected && startup.elapsed() < Duration::from_secs(30),
                    "PTY readiness failed"
                );
                thread::sleep(Duration::from_micros(50));
            }
            let startup_seconds = startup.elapsed().as_secs_f64();
            let initial_bytes = runtime.processed_bytes();
            if sample == 0 {
                runtime.start_processed_digest();
            }
            let start = Instant::now();
            runtime.write_input(b"G");
            let mut drains = Vec::new();
            loop {
                let call = Instant::now();
                let drained = drain_pty_output(&mut runtime, &mut inline, &mut camera);
                drains.push(call.elapsed().as_secs_f64());
                if drained.disconnected {
                    break;
                }
                ensure!(
                    start.elapsed() < Duration::from_secs(120),
                    "PTY delivery timed out"
                );
                if !drained.processed {
                    thread::sleep(Duration::from_micros(50));
                }
            }
            let seconds = start.elapsed().as_secs_f64();
            let exit_wait = Instant::now();
            loop {
                if let Some(success) = runtime.child_exit_success()? {
                    ensure!(success, "PTY replay child exited unsuccessfully");
                    break;
                }
                ensure!(
                    exit_wait.elapsed() < Duration::from_secs(5),
                    "PTY child did not exit after EOF"
                );
                thread::sleep(Duration::from_millis(1));
            }
            if sample == 0 {
                let expected_digest = data.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
                    (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
                });
                ensure!(
                    runtime.processed_digest() == Some(expected_digest),
                    "PTY full-stream digest differs from input"
                );
            }
            let consumed = runtime.processed_bytes() - initial_bytes;
            let (queued, peak_queued) = runtime.queued_bytes();
            ensure!(
                queued == 0 && peak_queued > 0 && peak_queued <= 18 * 16 * 1024,
                "PTY queue accounting out of bounds"
            );
            ensure!(
                consumed == data.len() as u64,
                "PTY byte count: {consumed} != {}",
                data.len()
            );
            ensure!(
                crate::vt_bench::fingerprint(runtime.screen_mut()) == expected,
                "PTY final state differs from direct parsing"
            );
            runtime.shutdown();
            if sample > 0 {
                samples.push(serde_json::json!({"seconds":seconds,"startup_to_ready_seconds":startup_seconds,"drain_seconds":drains,"consumed_bytes":consumed,"peak_outstanding_pty_bytes_upper_bound":peak_queued}));
            }
        }
        results.push(serde_json::json!({"workload":name,"metric":"ready-handshake-through-pty-eof-seconds","bytes_per_sample":data.len(),"final_state_hash":format!("{expected:016x}"),"samples":samples}));
    }
    ensure!(!results.is_empty(), "filter matched no workloads");
    fs::write(
        args.output,
        serde_json::to_string_pretty(
            &serde_json::json!({"schema":1,"kind":"application-cpu","note":"No GPU or window. Drawing timings include VT processing and CPU retained-buffer drawing; unchanged case forces redraw. PTY payload generated/loaded before readiness handshake; raw mode prevents echo/newline transformation. One warmup sample discarded.","measurements":results}),
        )? + "\n",
    )?;
    Ok(())
}

fn validate_surface(
    terminal: &TerminalSurface,
    screen: &ratty_vt::Screen,
    selection: &TerminalSelection,
    config: &AppConfig,
) -> Result<()> {
    let snapshot = terminal.tui.snapshot();
    let (rows, cols) = screen.size();
    ensure!(
        (snapshot.size().width, snapshot.size().height) == (cols, rows),
        "surface dimensions differ from VT grid"
    );
    for row in 0..rows {
        for col in 0..cols {
            let cell = screen.cell(row, col).expect("in-bounds cell");
            let expected = if cell.is_wide_continuation() || !cell.has_contents() {
                " "
            } else {
                cell.contents()
            };
            let rendered = snapshot.cell((col, row)).expect("in-bounds surface");
            ensure!(
                rendered.is_continuation() == cell.is_wide_continuation(),
                "incorrect continuation at {row},{col}"
            );
            if !cell.is_wide_continuation() {
                ensure!(
                    rendered.occupancy().columns() == if cell.is_wide() { 2 } else { 1 },
                    "incorrect width at {row},{col}"
                );
            }
            let owner_col = if cell.is_wide_continuation() {
                col - 1
            } else {
                col
            };
            let owner = screen.cell(row, owner_col).expect("wide owner");
            let selected = selection.normalized_bounds().is_some_and(|bounds| {
                bounds.contains(row, owner_col)
                    || (owner.is_wide() && bounds.contains(row, owner_col + 1))
            });
            let bold = matches!(
                config.font.style,
                FontStyleConfig::Bold | FontStyleConfig::BoldItalic
            );
            let italic = matches!(
                config.font.style,
                FontStyleConfig::Italic | FontStyleConfig::BoldItalic
            );
            for (flag, expected) in [
                (StyleFlags::BOLD, owner.bold() || bold),
                (StyleFlags::DIM, owner.dim()),
                (StyleFlags::ITALIC, owner.italic() || italic),
                (StyleFlags::UNDERLINED, owner.underline()),
                (StyleFlags::REVERSED, owner.inverse() || selected),
                (StyleFlags::HIDDEN, owner.hidden()),
                (StyleFlags::CROSSED_OUT, owner.strikeout()),
                (
                    StyleFlags::SLOW_BLINK,
                    owner.blink() == ratty_vt::Blink::Slow,
                ),
                (
                    StyleFlags::RAPID_BLINK,
                    owner.blink() == ratty_vt::Blink::Rapid,
                ),
            ] {
                ensure!(
                    rendered.style.has(flag) == expected,
                    "incorrect style {flag:?} at {row},{col}"
                );
            }
            let [r, g, b] = config.theme.foreground;
            for (actual, source, default) in [
                (
                    // Untouched blank cells may retain the renderer's default
                    // foreground, which resolves to this same configured theme.
                    match rendered.style.foreground {
                        TerminalColor::Default => TerminalColor::Rgb(r, g, b),
                        color => color,
                    },
                    owner.fgcolor(),
                    TerminalColor::Rgb(r, g, b),
                ),
                (
                    rendered.style.background,
                    owner.bgcolor(),
                    TerminalColor::Default,
                ),
                (
                    rendered.style.underline,
                    owner.underline_color(),
                    TerminalColor::Default,
                ),
            ] {
                let expected = match source {
                    ratty_vt::Color::Default => default,
                    ratty_vt::Color::Rgb(r, g, b) => TerminalColor::Rgb(r, g, b),
                    ratty_vt::Color::Idx(_) => {
                        anyhow::bail!("benchmark validator requires default or RGB corpus colors")
                    }
                };
                ensure!(
                    actual == expected,
                    "incorrect color at {row},{col}: {actual:?} != {expected:?}"
                );
            }
            let actual = if rendered.is_continuation() {
                " "
            } else {
                rendered.symbol()
            };
            ensure!(
                actual == expected,
                "stale surface at row {row}, col {col}: {actual:?} != {expected:?}"
            );
        }
    }
    Ok(())
}

fn bulk_corpus(lines: u32) -> Vec<u8> {
    let mut bytes = b"\x1b[2J\x1b[H".to_vec();
    for line in 0..lines {
        bytes.extend_from_slice(
            format!("{line:08} The quick brown fox jumps over the lazy dog.\r\n").as_bytes(),
        );
    }
    bytes.extend_from_slice(b"RATTY_BENCH_DONE");
    bytes
}

pub fn replay(path: &Path) -> Result<()> {
    let data = fs::read(path)?;
    crossterm::terminal::enable_raw_mode()?;
    let result = (|| -> Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(b"READY")?;
        out.flush()?;
        let mut signal = [0];
        io::stdin().read_exact(&mut signal)?;
        ensure!(signal == *b"G", "invalid replay handshake");
        out.write_all(&data)?;
        out.flush()?;
        Ok(())
    })();
    let restore = crossterm::terminal::disable_raw_mode();
    result?;
    restore?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_payload_scrolls_and_evicts_old_history() {
        let mut parser = ratty_vt::Parser::new(4, 80, 3);
        parser.process(&bulk_corpus(20));
        assert!(parser.screen().contents().starts_with("00000017 "));
        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(parser.screen().scrollback(), 3);
        assert!(parser.screen().contents().starts_with("00000014 "));
    }

    #[test]
    fn surface_validation_rejects_an_oversized_blank_surface() {
        let config = AppConfig::default();
        let terminal = TerminalSurface::new(&config).unwrap();
        let parser = ratty_vt::Parser::new(
            config.terminal.default_rows,
            config.terminal.default_cols / 2,
            0,
        );
        let error = validate_surface(
            &terminal,
            parser.screen(),
            &TerminalSelection::default(),
            &config,
        )
        .unwrap_err();
        assert!(error.to_string().contains("dimensions"));
    }

    #[test]
    fn surface_validation_rejects_stale_styles_selection_and_wide_cells() {
        let config = AppConfig::default();
        let mut terminal = TerminalSurface::new(&config).unwrap();
        let mut parser = ratty_vt::Parser::new(
            config.terminal.default_rows,
            config.terminal.default_cols,
            0,
        );
        parser.process("界x".as_bytes());
        let mut selection = TerminalSelection::default();
        terminal.tui.draw(|frame| {
            frame.render_widget(
                TerminalWidget {
                    screen: parser.screen(),
                    selection: &selection,
                    theme: &config.theme,
                    font_style: config.font.style,
                },
                frame.area(),
            )
        });
        validate_surface(&terminal, parser.screen(), &selection, &config).unwrap();

        selection.begin(UVec2::ZERO);
        selection.update(UVec2::new(2, 0));
        assert!(validate_surface(&terminal, parser.screen(), &selection, &config).is_err());
        selection.clear();
        parser.process("\x1b[H\x1b[1;38;2;10;20;30m界x".as_bytes());
        assert!(validate_surface(&terminal, parser.screen(), &selection, &config).is_err());
        parser.process(b"\x1b[0m\x1b[Habx");
        assert!(validate_surface(&terminal, parser.screen(), &selection, &config).is_err());
    }
}

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::LazyLock;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use regex::Regex;
use serde::Serialize;

#[derive(Parser)]
pub struct Args {
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    snapshot: Option<PathBuf>,
    #[arg(long)]
    document: Option<PathBuf>,
    #[arg(long)]
    render_test: Option<PathBuf>,
    #[arg(long, default_value = "/usr/share/fonts/truetype/dejavu")]
    font_dir: PathBuf,
}

#[derive(Serialize, Debug, PartialEq)]
struct Capture {
    width: u32,
    height: u32,
    stale_cells: u32,
}

struct Case {
    name: &'static str,
    config: PathBuf,
    command: Vec<String>,
    scene: bool,
    sends: Vec<String>,
    after: u32,
}

pub fn run(args: Args) -> Result<()> {
    fs::create_dir_all(&args.output)?;
    let output = args.output.canonicalize()?;
    let examples = crate::root().join("target/debug/examples");
    let binary = |provided: Option<PathBuf>, name: &str| -> Result<PathBuf> {
        let path = provided
            .unwrap_or_else(|| examples.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)));
        ensure!(
            is_executable(&path),
            "build the example first: {}",
            path.display()
        );
        Ok(path.canonicalize()?)
    };
    let snapshot = binary(args.snapshot, "headless_snapshot")?;
    let document = binary(args.document, "document")?;
    let render_test = binary(args.render_test, "render_test")?;
    let font_dir = args.font_dir.canonicalize().context("font directory")?;
    let normal = output.join("normal.toml");
    let compact = output.join("compact.toml");
    let fallback = output.join("fallback.toml");
    write_config(&normal, &font_dir, 1.0, true)?;
    write_config(&compact, &font_dir, 0.85, true)?;
    write_config(&fallback, &font_dir, 1.0, false)?;
    let emitter = std::env::current_exe()?.to_string_lossy().into_owned();
    let text_command = vec![emitter.clone(), "emit".into(), "text".into()];
    let case = |name, config, command, scene, sends, after| Case {
        name,
        config,
        command,
        scene,
        sends,
        after,
    };
    let cases = [
        case(
            "font-natural",
            normal.clone(),
            text_command.clone(),
            false,
            vec![],
            3,
        ),
        case(
            "font-compact",
            compact,
            text_command.clone(),
            false,
            vec![],
            3,
        ),
        case("missing-family", fallback, text_command, false, vec![], 3),
        case(
            "scene-child-exit",
            normal.clone(),
            vec![emitter, "emit".into(), "eof".into()],
            true,
            vec![],
            3,
        ),
        case(
            "document-inline",
            normal.clone(),
            vec![document.to_string_lossy().into_owned()],
            true,
            vec![],
            6,
        ),
        case(
            "styles-end",
            normal.clone(),
            vec![render_test.to_string_lossy().into_owned()],
            false,
            vec![r"\x1b[F".into()],
            5,
        ),
        case(
            "scroll-30-rows",
            normal,
            vec![render_test.to_string_lossy().into_owned()],
            false,
            vec!["j".into(); 30],
            12,
        ),
    ];
    let mut captures = BTreeMap::new();
    let mut failures = Vec::new();
    for case in cases {
        println!("Capturing {}...", case.name);
        match capture(&snapshot, &output, &case) {
            Ok(result) => {
                println!("PASS {}: {result:?}", case.name);
                captures.insert(case.name, result);
            }
            Err(error) => {
                let message = format!("{}: {error:#}", case.name);
                eprintln!("FAIL {message}");
                failures.push(message);
            }
        }
    }
    if let (Some(full), Some(short)) = (captures.get("font-natural"), captures.get("font-compact"))
        && (full.width != short.width || short.height >= full.height)
    {
        failures.push(format!(
            "line_height=0.85 must reduce only PNG height: {full:?} -> {short:?}"
        ));
    }
    let report = serde_json::json!({
        "captures": captures,
        "failures": failures,
        "visual_review": "Inspect inline images, box joins, and text styles in the PNG artifacts.",
    });
    fs::write(
        output.join("results.json"),
        serde_json::to_string_pretty(&report)? + "\n",
    )?;
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

fn write_config(path: &Path, font_dir: &Path, line_height: f32, explicit: bool) -> Result<()> {
    let family = if explicit {
        "DejaVu Sans Mono"
    } else {
        "Ratty Missing Smoke Font"
    };
    let mut config = format!(
        "[window]\nopacity = 1.0\n[font]\nfamily = {family:?}\nsize = 14\nline_height = {line_height:?}\n"
    );
    if explicit {
        for (face, suffix) in [
            ("regular", ""),
            ("bold", "-Bold"),
            ("italic", "-Oblique"),
            ("bold_italic", "-BoldOblique"),
        ] {
            let font = font_dir.join(format!("DejaVuSansMono{suffix}.ttf"));
            ensure!(
                font.is_file(),
                "required font is missing: {}",
                font.display()
            );
            config.push_str(&format!("{face} = {}\n", serde_json::to_string(&font)?));
        }
    }
    config.push_str("[cursor.model]\nvisible = false\n");
    fs::write(path, config)?;
    Ok(())
}

fn capture(snapshot: &Path, output: &Path, case: &Case) -> Result<Capture> {
    let image = output.join(format!("{}.png", case.name));
    let log_path = output.join(format!("{}.log", case.name));
    if image.exists() {
        fs::remove_file(&image)?;
    }
    let mut command = Command::new(snapshot);
    command
        .args(["--config-file"])
        .arg(&case.config)
        .arg("--out")
        .arg(&image)
        .args([
            "--after",
            &case.after.to_string(),
            "--timeout",
            "35",
            "--diagnose",
            "--width",
            if case.scene { "1100" } else { "110" },
            "--height",
            if case.scene { "760" } else { "30" },
            "--send-after",
            "1",
            "--send-interval",
            "0.25",
        ]);
    if case.scene {
        command.arg("--scene");
    }
    for send in &case.sends {
        command.args(["--send", send]);
    }
    command.arg("--").args(&case.command);
    command
        .current_dir(crate::root())
        .env("RUST_LOG", "info")
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR");
    let mut log = File::create(&log_path)?;
    writeln!(log, "{command:?}")?;
    command
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let status = run_with_timeout(&mut command, Duration::from_secs(90))
        .with_context(|| format!("see {}", log_path.display()))?;
    ensure!(
        status.success(),
        "exit status {status}; see {}",
        log_path.display()
    );
    let (width, height) = png_dimensions(&image)?;
    let log = fs::read(&log_path)?;
    validate_log(
        case.name,
        case.scene,
        case.sends.len(),
        width,
        height,
        &String::from_utf8_lossy(&log),
    )?;
    Ok(Capture {
        width,
        height,
        stale_cells: 0,
    })
}

fn run_with_timeout(command: &mut Command, timeout: Duration) -> Result<ExitStatus> {
    let mut child = command.spawn().context("start capture process")?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            result => {
                // Always reap the process, including on wait errors. The capture
                // binary's own 35s deadline normally shuts its PTY down first.
                let _ = child.kill();
                let _ = child.wait();
                result.context("wait for capture process")?;
                bail!("capture timed out after {} seconds", timeout.as_secs_f64());
            }
        }
    }
}

fn png_dimensions(path: &Path) -> Result<(u32, u32)> {
    // Decode image data as well as its header so truncated/corrupt captures fail.
    let mut reader = image::ImageReader::open(path)?;
    reader.set_format(image::ImageFormat::Png);
    let image = reader
        .decode()
        .with_context(|| format!("invalid PNG capture: {}", path.display()))?;
    ensure!(
        image.width() >= 2 && image.height() >= 2,
        "empty PNG dimensions"
    );
    Ok((image.width(), image.height()))
}

fn validate_log(
    name: &str,
    scene: bool,
    sends: usize,
    width: u32,
    height: u32,
    text: &str,
) -> Result<()> {
    static ANSI: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap());
    static STALE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"stale-cell check:\s*(\d+) mismatches").unwrap());
    static SENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"sent PTY input (\d+) of (\d+)").unwrap());
    let text = ANSI.replace_all(text, "");
    let stale: Vec<_> = STALE
        .captures_iter(&text)
        .map(|c| c[1].parse::<u64>())
        .collect();
    ensure!(
        !stale.is_empty() && stale.iter().all(|v| matches!(v, Ok(0))),
        "missing or failing stale-cell diagnostic: {stale:?}"
    );
    let delivered: Vec<_> = SENT
        .captures_iter(&text)
        .map(|c| Ok((c[1].parse::<usize>()?, c[2].parse::<usize>()?)))
        .collect::<Result<_>>()?;
    let expected: Vec<_> = (1..=sends).map(|index| (index, sends)).collect();
    ensure!(
        delivered == expected,
        "not all {sends} scheduled inputs were sent: {delivered:?}"
    );
    ensure!(
        !scene || (width, height) == (1100, 760),
        "unexpected scene dimensions: {width}x{height}"
    );
    let required = match name {
        "missing-family" => Some("using the generic monospace family"),
        "scene-child-exit" => Some("scene-eof-ok"),
        "styles-end" => Some("        font"),
        "document-inline" => Some("Ratty Editor"),
        "scroll-30-rows" => Some("        deco"),
        _ => None,
    };
    if let Some(marker) = required {
        ensure!(
            text.contains(marker),
            "{name}: missing expected marker {marker:?}"
        );
    }
    Ok(())
}

/// Mirrors the previous runner's `os.access(path, os.X_OK)` check.
fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_require_zero_stale_and_exact_input_delivery() {
        let valid = "\x1b[32mstale-cell check: 0 mismatches\x1b[0m\nsent PTY input 1 of 2\nsent PTY input 2 of 2";
        assert!(validate_log("test", false, 2, 10, 10, valid).is_ok());
        for invalid in [
            "",
            "stale-cell check: 1 mismatches",
            "stale-cell check: 0 mismatches\nsent PTY input 2 of 2",
            "stale-cell check: 0 mismatches\nsent PTY input 1 of 2\nsent PTY input 1 of 2",
        ] {
            assert!(validate_log("test", false, 2, 10, 10, invalid).is_err());
        }
    }

    #[test]
    fn scene_dimensions_and_case_markers_are_required() {
        let stale = "stale-cell check: 0 mismatches";
        assert!(validate_log("scene-child-exit", true, 0, 1100, 760, stale).is_err());
        assert!(
            validate_log(
                "scene-child-exit",
                true,
                0,
                1100,
                760,
                &format!("{stale}\nscene-eof-ok")
            )
            .is_ok()
        );
        assert!(validate_log("test", true, 0, 1100, 759, stale).is_err());
        for case in [
            "missing-family",
            "styles-end",
            "document-inline",
            "scroll-30-rows",
        ] {
            assert!(validate_log(case, false, 0, 10, 10, stale).is_err());
        }
    }

    #[test]
    fn missing_command_fails_without_waiting() {
        assert!(
            run_with_timeout(
                &mut Command::new("ratty-nonexistent-test-command"),
                Duration::ZERO
            )
            .is_err()
        );
    }

    #[test]
    fn png_validation_decodes_data_and_rejects_truncation() {
        let path = std::env::temp_dir().join(format!("ratty-xtask-png-{}.png", std::process::id()));
        let image = image::RgbaImage::new(4, 3);
        image.save(&path).unwrap();
        assert_eq!(png_dimensions(&path).unwrap(), (4, 3));
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(png_dimensions(&path).is_err());
        fs::write(&path, b"not a PNG").unwrap();
        assert!(png_dimensions(&path).is_err());
        fs::remove_file(&path).unwrap();
        assert!(png_dimensions(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_exit_and_timeout_are_observable() {
        let status = run_with_timeout(
            Command::new("sh").args(["-c", "exit 7"]),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(status.code(), Some(7));
        let start = Instant::now();
        let result = run_with_timeout(
            Command::new("sh").args(["-c", "exec sleep 10"]),
            Duration::from_millis(25),
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

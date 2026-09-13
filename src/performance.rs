//! Optional real-window measurement; enabled only by an explicit report path.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_terminal_ratatui::prelude::TerminalStats;
use ratty::inline::TerminalInlineObjects;
use ratty::runtime::TerminalRuntime;
use ratty::terminal::{TerminalRedrawState, TerminalSurface};

#[derive(Resource)]
struct Recording {
    output: PathBuf,
    duration: Duration,
    warmup: Duration,
    launched: Instant,
    ready: Option<Instant>,
    first: Instant,
    previous: Option<Instant>,
    rows: Vec<Sample>,
    finished: bool,
    written: Arc<AtomicBool>,
    tracked_object: Option<u32>,
    resize_initial: Option<(u32, u32, Option<f32>, i32)>,
    resize_phase: Option<u64>,
}

struct Sample {
    age: f64,
    interval: Option<f64>,
    marker_wall: f64,
    focused: bool,
    cols: u16,
    rows: u16,
    scale: f32,
    bytes: u64,
    since_install: f64,
    totals: [u64; 8],
    resources: [usize; 5],
    placement: Option<(u16, u16, f32)>,
    resize_phase: Option<u64>,
    physical_size: (u32, u32),
}

pub struct Completion(Arc<AtomicBool>);

#[derive(SystemParam)]
struct RecordedAssets<'w> {
    images: Res<'w, Assets<Image>>,
    meshes: Res<'w, Assets<Mesh>>,
    materials: Res<'w, Assets<StandardMaterial>>,
}

impl Completion {
    pub fn check(self) -> Result<()> {
        ensure!(
            self.0.load(Ordering::Relaxed),
            "desktop recording ended without a complete report"
        );
        Ok(())
    }
}

pub fn install(app: &mut App) -> Result<Option<Completion>> {
    let Some(output) = std::env::var_os("RATTY_PERF_REPORT") else {
        return Ok(None);
    };
    let duration = seconds("RATTY_PERF_SECONDS", 10.0, 1.0)?;
    let resize_cycle = match std::env::var_os("RATTY_PERF_RESIZE_CYCLE") {
        None => false,
        Some(value) => {
            ensure!(value == "1", "RATTY_PERF_RESIZE_CYCLE must be 1 when set");
            true
        }
    };
    let tracked_object = std::env::var_os("RATTY_PERF_OBJECT_ID")
        .map(|value| {
            value
                .to_str()
                .context("RATTY_PERF_OBJECT_ID must be UTF-8")?
                .parse::<u32>()
                .context("invalid RATTY_PERF_OBJECT_ID")
        })
        .transpose()?;
    let warmup = seconds("RATTY_PERF_WARMUP_SECONDS", 3.0, 0.0)?;
    let output = PathBuf::from(output);
    std::fs::write(&output, "# incomplete desktop recording\n")
        .with_context(|| format!("cannot initialize recording {}", output.display()))?;
    let now = Instant::now();
    let written = Arc::new(AtomicBool::new(false));
    app.insert_resource(Recording {
        output,
        duration,
        warmup,
        launched: now,
        ready: None,
        first: now,
        previous: None,
        rows: Vec::with_capacity(4096),
        finished: false,
        written: written.clone(),
        tracked_object,
        resize_initial: None,
        resize_phase: None,
    })
    .add_systems(First, begin)
    .add_systems(Last, record);
    if resize_cycle {
        app.add_systems(PreUpdate, resize_cycle_step);
    }
    Ok(Some(Completion(written)))
}

fn resize_cycle_step(
    mut recording: ResMut<Recording>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut terminal: ResMut<TerminalSurface>,
    mut redraw: ResMut<TerminalRedrawState>,
) {
    let Some(ready) = recording.ready else { return };
    if recording.finished || ready.elapsed() < recording.warmup {
        return;
    }
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let initial = *recording.resize_initial.get_or_insert_with(|| {
        (
            window.resolution.physical_width(),
            window.resolution.physical_height(),
            window.resolution.scale_factor_override(),
            terminal.font_size(),
        )
    });
    let phase = (ready.elapsed() - recording.warmup).as_secs() / 2;
    // Derive each phase from the captured baseline, even if a stalled frame
    // skipped an intervening phase. The CSV exposes such missing phases.
    let stage = phase % 7;
    let font = if stage == 1 {
        initial.3.saturating_add(2)
    } else {
        initial.3
    };
    let scale = if stage == 3 { Some(1.5) } else { initial.2 };
    let (width, height) = if stage == 5 {
        (
            initial.0.saturating_mul(5) / 4,
            initial.1.saturating_mul(5) / 4,
        )
    } else {
        (initial.0, initial.1)
    };
    if recording.resize_phase == Some(phase)
        && terminal.font_size() == font
        && window.resolution.scale_factor_override() == scale
        && window.resolution.physical_width() == width
        && window.resolution.physical_height() == height
    {
        return;
    }
    recording.resize_phase = Some(phase);
    let delta = font.saturating_sub(terminal.font_size());
    terminal.adjust_font_size(delta);
    window.resolution.set_scale_factor_override(scale);
    window.resolution.set_physical_resolution(width, height);
    redraw.request();
}

fn seconds(name: &str, default: f64, minimum: f64) -> Result<Duration> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<f64>()
            .with_context(|| format!("invalid {name}"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error.into()),
    };
    ensure!(
        value.is_finite() && (minimum..=300.0).contains(&value),
        "{name} must be between {minimum} and 300 seconds"
    );
    Ok(Duration::from_secs_f64(value))
}

fn begin(mut recording: ResMut<Recording>) {
    recording.first = Instant::now();
}

fn record(
    mut recording: ResMut<Recording>,
    windows: Query<&Window, With<PrimaryWindow>>,
    stats: Query<&TerminalStats>,
    terminal: Res<TerminalSurface>,
    runtime: (Res<TerminalRuntime>, Res<TerminalInlineObjects>),
    assets: RecordedAssets,
    mut exit: MessageWriter<AppExit>,
) {
    if recording.finished {
        return;
    }
    if recording.launched.elapsed()
        > Duration::from_secs(60) + recording.warmup + recording.duration
    {
        recording.finished = true;
        error!("desktop recording exceeded its startup/collection deadline");
        exit.write(AppExit::error());
        return;
    }
    let Ok(window) = windows.single() else { return };
    if !window.visible || !terminal.is_measured() {
        return;
    }
    let now = Instant::now();
    let ready = *recording.ready.get_or_insert(now);
    let age = now.duration_since(ready);
    let previous = recording.previous.replace(now);
    if age < recording.warmup {
        return;
    }
    let interval = previous.map(|last| now.duration_since(last).as_secs_f64());
    let schedule = now.duration_since(recording.first).as_secs_f64();
    let mut totals = [0_u64; 8];
    for stat in &stats {
        for (total, value) in totals.iter_mut().zip([
            u64::from(stat.changed_rows),
            u64::from(stat.snapshot_cells),
            u64::from(stat.solid_quads),
            u64::from(stat.glyph_quads),
            u64::from(stat.draw_batches),
            u64::from(stat.shape_misses),
            stat.snapshot_ns,
            stat.scene_ns,
        ]) {
            *total += value;
        }
    }
    let row = Sample {
        age: age.as_secs_f64(),
        interval,
        marker_wall: schedule,
        focused: window.focused,
        cols: terminal.cols,
        rows: terminal.rows,
        scale: window.resolution.scale_factor(),
        bytes: runtime.0.processed_bytes(),
        since_install: now.duration_since(recording.launched).as_secs_f64(),
        totals,
        resources: {
            let (objects, anchors) = runtime.1.object_counts();
            [
                objects,
                anchors,
                assets.images.len(),
                assets.meshes.len(),
                assets.materials.len(),
            ]
        },
        placement: recording
            .tracked_object
            .and_then(|id| runtime.1.placement_state(id)),
        resize_phase: recording.resize_phase,
        physical_size: (
            window.resolution.physical_width(),
            window.resolution.physical_height(),
        ),
    };
    recording.rows.push(row);
    let capped = recording.rows.len() >= 100_000;
    if age >= recording.warmup + recording.duration || capped {
        recording.finished = true;
        let mut report = String::from(
            "# First-to-Last marker wall times; not complete schedule CPU time, GPU duration or input-to-display latency. Recording overhead and frame allocations require separate overhead/memory runs. Missing first interval is empty.\n",
        );
        writeln!(
            report,
            "# warmup_seconds={},requested_seconds={},sample_cap_reached={capped}",
            recording.warmup.as_secs_f64(),
            recording.duration.as_secs_f64()
        )
        .expect("string write");
        writeln!(report, "# tracked_object_id={:?}", recording.tracked_object)
            .expect("string write");
        report.push_str("seconds_since_visible_measured,frame_interval_seconds,first_to_last_marker_wall_seconds,focused,cols,rows,scale_factor,processed_bytes,seconds_since_recorder_install,changed_rows,snapshot_cells,solid_quads,glyph_quads,draw_batches,shape_misses,snapshot_ns,scene_ns,inline_objects,inline_anchors,image_assets,mesh_assets,standard_material_assets,tracked_object_row,tracked_object_col,tracked_object_yaw_degrees,resize_phase,physical_width,physical_height\n");
        for row in &recording.rows {
            let interval = row
                .interval
                .map(|value| format!("{value:.9}"))
                .unwrap_or_default();
            write!(
                report,
                "{:.9},{interval},{:.9},{},{},{},{},{},{:.9}",
                row.age,
                row.marker_wall,
                row.focused,
                row.cols,
                row.rows,
                row.scale,
                row.bytes,
                row.since_install
            )
            .expect("string write");
            for value in row.totals {
                write!(report, ",{value}").expect("string write");
            }
            for value in row.resources {
                write!(report, ",{value}").expect("string write");
            }
            if let Some((row, col, yaw)) = row.placement {
                write!(report, ",{row},{col},{yaw}").expect("string write");
            } else {
                report.push_str(",,,");
            }
            if let Some(phase) = row.resize_phase {
                write!(report, ",{phase}").expect("string write");
            } else {
                report.push(',');
            }
            writeln!(report, ",{},{}", row.physical_size.0, row.physical_size.1)
                .expect("string write");
        }
        match std::fs::write(&recording.output, report) {
            Ok(()) if !capped => {
                recording.written.store(true, Ordering::Relaxed);
                exit.write(AppExit::Success);
            }
            result => {
                error!(
                    ?result,
                    capped, "desktop performance recording failed or reached sample cap"
                );
                exit.write(AppExit::error());
            }
        }
    }
}

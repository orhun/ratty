#[cfg(feature = "app")]
mod app_bench;
#[cfg(feature = "app")]
mod flood_bench;
mod graphics_workload;
#[cfg(feature = "app")]
mod interactive_bench;
mod smoke;
mod vt_bench;
mod window_workload;

use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(about = "Ratty development and performance tools")]
struct Args {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Repeatedly create, update, scroll, and delete Kitty and RGP objects.
    GraphicsWorkload(graphics_workload::Args),
    /// Emit a paced, deterministic workload inside a real Ratty window.
    WindowWorkload(window_workload::Args),
    /// Measure real PTY delivery and CPU terminal drawing (requires --features app).
    #[cfg(feature = "app")]
    BenchApp(app_bench::Args),
    /// Measure input/model acknowledgements interleaved with PTY output and replies.
    #[cfg(feature = "app")]
    BenchInteractive(interactive_bench::Args),
    /// Observe input acknowledgements while a producer continues flooding output.
    #[cfg(feature = "app")]
    BenchFlood(flood_bench::Args),
    #[cfg(feature = "app")]
    #[command(hide = true)]
    FloodProducer { bursts: u32, lines: u32 },
    #[cfg(feature = "app")]
    #[command(hide = true)]
    InteractiveProducer { bursts: u32, lines: u32 },
    /// Replay a prepared workload into a PTY after a one-byte readiness handshake.
    #[cfg(feature = "app")]
    Replay { path: PathBuf },
    /// Measure deterministic VT processing and reflow workloads.
    BenchVt(vt_bench::Args),
    /// Capture and validate the seven renderer smoke cases.
    Smoke(smoke::Args),
    /// Emit deterministic PTY input, without an interpreter dependency.
    Emit {
        #[arg(value_enum)]
        kind: Sample,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Sample {
    Text,
    Eof,
}

const TEXT_SAMPLE: &str = concat!(
    "line-height\n┌────┬────┐\n│box │draw│\n└────┴────┘\n",
    "▁▂▃▄▅▆▇█ ▏▎▍▌▋▊▉█ ▖▗▘▝ ▙▚▛▜▞▟\n",
    "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m ",
    "\x1b[1;3mbold italic\x1b[0m\n",
);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is inside the repository")
        .to_owned()
}

fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Task::GraphicsWorkload(args) => graphics_workload::run(args),
        Task::WindowWorkload(args) => window_workload::run(args),
        #[cfg(feature = "app")]
        Task::BenchApp(args) => app_bench::run(args),
        #[cfg(feature = "app")]
        Task::BenchInteractive(args) => interactive_bench::run(args),
        #[cfg(feature = "app")]
        Task::BenchFlood(args) => flood_bench::run(args),
        #[cfg(feature = "app")]
        Task::FloodProducer { bursts, lines } => flood_bench::producer(bursts, lines),
        #[cfg(feature = "app")]
        Task::InteractiveProducer { bursts, lines } => interactive_bench::producer(bursts, lines),
        #[cfg(feature = "app")]
        Task::Replay { path } => app_bench::replay(&path),
        Task::BenchVt(args) => vt_bench::run(args),
        Task::Smoke(args) => smoke::run(args),
        Task::Emit { kind } => {
            let bytes = match kind {
                Sample::Text => TEXT_SAMPLE,
                Sample::Eof => "scene-eof-ok\n",
            };
            io::stdout().lock().write_all(bytes.as_bytes())?;
            Ok(())
        }
    }
}

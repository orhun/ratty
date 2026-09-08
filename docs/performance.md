# Performance work

Use optimized builds and keep correctness validation enabled. Ratty's internal
`xtask` crate provides deterministic workloads and the renderer smoke runner.
It is unpublished; its dependencies are not application runtime dependencies.
The `app` tooling feature enables Bevy-dependent application measurements.
Cargo's workspace packaging command includes unpublished members, so exclude the
internal tool explicitly when verifying distributable crates:

```sh
cargo +stable package --workspace --exclude xtask --locked
```

## Commands

```sh
mkdir -p target/performance
cargo +stable build --release --locked -p xtask
target/release/xtask bench-vt --output target/performance/vt.json

cargo +stable build --release --locked -p xtask --features app
target/release/xtask bench-app --output target/performance/app.json
```

Both commands accept `--filter` and `--samples`. VT processing accepts `--rounds`;
the application harness accepts `--frames` and `--lines`. For example:

```sh
target/release/xtask bench-vt --filter ascii/120x40/chunk-4096 \
  --samples 7 --rounds 128 --output target/performance/ascii.json
target/release/xtask bench-app --filter pty/bulk/120x40 \
  --samples 7 --lines 32768 --output target/performance/pty.json
```

The VT matrix covers three grid sizes, five chunk sizes (including single bytes
and fragmented UTF-8), ASCII, Unicode, styles, cursor edits, alternate screens,
scroll regions, and resize/reflow with 2,000 or 20,000 retained rows. It compares
chunked input with whole-corpus parsing and validates visible state, input modes,
and retained history outside the timed region. Invalid workloads are listed in
`validation_failures`, excluded from the timing results, and cause a nonzero exit.

VT parser construction and corpus generation are excluded. Processing includes
initial history growth; these are not exclusively prefilled steady-state samples.
Reflow prefill/cloning is excluded. Its long Unicode lines fit the original width,
wrap at half width, and rejoin when widened; the initial narrow operation can evict
history at capacity. Each sample measures the configured number of resize pairs.

The application harness measures CPU parsing and retained-buffer drawing without
a GPU or window. It covers sparse/full redraws, scrolling, selections, and resize.
`forced-redraw-unchanged` deliberately calls the drawing path; it does not measure
the application's idle redraw suppression. Raw per-update times are retained.
Each sample primes the retained buffer before timing. Full redraws alternate
wide glyphs, foreground/background colors, and font attributes. The discarded
validation pass checks every update's dimensions, symbols, cell widths, colors,
attributes, and selection; measured samples check their final surface.

PTY replay uses a real child process, bounded reader channel, inline filtering,
and `drain_pty_output`. The child loads its prepared file and enters raw mode
before sending `READY`. Timing starts when the parent sends the replay signal,
and ends only after all output is drained and EOF is observed. The harness checks
the exact parser byte count and final state against direct parsing, and requires
a successful child exit. The discarded validation run also hashes the complete
delivered stream, including output evicted from scrollback. Digest collection is
disabled during reported samples. Startup to readiness and individual drain-call
times are recorded separately. This is not
input-to-display latency, and the replay includes producer/PTY scheduling costs.
The parser byte counter is compiled only with Ratty's `performance` feature.

The burst/input/reply workload uses a real PTY and checks each cursor-position
reply before accepting its acknowledgement:

```sh
target/release/xtask bench-interactive --samples 5 --bursts 64 \
  --burst-lines 4096 --output target/performance/interactive.json
```

A separate thread requests input after a 1 ms delay from each burst trigger.
The latency starts immediately before taking the input writer lock and writing;
it ends when the caller observes the acknowledged terminal model after draining.
The producer writes the whole burst and validates a cursor reply before sending
the acknowledgement. This is an input-request-to-burst-completion proxy, including
producer serialization; it does not establish responsiveness while output
continues or input-to-display latency. The requested delay does not prove overlap
with parsing. Raw per-burst latencies and drain durations are retained, alongside
sample elapsed time. A discarded validation run checks the entire stream digest;
all runs check byte count, final screen/history, child success, and the final
completion handshake, which rejects duplicate trailing input/replies.

For acknowledgements emitted while output continues, use the sustained producer:

```sh
target/release/xtask bench-flood --samples 5 --bursts 32 --lines 65536 \
  --output target/performance/flood.json
```

Its input reader runs concurrently with the output writer. The writer inserts
an acknowledgement between complete 64-line blocks, preserves a header row, and
continues scrolling the body. The report records the insertion block and whether
the producer acknowledged before finishing its payload. It separately records
whether the consumer observed that acknowledgement before consuming all payload
bytes. The observation endpoint is the first inspection after `drain_pty_output`
returns; when intermediate state coalesces into the completion marker, the
reported latency is an upper bound. Late injections are flagged and must not
support a claim about input response during ongoing output.

Every sample checks the cursor reply, final handshake, exit status, byte count,
and final model. The discarded sample also verifies exact byte order by
reconstructing the stream from acknowledgement insertion positions. Markers clear
old header text and include a terminator, so partial PTY reads remain pending.
The PTY channel holds at most 16 chunks of 16 KiB, with one further chunk in a
blocked reader send. All PTY reports include
`peak_outstanding_pty_bytes_upper_bound`, measured over the runtime's lifetime
(including readiness). It counts bytes before sending and subtracts them after
receipt. A scheduling gap between receipt and subtraction can temporarily count
an eighteenth chunk, so this metric is an upper bound on backlog, not exact
channel occupancy. Unread kernel PTY and producer bytes are excluded, so this
does not bound total end-to-end backlog. Each run checks the 288 KiB accounting bound and zero bytes
outstanding after EOF. The fixed reader buffer, parser history, inline assets,
and other process memory are excluded; this does not replace RSS measurements.
Counters are enabled only by the `performance` feature and must be identical in
baseline and candidate builds.

All three PTY benchmark producers use CRLF with child raw mode enabled. Regression
tests verify actual scrolling and history eviction for bulk/burst output, and
body scrolling with a protected header for sustained output. Older development
artifacts using CSI E are overwrite workloads and are superseded for these claims.

## Comparing revisions

See [current results and evidence limits](performance-results.md).

Use the same harness, corpora, compiler, lockfile, optimization flags, and machine
configuration for both revisions. Use **separate Cargo target directories** for
different worktrees and retain each executable with its source revision and hash.
Verify that the candidate really contains the intended change before timing it.

Record CPU/GPU, OS, backend, fonts, grid/window sizes, scale factor, update mode,
power source, thermal state, and background load. Run benchmarks serially without
compilation or other heavy work. Alternate baseline and candidate runs, discard
warmup samples, and retain raw results. Report medians, variability, absolute and
relative changes, and the number of independent runs. Use enough events to
support tail-latency claims; do not infer desktop responsiveness from parser
throughput or a handful of samples.

The initial ASCII pilot had typical min-to-max variation of about 2–5%, with
occasional larger outliers. The acceptance rule chosen before optimization was a
repeatable improvement of at least 10% in the profiled large-chunk workload over
alternating runs; median regressions above 5% require investigation and repetition.
Invalid baseline workloads cannot establish a performance gain.

For CPU sampling, retain symbols without changing release optimizations:

```sh
cargo +stable build --locked -p xtask --profile profiling
target/profiling/xtask bench-vt --filter ascii/120x40/chunk-4096 \
  --samples 1 --rounds 5000 --output target/performance/profile-run.json
```

Attach the platform profiler to this process (for example, macOS `sample`). Keep
profiled timings separate from uninstrumented comparison data. The `profiling`
profile inherits release optimizations and retains debug symbols. GPU timings,
desktop idle CPU, and input-to-display latency need their own measurements;
headless readback and software Vulkan do not substitute for them.

## Real-window recording

Build the application with optional instrumentation and set an explicit output
path to collect CSV rows from its normal window/plugin pipeline:

```sh
cargo +stable build --release --locked --bin ratty --features performance
RATTY_PERF_REPORT=/tmp/ratty-window.csv RATTY_PERF_SECONDS=10 \
  RATTY_PERF_WARMUP_SECONDS=3 target/release/ratty
```

The recorder starts its warmup when the window is visible and the terminal has
measured font geometry. It records window focus, grid size, scale, delivered
bytes, renderer counters, intervals between main-world updates, and wall time
between markers in `First` and `Last`. Those wall times include scheduling and
waiting; they are not thread CPU time, GPU duration, presentation completion, or
input-to-display latency. Renderer snapshot/scene counters describe CPU work.
Before desktop timing runs, separately verify that the OS session is unlocked
and the intended window is actually presented. Bevy's visible/focused fields do
not establish that the lock screen is absent. On macOS, the
`CGSSessionScreenIsLocked` property reported by `ioreg -n Root -d 1 -l` can expose
a locked session. Runs without verified session state support pipeline validation
only, not active-desktop or input-to-display performance claims.
The CSV also records registered inline objects/anchors and main-world image,
mesh, and standard-material asset counts. Asset counts include the rest of the
application and may settle after deferred cleanup; compare against the same
scene's pre-workload level after allowing cleanup frames. They do not measure
allocation sizes, pending payload buffers, render-world resources, or GPU memory.
The application exits after writing the requested interval. A 100,000-sample cap
limits storage; reaching it reports failure rather than silently truncating a run.
Keep the PTY child alive throughout the interval. Early exit, write failure, and
the startup/collection deadline return failure. Activation replaces any previous
report with an explicit incomplete marker; only a finished run replaces it with
CSV data. The output directory must already exist. With zero warmup, the first
frame interval is empty because there is no preceding observed frame.

The Rust tooling also supplies deterministic paced desktop workloads:

```sh
cargo +stable build --release --locked -p xtask
RATTY_PERF_REPORT=/tmp/ratty-sparse.csv RATTY_PERF_SECONDS=10 \
  target/release/ratty -e "$PWD/target/release/xtask" window-workload \
  --scene sparse --cols 120 --rows 40 --hz 30 --seconds 30
```

Scenes are `idle`, `sparse`, `full`, and `scroll`. Frames alternate precomputed
text/colors. Full redraws leave the last column untouched to avoid accidental
wrapping; scrolling uses CRLF. `--cursor` preserves cursor visibility for blink
measurements; otherwise the cursor is hidden. Idle emits its initial text once
and sleeps for the duration without periodic producer wakeups. Choose a child
duration longer than application startup plus warmup and collection. The requested
grid describes the payload, not a window resize: verify the actual recorder grid
and configure window size/font/scale accordingly. Cadence uses absolute producer
deadlines; backpressure can make emission late or cause catch-up writes, so `--hz`
is not evidence of achieved frame rate or presentation latency.

`xtask graphics-workload --cycles 20 --rows 24` supplies a graphics lifecycle
scene using the widget's `black.obj` model and RGP wire format, plus alternating
128×128 RGB Kitty images, split into 4,096-character base64 APC chunks with
continuation flags like the widget's image protocol. Run it as Ratty's child at a verified grid of at least
40 columns; `--rows` must match the actual grid for prefill and placement.
Each cycle registers/places the model, retransmits images and updates/animates
the model while scrolling, then deletes both IDs and allows 250 ms for cleanup.
Each cycle prefills uniquely labeled rows and uses explicit CSI S scrolling
without simultaneous text edits. This avoids ambiguity in Ratty's existing
row-comparison scroll inference; earlier blank-row/CRLF fixtures lost anchors
and cannot establish complete model-update coverage. The model starts six rows
above the bottom so it remains visible through all twelve scroll steps.
Set `RATTY_PERF_OBJECT_ID=711` to append the model's live placement row, column,
and yaw to the recorder CSV. Empty fields mean no placement (or no tracking ID).
These are main-world placement properties, not the final animated GPU transform.
The graphics producer starts with two idle seconds and ends with five cleanup
seconds. Choose the recorder duration to include cleanup but finish before the
child exits. The default 20 cycles take about 24 seconds plus backpressure;
frames and payloads are prepared before emission. Run this only in its dedicated
benchmark terminal: it clears the screen and owns object IDs 710 and 711.

For dynamic font/DPI/window coverage, set `RATTY_PERF_RESIZE_CYCLE=1` alongside
the report path and use a text child that stays alive. After warmup, two-second
phases repeat: initial configuration, font size +2 points, restore, scale-factor
override 1.5, restore, physical window dimensions +25%, restore. Font changes use
the same adjustment/redraw path as keyboard zoom, and normal renderer remeasurement
drives PTY reflow. `resize_phase` records the absolute phase number; actual grid
and scale/physical-dimension fields show when changes settle. Size targets are
reapplied when asynchronous window events change them during DPI transitions.
Missing phases or failed restoration
invalidate a complete-cycle claim. This is scripted application behavior, not
OS input latency or a physical monitor transition. Each phase derives its target
from the captured initial state, so skipped phases cannot accumulate zoom/size.

Use separate uninstrumented runs for process CPU and memory, and quantify
recording overhead before drawing performance conclusions. Numeric samples are
retained and formatted after collection; sample-vector growth is still recorder
memory, not terminal memory growth.
On macOS, `top -l 27 -s 1 -pid PID -stats pid,cpu,mem,time,threads,state`
samples a selected application process once per second. Its `MEM` column is
physical footprint, not RSS; `TIME` is cumulative process CPU time. Preserve
the raw output, child/application exit status, and workload phase timing. Start
the application without `RATTY_PERF_REPORT` for this overhead control and use a
bounded child workload. Sampling and other active jobs must be recorded; these
readings do not isolate the main thread or prove GPU allocation behavior.
Without `RATTY_PERF_REPORT`, no recorder systems or storage are installed; normal
builds omit the instrumentation entirely.

## Renderer smoke tooling migration

The maintained Python inventory contained `.github/scripts/headless-smoke.py`
and its two embedded sample producers. They are replaced by:

| Previous operation | Rust replacement |
| --- | --- |
| Smoke orchestration, configuration, PNG/log validation, results | `cargo run --locked -p xtask -- smoke --output <directory>` |
| Styled text producer | `xtask emit text` |
| Short-lived scene producer | `xtask emit eof` |

Build the capture and widget examples first. Sharing their target directory makes
the runner's default paths work:

```sh
cargo +stable build --locked --example headless_snapshot
CARGO_TARGET_DIR="$PWD/target" cargo +stable build --locked \
  --manifest-path widget/Cargo.toml --example document --example render_test
cargo +stable run --locked -p xtask -- smoke --output target/performance/smoke
```

Supply `--font-dir` with the directory containing all four DejaVu Sans Mono faces
when they are not in `/usr/share/fonts/truetype/dejavu`. `--snapshot`, `--document`,
and `--render-test` override binary paths. The CI job still supplies software
Vulkan and fonts; the runner itself has no Python dependency.

All seven original cases, diagnostics, exact input-delivery checks, dimensions,
fallback/EOF markers, logs, and result fields are retained. PNG validation now
explicitly decodes PNG data rather than checking only its header. During migration,
both runners passed against the same binaries/fonts: reports matched, six PNGs
were byte-identical, and independent visual review confirmed that the remaining
document image differed only in animated mesh silhouettes. Always visually review
the captures alongside automated state checks after rendering changes.

The runner kills and reaps a capture that exceeds its outer timeout. As with the
previous runner, this is not proof of cleanup for an arbitrary PTY descendant
ignoring hangup; the capture binary's own deadline normally shuts down its runtime
first. The `vte` dependency also has an existing chunk-dependent distinction in
callbacks for UTF-8-encoded C1 controls; the data-loss workaround does not claim to
change that callback classification.

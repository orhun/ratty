use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::{PRELUDE, QUERY, ack, chunks, done};
use anyhow::{Result, ensure};
use clap::Parser;
use ratty::config::AppConfig;
use ratty::inline::TerminalInlineObjects;
use ratty::runtime::{RuntimeOptions, TerminalRuntime};
use ratty::systems::drain_pty_output;

#[derive(Parser)]
pub struct Args {
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..))]
    samples: u32,
    #[arg(long, default_value_t = 32, value_parser = clap::value_parser!(u32).range(1..))]
    bursts: u32,
    #[arg(long, default_value_t = 65536, value_parser = clap::value_parser!(u32).range(1..=1_000_000))]
    lines: u32,
}

pub fn run(args: Args) -> Result<()> {
    let mut config = AppConfig::default();
    config.terminal.default_cols = 120;
    config.terminal.default_rows = 40;
    let chunks = chunks(args.lines);
    let mut samples = Vec::new();
    for index in 0..=args.samples {
        let sample = sample(&config, &args, &chunks, index == 0)?;
        if index > 0 {
            samples.push(sample);
        }
    }
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        args.output,
        serde_json::to_string_pretty(&serde_json::json!({
            "schema":1,"kind":"sustained-flood","cols":120,"rows":40,"scrollback":config.terminal.scrollback,
            "payload_bytes_per_burst":chunks.iter().map(Vec::len).sum::<usize>(),"blocks_per_burst":chunks.len(),
            "note":"Producer reads input on a separate thread and acknowledges between complete output blocks, preserving row 1 while scrolling rows 2..40. Input-request timestamp precedes writer lock/write. Observer endpoint is first drain-return inspection proving ACK processed, an upper bound when ACK coalesces into DONE. Overlap flags distinguish producer response during output from observer response before full consumption. One discarded digest-validation run; all runs validate replies, count, final state, final sentinel and child success. No display-latency claim.",
            "samples":samples
        }))? + "\n",
    )?;
    Ok(())
}

fn sample(
    config: &AppConfig,
    args: &Args,
    chunks: &[Vec<u8>],
    digest: bool,
) -> Result<serde_json::Value> {
    let mut runtime = TerminalRuntime::spawn(
        config,
        &RuntimeOptions {
            command: Some(vec![
                std::env::current_exe()?.to_string_lossy().into_owned(),
                "flood-producer".into(),
                args.bursts.to_string(),
                args.lines.to_string(),
            ]),
            working_dir: Some(crate::root()),
        },
    )?;
    let mut inline = TerminalInlineObjects::default();
    let mut camera = Vec::new();
    let ready = Instant::now();
    loop {
        let drained = drain_pty_output(&mut runtime, &mut inline, &mut camera);
        if runtime.screen().contents() == "READY" {
            break;
        }
        ensure!(
            !drained.disconnected && ready.elapsed() < Duration::from_secs(30),
            "flood readiness failed"
        );
        thread::sleep(Duration::from_micros(50));
    }
    let initial_bytes = runtime.processed_bytes();
    if digest {
        runtime.start_processed_digest();
    }
    let writer = runtime.writer.clone();
    let (trigger, requests) = mpsc::sync_channel::<()>(1);
    let (sent, timestamps) = mpsc::sync_channel(1);
    let input = thread::spawn(move || -> Result<()> {
        while requests.recv().is_ok() {
            thread::sleep(Duration::from_millis(1));
            let now = Instant::now();
            {
                let mut guard = writer
                    .lock()
                    .map_err(|_| anyhow::anyhow!("input writer poisoned"))?;
                let writer = guard
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("input writer closed"))?;
                writer.write_all(b"I")?;
                writer.flush()?;
            }
            if sent.send(now).is_err() {
                break;
            }
        }
        Ok(())
    });
    let result = measure(&mut runtime, chunks, args.bursts, &trigger, &timestamps).and_then(
        |(bursts, positions)| {
            let count = validate(
                &mut runtime,
                config,
                chunks,
                &positions,
                initial_bytes,
                digest,
            )?;
            let (queued, peak_queued) = runtime.queued_bytes();
            ensure!(queued == 0 && peak_queued > 0 && peak_queued <= ratty::runtime::PTY_QUEUE_ACCOUNTING_BOUND, "PTY queue accounting out of bounds");
            Ok(serde_json::json!({"bursts":bursts,"consumed_bytes":count,"peak_outstanding_pty_bytes_upper_bound":peak_queued}))
        },
    );
    drop(trigger);
    drop(timestamps);
    runtime.shutdown();
    input
        .join()
        .map_err(|_| anyhow::anyhow!("input thread panicked"))??;
    result
}

fn marker(text: &str) -> Result<Option<(bool, u32, usize)>> {
    let Some(text) = text.trim_end().strip_suffix('!') else {
        return Ok(None);
    };
    let (done, value) = if let Some(value) = text.strip_prefix("DONE") {
        (true, value)
    } else if let Some(value) = text.strip_prefix("ACK") {
        (false, value)
    } else {
        return Ok(None);
    };
    let (burst, block) = value
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid flood marker"))?;
    Ok(Some((done, burst.parse()?, block.parse()?)))
}

fn observer_overlaps(position: usize, blocks: usize, consumed: u64, payload_end: usize) -> bool {
    position < blocks && consumed < payload_end as u64
}

fn measure(
    runtime: &mut TerminalRuntime,
    chunks: &[Vec<u8>],
    count: u32,
    trigger: &mpsc::SyncSender<()>,
    timestamps: &mpsc::Receiver<Instant>,
) -> Result<(Vec<serde_json::Value>, Vec<usize>)> {
    let mut inline = TerminalInlineObjects::default();
    let mut camera = Vec::new();
    let payload_bytes: usize = chunks.iter().map(Vec::len).sum();
    let mut bursts = Vec::new();
    let mut positions = Vec::new();
    for burst in 0..count {
        let initial = runtime.processed_bytes();
        let start = Instant::now();
        runtime.write_input(b"G");
        trigger.send(())?;
        let mut first = None;
        let mut drains = Vec::new();
        let position = loop {
            let call = Instant::now();
            let drained = drain_pty_output(runtime, &mut inline, &mut camera);
            drains.push(call.elapsed().as_secs_f64());
            let row = runtime.screen().rows(0, 120).next().unwrap_or_default();
            if let Some((finished, id, block)) = marker(&row)? {
                ensure!(id <= burst, "future acknowledgement");
                if id == burst {
                    ensure!(
                        (1..=chunks.len()).contains(&block),
                        "invalid acknowledgement position"
                    );
                    first.get_or_insert((
                        Instant::now(),
                        runtime.processed_bytes() - initial,
                        finished,
                    ));
                    if finished {
                        break block;
                    }
                }
            }
            ensure!(
                !drained.disconnected && start.elapsed() < Duration::from_secs(30),
                "flood acknowledgement {burst} failed"
            );
            if !drained.processed {
                thread::sleep(Duration::from_micros(50));
            }
        };
        let elapsed = start.elapsed().as_secs_f64();
        let request = timestamps.recv_timeout(Duration::from_secs(5))?;
        let (observed, consumed, coalesced) = first.expect("DONE was observed");
        let payload_end = PRELUDE.len() + payload_bytes + ack(burst, position).len();
        bursts.push(serde_json::json!({"seconds":elapsed,
            "input_request_to_observed_ack_upper_bound_seconds":observed.duration_since(request).as_secs_f64(),
            "producer_ack_before_payload_end":position < chunks.len(),
            "observer_ack_before_payload_consumed":observer_overlaps(position, chunks.len(), consumed, payload_end),
            "ack_coalesced_into_done":coalesced,"ack_after_block":position,
            "consumed_bytes_at_first_observation":consumed,"drain_seconds":drains}));
        positions.push(position);
    }
    runtime.write_input(b"F");
    let end = Instant::now();
    while !drain_pty_output(runtime, &mut inline, &mut camera).disconnected {
        ensure!(end.elapsed() < Duration::from_secs(5), "flood EOF missing");
        thread::sleep(Duration::from_micros(50));
    }
    loop {
        if let Some(success) = runtime.child_exit_success()? {
            ensure!(success, "flood producer failed");
            break;
        }
        ensure!(
            end.elapsed() < Duration::from_secs(5),
            "flood child did not exit"
        );
        thread::sleep(Duration::from_millis(1));
    }
    Ok((bursts, positions))
}

fn validate(
    runtime: &mut TerminalRuntime,
    config: &AppConfig,
    chunks: &[Vec<u8>],
    positions: &[usize],
    initial: u64,
    check_digest: bool,
) -> Result<u64> {
    let mut expected = ratty_vt::Parser::new(40, 120, config.terminal.scrollback);
    expected.process(b"READY");
    let mut count = 0_u64;
    let mut digest = 0xcbf29ce484222325_u64;
    let mut consume = |bytes: &[u8]| {
        expected.process(bytes);
        count += bytes.len() as u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
    };
    for (burst, &position) in positions.iter().enumerate() {
        consume(PRELUDE);
        for (block, chunk) in chunks.iter().enumerate() {
            consume(chunk);
            if block + 1 == position {
                consume(&ack(burst as u32, position));
            }
        }
        consume(QUERY);
        consume(&done(burst as u32, position));
    }
    ensure!(
        runtime.processed_bytes() - initial == count,
        "flood byte count differs"
    );
    ensure!(
        crate::vt_bench::fingerprint(runtime.screen_mut())
            == crate::vt_bench::fingerprint(expected.screen_mut()),
        "flood final state differs"
    );
    if check_digest {
        ensure!(
            runtime.processed_digest() == Some(digest),
            "flood stream order/digest differs"
        );
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_markers_never_accept_a_partial_or_stale_suffix() {
        for replacement in [ack(0, 2), done(0, 2)] {
            let mut parser = ratty_vt::Parser::new(40, 120, 0);
            parser.process(PRELUDE);
            let data = chunks(128);
            for chunk in &data {
                parser.process(chunk);
            }
            let mut consumed = PRELUDE.len() + data.iter().map(Vec::len).sum::<usize>();
            let payload_end = consumed + ack(0, 2).len();
            let old = if replacement.starts_with(b"\x1b7") {
                None
            } else {
                parser.process(&ack(0, 2));
                consumed += ack(0, 2).len();
                Some((false, 0, 2))
            };
            let mut terminated = false;
            let mut observed_before_restore = false;
            for byte in replacement {
                parser.process(&[byte]);
                consumed += 1;
                terminated |= byte == b'!';
                let row = parser.screen().rows(0, 120).next().unwrap();
                let observed = marker(&row).unwrap();
                if !terminated {
                    assert!(
                        observed.is_none() || observed == old,
                        "premature marker {row:?}"
                    );
                } else {
                    assert_eq!(observed.map(|(_, id, block)| (id, block)), Some((0, 2)));
                }
                // A last-block ACK is never evidence of remaining payload,
                // including when the marker precedes its cursor-restore bytes.
                if observed.is_some() {
                    observed_before_restore |= consumed < payload_end;
                    assert!(!observer_overlaps(2, 2, consumed as u64, payload_end));
                }
            }
            if old.is_none() {
                assert!(observed_before_restore);
            }
        }
    }
}

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use clap::Parser;
use ratty::config::AppConfig;
use ratty::inline::TerminalInlineObjects;
use ratty::runtime::{RuntimeOptions, TerminalRuntime};
use ratty::systems::drain_pty_output;

const QUERY: &[u8] = b"\x1b[H\x1b[6n";
const REPLY: &[u8] = b"\x1b[1;1R";

#[derive(Parser)]
pub struct Args {
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..))]
    samples: u32,
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..))]
    bursts: u32,
    #[arg(long, default_value_t = 4096, value_parser = clap::value_parser!(u32).range(1..))]
    burst_lines: u32,
}

fn payload(lines: u32) -> Vec<u8> {
    let mut data = Vec::new();
    for line in 0..lines {
        data.extend_from_slice(
            format!("{line:08} interactive flood payload abcdefghijklmnopqrstuvwxyz\r\n")
                .as_bytes(),
        );
    }
    data
}

fn acknowledgement(burst: u32) -> Vec<u8> {
    format!("\x1b[2J\x1b[HACK{burst:08}").into_bytes()
}

pub fn run(args: Args) -> Result<()> {
    let mut config = AppConfig::default();
    config.terminal.default_cols = 120;
    config.terminal.default_rows = 40;
    let data = payload(args.burst_lines);
    let mut expected = ratty_vt::Parser::new(40, 120, config.terminal.scrollback);
    expected.process(b"READY");
    let mut expected_digest = 0xcbf29ce484222325_u64;
    let mut expected_bytes = 0_u64;
    for burst in 0..args.bursts {
        for bytes in [&data[..], QUERY, &acknowledgement(burst)] {
            expected.process(bytes);
            expected_bytes += bytes.len() as u64;
            for byte in bytes {
                expected_digest = (expected_digest ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
        }
    }
    let expected_state = crate::vt_bench::fingerprint(expected.screen_mut());
    let mut samples = Vec::new();
    for sample in 0..=args.samples {
        let mut runtime = TerminalRuntime::spawn(
            &config,
            &RuntimeOptions {
                command: Some(vec![
                    std::env::current_exe()?.to_string_lossy().into_owned(),
                    "interactive-producer".into(),
                    args.bursts.to_string(),
                    args.burst_lines.to_string(),
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
                "interactive readiness failed"
            );
            thread::sleep(Duration::from_micros(50));
        }
        let initial_bytes = runtime.processed_bytes();
        if sample == 0 {
            runtime.start_processed_digest();
        }
        let writer = runtime.writer.clone();
        let (trigger, requests) = mpsc::sync_channel::<()>(1);
        let (sent, timestamps) = mpsc::sync_channel(1);
        // Input is queued independently while the caller may be inside an
        // unbounded drain. This measures write-to-observed-model latency;
        // it does not include dispatching a physical keyboard event.
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
        let result = (|| -> Result<_> {
            let mut latencies = Vec::new();
            let mut drains = Vec::new();
            let start = Instant::now();
            for burst in 0..args.bursts {
                let marker = format!("ACK{burst:08}");
                runtime.write_input(b"G");
                trigger.send(())?;
                let deadline = Instant::now();
                loop {
                    let call = Instant::now();
                    let drained = drain_pty_output(&mut runtime, &mut inline, &mut camera);
                    drains.push(call.elapsed().as_secs_f64());
                    if runtime.screen().contents() == marker {
                        let observed = Instant::now();
                        let sent = timestamps.recv_timeout(Duration::from_secs(5))?;
                        latencies.push(observed.duration_since(sent).as_secs_f64());
                        break;
                    }
                    ensure!(
                        !drained.disconnected && deadline.elapsed() < Duration::from_secs(30),
                        "interactive acknowledgement {burst} failed"
                    );
                    if !drained.processed {
                        thread::sleep(Duration::from_micros(50));
                    }
                }
            }
            runtime.write_input(b"F");
            let eof = Instant::now();
            while !drain_pty_output(&mut runtime, &mut inline, &mut camera).disconnected {
                ensure!(
                    eof.elapsed() < Duration::from_secs(5),
                    "interactive EOF missing"
                );
                thread::sleep(Duration::from_micros(50));
            }
            let seconds = start.elapsed().as_secs_f64();
            loop {
                if let Some(success) = runtime.child_exit_success()? {
                    ensure!(
                        success,
                        "interactive producer rejected input or terminal reply"
                    );
                    break;
                }
                ensure!(
                    eof.elapsed() < Duration::from_secs(5),
                    "interactive child did not exit"
                );
                thread::sleep(Duration::from_millis(1));
            }
            ensure!(
                runtime.processed_bytes() - initial_bytes == expected_bytes,
                "interactive byte count mismatch"
            );
            ensure!(
                crate::vt_bench::fingerprint(runtime.screen_mut()) == expected_state,
                "interactive final state mismatch"
            );
            if sample == 0 {
                ensure!(
                    runtime.processed_digest() == Some(expected_digest),
                    "interactive stream order/digest mismatch"
                );
            }
            let (queued, peak_queued) = runtime.queued_bytes();
            ensure!(
                queued == 0 && peak_queued > 0 && peak_queued <= 18 * 16 * 1024,
                "PTY queue accounting out of bounds"
            );
            Ok(
                serde_json::json!({"seconds":seconds,"input_request_to_burst_ack_model_seconds":latencies,"drain_seconds":drains,"peak_outstanding_pty_bytes_upper_bound":peak_queued}),
            )
        })();
        drop(trigger);
        drop(timestamps);
        runtime.shutdown();
        input
            .join()
            .map_err(|_| anyhow::anyhow!("input thread panicked"))??;
        let result = result?;
        if sample > 0 {
            samples.push(result);
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
            "schema":1,"kind":"interactive-pty","cols":120,"rows":40,
            "scrollback":config.terminal.scrollback,"bursts_per_sample":args.bursts,
            "burst_lines":args.burst_lines,"bytes_per_sample":expected_bytes,
        "note":"One discarded digest-validation run. Input thread requests I after a 1ms delay from G; producer writes the complete payload, requests and validates CSI cursor reply, then acknowledges I. Latency starts before the writer lock/write and ends at screen inspection after drain returns. Sample elapsed time includes the injection delay. This burst-completion proxy does not establish responsiveness while output continues or display latency. No timing threshold.",
            "samples":samples
        }))? + "\n",
    )?;
    Ok(())
}

pub fn producer(bursts: u32, lines: u32) -> Result<()> {
    let data = payload(lines);
    crossterm::terminal::enable_raw_mode()?;
    let result = (|| -> Result<()> {
        let mut input = io::stdin().lock();
        let mut output = io::stdout().lock();
        output.write_all(b"READY")?;
        output.flush()?;
        for burst in 0..bursts {
            let mut signal = [0];
            input.read_exact(&mut signal)?;
            ensure!(signal == *b"G", "expected burst trigger");
            output.write_all(&data)?;
            output.write_all(QUERY)?;
            output.flush()?;
            read_input_and_reply(&mut input)?;
            output.write_all(&acknowledgement(burst))?;
            output.flush()?;
        }
        expect_finish(&mut input)?;
        Ok(())
    })();
    let restore = crossterm::terminal::disable_raw_mode();
    result?;
    restore?;
    Ok(())
}

fn read_input_and_reply(input: &mut impl Read) -> Result<()> {
    // The input may arrive before or after the terminal reply.
    let mut reply = Vec::new();
    let mut received_input = false;
    while !received_input || reply.len() < REPLY.len() {
        let mut signal = [0];
        input.read_exact(&mut signal)?;
        if signal == *b"I" {
            ensure!(!received_input, "duplicate input");
            ensure!(
                reply.is_empty() || reply.len() == REPLY.len(),
                "input interleaved within cursor reply"
            );
            received_input = true;
        } else {
            reply.push(signal[0]);
            ensure!(
                REPLY.starts_with(&reply),
                "incorrect cursor reply {reply:?}"
            );
        }
    }
    Ok(())
}

fn expect_finish(input: &mut impl Read) -> Result<()> {
    let mut finish = [0];
    input.read_exact(&mut finish)?;
    ensure!(
        finish == *b"F",
        "unexpected input/reply before completion sentinel"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_payload_scrolls_and_evicts_old_history() {
        let mut parser = ratty_vt::Parser::new(4, 80, 3);
        parser.process(&payload(20));
        assert!(parser.screen().contents().starts_with("00000017 "));
        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(parser.screen().scrollback(), 3);
        assert!(parser.screen().contents().starts_with("00000014 "));
    }

    #[test]
    fn final_handshake_rejects_trailing_duplicate_input_or_reply() {
        for suffix in [b"I".as_slice(), REPLY, b"x", b""] {
            let mut bytes = b"I\x1b[1;1R".to_vec();
            bytes.extend_from_slice(suffix);
            if !suffix.is_empty() {
                bytes.push(b'F');
            }
            let mut input = io::Cursor::new(bytes);
            read_input_and_reply(&mut input).unwrap();
            assert!(expect_finish(&mut input).is_err());
        }
        let mut input = io::Cursor::new(b"I\x1b[1;1RF");
        read_input_and_reply(&mut input).unwrap();
        expect_finish(&mut input).unwrap();
    }

    #[test]
    fn reply_validation_requires_exact_reply_and_one_input_in_either_order() {
        for bytes in [b"I\x1b[1;1R".as_slice(), b"\x1b[1;1RI"] {
            read_input_and_reply(&mut io::Cursor::new(bytes)).unwrap();
        }
        for bytes in [
            b"II\x1b[1;1R".as_slice(),
            b"I\x1b[2;1R",
            b"\x1b[1;1R",
            b"I",
            b"I\x1b[1;",
            b"\x1b[I1;1R",
        ] {
            assert!(
                read_input_and_reply(&mut io::Cursor::new(bytes)).is_err(),
                "accepted {bytes:?}"
            );
        }
    }
}

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Result, ensure};

mod run;
pub use run::{Args, run};

const PRELUDE: &[u8] = b"\x1b[2J\x1b[2;40r\x1b[2;1H";
const QUERY: &[u8] = b"\x1b[H\x1b[6n";

fn chunks(lines: u32) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    let mut chunk = Vec::new();
    for line in 0..lines {
        chunk.extend_from_slice(
            format!("{line:08} sustained output abcdefghijklmnopqrstuvwxyz\r\n").as_bytes(),
        );
        if (line + 1) % 64 == 0 || line + 1 == lines {
            chunks.push(std::mem::take(&mut chunk));
        }
    }
    chunks
}

fn ack(burst: u32, block: usize) -> Vec<u8> {
    format!("\x1b7\x1b[1;1H\x1b[2KACK{burst:08}:{block:08}!\x1b8").into_bytes()
}

fn done(burst: u32, block: usize) -> Vec<u8> {
    format!("\x1b[1;1H\x1b[2KDONE{burst:08}:{block:08}!").into_bytes()
}

pub fn producer(bursts: u32, lines: u32) -> Result<()> {
    ensure!(
        bursts > 0 && (1..=1_000_000).contains(&lines),
        "invalid flood size"
    );
    let chunks = chunks(lines);
    crossterm::terminal::enable_raw_mode()?;
    let result = (|| -> Result<()> {
        let mut output = io::stdout().lock();
        output.write_all(b"READY")?;
        output.flush()?;
        for burst in 0..bursts {
            let mut signal = [0];
            io::stdin().read_exact(&mut signal)?;
            ensure!(signal == *b"G", "expected flood trigger");
            let (received, input) = mpsc::sync_channel(1);
            let reader = thread::spawn(move || -> Result<()> {
                let mut byte = [0];
                io::stdin().read_exact(&mut byte)?;
                ensure!(byte == *b"I", "expected flood input");
                received.send(())?;
                Ok(())
            });
            output.write_all(PRELUDE)?;
            let mut position = None;
            for (block, chunk) in chunks.iter().enumerate() {
                output.write_all(chunk)?;
                if position.is_none() && input.try_recv().is_ok() {
                    position = Some(block + 1);
                    output.write_all(&ack(burst, block + 1))?;
                    output.flush()?;
                }
            }
            let position = match position {
                Some(position) => position,
                None => {
                    input.recv_timeout(Duration::from_secs(30))?;
                    output.write_all(&ack(burst, chunks.len()))?;
                    chunks.len()
                }
            };
            reader
                .join()
                .map_err(|_| anyhow::anyhow!("producer input thread panicked"))??;
            output.write_all(QUERY)?;
            output.flush()?;
            let mut reply = [0; 6];
            io::stdin().read_exact(&mut reply)?;
            ensure!(&reply == b"\x1b[1;1R", "incorrect flood cursor reply");
            output.write_all(&done(burst, position))?;
            output.flush()?;
        }
        let mut finish = [0];
        io::stdin().read_exact(&mut finish)?;
        ensure!(
            finish == *b"F",
            "unexpected input/reply before flood completion"
        );
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
    fn flood_scrolls_body_while_preserving_the_acknowledgement_header() {
        let mut parser = ratty_vt::Parser::new(40, 120, 2000);
        parser.process(PRELUDE);
        let data = chunks(128);
        parser.process(&data[0]);
        let cursor = parser.screen().cursor_position();
        parser.process(&ack(0, 1));
        assert_eq!(parser.screen().cursor_position(), cursor);
        parser.process(&data[1]);
        let rows: Vec<_> = parser.screen().rows(0, 120).collect();
        assert_eq!(rows[0], "ACK00000000:00000001!");
        assert!(rows[1].starts_with("00000090 "), "{}", rows[1]);
        assert!(rows[38].starts_with("00000127 "), "{}", rows[38]);
        assert!(rows[39].is_empty());
    }
}

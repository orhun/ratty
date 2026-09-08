/// A parser for terminal output which produces an in-memory representation of
/// the terminal contents.
pub struct Parser<CB: crate::callbacks::Callbacks = ()> {
    parser: vte::Parser,
    screen: crate::perform::WrappedScreen<CB>,
    // Conservative count of continuation bytes needed by the last input suffix.
    // vte 0.15 can skip bytes after completing a partial scalar in a bulk call.
    utf8_remaining: u8,
}

impl Parser {
    /// Creates a new terminal parser of the given size and with the given
    /// amount of scrollback.
    #[must_use]
    pub fn new(rows: u16, cols: u16, scrollback_len: usize) -> Self {
        Self {
            parser: vte::Parser::new(),
            screen: crate::perform::WrappedScreen::new(rows, cols, scrollback_len),
            utf8_remaining: 0,
        }
    }
}

impl<CB: crate::callbacks::Callbacks> Parser<CB> {
    /// Creates a new terminal parser of the given size and with the given
    /// amount of scrollback. Terminal events will be reported via method
    /// calls on the provided [`Callbacks`](crate::callbacks::Callbacks)
    /// implementation.
    pub fn new_with_callbacks(rows: u16, cols: u16, scrollback_len: usize, callbacks: CB) -> Self {
        Self {
            parser: vte::Parser::new(),
            screen: crate::perform::WrappedScreen::new_with_callbacks(
                rows,
                cols,
                scrollback_len,
                callbacks,
            ),
            utf8_remaining: 0,
        }
    }

    /// Processes the contents of the given byte string, and updates the
    /// in-memory terminal state.
    pub fn process(&mut self, mut bytes: &[u8]) {
        // Complete a possibly split scalar one byte at a time. In vte 0.15,
        // advance_partial_utf8 can print only its first scalar but consume
        // additional valid bytes before the next incomplete scalar. For example,
        // [C3] + [A9, 'x', C3, A9] loses 'x'. Once the prefix is complete we
        // retain bulk parsing for the rest of the chunk, with no buffering.
        while self.utf8_remaining > 0 && !bytes.is_empty() {
            let byte = bytes[0];
            self.parser.advance(&mut self.screen, &bytes[..1]);
            self.utf8_remaining = match byte {
                0x80..=0xbf => self.utf8_remaining.saturating_sub(1),
                _ => continuation_count(byte),
            };
            bytes = &bytes[1..];
        }
        if bytes.is_empty() {
            return;
        }
        self.parser.advance(&mut self.screen, bytes);
        self.utf8_remaining = incomplete_suffix(bytes);
    }

    /// Returns a reference to a [`Screen`](crate::Screen) object containing
    /// the terminal state.
    #[must_use]
    pub fn screen(&self) -> &crate::Screen {
        &self.screen.screen
    }

    /// Returns a mutable reference to a [`Screen`](crate::Screen) object
    /// containing the terminal state.
    #[must_use]
    pub fn screen_mut(&mut self) -> &mut crate::Screen {
        &mut self.screen.screen
    }

    /// Returns a reference to the [`Callbacks`](crate::callbacks::Callbacks)
    /// state object passed into the constructor.
    pub fn callbacks(&self) -> &CB {
        &self.screen.callbacks
    }

    /// Returns a mutable reference to the
    /// [`Callbacks`](crate::callbacks::Callbacks) state object passed into
    /// the constructor.
    pub fn callbacks_mut(&mut self) -> &mut CB {
        &mut self.screen.callbacks
    }
}

fn continuation_count(lead: u8) -> u8 {
    match lead {
        0xc2..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf4 => 3,
        _ => 0,
    }
}

fn incomplete_suffix(bytes: &[u8]) -> u8 {
    for (continuations, &byte) in bytes.iter().rev().take(4).enumerate() {
        if !(0x80..=0xbf).contains(&byte) {
            return continuation_count(byte).saturating_sub(continuations as u8);
        }
    }
    0
}

impl Default for Parser {
    /// Returns a parser with dimensions 80x24 and no scrollback.
    fn default() -> Self {
        Self::new(24, 80, 0)
    }
}

impl std::io::Write for Parser {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_utf8_does_not_swallow_text_controls_or_escapes() {
        for text in [
            "éxé",
            "é\n€",
            "é\x1b€",
            "你好 e\u{301} 👩\u{200d}💻 🇺🇳 क्षि",
            "\x1b]0;éxé\x07after",
            "\x1b]0;éxé\x1b\\after",
        ] {
            let bytes = text.as_bytes();
            let mut whole = Parser::new(8, 80, 100);
            whole.process(bytes);
            let expected = whole.screen().state_formatted();
            for first in 0..=bytes.len() {
                for second in first..=bytes.len() {
                    let mut split = Parser::new(8, 80, 100);
                    split.process(&bytes[..first]);
                    split.process(&bytes[first..second]);
                    split.process(&bytes[second..]);
                    assert_eq!(
                        split.screen().state_formatted(),
                        expected,
                        "{text:?}: cuts {first}, {second}"
                    );
                }
            }
        }
    }

    #[test]
    fn malformed_prefixes_do_not_hide_following_ascii_or_panic() {
        for prefix in [
            &b"\xc3\xff"[..],
            &b"\xf0\x80\xc3"[..],
            &b"\xe2\xc3\xc3\xc3"[..],
            &b"\xed\xa0\x80"[..],
            &b"\xf5\x80\x80\x80"[..],
            &b"\xe2\x1b[0m"[..],
        ] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(b"\r\nEND");
            for chunk in 1..=bytes.len() {
                let mut parser = Parser::new(8, 80, 100);
                for bytes in bytes.chunks(chunk) {
                    parser.process(bytes);
                }
                assert!(
                    parser.screen().contents().ends_with("END"),
                    "prefix={prefix:?}, chunk={chunk}"
                );
                assert_eq!(parser.utf8_remaining, 0);
            }
        }
    }

    #[test]
    fn suffix_tracking_distinguishes_complete_and_partial_scalars() {
        for c in ['é', '界', '💻'] {
            let mut buf = [0; 4];
            let bytes = c.encode_utf8(&mut buf).as_bytes();
            for len in 1..=bytes.len() {
                assert_eq!(
                    usize::from(incomplete_suffix(&bytes[..len])),
                    bytes.len() - len
                );
            }
        }
        for bytes in [&b"ascii"[..], &b"\x80\x80"[..], &b"\xff"[..], &b"\xf5"[..]] {
            assert_eq!(incomplete_suffix(bytes), 0);
        }
    }

    #[test]
    fn split_osc_preserves_title_payload_and_terminators() {
        #[derive(Default)]
        struct Titles(Vec<Vec<u8>>);
        impl crate::Callbacks for Titles {
            fn set_window_title(&mut self, _: &mut crate::Screen, title: &[u8]) {
                self.0.push(title.to_vec());
            }
        }
        for terminator in ["\x07", "\x1b\\"] {
            let input = format!("\x1b]2;éxé 界💻{terminator}after");
            let bytes = input.as_bytes();
            for first in 0..=bytes.len() {
                for second in first..=bytes.len() {
                    let mut parser = Parser::new_with_callbacks(8, 80, 0, Titles::default());
                    parser.process(&bytes[..first]);
                    parser.process(&bytes[first..second]);
                    parser.process(&bytes[second..]);
                    assert_eq!(
                        parser.callbacks().0,
                        vec!["éxé 界💻".as_bytes()],
                        "cuts {first}, {second}"
                    );
                    assert_eq!(parser.screen().contents(), "after");
                }
            }
        }
    }
}

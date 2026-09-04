// Vendored from doy/vt100-rust v0.16.2 tests/; see ../README.md.
use super::helpers;
use crate::ratty_vt as vt100;

#[test]
fn split_escape_sequences() {
    helpers::fixture("split_escape_sequences");
}

#[test]
fn split_utf8() {
    helpers::fixture("split_utf8");
}

#[test]
fn split_osc() {
    #[derive(Default)]
    struct Window {
        title: String,
        icon_name: String,
    }
    impl vt100::Callbacks for Window {
        fn set_window_icon_name(&mut self, _: &mut vt100::Screen, icon_name: &[u8]) {
            self.icon_name = std::str::from_utf8(icon_name).unwrap().to_string();
        }
        fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
            self.title = std::str::from_utf8(title).unwrap().to_string();
        }
    }

    let mut parser = vt100::Parser::new_with_callbacks(24, 80, 0, Window::default());
    for c in b"\x1b]0;a '[]_\x07" {
        assert_eq!(parser.callbacks().icon_name, "");
        assert_eq!(parser.callbacks().title, "");
        parser.process(&[*c]);
    }
    assert_eq!(parser.callbacks().icon_name, "a '[]_");
    assert_eq!(parser.callbacks().title, "a '[]_");
}

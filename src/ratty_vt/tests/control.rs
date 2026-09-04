// Vendored from doy/vt100-rust v0.16.2 tests/; see ../README.md.
use super::helpers;
use crate::ratty_vt as vt100;

#[test]
fn bel() {
    struct State {
        bel: usize,
    }

    impl vt100::Callbacks for State {
        fn audible_bell(&mut self, _: &mut vt100::Screen) {
            self.bel += 1;
        }
    }

    let mut parser = vt100::Parser::new_with_callbacks(24, 80, 0, State { bel: 0 });
    assert_eq!(parser.callbacks().bel, 0);

    let screen = parser.screen().clone();
    parser.process(b"\x07");
    assert_eq!(parser.callbacks().bel, 1);
    assert_eq!(parser.screen().contents_diff(&screen), b"");

    let screen = parser.screen().clone();
    parser.process(b"\x07");
    assert_eq!(parser.callbacks().bel, 2);
    assert_eq!(parser.screen().contents_diff(&screen), b"");

    let screen = parser.screen().clone();
    parser.process(b"\x07\x07\x07");
    assert_eq!(parser.callbacks().bel, 5);
    assert_eq!(parser.screen().contents_diff(&screen), b"");

    let screen = parser.screen().clone();
    parser.process(b"foo");
    assert_eq!(parser.callbacks().bel, 5);
    assert_eq!(parser.screen().contents_diff(&screen), b"foo");

    let screen = parser.screen().clone();
    parser.process(b"ba\x07r");
    assert_eq!(parser.callbacks().bel, 6);
    assert_eq!(parser.screen().contents_diff(&screen), b"bar");
}

#[test]
fn bs() {
    helpers::fixture("bs");
}

#[test]
fn tab() {
    helpers::fixture("tab");
}

#[test]
fn lf() {
    helpers::fixture("lf");
}

#[test]
fn vt() {
    helpers::fixture("vt");
}

#[test]
fn ff() {
    helpers::fixture("ff");
}

#[test]
fn cr() {
    helpers::fixture("cr");
}

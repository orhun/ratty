// Vendored from doy/vt100-rust v0.16.2 tests/; see ../README.md.
use super::helpers;
use crate::ratty_vt as vt100;

#[test]
fn intermediate_control() {
    helpers::fixture("intermediate_control");
}

#[test]
fn params() {
    let mut parser = vt100::Parser::default();
    parser.process(b"\x1b[::::::::::::::::::::::::::::::::@");
    parser.process(b"\x1b[::::::::::::::::::::::::::::::::H");
    parser.process(b"\x1b[::::::::::::::::::::::::::::::::r");
    parser.process(b"a\x1b[8888888X");
}

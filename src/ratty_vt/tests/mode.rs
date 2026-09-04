// Vendored from doy/vt100-rust v0.16.2 tests/; see ../README.md.
use super::helpers;

#[test]
fn modes() {
    helpers::fixture("modes");
}

#[test]
fn alternate_buffer() {
    helpers::fixture("alternate_buffer");
}

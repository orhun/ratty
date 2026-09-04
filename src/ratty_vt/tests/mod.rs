//! The upstream vt100 0.16.2 integration test suite, vendored from
//! `doy/vt100-rust` `tests/` so `cargo test --lib` runs it against the fork.
//!
//! Each file keeps its upstream content, with `vt100::` resolved to
//! [`crate::ratty_vt`] via a `use` alias, `mod helpers` turned into a path
//! import, fixture paths anchored at the crate root, and a `static mut`
//! replaced by an atomic. The `diff_crawl` tests are omitted because their
//! 30 MB fixture set is not vendored. Ratty-specific engine tests live next to
//! the code they cover, in `ratty_*_tests` modules.

mod helpers;

mod attr;
mod basic;
mod control;
mod csi;
mod escape;
mod init;
mod mode;
mod osc;
mod processing;
mod quickcheck;
mod scroll;
mod split_escapes;
mod text;
mod weird;
mod window_contents;
mod write;

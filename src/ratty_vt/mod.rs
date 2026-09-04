//! `ratty-vt`: ratty's VT state machine.
//!
//! This module is a fork of the [`vt100`](https://github.com/doy/vt100-rust)
//! crate by Jesse Luehrs, vendored from the `v0.16.2` tag (crates.io release
//! `vt100 0.16.2`, 2025-07-12) and carried in-tree so ratty can patch the
//! engine directly. The original is MIT licensed; the license text is kept
//! verbatim in `src/ratty_vt/LICENSE` and the upstream copyright notice
//! applies to everything under this directory. See `src/ratty_vt/README.md`
//! for the list of ratty-specific changes and the rebase notes.
//!
//! The engine parses a terminal byte stream and provides an in-memory
//! representation of the rendered contents. This is essentially the terminal
//! parser component of a graphical terminal emulator pulled out into a
//! separate module: it models the screen grid, scrollback, cell attributes,
//! cursor, and the input modes an application can toggle, and reports the
//! sequences it does not model through the [`Callbacks`] trait.
//!
//! # Synopsis
//!
//! ```
//! use ratty::ratty_vt as vt;
//!
//! let mut parser = vt::Parser::new(24, 80, 0);
//!
//! parser.process(b"this text is \x1b[31mRED\x1b[m");
//! assert_eq!(
//!     parser.screen().cell(0, 13).unwrap().fgcolor(),
//!     vt::Color::Idx(1),
//! );
//!
//! let screen = parser.screen().clone();
//! parser.process(b"\x1b[3D\x1b[32mGREEN");
//! assert_eq!(
//!     parser.screen().contents_formatted(),
//!     &b"\x1b[?25h\x1b[m\x1b[H\x1b[Jthis text is \x1b[32mGREEN"[..],
//! );
//! assert_eq!(
//!     parser.screen().contents_diff(&screen),
//!     &b"\x1b[1;14H\x1b[32mGREEN"[..],
//! );
//! ```

// The upstream code documents every `unwrap` with the invariant that makes it
// safe; keep that style rather than rewriting the engine around ratty's
// crate-wide lint. The remaining allows mirror upstream's own `lib.rs`, plus
// `collapsible_if`, which edition 2024 let-chains would otherwise force into a
// rewrite that makes rebasing onto upstream harder.
#![allow(clippy::unwrap_used)]
#![allow(clippy::cognitive_complexity)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::similar_names)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::type_complexity)]

mod attrs;
mod callbacks;
mod cell;
mod grid;
mod parser;
mod perform;
mod row;
mod screen;
mod term;

#[cfg(test)]
mod tests;

pub use attrs::{Blink, Color};
pub use callbacks::Callbacks;
pub use cell::Cell;
pub use parser::Parser;
pub use row::Row;
pub use screen::{MouseProtocolEncoding, MouseProtocolMode, Screen};

# ratty-vt

`ratty-vt` is ratty's VT state machine. It is a fork of
[`doy/vt100-rust`](https://github.com/doy/vt100-rust) at tag `v0.16.2`
(crates.io `vt100 0.16.2`, released 2025-07-12, upstream commit
`eb66ffaf7d77`).

The upstream crate is MIT licensed and © 2016 Jesse Luehrs. The license text
is kept verbatim in [`LICENSE`](./LICENSE) and applies to everything under this
directory, including ratty's modifications.

## Why a fork

Upstream has not merged patches since the 0.16.2 release. The features ratty
needs (SGR blink, HVP, resize reflow, borrowing accessors, kitty keyboard
state) were either open pull requests or absent, so ratty carries them here.

## Ratty-specific changes

Each change is a separate commit in ratty's history, tagged `ratty-vt:` in the
subject, so the fork can be rebased onto a future upstream release.

- Vendored as an in-package module (`ratty::ratty_vt`) rather than a crate.
- SGR 5 / 6 / 25 (slow blink, rapid blink, blink off) are parsed, stored on
  cells, and round-tripped by `contents_formatted`.
- SGR 8 / 28 (hidden), SGR 9 / 29 (strikeout), and SGR 58 / 59 (underline
  color) are parsed, stored on cells, and round-trip through the formatted
  output.
- `CSI f` (HVP) is handled like `CSI H` (CUP); `CSI s` / `CSI u` (SCOSC /
  SCORC) save and restore the cursor position and attributes like DECSC /
  DECRC.
- Grapheme clusters share one cell: a printed character that extends the
  previous cell's cluster (spacing vowel signs, VS16, ZWJ sequences, flag
  pairs, keycaps) joins it, and the cell's width is the cluster's
  `unicode-width` string width, matching how Ratatui lays out cells.
- Resize reflows wrapped lines, moves lines between the screen and scrollback,
  and resets the DECSTBM scroll region.
- Grids with a single row or column no longer panic on wide glyphs.
- `Screen::visible_row` and `Row` are public, O(1) borrowing accessors.
- Kitty keyboard protocol flags (`CSI > u`, `CSI < u`, `CSI = u`) and xterm
  `modifyOtherKeys` (`CSI > 4 ; n m`) are tracked with accessors.
- `Screen::display_cursor_position` and `Screen::cursor_hidden` resolve the
  cursor the way a renderer wants it (pending-wrap column, wide-cell snap,
  hidden while scrolled into history).
- Rows record whether they hold a kitty graphics Unicode placeholder.

## Tests

The upstream integration tests are vendored under `tests/` as a `#[cfg(test)]`
module so `cargo test --lib` runs them. The `crawl` fixture set (30 MB) is not
vendored; the two tests that consume it are omitted.

# ratty-vt

`ratty-vt` is Ratty's standalone terminal parser and screen-state crate. It has
no dependency on Ratty, Bevy, or a renderer. It is a fork of
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

- Maintained as the `ratty-vt` workspace crate, with public types available
  directly from `ratty_vt`.
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
  Zero-width marks attached after cursor movement also update the cell's
  width, so formatted output preserves its grid placement.
- Cell text uses `CompactString` so long emoji and combining sequences are
  preserved. On 64-bit targets a cell occupies 40 bytes, and text up to 24
  bytes stays inline; longer clusters use heap storage. Each cell is limited
  to 4096 UTF-8 bytes to bound memory and contextual Unicode processing.
  Excess zero-width marks are discarded; a following spacing character
  starts a new cell. Clustering checks the new boundary without allocating
  a copy of the growing cell.
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

Run the engine's tests independently from the repository root:

```sh
cargo test -p ratty-vt --locked
```

The upstream integration tests are vendored under `src/tests/` as a
`#[cfg(test)]` module. Their fixtures are included in the crate package, so
the tests also work from an unpacked release. The `crawl` fixture set (30 MB)
is not vendored; the two tests that consume it are omitted.

## Publishing

The crate has its own version. When releasing engine changes, bump this
crate's version and Ratty's `ratty-vt` version requirement together. Verify
all packages with `cargo package --workspace --exclude xtask --locked` from the
repository root; Cargo can verify their archives together before the new engine
version exists on crates.io.

`ratty-vt` depends on [`ratty-vte`](../ratty-vte/README.md) by path and
version. Publish `ratty-vte` first when it changed, then `ratty-vt`, before
releasing a Ratty version that depends on them:

```sh
cargo publish -p ratty-vte --locked
cargo publish -p ratty-vt --locked
```

The **Publish on crates.io** GitHub workflow also accepts `ratty-vt` as its
manual package selection. GitHub release events continue to publish `ratty`.
When Ratty uses an engine version that is already published, only Ratty needs
publishing again.

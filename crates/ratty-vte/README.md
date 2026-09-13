# ratty-vte

`ratty-vte` is Ratty's fork of the [`vte`](https://github.com/alacritty/vte)
ANSI escape sequence parser, based on vte 0.15.0. It exists so that parser
fixes can land directly rather than being worked around in `ratty-vt`.

## Differences from upstream

- A UTF-8 scalar split across `advance` calls no longer drops the bytes that
  follow it in the next buffer ([alacritty/vte#159]).
- A UTF-8 encoded C1 control (U+0080..=U+009F) is dispatched through `execute`
  whether or not its encoding is split across `advance` calls
  ([alacritty/vte#156]).
- The optional `ansi` module and its dependencies are not included. The
  `serde` feature is not included.
- Rust edition 2024 and the workspace's formatting.

Everything else, including the `Parser`, `Perform`, and `Params` API, matches
upstream. The original Apache-2.0 / MIT licensing is retained; see
`LICENSE-APACHE` and `LICENSE-MIT`.

[alacritty/vte#156]: https://github.com/alacritty/vte/issues/156
[alacritty/vte#159]: https://github.com/alacritty/vte/issues/159

## Development

```sh
cargo test -p ratty-vte --locked
cargo clippy -p ratty-vte --all-targets --locked -- -D warnings
```

## Publishing

`ratty-vt` depends on this crate by path and version, so publish `ratty-vte`
before `ratty-vt`, and `ratty-vt` before Ratty:

```sh
cargo publish -p ratty-vte --locked
```

The **Publish on crates.io** GitHub workflow also accepts `ratty-vte` as its
package input.

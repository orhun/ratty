# Contributing

Thanks for considering a contribution to **ratty**! 🐁

The goal is to keep changes easy to review and practical to integrate. If you
plan to make a larger change, open an issue first so the direction can be
discussed before implementation.

## Issues

- Search existing issues before opening a new one.
- Use issues for bugs, feature requests and discussions.
- If you are reporting a bug, include reproduction steps and environment details.

## Pull Requests

PRs are welcome. In general, please:

- Keep each PR focused on one change.
- Avoid mixing functional changes with broad cleanup or formatting-only edits.
- Add or update tests when the change affects existing behavior.
- Update documentation when needed.

For larger changes or breaking behavior, open an issue first.

## Development

1. Fork the repository and create a branch.
2. Build the project:

```bash
cargo build
```

For an optimized build that still rebuilds quickly after edits, use the
`release-fast` profile (the binary lands in `target/release-fast/`):

```bash
cargo run --profile release-fast
```

3. Run checks before opening a PR:

```bash
cargo check
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

If you change the widget crate, also check it directly:

```bash
cargo check --manifest-path widget/Cargo.toml --examples
```

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](./LICENSE).

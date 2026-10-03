# Contributing

Thanks for helping out. Bug reports, fixes and new features are all welcome.

## Before you start

- Build with the pinned toolchain (`rust-toolchain.toml`) and the committed `Cargo.lock`.
- For larger changes, open an issue first so we can agree on the approach.
- Read the docs for the area you touch; `docs/architecture.md` is a good starting point.

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Tests must run offline with synthetic data. SQLite tests use temporary databases. Never
put real Discord tokens, passwords or private messages in tests, issues, logs, screenshots
or CI.

## Guidelines

- Keep the boundaries between UI, model, protocol, storage and transport code clear.
- Bound item counts and byte sizes, and return explicit errors instead of panicking.
- Changes to sign-in, local storage or native dependencies should update the matching docs.
- Visible UI changes: include before/after screenshots from the demo mode
  (`./scripts/capture-screenshots.ps1`).
- Runtime changes: include before/after numbers for the same release workload.

## Dependencies and licenses

When dependencies change, run `cargo xtask licenses` and `node tests/license-policy.cjs`
(`cargo install cargo-deny --version 0.20.2 --locked` first). Exceptions in `deny.toml`
need a source and notice review when upgraded.

`cargo xtask fuzz` runs short offline fuzzing passes; see [fuzz/README.md](fuzz/README.md).

## License

Contributions are licensed under MIT OR Apache-2.0, like the rest of the project.

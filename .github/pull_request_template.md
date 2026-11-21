## What and why

<!-- What this changes and why. "Closes #N" for the issue it finishes. -->

## Components

<!-- The components it touches (docs/development/components.md); why, when more than one. -->

## Checks

- [ ] `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`
- [ ] Static build: `cargo clippy @StaticCargo --all-targets -- -D warnings`, `cargo test @StaticCargo -- --include-ignored` (or the `conversions` check)
- [ ] Results: unchanged, or the differences from `tests/data/cases.json` are listed below
- [ ] Command line: unchanged, or `interface` raised and `docs/reference/cli.md` updated
- [ ] GUI: looked at in the running window (when the window changes)
- [ ] `CHANGELOG.md` (what users see), `docs/development/decisions.md` (a design choice), `docs/development/architecture.md` and `components.md` (the code map)

## Result differences

<!-- Per case of tests/data/cases.json: what moved (counts, volume, checks), or "none". -->

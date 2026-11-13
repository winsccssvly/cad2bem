# Contributing

## Environment

Rust 1.99 or later. The conversion tests run on the static Gmsh/OpenCASCADE build, the Gmsh the executable
ships with: it carries corrections to Gmsh (see [docs/development/static-cad.md](docs/development/static-cad.md))
that a separately installed Gmsh SDK lacks, so a test there checks what users run. Build the static SDK once
with `scripts/build_static_cad.ps1`. `target/`, `out/` and `dist/` are not tracked.

## Before and after a change

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
. scripts/static_env.ps1
cargo clippy @StaticCargo --all-targets -- -D warnings
cargo test @StaticCargo -- --include-ignored
```

`scripts/static_env.ps1` sets up the session for the static build (SDK prefix, target directory, static CRT)
and defines `$StaticCargo`, the cargo flags of that build. The plain `cargo test --locked` needs no SDK or
display. A Gmsh SDK (`CAD2BEM_GMSH_LIBRARY`) still runs the default build, but quasi-structured quads
(`--patch-method qqs`, `--panel-method qqs`) may then differ from run to run. The models test converts every case of
[tests/data/cases.json](tests/data/cases.json) (the OC3 spar, the OC4 columns and the test bodies) and checks its state,
closure, volume and interior; a change that moves results updates the expectations there and says why in the
description of the change.

## Rules

- **Command line:** [docs/reference/cli.md](docs/reference/cli.md) is the agreement with programs that run
  cad2bem. A change to the commands, the options or the JSON result bumps its `interface`; fields may be added
  within an interface.
- **Geometry files:** HBG version 3, GDF and STL as described in the [user guide](docs/user-guide.md). Patch names
  `face_<N>` / `face_<N>.<k>` do not change.
- **Gmsh:** only inside worker processes, only through `cad::session::Session`.
- **Docs:** doc comments on every public item; module comments say what the module does.
- **Tests:** `tests/<topic>.rs`; tests that need the SDK are `#[ignore]` with the reason (or run in the static
  build). Test CAD lives in `tests/data/` (copies of the example models among it); no test reads `examples/`.
- **File headers:** new source code files (`.rs`, `.ps1`, `.py`, `.h`, `.wgsl`, `.rc`) start with the line
  `SPDX-License-Identifier: GPL-3.0-or-later`, with no copyright line: [NOTICE](NOTICE) holds the copyright.
  Documents and configuration files carry none; NOTICE puts every file that states nothing else under the
  GPL-3.0-or-later.
- **Dependencies:** a new dependency is listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) with its use
  and licence; it must be compatible with GPL-3.0.
- **User-visible changes** go into [CHANGELOG.md](CHANGELOG.md).

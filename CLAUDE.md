# cad2bem: notes for Claude Code

@AGENTS.md

Read [docs/development/decisions.md](docs/development/decisions.md) too before changing a design choice. Machine
paths and a maintainer's own notes go in `CLAUDE.local.md` (not tracked).

## Checks

- Every change: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`.
- Conversions (the ignored tests, among them `tests/models.rs` with the cases of `tests/data/cases.json`) run on
  the static build, whose Gmsh carries the build script's corrections. In PowerShell:
  `. scripts/static_env.ps1`, then `cargo clippy @StaticCargo --all-targets -- -D warnings` and
  `cargo test @StaticCargo -- --include-ignored`. An unpatched Gmsh SDK gives quasi-structured quads that move
  from run to run.
- A change that moves results runs `tests/models.rs` and states the differences from `tests/data/cases.json` in its
  description; a GUI change is looked at in the running GUI (`cad2bem gui ... --screenshot FILE.png` for a
  scripted look).

## Rules

- The command line is a contract with other programs ([docs/reference/cli.md](docs/reference/cli.md)): changing a
  command, an option or the JSON result raises `interface`; an added field keeps it. Patch names
  (`face_<face>`, `face_<face>.<k>`) do not change.
- Output: the body as HBG version 3, optionally with the interior surface after the body in the same file (z = 0);
  panels also as GDF and STL; spline patches only as HBG. A solver's format is never the internal model.
- Gmsh runs only in worker processes; the GUI and the command line share one request, job and publisher.
- Every tracked file is public. No machine paths, user names, e-mail addresses or private notes in tracked files;
  CAD files are checked for author, organisation and path in their STEP header. CAD made with software whose
  licence restricts the use of its files (a student edition, for example) is never added.
- New source code files (`.rs`, `.ps1`, `.py`, `.h`, `.wgsl`, `.rc`) start with the header line
  `SPDX-License-Identifier: GPL-3.0-or-later` (as a comment), with no copyright line; documents and configuration
  files carry none (`NOTICE` holds the copyright and covers every file that states nothing else). A new dependency
  goes in `THIRD_PARTY_NOTICES.md` with its licence, which must be compatible with GPL-3.0.
- Tracked files, code, comments and commit messages are in English; line endings LF (`.gitattributes`).
- Public items carry doc comments; a module comment says what the module does.
- A design choice gets its reason in [docs/development/decisions.md](docs/development/decisions.md); user-visible
  changes go in `CHANGELOG.md`; the code map in `docs/development/architecture.md` follows the code.
- Tests read `tests/data/`, never `examples/`.

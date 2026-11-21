# Release checklist

A release is a tag `vX.Y.Z` on `main` and a GitHub release with three files: the program folder as a zip, the
executable on its own, and the corresponding source as a zip. Versions follow semantic versioning; before 1.0
a minor version may change the command line, and then `interface` in the JSON result changes with it
([cli.md](../reference/cli.md)).

## 1. Prepare the version

In a pull request (`release/X.Y.Z`):

- `Cargo.toml`: `version = "X.Y.Z"`; `cargo build` updates `Cargo.lock`.
- `CHANGELOG.md`: rename `Unreleased` to `X.Y.Z (YYYY-MM-DD)` and start a new empty `Unreleased`.
- `README.md`: the file names in the download section, when they name the version.

Merge it when `ci` and `conversions` pass.

## 2. Build from the merged commit

On a clean checkout of `main` at the merge commit (no uncommitted changes: the packaging script refuses
them), with the static SDK built (`scripts/build_static_cad.ps1`):

```powershell
. scripts/static_env.ps1
cargo test @StaticCargo -- --include-ignored
cargo build @StaticCargo --bin cad2bem
scripts/package_release.ps1
```

`dist/` then holds `cad2bem-X.Y.Z-windows-x64` (and its zip), `cad2bem-X.Y.Z-source` (and its zip) and
`cad2bem-X.Y.Z-windows-x64.exe`.

## 3. Check the result

- The import table holds only Windows system DLLs (`scripts/verify_static_cad.ps1`, or `dumpbin /dependents`).
- In a fresh folder outside the repository, from the unpacked zip: convert an example in each mode (the
  commands are in [examples/README.md](../../examples/README.md)), open the GUI, the About window, and
  `cad2bem licenses --write licenses-check`.
- The executable holds no path, user name or e-mail address of the build machine (search its strings).
- The source zip builds: it is the committed tree of the merge commit (`git archive`) with the pinned native
  archives (their hashes are those of `scripts/native-sources.json`) and the vendored crates.

## 4. Publish

```powershell
git tag -a vX.Y.Z -m "cad2bem X.Y.Z"
git push origin vX.Y.Z
gh release create vX.Y.Z --title "cad2bem X.Y.Z" --notes-file NOTES.md `
  dist/cad2bem-X.Y.Z-windows-x64.zip dist/cad2bem-X.Y.Z-windows-x64.exe dist/cad2bem-X.Y.Z-source.zip
```

The notes are the version's section of `CHANGELOG.md`. Download the three files again and compare them with
`dist/` byte for byte.

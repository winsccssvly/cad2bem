<img src="resources/logo.svg" width="96" alt="cad2bem logo" align="right">

# cad2bem

A CAD preprocessor for boundary element methods: one executable, with a GUI and a command line, that turns a
STEP or IGES model into the surface geometry of a floating or submerged body. Written in Rust (egui/eframe and
wgpu for the GUI); CAD import, Booleans and surface meshing use Gmsh and OpenCASCADE through Gmsh's C API.

Three conversion modes:

- **Free panels (`lo-free`):** low-order panels meshed on the CAD faces. Mostly quadrilaterals, normals into
  the fluid, a closed mesh in which neighbouring faces share their nodes. GDF or HBG.
- **Structured panels (`lo-str`):** panels laid on grids of the spline patches, so they follow the patch layout
  and, on ruled faces, its straight lines. GDF or HBG.
- **Spline patches (`ho`):** four-sided B-spline patches (rational included) for higher-order methods. Pieces of
  planes, cylinders, cones, discs, rings, spheres and tori become exact rational patches. HBG only.

`--interior` also writes the interior free surface (the section z = 0 of the body) in the same file after the
body, for the removal of irregular frequencies. Nothing is cut or removed unless asked: the cut at the waterline
(`--cut-waterline`), at the symmetry planes of a whole model after a symmetry check (`--symmetry`) and the
removal of sheets (`--remove-sheets`). Orientation, closure, gaps, overlaps, volumes and the patch quality are
checked; a result that fails is never written.

```bash
cad2bem convert hull.step --mode lo-free --panel-size 1 --cut-waterline --symmetry xz --out out/hull.gdf
cad2bem convert hull.step --mode ho --patch-size 6 --cut-waterline --symmetry xz --interior --out out/hull.hbg
cad2bem gui --open hull.step
```

`cad2bem.exe` opened from Explorer starts the GUI without a console window (older than Windows 11 24H2: one
flashes and closes).

## Download and run

Windows 10 or 11 (x64). Download `cad2bem-VERSION-windows-x64.zip` from the
[releases](../../releases) of this repository, unzip it anywhere and open `cad2bem.exe`: there is nothing to
install, and Gmsh and OpenCASCADE are inside the executable. `cad2bem-VERSION-windows-x64.exe` is the same
executable on its own, without the documents and examples of the zip; it carries its licences (**About**, or
`cad2bem licenses`). `cad2bem-VERSION-source.zip` is the source of the release. The executable is not signed, so Windows SmartScreen
may say it protects your PC: choose **More info**, then **Run anyway**.

To start, open one of the models in `examples/` (in the zip), for example `examples/column/column.step`, and follow
the [GUI manual](docs/gui-manual.md); the [user guide](docs/user-guide.md) explains the conversion and the
[command line reference](docs/reference/cli.md) every option. A problem: **Save diagnostics** in the toolbar saves
what is needed to look at it.

## Build

Rust 1.99 or later. The default build loads a Gmsh 4.15 SDK at run time (`CAD2BEM_GMSH_LIBRARY`, the path of
`gmsh-4.15.dll` or `libgmsh.so`); the `static-gmsh` build links Gmsh and OpenCASCADE into the executable
([static CAD backend](docs/development/static-cad.md)).

```bash
cargo build --locked --release
```

The executable is `target/release/cad2bem` (`cad2bem.exe` on Windows).

## Documentation

| Document | Contents |
|---|---|
| [docs/user-guide.md](docs/user-guide.md) | Setup, conversion, units, symmetry, sheets, checks, output files |
| [docs/gui-manual.md](docs/gui-manual.md) | The GUI step by step, with screenshots |
| [examples/](examples/README.md) | Example CAD models and the command for each conversion |
| [docs/reference/cli.md](docs/reference/cli.md) | Command line: commands, options, exit codes, JSON result, check limits |
| [docs/development/architecture.md](docs/development/architecture.md) | Pipeline, code map, conventions, tests, pitfalls |
| [docs/development/decisions.md](docs/development/decisions.md) | Design decisions and the reason for each |
| [CHANGELOG.md](CHANGELOG.md) | Changes by version |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Environment, checks, rules |

## Licence

cad2bem is free software under the GNU General Public License, version 3 or later (GPL-3.0-or-later;
[LICENSE](LICENSE), [NOTICE](NOTICE)). It uses Gmsh (GPL-2.0-or-later) and OpenCASCADE (LGPL-2.1 with an
exception); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

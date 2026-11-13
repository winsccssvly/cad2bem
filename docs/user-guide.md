# User guide

## 1. Setup

Build the executable with Rust 1.99 or later (`cargo build --locked --release`; it is
`target/release/cad2bem`). The default build loads a Gmsh 4.15 SDK at run time: set `CAD2BEM_GMSH_LIBRARY` to
the path of `gmsh-4.15.dll` (Windows) or `libgmsh.so` (Linux), or put the library next to the executable. The
SDK of the Gmsh releases, or the library of a conda-forge `gmsh` package, will do. The `static-gmsh` build links
Gmsh and OpenCASCADE into the executable and needs nothing at run time
([static CAD backend](development/static-cad.md)).

## 2. The GUI

```bash
cad2bem gui --open examples/column/column.step
```

The [GUI manual](gui-manual.md) walks through the window: preparing the CAD (unit, waterline, symmetry, extra
faces, face cuts), converting, reading the checks, exporting (HBG, GDF, STL), the Mesh Viewer, Fast Mesh and
saving diagnostics.

`cad2bem.exe` opened from Explorer starts the GUI, and a CAD file dropped on its icon (or given alone on the
command line, or dropped on the open window) opens. On Windows 11 24H2 and later the GUI has no console window;
on older Windows a console window flashes and closes as the GUI starts again without it. In a terminal the same
executable is the command line: the shell waits for it and reads its output. Without a console, a GUI that cannot
start shows why in a message box. `cad2bem gui --open FILE` with the conversion options of section 3 opens with
them applied; the [command line reference](reference/cli.md#gui) lists the GUI options.

## 3. Conversion

```bash
cad2bem convert examples/column/column.step --mode lo-free --panel-size 2 --cut-waterline --symmetry xz_and_yz --interior --out out/column.gdf
```

writes `out/column.gdf` (the body and, after it, the interior surface), and ends with `ok: N panels; S s`. For a model of your own,
`hull.step`:

```bash
cad2bem convert hull.step --mode lo-free --panel-size 1 --cut-waterline --out out/hull_panels.gdf
cad2bem convert hull.step --mode lo-str --patch-size 6 --panel-size 1 --cut-waterline --out out/hull_grid.hbg
cad2bem convert hull.step --mode ho --patch-size 6 --cut-waterline --interior --out out/hull.hbg
cad2bem convert hull.step --mode lo-free --panel-size 1 --out out/hull_whole.hbg
```

The last one meshes the whole hull, above the water too, for a solver that places and cuts the body itself.
Panels go to GDF (the low-order format of WAMIT) or HBG; spline patches to HBG only. Patch files carry the
potential panel size as `panel_size` (`--potential-panel-size`, GUI: **Potential panels**; default 2 m), and
the solver divides each patch into spans of about that size, so a file opens without a discretization from its
reader. **Spans** in the GUI shows that division and follows a new size at once; the size is only that member of
the files, so **Export mesh…** writes the file now without converting again.

`--interior` writes the interior free surface, the section z = 0 of the body cut at the waterline (so it needs
`--cut-waterline`), in the same file after the body, of the body's kind: the solver's interior
surface "detect" takes the elements lying in z = 0 as it. It comes from the file's solids; a
file without solids (IGES, a STEP shell) gets it from the waterlines of its wetted faces, closed along the
symmetry planes; a loop inside another (a moonpool) is a hole. It is not part of the body checks.

`--json` prints the result as JSON for programs ([command line](reference/cli.md)). Exit code 0 means the files
were written, with state `ok` or `warn`; a result that fails its checks is not written.

### Modes

- **lo-free:** each prepared CAD face is meshed with quadrilaterals at `--panel-size`; neighbouring faces share
  their nodes; non-convex quadrilaterals are split into triangles.
- **ho:** each face becomes four-sided B-spline patches at about `--patch-size`: an O-grid or ring for smooth
  discs and annuli, the face's own surface when it is untrimmed (its stored B-spline poles when the file has
  them), one Coons patch of four trimmed sides, two of a five- or six-sided face split across, N patches of an
  N-sided face, or else quads of a mesh of the face alone (its own edge sizes, so neighbours do not change it). Patches on planes, cylinders, cones, spheres and tori become exact rational
  patches. `--patch-method` picks the quad mesher of those split faces.
- **lo-str:** the ho patches divided on grids of their parameters at `--panel-size`. Where a patch corner meets
  the middle of another patch's edge (a T-junction) the panels meet with hanging nodes (the solver takes them;
  cutting the patch there doubled the panels); opposite edges share one count and a shared edge one count on both sides, except where a straight shared edge would need
  counts more than 1.5 times apart (hanging nodes there). Panels on planes, cylinders and cones are flat.

### Length units

A STEP or IGES file states the unit of its numbers and they are read in it, converted to metres (`--unit
file`). If the stated unit is wrong (numbers in metres stated as millimetres, as some exporters write), give the
real one: `--unit m`. The `read` stage reports the stated unit and the body size, and the JSON result has them
in `cad`.

### Symmetry

`--symmetry` names the symmetry planes of the body (`yz`: x = 0, `xz`: y = 0, `xz_and_yz`: both vertical planes, the names the HBG file uses). They
are written into the file and the solver reflects the mesh in them, so only a half or a quarter is written.

- **A whole model** (it reaches across the plane) is checked for symmetry (each side mirrored lies on the other,
  with the same area) and cut, keeping x ≥ 0, y ≥ 0. A model that is not symmetric is refused with the plane
  and the distance found; within 1e-4 of the size it passes.
- **A model already cut** (a half or quarter on x ≥ 0, y ≥ 0) is taken as it is. A half on the negative side is
  taken for a whole model and refused.

Either way the faces on the planes are removed (a CAD half is often closed there by a plane face).

### Sheets

A sheet is a face that is not part of the closed body: a deck plate, a fin, a duplicate, a surface past the one
it meets. `--remove-sheets rules` removes them after the fragment; `--remove-sheets largest` keeps only the
largest connected group of faces (a model with loose parts). Without it every face is kept, and the
`orientation` check names faces that look like sheets.

### Partitioning faces

A face that no four-sided layout fits (a column wall with a pipe hole, the trimmed end of a pipe) is meshed into
many small patches. `--cuts FILE.json` splits faces with planar rectangles before the patches are made
([command line](reference/cli.md#cuts)), so that such a face becomes several simpler pieces. Placing the cuts
well matters: a thin strip left between two cuts can come out worse than the face it was cut from. In the GUI,
cuts are drawn in the CAD tab and **Pipe junctions: Cut all** adds the junction cuts
([GUI manual](gui-manual.md#cutting-faces)).

### Checks

A conversion whose checks fail is still shown: its failed patches are red and listed first in Checks (hover a
check for what it means), and it cannot be exported. Select a failed patch to find its face, then cut that face
or change its Layout and convert again. A saved prepared CAD is a half or quarter model when it was cut at the waterline
or at symmetry planes: open it with the same **Symmetry** and **Waterline: Cut at z = 0**. The CAD tab says when a model is
open along y = 0, x = 0 or z = 0 and its settings do not name that plane, and **Turn on the matching settings** sets them.

Every patch and the whole body are checked; each check is `ok`, `warn` or `fail` (limits in the
[command line](reference/cli.md#check-limits)).

| Check | What it means | What to try |
|---|---|---|
| `closed`, patch `gap` | Faces do not meet: a hole or a face removed that belonged to the hull (a warning: the solver takes it, the size is in the report) | The symmetry; no `--remove-sheets` |
| `volume` | The volumes by x, y and z differ (the surface is not closed) or the patches differ from the CAD solids | As for `closed` |
| `orientation` | A face looks like a sheet, or the side of a face is unsure | `--remove-sheets rules` |
| `waterline` | The body, cut at the waterline, is above z = 0 | Move the model: the waterline must be z = 0 |
| patch `jacobian`, `corner_angle` | A patch is strongly distorted (a sphere's pole always warns) | A smaller `--patch-size`, another `--patch-method` |
| patch `cad_error` | The patch is off the CAD face | A smaller `--patch-size` |
| patch `fold` | The patch folds over itself | Another `--patch-method`, CAD preparation |

Flat panels lose some volume on curved faces (the chord error): about (2π h / C)² / 6 of it for panels of size h
on a circle of circumference C, a few per cent at 2 m panels on a 6 m column.

## 4. Output files

**HBG** (version 3) is JSON: `kind` `panels` (`vertices`, quadrilaterals with a repeated fourth vertex for
triangles) or `spline_patches` (`patches` with `name`, orders, knots, control points and, for rational patches,
`weights`), the symmetry (`none`, `yz` for x = 0, `xz` for y = 0, `xz_and_yz` for both), and normals into the
fluid. A reader of version 3 refuses files of other versions: convert again to update an older file.
**GDF** is the WAMIT text format: a header with the length scale and gravity, the symmetry flags, the panel
count, then four vertices per panel. Both hold the engine's panels exactly (GDF at its text precision).
**STL** (low order only) is ASCII triangles for viewing and other tools: the whole body with its symmetry images
(STL has no symmetry), a quadrilateral as two triangles, facet normals out of the body, and the interior surface
as a second solid `interior_surface`. The Mesh Viewer reads STL files, ASCII or binary, as triangles of the
whole surface.

Every file holds the body and, with `--interior`, the interior surface after it: its elements lie in z = 0,
where the solver's interior surface "detect" finds them.

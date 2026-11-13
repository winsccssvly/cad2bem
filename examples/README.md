# Examples

One folder per CAD model. Each command below converts it with the `cad2bem` executable; run them from the
repository root. Outputs go to `out/` (not tracked); with `--interior` the interior surface is in the same file
after the body.

| Folder | CAD file |
|---|---|
| [column](column/) | `column.step`: a column on a pontoon, a quarter (STEP solid) |
| [moonpool](moonpool/) | `moonpool.step`: a cylinder with a moonpool, a quarter |
| [wigley](wigley/) | `wigley.igs`: a Wigley hull as loose IGES surfaces, a half, with a raked transom plate |
| [oc3](oc3/) | `OC3.STEP`: the OC3 spar, the whole body |
| [oc4](oc4/) | `OC4_only_column.STEP`: the columns of the OC4 semisubmersible without pontoons and braces, the whole body |

## Commands

```bash
# column: spline patches, free panels and structured panels
cad2bem convert examples/column/column.step --mode ho --patch-size 6 --cut-waterline --symmetry xz_and_yz --interior --out out/column_ho.hbg
cad2bem convert examples/column/column.step --mode lo-free --panel-size 2 --cut-waterline --symmetry xz_and_yz --interior --out out/column_lo_free.gdf
cad2bem convert examples/column/column.step --mode lo-str --patch-size 6 --panel-size 2 --cut-waterline --symmetry xz_and_yz --interior --out out/column_lo_str.gdf

# moonpool: the moonpool is a hole in the interior free surface
cad2bem convert examples/moonpool/moonpool.step --mode ho --patch-size 4 --cut-waterline --symmetry xz_and_yz --interior --out out/moonpool_ho.hbg

# wigley: loose surfaces; the transom plate and the hull past it are removed as sheets
cad2bem convert examples/wigley/wigley.igs --mode ho --patch-size 10 --cut-waterline --symmetry xz --interior --remove-sheets rules --out out/wigley_ho.hbg

# oc3: the whole spar, cut to its wetted quarter by the symmetry check
cad2bem convert examples/oc3/OC3.STEP --mode ho --patch-size 8 --cut-waterline --symmetry xz_and_yz --interior --out out/oc3_ho.hbg
cad2bem convert examples/oc3/OC3.STEP --mode lo-free --panel-size 2 --cut-waterline --symmetry xz_and_yz --interior --out out/oc3_lo_free.gdf
cad2bem convert examples/oc3/OC3.STEP --mode lo-str --patch-size 8 --panel-size 1 --cut-waterline --symmetry xz_and_yz --interior --out out/oc3_lo_str.gdf

# oc4: the four columns, wetted (symmetric about y = 0 only: cut there in lo-free)
cad2bem convert examples/oc4/OC4_only_column.STEP --mode ho --patch-size 6 --cut-waterline --interior --out out/oc4_ho.hbg
cad2bem convert examples/oc4/OC4_only_column.STEP --mode lo-free --panel-size 1.5 --cut-waterline --symmetry xz --interior --out out/oc4_lo_free.gdf
cad2bem convert examples/oc4/OC4_only_column.STEP --mode lo-str --patch-size 6 --panel-size 1 --cut-waterline --interior --out out/oc4_lo_str.gdf
```

The GUI does the same: `cad2bem gui --open examples/column/column.step`, then set the mode and options in the
**2 Mesh** tab. Add `--json` to a command for the checks as JSON; the `state` (ok, warn, fail) and the body checks
say whether the result is fit for a solver.

## A geometry file to view

[barge_gdf](barge_gdf/) is not CAD: `barge.gdf` is a higher-order GDF file (IGDEF 1) of the box barge, 60 x 20 x 10 m,
as four flat B-spline patches of the quarter x >= 0, y >= 0 (the bottom, two sides, and the interior surface at
z = 0 after them), and `barge.spl` gives each patch its spans for a 2 m potential panel size, its orders and Gauss
orders. Open either file in the Mesh Viewer:

```bash
cad2bem gui --open examples/barge_gdf/barge.spl
```

It shows the volume 12,000 m3 and the interior area 1,200 m2 exactly, and **Spans** draws the 2 m division.

## Your own model

Start from the command of the closest example with your STEP or IGES file. The file's own length unit is used;
`--unit m` (or mm, cm, in, ft) replaces a wrong one. Nothing is cut or removed unless asked: `--cut-waterline`
cuts at z = 0 and keeps the wetted surface; `--symmetry` names the body's symmetry planes (a whole model is cut
there after a symmetry check, a model already cut is taken as it is); `--remove-sheets rules` removes faces that
are not part of the closed body (plates, parts past an intersection).

## About the models

The column, moonpool and Wigley files are test bodies generated for this project. The OC3 and OC4 models were drawn
by the author in CAD from the published dimensions of the IEA Wind OC3 and OC4 reference platforms (Jonkman,
*Definition of the Floating System for Phase IV of OC3*, NREL/TP-500-47535, 2010; Robertson et al., *Definition of
the Semisubmersible Floating System for Phase II of OC4*, NREL/TP-5000-60601, 2014). The STEP files state
millimetres; the conversion works in metres with z = 0 the waterline. OC3 has a 120 m draft, 6.5 m upper and 9.4 m
lower diameter, the taper from z = -4 m to -12 m. The OC4 columns are the 6.5 m main column and three offset
columns, 12 m wide, 24 m at their base below z = -14 m, 28.87 m from the centre; they extend from z = -20 m to
+12 m. Both models were drawn in FreeCAD; the OC4 file is an assembly of the main column and three placed copies
of one offset column.

Known limits: complex trimmed junctions (pontoons and braces meeting columns) can need CAD preparation or explicit
face partitions for good element quality: spline patches there carry corner-angle and Jacobian warnings, and
structured panels keep hanging nodes at T-junctions (kept, not cut). Inspect the check states before using a file
in a solver.

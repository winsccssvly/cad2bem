# Command line (interface 3)

```text
cad2bem                                  Open the GUI
cad2bem gui [--open FILE] [OPTIONS]      Open the GUI, optionally with a CAD or geometry file
cad2bem convert CAD --out FILE.hbg|FILE.gdf|FILE.stl [OPTIONS]
cad2bem version
cad2bem licenses [--write FOLDER]
cad2bem --help
```

`convert`, `version`, `licenses` and `--help` never start graphics. `licenses` prints the licence of cad2bem and the
licence files of every component the executable carries (Gmsh with its bundled code, LEMON, OpenCASCADE, the Rust
crates); `--write FOLDER` writes them as files into a new folder. The GUI shows the same files under **About**. Programs should use `convert --json`.

## convert

| Option | Meaning |
|---|---|
| `--mode lo-free\|lo-str\|ho` | Free panels on the CAD faces (default), structured panels on the patch grids, or spline patches |
| `--out PATH` | Required; `.hbg`, `.gdf` or `.stl` (ho: `.hbg` only; STL: see [output files](../user-guide.md#4-output-files)). An existing path is refused before conversion |
| `--panel-size METRES` | Panel size for lo-free and lo-str; default 2; refused by ho |
| `--patch-size METRES` | Patch size for ho and lo-str; default 8; refused by lo-free |
| `--potential-panel-size METRES` | ho: written to the HBG file as `panel_size`, from which the solver sets the spans of every patch (the longer edge arc over the size, plus one); default 2, so every ho file is complete without a discretization from its reader; refused by lo-free and lo-str |
| `--unit file\|m\|mm\|cm\|in\|ft` | The unit of the file's numbers; default `file`, the unit the file states |
| `--cut-waterline` | Cut at z = 0 and keep the wetted surface |
| `--symmetry none\|xz\|yz\|xz_and_yz` | Symmetry planes: `xz` y = 0, `yz` x = 0, `xz_and_yz` both (the names of HBG); default none |
| `--interior` | Also write the interior free surface, in the same file after the body ([output files](../user-guide.md#4-output-files)); needs `--cut-waterline` |
| `--remove-sheets none\|rules\|largest` | Remove faces that are not part of the closed body by rules, or keep only the largest connected group of faces; default none |
| `--panel-method recombined\|qqs\|quads\|triangles` | lo-free mesher: mostly quadrilaterals (default), quasi-structured quadrilaterals, all quadrilaterals, or triangles. When the quasi-structured mesher fails on a model (Gmsh can fail, or crash, in its topology optimisation), the conversion runs again with the default mesher and says so on stderr and in `summary.panel_method_fallback` |
| `--patch-method tri3\|qqs\|subdiv` | Quad mesher of faces that are split into quads (ho, lo-str); default tri3 |
| `--cuts FILE.json` | Planar cuts that partition faces before meshing (see below) |
| `--faces FILE.json` | ho, lo-str: settings of single prepared faces (see below) |
| `--json` | The result as one JSON object on stdout; progress and errors stay on stderr |

Sizes must be positive and finite. A conversion runs in a private worker process of the executable; the build
that does not link Gmsh statically loads the Gmsh SDK named by `CAD2BEM_GMSH_LIBRARY`, or finds it next to the
executable, in an active conda environment or on the library path.

Progress goes to stderr as `[SECONDS] STAGE` lines (`faces`, `read`, `orient`, `mesh`, `checks`, `interior`,
`validate`, `complete`). Without `--json`, stdout ends with `STATE: N panels; S s` (or patches) and a
`Wrote PATH` line (one file).

### Cuts

`--cuts` takes a JSON array of rectangles, in metres of the model after import:

```json
[{"centre": [-4.9, 0, -17], "normal": [1, 0, 0], "axis": [0, 1, 0], "half": [1.1, 1.1]}]
```

`axis` is the first in-plane direction (perpendicular to `normal`); the second is normal × axis; `half` holds
the half sizes along them. After the waterline and symmetry cuts, every face whose bounding box meets a
rectangle is split along their intersection. A cut should cross the faces it meets from boundary to boundary.
An optional `on` limits a cut to the faces lying on one surface, `{"cylinder": {"point", "axis", "radius"}}` or
`{"plane": {"point", "normal"}}`, so that a cut can pass through the point where that surface meets another
(a column wall at the corner of a pipe junction) without cutting the other. An optional `face_at` (a point,
`[x, y, z]`) limits a cut to the one face through that point, as the faces are when the cut is made.
`cad.cuts` in the JSON result lists how many faces each cut split and the cuts that split none.

### Face settings

`--faces` takes a JSON array, one entry per prepared face (lo-free: `panel_size` and `priority`; ho and lo-str:
`path`, `patch_size` and `priority`) (the numbers of the prepared view and of the patch
names):

```json
[{"face": 39, "path": "split", "patch_size": 3, "priority": 1}]
```

`path` forces a layout (`auto`, `direct`, `single`, `polygon`, `ogrid`, `ring`, `split`); a forced layout that
does not fit splits the face and is listed in `cad.face_settings.forced_path_failed`. `patch_size` is the patch
size of the face when it is split. Where faces share a curve, the one with the higher `priority` sets the size
of its mesh. Unknown face numbers are listed in `cad.face_settings.faces_unknown`.

### Exit codes and files

Exit code 0 means every file was written, `ok` or `warn`. Invalid options, a missing SDK, a conversion whose
checks fail, an asymmetric body for `--symmetry`, and output errors exit nonzero with a message on stderr, and
write nothing. Ctrl+C cancels and reaps the worker.

The worker writes and reads back both formats in a private directory before anything is published: HBG keeps
the engine's coordinates exactly, GDF is checked at its text precision; counts, normals, symmetry and the three
volumes are compared on read-back. One file is published, the interior surface after the body; it never
overwrites an existing file.

### JSON result

| Key | Contents |
|---|---|
| `interface` | 3 (2 named both symmetry planes `xy`; 1 wrote the interior surface to a second file, `NAME.interior.EXT`) |
| `mode` | `lo-free`, `lo-str` or `ho` |
| `state` | `ok`, `warn` (a check warns; the files are written) |
| `engine_version` | The executable's version |
| `summary` | Counts (`panels`, `quadrilaterals`, `triangles` or `patches`), sizes, `seconds`, `state`, `interior` (its state, area and checks); `panel_method_fallback` (`requested`, `used`, `reason`) when the quasi-structured mesher failed and the default one was used |
| `body` | Body checks: `volume` (`volumes_m3` by x, y and z, `volume_cad_m3`, `spread_rel`, `cad_difference_rel`), `closed`, `orientation`, `waterline` |
| `cad` | The file: `file_unit`, `body_size_m`, `faces_imported`, cuts, symmetry, sheets removed |
| `patch_checks` | ho: every body patch with its checks, in the order of the body HBG; `discretization` (the spans at the potential panel size) and `edge_lengths_m` (the longer edge arc length in u and v, from which the spans follow for any size) |
| `interior_patch_checks` | ho with the interior surface: every interior patch with its checks, in the order they follow the body's patches in the HBG |
| `files` | `role` (`body`) and `path` of the written file (one) |
| `seconds` | Total time |

No mesh arrays or private paths are printed. Patch names are `face_<N>` or `face_<N>.<k>`, with `N` the face
number after the cuts.

## gui

`gui` takes the conversion options above and:

| Option | Meaning |
|---|---|
| `--open FILE` | Open a STEP or IGES file, or a geometry file (`.hbg`, `.gdf`, the `.spl` of a higher-order GDF, or `.stl`) in the **Mesh Viewer** tab |
| `--gmsh-library PATH` | Gmsh SDK for this session (dynamic builds) |
| `--convert` | Convert the opened file with the given options |
| `--export FILE` | Publish the converted result (needs `--convert`; existing files stay protected), or with `--final-tab fast` write the Fast Mesh body |
| `--show-interior` | Fail unless the shown result has an interior surface (needs `--convert --interior`, or a geometry file to open; the GUI shows it by default) |
| `--show-spans` | Show the lines between the recommended potential spans of spline patches |
| `--mesh-size METRES` | Diagnostic: an untrimmed triangle mesh of the whole file at this size, with triangle quality |
| `--screenshot FILE.png` | Save the window once the jobs finish, then exit; fails after 90 s or on an error |
| `--pick-centre` | Fail unless a ray through the view centre picks a face |
| `--diagnostics FOLDER` | Open saved diagnostics (**Save diagnostics**): its CAD file, mode and settings |
| `--write-diagnostics` | Smoke: save the diagnostics before the screenshot |
| `--click-cut` | Smoke: with `--cut-mode`, add the cut under the view centre and wait for the views |
| `--then-convert` | Smoke: convert once the CAD view is shown (as a click would), not at start |
| `--final-tab TAB` | Smoke: show `cad`, `mesh`, `view` (the Mesh Viewer) or `fast` before the screenshot, and print the viewport and camera |
| `--fast-shape SHAPE` | Smoke: the shape of the Fast Mesh tab (`cylinder`, `moonpool`, `sphere`, `hemisphere`, `barge`) with its usual dimensions |
| `--fast-high-order` | Smoke: make the Fast Mesh body as exact spline patches (high order) |
| `--fast-triangles` | Smoke: make the Fast Mesh body of triangles only (low order) |
| `--about` | Open the About window (the version, the licence and the licence files of the components) |
| `--write-junction-cuts FILE` | Smoke: write the pipe-junction cuts of the first CAD view to FILE and exit |
| `--cut-mode MODE` | `across`, `along`, `points` or `parallel`: Start in the cut mode; a smoke run shows the cut under the view centre |
| `--prepare` | Accepted for older scripts: the CAD view is always prepared |
| `--smoke` | Close the window once the scripted steps finish |

## Check limits

The size is the longest side of the body's bounding box.

| Check | warn | fail |
|---|---|---|
| Patch `fold` | – | A point whose normal opposes the centre normal by more than 0.1 % of the mean Jacobian |
| Patch `jacobian` | min/mean < 0.2 | – |
| Patch `corner_angle` | Below 30° or above 150° | – |
| Patch `cad_error` | > 1e-4 · size | – |
| Patch `gap` | > 1e-5 · size (the gap is reported in `gap_m`) | – |
| Patch `overlap` | Overlaps another patch | – |
| `volume` | The three volumes differ by > 1e-4; patches also differ from the CAD volume by > 1e-4 | Volume ≤ 0 |
| `closed` | Patches: gap > 1e-5 · size; panels: open edges (hanging nodes of T-junctions are counted apart, in `hanging_edges`) | – |
| `orientation` | A face looks like a sheet, or the side of a face is unsure | – |
| `waterline` | Cut at the waterline, and z > 1e-6 · size | – |
| Interior `patches` | The worst state of the interior patches (their checks as above, without overlap) | A patch fails |
| Interior `area` | Differs from the CAD section by > 1e-4 | – |
| Interior `waterline` | Gap > 1e-5 · size | – |
| Interior `plane` | – | A control point (panels: a vertex) off z = 0 by > 1e-6 · size |
| Interior `panels` | – | No panel |

A result whose state is `fail` is not written; the message names the failed checks. Only elements that are
broken in themselves fail (a folded patch, a body of no volume, an interior surface off z = 0): surfaces that
meet with T-junctions, with gaps or not at all are accepted by the solver, so gaps and open edges warn and
report their size.

# Architecture

cad2bem is one Rust crate (`cad2bem`, library and executable). The GUI and the command line build the same
conversion request and run it in a private worker process of the same executable; Gmsh is one global state per
process, so every Gmsh session lives in a worker process. A conversion worker exits afterwards; the GUI's CAD
views go to one resident view server (`--view-server`) that keeps its Gmsh session and remembers what depends
only on a face's geometry (samples, overlaps, mirror distances, display meshes), so a view after a settings
change redoes the preparation without sampling or meshing the unchanged faces again. It also holds the last
prepared model: a quick request that adds one cut cuts only that model and returns a provisional view, and
the GUI asks for the exact view (the preparation from the start, as a conversion runs it) right after.
Why the design is as it is: [decisions.md](decisions.md).

## 1. Pipeline

```
STEP/IGES ─▶ prepare ─▶ face sides ─┬─ lo-free ─▶ mesh (Algorithm 6, recombined) ─▶ turn, split ─▶ checks ─┐
             import, cuts,  solids or │                                                                      │
             fragment,      winding   └─ ho ─▶ patches per face ─▶ exact ─▶ orient ─▶ checks ─┬────────────┤
             sheets                                                                          └─ lo-str ─▶ grids ─▶ checks
                                                                                                             ▼
                                                     interior (z = 0 section) ─▶ write, read back ─▶ publish
```

1. **Import** (`engine/prepare.rs`, `engine/units.rs`): the file is read in the unit it states, converted to
   metres (`Geometry.OCCTargetUnit = "M"`; `--unit` replaces the stated unit through `Geometry.OCCScaling`). An
   IGES file is preceded by an empty STEP read (see the pitfalls). Loose faces are sewn (`sewFaces` only), so
   faces touching along an edge share it; a file of a single face is not sewn. With solids, the volume below
   the waterline is measured on copies.
2. **Cuts, overlaps, fragment, sheets** (`engine/prepare.rs`): only what the request asks. `--cut-waterline`
   fragments every face with z = 0 and drops the part above. A whole body that reaches across a declared
   symmetry plane is checked (each side's samples mirrored onto the other within 1e-4 of the size, areas within
   1e-3) and cut there the same way, keeping x ≥ 0, y ≥ 0; a body already cut is left as it is. Faces on the cut
   planes and overlapping duplicates are removed, then all faces are fragmented together so neighbours share
   their curves. `--remove-sheets rules` removes faces that are not part of the closed body; `largest` keeps the
   largest connected group.
3. **Face sides** (`prepare::face_sides`, `engine/winding.rs`): five well-separated probes per face are offset
   along the CAD normal both ways and classified in a copy of the solids. When every face is decided, that is the
   orientation (`method = "cad_solid"`). Otherwise all faces are solved together by the generalized winding
   number of their mesh, the surface closed by its images in the symmetry planes and by its mirror in z = 0 when
   it is open at the water.
4. **lo-free** (`engine/lofree.rs`): the remaining faces are meshed at once (Algorithm 6 with recombination,
   or the mesher `--panel-method` names; neighbouring faces share their nodes), each panel turned from its face's side so its normal points into the
   fluid, non-convex quadrilaterals split into triangles.
5. **ho** (`engine/patches.rs`, `bspline.rs`, `surfaces.rs`, `exact.rs`, `trim_repair.rs`, `ho.rs`): each face
   takes the first path that meets the fit tolerance and has fair corners: O-grid or ring, `native` (the file's
   own B-spline poles, read from IGES entity 128 or STEP), `direct` (the face's parameters), `single` (one Coons
   patch), `triangle` (a three-sided face, one edge folded into its sharpest corner), `bridge` (a five- or six-sided
   face as two Coons patches split by one line across it), `polygon` (spokes from the centre; `triangle` is tried after it); a face is judged
   down to its own sharpest corner, and a kink of more than 150 degrees is part of a side; otherwise `split`: a quad mesh of the face alone (`--patch-method`; its curves at its own size,
   the other faces hidden from the mesher, so that a face does not change with its neighbours), remeshed finer while quads fold. Remeshing candidates first minimize folded
   patches, then poor angles. Pieces of planes, cylinders, cones, discs and rings become rational quadratic ruled
   patches; pieces of spheres and tori bounded by parallels and meridians the rational biquadratics of their
   revolution. Folded quads on trimmed cylinders are unrolled into angular charts, remeshed and replaced by
   exact rational bicubic cylinder patches.
6. **lo-str** (`engine/structured.rs`): T-junctions of the ho patches stay (hanging nodes; `T_JUNCTION_ROUNDS` = 0), counts are
   matched over opposite and shared edges (a straight shared edge whose needs differ by more than
   `CONFORM_RATIO` = 1.5 keeps hanging nodes), edges are divided at equal arc lengths and interiors by Coons
   interpolation.
7. **Checks** (`engine/checks.rs`, and per mode): patches get fold, jacobian, corner_angle, cad_error, gap,
   overlap (Gauss–Legendre sampling, a k-d tree, `checks::par_map` over threads). The body gets volume (V_x, V_y,
   V_z, the CAD volume), closure (gaps for patches, free edges for panels), waterline, orientation. Limits are in
   the [command line reference](../reference/cli.md#check-limits). A result whose state is `fail` is refused.
8. **Interior free surface**: the solids cut below the water keep their faces on z = 0; without solids the
   waterlines of the wetted faces are closed along the symmetry planes (meeting at the origin for `xz_and_yz`) into
   plane faces, an inner loop a hole. They are converted like the body, turned with normals up and checked for
   area and z = 0 (patches: also the waterline gap to the hull).
9. **Output** (`engine/worker.rs`, `engine/panels.rs`, `conversion.rs`): the worker writes HBG and, for panels,
   GDF of the body and of the interior into its job directory, reads them back and compares counts, normals,
   symmetry and volumes. The parent validates the result and publishes one file without overwriting (the GUI
   replaces after its save dialog asked): the interior after the body (`conversion::merged`), or STL made from
   the HBG panels (`conversion::stl`).

## 2. Code map (`src/`)

| Path | Role |
|---|---|
| `lib.rs` | Library root: `cad`, `conversion`, `engine`, `view` (the binary adds `args`, `cli`, `console`, `gui`) |
| `main.rs` | Entry: private `--worker` (inspection), `--view-server` (resident inspection) and `--convert-worker`, `help`, `version`, `convert`, `licenses`, otherwise the GUI (a lone file argument opens it) |
| `licenses.rs` | The licence files the executable carries (collected by `build/licenses.rs` when it is built): printed, written as a folder, shown in About |
| `args.rs`, `cli.rs` | Option helpers and the conversion options shared by `convert` and `gui` (`Options::conversion`); the `convert` command, its help, progress and JSON result |
| `console.rs` | A GUI alone on its console (Windows before 24H2) starts again without one and exits; shared terminals stay attached; a message box for a GUI error nobody would read |
| `conversion.rs` | Request (`Mode`, `Symmetry`, `Sheets`, `PanelMethod`, `PatchMethod`, `Settings`, `Spec`), `Worker`, `Job` (start, progress, poll, validation, cancel), `Format` (HBG, GDF, STL), `publish`/`publish_with`, `merged`, `stl` |
| `cad/mod.rs`, `engine/mod.rs`, `view/mod.rs` | Module roots and their documentation |
| `cad/api.rs` | Table of the Gmsh C API functions used (dynamic SDK through `libloading`, or static link) |
| `cad/session.rs` | Safe Gmsh session: checked errors, copied and freed buffers, model, OCC, mesh and size-callback calls |
| `cad/gmsh.rs`, `cad/scene.rs`, `cad/worker.rs` | CAD preview and untrimmed mesh; inspection scenes (face identity apart from display triangles); the inspection worker and the resident view server (`serve`, `Job::start_resident`); `face_key` (a face's geometry fingerprint) and the display mesh cache |
| `engine/units.rs` | The unit a STEP or IGES file states; the units a request may name |
| `engine/prepare/` | `mod.rs` import, sewing and the order of the preparation (`body`), the volume below the water; `overlaps.rs` face samples and duplicate faces; `symmetry.rs` the symmetry check and cut; `sheets.rs` sheet removal; `cuts.rs` face cuts; `waterplane.rs` the z = 0 section; `sides.rs` face sides from the solids |
| `engine/primitives.rs` | Bodies from their dimensions (Fast Mesh): cylinder, moonpool, sphere, hemisphere, barge as structured panels with the interior surface, and their exact volumes |
| `engine/primitives_ho.rs` | The same bodies as exact rational spline patches (Fast Mesh, order High) from `exact::ruled` and `exact::revolved`, 45-degree pieces, normals into the body |
| `engine/winding.rs` | Generalized winding number orientation (solid angles, symmetry images, z mirror, least squares and sign flips) |
| `engine/lofree.rs` | lo-free panels, body and interior checks and summaries |
| `engine/panels.rs` | Panel geometry: normals, convexity, warp, volumes, symmetry images (`whole`); HBG, GDF and STL text |
| `engine/bspline.rs` | B-spline patches (rational included): evaluation, derivatives, fitting, knot insertion, restriction |
| `engine/patches/` | `mod.rs` the converter, the order of the paths per face, exact patches in place; `edges.rs` boundaries, smooth loops, sides and shared fitted edges; `paths.rs` direct, native, triangle, single, polygon, bridge, O-grid, ring; `split.rs` the quad mesh, its fitting with shared edges and remeshing |
| `engine/fanout.rs` | Opt-in (`CAD2BEM_PATCH_WORKERS`): the body faces split among several patch worker processes (`--patch-worker`), merged in one-process order |
| `engine/surfaces.rs` | Readers of the B-spline surfaces stored in IGES (entity 128) and STEP |
| `engine/exact.rs` | Rational arcs, ruled patches, surfaces of revolution, the tests for exact pieces |
| `engine/trim_repair.rs` | Folded quads on trimmed cylinders to exact rational cylinder patches |
| `engine/checks.rs`, `engine/kdtree.rs` | Patch checks, gaps, overlaps, volumes, areas, Gauss–Legendre; k-d tree |
| `engine/ho.rs` | ho conversion: patch names, orientation, checks, the interior, the HBG document |
| `engine/structured.rs` | lo-str grids: linked counts, arc-length divisions, open and hanging edges (T-junction cuts kept, off) |
| `engine/worker.rs` | The conversion worker: request, progress, result, staged artifacts read back |
| `gui/` | `mod.rs` state and jobs, `panels/` the window layout (`mod.rs` toolbar and shared rows, `model.rs` the Model and Fast Mesh settings, `properties.rs`, `quality.rs` the checks panel, `viewport.rs`, `cuts.rs` the cut modes), `junction.rs` the pipe-junction and wall-window cut rules, `history.rs` undo and redo, `diagnostics.rs` the session log and saved diagnostics, `fonts.rs` the system fonts read when the window opens (no font is built in), `about.rs` the About window, `startup.rs` options, `smoke.rs` scripted checks |
| `view/` | `viewport.rs`/`.wgsl` GPU drawing, camera and picking; `spline.rs` display sampling; `conversion.rs` GUI sessions and exports (span preview at any potential panel size, `publish_with`); `fast.rs` the display of a Fast Mesh body of panels; `mesh_file.rs` the geometry files the Mesh Viewer tab opens (HBG version 3, GDF low order and higher order with the spans of its SPL file, ASCII and binary STL; the elements in z = 0 as the interior surface; volumes and patch checks), also for a Fast Mesh body of patches (`Opened::hbg`) |

`tests/`: integration tests through the library and the built executable; `tests/data/` the test
bodies: small generated ones (cylinder, column, moonpool), copies of the OC3 and OC4 (columns) example models, and `cases.json`
(the conversions of `models.rs` with their expected checks); no test reads `examples/`. `examples/`: the CAD
models and the README with their commands. `scripts/`: the static Gmsh/OpenCASCADE build and
its verification (`build_static_cad.ps1`, `verify_static_cad.ps1`, `native-sources.json`,
`static_cad_probe.rs`), `static_env.ps1`, the session set-up for cargo on that build (the conversion tests), and `package_release.ps1`, the release folders with the licences and the corresponding source (`resources/licenses/`: licence texts it adds), and `make_icons.ps1`, the icons from the logo. `resources/`: the logo
(`logo.svg`; `logo-256.png`, the window icon; `cad2bem.ico` with `cad2bem.rc`, the executable's icon), the Windows
manifest. `build.rs` collects the licence files the executable carries (`build/licenses.rs`: cargo metadata for the linked crates, the unpacked Gmsh, LEMON and OpenCASCADE sources of the static build), embeds the manifest and the icon (`rc.exe` of the Windows SDK, or `RC`; without it the
executable has no icon) and links the static SDK when the `static-gmsh` feature is on.

## 3. Conventions

- **HBG** version 3 (`conversion::HBG_VERSION`; files of another version are refused, also by the viewer):
  `format: "hbg"`, `version`, `kind` (`spline_patches` or `panels`), `label`, `symmetry` (the planes:
  `none`, `yz` x = 0, `xz` y = 0, `xz_and_yz` both; `Symmetry::hbg_name`), `info.generator`, then `patches[]`
  (`name`, `u_order`, `v_order`, `u_knots`, `v_knots`, `coefficients` (M_v, M_u, 3), `weights`) or `vertices`
  (P, 4, 3). Patch files carry no `discretization` but always `panel_size`
  (`DEFAULT_POTENTIAL_PANEL_SIZE_M`, 2 m, when none is chosen).
- **Patch names** `face_<face>` or `face_<face>.<k>`, the face number after the cuts; interior patches
  `waterplane_<k>`. Names are part of the output contract.
- **Orientation.** Patch normals x_u × x_v point into the body; panel normals (right-hand rule) into the fluid.
  Interior patches lie in z = 0 with x_u × x_v down. Patch volume V = −Σ x·(x_u × x_v); panel volume
  V_i = Σ x_i n_i dA.
- **Symmetry** names: `xz` (y = 0), `yz` (x = 0), `xz_and_yz` (both vertical planes, as in HBG); `Symmetry::axes` gives the
  normal axes. Only the writers translate to the axis names of HBG and to GDF flags.
- **Size.** Check limits are ratios to the longest side of the body's bounding box.
- **Gmsh.** One session per worker process. Options persist within a process: the engine resets what it sets,
  and the view server restores every option to its default before each request (an option left by one view
  changed the next). Caches are keyed by face geometry, never by face number. Gmsh meshes a face slightly
  differently with other faces in the same run, so a face meshed next to cached ones may get another triangle
  pattern (display only).
- **Display never exports.** Display triangles and spline samples never reach a writer; GUI sessions keep the
  exact panels or splines and the staged files.
- **Interfaces.** The CLI JSON (`interface`), the conversion job files (`conversion::SCHEMA`) and the
  inspection jobs (`cad::scene::SCHEMA`) are versioned; a change bumps the number.

## 4. Tests

Commands: [CONTRIBUTING](../../CONTRIBUTING.md#before-and-after-a-change). CI (Windows) runs fmt, clippy and the plain `cargo test --locked`
(`.github/workflows/ci.yml`, `ci`) on every pull request, and the static build's clippy and every test
(`conversions.yml`, `conversions`) when code, tests or the build change; the static SDK is cached by the hash of
its build inputs.

The plain run needs no SDK or display (the command line's refusals in `modes_cli.rs`, parts of `convert_cli.rs`,
`mesh_files.rs` for Fast Mesh files, the unit tests in `src/`). Ignored tests run on the static build (`scripts/static_env.ps1`), whose
Gmsh carries the corrections of the build script; a Gmsh SDK (`CAD2BEM_GMSH_LIBRARY`) also runs them, but its
quasi-structured quads may differ from run to run. They are: real conversions in every mode (`tests/conversions.rs`), the executable's command line and files
(`convert_cli.rs`: one file with the interior, HBG/GDF/STL, triangles), the worker and job contract
(`conversion_bridge.rs`, `worker_sdk.rs`), the GUI adapter (`gui_conversion.rs`), converted files read back by the
Mesh Viewer (`mesh_files.rs`), the lo-free interior on the body's waterline nodes (`interior_waterline.rs`), and every
case of `tests/data/cases.json` (`models.rs`, minutes; `CAD2BEM_CASES=name,name` selects some). `tests/common/`
holds the helpers (`run_within`). Use `--release` for
the models.

## 5. Pitfalls

**Gmsh / OpenCASCADE**
- A dynamically loaded Gmsh library is never unloaded: OpenCASCADE starts a thread pool (for example for
  `Geometry.OCCBoundsUseStl` boxes) whose threads outlive `gmshFinalize`, and unloading the library while they
  exist deadlocks the process.
- Until a STEP file has been read in the process, Gmsh cannot set the target unit for an IGES file ("Could not
  set OpenCASCADE target unit"): an empty STEP file is read first, without a scaling set.
- OpenCASCADE's own bounding boxes are widened by the shape tolerance and a Boolean piece keeps the box of its
  original surface: a face on z = 0 then looks as if it crossed the waterline. The import sets
  `Geometry.OCCBoundsUseStl`; above or below is still judged by the centroid.
- `intersect`/`cut` of faces with a solid does not cut faces that straddle it: the waterline is a `fragment`.
- Booleans that measure volume run on copies of the solids; on the originals they renumber faces.
- `isInside` (xyz) is unreliable for faces; use it parametrically. For solids it is wrong within the CAD
  tolerance (~1 mm) of the surface.
- `generate(2)` meshes every surface of the model at once (that is how faces share nodes). A face left out is
  removed from the model first: meshed with the rest, it divides the curves it shares.
- Algorithm 8 makes twisted quads on cylinders; panels use Algorithm 6 with recombination. QQS nodes leave the
  surface and are snapped to the CAD. Blossom recombination makes 180° "ear" quads: patches default to `tri3`,
  panels split non-convex quadrilaterals.
- Opposite periodic faces can have interior mesh edges with the same end nodes but different paths: share a
  fitted edge only on the same face or on a shared CAD curve; subdivide triangles explicitly.
- The seam of a periodic face appears twice in the face (not a free edge); a pole is a curve that collapses to
  a point. Sewing a single face drops its poles and turns a whole sphere into a face `fragment` no longer cuts:
  a file of one face is not sewn.
- A loop with a seam (a whole torus) is chained by end points; going back along the curve just walked does not
  close it.
- OpenCASCADE's IGES reader writes to stdout whatever `General.Terminal` says; workers keep stdout private.
- Mesh options of the patch path (size callback, `SubdivisionAlgorithm`) stay in the session; they are cleared
  before the panel mesh.

**Orientation and sheets**
- A winding check at the offset points of a single face flips with the face's own sign and always passes: all
  faces are solved together.
- Offsets of large faces cross the symmetry planes; distances include the mirror images. Probe clearance is the
  distance to the triangles, not to the nodes.
- With sheets the winding number alone does not tell the intent: hence the sheet rules. A sheet left in the body
  can flip the solution; the orientation check warns (`sheets_left`, a flip margin not above 0).
- The z = 0 mirror overlaps a body above the water, and a deck in z = 0 is its own mirror: it is left out when the
  surface is not open at the water.

**Fitting and checks**
- On a flat face the CAD distance cannot see boundary errors within the face: sample errors are checked too.
- The patches nearest an edge point can all be small neighbours that do not reach it: `gaps` projects the point
  onto every patch near enough.
- A collapsed edge has x_u × x_v = 0 by construction: `jacobian` leaves its points out.
- The `native` path fits the factor from the file's numbers to the model instead of trusting the unit.

**Windows**
- Do not build or test while a GUI run of the same executable is open: the file is locked.

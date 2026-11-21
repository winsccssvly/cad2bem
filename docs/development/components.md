# Components

The code divides into eight components. Each one is a unit of work and of review: a change usually stays in
one, an issue names its component (an `area:` label), and a reviewer reads this page, the
[architecture](architecture.md) (pipeline, conventions, pitfalls) and the component's files, not the whole
crate. A change that crosses components says so in its pull request and is reviewed against the contracts of
each.

Every component keeps the rules of [CONTRIBUTING](../../CONTRIBUTING.md); the contracts below are what a change in
it must not break. "Tests" lists what checks the component: `unit` are the `#[cfg(test)]` modules in its files,
the others are files in `tests/` (most of them ignored: they run on the static build).

## Summary

| Component | Label | Files | Lines |
|---|---|---|---|
| [CAD layer](#cad-layer) | `area: cad` | `src/cad/` | 2,500 |
| [Preparation](#preparation) | `area: prepare` | `src/engine/prepare/`, `units.rs`, `winding.rs` | 2,300 |
| [Spline patches (ho)](#spline-patches-ho) | `area: ho` | `src/engine/patches/`, `ho.rs`, `exact.rs`, `bspline.rs`, `surfaces.rs`, `trim_repair.rs`, `fanout.rs`, `kdtree.rs` | 5,400 |
| [Panels (lo-free, lo-str)](#panels-lo-free-lo-str) | `area: panels` | `src/engine/lofree.rs`, `structured.rs`, `panels.rs`, `checks.rs` | 2,300 |
| [Fast Mesh](#fast-mesh) | `area: fast` | `src/engine/primitives.rs`, `primitives_ho.rs`, `src/view/fast.rs` | 1,150 |
| [Request, job and output](#request-job-and-output) | `area: output` | `src/conversion.rs`, `src/engine/worker.rs`, `src/cli.rs`, `args.rs`, `main.rs`, `console.rs`, `licenses.rs`, `src/view/conversion.rs`, `src/view/mesh_file.rs` | 3,700 |
| [GUI](#gui) | `area: gui` | `src/gui/`, `src/view/viewport.rs`, `viewport.wgsl`, `spline.rs` | 7,100 |
| [Build and release](#build-and-release) | `area: build` | `build.rs`, `build/`, `scripts/`, `resources/`, `.github/`, `Cargo.toml` | |

## CAD layer

`src/cad/`: the Gmsh C API table (`api.rs`), the safe session (`session.rs`), the CAD preview and untrimmed
mesh (`gmsh.rs`), the inspection scenes (`scene.rs`) and the inspection worker with the resident view server
(`worker.rs`).

Contracts:
- Every Gmsh call goes through `cad::session::Session`; Gmsh runs only in worker processes, never in the GUI
  process.
- A function added to `api.rs` exists in the pinned Gmsh (4.15.2) and is linked the same way in the dynamic
  and the static build.
- The inspection job files are versioned (`cad::scene::SCHEMA`); a change bumps the number.
- The view server restores every Gmsh option to its default before a request; its caches are keyed by face
  geometry (`face_key`), never by face number.
- Display triangles are not CAD topology: a scene carries face identity apart from its triangles.

Tests: unit (`scene.rs`), `worker_sdk.rs`, `gui_conversion.rs`.

## Preparation

`src/engine/prepare/` (import and the whole preparation in `mod.rs`; `overlaps`, `symmetry`, `sheets`, `cuts`,
`waterplane`, `sides`), `units.rs`, `winding.rs`.

Contracts:
- Faces are renumbered by cuts and fragments: everything after the preparation uses the prepared numbers, and
  the patch names (`face_<N>`) are those numbers. A change that renumbers the faces of a model changes its
  output names: it is a results change.
- Neighbouring faces share their curves after the fragment (the meshers depend on it).
- Booleans that measure volume run on copies of the solids.
- The orientation is decided for every face, by the solids or by the winding number of the whole body; never
  face by face (see the pitfalls).
- A symmetry cut happens only after the symmetry check passed.

Tests: unit (`prepare`, `units.rs`, `winding.rs`), `conversions.rs`, `interior_waterline.rs`, `models.rs`.

## Spline patches (ho)

`src/engine/patches/` (the converter and the order of the paths in `mod.rs`; `edges` the boundaries and shared
fitted edges, `paths` the paths before split, `split` the quad mesh path), `ho.rs` (names, orientation, checks,
interior, the HBG document), `exact.rs`, `bspline.rs`, `surfaces.rs`, `trim_repair.rs`, `fanout.rs`,
`kdtree.rs`.

Contracts:
- Patch names `face_<face>` or `face_<face>.<k>`, interior patches `waterplane_<k>`: part of the output
  contract.
- Patch normals x_u × x_v point into the body; interior patches lie in z = 0 with x_u × x_v down.
- Neighbouring patches share fitted edges exactly where they share a CAD curve or a face.
- A face meshed for split sees only itself (its result does not change with its neighbours).
- The fan-out over several workers (`CAD2BEM_PATCH_WORKERS`) gives the same patches, in the same order, as one
  process.

Tests: unit (`patches`, `bspline.rs`, `exact.rs`, `surfaces.rs`, `fanout.rs`, `kdtree.rs`), `conversions.rs`,
`models.rs` (the `_ho` cases), `mesh_files.rs`.

## Panels (lo-free, lo-str)

`src/engine/lofree.rs` (lo-free: one mesh of all faces), `structured.rs` (lo-str: grids on the patch layout),
`panels.rs` (panel geometry and the HBG, GDF and STL text), `checks.rs` (patch and body checks, shared with ho).

Contracts:
- Panel normals (right-hand rule) point into the fluid; non-convex quadrilaterals are split.
- lo-free meshes every face at once so faces share nodes; the interior shares the body's waterline nodes.
- The quasi-structured fallback (`--panel-method qqs`) reruns with the default mesher and says so in the result.
- Check limits are ratios to the longest side of the body's bounding box; a limit that changes is a results
  change and goes in the [command line reference](../reference/cli.md#check-limits).

Tests: unit (`panels.rs`, `checks.rs`), `conversions.rs`, `convert_cli.rs`, `interior_waterline.rs`, `models.rs`
(the `_lo_free` and `_lo_str` cases).

## Fast Mesh

`src/engine/primitives.rs` (bodies from dimensions as panels), `primitives_ho.rs` (the same as exact patches),
`src/view/fast.rs` (their display). No Gmsh.

Contracts: the bodies close, their normals follow the conventions above, their volumes match the exact ones.

Tests: unit (`primitives.rs`, `primitives_ho.rs`), `mesh_files.rs` (Fast Mesh files, in the plain run).

## Request, job and output

`src/conversion.rs` (request, worker, job, formats, publishing), `src/engine/worker.rs` (the conversion
worker), `src/cli.rs`, `args.rs`, `main.rs` (the command line), `console.rs`, `licenses.rs`,
`src/view/conversion.rs` (GUI sessions and exports), `src/view/mesh_file.rs` (the files the Mesh Viewer opens).

Contracts:
- The command line is an interface with other programs ([cli.md](../reference/cli.md)): a change to a command,
  an option or the JSON result bumps `interface`; an added field keeps it.
- The GUI and the command line build the same request and run the same job and publisher.
- HBG version 3, GDF and STL as in the [user guide](../user-guide.md); the job files are versioned
  (`conversion::SCHEMA`).
- A result that fails its checks is not written; an existing file is never overwritten (the GUI replaces only
  after its save dialog asked).

Tests: unit (`licenses.rs`, `view/conversion.rs`, `view/mesh_file.rs`), `modes_cli.rs`, `convert_cli.rs`,
`conversion_bridge.rs`, `mesh_files.rs`.

## GUI

`src/gui/` (state and jobs in `mod.rs`; `panels/` the window layout: `mod.rs` the toolbar and shared rows,
`model` the settings of the Model and Fast Mesh tabs, `properties`, `quality` the checks, `viewport`, `cuts` the
cut placement; `junction.rs`, `history.rs`, `diagnostics.rs`, `fonts.rs`, `about.rs`, `startup.rs`,
`smoke.rs`), `src/view/viewport.rs` with `viewport.wgsl`, `src/view/spline.rs`.

Contracts:
- Nothing slow on the rendering thread: Gmsh and conversions run in workers.
- Display never exports: the viewport draws display triangles and spline samples; exports read the staged
  files.
- CAD preview triangles, untrimmed meshes and BEM panels or patches are labelled differently.
- Texts are plain sentences; a warning stays visible.

Tests: unit (`gui/`, `viewport.rs`, `spline.rs`), `gui_conversion.rs`; a change to the window is looked at in
the running GUI (`cad2bem gui ... --screenshot FILE.png`).

## Build and release

`build.rs` with `build/licenses.rs` (the licence files the executable carries, the icon, the static link),
`scripts/` (the static Gmsh/OpenCASCADE build and its check, the session set-up, packaging, icons),
`resources/`, `.github/`, `Cargo.toml` and `Cargo.lock`.

Contracts:
- The release executable links Gmsh and OpenCASCADE statically and imports only Windows system DLLs.
- The native sources are pinned by hash (`scripts/native-sources.json`); a new version or patch changes the
  cache key of the conversion CI and is a results change until the models test says otherwise.
- Every linked component's licence is carried (`cad2bem licenses`) and listed in
  [THIRD_PARTY_NOTICES](../../THIRD_PARTY_NOTICES.md); a new dependency must be compatible with GPL-3.0.

Tests: unit (`licenses.rs`), `scripts/verify_static_cad.ps1`, the [release checklist](release.md).

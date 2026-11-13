# Design decisions

The choices that shape the code today, each with the reason for it. [architecture.md](architecture.md) says how
the code works and [roadmap.md](roadmap.md) what comes next; this page says why it is the way it is. A change
that reverses one of these should say so here, with the new reason.

## Product and process structure

- **One Rust executable, GUI and command line.** `cad2bem.exe` is the GUI and the `convert` command; the GUI and
  the command line build the same request and run the same job and publisher. *Why:* one code path means a file
  converted in the GUI and the same conversion on the command line are the same file, and every check covers
  both.
- **Gmsh only in worker processes.** Conversions run in a fresh `--convert-worker` process each time; the CAD view
  uses one long-lived `--view-server` that restores every Gmsh option to its default and rebuilds the model from
  the file for each request. *Why:* Gmsh keeps process-wide state (options, models, the mesh of every face), a
  crash in Gmsh or OpenCASCADE must not take the window down, and cancelling must be able to kill the work.
  Rebuilding from the file keeps face numbers equal in the viewer and in the conversion; what is kept between
  view requests is only what is a function of one face's geometry (samples, overlaps, display meshes).
- **The dynamic Gmsh library is never unloaded.** *Why:* OpenCASCADE starts a thread pool whose threads outlive
  `gmshFinalize`; unloading the library while they exist deadlocks the process.
- **Static Gmsh and OpenCASCADE for the release.** The release links Gmsh 4.15.2 and OpenCASCADE 7.8.1, built
  from pinned archives with a static C runtime, into one executable that needs only Windows system libraries.
  *Why:* a user unzips one file and runs it, with no SDK, interpreter or private DLL to install.
- **Conversion tests run on the static build.** *Why:* the build script corrects Gmsh (determinism, an error path,
  the matching below); an unpatched Gmsh SDK gives quasi-structured quads that differ from run to run.
- **One console program, no console window from Explorer.** The executable stays a console program (a shell
  waits for it and gets its output and exit code); its manifest asks Windows 11 24H2 and later not to create a
  console when it is opened from Explorer, and older Windows restarts the GUI without one. *Why:* one file serves
  scripts and double-clicks.

## Licences

- **GPL-3.0-or-later.** *Why:* Gmsh is GPL-2.0-or-later and is linked; GPL-3.0 is the licence they combine under.
- **No Blossom IV.** The static Gmsh is built without Blossom IV and Concorde, and the minimum-cost perfect matching
  of the quadrilateral recombination comes from LEMON (Boost Software License). *Why:* Blossom IV's author allows
  it only within Gmsh, an extra restriction a GPL program that links Gmsh may not carry. LEMON computes the same
  matching (the maximum-weight perfect matching of the weights BIG − cost); the edges are given in a fixed order
  (cost, then triangle numbers) because many costs tie and Gmsh's own order follows memory addresses.
- **The executable carries its licences.** `build.rs` collects the licence files of everything linked (the crates
  from cargo metadata, Gmsh with its bundled code, LEMON and OpenCASCADE from the static build's sources); the
  executable prints them (`cad2bem licenses`), shows them (About) and writes the release's `licenses/` folder.
  *Why:* the permissive and LGPL licences ask for their notices with every copy of the binary, so the executable
  can be passed on alone; one collection serves the folder and the program.
- **No bundled fonts.** The window reads the operating system's fonts when it opens. *Why:* egui's bundled fonts
  are under the Ubuntu Font Licence and the SIL Open Font License, whose combination with a GPL executable is
  disputed; system fonts are not distributed. They also cover Korean file names.

## Preparation of the CAD model

- **Nothing is cut or removed unless asked.** The waterline cut (`--cut-waterline`), the symmetry cut
  (`--symmetry`), sheet removal (`--remove-sheets`) and the interior surface (`--interior`) are options. *Why:* a
  CAD file may be meant whole (a solver can trim it), and silent cuts hide mistakes in the model.
- **The file's length unit is used.** OpenCASCADE converts what the file states to metres; `--unit` overrides a
  wrong statement. *Why:* the earlier default (millimetres) turned correct metre files into wrong ones.
- **A whole model with `--symmetry` is checked, then cut.** The faces on each side are mirrored and compared
  (distance within 1e-4 of the body size, areas within 1e-3); a model that is not symmetric is refused with the
  numbers. *Why:* real CAD files are whole far more often than halved, and a mirrored copy of a whole body
  overlaps itself. The tolerance admits the millimetre asymmetries real CAD files have.
- **Face cuts are geometry, not face numbers.** A cut is a rectangle in a plane; the automatic junction rules and
  the GUI make the same list (`--cuts FILE.json`). *Why:* every cut renumbers the faces, so a list of numbers would
  stop meaning anything after the first cut.
- **Automatic junction cuts.** Where a pipe ends on another body, the pipe face is cut across beyond the junction
  and its end piece along the axis; a column wall with holes is cut into windows around each hole. *Why:* trimmed
  junction faces are the faces whose spline patches fail or warn; four-sided pieces fit well.
- **Loose faces are sewn and a waterplane closed without solids.** An IGES file of loose surfaces is sewn after
  import, and its interior surface is closed along the waterline curves. *Why:* faces that only touch at their
  edges share no curve, so every face would be an open sheet.

## Orientation and checks

- **Orientation is solved for the whole body.** Solids decide where they exist; elsewhere the generalized winding
  number decides per CAD face, with all faces and their mirror images together. *Why:* a check of a single face
  flips with that face's own sign and always passes, and loose IGES faces have no solid.
- **Fail only for elements a solver cannot use.** A conversion fails for folded patches, a volume of zero or less,
  an interior surface off z = 0 or an empty one. Gaps, T-junctions and free edges are warnings with their sizes,
  and the file is written. *Why:* the solver refuses only invalid single elements; it accepts panels that meet at
  hanging nodes and small gaps.
- **A failed conversion is still shown.** The worker writes its artifacts and the result says why it failed; the
  publisher refuses to write it, the GUI shows the failing patches. *Why:* without the result the user cannot see
  which face to cut or remesh.
- **A fold is a reversed Jacobian above 1e-3 of the patch mean.** *Why:* a corner a little over 180° has a
  vanishing Jacobian at one point; that is a warning, not a fold.

## Meshing

- **lo-free panels: Frontal-Delaunay with recombination.** Gmsh algorithm 6 with recombination; non-convex
  quadrilaterals are split in two triangles. *Why:* algorithm 8 makes twisted quads on cylinders and, at the
  same size, coarser panels with volume errors of several per cent; the recombination makes some 180° quads
  that a solver refuses unless split.
- **The interior surface is meshed with the body.** In lo-free the waterplane faces are added to the body model and
  meshed in the same run. *Why:* meshed apart, the waterline was divided differently for the body and the interior
  surface.
- **A failed quasi-structured mesh falls back for the whole model.** When `--panel-method qqs` fails or crashes the
  worker, the job runs again with the default mesher and says so (`panel_method_fallback`). *Why:* Gmsh's
  quasi-structured pipeline works on the whole model at once (background field, cross field, topology
  optimisation), so one face cannot be given another mesher.
- **lo-str leaves T-junctions.** Structured panels meet with hanging nodes where a patch corner lies inside a
  neighbour's edge, and no transition rows are added. *Why:* the solver accepts them, cutting them doubled the
  panel count, and transition rows never reduced it on the test models.
- **ho split faces are meshed one by one.** A face no layout fits is meshed on its own, with its own size. *Why:*
  changing one face's settings then never changes its neighbours.
- **Five- and six-sided faces try a bridge first.** One line across the face makes two four-sided pieces before
  the polygon layout is tried. *Why:* two Coons patches fit and stay fair more often than a polygon split.
- **Deterministic output.** Meshes are renumbered by position, the quasi-structured refinement and the matching
  work in number order, and Gmsh runs one thread. *Why:* the same input gives the same file every run, which the
  tests compare; Gmsh threads gave deadlocks and moving results.
- **Fits stop when the error stops falling.** An adaptive spline fit gives up when one more span level does not
  cut the error by 10 %. *Why:* only fits that would never reach the tolerance end early; the others are unchanged.

## Output

- **HBG version 3 only.** Symmetry names are those of HBG (`none`, `yz`, `xz`, `xz_and_yz`); other versions are
  neither written nor read. Spline-patch files always carry `panel_size` (2 m unless chosen). *Why:* one version
  the solver reads; two would double every writer and reader.
- **The interior surface is in the body's file, after the body.** *Why:* the solver finds the elements lying
  wholly in z = 0 itself; one file cannot be separated from its interior surface.
- **Patch names `face_<face>` and `face_<face>.<k>` are part of the contract.** *Why:* the GUI and other programs
  key their per-face results and settings on them.
- **GDF is low order only; STL is for viewing.** Patches are never flattened to GDF; STL holds the whole body
  (mirror images included) and the interior surface as a second solid. *Why:* each format holds what it can
  represent exactly.
- **Display never exports.** Display triangles and spline samples never reach a writer; exports read the staged
  files of the job. *Why:* the file must be the conversion, not its picture.
- **Results are never overwritten.** A result that fails its checks is not written and an existing file is not
  replaced. *Why:* a failed run must not destroy the last good file.
- **The command line is a contract.** Its options and JSON carry an `interface` number; a change raises it, an added
  field does not. *Why:* other programs (a solver GUI) run `cad2bem convert --json`.

## Repository

- **Tests never read `examples/`.** The models the tests convert are copies in `tests/data/` with their expected
  checks in `tests/data/cases.json`. *Why:* the examples can change for users without moving test expectations.
- **Example CAD files are drawn for the project.** The OC3 and OC4 models were drawn in FreeCAD from the published
  NREL dimensions; the other bodies are generated. *Why:* every file of the repository can be redistributed under
  its licence.
- **Fast Mesh needs no Gmsh.** Its bodies (cylinders, a moonpool, spheres, a barge) are built from their
  dimensions, as panels or as exact rational spline patches. *Why:* simple bodies need no CAD file and convert
  instantly.

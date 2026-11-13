# Changelog

User-visible changes by version.

## 0.1.0 (unreleased)

The first release: one Rust executable, GUI and command line, that converts STEP and IGES files to BEM surface
geometry (GDF, HBG version 3, STL). Licence: GPL-3.0-or-later.

### Conversion

- **lo-free interior surface on the body's waterline.** The interior panels are meshed with the body, on the
  body's own waterline curves, so they share its waterline nodes with every panel mesher (meshed apart, the
  quasi-structured mesher divided the same waterline differently for the two).

- **One engine.** Every mode converts in the Rust engine of the executable, in a private worker process.
- **Quasi-structured fallback.** When Gmsh's quasi-structured mesher fails on a model (`--panel-method qqs`; it can
  fail, or crash the worker, in its topology optimisation of a face, as on an OC4 platform with braces), the conversion runs again
  with the default mesher, says so, and records it in `summary.panel_method_fallback`.
- **Modes.** `lo-free` (panels on the CAD faces, mesher `--panel-method`: mostly quads, quasi-structured quads,
  all quads, triangles), `lo-str` (panels on the spline patch grids) and `ho` (four-sided B-spline patches, exact rational
  on analytic pieces, HBG only), with the interior free surface (`--interior`).
- **Preparation.** The file's length unit (or `--unit`), the waterline cut (`--cut-waterline`), symmetry cuts
  after a symmetry check (`--symmetry`), overlapping and cut-plane faces removed, sewing of loose surfaces,
  sheet removal (`--remove-sheets rules|largest`).
- **Face cuts.** `--cuts FILE.json` splits faces with planar rectangles before meshing, optionally only those on
  one cylinder or plane (`on`) or only the one face through a point (`face_at`).
- **Orientation.** From the CAD solids where they decide every face (denser probes for thin and junction
  pieces), otherwise by the generalized winding number of the whole body with its symmetry images and z mirror,
  per CAD face in every mode (a saved half model without solids took minutes in ho, now seconds).
- **Spline patch layouts.** O-grid, ring, the file's own B-spline poles (IGES entity 128, STEP), direct, Coons,
  bridge, triangle, polygon and split. A face whose own corner is sharper than 30 degrees is judged down to that
  corner (it stays one patch's corner whatever the layout), and a kink of more than 150 degrees is part of a side.
  O-grid and ring layouts start at a CAD vertex, so their corners meet the
  neighbours' exactly. A four-sided face whose opposite sides turn more than 90 degrees (a half ring) is split
  into strips. A five- or six-sided face (a piece of a pipe junction window) is split by one line across it into
  two four-sided patches (bridge). A three-sided face is one patch with an edge folded into its sharpest corner
  (triangle). Split faces use `--patch-method tri3|qqs|subdiv`; folded quads on trimmed
  cylinders are repaired as exact rational patches, and qqs or subdiv split faces that fold are made again with
  tri3.
- **Face settings.** `--faces FILE.json` (and the GUI) forces a face's layout, gives it its own patch or panel
  size and a priority (on shared edges the higher priority sets the mesh). A forced layout that does not fit is split and reported.
- **Checks.** Per patch fold, Jacobian, corner angle, CAD error, gap and overlap; for the body volume, closure,
  waterline and orientation; for the interior area, plane and waterline. Only elements broken in themselves
  fail (a folded patch, a body of no volume, an interior surface off z = 0); gaps and open edges warn and report
  their size, since the solver takes surfaces that meet with T-junctions, gaps or not at all. A patch corner a
  hair past 180 degrees is a small Jacobian (warn), not a fold. A result that fails is never written; its report
  names the failing patches and their checks.
- **Split faces on their own.** A face that no layout fits is meshed alone, its edges at its own size: changing one
  face's settings no longer changes its neighbours (an OC4 platform with braces: 1 face changed instead of 8).
- **lo-str keeps T-junctions.** Where a patch corner meets the middle of another patch's edge, the panels meet
  with hanging nodes instead of the patch being cut there: an OC4 platform with braces in lo-str, 23,450 panels before and
  11,290 after, the enclosed volume 0.06 % smaller from small chord gaps on curved edges.
- **Speed.** Five- and six-sided faces skip splits whose coarsest quads already have a corner far outside the
  fair range, and the written HBG is read back in one piece: an OC4 platform with braces in ho converts in about 43 s
  instead of 100 s. Fits whose error stops falling as spans are added end early: an OC4 model with polygonal
  pontoons in ho takes 20 s instead of 64 s. A face's repair of folded cylinder patches no longer depends on the faces
  repaired before it in the same run. With `--patch-method qqs` every split face is meshed with its own guiding
  field (Gmsh kept the first face's and used it for the faces meshed after it). Patches no longer depend on how
  Gmsh numbers the mesh: the same file gives the same patches every run and with any number of patch workers.

### Command line (interface 3)

- **Licences in the executable.** `cad2bem licenses` prints the licence of cad2bem and the licence files of every
  component the executable carries (Gmsh with its bundled code, LEMON, OpenCASCADE, the Rust crates); `--write
  FOLDER` writes them as files. **About** in the GUI shows the same files, so the executable can be passed on alone.
- **STL and triangle panels.** `--out FILE.stl` writes ASCII STL triangles of a low-order result: the whole body
  (the symmetry images made too; STL has no symmetry), the interior surface a second solid. `--panel-method
  triangles` meshes lo-free with triangles only. The GUI's **Export mesh…** (conversions and Fast Mesh panels)
  offers STL, and the Panel mesher **Triangles**.

- **Symmetry names of the geometry file** (interface 3). Both vertical planes are `--symmetry xz_and_yz`, as HBG
  names them, instead of `xy` (which read as the plane z = 0); the GUI shows the same names with their planes.

- **One file with the interior surface.** `--interior` writes the interior free surface in the
  output file after the body (its elements lie in z = 0, where the solver's interior surface
  "detect" finds them) instead of a second file `NAME.interior.EXT`; `files` lists the one file. The GUI's
  export and Fast Mesh write the same; the Mesh Viewer finds the interior in a file the same way.

- `cad2bem convert CAD --out FILE.hbg|FILE.gdf [options] [--json]`, `cad2bem gui`, `cad2bem version`. Progress on
  stderr, the result as JSON on stdout; existing files are never overwritten; Ctrl+C cancels the worker.
- The JSON result lists the checks of every ho patch, body (`patch_checks`) and interior (`interior_patch_checks`).
- `--potential-panel-size METRES` (ho; GUI **Potential panels**) writes the potential panel size to the HBG
  file (`panel_size`, 2 m by default, so every ho file opens without a discretization from its reader); the solver sets each patch's spans from it, and **Spans** shows that division. In the GUI
  a new size redraws Spans and is written on export without converting again. Each patch check lists its edge
  lengths (`edge_lengths_m`).

### GUI

- **System fonts.** The window uses the fonts of the operating system (on Windows Segoe UI, with Consolas,
  Segoe UI Symbol and Malgun Gothic, so Korean file names show); the executable carries no font of its own.
- **Logo.** The window, the taskbar and the executable in Explorer show the cad2bem logo, drawn to the Windows app
  icon guidelines: a column floating in water, its smooth CAD surface above the waterline made of flat panels below
  it (`resources/logo.svg`).
- **GUI manual** (`docs/gui-manual.md`): the window step by step with screenshots.
- **Plain wording.** Hints, hover texts, status lines and errors say in full sentences what happened and what to do
  (the open-model hint, the cut modes, the face settings, Fast Mesh); counts say "1 face" and "3 faces"; the checks name
  their measures in words ("largest gap", "differs from the CAD by", percentages); the list names faces, patches and parts.
- **Layout.** The toolbar holds only actions (Open…, Save prepared CAD…, Export mesh… | Undo, Redo | Save
  diagnostics); the settings of each tab are aligned label | control under section headings; the faces and the
  selection (with its face settings and the extent) share the right panel; the file and the kind of view are in
  the view's corner, and the status line (job, Cancel, error) is under the view. The side panels can be
  resized by dragging; long rows wrap or are cut short rather than widening them. Opening another CAD file
  resets the settings to their defaults. A conversion that wrote the interior surface, and a
  viewed file with one, show it at once (the Interior surface switch on). With it on, the interior surface is
  green and the wetted body blue-grey, with their legend in the view's corner.
- **Fast Mesh tab.** Makes a vertical cylinder, a cylinder with a moonpool, a submerged sphere, a floating
  hemisphere or a box barge from its dimensions and a panel size, without CAD: structured panels (rings and rays on
  discs, latitudes and longitudes on spheres), the symmetry, and the interior surface on the body's waterline nodes;
  its volume against the exact one; **Export mesh…** writes GDF, HBG or STL. **Panel shape: Triangles** splits every quadrilateral along its shorter
  diagonal. **Order: High** makes the same bodies as
  exact spline patches (HBG; 45-degree ruled and revolved rational pieces, the size written as the potential panel
  size), their volume and interior area exact. **Export mesh…** (a conversion or a Fast Mesh
  body) replaces a file that exists once the save dialog has asked; the command line still refuses it.
- **Mesh Viewer tab** (the tabs apart sit under **1 CAD  2 Mesh**). Opens a BEM geometry file as it is, apart
  from the CAD and Mesh tabs (which keep their work): a geometry file (`.hbg`, version 3: panels, flat or spline
  patches) or a GDF file (low-order panels, or higher-order flat or B-spline patches with the spans of the SPL file
  beside it; opening the `.spl` opens its GDF) or an STL file (ASCII or binary triangles of the whole surface), its
  elements lying in z = 0 shown as the interior surface. It lists the kind, count, symmetry, discretization, volume, interior area and the checks
  of every spline patch, and **Spans** shows the file's own division. **Open…** and dropping take CAD and geometry
  files; the toolbar's export button is **Export mesh…**.
- **Two tabs.** **1 CAD** prepares the model (unit, waterline, symmetry, extra faces, face cuts) and always shows
  the faces the conversion will mesh, following every change; **2 Mesh** sets the mode, sizes, layouts and face
  settings and shows the result. One executable: `cad2bem.exe` opened from Explorer is the GUI, without a
  console window on Windows 11 24H2 and later (its manifest; older Windows: a console flashes and closes as the
  GUI starts again without it), and a CAD file dropped on its icon or on the window
  opens. Light theme, short setting names.
- **Fast CAD view.** CAD views run in one resident process that keeps its CAD kernel and remembers what depends
  only on a face's geometry, so a view after a settings change redoes the preparation without sampling or
  meshing the unchanged faces again (an OC4 platform with braces, 45 cuts: 1.1 s instead of 2.9 s).
- **Cutting faces.** A cut mode in the view bar: **Across** and **Along** a cylinder or wall, **Two points**
  (through two clicked points, square to the face) and **Parallel** (to a reference face clicked first), on the
  face under the mouse (**Face**), its surface (**Surface**) or every face (**All**). The cut is drawn in red (on a
  cylinder along the exact curve, also for a cut nearly tangent to it), snaps to corners, and a click shows it
  at once, then the exact prepared view follows; the next cut (a click, the cut list, junction cuts) waits
  until those faces are shown, so cuts are made one at a time. **Pipe junctions: Cut all** adds the cuts that pipe ends and the
  column walls they meet need for four-sided pieces (an OC4 platform with braces in ho:
  2,547 patches with 461 warnings without cuts, 552 with 116 with them and the bridge layout). The cut list
  removes, saves and loads cuts. The face under the mouse is highlighted and named.
- **Open models.** The CAD tab says when a model is open along y = 0, x = 0 or z = 0 (a saved half model) without
  the matching Symmetry or Cut at waterline, whose cut edges would fail as gaps, and sets them with one click.
  **Save prepared CAD** writes the prepared faces as STEP in metres.
- **Results.** Spline patches are coloured by their check state (warn orange, fail red); **Spans** shows how each
  patch is divided for the potential, the interior surface's patches included. A conversion whose checks fail is still shown, failed patches first in
  Checks with each check explained on hover, and cannot be exported. Very large spline results are displayed
  coarser instead of exceeding the GPU's buffer limit.
- **Viewport.** Orbit about the surface point under the cursor (drag), pan by the grabbed surface point (right
  drag), zoom toward the surface under the cursor; a scale bar and axes; Fit, Iso, Front, Side and Top.
  The camera stays where it was across conversions, remeshes, cuts and the two tabs, and the view keeps its
  size and place whatever the view bar holds.
- **Undo and redo** of the settings (Ctrl+Z, Ctrl+Y or Ctrl+Shift+Z, and toolbar buttons).
- **Saved diagnostics and a session log.** **Save diagnostics** (to report a problem) saves the GUI state, the conversion request, the last
  result, the log, a screenshot and the CAD file to a folder and copies its path; `gui --diagnostics FOLDER` opens it
  again. Each session writes a log, panics included.
- **Scripted checks** for development: `--screenshot`, `--pick-centre`, `--smoke` and the options in the command
  line reference.
- **A file whose view fails stays open.** When a newly opened file cannot be shown (a symmetry it does not have,
  say), the window no longer shows the previous file under its name; changing the settings tries the new file again.

### Platform

- **Gmsh.** One typed C API table for a dynamically loaded Gmsh 4.15 SDK (`CAD2BEM_GMSH_LIBRARY`) or the
  `static-gmsh` build that links Gmsh 4.15.2 and OpenCASCADE 7.8.1 from pinned sources into one executable that
  needs only Windows system libraries. The SDK library is never unloaded, which fixes a hang at the end of
  conversions that used triangulated bounding boxes. The static Gmsh leaves out Blossom IV, whose licence allows it
  only within Gmsh: the quadrilateral recombination takes its perfect matching from LEMON (Boost licence), with the
  same meshes on the models: equal-cost matchings are taken in a fixed order, so a conversion repeats exactly.
- **Examples.** Column, moonpool, Wigley hull, the OC3 spar and the OC4 columns (drawn in FreeCAD), with the command
  of every conversion; a
  higher-order GDF file with its SPL file (a box barge) to open in the Mesh Viewer.
- **Release.** `scripts/package_release.ps1` assembles the Windows release (the zip, the executable on its own and
  the source): the executable with the documentation, the
  example models and the licences
  of Gmsh (and the code it bundles), OpenCASCADE, every linked crate, and the corresponding source
  (cad2bem, the pinned Gmsh and OpenCASCADE archives, the vendored crates). The executable carries no path of
  the build machine.

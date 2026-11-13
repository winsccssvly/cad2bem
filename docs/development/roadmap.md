# Design and roadmap

cad2bem is a one-shot CAD-to-BEM converter: open STEP/IGES, choose a mode, convert, inspect the checks, export
GDF, HBG or STL. The product is one executable with the GUI and the command line, with Gmsh and OpenCASCADE linked
statically: no interpreter, extracted runtime or private CAD DLL. Normal system and graphics-driver DLLs are
allowed.

## Output contract

| Mode | Geometry retained for export | Formats |
|---|---|---|
| lo-free | Quadrilateral/triangle panels meshed on the CAD faces | GDF, HBG panels, STL |
| lo-str | Panels laid on the spline patch layout | GDF, HBG panels, STL |
| ho | B-spline/NURBS patches with orders, knots, coefficients and weights | HBG spline_patches |

GDF is a low-order format: patches are never flattened to it. Display tessellation is never export geometry.
File validity, units, orientation, closure and fidelity are checked by the application's own readers and
tests; no consumer program is needed.

## Design rules

- **One request model.** The GUI and the CLI build the same request and use the same job and publisher.
  Settings depend on the mode: panel size for lo-free, patch size for ho, both for lo-str; units, waterline,
  symmetry, sheets and the interior are shared. An option a mode does not use is refused.
- **Isolated CAD state.** Gmsh has process-wide state. It runs only in a private worker process of the same
  executable, never on the rendering thread. Cancelling or closing kills and reaps only that worker.
- **Exact artifacts, separate display.** Results keep the original panels or splines and the checks; a display
  adapter tessellates them for wgpu; exports read the staged files.
- **Honest results.** Warnings stay visible; a result that fails is not written. A failed or cancelled run never
  replaces the last result. Existing files are never overwritten.
- **Face identity per revision.** Cuts and fragments renumber faces: a converted result is selected by its
  prepared-face numbers, not by the imported ones.
- **Names that match the data.** CAD preview triangles, untrimmed meshes and BEM panels or patches are labelled
  differently.

## State

| Stage | Done |
|---|---|
| GUI and CLI | egui/wgpu window, CAD preview, face picking, untrimmed mesh, conversion, inspection, export; headless `convert` |
| Gmsh access | One typed C API table for the dynamic SDK and the static link; a safe session; inspection and conversion workers |
| Static CAD | Pinned Gmsh 4.15.2 and OpenCASCADE 7.8.1 static libraries, the static CRT, import audits ([static CAD backend](static-cad.md)) |
| lo-free | Preparation, solid and winding orientation, meshing, checks, interior, writers |
| ho | B-spline machinery, every face path, exact rational patches, trim repair, orientation, checks, interior |
| lo-str | T-junction cuts, linked counts, arc-length grids, hanging-node closure |
| Models | Every model in every mode converts as `tests/data/cases.json` expects |
| GUI tabs | Mesh Viewer (HBG version 3, low-order GDF, the interior as the elements in z = 0); Fast Mesh (low-order panels, high-order exact patches) |

## Next

1. **Release build.** Verify every mode in the `static-gmsh` executable on a clean machine (SDK hidden, import
   table audited) and make it the release artifact, with the notices of the linked libraries.
2. **Per-face settings.** Remove or keep single faces; a recipe file for the options.
3. **Mesh options.** Finer panels at the waterline, a minimum count per turn of curved faces, a z shift.
4. **Quality.** Complex trimmed junctions (pontoons and braces meeting columns) give spline patches with corner-angle and Jacobian
   warnings and structured panels with hanging nodes; explicit face partitions or better split layouts.
5. **Speed.** Large models: the inspection result decoded off the UI thread, picking faster than linear, one
   display buffer for body and interior.

## Deferred

Project save/restore, a CAD editing history, a CAD editor, local remeshing and solver analysis are not part
of the one-shot converter. Rebuilding selected regions needs a topology/history model so neighbours still agree
on nodes and fitted curves. Complex trims can still need CAD preparation outside the application.

# Third-party notices

The source code of cad2bem contains no third-party source code apart from short excerpts of Gmsh source in the
patches of `scripts/build_static_cad.ps1` (GPL-2.0-or-later); `resources/licenses/` holds the texts of the
Apache License 2.0 and the LGPL-3.0 for a release. This file lists the software cad2bem uses and its licences.

## CAD kernel

| Software | Use | Licence |
|---|---|---|
| Gmsh 4.15 (C API) | CAD import, Booleans, cuts, sewing, surface meshing | GPL-2.0-or-later (exact terms and exceptions in `LICENSE.txt` of the Gmsh distribution) |
| OpenCASCADE Technology 7.8 (through Gmsh) | STEP/IGES reading, B-rep operations | LGPL-2.1 with the OCCT exception |
| LEMON 1.3.1 (headers, through Gmsh) | The minimum-cost perfect matching of Gmsh's quadrilateral recombination | BSL-1.0 (Boost Software License 1.0) |

The default build loads a separately installed Gmsh 4.15 SDK library at run time. The `static-gmsh` build links
Gmsh 4.15.2 and OpenCASCADE 7.8.1, built from the source archives pinned in `scripts/native-sources.json`; the
builder applies small Gmsh changes, kept in the build script: a CMake static-link correction, and
source fixes marked `cad2bem:`: in `src/mesh/meshGFaceDelaunayInsertion.cpp`, two mesher globals reset on an early
error return of the frontal quad mesher; in `contrib/QuadMeshingTools` (`qmtMeshUtils.cpp`, `cppUtils.h`),
mesh vertices and elements ordered by number instead of memory address; in `src/mesh/meshQuadQuasiStructured.cpp`,
the refinement deletes replaced vertices only after the elements are updated and starts every element at its
vertex of smallest number; in `src/mesh/meshGFaceOptimize.cpp` and `src/common/DefaultOptions.h`, the perfect matching
of the quadrilateral recombination computed with LEMON (`scripts/gmsh/cad2bemLemonMatching.h`, copied into the Gmsh
sources) instead of Blossom IV, with the recombination default kept on the matching. LEMON comes from the archive
pinned in `scripts/native-sources.json` (a copy of the LEMON 1.3.1 sources); only its headers are used. cad2bem is
GPL-3.0-or-later, and Gmsh's "version 2 or later" includes version 3, so
the two combine under GPL-3.0; OpenCASCADE's LGPL-2.1 allows the combination with a GPL program, and Gmsh's
licence names OpenCASCADE in its exception.

### Code bundled in Gmsh

The static Gmsh is built with the following contributed code of the Gmsh distribution (`contrib/`), each under
its own licence; the Gmsh files `LICENSE.txt` and `CREDITS.txt` give the details.

| Code | Licence |
|---|---|
| ALGLIB, kbipack, QuadTri | GPL-2.0-or-later |
| ANN, bamg, MathEx | LGPL-2.1-or-later |
| gmm | LGPL-3.0-or-later with the GCC Runtime Library Exception |
| Eigen | MPL-2.0 (with BSD-licensed parts) |
| tinyxml2 | Zlib |
| voro++ | BSD-3-Clause-LBNL |
| nii2mesh | BSD-2-Clause and Zlib |
| DiscreteIntegration, domhex, HighOrderMeshOptimizer, MeshOptimizer, MeshQualityOptimizer, onelab, QuadMeshingTools, untangle, WinslowUntangler | Gmsh's licence |

Gmsh's boundary recovery (`src/mesh/tetgenBR`) is relicensed under Gmsh's licence by agreement with its
authors.

Gmsh is built **without Blossom IV and Concorde** (`contrib/blossom`, `ENABLE_BLOSSOM=OFF`): their author allows
them only within the Gmsh system, a restriction the GPL of an executable that links Gmsh does not admit. The
perfect matching they computed for the quadrilateral recombination comes from LEMON instead (above), so the
executable contains no code under a licence other than those listed here. The Gmsh source archive of a release
still holds `contrib/blossom` as Gmsh distributes it; the build does not compile it.

## Rust crates (Cargo)

The locked dependency graph is in `Cargo.lock`.

| Crate | Use | Licence |
|---|---|---|
| egui / eframe / egui-wgpu | Application window and widgets | MIT OR Apache-2.0 |
| wgpu | GPU rendering | MIT OR Apache-2.0 |
| glam | Vectors and matrices | MIT OR Apache-2.0 |
| nalgebra | Dense least squares (SVD) for the spline fits | Apache-2.0 |
| bytemuck | Typed GPU buffer layout | Zlib OR Apache-2.0 OR MIT |
| libloading | Dynamic Gmsh SDK loading | ISC |
| rfd | Native file dialogs | MIT |
| serde / serde_json | Job requests, results and geometry files | MIT OR Apache-2.0 |
| anyhow | Error propagation | MIT OR Apache-2.0 |
| ctrlc | Command-line cancellation | MIT OR Apache-2.0 |
| image | PNG output of the visual checks | MIT OR Apache-2.0 |
| tempfile | Private job directories, test fixtures | MIT OR Apache-2.0 |

These are the direct dependencies. The crates linked into the Windows build (about 200) are under
permissive licences compatible with GPL-3.0: MIT or Apache-2.0 for most, the rest Unicode-3.0 (ICU data
crates), Zlib, BSD, BSL-1.0, ISC, Unlicense and CC0-1.0.

The executable carries no font: egui is built without its bundled fonts, and the window reads the fonts of the
operating system when it opens (on Windows Segoe UI, Consolas, Segoe UI Symbol and Malgun Gothic).

## A binary release

The Windows executable links all of the above. `scripts/package_release.ps1` assembles a release under the
GPL-3.0 terms:

- `cad2bem-VERSION-windows-x64`: `cad2bem.exe` with this file, `LICENSE`, `NOTICE`, `SOURCE.txt` and
  `licenses/`: the Gmsh licence and credits with the licence files of the bundled code above, the
  OpenCASCADE LGPL-2.1 and its exception, and the licence files of each linked crate (listed in
  `licenses/crates/README.md`; a crate that ships none is used under its Apache-2.0 option with that text).
  The executable carries the same files: `cad2bem licenses` prints them (`--write FOLDER` writes the folder the
  release script packs), and **About** in the GUI shows them.
- `cad2bem-VERSION-windows-x64.exe`: the same executable on its own.
- `cad2bem-VERSION-source`: the corresponding source: cad2bem at the commit of the executable, the pinned
  Gmsh and OpenCASCADE archives the build script patches and builds, and the vendored crate sources.

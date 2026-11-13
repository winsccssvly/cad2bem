# Working on cad2bem

cad2bem is a one-shot CAD-to-BEM surface converter: one Rust executable (egui + wgpu GUI and a command line)
with Gmsh and OpenCASCADE, writing GDF, HBG and STL. The release target links Gmsh and OpenCASCADE statically: no
interpreter, extracted runtime or private CAD DLL.

Read [docs/development/architecture.md](docs/development/architecture.md) (pipeline, code map, conventions,
pitfalls), [docs/development/decisions.md](docs/development/decisions.md) (why the design is as it is) and
[docs/development/roadmap.md](docs/development/roadmap.md) (design rules, state, next work) before a change, and [CONTRIBUTING.md](CONTRIBUTING.md) for the checks.

- Work in independently reviewable steps; verify each (fmt, clippy, tests, the examples when results move).
- Keep Gmsh in worker processes, never on the rendering thread; every call through `cad::session::Session`.
- Keep CAD-face identity within a geometry revision; do not confuse display triangles with CAD topology or with
  BEM panels and patches, in the UI and in the documentation.
- The GUI and the CLI share one request, job and publisher; neither has its own mesher.
- Never overwrite user files; a result that fails its checks is not written.
- Do not describe partial remeshing, CAD editing or project persistence as done before they are implemented
  and verified. MSH output and solver GUI integration are out of scope.
- Preserve uncommitted user changes; do not stage editor settings or unrelated work.

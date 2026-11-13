# Static CAD backend

The opt-in `static-gmsh` build links Gmsh 4.15.2 and OpenCASCADE 7.8.1 into the Rust executable,
including the MSVC static CRT: the inspection and conversion workers then need no SDK at run time.

## Reproduce on Windows x64

Install Rust's MSVC target and Visual Studio with C++ tools, CMake and Ninja. From the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build_static_cad.ps1 -VisualStudioPath 'C:\path\to\Visual Studio' -Stage All
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_static_cad.ps1 -VisualStudioPath 'C:\path\to\Visual Studio' -Out out/static-cad-proof
```

Use a fresh output directory for verification. Source archives and SHA-256 hashes are pinned in
`scripts/native-sources.json`; downloads, sources and build products stay under ignored `out/`. The static
build of cad2bem also reads the licence files of Gmsh (with its bundled code), LEMON and OpenCASCADE from the
unpacked sources in `out/native-sources` (or `CAD2BEM_NATIVE_SOURCES`) to carry them in the executable.
The first C++ build is substantial; later invocations reuse it. `-Stage Occt` and `-Stage Gmsh`
allow separate builds; Gmsh requires the installed OCCT prefix. `-Jobs` defaults to 8.

For a Rust build after creating the SDK, dot-source `scripts/static_env.ps1`: it sets
`CAD2BEM_STATIC_CAD_PREFIX` (default `out/static-cad/install`), `CARGO_TARGET_DIR=target/static` and the
Rust flags (`-C target-feature=+crt-static`, and `--remap-path-prefix` for the Cargo home and the repository,
in `CARGO_ENCODED_RUSTFLAGS`), removes `CAD2BEM_GMSH_LIBRARY` and defines `$StaticCargo`
(`--locked --release --target x86_64-pc-windows-msvc --features static-gmsh`). The conversion tests run here.

```powershell
. scripts/static_env.ps1
cargo build @StaticCargo --bin cad2bem --example static_cad_probe
cargo test @StaticCargo -- --include-ignored
```

The static backend rejects explicit external library paths. Remove `CAD2BEM_GMSH_LIBRARY` and do not pass
`--gmsh-library`. Default builds retain the existing dynamic SDK backend. Both use the same isolated
`--worker REQUEST RESULT` process protocol and preserve Gmsh allocation/free pairs.

## Build profile and evidence

OCCT uses static libraries, `/MT` and the STEP/IGES/Boolean/mesh toolkit dependency closure.
Gmsh retains OCC, surface meshing, the matching recombination (with LEMON's perfect matching in place of
Blossom IV, which is left out for its licence: `-Matching Lemon`, the default) and quadrilateral meshing tools. Its own GUI,
Python wrappers, BLAS/LAPACK and unnecessary third-party facilities are disabled. The builder asserts
the required features and creates a link manifest including transitive OCCT providers. For pinned Gmsh
4.15.2, a checked, idempotent CMake patch removes forced MSVC `dllimport` for static OCC. CMake 4's
minimum-policy override supports the older OCCT project. Build scripts and source locks are retained
so this is reproducible; renamed DLL import libraries do not qualify.

Verification builds the actual GUI/CLI EXE and a Rust-only analytic probe, audits both PE import tables,
copies only these EXEs outside the checkout, removes developer paths and CAD resource variables,
and temporarily hides the entire SDK install prefix. The SDK and environment are restored in `finally`.
Normal Windows system DLLs are allowed; private CAD, interpreter, numerical and VC runtime DLLs are rejected.

The verified analytic box is 4 by 2 by 6 metres: volume 48, waterline fragments 24/24 and quarter-wetted
volume 6. STEP and IGES reimport retain six CAD faces and metre extents. Triangle and recombined quad
meshing run through the actual static libraries. The reference run produced 273 quads with 8 remaining
triangles; recombination is not a guarantee of all-quad output. Fresh same-EXE workers import IGES first
and STEP independently. JSON reports and subprocess logs are kept in the selected proof directory.

The local release EXE was about 48.7 MB. SDK build files/resources are development inputs, not required
by the verified CAD workers at runtime. This is same-host isolation evidence, not fresh-OS certification.
Raw closed-solid meshes still include caps: they are not solver-ready wetted BEM exports.

## Paths of the build machine

The executable carries no path of the machine that built it. The C++ builds set `CL=/d1trimfile:...` for the
source and build folders, so `__FILE__` in Gmsh's and OpenCASCADE's messages starts at the source folder;
`cl` reads `CL` on every compile, but Ninja does not see it, so an SDK built before this needs a clean build
(a fresh `-BuildRoot`). The Rust flags remap the Cargo home to `/cargo` and the repository to `/cad2bem` in
panic messages and debug information.

## Release

`scripts/package_release.ps1` assembles a release from the static executable: the executable with the
licences of everything it links, and its corresponding source, each as a folder and a zip (see
`THIRD_PARTY_NOTICES.md`). Run it on a committed tree whose executable was built from that commit:

```powershell
. scripts/static_env.ps1
cargo build @StaticCargo --bin cad2bem
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package_release.ps1
```

It refuses uncommitted changes, an executable whose version differs from `Cargo.toml`, source archives that
do not match `scripts/native-sources.json`, and an executable that contains the user profile or repository
path. `-NoVendor` leaves out the vendored crate sources (about 80 MB zipped).

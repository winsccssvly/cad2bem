// SPDX-License-Identifier: GPL-3.0-or-later
//! Typed table of the Gmsh 4.15 C API functions the application uses.
//!
//! One list generates both backends: the default build resolves the functions from a Gmsh SDK library at
//! run time; the `static-gmsh` build takes the addresses of the statically linked functions. Signatures follow
//! the pinned `gmshc.h`. Callers go through [`crate::cad::session::Session`], which checks errors and frees
//! every buffer Gmsh allocates.

#![allow(non_snake_case)]

use std::ffi::{c_char, c_int, c_void};

type Int = c_int;
type Size = usize;
type Real = f64;

macro_rules! gmsh_api {
    ($($name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
        /// Function pointers of the Gmsh C API.
        pub struct Api {
            $(pub $name: unsafe extern "C" fn($($ty),*) $(-> $ret)?,)*
        }

        #[cfg(feature = "static-gmsh")]
        mod linked {
            use super::*;
            unsafe extern "C" {
                $(pub fn $name($($arg: $ty),*) $(-> $ret)?;)*
            }
        }

        impl Api {
            /// The statically linked functions.
            #[cfg(feature = "static-gmsh")]
            pub fn linked() -> Self {
                Self { $($name: linked::$name,)* }
            }

            /// Resolve every function from a loaded SDK library.
            ///
            /// # Safety
            /// The library must be a Gmsh 4.15 SDK whose functions have the signatures listed here.
            #[cfg(not(feature = "static-gmsh"))]
            pub unsafe fn load(library: &libloading::Library) -> anyhow::Result<Self> {
                Ok(Self {
                    $($name: unsafe {
                        *library.get(concat!(stringify!($name), "\0").as_bytes())?
                    },)*
                })
            }
        }
    };
}

gmsh_api! {
    gmshInitialize(argc: Int, argv: *mut *mut c_char, read_config: Int, run: Int, ierr: *mut Int);
    gmshFinalize(ierr: *mut Int);
    gmshFree(p: *mut c_void);
    gmshClear(ierr: *mut Int);
    gmshWrite(file: *const c_char, ierr: *mut Int);
    gmshLoggerGetLastError(error: *mut *mut c_char, ierr: *mut Int);
    gmshOptionRestoreDefaults(ierr: *mut Int);
    gmshOptionSetNumber(name: *const c_char, value: Real, ierr: *mut Int);
    gmshOptionSetString(name: *const c_char, value: *const c_char, ierr: *mut Int);
    gmshOptionGetNumber(name: *const c_char, value: *mut Real, ierr: *mut Int);
    gmshOptionGetString(name: *const c_char, value: *mut *mut c_char, ierr: *mut Int);
    gmshModelAdd(name: *const c_char, ierr: *mut Int);
    gmshModelRemove(ierr: *mut Int);
    gmshModelSetCurrent(name: *const c_char, ierr: *mut Int);
    gmshModelGetCurrent(name: *mut *mut c_char, ierr: *mut Int);
    gmshModelGetEntities(dim_tags: *mut *mut Int, dim_tags_n: *mut Size, dim: Int, ierr: *mut Int);
    gmshModelGetType(dim: Int, tag: Int, kind: *mut *mut c_char, ierr: *mut Int);
    gmshModelGetBoundingBox(
        dim: Int, tag: Int, x0: *mut Real, y0: *mut Real, z0: *mut Real, x1: *mut Real, y1: *mut Real,
        z1: *mut Real, ierr: *mut Int,
    );
    gmshModelGetAdjacencies(
        dim: Int, tag: Int, up: *mut *mut Int, up_n: *mut Size, down: *mut *mut Int, down_n: *mut Size,
        ierr: *mut Int,
    );
    gmshModelSetVisibility(dim_tags: *const Int, dim_tags_n: Size, value: Int, recursive: Int, ierr: *mut Int);
    gmshModelGetBoundary(
        dim_tags: *const Int, dim_tags_n: Size, out: *mut *mut Int, out_n: *mut Size, combined: Int,
        oriented: Int, recursive: Int, ierr: *mut Int,
    );
    gmshModelGetParametrizationBounds(
        dim: Int, tag: Int, min: *mut *mut Real, min_n: *mut Size, max: *mut *mut Real, max_n: *mut Size,
        ierr: *mut Int,
    );
    gmshModelGetValue(
        dim: Int, tag: Int, param: *const Real, param_n: Size, coord: *mut *mut Real, coord_n: *mut Size,
        ierr: *mut Int,
    );
    gmshModelGetNormal(
        tag: Int, param: *const Real, param_n: Size, normals: *mut *mut Real, normals_n: *mut Size, ierr: *mut Int,
    );
    gmshModelGetClosestPoint(
        dim: Int, tag: Int, coord: *const Real, coord_n: Size, closest: *mut *mut Real, closest_n: *mut Size,
        param: *mut *mut Real, param_n: *mut Size, ierr: *mut Int,
    );
    gmshModelGetParametrization(
        dim: Int, tag: Int, coord: *const Real, coord_n: Size, param: *mut *mut Real, param_n: *mut Size,
        ierr: *mut Int,
    );
    gmshModelGetDerivative(
        dim: Int, tag: Int, param: *const Real, param_n: Size, out: *mut *mut Real, out_n: *mut Size, ierr: *mut Int,
    );
    gmshModelGeoAddPoint(x: Real, y: Real, z: Real, mesh_size: Real, tag: Int, ierr: *mut Int) -> Int;
    gmshModelGeoAddLine(start: Int, end: Int, tag: Int, ierr: *mut Int) -> Int;
    gmshModelGeoAddCurveLoop(curves: *const Int, curves_n: Size, tag: Int, reorient: Int, ierr: *mut Int) -> Int;
    gmshModelGeoAddPlaneSurface(wires: *const Int, wires_n: Size, tag: Int, ierr: *mut Int) -> Int;
    gmshModelGeoSynchronize(ierr: *mut Int);
    gmshModelIsInside(dim: Int, tag: Int, coord: *const Real, coord_n: Size, parametric: Int, ierr: *mut Int) -> Int;
    gmshModelOccImportShapes(
        file: *const c_char, out: *mut *mut Int, out_n: *mut Size, highest_dim_only: Int, format: *const c_char,
        ierr: *mut Int,
    );
    gmshModelOccSynchronize(ierr: *mut Int);
    gmshModelOccAddPoint(x: Real, y: Real, z: Real, mesh_size: Real, tag: Int, ierr: *mut Int) -> Int;
    gmshModelOccAddLine(start: Int, end: Int, tag: Int, ierr: *mut Int) -> Int;
    gmshModelOccAddCurveLoop(curves: *const Int, curves_n: Size, tag: Int, ierr: *mut Int) -> Int;
    gmshModelOccAddPlaneSurface(wires: *const Int, wires_n: Size, tag: Int, ierr: *mut Int) -> Int;
    gmshModelOccAddBox(x: Real, y: Real, z: Real, dx: Real, dy: Real, dz: Real, tag: Int, ierr: *mut Int) -> Int;
    gmshModelOccAddRectangle(
        x: Real, y: Real, z: Real, dx: Real, dy: Real, tag: Int, rounded_radius: Real, ierr: *mut Int,
    ) -> Int;
    gmshModelOccRotate(
        dim_tags: *const Int, dim_tags_n: Size, x: Real, y: Real, z: Real, ax: Real, ay: Real, az: Real,
        angle: Real, ierr: *mut Int,
    );
    gmshModelOccCopy(dim_tags: *const Int, dim_tags_n: Size, out: *mut *mut Int, out_n: *mut Size, ierr: *mut Int);
    gmshModelOccRemove(dim_tags: *const Int, dim_tags_n: Size, recursive: Int, ierr: *mut Int);
    gmshModelOccFragment(
        object: *const Int, object_n: Size, tool: *const Int, tool_n: Size, out: *mut *mut Int, out_n: *mut Size,
        map: *mut *mut *mut Int, map_n: *mut *mut Size, map_nn: *mut Size, tag: Int, remove_object: Int,
        remove_tool: Int, ierr: *mut Int,
    );
    gmshModelOccIntersect(
        object: *const Int, object_n: Size, tool: *const Int, tool_n: Size, out: *mut *mut Int, out_n: *mut Size,
        map: *mut *mut *mut Int, map_n: *mut *mut Size, map_nn: *mut Size, tag: Int, remove_object: Int,
        remove_tool: Int, ierr: *mut Int,
    );
    gmshModelOccCut(
        object: *const Int, object_n: Size, tool: *const Int, tool_n: Size, out: *mut *mut Int, out_n: *mut Size,
        map: *mut *mut *mut Int, map_n: *mut *mut Size, map_nn: *mut Size, tag: Int, remove_object: Int,
        remove_tool: Int, ierr: *mut Int,
    );
    gmshModelOccFuse(
        object: *const Int, object_n: Size, tool: *const Int, tool_n: Size, out: *mut *mut Int, out_n: *mut Size,
        map: *mut *mut *mut Int, map_n: *mut *mut Size, map_nn: *mut Size, tag: Int, remove_object: Int,
        remove_tool: Int, ierr: *mut Int,
    );
    gmshModelOccHealShapes(
        out: *mut *mut Int, out_n: *mut Size, dim_tags: *const Int, dim_tags_n: Size, tolerance: Real,
        fix_degenerated: Int, fix_small_edges: Int, fix_small_faces: Int, sew_faces: Int, make_solids: Int,
        ierr: *mut Int,
    );
    gmshModelOccGetCurveLoops(
        surface: Int, loops: *mut *mut Int, loops_n: *mut Size, curves: *mut *mut *mut Int, curves_n: *mut *mut Size,
        curves_nn: *mut Size, ierr: *mut Int,
    );
    gmshModelOccGetMass(dim: Int, tag: Int, mass: *mut Real, ierr: *mut Int);
    gmshModelOccGetCenterOfMass(dim: Int, tag: Int, x: *mut Real, y: *mut Real, z: *mut Real, ierr: *mut Int);
    gmshModelMeshClear(dim_tags: *const Int, dim_tags_n: Size, ierr: *mut Int);
    gmshModelMeshGenerate(dim: Int, ierr: *mut Int);
    gmshModelMeshFieldList(tags: *mut *mut Int, tags_n: *mut Size, ierr: *mut Int);
    gmshModelMeshFieldRemove(tag: Int, ierr: *mut Int);
    gmshModelMeshFieldSetAsBackgroundMesh(tag: Int, ierr: *mut Int);
    gmshViewGetTags(tags: *mut *mut Int, tags_n: *mut Size, ierr: *mut Int);
    gmshViewRemove(tag: Int, ierr: *mut Int);
    gmshModelMeshSetSizeCallback(
        callback: Option<unsafe extern "C" fn(Int, Int, Real, Real, Real, Real, *mut c_void) -> Real>,
        data: *mut c_void, ierr: *mut Int,
    );
    gmshModelMeshRemoveSizeCallback(ierr: *mut Int);
    gmshModelMeshGetNodes(
        tags: *mut *mut Size, tags_n: *mut Size, coord: *mut *mut Real, coord_n: *mut Size, param: *mut *mut Real,
        param_n: *mut Size, dim: Int, tag: Int, include_boundary: Int, return_param: Int, ierr: *mut Int,
    );
    gmshModelMeshGetElements(
        types: *mut *mut Int, types_n: *mut Size, element_tags: *mut *mut *mut Size, element_tags_n: *mut *mut Size,
        element_tags_nn: *mut Size, node_tags: *mut *mut *mut Size, node_tags_n: *mut *mut Size,
        node_tags_nn: *mut Size, dim: Int, tag: Int, ierr: *mut Int,
    );
    gmshModelMeshGetElementProperties(
        element_type: Int, name: *mut *mut c_char, dim: *mut Int, order: *mut Int, nodes: *mut Int,
        local: *mut *mut Real, local_n: *mut Size, primary: *mut Int, ierr: *mut Int,
    );
    gmshModelMeshGetElementTypes(types: *mut *mut Int, types_n: *mut Size, dim: Int, tag: Int, ierr: *mut Int);
    gmshModelMeshGetElementsByType(
        element_type: Int, element_tags: *mut *mut Size, element_tags_n: *mut Size, node_tags: *mut *mut Size,
        node_tags_n: *mut Size, tag: Int, task: Size, tasks: Size, ierr: *mut Int,
    );
}

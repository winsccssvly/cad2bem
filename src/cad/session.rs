// SPDX-License-Identifier: GPL-3.0-or-later
//! A Gmsh session with safe calls: every error is checked and every Gmsh-allocated buffer is copied and freed.
//!
//! Gmsh is one global state per process, so a session lives only inside a private worker process. Dropping it
//! finalizes Gmsh. The calls follow the Gmsh API groups (`model`, `model.occ`, `model.mesh`).

use super::api::Api;
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{CStr, CString, c_char, c_int},
    path::Path,
    ptr,
};

/// An entity as (dimension, tag).
pub type DimTag = (i32, i32);

/// The output of a Boolean operation: the result entities and, per input entity, the entities it became.
pub struct Boolean {
    pub out: Vec<DimTag>,
    pub map: Vec<Vec<DimTag>>,
}

/// An open Gmsh session; dropping it finalizes Gmsh.
///
/// A dynamically loaded SDK library is never unloaded: OpenCASCADE starts a thread pool (for example to
/// triangulate shapes for `Geometry.OCCBoundsUseStl` bounding boxes), its threads outlive `gmshFinalize`, and
/// unloading the library while they exist deadlocks the process. The library stays until the process exits.
pub struct Session {
    api: Api,
    #[cfg(not(feature = "static-gmsh"))]
    _library: std::mem::ManuallyDrop<libloading::Library>,
    /// The size table read by the mesh size callback while it is set.
    sizes: std::cell::RefCell<Option<Box<SizeTable>>>,
}

/// Mesh sizes per entity (dim, tag); other entities keep the size Gmsh proposes.
pub type SizeTable = std::collections::HashMap<(i32, i32), f64>;

unsafe extern "C" fn size_callback(
    dim: c_int,
    tag: c_int,
    _x: f64,
    _y: f64,
    _z: f64,
    lc: f64,
    data: *mut std::ffi::c_void,
) -> f64 {
    // SAFETY: data is the boxed table the session owns until the callback is removed.
    let table = unsafe { &*(data as *const SizeTable) };
    table.get(&(dim, tag)).copied().unwrap_or(lc)
}

fn flat(dim_tags: &[DimTag]) -> Vec<c_int> {
    dim_tags.iter().flat_map(|&(d, t)| [d, t]).collect()
}

fn pairs(flat: &[c_int]) -> Result<Vec<DimTag>> {
    ensure!(flat.len().is_multiple_of(2), "Malformed Gmsh entity list");
    Ok(flat
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| (p[0], p[1]))
        .collect())
}

fn points(flat: Vec<f64>) -> Result<Vec<[f64; 3]>> {
    ensure!(flat.len().is_multiple_of(3), "Malformed Gmsh coordinates");
    Ok(flat.as_chunks::<3>().0.to_vec())
}

impl Session {
    /// Start Gmsh from the statically linked library.
    #[cfg(feature = "static-gmsh")]
    pub fn open(library: Option<&Path>) -> Result<Self> {
        ensure!(
            library.is_none(),
            "Static CAD build rejects external SDK paths"
        );
        Self::start(Self {
            api: Api::linked(),
            sizes: Default::default(),
        })
    }

    /// Start Gmsh from an SDK library: `library`, `CAD2BEM_GMSH_LIBRARY`, next to the executable, the active
    /// conda environment, then the system search path.
    #[cfg(not(feature = "static-gmsh"))]
    pub fn open(library: Option<&Path>) -> Result<Self> {
        let mut errors = Vec::new();
        for path in super::gmsh::library_candidates(library)? {
            // SAFETY: the library is a Gmsh SDK loaded in an isolated worker; the symbols use the documented
            // Gmsh 4.15 C ABI listed in `api.rs`, and the version is checked right after initialization.
            let library = match unsafe { libloading::Library::new(&path) } {
                Ok(library) => library,
                Err(error) => {
                    errors.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            let api = unsafe { Api::load(&library) }
                .with_context(|| format!("{} is not a Gmsh 4.15 SDK", path.display()))?;
            return Self::start(Self {
                api,
                _library: std::mem::ManuallyDrop::new(library),
                sizes: Default::default(),
            });
        }
        bail!(
            "Gmsh 4.15 SDK library not found. Select its DLL or set CAD2BEM_GMSH_LIBRARY.\n{}",
            errors.join("\n")
        )
    }

    fn start(session: Self) -> Result<Self> {
        let mut error = 0;
        unsafe { (session.api.gmshInitialize)(0, ptr::null_mut(), 0, 0, &mut error) };
        session.check(error)?;
        session.number("General.Terminal", 0.)?;
        let version = session.option_string("General.Version")?;
        ensure!(
            version.starts_with("4.15."),
            "Expected Gmsh 4.15.x SDK, got {version}"
        );
        Ok(session)
    }

    fn check(&self, error: c_int) -> Result<()> {
        if error == 0 {
            return Ok(());
        }
        let mut message = ptr::null_mut();
        let mut ignored = 0;
        unsafe { (self.api.gmshLoggerGetLastError)(&mut message, &mut ignored) };
        let text = unsafe { self.take_string(message) };
        bail!("Gmsh error {error}: {text}")
    }

    /// Copy and free a Gmsh-allocated array.
    unsafe fn take<T: Copy>(&self, data: *mut T, count: usize) -> Vec<T> {
        if data.is_null() {
            return Vec::new();
        }
        let out = unsafe { std::slice::from_raw_parts(data, count) }.to_vec();
        unsafe { (self.api.gmshFree)(data.cast()) };
        out
    }

    /// Copy and free a Gmsh-allocated array of arrays.
    unsafe fn take_nested<T: Copy>(
        &self,
        data: *mut *mut T,
        sizes: *mut usize,
        count: usize,
    ) -> Vec<Vec<T>> {
        let sizes = unsafe { self.take(sizes, count) };
        let rows = unsafe { self.take(data, count) };
        rows.into_iter()
            .zip(sizes)
            .map(|(row, n)| unsafe { self.take(row, n) })
            .collect()
    }

    unsafe fn take_string(&self, data: *mut c_char) -> String {
        if data.is_null() {
            return String::new();
        }
        let out = unsafe { CStr::from_ptr(data) }
            .to_string_lossy()
            .into_owned();
        unsafe { (self.api.gmshFree)(data.cast()) };
        out
    }

    /// Restore every option to its default (as in a fresh session), keeping the terminal quiet.
    pub fn restore_defaults(&self) -> Result<()> {
        let mut error = 0;
        unsafe { (self.api.gmshOptionRestoreDefaults)(&mut error) };
        self.check(error)?;
        self.number("General.Terminal", 0.)
    }

    /// Clear all models and start an empty one, as `gmsh.clear()` then `gmsh.model.add(name)`.
    pub fn reset(&self, name: &str) -> Result<()> {
        let mut error = 0;
        unsafe { (self.api.gmshClear)(&mut error) };
        self.check(error)?;
        self.add_model(name)
    }

    /// Write the current model; the format follows the extension.
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut error = 0;
        let file = CString::new(path.to_str().context("Output path is not valid Unicode")?)?;
        unsafe { (self.api.gmshWrite)(file.as_ptr(), &mut error) };
        self.check(error)
    }

    // -- options and models --------------------------------------------------------------------

    /// Set a numeric Gmsh option (`gmshOptionSetNumber`).
    pub fn number(&self, name: &str, value: f64) -> Result<()> {
        let mut error = 0;
        let name = CString::new(name)?;
        unsafe { (self.api.gmshOptionSetNumber)(name.as_ptr(), value, &mut error) };
        self.check(error)
    }

    /// Set a string Gmsh option (`gmshOptionSetString`).
    pub fn string(&self, name: &str, value: &str) -> Result<()> {
        let mut error = 0;
        let (name, value) = (CString::new(name)?, CString::new(value)?);
        unsafe { (self.api.gmshOptionSetString)(name.as_ptr(), value.as_ptr(), &mut error) };
        self.check(error)
    }

    /// The value of a numeric Gmsh option (`gmshOptionGetNumber`).
    pub fn option_number(&self, name: &str) -> Result<f64> {
        let mut error = 0;
        let mut value = 0.;
        let name = CString::new(name)?;
        unsafe { (self.api.gmshOptionGetNumber)(name.as_ptr(), &mut value, &mut error) };
        self.check(error)?;
        Ok(value)
    }

    /// The value of a string Gmsh option (`gmshOptionGetString`).
    pub fn option_string(&self, name: &str) -> Result<String> {
        let mut error = 0;
        let mut value = ptr::null_mut();
        let name = CString::new(name)?;
        unsafe { (self.api.gmshOptionGetString)(name.as_ptr(), &mut value, &mut error) };
        let value = unsafe { self.take_string(value) };
        self.check(error)?;
        Ok(value)
    }

    /// Add a model and make it current (`gmshModelAdd`).
    pub fn add_model(&self, name: &str) -> Result<()> {
        let mut error = 0;
        let name = CString::new(name)?;
        unsafe { (self.api.gmshModelAdd)(name.as_ptr(), &mut error) };
        self.check(error)
    }

    /// Remove the current model.
    pub fn remove_model(&self) -> Result<()> {
        let mut error = 0;
        unsafe { (self.api.gmshModelRemove)(&mut error) };
        self.check(error)
    }

    /// The name of the current model (`gmshModelGetCurrent`).
    pub fn current_model(&self) -> Result<String> {
        let mut error = 0;
        let mut name = ptr::null_mut();
        unsafe { (self.api.gmshModelGetCurrent)(&mut name, &mut error) };
        let name = unsafe { self.take_string(name) };
        self.check(error)?;
        Ok(name)
    }

    /// Make the named model current (`gmshModelSetCurrent`).
    pub fn set_current_model(&self, name: &str) -> Result<()> {
        let mut error = 0;
        let name = CString::new(name)?;
        unsafe { (self.api.gmshModelSetCurrent)(name.as_ptr(), &mut error) };
        self.check(error)
    }

    // -- model queries -------------------------------------------------------------------------

    /// The entities of a dimension (-1: all).
    pub fn entities(&self, dim: i32) -> Result<Vec<DimTag>> {
        let mut error = 0;
        let (mut data, mut n) = (ptr::null_mut(), 0);
        unsafe { (self.api.gmshModelGetEntities)(&mut data, &mut n, dim, &mut error) };
        let data = unsafe { self.take(data, n) };
        self.check(error)?;
        pairs(&data)
    }

    /// The tags of the entities of a dimension.
    pub fn tags(&self, dim: i32) -> Result<Vec<i32>> {
        Ok(self.entities(dim)?.into_iter().map(|(_, t)| t).collect())
    }

    /// The type name of an entity, such as `Plane` (`gmshModelGetType`).
    pub fn kind(&self, dim: i32, tag: i32) -> Result<String> {
        let mut error = 0;
        let mut kind = ptr::null_mut();
        unsafe { (self.api.gmshModelGetType)(dim, tag, &mut kind, &mut error) };
        let kind = unsafe { self.take_string(kind) };
        self.check(error)?;
        Ok(kind)
    }

    /// The bounding box [x0, y0, z0, x1, y1, z1] of an entity, or of the whole model with (-1, -1).
    pub fn bounding_box(&self, dim: i32, tag: i32) -> Result<[f64; 6]> {
        let mut error = 0;
        let mut b = [0.; 6];
        let p = b.as_mut_ptr();
        unsafe {
            (self.api.gmshModelGetBoundingBox)(
                dim,
                tag,
                p,
                p.add(1),
                p.add(2),
                p.add(3),
                p.add(4),
                p.add(5),
                &mut error,
            )
        };
        self.check(error)?;
        Ok(b)
    }

    /// The entities of dimension dim + 1 and dim - 1 adjacent to an entity (tags).
    pub fn adjacencies(&self, dim: i32, tag: i32) -> Result<(Vec<i32>, Vec<i32>)> {
        let mut error = 0;
        let (mut up, mut nu, mut down, mut nd) = (ptr::null_mut(), 0, ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelGetAdjacencies)(
                dim, tag, &mut up, &mut nu, &mut down, &mut nd, &mut error,
            )
        };
        let (up, down) = unsafe { (self.take(up, nu), self.take(down, nd)) };
        self.check(error)?;
        Ok((up, down))
    }

    /// Show or hide entities (hidden ones are not meshed while `Mesh.MeshOnlyVisible` is set).
    pub fn set_visibility(
        &self,
        dim_tags: &[DimTag],
        visible: bool,
        recursive: bool,
    ) -> Result<()> {
        let flat: Vec<c_int> = dim_tags.iter().flat_map(|&(d, t)| [d, t]).collect();
        let mut error = 0;
        unsafe {
            (self.api.gmshModelSetVisibility)(
                flat.as_ptr(),
                flat.len(),
                visible.into(),
                recursive.into(),
                &mut error,
            )
        };
        self.check(error)
    }

    /// The boundary entities of `dim_tags` (`gmshModelGetBoundary`).
    pub fn boundary(
        &self,
        dim_tags: &[DimTag],
        combined: bool,
        oriented: bool,
        recursive: bool,
    ) -> Result<Vec<DimTag>> {
        let mut error = 0;
        let input = flat(dim_tags);
        let (mut out, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelGetBoundary)(
                input.as_ptr(),
                input.len(),
                &mut out,
                &mut n,
                combined.into(),
                oriented.into(),
                recursive.into(),
                &mut error,
            )
        };
        let out = unsafe { self.take(out, n) };
        self.check(error)?;
        pairs(&out)
    }

    /// The parameter bounds (min, max) of an entity.
    pub fn parametrization_bounds(&self, dim: i32, tag: i32) -> Result<(Vec<f64>, Vec<f64>)> {
        let mut error = 0;
        let (mut lo, mut nl, mut hi, mut nh) = (ptr::null_mut(), 0, ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelGetParametrizationBounds)(
                dim, tag, &mut lo, &mut nl, &mut hi, &mut nh, &mut error,
            )
        };
        let (lo, hi) = unsafe { (self.take(lo, nl), self.take(hi, nh)) };
        self.check(error)?;
        Ok((lo, hi))
    }

    /// Points of an entity at flat parameters (u for curves, u v pairs for faces).
    pub fn value(&self, dim: i32, tag: i32, param: &[f64]) -> Result<Vec<[f64; 3]>> {
        let mut error = 0;
        let (mut out, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelGetValue)(
                dim,
                tag,
                param.as_ptr(),
                param.len(),
                &mut out,
                &mut n,
                &mut error,
            )
        };
        let out = unsafe { self.take(out, n) };
        self.check(error)?;
        points(out)
    }

    /// The normals of a face at flat u v parameters.
    pub fn normal(&self, tag: i32, param: &[f64]) -> Result<Vec<[f64; 3]>> {
        let mut error = 0;
        let (mut out, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelGetNormal)(
                tag,
                param.as_ptr(),
                param.len(),
                &mut out,
                &mut n,
                &mut error,
            )
        };
        let out = unsafe { self.take(out, n) };
        self.check(error)?;
        points(out)
    }

    /// The closest points of an entity to the given points, and their flat parameters.
    pub fn closest_point(
        &self,
        dim: i32,
        tag: i32,
        coord: &[[f64; 3]],
    ) -> Result<(Vec<[f64; 3]>, Vec<f64>)> {
        let mut error = 0;
        let input = coord.as_flattened();
        let (mut out, mut no, mut param, mut np) = (ptr::null_mut(), 0, ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelGetClosestPoint)(
                dim,
                tag,
                input.as_ptr(),
                input.len(),
                &mut out,
                &mut no,
                &mut param,
                &mut np,
                &mut error,
            )
        };
        let (out, param) = unsafe { (self.take(out, no), self.take(param, np)) };
        self.check(error)?;
        Ok((points(out)?, param))
    }

    /// The number of the given points (flat coordinates or parameters) inside an entity.
    pub fn inside(&self, dim: i32, tag: i32, coord: &[f64], parametric: bool) -> Result<i32> {
        let mut error = 0;
        let count = unsafe {
            (self.api.gmshModelIsInside)(
                dim,
                tag,
                coord.as_ptr(),
                coord.len(),
                parametric.into(),
                &mut error,
            )
        };
        self.check(error)?;
        Ok(count)
    }

    // -- OpenCASCADE kernel -------------------------------------------------------------------

    /// Import a STEP or IGES file into the OpenCASCADE kernel, highest-dimensional entities only
    /// (`gmshModelOccImportShapes`): the imported entities.
    pub fn import_shapes(&self, path: &Path) -> Result<Vec<DimTag>> {
        let mut error = 0;
        let file = CString::new(path.to_str().context("CAD path is not valid Unicode")?)?;
        let (mut out, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelOccImportShapes)(
                file.as_ptr(),
                &mut out,
                &mut n,
                1,
                c"".as_ptr(),
                &mut error,
            )
        };
        let out = unsafe { self.take(out, n) };
        self.check(error)?;
        pairs(&out)
    }

    /// Synchronize the model with the OpenCASCADE kernel (`gmshModelOccSynchronize`).
    pub fn synchronize(&self) -> Result<()> {
        let mut error = 0;
        unsafe { (self.api.gmshModelOccSynchronize)(&mut error) };
        self.check(error)
    }

    /// Add an OpenCASCADE point (`gmshModelOccAddPoint`): its tag.
    pub fn add_point(&self, x: [f64; 3]) -> Result<i32> {
        let mut error = 0;
        let tag = unsafe { (self.api.gmshModelOccAddPoint)(x[0], x[1], x[2], 0., -1, &mut error) };
        self.check(error)?;
        Ok(tag)
    }

    /// Add a straight line between two points (`gmshModelOccAddLine`): its tag.
    pub fn add_line(&self, start: i32, end: i32) -> Result<i32> {
        let mut error = 0;
        let tag = unsafe { (self.api.gmshModelOccAddLine)(start, end, -1, &mut error) };
        self.check(error)?;
        Ok(tag)
    }

    /// Add a curve loop of these curves (`gmshModelOccAddCurveLoop`): its tag.
    pub fn add_curve_loop(&self, curves: &[i32]) -> Result<i32> {
        let mut error = 0;
        let tag = unsafe {
            (self.api.gmshModelOccAddCurveLoop)(curves.as_ptr(), curves.len(), -1, &mut error)
        };
        self.check(error)?;
        Ok(tag)
    }

    /// A plane surface bounded by curve loops: the outer loop first, then the holes.
    pub fn add_plane_surface(&self, loops: &[i32]) -> Result<i32> {
        let mut error = 0;
        let tag = unsafe {
            (self.api.gmshModelOccAddPlaneSurface)(loops.as_ptr(), loops.len(), -1, &mut error)
        };
        self.check(error)?;
        Ok(tag)
    }

    /// Add a box of a corner and side lengths (`gmshModelOccAddBox`): its volume tag.
    pub fn add_box(&self, origin: [f64; 3], size: [f64; 3]) -> Result<i32> {
        let mut error = 0;
        let tag = unsafe {
            (self.api.gmshModelOccAddBox)(
                origin[0], origin[1], origin[2], size[0], size[1], size[2], -1, &mut error,
            )
        };
        self.check(error)?;
        Ok(tag)
    }

    /// A rectangle in the plane z = `origin[2]`.
    pub fn add_rectangle(&self, origin: [f64; 3], dx: f64, dy: f64) -> Result<i32> {
        let mut error = 0;
        let tag = unsafe {
            (self.api.gmshModelOccAddRectangle)(
                origin[0], origin[1], origin[2], dx, dy, -1, 0., &mut error,
            )
        };
        self.check(error)?;
        Ok(tag)
    }

    /// Rotate entities by `angle` (radians) about the axis through `point` (`gmshModelOccRotate`).
    pub fn rotate(
        &self,
        dim_tags: &[DimTag],
        point: [f64; 3],
        axis: [f64; 3],
        angle: f64,
    ) -> Result<()> {
        let mut error = 0;
        let input = flat(dim_tags);
        unsafe {
            (self.api.gmshModelOccRotate)(
                input.as_ptr(),
                input.len(),
                point[0],
                point[1],
                point[2],
                axis[0],
                axis[1],
                axis[2],
                angle,
                &mut error,
            )
        };
        self.check(error)
    }

    /// Copy entities (`gmshModelOccCopy`): the copies.
    pub fn copy(&self, dim_tags: &[DimTag]) -> Result<Vec<DimTag>> {
        let mut error = 0;
        let input = flat(dim_tags);
        let (mut out, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelOccCopy)(input.as_ptr(), input.len(), &mut out, &mut n, &mut error)
        };
        let out = unsafe { self.take(out, n) };
        self.check(error)?;
        pairs(&out)
    }

    /// Remove entities, and with `recursive` their boundaries (`gmshModelOccRemove`).
    pub fn remove(&self, dim_tags: &[DimTag], recursive: bool) -> Result<()> {
        let mut error = 0;
        let input = flat(dim_tags);
        unsafe {
            (self.api.gmshModelOccRemove)(input.as_ptr(), input.len(), recursive.into(), &mut error)
        };
        self.check(error)
    }

    /// Fragment object and tool into conforming pieces, removing both (`gmshModelOccFragment`).
    pub fn fragment(&self, object: &[DimTag], tool: &[DimTag]) -> Result<Boolean> {
        self.boolean(self.api.gmshModelOccFragment, object, tool, true, true)
    }

    /// The intersection of object and tool, removing both (`gmshModelOccIntersect`).
    pub fn intersect(&self, object: &[DimTag], tool: &[DimTag]) -> Result<Boolean> {
        self.boolean(self.api.gmshModelOccIntersect, object, tool, true, true)
    }

    /// Cut the tool out of the object (`gmshModelOccCut`).
    pub fn cut(
        &self,
        object: &[DimTag],
        tool: &[DimTag],
        remove_object: bool,
        remove_tool: bool,
    ) -> Result<Boolean> {
        self.boolean(
            self.api.gmshModelOccCut,
            object,
            tool,
            remove_object,
            remove_tool,
        )
    }

    /// The union of object and tool, removing both (`gmshModelOccFuse`).
    pub fn fuse(&self, object: &[DimTag], tool: &[DimTag]) -> Result<Boolean> {
        self.boolean(self.api.gmshModelOccFuse, object, tool, true, true)
    }

    #[allow(clippy::type_complexity)]
    fn boolean(
        &self,
        operation: unsafe extern "C" fn(
            *const c_int,
            usize,
            *const c_int,
            usize,
            *mut *mut c_int,
            *mut usize,
            *mut *mut *mut c_int,
            *mut *mut usize,
            *mut usize,
            c_int,
            c_int,
            c_int,
            *mut c_int,
        ),
        object: &[DimTag],
        tool: &[DimTag],
        remove_object: bool,
        remove_tool: bool,
    ) -> Result<Boolean> {
        let mut error = 0;
        let (object, tool) = (flat(object), flat(tool));
        let (mut out, mut n) = (ptr::null_mut(), 0);
        let (mut map, mut map_n, mut map_nn) = (ptr::null_mut(), ptr::null_mut(), 0);
        unsafe {
            operation(
                object.as_ptr(),
                object.len(),
                tool.as_ptr(),
                tool.len(),
                &mut out,
                &mut n,
                &mut map,
                &mut map_n,
                &mut map_nn,
                -1,
                remove_object.into(),
                remove_tool.into(),
                &mut error,
            )
        };
        let out = unsafe { self.take(out, n) };
        let map = unsafe { self.take_nested(map, map_n, map_nn) };
        self.check(error)?;
        Ok(Boolean {
            out: pairs(&out)?,
            map: map.iter().map(|row| pairs(row)).collect::<Result<_>>()?,
        })
    }

    /// Sew the faces of the whole model within `tolerance` (only `sewFaces` of the healing steps), for
    /// loose faces.
    pub fn sew_faces(&self, tolerance: f64) -> Result<()> {
        let mut error = 0;
        let (mut out, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelOccHealShapes)(
                &mut out,
                &mut n,
                ptr::null(),
                0,
                tolerance,
                0,
                0,
                0,
                1,
                0,
                &mut error,
            )
        };
        unsafe { self.take(out, n) };
        self.check(error)
    }

    /// Length, area or volume of an entity.
    pub fn mass(&self, dim: i32, tag: i32) -> Result<f64> {
        let mut error = 0;
        let mut mass = 0.;
        unsafe { (self.api.gmshModelOccGetMass)(dim, tag, &mut mass, &mut error) };
        self.check(error)?;
        Ok(mass)
    }

    /// The centre of mass of an entity (`gmshModelOccGetCenterOfMass`).
    pub fn center_of_mass(&self, dim: i32, tag: i32) -> Result<[f64; 3]> {
        let mut error = 0;
        let mut c = [0.; 3];
        let p = c.as_mut_ptr();
        unsafe {
            (self.api.gmshModelOccGetCenterOfMass)(dim, tag, p, p.add(1), p.add(2), &mut error)
        };
        self.check(error)?;
        Ok(c)
    }

    /// First derivatives of an entity at flat parameters (a face: x_u then x_v per point, flat).
    pub fn derivative(&self, dim: i32, tag: i32, param: &[f64]) -> Result<Vec<f64>> {
        let mut error = 0;
        let (mut out, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelGetDerivative)(
                dim,
                tag,
                param.as_ptr(),
                param.len(),
                &mut out,
                &mut n,
                &mut error,
            )
        };
        let out = unsafe { self.take(out, n) };
        self.check(error)?;
        Ok(out)
    }

    /// A planar polygon face in the built-in kernel (for temporary chart meshes): its surface tag.
    pub fn geo_polygon(&self, points: &[[f64; 2]], size: f64) -> Result<i32> {
        let mut error = 0;
        let mut nodes = Vec::new();
        for p in points {
            nodes.push(unsafe {
                (self.api.gmshModelGeoAddPoint)(p[0], p[1], 0., size, -1, &mut error)
            });
            self.check(error)?;
        }
        let mut lines = Vec::new();
        for k in 0..nodes.len() {
            let (a, b) = (nodes[k], nodes[(k + 1) % nodes.len()]);
            lines.push(unsafe { (self.api.gmshModelGeoAddLine)(a, b, -1, &mut error) });
            self.check(error)?;
        }
        let lp = unsafe {
            (self.api.gmshModelGeoAddCurveLoop)(lines.as_ptr(), lines.len(), -1, 0, &mut error)
        };
        self.check(error)?;
        let surface = unsafe { (self.api.gmshModelGeoAddPlaneSurface)(&lp, 1, -1, &mut error) };
        self.check(error)?;
        unsafe { (self.api.gmshModelGeoSynchronize)(&mut error) };
        self.check(error)?;
        Ok(surface)
    }

    /// The parameters of points on an entity (u for curves, u v pairs for faces), flat.
    pub fn parametrization(&self, dim: i32, tag: i32, coord: &[[f64; 3]]) -> Result<Vec<f64>> {
        let mut error = 0;
        let input = coord.as_flattened();
        let (mut out, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelGetParametrization)(
                dim,
                tag,
                input.as_ptr(),
                input.len(),
                &mut out,
                &mut n,
                &mut error,
            )
        };
        let out = unsafe { self.take(out, n) };
        self.check(error)?;
        Ok(out)
    }

    /// The curve loops of a face: loop tags and the signed curve tags of each loop.
    pub fn curve_loops(&self, face: i32) -> Result<Vec<Vec<i32>>> {
        let mut error = 0;
        let (mut loops, mut nl) = (ptr::null_mut(), 0);
        let (mut curves, mut cn, mut cnn) = (ptr::null_mut(), ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelOccGetCurveLoops)(
                face,
                &mut loops,
                &mut nl,
                &mut curves,
                &mut cn,
                &mut cnn,
                &mut error,
            )
        };
        unsafe { self.take(loops, nl) };
        let curves = unsafe { self.take_nested(curves, cn, cnn) };
        self.check(error)?;
        Ok(curves)
    }

    // -- mesh -----------------------------------------------------------------------------------

    /// Clear the mesh of the whole model (`gmshModelMeshClear`).
    pub fn clear_mesh(&self) -> Result<()> {
        let mut error = 0;
        unsafe { (self.api.gmshModelMeshClear)(ptr::null(), 0, &mut error) };
        self.check(error)
    }

    /// Mesh sizes per entity for the next generation (a curve or point takes the smallest of its faces, as
    /// the caller fills the table).
    pub fn set_sizes(&self, table: SizeTable) -> Result<()> {
        let mut boxed = Box::new(table);
        let data = (boxed.as_mut() as *mut SizeTable).cast();
        let mut error = 0;
        unsafe { (self.api.gmshModelMeshSetSizeCallback)(Some(size_callback), data, &mut error) };
        self.check(error)?;
        *self.sizes.borrow_mut() = Some(boxed);
        Ok(())
    }

    /// Remove the mesh size callback that [`Self::set_sizes`] set (`gmshModelMeshRemoveSizeCallback`).
    pub fn clear_sizes(&self) -> Result<()> {
        let mut error = 0;
        unsafe { (self.api.gmshModelMeshRemoveSizeCallback)(&mut error) };
        self.check(error)?;
        *self.sizes.borrow_mut() = None;
        Ok(())
    }

    /// The mesh nodes on one entity (not its boundary): tags and coordinates.
    pub fn entity_nodes(&self, dim: i32, tag: i32) -> Result<(Vec<usize>, Vec<[f64; 3]>)> {
        let mut error = 0;
        let (mut tags, mut nt) = (ptr::null_mut(), 0);
        let (mut xyz, mut nx) = (ptr::null_mut(), 0);
        let (mut param, mut np) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelMeshGetNodes)(
                &mut tags, &mut nt, &mut xyz, &mut nx, &mut param, &mut np, dim, tag, 0, 0,
                &mut error,
            )
        };
        let (tags, xyz) = unsafe { (self.take(tags, nt), self.take(xyz, nx)) };
        unsafe { self.take(param, np) };
        self.check(error)?;
        Ok((tags, points(xyz)?))
    }

    /// Mesh the current model up to `dim`. Mesh size fields and post-processing views an earlier mesh left are
    /// removed first: the quasi-structured quad mesher keeps its guiding field (a view and the background field)
    /// and, finding it, uses it for the next mesh of the session instead of building one for the faces meshed then,
    /// so a face's quads followed the first face meshed. The application makes no fields or views of its own (sizes
    /// go through the size callback).
    pub fn generate(&self, dim: i32) -> Result<()> {
        self.remove_fields_and_views()?;
        let mut error = 0;
        unsafe { (self.api.gmshModelMeshGenerate)(dim, &mut error) };
        self.check(error)
    }

    /// Remove every mesh size field and post-processing view, and unset the background field.
    fn remove_fields_and_views(&self) -> Result<()> {
        let mut error = 0;
        let (mut tags, mut n) = (ptr::null_mut(), 0);
        unsafe { (self.api.gmshModelMeshFieldList)(&mut tags, &mut n, &mut error) };
        let fields = unsafe { self.take(tags, n) };
        self.check(error)?;
        for tag in fields {
            unsafe { (self.api.gmshModelMeshFieldRemove)(tag, &mut error) };
            self.check(error)?;
        }
        unsafe { (self.api.gmshModelMeshFieldSetAsBackgroundMesh)(0, &mut error) };
        self.check(error)?;
        let (mut tags, mut n) = (ptr::null_mut(), 0);
        unsafe { (self.api.gmshViewGetTags)(&mut tags, &mut n, &mut error) };
        let views = unsafe { self.take(tags, n) };
        self.check(error)?;
        for tag in views {
            unsafe { (self.api.gmshViewRemove)(tag, &mut error) };
            self.check(error)?;
        }
        Ok(())
    }

    /// All mesh nodes: tags and coordinates.
    pub fn nodes(&self) -> Result<(Vec<usize>, Vec<[f64; 3]>)> {
        let mut error = 0;
        let (mut tags, mut nt) = (ptr::null_mut(), 0);
        let (mut xyz, mut nx) = (ptr::null_mut(), 0);
        let (mut param, mut np) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelMeshGetNodes)(
                &mut tags, &mut nt, &mut xyz, &mut nx, &mut param, &mut np, -1, -1, 0, 0,
                &mut error,
            )
        };
        let (tags, xyz) = unsafe { (self.take(tags, nt), self.take(xyz, nx)) };
        unsafe { self.take(param, np) };
        self.check(error)?;
        let xyz = points(xyz)?;
        ensure!(
            xyz.len() == tags.len(),
            "Gmsh node coordinate count mismatch"
        );
        Ok((tags, xyz))
    }

    /// The elements of an entity, per element type: (type, node tags of all its elements).
    pub fn elements(&self, dim: i32, tag: i32) -> Result<Vec<(i32, Vec<usize>)>> {
        let mut error = 0;
        let (mut types, mut nt) = (ptr::null_mut(), 0);
        let (mut et, mut et_n, mut et_nn) = (ptr::null_mut(), ptr::null_mut(), 0);
        let (mut nd, mut nd_n, mut nd_nn) = (ptr::null_mut(), ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelMeshGetElements)(
                &mut types, &mut nt, &mut et, &mut et_n, &mut et_nn, &mut nd, &mut nd_n,
                &mut nd_nn, dim, tag, &mut error,
            )
        };
        let types = unsafe { self.take(types, nt) };
        unsafe { self.take_nested(et, et_n, et_nn) };
        let nodes = unsafe { self.take_nested(nd, nd_n, nd_nn) };
        self.check(error)?;
        ensure!(types.len() == nodes.len(), "Malformed Gmsh element lists");
        Ok(types.into_iter().zip(nodes).collect())
    }

    /// The number of nodes of an element type.
    pub fn element_nodes(&self, element_type: i32) -> Result<usize> {
        let mut error = 0;
        let mut name = ptr::null_mut();
        let (mut dim, mut order, mut nodes, mut primary) = (0, 0, 0, 0);
        let (mut local, mut nl) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelMeshGetElementProperties)(
                element_type,
                &mut name,
                &mut dim,
                &mut order,
                &mut nodes,
                &mut local,
                &mut nl,
                &mut primary,
                &mut error,
            )
        };
        unsafe {
            self.take_string(name);
            self.take(local, nl);
        }
        self.check(error)?;
        Ok(usize::try_from(nodes)?)
    }

    /// The element types of the mesh of an entity.
    pub fn element_types(&self, dim: i32, tag: i32) -> Result<Vec<i32>> {
        let mut error = 0;
        let (mut types, mut n) = (ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelMeshGetElementTypes)(&mut types, &mut n, dim, tag, &mut error)
        };
        let types = unsafe { self.take(types, n) };
        self.check(error)?;
        Ok(types)
    }

    /// Element and node tags of one element type on an entity.
    pub fn elements_by_type(
        &self,
        element_type: i32,
        tag: i32,
    ) -> Result<(Vec<usize>, Vec<usize>)> {
        let mut error = 0;
        let (mut elements, mut ne, mut nodes, mut nn) = (ptr::null_mut(), 0, ptr::null_mut(), 0);
        unsafe {
            (self.api.gmshModelMeshGetElementsByType)(
                element_type,
                &mut elements,
                &mut ne,
                &mut nodes,
                &mut nn,
                tag,
                0,
                1,
                &mut error,
            )
        };
        let (elements, nodes) = unsafe { (self.take(elements, ne), self.take(nodes, nn)) };
        self.check(error)?;
        Ok((elements, nodes))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let mut ignored = 0;
        // SAFETY: finalization happens once; the library (if any) stays loaded (see the type documentation).
        unsafe { (self.api.gmshFinalize)(&mut ignored) };
    }
}

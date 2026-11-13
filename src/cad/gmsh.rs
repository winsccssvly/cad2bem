// SPDX-License-Identifier: GPL-3.0-or-later
//! CAD preview and untrimmed surface mesh through Gmsh. Called only inside the disposable worker process.

use super::{
    scene::{Face, Mode, Preparation, Request, SCHEMA, Scene},
    session::Session,
};
use crate::conversion::Surface;
use anyhow::{Context, Result, ensure};
use glam::DVec3;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

/// A STEP file with one point and no shape. Reading it makes OpenCASCADE define the unit parameter that
/// `Geometry.OCCTargetUnit` sets; until a STEP file has been read in the process, an IGES file cannot set it.
const UNIT_BOOTSTRAP_STEP: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
    FILE_NAME('','',(''),(''),'','','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\n\
    ENDSEC;\nDATA;\n#1=CARTESIAN_POINT('',(0.,0.,0.));\nENDSEC;\nEND-ISO-10303-21;\n";

/// Whether a path names an IGES file.
pub fn is_iges(path: &Path) -> bool {
    path.extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("iges") || s.eq_ignore_ascii_case("igs"))
}

/// Import a STEP or IGES file in metres, scaled by `scaling`, and synchronize.
///
/// Both unit options are set on every import. An IGES file is preceded by the bootstrap STEP file.
pub fn import_metres(gmsh: &Session, source: &Path, scaling: f64) -> Result<()> {
    gmsh.string("Geometry.OCCTargetUnit", "M")?;
    gmsh.number("Geometry.OCCScaling", 1.)?;
    if is_iges(source) {
        let directory = tempfile::tempdir()?;
        let bootstrap = directory.path().join("units.step");
        std::fs::write(&bootstrap, UNIT_BOOTSTRAP_STEP)?;
        gmsh.import_shapes(&bootstrap)?;
    }
    gmsh.number("Geometry.OCCScaling", scaling)?;
    gmsh.import_shapes(source)?;
    gmsh.synchronize()
}

/// Gmsh SDK library candidates in search order.
#[cfg(not(feature = "static-gmsh"))]
pub fn library_candidates(explicit: Option<&Path>) -> Result<Vec<PathBuf>> {
    if let Some(path) = explicit {
        return Ok(vec![path.to_owned()]);
    }
    if let Some(path) = std::env::var_os("CAD2BEM_GMSH_LIBRARY") {
        return Ok(vec![path.into()]);
    }
    let names: &[&str] = if cfg!(target_os = "windows") {
        &["gmsh-4.15.dll", "gmsh.dll"]
    } else if cfg!(target_os = "macos") {
        &["libgmsh.4.15.dylib", "libgmsh.dylib"]
    } else {
        &["libgmsh.so.4.15", "libgmsh.so"]
    };
    let executable = std::env::current_exe()?;
    let directory = executable.parent().context("Executable has no parent")?;
    let mut candidates = Vec::new();
    for name in names {
        candidates.push(directory.join(name));
    }
    if let Some(prefix) = std::env::var_os("CONDA_PREFIX") {
        candidates.extend(
            names
                .iter()
                .map(|n| PathBuf::from(&prefix).join("Lib").join(n)),
        );
    }
    candidates.extend(names.iter().map(PathBuf::from));
    Ok(candidates)
}

/// The cylinder or plane a face lies on: a cylinder's axis is the direction its normals are all
/// perpendicular to, its radius and axis point a circle fit of its points across the axis.
pub(crate) fn face_surface(gmsh: &Session, tag: i32, kind: &str) -> Result<Option<Surface>> {
    let (lo, hi) = gmsh.parametrization_bounds(2, tag)?;
    let n = 8;
    let uv: Vec<f64> = (0..n)
        .flat_map(|i| {
            let (lo, hi) = (lo.clone(), hi.clone());
            (0..n).flat_map(move |j| {
                let s = (i as f64 + 0.5) / n as f64;
                let t = (j as f64 + 0.5) / n as f64;
                [lo[0] + s * (hi[0] - lo[0]), lo[1] + t * (hi[1] - lo[1])]
            })
        })
        .collect();
    let points: Vec<DVec3> = gmsh
        .value(2, tag, &uv)?
        .into_iter()
        .map(DVec3::from)
        .collect();
    let normals: Vec<DVec3> = gmsh
        .normal(tag, &uv)?
        .into_iter()
        .map(|n| DVec3::from(n).normalize_or_zero())
        .collect();
    Ok(match kind {
        "Plane" => Some(Surface::Plane {
            point: points[points.len() / 2].to_array(),
            normal: normals[normals.len() / 2].to_array(),
        }),
        "Cylinder" => {
            let mut m = nalgebra::Matrix3::zeros();
            for q in &normals {
                let v = nalgebra::Vector3::new(q.x, q.y, q.z);
                m += v * v.transpose();
            }
            let e = nalgebra::SymmetricEigen::new(m);
            let k = e.eigenvalues.imin();
            let axis = DVec3::new(
                e.eigenvectors[(0, k)],
                e.eigenvectors[(1, k)],
                e.eigenvectors[(2, k)],
            )
            .normalize();
            let e1 = axis.any_orthonormal_vector();
            let e2 = axis.cross(e1);
            let mut a = nalgebra::DMatrix::zeros(points.len(), 3);
            let mut b = nalgebra::DVector::zeros(points.len());
            for (r, p) in points.iter().enumerate() {
                let (x, y) = (p.dot(e1), p.dot(e2));
                a[(r, 0)] = x;
                a[(r, 1)] = y;
                a[(r, 2)] = 1.;
                b[r] = -(x * x + y * y);
            }
            let Ok(s) = a.svd(true, true).solve(&b, 1e-12) else {
                return Ok(None);
            };
            let (cx, cy) = (-s[0] / 2., -s[1] / 2.);
            let radius = (cx * cx + cy * cy - s[2]).sqrt();
            let along = points.iter().map(|p| p.dot(axis)).sum::<f64>() / points.len() as f64;
            radius.is_finite().then(|| Surface::Cylinder {
                point: (cx * e1 + cy * e2 + along * axis).to_array(),
                axis: axis.to_array(),
                radius,
            })
        }
        _ => None,
    })
}

/// Run a complete import/mesh transaction in a dedicated worker.
pub fn run(request: &Request) -> Result<Scene> {
    request.validate()?;
    let gmsh = Session::open(request.library.as_deref())?;
    run_in(&gmsh, request, &mut None)
}

/// What a resident view server holds after a prepared view: the prepared model stays in the session, so a
/// cut added next can be shown at once by cutting only the faces it meets ([`Request::quick`]).
pub struct Held {
    source: PathBuf,
    preparation: Preparation,
    scale: f64,
    bounds: [f64; 6],
    sdk_version: String,
    record: serde_json::Value,
}

impl Held {
    /// Whether `request` adds one cut to what is held.
    fn extended_by(&self, request: &Request) -> bool {
        let Some(wanted) = &request.preparation else {
            return false;
        };
        let Some((_, before)) = wanted.cuts.split_last() else {
            return false;
        };
        self.source == request.source
            && self.preparation
                == Preparation {
                    cuts: before.to_vec(),
                    ..wanted.clone()
                }
    }
}

/// Run a request in an open Gmsh session (the resident view server keeps one between requests): every
/// model of the session is cleared first, so the result is the same as in a fresh worker. A quick request
/// that adds one cut to the prepared model in `held` cuts only that model and is marked provisional.
pub fn run_in(gmsh: &Session, request: &Request, held: &mut Option<Held>) -> Result<Scene> {
    request.validate()?;
    if request.quick
        && let Some(h) = held.take_if(|h| h.extended_by(request))
    {
        let cuts = &request.preparation.as_ref().expect("checked").cuts;
        crate::engine::prepare::apply_cuts(gmsh, &cuts[cuts.len() - 1..], h.scale)?;
        let mut record = h.record;
        record["provisional"] = true.into();
        return display(gmsh, request, h.sdk_version, h.bounds, Some(record));
    }
    *held = None;
    // Options set by an earlier request (import bounds, mesh sizes, units) must not change this one.
    gmsh.restore_defaults()?;
    let sdk_version = gmsh.option_string("General.Version")?;
    gmsh.number("General.NumThreads", 1.)?;
    gmsh.reset("cad2bem")?;
    // A prepared view runs the conversion's own preparation (import in the requested unit, cuts, sheet
    // removal), leaving the faces the conversion would mesh; otherwise the file is shown as imported.
    let (prepared, scale) = match &request.preparation {
        Some(preparation) => {
            let options = crate::engine::prepare::Options {
                unit: crate::engine::units::request_unit(&preparation.unit)?,
                symmetry: preparation.symmetry,
                waterline: preparation.cut_waterline,
                sheets: preparation.remove_sheets,
                cuts: preparation.cuts.clone(),
            };
            let prepared =
                crate::engine::prepare::body(gmsh, &request.source, &options, &mut |_, _| {})?;
            ensure!(
                !prepared.faces.is_empty(),
                "No face is left after the preparation"
            );
            (Some(prepared.cad_record()), prepared.scale)
        }
        None => {
            import_metres(gmsh, &request.source, 1.)?;
            (None, 0.)
        }
    };
    if let Some(path) = &request.save_cad {
        // The model is in metres: STEP states metres.
        gmsh.string("Geometry.OCCTargetUnit", "M")?;
        gmsh.write(path)
            .with_context(|| format!("Could not write {}", path.display()))?;
    }
    let bounds = gmsh.bounding_box(-1, -1)?;
    let keep = match (&request.preparation, &prepared) {
        (Some(preparation), Some(record)) if request.mode == Mode::Preview => Some(Held {
            source: request.source.clone(),
            preparation: preparation.clone(),
            scale,
            bounds,
            sdk_version: sdk_version.clone(),
            record: record.clone(),
        }),
        _ => None,
    };
    let scene = display(gmsh, request, sdk_version, bounds, prepared)?;
    *held = keep;
    Ok(scene)
}

/// The display mesh and face facts of the current model (`bounds` sets the display size). The mesh is
/// cleared afterwards, so that the model can be changed again.
fn display(
    gmsh: &Session,
    request: &Request,
    sdk_version: String,
    bounds: [f64; 6],
    prepared: Option<serde_json::Value>,
) -> Result<Scene> {
    ensure!(
        bounds.iter().all(|x| x.is_finite()),
        "CAD has invalid extents"
    );
    let diagonal = ((bounds[3] - bounds[0]).powi(2)
        + (bounds[4] - bounds[1]).powi(2)
        + (bounds[5] - bounds[2]).powi(2))
    .sqrt();
    ensure!(diagonal > 0., "CAD extents collapse to a point");
    let size = if request.mode == Mode::Preview {
        diagonal / 30.
    } else {
        request.size_m.context("Missing surface size")?
    };
    let curvature = if request.mode == Mode::Preview {
        36.
    } else {
        0.
    };
    for (key, value) in [
        ("Mesh.Algorithm", 6.),
        ("Mesh.RecombineAll", 0.),
        ("Mesh.ElementOrder", 1.),
        ("Mesh.MeshSizeMin", size * 0.2),
        ("Mesh.MeshSizeMax", size),
        ("Mesh.MeshSizeFromPoints", 0.),
        ("Mesh.MeshSizeFromCurvature", curvature),
    ] {
        gmsh.number(key, value)?;
    }
    // A face met before in this process (the resident view server) keeps its display mesh and facts: it is
    // hidden from the mesher, which meshes only the new faces. A face's display mesh depends only on the
    // face, its boundary curves and the sizes, so it is the one the mesher would make again.
    let tags = gmsh.tags(2)?;
    // A held model (quick requests) keeps the visibility of the last view: every face is shown again first.
    let all: Vec<_> = tags.iter().map(|&t| (2, t)).collect();
    gmsh.set_visibility(&all, true, false)?;
    gmsh.number("Mesh.MeshOnlyVisible", 0.)?;
    {
        // An overfull cache is emptied before the known faces are chosen, not between choosing and using them.
        let mut cache = MESHES.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > CACHED_FACES {
            cache.clear();
        }
    }
    let mut keys = HashMap::new();
    for &tag in &tags {
        let mut key = face_key(gmsh, tag)?;
        key.extend([request.mode as u64, size.to_bits(), curvature.to_bits()]);
        keys.insert(tag, key);
    }
    let known: Vec<i32> = {
        let cache = MESHES.lock().unwrap_or_else(|e| e.into_inner());
        tags.iter()
            .copied()
            .filter(|t| cache.contains_key(&keys[t]))
            .collect()
    };
    let fresh: Vec<i32> = tags
        .iter()
        .copied()
        .filter(|t| !known.contains(t))
        .collect();
    let mut made = HashMap::new();
    if !fresh.is_empty() {
        if !known.is_empty() {
            let hidden: Vec<_> = known.iter().map(|&t| (2, t)).collect();
            gmsh.set_visibility(&hidden, false, false)?;
            gmsh.number("Mesh.MeshOnlyVisible", 1.)?;
        }
        gmsh.generate(2)?;
        let (node_tags, positions) = gmsh.nodes()?;
        let index: HashMap<_, _> = node_tags.into_iter().zip(positions).collect();
        for &tag in &fresh {
            made.insert(tag, face_mesh(gmsh, tag, &gmsh.kind(2, tag)?, &index)?);
        }
    }
    let mut cache = MESHES.lock().unwrap_or_else(|e| e.into_inner());
    let mut scene = Scene {
        schema: SCHEMA,
        source: request.source.clone(),
        sdk_version,
        mode: request.mode,
        bounds_m: bounds,
        positions_m: Vec::new(),
        triangles: Vec::new(),
        face_tags: Vec::new(),
        faces: Vec::new(),
        prepared,
        open: if request.mode == Mode::Preview {
            Some(open_edges(gmsh, &tags, &bounds)?)
        } else {
            None
        },
    };
    for tag in tags {
        let mesh = match made.remove(&tag) {
            Some(mesh) => {
                cache.insert(keys[&tag].clone(), mesh);
                &cache[&keys[&tag]]
            }
            None => cache
                .get(&keys[&tag])
                .context("A cached face mesh is missing")?,
        };
        let offset = u32::try_from(scene.positions_m.len())
            .ok()
            .filter(|o| o.checked_add(mesh.positions.len() as u32).is_some())
            .context("Mesh is too large for the viewport")?;
        scene.positions_m.extend(&mesh.positions);
        for t in &mesh.triangles {
            scene.triangles.push(t.map(|i| i + offset));
            scene.face_tags.push(tag);
        }
        scene.faces.push(Face {
            tag,
            ..mesh.face.clone()
        });
    }
    gmsh.clear_mesh()?;
    scene.validate()?;
    Ok(scene)
}

/// The edges that belong to one face only, by the coordinate plane they lie on.
fn open_edges(gmsh: &Session, faces: &[i32], bounds: &[f64; 6]) -> Result<super::scene::OpenEdges> {
    let mut count: HashMap<i32, usize> = HashMap::new();
    for &f in faces {
        for (_, c) in gmsh.boundary(&[(2, f)], false, false, false)? {
            *count.entry(c.abs()).or_default() += 1;
        }
    }
    let size = (0..3).map(|k| bounds[k + 3] - bounds[k]).fold(0., f64::max);
    let tol = 1e-6 * size.max(1e-300);
    let mut open = super::scene::OpenEdges::default();
    for (&c, _) in count.iter().filter(|(_, n)| **n == 1) {
        let b = gmsh.bounding_box(1, c)?;
        let on = |k: usize| b[k].abs() <= tol && b[k + 3].abs() <= tol;
        if on(1) {
            open.on_y0 += 1;
        } else if on(0) {
            open.on_x0 += 1;
        } else if on(2) {
            open.on_z0 += 1;
        } else {
            open.elsewhere += 1;
        }
    }
    Ok(open)
}

/// Display meshes kept between requests of a resident view server, by face geometry and sizes.
static MESHES: LazyLock<Mutex<HashMap<Vec<u64>, FaceMesh>>> = LazyLock::new(Default::default);
/// The cache is emptied when it holds more faces than this.
const CACHED_FACES: usize = 20_000;

/// The display mesh of one face (positions local to it) and its facts (the tag is set when used).
struct FaceMesh {
    positions: Vec<[f64; 3]>,
    triangles: Vec<[u32; 3]>,
    face: Face,
}

/// The triangles of a meshed face, its nodes renumbered from 0, and its facts.
fn face_mesh(
    gmsh: &Session,
    tag: i32,
    kind: &str,
    index: &HashMap<usize, [f64; 3]>,
) -> Result<FaceMesh> {
    let types = gmsh.element_types(2, tag)?;
    ensure!(
        !types.is_empty() && types.iter().all(|&k| k == 2),
        "Face {tag}: expected first-order triangles, got {types:?}"
    );
    let (elements, nodes) = gmsh.elements_by_type(2, tag)?;
    ensure!(
        nodes.len() == elements.len() * 3,
        "Face {tag}: malformed triangles"
    );
    let mut local: HashMap<usize, u32> = HashMap::new();
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    for triangle in nodes.as_chunks::<3>().0 {
        let mut row = [0; 3];
        for (out, node) in row.iter_mut().zip(triangle) {
            *out = match local.get(node) {
                Some(&i) => i,
                None => {
                    let p = *index
                        .get(node)
                        .context("Triangle references missing Gmsh node")?;
                    let i = positions.len() as u32;
                    positions.push(p);
                    local.insert(*node, i);
                    i
                }
            };
        }
        triangles.push(row);
    }
    let surface = face_surface(gmsh, tag, kind).unwrap_or(None);
    let corners = gmsh
        .boundary(&[(2, tag)], false, false, true)?
        .into_iter()
        .filter(|&(d, _)| d == 0)
        .map(|(_, v)| gmsh.bounding_box(0, v).map(|b| [b[0], b[1], b[2]]))
        .collect::<Result<Vec<_>>>()?;
    Ok(FaceMesh {
        face: Face {
            tag,
            kind: kind.to_owned(),
            area_m2: gmsh.mass(2, tag)?,
            triangles: triangles.len(),
            surface,
            corners,
        },
        positions,
        triangles,
    })
}

/// A fingerprint of a face's geometry, the same for the same face in another request of the same file:
/// its type, box and parameter range and the boxes of its boundary curves (bit for bit).
pub(crate) fn face_key(gmsh: &Session, tag: i32) -> Result<Vec<u64>> {
    let mut key: Vec<u64> = gmsh.kind(2, tag)?.bytes().map(u64::from).collect();
    key.extend(gmsh.bounding_box(2, tag)?.map(f64::to_bits));
    let (lo, hi) = gmsh.parametrization_bounds(2, tag)?;
    key.extend(lo.iter().chain(&hi).map(|x| x.to_bits()));
    // The surface itself: two faces with the same boxes and ranges can lie on different surfaces.
    let centre = [0.5 * (lo[0] + hi[0]), 0.5 * (lo[1] + hi[1])];
    for v in gmsh
        .value(2, tag, &centre)?
        .into_iter()
        .chain(gmsh.normal(tag, &centre)?)
    {
        key.extend(v.map(f64::to_bits));
    }
    let mut curves = Vec::new();
    for (_, c) in gmsh.boundary(&[(2, tag)], false, false, false)? {
        curves.push(gmsh.bounding_box(1, c.abs())?.map(f64::to_bits));
    }
    curves.sort_unstable();
    key.push(curves.len() as u64);
    key.extend(curves.into_iter().flatten());
    Ok(key)
}

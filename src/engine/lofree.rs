// SPDX-License-Identifier: GPL-3.0-or-later
//! Free low-order panels (lo-free): prepared CAD faces meshed with quadrilaterals or triangles
//! (`PanelMethod`), every panel turned so that
//! its normal points into the fluid, non-convex quadrilaterals split, and the checks of the body.
//!
//! Face sides come from the CAD solids, or from the generalized winding number where the solids do not
//! decide.

use super::{
    panels::{Panel, Panels, convex, normal, reversed, warp},
    prepare::{self, Options, Progress},
    winding,
};
use crate::{
    cad::session::{Session, SizeTable},
    conversion::{FaceSetting, Mode, PanelMethod, Spec, Symmetry},
};
use anyhow::{Context, Result, ensure};
use glam::DVec3;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::Instant,
};

/// Relative spread of the three volumes above which the volume check warns.
const VOLUME_WARN: f64 = 1e-4;

/// Set in a test's environment, the quasi-structured mesher fails as Gmsh's pipeline can on a junction face:
/// the models the tests carry all mesh, and the fallback to the default mesher still needs a failure to run.
pub const FAIL_QUASI_STRUCTURED: &str = "CAD2BEM_TEST_FAIL_QQS";

/// The result of a free panel conversion.
pub struct Conversion {
    pub body: Panels,
    pub interior: Option<Panels>,
    /// The report's `summary`, with the `interior` item when there is one.
    pub summary: Value,
    /// The checks of the body.
    pub checks: Value,
    pub cad: Value,
}

/// The worst of the states.
pub fn state<'a>(states: impl IntoIterator<Item = &'a str>) -> &'static str {
    let mut out = "ok";
    for s in states {
        match s {
            "fail" => return "fail",
            "warn" => out = "warn",
            _ => {}
        }
    }
    out
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Gmsh's surface mesh (quadrilaterals, or triangles) of the open model at one uniform size.
struct Mesh {
    /// Node tags of the panels of every face; a triangle repeats its last node.
    faces: BTreeMap<i32, Vec<[usize; 4]>>,
    node: HashMap<usize, [f64; 3]>,
}

impl Mesh {
    fn panel(&self, tags: &[usize; 4]) -> Result<Panel> {
        let mut out = [[0.; 3]; 4];
        for (p, t) in out.iter_mut().zip(tags) {
            *p = *self
                .node
                .get(t)
                .context("Panel references a missing mesh node")?;
        }
        Ok(out)
    }
}

/// Mesh the faces at `size`, or at their own sizes (`settings`), with `method`. A curve shared by faces of
/// different sizes takes the size of its faces of the highest priority (the smallest of those).
fn mesh_faces(
    gmsh: &Session,
    faces: &[i32],
    size: f64,
    method: PanelMethod,
    settings: &[FaceSetting],
) -> Result<Mesh> {
    gmsh.clear_mesh()?;
    gmsh.clear_sizes()?;
    // Subdivision halves the elements: mesh at twice the size so that panels keep the asked size.
    let k = if method == PanelMethod::Quads { 2. } else { 1. };
    let own: BTreeMap<i32, &FaceSetting> = settings.iter().map(|s| (s.face, s)).collect();
    let face_size = |f: i32| k * own.get(&f).and_then(|s| s.panel_size).unwrap_or(size);
    let base = k * size;
    let (mut lo, mut hi) = (base, base);
    if own.values().any(|s| s.panel_size.is_some()) {
        let priority = |f: i32| own.get(&f).map_or(0, |s| s.priority);
        let mut table: SizeTable = HashMap::new();
        for f in gmsh.tags(2)? {
            table.insert((2, f), face_size(f));
        }
        // Curves and points take the size of their faces (curves) of the highest priority, the smallest
        // of those.
        let mut rank: HashMap<(i32, i32), i32> = HashMap::new();
        for f in gmsh.tags(2)? {
            rank.insert((2, f), priority(f));
        }
        for dim in [1, 0] {
            for t in gmsh.tags(dim)? {
                let up = gmsh.adjacencies(dim, t)?.0;
                let of = |u: i32| (dim + 1, u);
                let top = up
                    .iter()
                    .filter_map(|&u| rank.get(&of(u)))
                    .copied()
                    .max()
                    .unwrap_or(0);
                let size = up
                    .iter()
                    .filter(|&&u| rank.get(&of(u)) == Some(&top))
                    .filter_map(|&u| table.get(&of(u)))
                    .copied()
                    .fold(f64::INFINITY, f64::min);
                rank.insert((dim, t), top);
                table.insert((dim, t), if size.is_finite() { size } else { base });
            }
        }
        lo = table.values().copied().fold(f64::INFINITY, f64::min);
        hi = table.values().copied().fold(0., f64::max);
        gmsh.set_sizes(table)?;
    }
    let (algorithm, recombine, subdivision) = match method {
        PanelMethod::Recombined => (6., 1., 0.),
        PanelMethod::Qqs => (11., 0., 0.),
        PanelMethod::Quads => (6., 1., 1.),
        PanelMethod::Triangles => (6., 0., 0.),
    };
    for (key, value) in [
        ("Mesh.MeshSizeMin", lo),
        ("Mesh.MeshSizeMax", hi),
        ("Mesh.MeshSizeFromCurvature", 0.),
        ("Mesh.MeshSizeFromPoints", 0.),
        ("Mesh.Algorithm", algorithm),
        ("Mesh.RecombineAll", recombine),
        ("Mesh.RecombinationAlgorithm", 1.),
        ("Mesh.SubdivisionAlgorithm", subdivision),
    ] {
        gmsh.number(key, value)?;
    }
    ensure!(
        method != PanelMethod::Qqs || std::env::var_os(FAIL_QUASI_STRUCTURED).is_none(),
        "The quasi-structured mesher failed ({FAIL_QUASI_STRUCTURED} is set)"
    );
    gmsh.generate(2)?;
    gmsh.clear_sizes()?;
    let (tags, xyz) = gmsh.nodes()?;
    let node = tags.into_iter().zip(xyz).collect();
    let mut out = BTreeMap::new();
    for &f in faces {
        let mut rows = Vec::new();
        for (kind, nodes) in gmsh.elements(2, f)? {
            let n = gmsh.element_nodes(kind)?;
            ensure!(
                (n == 3 || n == 4) && nodes.len().is_multiple_of(n),
                "Face {f}: unexpected surface element type {kind}"
            );
            for e in nodes.chunks(n) {
                rows.push(if n == 4 {
                    [e[0], e[1], e[2], e[3]]
                } else {
                    [e[0], e[1], e[2], e[2]]
                });
            }
        }
        out.insert(f, rows);
    }
    Ok(Mesh { faces: out, node })
}

/// The unit CAD normals of a face at its points closest to the given points.
fn cad_normals(gmsh: &Session, face: i32, points: &[[f64; 3]]) -> Result<Vec<DVec3>> {
    let (_, uv) = gmsh.closest_point(2, face, points)?;
    Ok(gmsh
        .normal(face, &uv)?
        .into_iter()
        .map(|n| DVec3::from_array(n).normalize())
        .collect())
}

fn is_triangle(p: &Panel) -> bool {
    p[2] == p[3]
}

/// The two triangles of a quadrilateral, cut by the diagonal that keeps its normal.
pub fn split(quad: &Panel) -> [Panel; 2] {
    let n = normal(quad);
    for [a, b, c, d] in [[0, 1, 2, 3], [1, 2, 3, 0]] {
        let t1 = [quad[a], quad[b], quad[c], quad[c]];
        let t2 = [quad[a], quad[c], quad[d], quad[d]];
        if normal(&t1).dot(n) > 0. && normal(&t2).dot(n) > 0. {
            return [t1, t2];
        }
    }
    [
        [quad[0], quad[1], quad[2], quad[2]],
        [quad[0], quad[2], quad[3], quad[3]],
    ]
}

/// Append the panels, splitting every quadrilateral that is not strictly convex; the count split.
pub fn push_convex(panels: Vec<Panel>, owner: i32, out: &mut Panels) -> usize {
    let mut splits = 0;
    for p in panels {
        if is_triangle(&p) || convex(&p) {
            out.vertices.push(p);
            out.faces.push(owner);
        } else {
            for t in split(&p) {
                out.vertices.push(t);
                out.faces.push(owner);
            }
            splits += 1;
        }
    }
    splits
}

/// The panel edges used once (by node tag) that do not lie in z = 0 or a symmetry plane.
fn free_edges(mesh: &Mesh, faces: &[i32], size: f64, symmetry: Symmetry) -> usize {
    let mut count: HashMap<(usize, usize), usize> = HashMap::new();
    for row in faces.iter().flat_map(|f| &mesh.faces[f]) {
        let mut ring: Vec<usize> = Vec::new();
        for (k, &t) in row.iter().enumerate() {
            if k == 0 || t != row[k - 1] {
                ring.push(t);
            }
        }
        if ring.len() > 1 && ring.last() == ring.first() {
            ring.pop();
        }
        for (i, &a) in ring.iter().enumerate() {
            let b = ring[(i + 1) % ring.len()];
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let tol = 1e-6 * size;
    let mut planes = vec![2];
    planes.extend(symmetry.axes());
    let on_plane = |a: usize, b: usize| {
        planes
            .iter()
            .any(|&k| mesh.node[&a][k].abs() < tol && mesh.node[&b][k].abs() < tol)
    };
    count
        .into_iter()
        .filter(|&((a, b), n)| n == 1 && !on_plane(a, b))
        .count()
}

/// The `panels` report record: panel, quadrilateral and triangle counts, split panels, faces, panel size,
/// the diameter range and the largest warp.
pub fn summary(panels: &Panels, splits: usize, faces: usize, panel_size: f64) -> Result<Value> {
    let v = &panels.vertices;
    let triangles = v.iter().filter(|p| is_triangle(p)).count();
    let diameter = |p: &Panel| {
        let q = p.map(DVec3::from_array);
        q[2].distance(q[0]).max(q[3].distance(q[1]))
    };
    let warp_max = v
        .iter()
        .map(warp)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .fold(0., f64::max);
    Ok(json!({
        "panels": v.len(), "quadrilaterals": v.len() - triangles, "triangles": triangles,
        "split_nonconvex": splits, "faces": faces, "panel_size_m": panel_size,
        "diameter_min_m": v.iter().map(diameter).fold(f64::INFINITY, f64::min),
        "diameter_max_m": v.iter().map(diameter).fold(0., f64::max),
        "warp_max_m": warp_max,
    }))
}

/// The body checks of the report (volume, closed surface, waterline, orientation), each with a state: ok, warn
/// or fail.
pub fn body_checks(
    volumes: [f64; 3],
    volume_cad: Option<f64>,
    free_edges: usize,
    z_max: f64,
    orientation: Value,
    size: f64,
    cut: bool,
) -> Value {
    let mean = volumes.iter().map(|v| v.abs()).sum::<f64>() / 3.;
    let min = volumes.iter().copied().fold(f64::INFINITY, f64::min);
    let max = volumes.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let spread = (max - min) / mean.max(1e-300);
    let volume_state = if min <= 0. {
        "fail"
    } else if spread > VOLUME_WARN {
        "warn"
    } else {
        "ok"
    };
    let difference = volume_cad
        .filter(|&v| v != 0.)
        .map(|v| volumes.iter().sum::<f64>() / 3. / v - 1.);
    let orientation_state = if orientation["sheets_left"]
        .as_array()
        .is_some_and(|s| !s.is_empty())
        || orientation["flip_margin_min"].as_f64().unwrap_or(1.) <= 0.
    {
        "warn"
    } else {
        "ok"
    };
    let mut orientation_entry = json!({"state": orientation_state});
    if let (Some(entry), Some(record)) =
        (orientation_entry.as_object_mut(), orientation.as_object())
    {
        entry.extend(record.clone());
    }
    json!({
        "volume": {"state": volume_state, "volumes_m3": volumes, "volume_cad_m3": volume_cad,
                   "cad_difference_rel": difference, "spread_rel": spread},
        "closed": {"state": if free_edges > 0 { "warn" } else { "ok" }, "free_edges": free_edges},
        "waterline": {"state": if cut && z_max > 1e-6 * size { "warn" } else { "ok" },
                      "z_max_m": z_max, "cut_waterline": cut},
        "orientation": orientation_entry,
    })
}

/// Convert a CAD file to free panels with the native engine.
pub fn convert(spec: &Spec, progress: Progress) -> Result<Conversion> {
    ensure!(
        spec.mode == Mode::LoFree,
        "The native engine converts lo-free only"
    );
    let started = Instant::now();
    let path = spec.source.as_path();
    let settings = &spec.settings;
    let panel_size = settings.panel_size.context("lo-free needs a panel size")?;
    let options = Options {
        unit: super::units::request_unit(&settings.unit)?,
        symmetry: settings.symmetry,
        waterline: settings.cut_waterline,
        sheets: settings.remove_sheets,
        cuts: settings.cuts.clone(),
    };
    progress("faces", json!({"file": path.display().to_string()}));
    let gmsh = Session::open(None)?;
    let prepared = prepare::body(&gmsh, path, &options, progress)?;
    let sides = prepare::face_sides(&gmsh, path, &prepared)?;
    let faces: Vec<i32> = sides.inward.keys().copied().collect();
    ensure!(
        !faces.is_empty(),
        "{}: no face left{}",
        path.display(),
        if settings.cut_waterline {
            " below the waterline"
        } else {
            ""
        }
    );
    progress("orient", json!({"patches": 0, "faces": faces.len()}));
    let mut estimate = 0.;
    for &f in &faces {
        estimate += gmsh.mass(2, f)? / (panel_size * panel_size);
    }
    progress(
        "mesh",
        json!({"faces": faces.len(), "panel_size": panel_size, "panels_estimated": estimate.round() as i64}),
    );
    // The interior surface on the body's own waterline curves, meshed with the body: the two share their
    // waterline nodes whatever the mesher (meshed apart, quasi-structured quads divided the same curve
    // differently, as each mesh takes its sizes from the faces around it).
    let waterplane = settings
        .interior
        .then(|| prepare::waterplane_on_body(&gmsh, path, settings.symmetry, &prepared))
        .transpose()?;
    let meshed: Vec<i32> = faces
        .iter()
        .chain(waterplane.iter().flat_map(|(w, _)| w))
        .copied()
        .collect();
    let mesh = mesh_faces(
        &gmsh,
        &meshed,
        panel_size,
        settings.panel_method,
        &settings.faces,
    )?;
    // Per face: its panels and, per panel, the panel normal against the CAD normal at the panel centre.
    let mut faces_panels = BTreeMap::new();
    for &f in &faces {
        let rows = &mesh.faces[&f];
        if rows.is_empty() {
            continue;
        }
        let panels = rows
            .iter()
            .map(|r| mesh.panel(r))
            .collect::<Result<Vec<_>>>()?;
        let centres: Vec<[f64; 3]> = panels
            .iter()
            .map(|p| (p.map(DVec3::from_array).into_iter().sum::<DVec3>() / 4.).to_array())
            .collect();
        let cad = cad_normals(&gmsh, f, &centres)?;
        let dots: Vec<f64> = panels
            .iter()
            .zip(cad)
            .map(|(p, n)| normal(p).dot(n))
            .collect();
        faces_panels.insert(f, (panels, dots));
    }
    ensure!(
        !faces_panels.is_empty(),
        "{}: no panels: no face was meshed",
        path.display()
    );
    // The side of every face: from the CAD solids when they decide every face, otherwise (loose surfaces,
    // or a face where solids touch) all faces at once by the winding number.
    let (inward, orientation) = if sides.inward.values().all(Option::is_some) {
        let inward: BTreeMap<i32, i32> = sides
            .inward
            .iter()
            .map(|(&f, s)| (f, s.unwrap_or(1)))
            .collect();
        let record = json!({"method": "cad_solid", "tested_points": sides.tested, "sheets": [], "sheets_left": []});
        (inward, record)
    } else {
        // Provisional panels along the CAD normal; the winding says whether that normal points out of the body.
        let provisional: Vec<Vec<Panel>> = faces_panels
            .values()
            .map(|(panels, dots)| {
                panels
                    .iter()
                    .zip(dots)
                    .map(|(p, &d)| if d < 0. { reversed(p) } else { *p })
                    .collect()
            })
            .collect();
        let input: Vec<winding::Face> = provisional
            .iter()
            .map(|panels| winding::Face { panels })
            .collect();
        let result = winding::orient(&input, settings.symmetry.axes())?;
        let inward = faces_panels
            .keys()
            .zip(&result.out_of_body)
            .map(|(&f, &out)| (f, if out { -1 } else { 1 }))
            .collect();
        let undecided: Vec<i32> = sides
            .inward
            .iter()
            .filter(|(_, side)| side.is_none())
            .map(|(&f, _)| f)
            .collect();
        let mut record = json!({"method": "winding", "tested_points": sides.tested, "sheets": [],
            "faces_undecided": undecided});
        if let (Some(entry), Some(extra)) = (record.as_object_mut(), result.record.as_object()) {
            entry.extend(extra.clone());
        }
        // Report faces that look like sheets by their face numbers.
        let faces: Vec<i32> = faces_panels.keys().copied().collect();
        let left: Vec<i32> = result.record["sheets_left_faces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| k.as_u64().map(|k| faces[k as usize]))
            .collect();
        record["sheets_left"] = json!(left);
        (inward, record)
    };
    let mut body = Panels {
        vertices: Vec::new(),
        faces: Vec::new(),
        symmetry: settings.symmetry,
        label: stem(path),
    };
    let mut splits = 0;
    let meshed: Vec<i32> = faces_panels.keys().copied().collect();
    for (f, (panels, dots)) in faces_panels {
        let sign = f64::from(inward[&f]);
        // Turn every panel whose normal points into the body: its normal then points into the fluid.
        let panels = panels
            .into_iter()
            .zip(dots)
            .map(|(p, d)| if d * sign > 0. { reversed(&p) } else { p })
            .collect();
        splits += push_convex(panels, f, &mut body);
    }
    let holes = free_edges(&mesh, &meshed, prepared.scale, settings.symmetry);
    let mut cad = prepared.cad_record();
    if !settings.faces.is_empty() {
        let unknown: Vec<i32> = settings
            .faces
            .iter()
            .map(|s| s.face)
            .filter(|f| !meshed.contains(f))
            .collect();
        cad["face_settings"] = json!({"faces": settings.faces.len(), "faces_unknown": unknown});
    }
    let size = prepared.scale;
    let volume_cad = prepared.volume_cad.map(|v| v * settings.symmetry.images());

    progress("checks", json!({"panels": body.vertices.len()}));
    let volumes = body.volumes()?;
    let z_max = body
        .vertices
        .iter()
        .flatten()
        .map(|p| p[2])
        .fold(f64::NEG_INFINITY, f64::max);
    let checks = body_checks(
        volumes,
        volume_cad,
        holes,
        z_max,
        orientation,
        size,
        settings.cut_waterline,
    );
    let mut summary = summary(&body, splits, faces.len(), panel_size)?;
    let body_state = state(
        checks
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, c)| c["state"].as_str()),
    );
    summary["state"] = body_state.into();
    summary["body_size_m"] = size.into();
    summary["seconds"] = ((started.elapsed().as_secs_f64() * 100.).round() / 100.).into();
    let interior = if let Some((waterplane, area_cad)) = &waterplane {
        let (panels, entry) = interior(&mesh, waterplane, *area_cad, spec, size, progress)?;
        summary["state"] = state([body_state, entry["state"].as_str().unwrap_or("fail")]).into();
        summary["interior"] = entry;
        Some(panels)
    } else {
        None
    };
    Ok(Conversion {
        body,
        interior,
        summary,
        checks,
        cad,
    })
}

/// The interior free surface as free panels, every normal up, and its summary entry: the panels of the waterplane
/// faces (`faces`, of area `area_cad`) meshed with the body.
fn interior(
    mesh: &Mesh,
    faces: &[i32],
    area_cad: f64,
    spec: &Spec,
    size: f64,
    progress: Progress,
) -> Result<(Panels, Value)> {
    let path = spec.source.as_path();
    progress("interior", json!({"file": path.display().to_string()}));
    let mut panels = Panels {
        vertices: Vec::new(),
        faces: Vec::new(),
        symmetry: spec.settings.symmetry,
        label: format!("{} interior", stem(path)),
    };
    for (k, f) in faces.iter().enumerate() {
        let rows = &mesh.faces[f];
        let turned = rows
            .iter()
            .map(|r| {
                let p = mesh.panel(r)?;
                Ok(if normal(&p).z < 0. { reversed(&p) } else { p })
            })
            .collect::<Result<Vec<_>>>()?;
        push_convex(turned, k as i32 + 1, &mut panels);
    }
    ensure!(
        !panels.vertices.is_empty(),
        "{}: no interior panels: the section was not meshed",
        path.display()
    );
    let images = spec.settings.symmetry.images();
    let entry = interior_entry(&panels, images * area_cad, size, images);
    Ok((panels, entry))
}

/// The `interior` item of a panel summary: `area_cad` the CAD section's area with the symmetry images
/// (compared, not judged: flat panels cut its curved edges); every panel must lie in z = 0.
pub fn interior_entry(panels: &Panels, area_cad: f64, size: f64, images: f64) -> Value {
    let area = images
        * panels
            .vertices
            .iter()
            .map(|p| {
                let q = p.map(DVec3::from_array);
                0.5 * ((q[1] - q[0]).cross(q[2] - q[0]).length()
                    + (q[2] - q[0]).cross(q[3] - q[0]).length())
            })
            .sum::<f64>();
    let z_max = panels
        .vertices
        .iter()
        .flatten()
        .map(|p| p[2].abs())
        .fold(0., f64::max);
    let panels_state = if panels.vertices.is_empty() {
        "fail"
    } else {
        "ok"
    };
    let plane = if z_max > 1e-6 * size { "fail" } else { "ok" };
    json!({
        "state": state([panels_state, plane]),
        "panels": panels.vertices.len(), "area_m2": area, "area_cad_m2": area_cad,
        "area_difference_rel": if area_cad != 0. { Some(area / area_cad - 1.) } else { None },
        "z_max_m": z_max, "checks": {"panels": panels_state, "plane": plane},
    })
}

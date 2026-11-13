// SPDX-License-Identifier: GPL-3.0-or-later
//! High-order spline patches (ho): prepared CAD faces as four-sided B-spline patches, oriented into the body,
//! named, checked and reported, with the optional interior surface.
//!
//! The side of the patches comes from the CAD solids when they decide every face, otherwise from the generalized winding
//! number of the tessellated patches.

use super::{
    bspline::{Patch, unit_grid},
    checks,
    panels::{Panel, reversed},
    patches::{Converter, QuadPatch, Settings},
    prepare::{self, Options, Progress},
    winding,
};
use crate::{
    cad::session::Session,
    conversion::{
        DEFAULT_POTENTIAL_PANEL_SIZE_M, HBG_VERSION, Mode, Spec, Symmetry, potential_spans,
    },
};
use anyhow::{Context, Result, bail, ensure};
use glam::DVec3;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Instant};

/// Corner angles (degrees) outside this range warn.
pub const CORNER_WARN: (f64, f64) = (30., 150.);
/// min |x_u x x_v| / mean below this warns.
pub const JACOBIAN_WARN: f64 = 0.2;
/// CAD error relative to the body size above this warns.
const CAD_WARN: f64 = 1e-4;
/// Gaps relative to the body size above this warn. A gap never fails: the solver takes surfaces that meet with gaps
/// or not at all, and the gap is reported (`gap_m`, `gap_max_m`) for the user to judge.
const GAP_WARN: f64 = 1e-5;
/// Relative spread of the three volumes (and difference to the CAD volume) above this warns.
pub const VOLUME_WARN: f64 = 1e-4;

/// Named spline patches and their HBG document.
pub struct Splines {
    pub patches: Vec<Patch>,
    pub names: Vec<String>,
    pub symmetry: Symmetry,
    pub label: String,
    /// The potential panel size (m) the file carries (`panel_size`; the default when none was chosen).
    pub panel_size: f64,
}

impl Splines {
    /// The geometry file (HBG version 3, kind `spline_patches`).
    pub fn to_hbg(&self) -> Value {
        let patches: Vec<Value> = self
            .patches
            .iter()
            .zip(&self.names)
            .map(|(p, name)| {
                let mu = p.mu();
                let rows: Vec<Vec<[f64; 3]>> = (0..p.mv())
                    .map(|j| (0..mu).map(|i| p.coefficient(j, i).to_array()).collect())
                    .collect();
                let mut entry = json!({"name": name, "u_order": p.u_order, "v_order": p.v_order,
                    "u_knots": p.u_knots, "v_knots": p.v_knots, "coefficients": rows});
                if p.rational() {
                    let w = p
                        .weights
                        .as_ref()
                        .map(|w| w.chunks(mu).map(|r| r.to_vec()).collect::<Vec<_>>());
                    entry["weights"] = json!(w);
                }
                entry
            })
            .collect();
        // `panel_size` is the solver's rule (spans from the edge arc lengths, orders 3, Gauss 3 and 4) in place of a
        // `discretization`: always written, so the file is complete without a discretization from its reader.
        json!({"format": "hbg", "version": HBG_VERSION, "kind": "spline_patches", "label": self.label,
            "symmetry": self.symmetry.name(),
            "info": {"generator": {"name": "cad2bem", "version": env!("CARGO_PKG_VERSION"), "backend": "rust"}},
            "patches": patches, "panel_size": self.panel_size})
    }
}

/// The result of a spline patch conversion.
pub struct Conversion {
    pub body: Splines,
    pub interior: Option<Splines>,
    pub summary: Value,
    pub checks: Value,
    pub cad: Value,
    /// The prepared face of every body patch.
    pub faces: Vec<i32>,
    /// The checks of every body patch.
    pub patch_checks: Vec<Value>,
    /// The checks of every interior patch (empty without the interior surface).
    pub interior_patch_checks: Vec<Value>,
}

fn state<'a>(states: impl IntoIterator<Item = &'a str>) -> &'static str {
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

/// `face_<f>` for the only patch of face f, `face_<f>.<k>` for its k-th of several.
pub fn patch_names(faces: &[i32]) -> Vec<String> {
    let mut count: BTreeMap<i32, usize> = BTreeMap::new();
    for &f in faces {
        *count.entry(f).or_default() += 1;
    }
    let mut seen: BTreeMap<i32, usize> = BTreeMap::new();
    faces
        .iter()
        .map(|&f| {
            let k = seen.entry(f).or_default();
            *k += 1;
            if count[&f] == 1 {
                format!("face_{f}")
            } else {
                format!("face_{f}.{k}")
            }
        })
        .collect()
}

/// `waterplane_<k>` for the only patch of the k-th section face, `waterplane_<k>.<j>` for its j-th.
fn interior_names(faces: &[i32]) -> Vec<String> {
    let mut order: Vec<i32> = Vec::new();
    for &f in faces {
        if !order.contains(&f) {
            order.push(f);
        }
    }
    let count = |f: i32| faces.iter().filter(|&&x| x == f).count();
    let mut seen: BTreeMap<i32, usize> = BTreeMap::new();
    faces
        .iter()
        .map(|&f| {
            let k = order.iter().position(|&x| x == f).unwrap_or(0) + 1;
            let j = seen.entry(f).or_default();
            *j += 1;
            if count(f) == 1 {
                format!("waterplane_{k}")
            } else {
                format!("waterplane_{k}.{j}")
            }
        })
        .collect()
}

/// The longer arc length of the two u edges and of the two v edges of a patch (m).
pub fn edge_lengths(patch: &Patch) -> [f64; 2] {
    let n = 65;
    let u = unit_grid(n);
    let arc = |points: Vec<DVec3>| points.windows(2).map(|w| w[0].distance(w[1])).sum::<f64>();
    let lu = arc(u.iter().map(|&x| patch.at(x, 0.)).collect())
        .max(arc(u.iter().map(|&x| patch.at(x, 1.)).collect()));
    let lv = arc(u.iter().map(|&x| patch.at(0., x)).collect())
        .max(arc(u.iter().map(|&x| patch.at(1., x)).collect()));
    [lu, lv]
}

/// The discretization row (nu, nv, ku, kv, outer, inner) by the solver's panel-size rule (orders 3, Gauss
/// orders 3 and 4).
fn discretization(lengths: [f64; 2], span: f64) -> [usize; 6] {
    let [nu, nv] = potential_spans(lengths, span);
    [nu, nv, 3, 3, 3, 4]
}

/// The checks of one patch, as the report's patch entry.
#[allow(clippy::too_many_arguments)]
fn patch_entry(
    record: usize,
    name: &str,
    q: &QuadPatch,
    patch: &Patch,
    swapped: bool,
    gap: f64,
    overlapping: bool,
    size: f64,
    span: f64,
) -> Value {
    let (jacobian, folds) = checks::jacobian(patch, 21);
    let angles = checks::corner_angles(patch);
    let lengths = edge_lengths(patch);
    let rows = discretization(lengths, span);
    let c = json!({
        "fold": if folds > 0 { "fail" } else { "ok" },
        "jacobian": if jacobian < JACOBIAN_WARN { "warn" } else { "ok" },
        "corner_angle": if angles.iter().any(|&a| a < CORNER_WARN.0 || a > CORNER_WARN.1) { "warn" } else { "ok" },
        "cad_error": if q.cad_error > CAD_WARN * size { "warn" } else { "ok" },
        "gap": if gap > GAP_WARN * size { "warn" } else { "ok" },
        "overlap": if overlapping { "warn" } else { "ok" },
    });
    let s = state(
        c.as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, v)| v.as_str()),
    );
    json!({"name": name, "record": record, "face": q.face, "face_type": q.face_type, "path": q.path,
        "exact": q.exact.unwrap_or("fitted"), "rational": patch.rational(), "swapped": swapped,
        "cad_error_m": q.cad_error, "gap_m": gap, "jacobian_min_rel": jacobian, "fold_points": folds,
        "corner_angles": angles, "edge_lengths_m": lengths, "discretization": rows, "checks": c, "state": s})
}

/// The tessellated panels of a patch along x_u x x_v, for the winding number.
fn tessellate(patch: &Patch, n: usize) -> Vec<Panel> {
    let g = unit_grid(n + 1);
    let x = patch.grid(&g, &g);
    let mut out = Vec::with_capacity(n * n);
    for j in 0..n {
        for i in 0..n {
            let p = |a: usize, b: usize| x[(j + b) * (n + 1) + i + a].to_array();
            out.push([p(0, 0), p(1, 0), p(1, 1), p(0, 1)]);
        }
    }
    out
}

/// Orient the patches into the body: per patch whether it was swapped, and the orientation record.
fn orient(
    gmsh: &Session,
    path: &Path,
    prepared: &prepare::Prepared,
    qp: &[QuadPatch],
    symmetry: Symmetry,
) -> Result<(Vec<bool>, Value)> {
    let sides = prepare::face_sides(gmsh, path, prepared)?;
    // Each patch's normal against its CAD face's normal: the decided faces swap by their side, the others are
    // oriented as whole CAD faces by the winding number (as free panels are: one unknown per face, not per
    // patch, which kept a model of 1110 patches without solids busy for minutes).
    let mut along = Vec::new();
    for q in qp {
        let (x, n) = checks::center_normal(&q.patch);
        let (_, uv) = gmsh.closest_point(2, q.face, &[x.to_array()])?;
        let cad = DVec3::from_array(gmsh.normal(q.face, &uv)?[0]);
        let dot = n.dot(cad);
        ensure!(
            dot.is_finite() && dot.abs() >= 1e-8,
            "Face {}: patch normal is tangent to the CAD face",
            q.face
        );
        along.push(dot > 0.);
    }
    let decided = qp
        .iter()
        .all(|q| sides.inward.get(&q.face).copied().flatten().is_some());
    if decided {
        let swapped = qp
            .iter()
            .zip(&along)
            .map(|(q, &along)| {
                // inward +1: the CAD normal points into the body, where x_u x x_v must point.
                let inward = sides.inward[&q.face].unwrap_or(1);
                (inward > 0) != along
            })
            .collect();
        let record = json!({"method": "cad_solid", "tested_points": sides.tested, "sheets": [], "sheets_left": []});
        return Ok((swapped, record));
    }
    // The winding number of the tessellated patches of each CAD face, along its CAD normal; finer for large
    // patches.
    let diameters: Vec<f64> = qp.iter().map(|q| checks::diameter(&q.patch)).collect();
    let mut sorted = diameters.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2].max(1e-300);
    let mut by_face: BTreeMap<i32, Vec<Panel>> = BTreeMap::new();
    for ((q, d), &along) in qp.iter().zip(&diameters).zip(&along) {
        // 4 x 4 panels for a median patch, more for larger ones: a CAD face is tessellated by all its patches.
        let n = (4f64.max(4. * d / median) / 4.).ceil() as usize * 4;
        let panels = by_face.entry(q.face).or_default();
        for p in tessellate(&q.patch, n.min(16)) {
            panels.push(if along { p } else { reversed(&p) });
        }
    }
    let faces: Vec<i32> = by_face.keys().copied().collect();
    let input: Vec<winding::Face> = by_face
        .values()
        .map(|panels| winding::Face { panels })
        .collect();
    let result = winding::orient(&input, symmetry.axes())?;
    let mut record = json!({"method": "winding", "sheets": []});
    if let (Some(entry), Some(extra)) = (record.as_object_mut(), result.record.as_object()) {
        entry.extend(extra.clone());
    }
    let left: Vec<i32> = result.record["sheets_left_faces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| k.as_u64().and_then(|k| faces.get(k as usize).copied()))
        .collect();
    record["sheets_left"] = json!(left);
    record["sheets_left_faces"] = json!(left);
    // x_u x x_v must point into the body: a patch along a CAD normal that points out of it is swapped, and a
    // patch against such a normal is not.
    let out: BTreeMap<i32, bool> = faces.iter().copied().zip(result.out_of_body).collect();
    Ok((
        qp.iter()
            .zip(&along)
            .map(|(q, &along)| along == out[&q.face])
            .collect(),
        record,
    ))
}

/// Convert a CAD file to spline patches with the native engine.
/// The preparation of the body a spline conversion asks for.
pub fn options(spec: &Spec) -> Result<Options> {
    let settings = &spec.settings;
    Ok(Options {
        unit: super::units::request_unit(&settings.unit)?,
        symmetry: settings.symmetry,
        waterline: settings.cut_waterline,
        sheets: settings.remove_sheets,
        cuts: settings.cuts.clone(),
    })
}

/// The patch settings of the body faces, for a body of size `scale`.
pub fn converter_settings(spec: &Spec, scale: f64) -> Result<Settings> {
    let settings = &spec.settings;
    let patch_size = settings.patch_size.context("ho needs a patch size")?;
    Ok(Settings {
        method: settings.patch_method,
        faces: settings.faces.iter().map(|s| (s.face, s.clone())).collect(),
        ..Settings::new(patch_size, scale, true)
    })
}

/// Convert a CAD file to high-order spline patches (the ho mode).
pub fn convert(spec: &Spec, progress: Progress) -> Result<Conversion> {
    ensure!(spec.mode == Mode::Ho, "Spline patches are the ho mode");
    let started = Instant::now();
    let path = spec.source.as_path();
    let settings = &spec.settings;
    let patch_size = settings.patch_size.context("ho needs a patch size")?;
    let span = settings
        .potential_panel_size
        .unwrap_or(DEFAULT_POTENTIAL_PANEL_SIZE_M);
    let options = options(spec)?;
    progress("faces", json!({"file": path.display().to_string()}));
    let gmsh = Session::open(None)?;
    let prepared = prepare::body(&gmsh, path, &options, progress)?;
    let size = prepared.scale;
    let mut converter = Converter::new(&gmsh, converter_settings(spec, size)?);
    converter.surfaces = super::surfaces::read(path);
    let workers = super::fanout::workers(prepared.faces.len());
    let qp = if workers > 1 {
        super::fanout::patches(&mut converter, spec, &prepared.faces, workers)?
    } else {
        converter.patches(&prepared.faces)?
    };
    if qp.is_empty() {
        bail!(
            "{}: no face left{}",
            path.display(),
            if settings.cut_waterline {
                " below the waterline"
            } else {
                ""
            }
        );
    }
    let mut cad = prepared.cad_record();
    cad["face_paths"] = json!(
        converter
            .paths
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect::<BTreeMap<_, _>>()
    );
    if !settings.faces.is_empty() {
        let unknown: Vec<i32> = settings
            .faces
            .iter()
            .map(|s| s.face)
            .filter(|f| !prepared.faces.contains(f))
            .collect();
        cad["face_settings"] = json!({"faces": settings.faces.len(),
            "forced_path_failed": converter.forced_failed, "faces_unknown": unknown});
    }
    if converter.method_fallback {
        cad["patch_method_used"] = json!("tri3");
    }
    cad["remeshed"] = json!(
        converter
            .remeshed
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect::<BTreeMap<_, _>>()
    );
    progress("orient", json!({"patches": qp.len()}));
    let (swapped, orientation) = orient(&gmsh, path, &prepared, &qp, settings.symmetry)?;
    let patches: Vec<Patch> = qp
        .iter()
        .zip(&swapped)
        .map(|(q, &s)| {
            if s {
                q.patch.swapped()
            } else {
                q.patch.clone()
            }
        })
        .collect();
    let faces: Vec<i32> = qp.iter().map(|q| q.face).collect();
    let names = patch_names(&faces);

    progress("checks", json!({"patches": patches.len()}));
    let symmetry = settings.symmetry;
    let gap = checks::gaps(&patches, &|x| checks::on_planes(x, size, symmetry), None);
    let groups: Vec<i64> = qp.iter().map(|q| q.disjoint_group).collect();
    let over = checks::overlaps(&patches, 1e-4 * size, &groups);
    let overlapping: Vec<usize> = over.iter().flat_map(|&(i, j, _)| [i - 1, j - 1]).collect();
    let entries: Vec<Value> = (0..patches.len())
        .map(|k| {
            patch_entry(
                k + 1,
                &names[k],
                &qp[k],
                &patches[k],
                swapped[k],
                gap[k],
                overlapping.contains(&k),
                size,
                span,
            )
        })
        .collect();
    let rows: Vec<[usize; 6]> = patches
        .iter()
        .map(|p| discretization(edge_lengths(p), span))
        .collect();
    let z_max = patches
        .iter()
        .flat_map(|p| p.coefficients.iter().map(|c| c.z))
        .fold(f64::NEG_INFINITY, f64::max);
    let volume_cad = prepared.volume_cad.map(|v| v * symmetry.images());
    let volumes = checks::volumes(&patches, symmetry);
    let mean = volumes.iter().sum::<f64>() / 3.;
    let spread = (volumes.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        - volumes.iter().copied().fold(f64::INFINITY, f64::min))
        / (volumes.iter().map(|v| v.abs()).sum::<f64>() / 3.).max(1e-300);
    let volume_state = if volumes.iter().copied().fold(f64::INFINITY, f64::min) <= 0. {
        "fail"
    } else if spread > VOLUME_WARN
        || volume_cad.is_some_and(|c| c != 0. && (mean / c - 1.).abs() > VOLUME_WARN)
    {
        "warn"
    } else {
        "ok"
    };
    let gap_max = gap.iter().copied().fold(0., f64::max);
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
    if let (Some(entry), Some(extra)) = (orientation_entry.as_object_mut(), orientation.as_object())
    {
        entry.extend(extra.clone());
    }
    let overlaps_json: Vec<Value> = over
        .iter()
        .map(|&(i, j, c)| json!({"records": [i, j], "covered": c}))
        .collect();
    let body = json!({
        "volume": {"state": volume_state, "volumes_m3": volumes, "volume_cad_m3": volume_cad, "spread_rel": spread},
        "closed": {"state": if gap_max > GAP_WARN * size { "warn" } else { "ok" },
                   "gap_max_m": gap_max, "overlaps": overlaps_json},
        "waterline": {"state": if settings.cut_waterline && z_max > 1e-6 * size { "warn" } else { "ok" },
                      "z_max_m": z_max, "cut_waterline": settings.cut_waterline},
        "orientation": orientation_entry,
    });
    let mut patch_states: BTreeMap<String, usize> = BTreeMap::new();
    let mut paths: BTreeMap<&str, usize> = BTreeMap::new();
    let mut exact: BTreeMap<&str, usize> = BTreeMap::new();
    for (e, q) in entries.iter().zip(&qp) {
        *patch_states
            .entry(e["state"].as_str().unwrap_or("fail").to_owned())
            .or_default() += 1;
        *paths.entry(q.path).or_default() += 1;
        *exact.entry(q.exact.unwrap_or("fitted")).or_default() += 1;
    }
    let all_states = body
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, c)| c["state"].as_str())
        .chain(entries.iter().filter_map(|e| e["state"].as_str()));
    let mut summary = json!({
        "state": state(all_states), "patches": patches.len(), "patch_states": patch_states, "body_size_m": size,
        "seconds": (started.elapsed().as_secs_f64() * 100.).round() / 100.,
        "potential_unknowns": rows.iter().map(|r| (r[0] + r[2] - 1) * (r[1] + r[3] - 1)).sum::<usize>(),
        "paths": paths, "exact": exact,
    });
    let label = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut interior_patch_checks = Vec::new();
    let interior = if settings.interior {
        let (splines, entry, interior_entries) =
            interior(&gmsh, spec, &options, patch_size, size, &patches, progress)?;
        let hull = summary["state"].as_str().unwrap_or("fail").to_owned();
        summary["state"] = state([hull.as_str(), entry["state"].as_str().unwrap_or("fail")]).into();
        summary["interior"] = entry;
        interior_patch_checks = interior_entries;
        Some(splines)
    } else {
        None
    };
    Ok(Conversion {
        body: Splines {
            patches,
            names,
            symmetry,
            label,
            panel_size: spec
                .settings
                .potential_panel_size
                .unwrap_or(DEFAULT_POTENTIAL_PANEL_SIZE_M),
        },
        interior,
        summary,
        checks: body,
        cad,
        faces,
        patch_checks: entries,
        interior_patch_checks,
    })
}

/// The interior free surface as spline patches (x_u x x_v down), its summary entry and its patch checks.
fn interior(
    gmsh: &Session,
    spec: &Spec,
    options: &Options,
    patch_size: f64,
    size: f64,
    hull: &[Patch],
    progress: Progress,
) -> Result<(Splines, Value, Vec<Value>)> {
    let path = spec.source.as_path();
    let span = spec
        .settings
        .potential_panel_size
        .unwrap_or(DEFAULT_POTENTIAL_PANEL_SIZE_M);
    progress("interior", json!({"file": path.display().to_string()}));
    gmsh.reset("")?;
    let (faces, area_cad) = prepare::waterplane(gmsh, path, options)?;
    let mut converter = Converter::new(
        gmsh,
        Settings {
            method: spec.settings.patch_method,
            ..Settings::new(patch_size, size, true)
        },
    );
    let qp = converter.patches(&faces)?;
    ensure!(!qp.is_empty(), "{}: no interior patches", path.display());
    let mut swapped = Vec::new();
    let patches: Vec<Patch> = qp
        .iter()
        .map(|q| {
            let up = checks::center_normal(&q.patch).1.z > 0.;
            swapped.push(up);
            if up {
                q.patch.swapped()
            } else {
                q.patch.clone()
            }
        })
        .collect();
    let names = interior_names(&qp.iter().map(|q| q.face).collect::<Vec<_>>());
    let symmetry = spec.settings.symmetry;
    let tol = 1e-6 * size;
    let mirror = |x: DVec3| symmetry.axes().iter().any(|&k| x[k].abs() < tol);
    let both: Vec<Patch> = hull.iter().chain(&patches).cloned().collect();
    let h = hull.len();
    let interior_ids: Vec<usize> = (h..both.len()).collect();
    let gap = checks::gaps(&both, &mirror, Some(&interior_ids))[h..].to_vec();
    let hull_ids: Vec<usize> = (0..h).collect();
    let uncovered = checks::gaps(&both, &|x| x.z.abs() >= tol || mirror(x), Some(&hull_ids))[..h]
        .iter()
        .copied()
        .fold(0., f64::max);
    let entries: Vec<Value> = (0..patches.len())
        .map(|k| {
            patch_entry(
                k + 1,
                &names[k],
                &qp[k],
                &patches[k],
                swapped[k],
                gap[k],
                false,
                size,
                span,
            )
        })
        .collect();
    let images = symmetry.images();
    let area = images * patches.iter().map(checks::area).sum::<f64>();
    let area_cad = images * area_cad;
    let z_max = patches
        .iter()
        .flat_map(|p| p.coefficients.iter().map(|c| c.z.abs()))
        .fold(0., f64::max);
    let difference = area / area_cad - 1.;
    let gap_max = gap.iter().copied().fold(uncovered, f64::max);
    let c = json!({
        "patches": state(entries.iter().filter_map(|e| e["state"].as_str())),
        "area": if difference.abs() > VOLUME_WARN { "warn" } else { "ok" },
        "waterline": if gap_max > GAP_WARN * size { "warn" } else { "ok" },
        "plane": if z_max > 1e-6 * size { "fail" } else { "ok" },
    });
    let mut patch_states: BTreeMap<String, usize> = BTreeMap::new();
    for e in &entries {
        *patch_states
            .entry(e["state"].as_str().unwrap_or("fail").to_owned())
            .or_default() += 1;
    }
    let entry = json!({
        "state": state(c.as_object().into_iter().flatten().filter_map(|(_, v)| v.as_str())),
        "patches": patches.len(), "patch_states": patch_states, "area_m2": area, "area_cad_m2": area_cad,
        "area_difference_rel": difference, "waterline_gap_max_m": gap_max, "z_max_m": z_max, "checks": c,
    });
    let label = format!(
        "{} interior",
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    Ok((
        Splines {
            patches,
            names,
            symmetry,
            label,
            panel_size: spec
                .settings
                .potential_panel_size
                .unwrap_or(DEFAULT_POTENTIAL_PANEL_SIZE_M),
        },
        entry,
        entries,
    ))
}

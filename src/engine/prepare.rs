// SPDX-License-Identifier: GPL-3.0-or-later
//! CAD preparation before panel meshing: import, waterline and symmetry cuts, faces on the cut planes,
//! overlapping faces, fragment, and the side of every face from the CAD solids.
//!
//! Faces without CAD solids, or whose side the solids cannot decide, are left undecided by [`face_sides`];
//! the generalized winding number ([`super::winding`]) orients them.

use super::units::{FileUnit, file_unit};
use crate::{
    cad::{
        gmsh::import_metres,
        session::{DimTag, Session},
    },
    conversion::{Cut, Sheets, Surface, Symmetry},
};
use anyhow::{Result, bail};
use glam::DVec3;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{LazyLock, Mutex},
};

/// Relative distance within which a sample lies on another face (`overlap_tol`).
const OVERLAP_TOL: f64 = 1e-4;
/// Relative mirror distance and relative area difference a symmetric body may have.
const MIRROR_TOL: f64 = 1e-4;
const AREA_TOL: f64 = 1e-3;

/// Progress callback: stage name and data.
pub type Progress<'a> = &'a mut dyn FnMut(&str, Value);

/// What the preparation needs from a conversion request.
#[derive(Clone, Debug)]
pub struct Options {
    /// Factor to metres of the file's numbers, replacing the unit the file states (None: the file's own).
    pub unit: Option<f64>,
    pub symmetry: Symmetry,
    /// Cut at the waterline z = 0 and keep the part below.
    pub waterline: bool,
    /// Faces removed as sheets after the fragment.
    pub sheets: Sheets,
    /// Planar cuts that partition the faces after the waterline and symmetry cuts.
    pub cuts: Vec<Cut>,
}

/// The prepared model state and the facts of the report's `cad` record.
pub struct Prepared {
    pub file_unit: FileUnit,
    pub unit_m: Option<f64>,
    /// Scaling applied at import (the requested unit over the file's unit).
    pub scaling: f64,
    pub bounding_box: [f64; 6],
    /// The body size: the longest side of the bounding box before any cut. Tolerances are relative to it.
    pub scale: f64,
    /// Symmetry axes the whole body reaches across (cut there).
    pub halves: Vec<usize>,
    /// Volume of the CAD solids below the water (all of it without the waterline cut), one side of each
    /// symmetry plane the body was cut at.
    pub volume_cad: Option<f64>,
    pub has_solids: bool,
    pub faces: Vec<i32>,
    imported: usize,
    cut_at_waterline: usize,
    not_cut_at_waterline: usize,
    dropped: usize,
    overlaps: Vec<Value>,
    symmetry_cut: Option<Value>,
    cuts: Value,
    cad_sheets: Vec<Value>,
}

impl Prepared {
    /// The report's `cad` record of a free panel conversion.
    pub fn cad_record(&self) -> Value {
        json!({
            "file_unit": self.file_unit.name, "file_unit_m": self.file_unit.metres, "unit_m": self.unit_m,
            "faces_imported": self.imported, "faces_cut_at_waterline": self.cut_at_waterline,
            "faces_not_cut_at_waterline": self.not_cut_at_waterline, "faces_dropped": self.dropped,
            "face_overlaps": self.overlaps, "cad_sheets": self.cad_sheets, "recipe_faces_unknown": [],
            "symmetry_cut": self.symmetry_cut, "cuts": self.cuts, "remeshed": {}, "face_paths": {}, "faces_unsided": [],
        })
    }
}

fn plane_name(axis: usize) -> &'static str {
    if axis == 0 { "yz" } else { "xz" }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// Read a file into the current model in metres, with triangulation bounding boxes.
fn import(gmsh: &Session, path: &Path, scaling: f64) -> Result<()> {
    // Boxes from the triangulation: OpenCASCADE's own are widened by the shape tolerance, which made a face
    // on z = 0 look as if it crossed the waterline, and a trimmed piece keeps the box of its untrimmed surface.
    gmsh.number("Geometry.OCCBoundsUseStl", 1.)?;
    import_metres(gmsh, path, scaling)
}

/// A file read into the model: its unit, the import scaling, the bounding box and the body size.
struct Opened {
    unit: FileUnit,
    unit_m: Option<f64>,
    scaling: f64,
    bounding_box: [f64; 6],
    scale: f64,
}

/// Unit, import, size and the `read` progress event shared by the body and the waterplane.
fn open(
    gmsh: &Session,
    path: &Path,
    options: &Options,
    progress: Option<Progress>,
) -> Result<Opened> {
    let unit = file_unit(path)?;
    let (unit_m, scaling) = match (options.unit, unit.metres) {
        (None, metres) => (metres, 1.),
        (Some(_), None) => {
            let why = if unit.name.as_deref() == Some("mixed") {
                "states different length units"
            } else {
                "states no length unit that cad2bem reads"
            };
            bail!(
                "{}: the file {why}, so a unit cannot replace it; convert with unit = 'file'",
                display(path)
            )
        }
        (Some(requested), Some(metres)) => (Some(requested), requested / metres),
    };
    import(gmsh, path, scaling)?;
    let b = gmsh.bounding_box(-1, -1)?;
    let scale = (b[3] - b[0]).max(b[4] - b[1]).max(b[5] - b[2]);
    if let Some(progress) = progress {
        progress(
            "read",
            json!({"file_unit": unit.name, "file_unit_m": unit.metres, "unit_m": unit_m,
                "body_size_m": scale, "bounding_box_m": b}),
        );
    }
    Ok(Opened {
        unit,
        unit_m,
        scaling,
        bounding_box: b,
        scale,
    })
}

fn halves(symmetry: Symmetry, b: &[f64; 6], scale: f64) -> Vec<usize> {
    symmetry
        .axes()
        .iter()
        .copied()
        .filter(|&k| b[k] < -1e-6 * scale)
        .collect()
}

/// A box around the model below z = 0 (`whole`: at every height), on the positive side of `halves`.
fn below_water_box(gmsh: &Session, halves: &[usize], whole: bool) -> Result<i32> {
    let [x0, y0, z0, x1, y1, z1] = gmsh.bounding_box(-1, -1)?;
    let lo: [f64; 2] = std::array::from_fn(|k| {
        if halves.contains(&k) {
            0.
        } else {
            [x0, y0][k] - 1.
        }
    });
    let top = if whole { z1.max(0.) + 1. } else { 0. };
    gmsh.add_box(
        [lo[0], lo[1], z0 - 1.],
        [x1 + 1. - lo[0], y1 + 1. - lo[1], top - (z0 - 1.)],
    )
}

/// The volume of solids below z = 0, measured on copies; the solids are left as they are.
fn volume_below_water(
    gmsh: &Session,
    solids: &[DimTag],
    halves: &[usize],
    whole: bool,
) -> Result<f64> {
    let copies = gmsh.copy(solids)?;
    let tool = below_water_box(gmsh, halves, whole)?;
    let out = gmsh.intersect(&copies, &[(3, tool)])?.out;
    gmsh.synchronize()?;
    let mut volume = 0.;
    for &(_, t) in &out {
        volume += gmsh.mass(3, t)?;
    }
    gmsh.remove(&out, true)?;
    gmsh.synchronize()?;
    Ok(volume)
}

/// Samples of a face: an n x n grid of its parameter box kept inside the trimmed face, with points and
/// unit normals.
#[derive(Clone)]
struct Samples {
    x: Vec<[f64; 3]>,
    normals: Vec<[f64; 3]>,
}

type SampleKey = (Vec<u64>, usize);
/// Samples of faces met before in this process (the resident view server), by face geometry and grid size.
static SAMPLES: LazyLock<Mutex<HashMap<SampleKey, Samples>>> = LazyLock::new(Default::default);
/// The sample cache is emptied when it holds more faces than this.
const CACHED_SAMPLES: usize = 50_000;

/// [`sample_face`], remembered by face geometry (sampling a trimmed face is slow).
fn samples(gmsh: &Session, face: i32, n: usize) -> Result<Samples> {
    let key = (crate::cad::gmsh::face_key(gmsh, face)?, n);
    if let Some(s) = SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(s.clone());
    }
    let s = sample_face(gmsh, face, n)?;
    let mut cache = SAMPLES.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > CACHED_SAMPLES {
        cache.clear();
    }
    cache.insert(key, s.clone());
    Ok(s)
}

fn sample_face(gmsh: &Session, face: i32, n: usize) -> Result<Samples> {
    let (lo, hi) = gmsh.parametrization_bounds(2, face)?;
    let g = linspace(0.05, 0.95, n);
    let mut uv = Vec::new();
    for &b in &g {
        for &a in &g {
            let p = [lo[0] + a * (hi[0] - lo[0]), lo[1] + b * (hi[1] - lo[1])];
            if gmsh.inside(2, face, &p, true)? > 0 {
                uv.push(p);
            }
        }
    }
    if uv.is_empty() {
        return Ok(Samples {
            x: Vec::new(),
            normals: Vec::new(),
        });
    }
    let flat = uv.as_flattened();
    Ok(Samples {
        x: gmsh.value(2, face, flat)?,
        normals: gmsh.normal(face, flat)?,
    })
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    DVec3::from_array(a).distance(DVec3::from_array(b))
}

/// The fraction of the samples of face `a` that lie on face `b`: near it, inside its trimmed domain and
/// with parallel normals.
fn covered(gmsh: &Session, a: &Samples, b: i32, box_b: &[f64; 6], tol: f64) -> Result<f64> {
    if a.x.is_empty() {
        return Ok(0.);
    }
    // Only samples near b's box can lie on b. The box comes from b's display triangulation, which can sit
    // inside the surface by the chord height: it is widened by 2 % of its size.
    let size = (0..3).map(|k| box_b[k + 3] - box_b[k]).fold(0., f64::max);
    let near = tol + 0.02 * size;
    let candidates: Vec<usize> = (0..a.x.len())
        .filter(|&k| {
            (0..3).all(|j| a.x[k][j] >= box_b[j] - near && a.x[k][j] <= box_b[j + 3] + near)
        })
        .collect();
    if candidates.is_empty() {
        return Ok(0.);
    }
    let points: Vec<[f64; 3]> = candidates.iter().map(|&k| a.x[k]).collect();
    let (closest, uv) = gmsh.closest_point(2, b, &points)?;
    let mut hits = 0;
    for (i, c) in closest.iter().enumerate() {
        let k = candidates[i];
        if distance(*c, a.x[k]) >= tol {
            continue;
        }
        let p = [uv[2 * i], uv[2 * i + 1]];
        if gmsh.inside(2, b, &p, true)? > 0 {
            let nb = gmsh.normal(b, &p)?[0];
            hits += usize::from(
                DVec3::from_array(a.normals[k])
                    .dot(DVec3::from_array(nb))
                    .abs()
                    > 0.97,
            );
        }
    }
    Ok(hits as f64 / a.x.len() as f64)
}

fn boxes_overlap(a: &[f64; 6], b: &[f64; 6], tol: f64) -> bool {
    (0..3).all(|k| a[k] <= b[k + 3] + tol && b[k] <= a[k + 3] + tol)
}

fn round3(x: f64) -> f64 {
    (x * 1000.).round() / 1000.
}

/// Overlap fractions of face pairs met before in this process, by the faces' geometry and the tolerance: a
/// resident view server does not sample unchanged faces again.
static PAIRS: LazyLock<Mutex<HashMap<PairKey, (f64, f64)>>> = LazyLock::new(Default::default);
type PairKey = (Vec<u64>, Vec<u64>, u64);
/// The pair cache is emptied when it holds more pairs than this.
const CACHED_PAIRS: usize = 200_000;

/// Pairs of faces that cover the same area twice; the more covered face is cut by the other, or removed
/// when it is covered entirely.
fn overlaps(gmsh: &Session, scale: f64) -> Result<Vec<Value>> {
    let faces = gmsh.tags(2)?;
    let mut boxes = BTreeMap::new();
    for &f in &faces {
        boxes.insert(f, gmsh.bounding_box(2, f)?);
    }
    let tol = OVERLAP_TOL * scale;
    let mut cache: BTreeMap<i32, Samples> = BTreeMap::new();
    let mut keys = BTreeMap::new();
    let mut found = Vec::new();
    for (i, &a) in faces.iter().enumerate() {
        for &b in &faces[i + 1..] {
            if !boxes_overlap(&boxes[&a], &boxes[&b], tol) {
                continue;
            }
            // Faces on two different planes or cylinders share no area: they are not sampled (sampling a
            // trimmed face is the slow part).
            // A pair met before in this process (the resident view server) is not sampled again.
            for f in [a, b] {
                if let std::collections::btree_map::Entry::Vacant(entry) = keys.entry(f) {
                    entry.insert(crate::cad::gmsh::face_key(gmsh, f)?);
                }
            }
            let pair = (keys[&a].clone(), keys[&b].clone(), tol.to_bits());
            let known = PAIRS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&pair)
                .copied();
            let (ca, cb) = match known {
                Some(fractions) => fractions,
                None => {
                    for f in [a, b] {
                        if let std::collections::btree_map::Entry::Vacant(entry) = cache.entry(f) {
                            entry.insert(samples(gmsh, f, 9)?);
                        }
                    }
                    let ca = covered(gmsh, &cache[&a], b, &boxes[&b], tol)?;
                    let cb = covered(gmsh, &cache[&b], a, &boxes[&a], tol)?;
                    let mut pairs = PAIRS.lock().unwrap_or_else(|e| e.into_inner());
                    if pairs.len() > CACHED_PAIRS {
                        pairs.clear();
                    }
                    pairs.insert(pair, (ca, cb));
                    (ca, cb)
                }
            };
            if ca.max(cb) > 0. {
                found.push((a, b, ca, cb));
            }
        }
    }
    let mut out = Vec::new();
    for (a, b, ca, cb) in found {
        let ((cut, by), cover) = if round3(ca) >= round3(cb) {
            ((a, b), round3(ca))
        } else {
            ((b, a), round3(cb))
        };
        let alive = gmsh.tags(2)?;
        let action = if !alive.contains(&cut) || !alive.contains(&by) {
            "skipped (face already changed)".to_owned()
        } else {
            let result = if cover > 0.99 {
                gmsh.remove(&[(2, cut)], true)
                    .map(|()| format!("face {cut} removed (duplicate of {by})"))
            } else {
                gmsh.cut(&[(2, cut)], &[(2, by)], true, false).map(|r| {
                    let tags: Vec<String> = r.out.iter().map(|(_, t)| t.to_string()).collect();
                    format!("face {cut} cut by {by} -> [{}]", tags.join(", "))
                })
            };
            match result.and_then(|action| gmsh.synchronize().map(|()| action)) {
                Ok(action) => action,
                Err(error) => format!("cut failed: {error:#}"),
            }
        };
        out.push(json!({"faces": [a, b], "covered": [round3(ca), round3(cb)], "action": action}));
    }
    Ok(out)
}

/// Mirror distances met before in this process, by the faces' geometry on both sides and the axis.
static MIRRORS: LazyLock<Mutex<HashMap<MirrorKey, f64>>> = LazyLock::new(Default::default);
type MirrorKey = (Vec<Vec<u64>>, Vec<Vec<u64>>, usize);

/// The largest distance of the samples of faces `a`, mirrored in the plane normal to axis k, to the nearest
/// of faces `b` (infinite when only one side has faces); remembered by the faces' geometry.
fn mirror_distance(gmsh: &Session, a: &[i32], b: &[i32], k: usize) -> Result<f64> {
    let keys = |faces: &[i32]| -> Result<Vec<Vec<u64>>> {
        let mut keys = faces
            .iter()
            .map(|&f| crate::cad::gmsh::face_key(gmsh, f))
            .collect::<Result<Vec<_>>>()?;
        keys.sort();
        Ok(keys)
    };
    let key = (keys(a)?, keys(b)?, k);
    if let Some(&d) = MIRRORS.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(d);
    }
    let d = measure_mirror_distance(gmsh, a, b, k)?;
    let mut cache = MIRRORS.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 1000 {
        cache.clear();
    }
    cache.insert(key, d);
    Ok(d)
}

fn measure_mirror_distance(gmsh: &Session, a: &[i32], b: &[i32], k: usize) -> Result<f64> {
    let mut points = Vec::new();
    for &f in a {
        points.extend(samples(gmsh, f, 7)?.x);
    }
    if points.is_empty() || b.is_empty() {
        return Ok(if points.is_empty() && b.is_empty() {
            0.
        } else {
            f64::INFINITY
        });
    }
    for p in &mut points {
        p[k] = -p[k];
    }
    let mut d = vec![f64::INFINITY; points.len()];
    for &f in b {
        let (closest, _) = gmsh.closest_point(2, f, &points)?;
        for (i, c) in closest.iter().enumerate() {
            d[i] = d[i].min(distance(*c, points[i]));
        }
    }
    Ok(d.into_iter().fold(0., f64::max))
}

/// Cut a whole body at the symmetry planes it reaches across and keep x >= 0, y >= 0, after checking per
/// plane that it is symmetric. Returns the report's `symmetry_cut` record.
fn cut_symmetry(
    gmsh: &Session,
    path: &Path,
    symmetry: Symmetry,
    halves: &[usize],
    scale: f64,
) -> Result<Value> {
    let tol = 1e-6 * scale;
    let (mut planes, mut faces_cut, mut faces_dropped) = (Vec::new(), 0, 0);
    let (mut mirror_error, mut area_difference) = (0_f64, 0_f64);
    for &k in halves {
        let [x0, y0, z0, x1, y1, z1] = gmsh.bounding_box(-1, -1)?;
        for f in gmsh.tags(2)? {
            let x = samples(gmsh, f, 7)?.x;
            let across = !x.is_empty()
                && x.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min) < -tol
                && x.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) > tol;
            if !across {
                continue;
            }
            let cutter = if k == 0 {
                let c = gmsh.add_rectangle([z0 - 1., y0 - 1., 0.], z1 - z0 + 2., y1 - y0 + 2.)?;
                gmsh.rotate(
                    &[(2, c)],
                    [0.; 3],
                    [0., 1., 0.],
                    -std::f64::consts::FRAC_PI_2,
                )?;
                c
            } else {
                let c = gmsh.add_rectangle([x0 - 1., z0 - 1., 0.], x1 - x0 + 2., z1 - z0 + 2.)?;
                gmsh.rotate(
                    &[(2, c)],
                    [0.; 3],
                    [1., 0., 0.],
                    std::f64::consts::FRAC_PI_2,
                )?;
                c
            };
            let pieces = gmsh.fragment(&[(2, f)], &[(2, cutter)])?.map;
            gmsh.remove(&pieces[1], true)?;
            gmsh.synchronize()?;
            faces_cut += usize::from(pieces[0].len() > 1);
        }
        let (mut negative, mut positive) = (Vec::new(), Vec::new());
        for f in gmsh.tags(2)? {
            let b = gmsh.bounding_box(2, f)?;
            if b[k].abs() < tol && b[k + 3].abs() < tol {
                continue;
            }
            if gmsh.center_of_mass(2, f)?[k] > 0. {
                positive.push(f);
            } else {
                negative.push(f);
            }
        }
        let mut area = [0.; 2];
        for (sum, faces) in area.iter_mut().zip([&negative, &positive]) {
            for &f in faces {
                *sum += gmsh.mass(2, f)?;
            }
        }
        let error = mirror_distance(gmsh, &negative, &positive, k)?
            .max(mirror_distance(gmsh, &positive, &negative, k)?);
        let difference = (area[0] - area[1]).abs() / area[1].max(1e-300);
        if error > MIRROR_TOL * scale || difference > AREA_TOL {
            let rest: Vec<_> = symmetry
                .axes()
                .iter()
                .filter(|&&a| a != k)
                .map(|&a| plane_name(a))
                .collect();
            let rest = if rest.is_empty() {
                "none".into()
            } else {
                rest.join("+")
            };
            bail!(
                "{}: the body is not symmetric about {} = 0 (mirror distance {} m, area difference {}); \
                 convert with symmetry '{rest}'",
                display(path),
                ["x", "y"][k],
                general(error, 3),
                general(difference, 3)
            );
        }
        let removed: Vec<DimTag> = negative.iter().map(|&f| (2, f)).collect();
        gmsh.remove(&removed, true)?;
        gmsh.synchronize()?;
        planes.push(plane_name(k));
        faces_dropped += negative.len();
        mirror_error = mirror_error.max(error);
        area_difference = area_difference.max(difference);
    }
    Ok(
        json!({"planes": planes, "faces_cut": faces_cut, "faces_dropped": faces_dropped,
        "mirror_error_m": mirror_error, "area_difference_rel": area_difference}),
    )
}

/// Remove face f as a CAD sheet and record why.
fn remove_sheet(
    gmsh: &Session,
    f: i32,
    parent: &HashMap<i32, i32>,
    reason: String,
    sheets: &mut Vec<Value>,
) -> Result<()> {
    let area = (gmsh.mass(2, f)? * 1000.).round() / 1000.;
    sheets.push(json!({"face": f, "parent": parent.get(&f).copied().unwrap_or(f), "area": area, "reason": reason}));
    gmsh.remove(&[(2, f)], true)?;
    gmsh.synchronize()
}

/// Remove the pieces that are not part of the closed wetted surface, by two rules on the fragmented faces:
/// (1) a piece with a free boundary curve (a curve of no other piece, not on the waterline or a symmetry
/// plane, not a seam or a pole) is a sheet, repeatedly; (2) where three or more pieces meet along a curve, a
/// face the fragment split there keeps its largest piece (the others ran past the intersection).
fn remove_sheets(
    gmsh: &Session,
    parent: &HashMap<i32, i32>,
    symmetry: Symmetry,
    scale: f64,
    sheets: &mut Vec<Value>,
) -> Result<()> {
    let tol = 1e-6 * scale;
    let mut planes = vec![2];
    planes.extend(symmetry.axes());
    let on_plane = |c: i32| -> Result<bool> {
        let b = gmsh.bounding_box(1, c)?;
        Ok(planes
            .iter()
            .any(|&k| b[k].abs() < tol && b[k + 3].abs() < tol))
    };
    let point = |c: i32| -> Result<bool> {
        let b = gmsh.bounding_box(1, c)?;
        Ok((0..3).all(|k| b[k + 3] - b[k] < tol))
    };
    let same_place = |a: i32, b: i32| -> Result<bool> {
        let (lo, hi) = gmsh.parametrization_bounds(1, a)?;
        let x = gmsh.value(1, a, &linspace(lo[0], hi[0], 9))?;
        let (cx, _) = gmsh.closest_point(1, b, &x)?;
        let worst = x
            .iter()
            .zip(cx)
            .flat_map(|(p, q)| (0..3).map(move |d| (p[d] - q[d]).abs()))
            .fold(0., f64::max);
        Ok(worst < 1e-5 * scale)
    };
    // The curves that bound a face on both sides (a seam): one curve twice in its loops, or two curves of it
    // in the same place (a face that was not sewn).
    let seams = |f: i32| -> Result<Vec<i32>> {
        let mut count: Vec<(i32, usize)> = Vec::new();
        for c in gmsh.curve_loops(f)?.into_iter().flatten().map(|c| c.abs()) {
            match count.iter_mut().find(|(k, _)| *k == c) {
                Some(entry) => entry.1 += 1,
                None => count.push((c, 1)),
            }
        }
        let mut out: Vec<i32> = count
            .iter()
            .filter(|(_, n)| *n > 1)
            .map(|(c, _)| *c)
            .collect();
        let mut lines = Vec::new();
        for &(c, _) in &count {
            if !out.contains(&c) && !point(c)? {
                lines.push(c);
            }
        }
        for (k, &a) in lines.iter().enumerate() {
            for &b in &lines[k + 1..] {
                if same_place(a, b)? && same_place(b, a)? {
                    out.extend([a, b]);
                }
            }
        }
        Ok(out)
    };
    loop {
        let mut changed = false;
        for f in gmsh.tags(2)? {
            let curves: Vec<i32> = gmsh
                .boundary(&[(2, f)], true, false, false)?
                .into_iter()
                .map(|(_, c)| c.abs())
                .collect();
            let seam = seams(f)?;
            let mut free = false;
            for c in curves {
                if gmsh.adjacencies(1, c)?.0.len() == 1
                    && !on_plane(c)?
                    && !seam.contains(&c)
                    && !point(c)?
                {
                    free = true;
                    break;
                }
            }
            if free {
                remove_sheet(gmsh, f, parent, "free edge".into(), sheets)?;
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    for c in gmsh.tags(1)? {
        if !gmsh.tags(1)?.contains(&c) {
            continue; // removed with a face already
        }
        let faces = gmsh.adjacencies(1, c)?.0;
        if faces.len() < 3 {
            continue;
        }
        let mut groups: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        for f in faces {
            groups
                .entry(parent.get(&f).copied().unwrap_or(f))
                .or_default()
                .push(f);
        }
        for (origin, kids) in groups {
            if kids.len() < 2 {
                continue;
            }
            let mut keep = kids[0];
            let mut largest = f64::NEG_INFINITY;
            for &k in &kids {
                let area = gmsh.mass(2, k)?;
                if area > largest {
                    (keep, largest) = (k, area);
                }
            }
            for f in kids {
                if f != keep {
                    remove_sheet(
                        gmsh,
                        f,
                        parent,
                        format!("overshoot of face {origin} at curve {c}"),
                        sheets,
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Remove every connected group of faces (faces sharing a curve) but the one of the largest area.
fn keep_largest(gmsh: &Session, parent: &HashMap<i32, i32>, sheets: &mut Vec<Value>) -> Result<()> {
    let faces = gmsh.tags(2)?;
    let mut group: HashMap<i32, i32> = faces.iter().map(|&f| (f, f)).collect();
    fn find(group: &mut HashMap<i32, i32>, mut f: i32) -> i32 {
        while group[&f] != f {
            let up = group[&group[&f]];
            group.insert(f, up);
            f = up;
        }
        f
    }
    for c in gmsh.tags(1)? {
        let fs: Vec<i32> = gmsh
            .adjacencies(1, c)?
            .0
            .into_iter()
            .filter(|f| group.contains_key(f))
            .collect();
        for &f in fs.iter().skip(1) {
            let (a, b) = (find(&mut group, f), find(&mut group, fs[0]));
            group.insert(a, b);
        }
    }
    let mut area: BTreeMap<i32, f64> = BTreeMap::new();
    for &f in &faces {
        let root = find(&mut group, f);
        *area.entry(root).or_default() += gmsh.mass(2, f)?;
    }
    if area.len() < 2 {
        return Ok(());
    }
    let best = area
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(k, _)| *k)
        .unwrap_or(0);
    for f in faces {
        if find(&mut group, f) != best {
            remove_sheet(gmsh, f, parent, "not the largest body".into(), sheets)?;
        }
    }
    Ok(())
}

/// Import a body and prepare its wetted faces for free panels.
pub fn body(
    gmsh: &Session,
    path: &Path,
    options: &Options,
    progress: Progress,
) -> Result<Prepared> {
    let Opened {
        unit,
        unit_m,
        scaling,
        bounding_box,
        scale,
    } = open(gmsh, path, options, Some(progress))?;
    let halves = halves(options.symmetry, &bounding_box, scale);
    let solids = gmsh.entities(3)?;
    let has_solids = !solids.is_empty();
    let volume_cad = if has_solids {
        Some(volume_below_water(
            gmsh,
            &solids,
            &halves,
            !options.waterline,
        )?)
    } else {
        None
    };
    if has_solids {
        gmsh.remove(&gmsh.entities(3)?, false)?; // keep the faces only
        gmsh.synchronize()?;
    } else if gmsh.tags(2)?.len() > 1 {
        // Loose faces that only touch along their edges share no curve, and the fragment does not join them:
        // they are sewn first. A single face is not: sewing would drop its poles and close a closed face.
        gmsh.sew_faces(1e-5 * scale)?;
        gmsh.synchronize()?;
    }
    let imported = gmsh.tags(2)?.len();
    if imported == 0 {
        bail!("{}: no surfaces in the file", display(path));
    }
    let tol = 1e-6 * scale;
    let (mut cut_at_waterline, mut not_cut_at_waterline) = (0, 0);
    if options.waterline {
        for f in gmsh.tags(2)? {
            let [x0, y0, z0, x1, y1, z1] = gmsh.bounding_box(2, f)?;
            if !(z0 < -tol && z1 > tol) {
                continue;
            }
            let plane = gmsh.add_rectangle([x0 - 1., y0 - 1., 0.], x1 - x0 + 2., y1 - y0 + 2.)?;
            let pieces = gmsh.fragment(&[(2, f)], &[(2, plane)])?.map;
            gmsh.synchronize()?;
            // A piece keeps the bounding box of its untrimmed surface: its centre decides.
            let mut drop = pieces[1].clone();
            for &piece in &pieces[0] {
                if gmsh.center_of_mass(piece.0, piece.1)?[2] > 0. {
                    drop.push(piece);
                }
            }
            gmsh.remove(&drop, true)?;
            gmsh.synchronize()?;
            if pieces[0].len() > 1 {
                cut_at_waterline += 1;
            } else {
                not_cut_at_waterline += 1;
            }
        }
    }
    let symmetry_cut = if halves.is_empty() {
        None
    } else {
        Some(cut_symmetry(gmsh, path, options.symmetry, &halves, scale)?)
    };
    let mut planes: Vec<usize> = if options.waterline { vec![2] } else { vec![] };
    planes.extend(options.symmetry.axes());
    let mut drop = Vec::new();
    for f in gmsh.tags(2)? {
        let b = gmsh.bounding_box(2, f)?;
        if (options.waterline && b[2] > -tol)
            || planes
                .iter()
                .any(|&k| b[k].abs() < tol && b[k + 3].abs() < tol)
        {
            drop.push((2, f));
        }
    }
    gmsh.remove(&drop, true)?;
    gmsh.synchronize()?;
    let cuts = apply_cuts(gmsh, &options.cuts, scale)?;
    let overlaps = overlaps(gmsh, scale)?;
    let before = gmsh.entities(2)?;
    // The face each fragment piece came from.
    let mut parent: HashMap<i32, i32> = before.iter().map(|&(_, t)| (t, t)).collect();
    if before.len() > 1 {
        let pieces = gmsh.fragment(&before, &[])?;
        gmsh.synchronize()?;
        parent = before
            .iter()
            .zip(&pieces.map)
            .flat_map(|(&(_, p), kids)| kids.iter().map(move |&(_, t)| (t, p)))
            .collect();
    }
    let mut cad_sheets = Vec::new();
    if options.sheets != Sheets::None {
        remove_sheets(gmsh, &parent, options.symmetry, scale, &mut cad_sheets)?;
    }
    if options.sheets == Sheets::Largest {
        keep_largest(gmsh, &parent, &mut cad_sheets)?;
    }
    Ok(Prepared {
        file_unit: unit,
        unit_m,
        scaling,
        bounding_box,
        scale,
        halves,
        volume_cad,
        has_solids,
        faces: gmsh.tags(2)?,
        imported,
        cut_at_waterline,
        not_cut_at_waterline,
        dropped: drop.len(),
        overlaps,
        symmetry_cut,
        cuts,
        cad_sheets,
    })
}

/// Split the faces with planar cuts (rectangles); returns the report record: the faces each cut split, and the
/// cuts that split none.
pub(crate) fn apply_cuts(gmsh: &Session, cuts: &[Cut], scale: f64) -> Result<Value> {
    if cuts.is_empty() {
        return Ok(Value::Null);
    }
    let tol = 1e-6 * scale;
    let (mut split, mut unused) = (Vec::new(), Vec::new());
    for (k, cut) in cuts.iter().enumerate() {
        let n = DVec3::from(cut.normal).normalize();
        let u = DVec3::from(cut.axis).normalize();
        let v = n.cross(u);
        let corners: Vec<DVec3> = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .iter()
            .map(|&(a, b)| DVec3::from(cut.centre) + a * cut.half[0] * u + b * cut.half[1] * v)
            .collect();
        let lo = corners.iter().fold(DVec3::INFINITY, |m, &c| m.min(c));
        let hi = corners.iter().fold(DVec3::NEG_INFINITY, |m, &c| m.max(c));
        let reach = [lo.x, lo.y, lo.z, hi.x, hi.y, hi.z];
        // `face_at`: only the face nearest to that point (within 1 % of the body size).
        let only = match cut.face_at {
            Some(point) => {
                let mut best = (None, 1e-2 * scale);
                for f in gmsh.tags(2)? {
                    // The closest point is on the untrimmed surface: the face must also hold it.
                    let b = gmsh.bounding_box(2, f)?;
                    if !boxes_overlap(
                        &b,
                        &[point[0], point[1], point[2], point[0], point[1], point[2]],
                        best.1,
                    ) {
                        continue;
                    }
                    let (closest, uv) = gmsh.closest_point(2, f, &[point])?;
                    let d = distance(closest[0], point);
                    if d < best.1 && gmsh.inside(2, f, &uv[..2], true)? > 0 {
                        best = (Some(f), d);
                    }
                }
                Some(best.0)
            }
            None => None,
        };
        let mut faces = 0;
        for f in gmsh.tags(2)? {
            if only.is_some_and(|only| only != Some(f)) {
                continue;
            }
            if !boxes_overlap(&gmsh.bounding_box(2, f)?, &reach, tol) {
                continue;
            }
            if let Some(surface) = &cut.on
                && !on_surface(gmsh, f, surface, 1e-4 * scale)?
            {
                continue;
            }
            let rectangle = rectangle(gmsh, cut.centre.into(), n, u, cut.half)?;
            let pieces = gmsh.fragment(&[(2, f)], &[(2, rectangle)])?.map;
            gmsh.remove(&pieces[1], true)?;
            gmsh.synchronize()?;
            faces += usize::from(pieces[0].len() > 1);
        }
        split.push(faces);
        if faces == 0 {
            unused.push(k);
        }
    }
    Ok(json!({"cuts": cuts.len(), "faces_split": split, "cuts_unused": unused}))
}

/// Whether a face's surface lies on `surface`: a 3 x 3 grid of its parameter box within `tol`.
fn on_surface(gmsh: &Session, face: i32, surface: &Surface, tol: f64) -> Result<bool> {
    let (lo, hi) = gmsh.parametrization_bounds(2, face)?;
    let (lo, hi) = ([lo[0], lo[1]], [hi[0], hi[1]]);
    let uv: Vec<f64> = [0.1, 0.5, 0.9]
        .iter()
        .flat_map(|&a| {
            [0.1, 0.5, 0.9]
                .iter()
                .flat_map(move |&b| [lo[0] + a * (hi[0] - lo[0]), lo[1] + b * (hi[1] - lo[1])])
        })
        .collect();
    Ok(gmsh
        .value(2, face, &uv)?
        .into_iter()
        .all(|p| surface.distance(p) <= tol))
}

/// A rectangle face with `centre`, normal `n`, first axis `u` and half sizes `half`.
fn rectangle(gmsh: &Session, centre: DVec3, n: DVec3, u: DVec3, half: [f64; 2]) -> Result<i32> {
    // Drawn in the frame (u, v, n) at the centre's coordinates in that frame, then rotated into place.
    let frame = glam::DMat3::from_cols(u, n.cross(u), n);
    let local = frame.transpose() * centre;
    let face = gmsh.add_rectangle(
        [local.x - half[0], local.y - half[1], local.z],
        2. * half[0],
        2. * half[1],
    )?;
    let (axis, angle) = glam::DQuat::from_mat3(&frame).to_axis_angle();
    if angle.abs() > 1e-12 {
        gmsh.rotate(&[(2, face)], [0.; 3], axis.into(), angle)?;
    }
    Ok(face)
}

/// Import a body and keep its waterplane instead: the faces at z = 0 of its solids cut below the water,
/// fused where the sections of several solids overlap. Returns the section faces and their total area.
pub fn waterplane(gmsh: &Session, path: &Path, options: &Options) -> Result<(Vec<i32>, f64)> {
    let Opened {
        bounding_box,
        scale,
        ..
    } = open(gmsh, path, options, None)?;
    let halves = halves(options.symmetry, &bounding_box, scale);
    let solids = gmsh.entities(3)?;
    if solids.is_empty() {
        // No solids: prepare the wetted faces as for the body, then close their waterlines.
        gmsh.reset("")?;
        let prepared = body(gmsh, path, options, &mut |_, _| {})?;
        return waterplane_from_waterlines(gmsh, path, options.symmetry, &prepared, false);
    }
    let tol = 1e-6 * scale;
    let tool = below_water_box(gmsh, &halves, false)?;
    let cut = gmsh.intersect(&solids, &[(3, tool)])?.out;
    gmsh.synchronize()?;
    let mut faces = std::collections::BTreeSet::new();
    for &(_, v) in &cut {
        faces.extend(gmsh.adjacencies(3, v)?.1);
    }
    let mut lids = Vec::new();
    for f in faces {
        let b = gmsh.bounding_box(2, f)?;
        if b[2].abs().max(b[5].abs()) < tol {
            lids.push((2, f));
        }
    }
    if lids.is_empty() {
        bail!("{}: no solid crosses the waterline z = 0", display(path));
    }
    let mut keep = gmsh.copy(&lids)?;
    gmsh.remove(&cut, true)?;
    gmsh.synchronize()?;
    if keep.len() > 1 {
        keep = gmsh.fuse(&keep[..1], &keep[1..])?.out;
        gmsh.synchronize()?;
    }
    let others: Vec<DimTag> = gmsh
        .entities(2)?
        .into_iter()
        .filter(|e| !keep.contains(e))
        .collect();
    gmsh.remove(&others, true)?;
    gmsh.synchronize()?;
    let faces = gmsh.tags(2)?;
    let mut area = 0.;
    for &f in &faces {
        area += gmsh.mass(2, f)?;
    }
    Ok((faces, area))
}

/// The waterplane of a model without solids: the curves of the wetted faces on z = 0, closed along a
/// symmetry plane by straight segments, as plane faces; a loop inside another (a moonpool) is a hole. The
/// wetted faces are removed. Returns the plane faces and their area.
/// The waterplane of the prepared body, added to its model on the body's own waterline curves: meshed with the
/// body, the waterplane shares the body's waterline nodes whatever the mesher. Returns the plane faces and
/// their area.
pub fn waterplane_on_body(
    gmsh: &Session,
    path: &Path,
    symmetry: Symmetry,
    prepared: &Prepared,
) -> Result<(Vec<i32>, f64)> {
    waterplane_from_waterlines(gmsh, path, symmetry, prepared, true)
}

/// The waterplane from the curves of the wetted faces on z = 0 (`keep`: the wetted faces stay in the model).
fn waterplane_from_waterlines(
    gmsh: &Session,
    path: &Path,
    symmetry: Symmetry,
    prepared: &Prepared,
    keep: bool,
) -> Result<(Vec<i32>, f64)> {
    let tol = 1e-6 * prepared.scale;
    let mut lines = Vec::new();
    for c in gmsh.tags(1)? {
        let (lo, hi) = gmsh.parametrization_bounds(1, c)?;
        let x = gmsh.value(1, c, &linspace(lo[0], hi[0], 9))?;
        let flat = x.iter().all(|p| p[2].abs() < tol) && x.iter().any(|p| distance(*p, x[0]) > tol);
        if flat && !gmsh.adjacencies(1, c)?.0.is_empty() {
            lines.push(c);
        }
    }
    if lines.is_empty() {
        bail!(
            "{}: no waterline (no wetted face reaches z = 0)",
            display(path)
        );
    }
    // Ends of one waterline curve lie on a symmetry plane: count the end points, in order of appearance.
    let mut ends: Vec<(i32, usize)> = Vec::new();
    for &c in &lines {
        for (_, p) in gmsh.boundary(&[(1, c)], true, false, false)? {
            let p = p.abs();
            match ends.iter_mut().find(|(q, _)| *q == p) {
                Some(entry) => entry.1 += 1,
                None => ends.push((p, 1)),
            }
        }
    }
    let loose: Vec<i32> = ends
        .iter()
        .filter(|(_, n)| *n == 1)
        .map(|(p, _)| *p)
        .collect();
    let mut closing = Vec::new();
    // Close y = 0 first, then x = 0. With both planes, an odd end on a plane runs to the corner at the origin.
    for k in [1, 0] {
        if !symmetry.axes().contains(&k) {
            continue;
        }
        let mut on = Vec::new();
        for &p in &loose {
            let x = gmsh.value(0, p, &[])?[0];
            if x[k].abs() < tol {
                on.push((p, x[1 - k]));
            }
        }
        if on.len() % 2 == 1 && symmetry == Symmetry::XzAndYz {
            on.push((origin_point(gmsh, tol)?, 0.));
        }
        on.sort_by(|a, b| a.1.total_cmp(&b.1));
        for pair in on.as_chunks::<2>().0 {
            closing.push(gmsh.add_line(pair[0].0, pair[1].0)?);
        }
    }
    if !keep {
        let wetted: Vec<DimTag> = prepared.faces.iter().map(|&f| (2, f)).collect();
        gmsh.remove(&wetted, false)?; // their curves stay
    }
    gmsh.synchronize()?;
    lines.extend(closing);
    let loops = waterline_loops(gmsh, &lines)?;
    if loops.is_empty() {
        bail!(
            "{}: the waterlines do not close (symmetry '{}')",
            display(path),
            symmetry.name()
        );
    }
    let mut areas = Vec::new();
    for lp in &loops {
        areas.push(loop_area(&loop_points(gmsh, lp)?).abs());
    }
    let mut order: Vec<usize> = (0..loops.len()).collect();
    order.sort_by(|&a, &b| areas[b].total_cmp(&areas[a]));
    let mut outer: Vec<(usize, Vec<usize>)> = Vec::new();
    for k in order {
        let mut host = None;
        for (i, (o, _)) in outer.iter().enumerate() {
            if inside(
                &loop_points(gmsh, &loops[k])?,
                &loop_points(gmsh, &loops[*o])?,
            ) {
                host = Some(i);
                break;
            }
        }
        match host {
            Some(i) => outer[i].1.push(k),
            None => outer.push((k, Vec::new())),
        }
    }
    let mut faces = Vec::new();
    for (lp, holes) in &outer {
        let mut wires = vec![gmsh.add_curve_loop(&loops[*lp])?];
        for &h in holes {
            wires.push(gmsh.add_curve_loop(&loops[h])?);
        }
        faces.push(gmsh.add_plane_surface(&wires)?);
    }
    gmsh.synchronize()?;
    let mut area = 0.;
    for &f in &faces {
        area += gmsh.mass(2, f)?;
    }
    Ok((faces, area))
}

/// A point at the origin (the corner of the symmetry planes on z = 0): an existing one, or a new one.
fn origin_point(gmsh: &Session, tol: f64) -> Result<i32> {
    for p in gmsh.tags(0)? {
        let x = gmsh.value(0, p, &[])?[0];
        if DVec3::from_array(x).length() < tol {
            return Ok(p);
        }
    }
    let p = gmsh.add_point([0.; 3])?;
    gmsh.synchronize()?;
    Ok(p)
}

/// The end points of a curve (one for a closed curve).
fn curve_ends(gmsh: &Session, c: i32) -> Result<Vec<i32>> {
    Ok(gmsh
        .boundary(&[(1, c)], true, false, false)?
        .into_iter()
        .map(|(_, p)| p.abs())
        .collect())
}

/// The closed loops of curves chained by their end points; empty when a chain does not close.
fn waterline_loops(gmsh: &Session, curves: &[i32]) -> Result<Vec<Vec<i32>>> {
    gmsh.synchronize()?;
    let mut ends = BTreeMap::new();
    for &c in curves {
        ends.insert(c, curve_ends(gmsh, c)?);
    }
    let mut rest: std::collections::BTreeSet<i32> = curves.iter().copied().collect();
    let mut loops = Vec::new();
    for &c in curves {
        let distinct: std::collections::BTreeSet<_> = ends[&c].iter().collect();
        if distinct.len() < 2 {
            loops.push(vec![c]); // a closed curve (a whole circle) is a loop of its own
            rest.remove(&c);
        }
    }
    while let Some(c) = rest.pop_first() {
        let mut lp = vec![c];
        let (start, mut end) = (ends[&c][0], *ends[&c].last().unwrap_or(&ends[&c][0]));
        while end != start {
            let Some(next) = rest.iter().copied().find(|d| ends[d].contains(&end)) else {
                return Ok(Vec::new());
            };
            rest.remove(&next);
            lp.push(next);
            end = if ends[&next][0] == end {
                ends[&next][1]
            } else {
                ends[&next][0]
            };
        }
        loops.push(lp);
    }
    Ok(loops)
}

/// Samples (x, y) along a loop of curves, in loop order.
fn loop_points(gmsh: &Session, lp: &[i32]) -> Result<Vec<[f64; 2]>> {
    let mut out: Vec<[f64; 2]> = Vec::new();
    let mut end: Option<[f64; 2]> = None;
    for &c in lp {
        let (lo, hi) = gmsh.parametrization_bounds(1, c)?;
        let mut x: Vec<[f64; 2]> = gmsh
            .value(1, c, &linspace(lo[0], hi[0], 33))?
            .into_iter()
            .map(|p| [p[0], p[1]])
            .collect();
        if let Some(e) = end {
            let d = |p: [f64; 2]| (p[0] - e[0]).hypot(p[1] - e[1]);
            if d(x[x.len() - 1]) < d(x[0]) {
                x.reverse();
            }
        }
        end = x.last().copied();
        out.extend(x);
    }
    Ok(out)
}

/// The signed area enclosed by sampled loop points (the shoelace formula).
fn loop_area(points: &[[f64; 2]]) -> f64 {
    let n = points.len();
    let (mut a, mut b) = (0., 0.);
    for i in 0..n {
        let j = (i + 1) % n;
        a += points[i][0] * points[j][1];
        b += points[i][1] * points[j][0];
    }
    0.5 * (a - b)
}

/// Whether the first point of loop `a` lies inside loop `b` (the crossing number).
fn inside(a: &[[f64; 2]], b: &[[f64; 2]]) -> bool {
    let p = a[0];
    let mut count = 0;
    for i in 0..b.len() {
        let (x, y) = (b[i][0], b[i][1]);
        let (x2, y2) = (b[(i + 1) % b.len()][0], b[(i + 1) % b.len()][1]);
        let crosses = (y > p[1]) != (y2 > p[1]);
        let xs = x + (p[1] - y) * (x2 - x) / if y2 != y { y2 - y } else { 1. };
        if crosses && p[0] < xs {
            count += 1;
        }
    }
    count % 2 == 1
}

/// The sides of the prepared faces decided by the CAD solids.
pub struct Sides {
    /// Per face: +1 or -1 when its CAD normal points into or out of the body, None when undecided.
    pub inward: BTreeMap<i32, Option<i32>>,
    /// The number of offset points classified.
    pub tested: usize,
}

/// The inward side of every prepared face from the original CAD solids.
///
/// Five well-separated points inside each trimmed face are offset along the CAD normal both ways and
/// classified in a separate copy of the solids (denser sample grids for thin faces). A probe is clear when
/// exactly one of its two points is inside. A face is decided when at least three probes are clear and all
/// clear probes agree; otherwise (too few samples, a face where solids touch, disagreeing probes) it is left
/// undecided, and the whole model is then oriented by the generalized winding
/// number of its mesh (see `winding`).
pub fn face_sides(gmsh: &Session, path: &Path, prepared: &Prepared) -> Result<Sides> {
    if !prepared.has_solids {
        return Ok(Sides {
            inward: prepared.faces.iter().map(|&f| (f, None)).collect(),
            tested: 0,
        });
    }
    let mut probes = BTreeMap::new();
    for &face in &prepared.faces {
        let (lo, hi) = gmsh.parametrization_bounds(2, face)?;
        // A thin piece (a cut band, a short pipe end) can miss a coarse grid: denser grids are tried next.
        let mut uv = Vec::new();
        for (n, margin) in [(9, 0.1), (17, 0.03), (33, 0.01)] {
            uv.clear();
            let fractions = linspace(margin, 1. - margin, n);
            for &u in &fractions {
                for &v in &fractions {
                    let p = [lo[0] + u * (hi[0] - lo[0]), lo[1] + v * (hi[1] - lo[1])];
                    if gmsh.inside(2, face, &p, true)? != 0 {
                        uv.push(p);
                    }
                }
            }
            if uv.len() >= 5 {
                break;
            }
        }
        if uv.len() < 5 {
            probes.insert(face, Vec::new());
            continue;
        }
        let xyz = gmsh.value(2, face, uv.as_flattened())?;
        let normals = gmsh.normal(face, uv.as_flattened())?;
        let points: Vec<DVec3> = xyz.iter().map(|&p| DVec3::from_array(p)).collect();
        let mean = points.iter().sum::<DVec3>() / points.len() as f64;
        let first = argmin(points.iter().map(|p| p.distance(mean)));
        let mut chosen = vec![first];
        let mut distance: Vec<f64> = points.iter().map(|p| p.distance(points[first])).collect();
        for _ in 0..4 {
            for &c in &chosen {
                distance[c] = -1.;
            }
            let k = argmax(distance.iter().copied());
            chosen.push(k);
            for (d, p) in distance.iter_mut().zip(&points) {
                *d = d.min(p.distance(points[k]));
            }
        }
        let lo = points.iter().fold(DVec3::INFINITY, |a, &p| a.min(p));
        let hi = points.iter().fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
        let size = (hi - lo).length();
        probes.insert(
            face,
            chosen
                .into_iter()
                .map(|k| (points[k], DVec3::from_array(normals[k]), size))
                .collect::<Vec<_>>(),
        );
    }
    let current = gmsh.current_model()?;
    gmsh.add_model(&format!("{current}_solid_orientation"))?;
    let decided = (|| -> Result<Sides> {
        import(gmsh, path, prepared.scaling)?;
        let solids = gmsh.tags(3)?;
        let mut sides = Sides {
            inward: BTreeMap::new(),
            tested: 0,
        };
        for (&face, candidates) in &probes {
            let mut votes = Vec::new();
            for &(x, normal, size) in candidates {
                let h = (1e-4 * prepared.scale).max((0.01 * size).min(1e-3 * prepared.scale));
                let mut inside = [false; 2];
                for (side, s) in inside.iter_mut().zip([1., -1.]) {
                    let point = (x + s * h * normal).to_array();
                    for &tag in &solids {
                        if gmsh.inside(3, tag, &point, false)? != 0 {
                            *side = true;
                            break;
                        }
                    }
                }
                sides.tested += 2;
                if inside[0] != inside[1] {
                    votes.push(if inside[0] { 1 } else { -1 });
                }
            }
            // A probe whose two offset points are both inside (or both outside) a solid says nothing; at least
            // three clear probes must agree and none may disagree.
            let unanimous = votes.len() >= 3 && votes.iter().all(|&v| v == votes[0]);
            sides.inward.insert(face, unanimous.then(|| votes[0]));
        }
        Ok(sides)
    })();
    gmsh.remove_model()?;
    gmsh.set_current_model(&current)?;
    // Gmsh's global model size (its tolerances scale with it) is the last synchronized model's: the whole
    // file imported above. Synchronizing the body again gives it back the body's own.
    gmsh.synchronize()?;
    decided
}

/// Index of the first smallest value.
fn argmin(values: impl Iterator<Item = f64>) -> usize {
    let mut best = (0, f64::INFINITY);
    for (i, v) in values.enumerate() {
        if v < best.1 {
            best = (i, v);
        }
    }
    best.0
}

/// Index of the first largest value.
fn argmax(values: impl Iterator<Item = f64>) -> usize {
    let mut best = (0, f64::NEG_INFINITY);
    for (i, v) in values.enumerate() {
        if v > best.1 {
            best = (i, v);
        }
    }
    best.0
}

/// Evenly spaced values as NumPy's `linspace` computes them: `i * step + start`, the last one `stop`.
pub fn linspace(start: f64, stop: f64, n: usize) -> Vec<f64> {
    let step = (stop - start) / (n - 1) as f64;
    let mut out: Vec<f64> = (0..n).map(|i| i as f64 * step + start).collect();
    if let Some(last) = out.last_mut() {
        *last = stop;
    }
    out
}

/// A number with `digits` significant digits in the `%g` style of C's printf (no trailing zeros,
/// exponent below 1e-4 or from 10^digits), for messages.
pub fn general(x: f64, digits: usize) -> String {
    if x == 0. || !x.is_finite() {
        return format!("{x}");
    }
    let exponent = x.abs().log10().floor() as i32;
    let rounded: f64 = format!("{x:.*e}", digits - 1).parse().unwrap_or(x);
    let exponent = if rounded.abs() >= 10f64.powi(exponent + 1) {
        exponent + 1
    } else {
        exponent
    };
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s
        }
    };
    if exponent < -4 || exponent >= digits as i32 {
        let mantissa = rounded / 10f64.powi(exponent);
        let sign = if exponent < 0 { '-' } else { '+' };
        format!(
            "{}e{sign}{:02}",
            trim(format!("{mantissa:.*}", digits - 1)),
            exponent.abs()
        )
    } else {
        let decimals = (digits as i32 - 1 - exponent).max(0) as usize;
        trim(format!("{rounded:.decimals$}"))
    }
}

#[cfg(test)]
mod tests {
    use super::general;

    #[test]
    fn general_matches_percent_g() {
        assert_eq!(general(11.2, 3), "11.2");
        assert_eq!(general(0.4230, 3), "0.423");
        assert_eq!(general(1.23456e-5, 3), "1.23e-05");
        assert_eq!(general(123456., 3), "1.23e+05");
        assert_eq!(general(99.96, 3), "100");
        assert_eq!(general(2.0, 3), "2");
    }
}

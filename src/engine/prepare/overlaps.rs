// SPDX-License-Identifier: GPL-3.0-or-later
//! Overlapping faces: face samples, the share of one face's samples on another, and the duplicates removed.

use super::*;

/// Samples of a face: an n x n grid of its parameter box kept inside the trimmed face, with points and
/// unit normals.
#[derive(Clone)]
pub(super) struct Samples {
    pub(super) x: Vec<[f64; 3]>,
    normals: Vec<[f64; 3]>,
}

pub(super) type SampleKey = (Vec<u64>, usize);
/// Samples of faces met before in this process (the resident view server), by face geometry and grid size.
pub(super) static SAMPLES: LazyLock<Mutex<HashMap<SampleKey, Samples>>> =
    LazyLock::new(Default::default);
/// The sample cache is emptied when it holds more faces than this.
pub(super) const CACHED_SAMPLES: usize = 50_000;

/// [`sample_face`], remembered by face geometry (sampling a trimmed face is slow).
pub(super) fn samples(gmsh: &Session, face: i32, n: usize) -> Result<Samples> {
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

pub(super) fn sample_face(gmsh: &Session, face: i32, n: usize) -> Result<Samples> {
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

pub(super) fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    DVec3::from_array(a).distance(DVec3::from_array(b))
}

/// The fraction of the samples of face `a` that lie on face `b`: near it, inside its trimmed domain and
/// with parallel normals.
pub(super) fn covered(
    gmsh: &Session,
    a: &Samples,
    b: i32,
    box_b: &[f64; 6],
    tol: f64,
) -> Result<f64> {
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

pub(super) fn boxes_overlap(a: &[f64; 6], b: &[f64; 6], tol: f64) -> bool {
    (0..3).all(|k| a[k] <= b[k + 3] + tol && b[k] <= a[k + 3] + tol)
}

pub(super) fn round3(x: f64) -> f64 {
    (x * 1000.).round() / 1000.
}

/// Overlap fractions of face pairs met before in this process, by the faces' geometry and the tolerance: a
/// resident view server does not sample unchanged faces again.
pub(super) static PAIRS: LazyLock<Mutex<HashMap<PairKey, (f64, f64)>>> =
    LazyLock::new(Default::default);
pub(super) type PairKey = (Vec<u64>, Vec<u64>, u64);
/// The pair cache is emptied when it holds more pairs than this.
pub(super) const CACHED_PAIRS: usize = 200_000;

/// Pairs of faces that cover the same area twice; the more covered face is cut by the other, or removed
/// when it is covered entirely.
pub(super) fn overlaps(gmsh: &Session, scale: f64) -> Result<Vec<Value>> {
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

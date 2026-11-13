// SPDX-License-Identifier: GPL-3.0-or-later
//! Structured low-order panels (lo-str): the spline patches of the body divided on grids of their own
//! parameters, so the panels follow the patch layout and, on ruled faces, its straight lines.
//!
//! 1. A patch corner inside an edge of another patch (a T-junction) stays: the panels of the two sides meet with
//!    hanging nodes (the solver takes them; cutting the patch there doubled the panels, see `T_JUNCTION_ROUNDS`).
//! 2. Opposite edges of a patch share one panel count, from the longer edge; a shared edge links the counts
//!    of its patches, always on a curved edge, on a straight one while the linked counts stay within
//!    `CONFORM_RATIO` (beyond it the two sides meet with hanging nodes on one line).
//! 3. Edges are divided into equal arc lengths, the interior parameters are the Coons blend.
//! 4. Panels follow the patch normal (into the body) reversed: their normal points into the fluid; collapsed
//!    edges give triangles, non-convex quadrilaterals are split.

use super::{
    bspline::{self, Patch, unit_grid},
    checks::{self, matching_edges},
    ho,
    kdtree::KdTree,
    lofree::{self, body_checks, interior_entry, push_convex, state},
    panels::{Panel, Panels, normal, reversed},
    prepare::{Progress, linspace},
};
use crate::conversion::{Mode, Spec, Symmetry};
use anyhow::{Context, Result, ensure};
use glam::DVec3;
use serde_json::{Value, json};
use std::{collections::HashMap, time::Instant};

const EDGE_SAMPLES: usize = 2001;
/// Linked counts differ by at most this factor on a straight shared edge.
const CONFORM_RATIO: f64 = 1.5;
/// Rounds of T-junction cuts: none. The solver takes panels that meet with hanging nodes, and the cuts doubled
/// the panels (an OC4 platform with braces: 23,450 with them, 11,290 without, the enclosed volume 0.06 % smaller
/// from chord gaps where a T-junction lies on a curved edge). The cut is kept for a caller that wants corner-to-corner panels.
const T_JUNCTION_ROUNDS: usize = 0;

/// The result of a structured panel conversion.
pub struct Conversion {
    pub body: Panels,
    pub interior: Option<Panels>,
    pub summary: Value,
    pub checks: Value,
    pub cad: Value,
}

/// The (s, t) of edge k (0: t = 0, 1: s = 1, 2: t = 1, 3: s = 0) at the edge parameter w.
fn edge_param(k: usize, w: f64) -> (f64, f64) {
    match k {
        0 => (w, 0.),
        1 => (1., w),
        2 => (w, 1.),
        _ => (0., w),
    }
}

fn edge(p: &Patch, k: usize, n: usize) -> Vec<DVec3> {
    unit_grid(n)
        .into_iter()
        .map(|w| {
            let (s, t) = edge_param(k, w);
            p.at(s, t)
        })
        .collect()
}

/// The sub-patches of a patch cut at the parameters s_cuts and t_cuts.
fn pieces(p: &Patch, s_cuts: &[f64], t_cuts: &[f64]) -> Vec<Patch> {
    let sorted = |cuts: &[f64]| {
        let mut v = vec![0.];
        let mut c = cuts.to_vec();
        c.sort_by(f64::total_cmp);
        v.extend(c);
        v.push(1.);
        v
    };
    let (s, t) = (sorted(s_cuts), sorted(t_cuts));
    let mut out = Vec::new();
    for tw in t.windows(2) {
        for sw in s.windows(2) {
            out.push(bspline::normalized(&bspline::restrict(
                p, sw[0], sw[1], tw[0], tw[1],
            )));
        }
    }
    out
}

/// The distance of q to edge k and the edge parameter of its closest point.
fn closest_on_edge(p: &Patch, k: usize, q: DVec3) -> (f64, f64) {
    let n = 401;
    let g = unit_grid(n);
    let x: Vec<DVec3> = g
        .iter()
        .map(|&w| {
            let (s, t) = edge_param(k, w);
            p.at(s, t)
        })
        .collect();
    let chord = x[n - 1] - x[0];
    let length2 = chord.dot(chord);
    let straight = length2 > 0.
        && x.iter()
            .zip(&g)
            .all(|(p, &w)| p.distance(x[0] + w * chord) < 1e-12 * length2.sqrt());
    if straight {
        let w = ((q - x[0]).dot(chord) / length2).clamp(0., 1.);
        return ((x[0] + w * chord).distance(q), w);
    }
    let i = (0..n)
        .min_by(|&a, &b| x[a].distance(q).total_cmp(&x[b].distance(q)))
        .unwrap_or(0);
    let (lo, hi) = (g[i.saturating_sub(1)], g[(i + 1).min(n - 1)]);
    let at = |w: f64| {
        let (s, t) = edge_param(k, w);
        p.at(s, t)
    };
    // Newton steps along the edge, then a golden-section search if they do not settle.
    let mut w = g[i];
    for _ in 0..10 {
        let (s, t) = edge_param(k, w);
        let (point, xu, xv) = p.frame(s, t);
        let tangent = if k.is_multiple_of(2) { xu } else { xv };
        let delta = -(point - q).dot(tangent) / tangent.dot(tangent).max(1e-300);
        let next = (w + delta).clamp(lo, hi);
        let step = (next - w).abs();
        w = next;
        if step <= 1e-12 {
            return (at(w).distance(q), w);
        }
    }
    let r = (5f64.sqrt() - 1.) / 2.;
    let (mut a, mut b) = (lo, hi);
    for _ in 0..60 {
        let (c, d) = (b - r * (b - a), a + r * (b - a));
        if at(c).distance(q) < at(d).distance(q) {
            b = d;
        } else {
            a = c;
        }
        if b - a < 1e-12 {
            break;
        }
    }
    let w = 0.5 * (a + b);
    (at(w).distance(q), w)
}

/// Per patch the parameters (along s, along t) where a corner of another patch lies inside its edges.
fn t_junction_cuts(patches: &[Patch], tol: f64) -> Vec<(Vec<f64>, Vec<f64>)> {
    let mut corners: Vec<DVec3> = Vec::new();
    for p in patches {
        for (s, t) in [(0., 0.), (1., 0.), (1., 1.), (0., 1.)] {
            let c = p.at(s, t);
            let snap = (tol * 1e-6).max(f64::MIN_POSITIVE);
            if !corners
                .iter()
                .any(|x| ((*x - c) / snap).round() == DVec3::ZERO)
            {
                corners.push(c);
            }
        }
    }
    let tree = KdTree::new(corners.clone());
    let lo = corners.iter().fold(DVec3::INFINITY, |a, &p| a.min(p));
    let hi = corners.iter().fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
    let matched = matching_edges(patches, (hi - lo).length().max(1e-300));
    let mut out = Vec::new();
    for (index, p) in patches.iter().enumerate() {
        let mut cuts: (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
        for (k, &already) in matched[index].iter().enumerate() {
            if already {
                continue; // a complete coincident neighbour already covers this edge
            }
            let e = edge(p, k, 33);
            let elo = e.iter().fold(DVec3::INFINITY, |a, &p| a.min(p)) - DVec3::splat(tol);
            let ehi = e.iter().fold(DVec3::NEG_INFINITY, |a, &p| a.max(p)) + DVec3::splat(tol);
            let centre = (elo + ehi) / 2.;
            let radius = (ehi - elo).length() / 2.;
            for c in tree
                .within(centre, radius * (1. + 1e-12))
                .into_iter()
                .map(|i| tree.point(i))
            {
                if c.cmplt(elo).any() || c.cmpgt(ehi).any() {
                    continue;
                }
                if c.distance(e[0]).min(c.distance(e[32])) < 10. * tol {
                    continue; // at an end of this edge
                }
                let (d, w) = closest_on_edge(p, k, c);
                let list = if k % 2 == 0 { &mut cuts.0 } else { &mut cuts.1 };
                if d < tol
                    && 1e-6 < w
                    && w < 1. - 1e-6
                    && list.iter().all(|&x| (w - x).abs() > 1e-6)
                {
                    list.push(w);
                }
            }
        }
        out.push(cuts);
    }
    out
}

/// Patches cut where a corner of another patch lies inside their edges, `rounds` times at most; the patches,
/// their faces and how many still have a T-junction.
fn remove_t_junctions(
    mut patches: Vec<Patch>,
    mut faces: Vec<i32>,
    tol: f64,
    rounds: usize,
) -> (Vec<Patch>, Vec<i32>, usize) {
    for _ in 0..rounds {
        let cuts = t_junction_cuts(&patches, tol);
        if cuts.iter().all(|(s, t)| s.is_empty() && t.is_empty()) {
            return (patches, faces, 0);
        }
        let (mut next, mut next_faces) = (Vec::new(), Vec::new());
        for ((p, f), (s, t)) in patches.iter().zip(&faces).zip(&cuts) {
            let parts = if s.is_empty() && t.is_empty() {
                vec![p.clone()]
            } else {
                pieces(p, s, t)
            };
            next_faces.extend(std::iter::repeat_n(*f, parts.len()));
            next.extend(parts);
        }
        patches = next;
        faces = next_faces;
    }
    let left = t_junction_cuts(&patches, tol)
        .iter()
        .filter(|(s, t)| !s.is_empty() || !t.is_empty())
        .count();
    (patches, faces, left)
}

/// The edge parameters that divide edge k into n equal arc lengths.
fn arc_params(p: &Patch, k: usize, n: usize) -> Vec<f64> {
    if n == 1 {
        return vec![0., 1.];
    }
    let g = unit_grid(EDGE_SAMPLES);
    let x = edge(p, k, EDGE_SAMPLES);
    let mut s = vec![0.];
    for w in x.windows(2) {
        s.push(s[s.len() - 1] + w[0].distance(w[1]));
    }
    let total = s[s.len() - 1];
    if total <= 0. {
        return unit_grid(n + 1);
    }
    linspace(0., total, n + 1)
        .into_iter()
        .map(|target| {
            let i = s
                .partition_point(|&v| v <= target)
                .saturating_sub(1)
                .min(s.len() - 2);
            let span = s[i + 1] - s[i];
            let f = if span > 0. {
                (target - s[i]) / span
            } else {
                0.
            };
            g[i] + f * (g[i + 1] - g[i])
        })
        .collect()
}

/// Labels per point, equal for points closer than tol (connected).
fn cluster(points: &[DVec3], tol: f64) -> Vec<usize> {
    let tree = KdTree::new(points.to_vec());
    let mut parent: Vec<usize> = (0..points.len()).collect();
    fn find(parent: &mut [usize], mut a: usize) -> usize {
        while parent[a] != a {
            parent[a] = parent[parent[a]];
            a = parent[a];
        }
        a
    }
    for (i, p) in points.iter().enumerate() {
        for j in tree.within(*p, tol) {
            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    (0..points.len()).map(|i| find(&mut parent, i)).collect()
}

/// Panel counts (along s, along t) of every patch and the shared straight edges left not conforming.
fn counts(patches: &[Patch], size: f64, tol: f64) -> (Vec<[usize; 2]>, usize) {
    let count = patches.len();
    let (mut ends, mut mids, mut info, mut straight) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut own = vec![1usize; 2 * count];
    for (j, p) in patches.iter().enumerate() {
        for k in 0..4 {
            let e = edge(p, k, 257);
            let mut s = vec![0.];
            for w in e.windows(2) {
                s.push(s[s.len() - 1] + w[0].distance(w[1]));
            }
            let total = s[s.len() - 1];
            ends.push(e[0]);
            ends.push(e[256]);
            let half = 0.5 * total;
            let i = s
                .partition_point(|&v| v <= half)
                .saturating_sub(1)
                .min(s.len() - 2);
            let span = s[i + 1] - s[i];
            let f = if span > 0. { (half - s[i]) / span } else { 0. };
            mids.push(e[i] + f * (e[i + 1] - e[i]));
            info.push((j, k));
            straight.push(total - e[256].distance(e[0]) <= 1e-9 * total.max(1e-300));
            let slot = 2 * j + k % 2;
            own[slot] = own[slot].max((total / size - 1e-9).ceil() as usize);
        }
    }
    let node = cluster(&ends, tol);
    let mid = cluster(&mids, tol);
    // per shared edge (end nodes, midpoint): its count slots and whether every side is straight
    type Group = ((usize, usize, usize), Vec<usize>, bool);
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<(usize, usize, usize), usize> = HashMap::new();
    for (i, &(j, k)) in info.iter().enumerate() {
        let (a, b) = (node[2 * i], node[2 * i + 1]);
        if a == b {
            continue; // a collapsed edge
        }
        let key = (a.min(b), a.max(b), mid[i]);
        let g = *index.entry(key).or_insert_with(|| {
            groups.push((key, Vec::new(), true));
            groups.len() - 1
        });
        groups[g].1.push(2 * j + k % 2);
        groups[g].2 &= straight[i];
    }
    let mut links: Vec<(usize, usize, bool)> = Vec::new();
    for (_, members, flat) in &groups {
        for &x in &members[1..] {
            if x != members[0] {
                links.push((members[0], x, *flat));
            }
        }
    }
    let ratio = |a: usize, b: usize| own[a].max(own[b]) as f64 / own[a].min(own[b]) as f64;
    links.sort_by(|x, y| {
        (x.2, ratio(x.0, x.1))
            .partial_cmp(&(y.2, ratio(y.0, y.1)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut parent: Vec<usize> = (0..2 * count).collect();
    let (mut lo, mut hi) = (own.clone(), own.clone());
    fn find(parent: &mut [usize], mut a: usize) -> usize {
        while parent[a] != a {
            parent[a] = parent[parent[a]];
            a = parent[a];
        }
        a
    }
    let mut broken = 0;
    for (a, b, flat) in links {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra == rb {
            continue;
        }
        if flat && hi[ra].max(hi[rb]) as f64 > CONFORM_RATIO * lo[ra].min(lo[rb]) as f64 {
            broken += 1;
            continue;
        }
        parent[rb] = ra;
        lo[ra] = lo[ra].min(lo[rb]);
        hi[ra] = hi[ra].max(hi[rb]);
    }
    let n = (0..count)
        .map(|j| {
            [
                hi[find(&mut parent, 2 * j)],
                hi[find(&mut parent, 2 * j + 1)],
            ]
        })
        .collect();
    (n, broken)
}

/// The panel corners of a patch: equal arc lengths on its edges, the Coons blend inside; nt + 1 rows.
fn grid(p: &Patch, ns: usize, nt: usize) -> Vec<Vec<DVec3>> {
    let (b, t) = (arc_params(p, 0, ns), arc_params(p, 2, ns));
    let (l, r) = (arc_params(p, 3, nt), arc_params(p, 1, nt));
    let xi = unit_grid(ns + 1);
    let eta = unit_grid(nt + 1);
    (0..=nt)
        .map(|j| {
            (0..=ns)
                .map(|i| {
                    let mut s = (1. - eta[j]) * b[i] + eta[j] * t[i];
                    let mut tt = (1. - xi[i]) * l[j] + xi[i] * r[j];
                    if i == 0 {
                        s = 0.;
                    } else if i == ns {
                        s = 1.;
                    }
                    if j == 0 {
                        tt = 0.;
                    } else if j == nt {
                        tt = 1.;
                    }
                    p.at(s, tt)
                })
                .collect()
        })
        .collect()
}

/// A grid quadrilateral as a panel: corners closer than 1e-6 of its diameter are one; three corners give
/// the triangle (a, b, c, c); fewer, or no area, drop it.
fn panel(q: [DVec3; 4]) -> Option<Panel> {
    let mut d: f64 = 0.;
    for i in 0..4 {
        for j in i + 1..4 {
            d = d.max(q[i].distance(q[j]));
        }
    }
    if d == 0. {
        return None;
    }
    let mut ring = vec![q[0]];
    for k in 1..4 {
        if q[k].distance(q[k - 1]) > 1e-6 * d {
            ring.push(q[k]);
        }
    }
    if ring.len() > 1 && ring[ring.len() - 1].distance(ring[0]) <= 1e-6 * d {
        ring.pop();
    }
    if ring.len() < 3 {
        return None;
    }
    let area = 0.5
        * (0..ring.len())
            .map(|k| ring[k].cross(ring[(k + 1) % ring.len()]))
            .sum::<DVec3>()
            .length();
    if area <= 1e-6 * d * d {
        return None;
    }
    while ring.len() < 4 {
        ring.push(ring[ring.len() - 1]);
    }
    Some([
        ring[0].to_array(),
        ring[1].to_array(),
        ring[2].to_array(),
        ring[3].to_array(),
    ])
}

/// The panels of the patches (normal into the fluid), split counts, counts and edges not conforming.
fn panels_of(
    patches: &[Patch],
    faces: &[i32],
    size: f64,
    tol: f64,
    symmetry: Symmetry,
    label: String,
) -> (Panels, usize, Vec<[usize; 2]>, usize) {
    let (n, broken) = counts(patches, size, tol);
    let mut out = Panels {
        vertices: Vec::new(),
        faces: Vec::new(),
        symmetry,
        label,
    };
    let mut splits = 0;
    for ((p, &f), [ns, nt]) in patches.iter().zip(faces).zip(&n) {
        let x = grid(p, *ns, *nt);
        let mut panels = Vec::new();
        for j in 0..*nt {
            for i in 0..*ns {
                // (j, i), (j + 1, i), (j + 1, i + 1), (j, i + 1): the normal into the fluid
                if let Some(q) = panel([x[j][i], x[j + 1][i], x[j + 1][i + 1], x[j][i + 1]]) {
                    panels.push(q);
                }
            }
        }
        splits += push_convex(panels, f, &mut out);
    }
    (out, splits, n, broken)
}

/// Panel edges used once, apart from z = 0 and the symmetry planes: (holes, hanging edges along another).
fn free_edges(panels: &Panels, size: f64, symmetry: Symmetry, tol: f64) -> (usize, usize) {
    let points: Vec<DVec3> = panels
        .vertices
        .iter()
        .flatten()
        .map(|&p| DVec3::from_array(p))
        .collect();
    let label = cluster(&points, tol);
    let mut position: HashMap<usize, DVec3> = HashMap::new();
    for (l, p) in label.iter().zip(&points) {
        position.entry(*l).or_insert(*p);
    }
    let mut count: HashMap<(usize, usize), usize> = HashMap::new();
    for row in label.chunks(4) {
        let mut ring: Vec<usize> = Vec::new();
        for (k, &t) in row.iter().enumerate() {
            if k == 0 || t != row[k - 1] {
                ring.push(t);
            }
        }
        if ring.len() > 1 && ring.last() == ring.first() {
            ring.pop();
        }
        for i in 0..ring.len() {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut planes = vec![2];
    planes.extend(symmetry.axes());
    let eps = 1e-6 * size;
    let once: Vec<(DVec3, DVec3)> = count
        .into_iter()
        .filter(|&((a, b), c)| {
            c == 1
                && !planes
                    .iter()
                    .any(|&k| position[&a][k].abs() < eps && position[&b][k].abs() < eps)
        })
        .map(|((a, b), _)| (position[&a], position[&b]))
        .collect();
    if once.is_empty() {
        return (0, 0);
    }
    let mids: Vec<DVec3> = once.iter().map(|(a, b)| (*a + *b) / 2.).collect();
    let half: Vec<f64> = once.iter().map(|(a, b)| 0.5 * a.distance(*b)).collect();
    let half_max = half.iter().copied().fold(0., f64::max);
    let tree = KdTree::new(mids.clone());
    let mut holes = 0;
    for (e, m) in mids.iter().enumerate() {
        let mut d = f64::INFINITY;
        for i in tree.within(*m, half_max + half[e] + tol) {
            if i == e {
                continue;
            }
            let (a, b) = once[i];
            let ab = b - a;
            let w = ((*m - a).dot(ab) / ab.dot(ab).max(1e-300)).clamp(0., 1.);
            d = d.min((a + w * ab).distance(*m));
        }
        if d > 0.05 * half[e] + tol {
            holes += 1;
        }
    }
    (holes, once.len() - holes)
}

/// Convert a CAD file to structured panels: the spline patches of the body gridded on their parameters.
pub fn convert(spec: &Spec, progress: Progress) -> Result<Conversion> {
    ensure!(
        spec.mode == Mode::LoStr,
        "Structured panels are the lo-str mode"
    );
    let started = Instant::now();
    let panel_size = spec
        .settings
        .panel_size
        .context("lo-str needs a panel size")?;
    let mut patch_spec = spec.clone();
    patch_spec.mode = Mode::Ho;
    patch_spec.settings.panel_size = None;
    patch_spec.settings.potential_panel_size = None;
    let patches = ho::convert(&patch_spec, progress)?;
    let size = patches.summary["body_size_m"]
        .as_f64()
        .context("Missing body size")?;
    let tol = 1e-5 * size;
    let symmetry = spec.settings.symmetry;
    let estimate: f64 = patches
        .body
        .patches
        .iter()
        .map(|p| checks::rough_area(p, 9))
        .sum::<f64>()
        / (panel_size * panel_size);
    progress(
        "mesh",
        json!({"patches": patches.body.patches.len(), "panel_size": panel_size, "panels_estimated": estimate.round() as i64}),
    );
    let normalized: Vec<Patch> = patches
        .body
        .patches
        .iter()
        .map(bspline::normalized)
        .collect();
    let (cut, faces, t_left) =
        remove_t_junctions(normalized, patches.faces.clone(), tol, T_JUNCTION_ROUNDS);
    let label = patches.body.label.clone();
    let (body, splits, n, broken) = panels_of(&cut, &faces, panel_size, tol, symmetry, label);
    ensure!(
        !body.vertices.is_empty(),
        "{}: no panels: every panel collapsed",
        spec.source.display()
    );
    let (holes, hanging) = free_edges(&body, size, symmetry, tol);
    progress("checks", json!({"panels": body.vertices.len()}));
    let volume_cad = patches.checks["volume"]["volume_cad_m3"].as_f64();
    let z_max = body
        .vertices
        .iter()
        .flatten()
        .map(|p| p[2])
        .fold(f64::NEG_INFINITY, f64::max);
    let mut checks = body_checks(
        body.volumes()?,
        volume_cad,
        holes,
        z_max,
        patches.checks["orientation"].clone(),
        size,
        spec.settings.cut_waterline,
    );
    checks["closed"]["hanging_edges"] = hanging.into();
    let folded: Vec<&str> = patches
        .patch_checks
        .iter()
        .filter(|p| p["checks"]["fold"] == "fail")
        .filter_map(|p| p["name"].as_str())
        .collect();
    if !folded.is_empty() {
        checks["patch_source"] = json!({"state": "fail", "folded_patches": folded});
    }
    let mut distinct = faces.clone();
    distinct.sort_unstable();
    distinct.dedup();
    let mut summary = lofree::summary(&body, splits, distinct.len(), panel_size)?;
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
    let mut cad = patches.cad.clone();
    cad["structured"] = json!({"patches": patches.body.patches.len(), "patches_after_t_junctions": cut.len(),
        "t_junctions_left": t_left, "edges_not_conforming": broken, "counts": n});
    let interior = match &patches.interior {
        Some(lids) => {
            let normalized: Vec<Patch> = lids.patches.iter().map(bspline::normalized).collect();
            let owners: Vec<i32> = (1..=normalized.len() as i32).collect();
            let (cut, owners, _) = remove_t_junctions(normalized, owners, tol, T_JUNCTION_ROUNDS);
            let (mut panels, _, _, _) =
                panels_of(&cut, &owners, panel_size, tol, symmetry, lids.label.clone());
            for p in panels.vertices.iter_mut() {
                if normal(p).z < 0. {
                    *p = reversed(p); // up
                }
            }
            let area_cad = patches.summary["interior"]["area_cad_m2"]
                .as_f64()
                .unwrap_or(0.);
            let entry = interior_entry(&panels, area_cad, size, symmetry.images());
            summary["state"] =
                state([body_state, entry["state"].as_str().unwrap_or("fail")]).into();
            summary["interior"] = entry;
            Some(panels)
        }
        None => None,
    };
    Ok(Conversion {
        body,
        interior,
        summary,
        checks,
        cad,
    })
}

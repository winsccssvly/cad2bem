// SPDX-License-Identifier: GPL-3.0-or-later
//! Geometric checks of a set of B-spline patches: Jacobian and folds, corner angles, gaps between patches,
//! overlapping patches, volumes and areas.

use super::{
    bspline::{Patch, unit_grid},
    kdtree::KdTree,
};
use crate::conversion::Symmetry;
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

/// The unit normal x_u x x_v at the centre of the parameters.
pub fn center_normal(patch: &Patch) -> (DVec3, DVec3) {
    let [u0, u1, v0, v1] = patch.range();
    let (s, t) = ((u0 + u1) / 2., (v0 + v1) / 2.);
    (patch.at(s, t), patch.normal(s, t))
}

/// The largest distance between two corners of the control net.
pub fn diameter(patch: &Patch) -> f64 {
    let (mu, mv) = (patch.mu(), patch.mv());
    let corners = [
        patch.coefficient(0, 0),
        patch.coefficient(0, mu - 1),
        patch.coefficient(mv - 1, mu - 1),
        patch.coefficient(mv - 1, 0),
    ];
    let mut out: f64 = 0.;
    for a in corners {
        for b in corners {
            out = out.max(a.distance(b));
        }
    }
    out
}

/// A reversed Jacobian counts as a fold above this fraction of the patch's mean Jacobian.
pub const FOLD_TOL: f64 = 1e-3;

/// The Jacobian on an n x n grid: min |x_u x x_v| / mean after removing the linear zero of collapsed edges
/// (poles), and the number of grid points whose normal turns against the normal at the centre by more than
/// [`FOLD_TOL`] of the mean Jacobian (a corner a hair past 180 degrees turns a vanishing Jacobian over at that
/// one point: no fold, but a small Jacobian).
pub fn jacobian(patch: &Patch, n: usize) -> (f64, usize) {
    let s = unit_grid(n);
    let mut x = Vec::with_capacity(n * n);
    let mut jac = Vec::with_capacity(n * n);
    for &t in &s {
        for &u in &s {
            let (p, xu, xv) = patch.frame(u, t);
            x.push(p);
            jac.push(xu.cross(xv));
        }
    }
    let lo = x.iter().fold(DVec3::INFINITY, |a, &p| a.min(p));
    let hi = x.iter().fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
    let tol = 1e-9 * (hi - lo).length();
    let collapsed = |points: Vec<DVec3>| points.iter().all(|p| p.distance(points[0]) <= tol);
    let t0 = collapsed((0..n).map(|i| x[i]).collect());
    let t1 = collapsed((0..n).map(|i| x[(n - 1) * n + i]).collect());
    let s0 = collapsed((0..n).map(|j| x[j * n]).collect());
    let s1 = collapsed((0..n).map(|j| x[j * n + n - 1]).collect());
    let rows = usize::from(t0)..(if t1 { n - 1 } else { n });
    let cols = usize::from(s0)..(if s1 { n - 1 } else { n });
    let (_, centre) = center_normal(patch);
    let mean_length = jac.iter().map(|j| j.length()).sum::<f64>() / jac.len().max(1) as f64;
    let (mut minimum, mut sum, mut count, mut folds) = (f64::INFINITY, 0., 0, 0);
    for j in rows {
        for i in cols.clone() {
            let mut factor = 1.;
            if t0 {
                factor *= s[j];
            }
            if t1 {
                factor *= 1. - s[j];
            }
            if s0 {
                factor *= s[i];
            }
            if s1 {
                factor *= 1. - s[i];
            }
            let value = jac[j * n + i].length() / factor;
            minimum = minimum.min(value);
            sum += value;
            count += 1;
            if jac[j * n + i].dot(centre) <= -FOLD_TOL * mean_length {
                folds += 1;
            }
        }
    }
    (minimum / (sum / count as f64), folds)
}

/// The interior angles (degrees) at the corners (0, 0), (1, 0), (1, 1), (0, 1); at a collapsed edge the
/// angle between the two edges meeting there.
pub fn corner_angles(patch: &Patch) -> [f64; 4] {
    let angle = |a: DVec3, b: DVec3| {
        let d = a.length() * b.length();
        let c = if d > 0. { a.dot(b) / d } else { 1. };
        c.clamp(-1., 1.).acos().to_degrees()
    };
    let mut out = [0.; 4];
    for (k, (s, t, su, sv)) in [
        (0., 0., 1., 1.),
        (1., 0., -1., 1.),
        (1., 1., -1., -1.),
        (0., 1., 1., -1.),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, xu, xv) = patch.frame(s, t);
        out[k] = angle(xu * su, xv * sv);
    }
    let (mu, mv) = (patch.mu(), patch.mv());
    let corners = [
        patch.coefficient(0, 0),
        patch.coefficient(0, mu - 1),
        patch.coefficient(mv - 1, mu - 1),
        patch.coefficient(mv - 1, 0),
    ];
    let lo = patch
        .coefficients
        .iter()
        .fold(DVec3::INFINITY, |a, &p| a.min(p));
    let hi = patch
        .coefficients
        .iter()
        .fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
    let scale = (hi - lo).length().max(1e-300);
    let g = unit_grid(5);
    for (pair, along_t, fixed) in [
        ([0, 1], true, 0.),
        ([1, 2], false, 1.),
        ([3, 2], true, 1.),
        ([0, 3], false, 0.),
    ] {
        if corners[pair[0]].distance(corners[pair[1]]) > 1e-9 * scale {
            continue;
        }
        let points: Vec<DVec3> = g
            .iter()
            .map(|&x| {
                if along_t {
                    patch.at(x, fixed)
                } else {
                    patch.at(fixed, x)
                }
            })
            .collect();
        if points.iter().any(|p| p.distance(points[0]) > 1e-9 * scale) {
            continue;
        }
        let (a, b) = if along_t {
            (patch.frame(0., fixed).2, patch.frame(1., fixed).2)
        } else {
            (patch.frame(fixed, 0.).1, patch.frame(fixed, 1.).1)
        };
        let d = a.length() * b.length();
        let value = if d > 0. {
            (a.dot(b) / d).clamp(-1., 1.).acos().to_degrees()
        } else {
            0.
        };
        out[pair[0]] = value;
        out[pair[1]] = value;
    }
    out
}

/// Points along the four edges (bottom, right, top, left), n each.
pub fn boundary_points(patch: &Patch, n: usize) -> Vec<DVec3> {
    let u = unit_grid(n);
    let mut out = Vec::with_capacity(4 * n);
    out.extend(u.iter().map(|&x| patch.at(x, 0.)));
    out.extend(u.iter().map(|&x| patch.at(1., x)));
    out.extend(u.iter().map(|&x| patch.at(x, 1.)));
    out.extend(u.iter().map(|&x| patch.at(0., x)));
    out
}

/// Whether points lie on z = 0 or a symmetry plane (need no neighbour).
pub fn on_planes(point: DVec3, scale: f64, symmetry: Symmetry) -> bool {
    let tol = 1e-6 * scale;
    point.z.abs() < tol || symmetry.axes().iter().any(|&k| point[k].abs() < tol)
}

/// The control points, weights and normalized knots of an edge (0 bottom, 1 right, 2 top, 3 left).
fn edge_curve(p: &Patch, edge: usize) -> (Vec<DVec3>, Vec<f64>, Vec<f64>, usize) {
    let (mu, mv) = (p.mu(), p.mv());
    let index: Vec<usize> = match edge {
        0 => (0..mu).collect(),
        1 => (0..mv).map(|j| j * mu + mu - 1).collect(),
        2 => (0..mu).map(|i| (mv - 1) * mu + i).collect(),
        _ => (0..mv).map(|j| j * mu).collect(),
    };
    let c = index.iter().map(|&k| p.coefficients[k]).collect();
    let w = index
        .iter()
        .map(|&k| p.weights.as_ref().map_or(1., |w| w[k]))
        .collect();
    let (knots, order) = if edge.is_multiple_of(2) {
        (&p.u_knots, p.u_order)
    } else {
        (&p.v_knots, p.v_order)
    };
    let (a, b) = (knots[0], knots[knots.len() - 1]);
    (
        c,
        w,
        knots.iter().map(|&x| (x - a) / (b - a)).collect(),
        order,
    )
}

/// Edges whose complete boundary curves coincide (control points, normalized weights and knots).
pub fn matching_edges(patches: &[Patch], scale: f64) -> Vec<[bool; 4]> {
    let mut matched = vec![[false; 4]; patches.len()];
    let quantum = (1e-10 * scale).max(1e-300);
    let eps = (512. * f64::EPSILON * scale).max(1e-300);
    let key_of = |x: DVec3| ((x / quantum).round().as_i64vec3()).to_array();
    #[allow(clippy::type_complexity)]
    let mut buckets: HashMap<
        (usize, usize, [i64; 3], [i64; 3]),
        Vec<(usize, usize, Vec<DVec3>, Vec<f64>, Vec<f64>)>,
    > = HashMap::new();
    for (i, p) in patches.iter().enumerate() {
        for edge in 0..4 {
            let (mut c, mut w, mut knots, order) = edge_curve(p, edge);
            if c[0].distance(c[c.len() - 1]) <= eps {
                continue;
            }
            let (start, end) = (key_of(c[0]), key_of(c[c.len() - 1]));
            if start == end {
                continue;
            }
            if start > end {
                c.reverse();
                w.reverse();
                knots = knots.iter().rev().map(|&x| 1. - x).collect();
            }
            let w0 = w[0];
            let w: Vec<f64> = w.iter().map(|&x| x / w0).collect();
            let key = (order, c.len(), key_of(c[0]), key_of(c[c.len() - 1]));
            let entries = buckets.entry(key).or_default();
            for (j, k, oc, ow, ok) in entries.iter() {
                let same = c
                    .iter()
                    .zip(oc)
                    .all(|(a, b)| (*a - *b).abs().max_element() <= eps)
                    && w.iter()
                        .zip(ow)
                        .all(|(a, b)| (a - b).abs() <= 1e-13 + 1e-12 * b.abs())
                    && knots.iter().zip(ok).all(|(a, b)| (a - b).abs() <= 1e-13);
                if same {
                    matched[i][edge] = true;
                    matched[*j][*k] = true;
                }
            }
            entries.push((i, edge, c, w, knots));
        }
    }
    matched
}

/// Map `f` over 0..n on all CPU cores, keeping the order.
pub fn par_map<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |c| c.get())
        .min(n.max(1));
    if threads <= 1 {
        return (0..n).map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<(usize, T)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        out.push((i, f(i)));
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("worker thread panicked"))
            .collect()
    });
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, v)| v).collect()
}

/// Samples of every patch on an m x m grid, with search trees binned by patch size: each tree searched with
/// its own sample spacing (one tree below 128 patches).
struct Samples {
    m: usize,
    st: Vec<(f64, f64)>,
    cloud: Vec<DVec3>,
    tree: KdTree,
    /// (tree, its sample indices into the cloud, its largest sample spacing)
    bins: Vec<(KdTree, Vec<usize>, f64)>,
}

impl Samples {
    fn new(patches: &[Patch], m: usize) -> Self {
        let g = unit_grid(m);
        let st: Vec<(f64, f64)> = g
            .iter()
            .flat_map(|&t| g.iter().map(move |&s| (s, t)))
            .collect();
        let mut cloud = Vec::with_capacity(patches.len() * m * m);
        for p in patches {
            cloud.extend(p.grid(&g, &g));
        }
        let sizes: Vec<f64> = (0..patches.len())
            .map(|i| {
                let points = &cloud[i * m * m..(i + 1) * m * m];
                let lo = points.iter().fold(DVec3::INFINITY, |a, &p| a.min(p));
                let hi = points.iter().fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
                (hi - lo).length()
            })
            .collect();
        let largest = sizes.iter().copied().fold(0., f64::max);
        let tree = KdTree::new(cloud.clone());
        let mut bins = Vec::new();
        if patches.len() < 128 {
            bins.push((
                KdTree::new(cloud.clone()),
                (0..cloud.len()).collect(),
                largest / (m - 1) as f64,
            ));
        } else {
            let mut by_bin: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
            for (i, &s) in sizes.iter().enumerate() {
                let key = (s.max(largest * 1e-12) / largest).log2().floor() as i64;
                by_bin.entry(key).or_default().push(i);
            }
            for ids in by_bin.values() {
                let indices: Vec<usize> = ids
                    .iter()
                    .flat_map(|&i| i * m * m..(i + 1) * m * m)
                    .collect();
                let spacing = ids.iter().map(|&i| sizes[i]).fold(0., f64::max) / (m - 1) as f64;
                bins.push((
                    KdTree::new(indices.iter().map(|&k| cloud[k]).collect()),
                    indices,
                    spacing,
                ));
            }
        }
        Self {
            m,
            st,
            cloud,
            tree,
            bins,
        }
    }

    fn owner(&self, sample: usize) -> usize {
        sample / (self.m * self.m)
    }

    fn start(&self, sample: usize) -> (f64, f64) {
        self.st[sample % (self.m * self.m)]
    }

    /// Per other patch the nearest of its samples within `extra(spacing)` of x (each bin with its spacing).
    fn nearest_per_patch(
        &self,
        x: DVec3,
        radius: impl Fn(f64) -> f64,
        skip: impl Fn(usize) -> bool,
    ) -> BTreeMap<usize, (f64, usize)> {
        let mut nearest: BTreeMap<usize, (f64, usize)> = BTreeMap::new();
        for (tree, indices, spacing) in &self.bins {
            for local in tree.within(x, radius(*spacing)) {
                let sample = indices[local];
                let other = self.owner(sample);
                if skip(other) {
                    continue;
                }
                let d = self.cloud[sample].distance(x);
                let entry = nearest.entry(other).or_insert((d, sample));
                if d < entry.0 {
                    *entry = (d, sample);
                }
            }
        }
        nearest
    }
}

/// Per patch the largest distance of its edges to the other patches (only the patches in `only`, the others
/// 0). Edge points for which `free` holds (on z = 0 or a symmetry plane, see [`on_planes`]) need no neighbour;
/// a collapsed edge (a pole) neither; an edge on the patch's own opposite edge (the seam of a closed patch)
/// has it. A point near no patch gets the body size.
pub fn gaps(
    patches: &[Patch],
    free: &(dyn Fn(DVec3) -> bool + Sync),
    only: Option<&[usize]>,
) -> Vec<f64> {
    let (n, candidates) = (33, 3);
    let samples = Samples::new(patches, 25);
    let lo = samples.cloud.iter().fold(DVec3::INFINITY, |a, &p| a.min(p));
    let hi = samples
        .cloud
        .iter()
        .fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
    let size = (hi - lo).length();
    let matched = matching_edges(patches, size);
    par_map(patches.len(), |i| {
        if only.is_some_and(|o| !o.contains(&i)) {
            return 0.;
        }
        let q = boundary_points(&patches[i], n);
        let mut worst: f64 = 0.;
        for edge in 0..4 {
            let points = &q[edge * n..(edge + 1) * n];
            let length: f64 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
            if length <= 1e-9 * size || matched[i][edge] {
                continue;
            }
            let opposite = &q[((edge + 2) % 4) * n..((edge + 2) % 4 + 1) * n];
            for &x in points {
                if free(x) {
                    continue;
                }
                let mut best = opposite
                    .iter()
                    .map(|o| o.distance(x))
                    .fold(f64::INFINITY, f64::min);
                let mut tried: Vec<(usize, usize)> = Vec::new();
                for (sample, _) in samples
                    .tree
                    .nearest(x, (8 * samples.m).min(samples.cloud.len()))
                {
                    let other = samples.owner(sample);
                    if other == i || tried.iter().any(|&(o, _)| o == other) {
                        continue;
                    }
                    tried.push((other, sample));
                    if tried.len() == candidates {
                        break;
                    }
                }
                for &(other, sample) in &tried {
                    let (s, t) = samples.start(sample);
                    best = best.min(patches[other].closest(x, s, t, 30).2);
                }
                if best > 1e-9 * size {
                    // Small neighbours can surround the point without reaching it: try every patch with a
                    // sample within that distance plus the sample spacing.
                    let found = samples.nearest_per_patch(
                        x,
                        |spacing| best + spacing,
                        |other| other == i || tried.iter().any(|&(o, _)| o == other),
                    );
                    for (other, (_, sample)) in found {
                        let (s, t) = samples.start(sample);
                        best = best.min(patches[other].closest(x, s, t, 30).2);
                    }
                }
                worst = worst.max(best.min(size));
            }
        }
        worst
    })
}

/// Pairs of patches that cover the same area twice: interior samples of one patch within `tol` of another,
/// projecting inside it with a parallel normal. Returns (record i, record j, fraction covered), 1-based.
/// Patches of one non-negative group never overlap each other by construction and are not compared.
pub fn overlaps(patches: &[Patch], tol: f64, groups: &[i64]) -> Vec<(usize, usize, f64)> {
    let (n, margin, parallel) = (9, 0.05, 0.97);
    let samples = Samples::new(patches, 25);
    let u = super::prepare::linspace(margin, 1. - margin, n);
    let interior: Vec<(f64, f64)> = u
        .iter()
        .flat_map(|&v| u.iter().map(move |&s| (s, v)))
        .collect();
    let rows = par_map(patches.len(), |i| {
        let p = &patches[i];
        let same_group =
            |j: usize| groups.get(i).is_some_and(|&g| g >= 0) && groups.get(j) == groups.get(i);
        let mut hits: BTreeMap<usize, usize> = BTreeMap::new();
        let mut order: Vec<usize> = Vec::new();
        for &(s, t) in &interior {
            let (x, xu, xv) = p.frame(s, t);
            let ni = xu.cross(xv).normalize_or_zero();
            let found =
                samples.nearest_per_patch(x, |step| tol + 1.5 * step, |j| j == i || same_group(j));
            for (j, (_, sample)) in found {
                if !order.contains(&j) {
                    order.push(j);
                }
                let (s0, t0) = samples.start(sample);
                let (sj, tj, d) = patches[j].closest(x, s0, t0, 30);
                let inner = sj > 1e-6 && sj < 1. - 1e-6 && tj > 1e-6 && tj < 1. - 1e-6;
                if d < tol && inner && ni.dot(patches[j].normal(sj, tj)).abs() > parallel {
                    *hits.entry(j).or_default() += 1;
                }
            }
        }
        order
            .into_iter()
            .filter_map(|j| {
                hits.get(&j).map(|&h| {
                    (
                        i + 1,
                        j + 1,
                        (h as f64 / interior.len() as f64 * 1000.).round() / 1000.,
                    )
                })
            })
            .collect::<Vec<_>>()
    });
    rows.into_iter().flatten().collect()
}

/// Gauss-Legendre nodes and weights on [0, 1].
pub fn gauss_legendre(n: usize) -> (Vec<f64>, Vec<f64>) {
    let (mut x, mut w) = (vec![0.; n], vec![0.; n]);
    for i in 0..n {
        let mut z = (std::f64::consts::PI * (i as f64 + 0.75) / (n as f64 + 0.5)).cos();
        let mut derivative = 0.;
        for _ in 0..100 {
            let (mut p0, mut p1) = (1., z);
            for k in 2..=n {
                let p2 = ((2 * k - 1) as f64 * z * p1 - (k - 1) as f64 * p0) / k as f64;
                p0 = p1;
                p1 = p2;
            }
            let pn = if n == 1 { z } else { p1 };
            let pn1 = if n == 1 { 1. } else { p0 };
            derivative = n as f64 * (z * pn - pn1) / (z * z - 1.);
            let dz = pn / derivative;
            z -= dz;
            if dz.abs() < 1e-16 {
                break;
            }
        }
        x[i] = (1. - z) / 2.;
        w[i] = 1. / ((1. - z * z) * derivative * derivative);
    }
    (x, w)
}

fn spans(knots: &[f64]) -> Vec<(f64, f64)> {
    let mut unique: Vec<f64> = knots.to_vec();
    unique.dedup();
    unique.windows(2).map(|w| (w[0], w[1])).collect()
}

/// The volume below the waterline, three ways (V_x, V_y, V_z, normals x_u x x_v into the body), by
/// Gauss-Legendre on every knot span, with the images of the symmetry planes.
pub fn volumes(patches: &[Patch], symmetry: Symmetry) -> [f64; 3] {
    let mut out = DVec3::ZERO;
    for p in patches {
        let n = if p.weights.is_some() {
            12
        } else {
            12.min((3 * (p.u_order.max(p.v_order) - 1)).div_ceil(2))
        };
        let (g, w) = gauss_legendre(n);
        for (ua, ub) in spans(&p.u_knots) {
            for (va, vb) in spans(&p.v_knots) {
                for (gi, wi) in g.iter().zip(&w) {
                    for (gj, wj) in g.iter().zip(&w) {
                        let (s, t) = (ua + (ub - ua) * gi, va + (vb - va) * gj);
                        let (x, xu, xv) = p.frame(s, t);
                        out -= x * xu.cross(xv) * (wi * (ub - ua) * wj * (vb - va));
                    }
                }
            }
        }
    }
    (out * symmetry.images()).to_array()
}

/// The area of a patch, by Gauss-Legendre on every knot span.
pub fn area(patch: &Patch) -> f64 {
    let (g, w) = gauss_legendre(12);
    let mut out = 0.;
    for (ua, ub) in spans(&patch.u_knots) {
        for (va, vb) in spans(&patch.v_knots) {
            for (gi, wi) in g.iter().zip(&w) {
                for (gj, wj) in g.iter().zip(&w) {
                    let (_, xu, xv) = patch.frame(ua + (ub - ua) * gi, va + (vb - va) * gj);
                    out += xu.cross(xv).length() * wi * (ub - ua) * wj * (vb - va);
                }
            }
        }
    }
    out
}

/// The area of a patch from the flat quads of an n x n grid (for estimates).
pub fn rough_area(patch: &Patch, n: usize) -> f64 {
    let g = unit_grid(n);
    let x = patch.grid(&g, &g);
    let mut out = 0.;
    for j in 0..n - 1 {
        for i in 0..n - 1 {
            let a = (x[(j + 1) * n + i + 1] - x[j * n + i])
                .cross(x[(j + 1) * n + i] - x[j * n + i + 1]);
            out += 0.5 * a.length();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::exact::{Conic, ruled};

    #[test]
    fn gauss_legendre_integrates_polynomials() {
        let (x, w) = gauss_legendre(5);
        let integral: f64 = x.iter().zip(&w).map(|(x, w)| w * x.powi(9)).sum();
        assert!((integral - 0.1).abs() < 1e-14);
    }

    /// A closed unit cube of six bilinear patches (normals into the body): three equal volumes.
    #[test]
    fn cube_volumes_jacobian_and_angles() {
        let face = |a: DVec3, b: DVec3, c: DVec3, d: DVec3| {
            ruled(
                &Conic {
                    points: [a, (a + b) / 2., b],
                    weights: [1.; 3],
                },
                &Conic {
                    points: [d, (d + c) / 2., c],
                    weights: [1.; 3],
                },
            )
        };
        let v = |x: f64, y: f64, z: f64| DVec3::new(x, y, z - 1.);
        // each face: bottom edge a->b, top edge d->c; x_u x x_v into the cube
        let patches = vec![
            face(v(0., 0., 0.), v(1., 0., 0.), v(1., 1., 0.), v(0., 1., 0.)),
            face(v(0., 0., 1.), v(0., 1., 1.), v(1., 1., 1.), v(1., 0., 1.)),
            face(v(0., 0., 0.), v(0., 0., 1.), v(1., 0., 1.), v(1., 0., 0.)),
            face(v(0., 1., 0.), v(1., 1., 0.), v(1., 1., 1.), v(0., 1., 1.)),
            face(v(0., 0., 0.), v(0., 1., 0.), v(0., 1., 1.), v(0., 0., 1.)),
            face(v(1., 0., 0.), v(1., 0., 1.), v(1., 1., 1.), v(1., 1., 0.)),
        ];
        let vols = volumes(&patches, Symmetry::None);
        for x in vols {
            assert!((x - 1.).abs() < 1e-12, "{vols:?}");
        }
        let (jmin, folds) = jacobian(&patches[0], 21);
        assert!((jmin - 1.).abs() < 1e-12 && folds == 0);
        assert!(
            corner_angles(&patches[0])
                .iter()
                .all(|a| (a - 90.).abs() < 1e-9)
        );
        assert!((area(&patches[0]) - 1.).abs() < 1e-12);
        let gaps = gaps(&patches, &|x| on_planes(x, 1., Symmetry::None), None);
        assert!(gaps.iter().all(|&g| g < 1e-9), "{gaps:?}");
        assert!(overlaps(&patches, 1e-4, &[-1; 6]).is_empty());
    }
}

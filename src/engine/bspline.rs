// SPDX-License-Identifier: GPL-3.0-or-later
//! Clamped tensor-product B-spline patches, polynomial or rational (NURBS), on their knot ranges: evaluation,
//! first derivatives, closest points, least-squares fits, knot insertion, restriction and C0 splits.
//!
//! Coefficients are stored row by row along v, the u index fastest (`coefficients[j * mu + i]`), with
//! `mu = u_knots.len() - u_order` and `mv = v_knots.len() - v_order`, as geometry files hold them.

use anyhow::{Result, ensure};
use glam::DVec3;
use nalgebra::DMatrix;

/// A tensor-product B-spline patch x(s, t), polynomial or rational.
#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    pub u_order: usize,
    pub v_order: usize,
    pub u_knots: Vec<f64>,
    pub v_knots: Vec<f64>,
    /// Control points, `mv` rows of `mu`, u fastest.
    pub coefficients: Vec<DVec3>,
    /// Weights of a rational patch, laid out as the coefficients; None for a polynomial patch.
    pub weights: Option<Vec<f64>>,
}

/// The complete clamped knot vector of uniform spans on [0, 1].
pub fn uniform_knots(spans: usize, order: usize) -> Vec<f64> {
    let mut out = vec![0.; order];
    out.extend((1..spans).map(|k| k as f64 / spans as f64));
    out.extend(std::iter::repeat_n(1., order));
    out
}

/// The knot span index i with knots[i] <= s < knots[i + 1] (the last non-empty span at the right end).
fn span(knots: &[f64], order: usize, s: f64) -> usize {
    let m = knots.len() - order; // number of basis functions
    let last = m - 1;
    if s >= knots[m] {
        // the right end: the last non-empty span
        let mut i = last;
        while i > order - 1 && knots[i] >= knots[i + 1] {
            i -= 1;
        }
        return i;
    }
    let (mut lo, mut hi) = (order - 1, m);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if s < knots[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo
}

/// The largest supported order (degree + 1); the basis is evaluated on the stack.
pub const MAX_ORDER: usize = 16;

/// Non-zero basis values and derivatives: the first index and `order` entries of each array.
pub type Nonzero = (usize, [f64; MAX_ORDER], [f64; MAX_ORDER]);

/// The non-zero basis functions of `order` and their first derivatives at s (clamped to the knot range).
pub fn basis_nonzero(knots: &[f64], order: usize, s: f64) -> Nonzero {
    assert!(order <= MAX_ORDER, "B-spline order above {MAX_ORDER}");
    let p = order - 1;
    let s = s.clamp(knots[0], knots[knots.len() - 1]);
    let i = span(knots, order, s);
    // Piegl & Tiller A2.3 for n = 1 derivative.
    let mut ndu = [[0.; MAX_ORDER]; MAX_ORDER];
    ndu[0][0] = 1.;
    let (mut left, mut right) = ([0.; MAX_ORDER], [0.; MAX_ORDER]);
    for j in 1..=p {
        left[j] = s - knots[i + 1 - j];
        right[j] = knots[i + j] - s;
        let mut saved = 0.;
        for r in 0..j {
            ndu[j][r] = right[r + 1] + left[j - r];
            let temp = if ndu[j][r] != 0. {
                ndu[r][j - 1] / ndu[j][r]
            } else {
                0.
            };
            ndu[r][j] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        ndu[j][j] = saved;
    }
    let mut values = [0.; MAX_ORDER];
    for (j, v) in values.iter_mut().take(order).enumerate() {
        *v = ndu[j][p];
    }
    let mut derivatives = [0.; MAX_ORDER];
    if p >= 1 {
        for (r, d) in derivatives.iter_mut().take(order).enumerate() {
            // N'_{i-p+r,p} = p (N_{i-p+r,p-1}/(t_{i+r} - t_{i-p+r}) - N_{i-p+r+1,p-1}/(t_{i+r+1} - t_{i-p+r+1}))
            let mut value = 0.;
            if r >= 1 {
                let denominator = ndu[p][r - 1];
                if denominator != 0. {
                    value += ndu[r - 1][p - 1] / denominator;
                }
            }
            if r < p {
                let denominator = ndu[p][r];
                if denominator != 0. {
                    value -= ndu[r][p - 1] / denominator;
                }
            }
            *d = p as f64 * value;
        }
    }
    (i - p, values, derivatives)
}

/// All basis functions (or their first derivatives) at s, as a dense row of length M.
pub fn basis(knots: &[f64], order: usize, s: f64, derivative: bool) -> Vec<f64> {
    let (first, values, derivatives) = basis_nonzero(knots, order, s);
    let mut row = vec![0.; knots.len() - order];
    for (k, (v, d)) in values[..order]
        .iter()
        .zip(&derivatives[..order])
        .enumerate()
    {
        row[first + k] = if derivative { *d } else { *v };
    }
    row
}

/// Evenly spaced values from 0 to 1, as NumPy's `linspace(0, 1, n)`.
pub fn unit_grid(n: usize) -> Vec<f64> {
    super::prepare::linspace(0., 1., n)
}

/// Least-squares solution of A x = B (SVD, singular values below `max(m, n) eps s_max` dropped, like
/// `numpy.linalg.lstsq` with `rcond=None`).
fn least_squares(a: DMatrix<f64>, b: DMatrix<f64>) -> Result<DMatrix<f64>> {
    let (m, n) = a.shape();
    let svd = a.svd(true, true);
    let largest = svd.singular_values.iter().copied().fold(0., f64::max);
    let eps = f64::EPSILON * m.max(n) as f64 * largest;
    svd.solve(&b, eps)
        .map_err(|e| anyhow::anyhow!("Least-squares solve failed: {e}"))
}

/// The coefficients of the clamped B-spline on uniform spans that interpolates the first and last point and
/// fits the others by least squares at the parameters s in [0, 1].
pub fn fit_curve(s: &[f64], points: &[DVec3], spans: usize, order: usize) -> Result<Vec<DVec3>> {
    ensure!(
        s.len() == points.len() && points.len() >= 2,
        "Curve fit needs matching samples"
    );
    let knots = uniform_knots(spans, order);
    let m = knots.len() - order;
    let rows: Vec<Vec<f64>> = s.iter().map(|&x| basis(&knots, order, x, false)).collect();
    let mut c = vec![DVec3::ZERO; m];
    c[0] = points[0];
    c[m - 1] = points[points.len() - 1];
    let inner = s.len() - 2;
    let a = DMatrix::from_fn(inner, m - 2, |r, k| rows[r + 1][k + 1]);
    let b = DMatrix::from_fn(inner, 3, |r, d| {
        let row = &rows[r + 1];
        (points[r + 1] - row[0] * c[0] - row[m - 1] * c[m - 1])[d]
    });
    let x = least_squares(a, b)?;
    for k in 0..m - 2 {
        c[k + 1] = DVec3::new(x[(k, 0)], x[(k, 1)], x[(k, 2)]);
    }
    Ok(c)
}

/// The patch on uniform spans that fits the grid of points (`t.len()` rows of `s.len()`) by least squares;
/// with `boundary` (bottom t = 0, right s = 1, top t = 1, left s = 0 coefficients) the edge coefficients are
/// fixed and only the interior is fitted, so neighbours sharing an edge curve meet without a gap.
pub fn fit_surface(
    s: &[f64],
    t: &[f64],
    points: &[DVec3],
    spans: usize,
    order: usize,
    boundary: Option<[&[DVec3]; 4]>,
) -> Result<Patch> {
    let knots = uniform_knots(spans, order);
    let m = knots.len() - order;
    ensure!(
        points.len() == s.len() * t.len(),
        "Surface fit grid size mismatch"
    );
    let bu: Vec<Vec<f64>> = s.iter().map(|&x| basis(&knots, order, x, false)).collect();
    let bv: Vec<Vec<f64>> = t.iter().map(|&x| basis(&knots, order, x, false)).collect();
    let mut c = vec![DVec3::ZERO; m * m];
    let mut free = vec![true; m * m];
    if let Some([bottom, right, top, left]) = boundary {
        ensure!(
            [bottom, right, top, left].iter().all(|e| e.len() == m),
            "Boundary coefficient count mismatch"
        );
        for k in 0..m {
            c[k] = bottom[k];
            c[(m - 1) * m + k] = top[k];
            c[k * m] = left[k];
            c[k * m + m - 1] = right[k];
            for index in [k, (m - 1) * m + k, k * m, k * m + m - 1] {
                free[index] = false;
            }
        }
    }
    let unknowns: Vec<usize> = (0..m * m).filter(|&k| free[k]).collect();
    let fixed: Vec<usize> = (0..m * m).filter(|&k| !free[k]).collect();
    let rows = points.len();
    let coefficient = |row: usize, index: usize| {
        let (j, i) = (row / s.len(), row % s.len());
        let (b, a) = (index / m, index % m);
        bv[j][b] * bu[i][a]
    };
    let a = DMatrix::from_fn(rows, unknowns.len(), |r, k| coefficient(r, unknowns[k]));
    let b = DMatrix::from_fn(rows, 3, |r, d| {
        let mut value = points[r][d];
        for &index in &fixed {
            value -= coefficient(r, index) * c[index][d];
        }
        value
    });
    let x = least_squares(a, b)?;
    for (k, &index) in unknowns.iter().enumerate() {
        c[index] = DVec3::new(x[(k, 0)], x[(k, 1)], x[(k, 2)]);
    }
    Ok(Patch {
        u_order: order,
        v_order: order,
        u_knots: knots.clone(),
        v_knots: knots,
        coefficients: c,
        weights: None,
    })
}

impl Patch {
    /// The number of control points in u.
    pub fn mu(&self) -> usize {
        self.u_knots.len() - self.u_order
    }

    /// The number of control points in v.
    pub fn mv(&self) -> usize {
        self.v_knots.len() - self.v_order
    }

    /// The control point in row j (v) and column i (u).
    pub fn coefficient(&self, j: usize, i: usize) -> DVec3 {
        self.coefficients[j * self.mu() + i]
    }

    /// Whether the weights differ (equal weights describe a polynomial patch).
    pub fn rational(&self) -> bool {
        self.weights
            .as_ref()
            .is_some_and(|w| w.iter().any(|&x| (x - w[0]).abs() > 1e-14 * w[0].abs()))
    }

    /// The point and both first partial derivatives at (s, t).
    pub fn frame(&self, s: f64, t: f64) -> (DVec3, DVec3, DVec3) {
        let (fu, nu, du) = basis_nonzero(&self.u_knots, self.u_order, s);
        let (fv, nv, dv) = basis_nonzero(&self.v_knots, self.v_order, t);
        let mu = self.mu();
        let (mut x, mut xu, mut xv) = (DVec3::ZERO, DVec3::ZERO, DVec3::ZERO);
        let (mut w, mut wu, mut wv) = (0., 0., 0.);
        for (b, (&n_v, &d_v)) in nv[..self.v_order]
            .iter()
            .zip(&dv[..self.v_order])
            .enumerate()
        {
            for (a, (&n_u, &d_u)) in nu[..self.u_order]
                .iter()
                .zip(&du[..self.u_order])
                .enumerate()
            {
                let index = (fv + b) * mu + fu + a;
                let weight = self.weights.as_ref().map_or(1., |ws| ws[index]);
                let p = self.coefficients[index] * weight;
                x += p * (n_v * n_u);
                xu += p * (n_v * d_u);
                xv += p * (d_v * n_u);
                w += weight * n_v * n_u;
                wu += weight * n_v * d_u;
                wv += weight * d_v * n_u;
            }
        }
        if self.weights.is_none() {
            return (x, xu, xv);
        }
        let point = x / w;
        (point, (xu - point * wu) / w, (xv - point * wv) / w)
    }

    /// The point at (s, t).
    pub fn at(&self, s: f64, t: f64) -> DVec3 {
        self.frame(s, t).0
    }

    /// The unit normal x_u x x_v at (s, t) (zero where it vanishes).
    pub fn normal(&self, s: f64, t: f64) -> DVec3 {
        let (_, xu, xv) = self.frame(s, t);
        xu.cross(xv).normalize_or_zero()
    }

    /// Points on the tensor grid s x t, `t.len()` rows of `s.len()`.
    pub fn grid(&self, s: &[f64], t: &[f64]) -> Vec<DVec3> {
        t.iter()
            .flat_map(|&tv| s.iter().map(move |&sv| self.at(sv, tv)))
            .collect()
    }

    /// The parameter range of the knots (u0, u1, v0, v1).
    pub fn range(&self) -> [f64; 4] {
        [
            self.u_knots[0],
            self.u_knots[self.u_knots.len() - 1],
            self.v_knots[0],
            self.v_knots[self.v_knots.len() - 1],
        ]
    }

    /// The same surface with u and v exchanged; its normal x_u x x_v points the other way.
    pub fn swapped(&self) -> Patch {
        let (mu, mv) = (self.mu(), self.mv());
        let transpose = |values: &Vec<DVec3>| -> Vec<DVec3> {
            (0..mu)
                .flat_map(|i| (0..mv).map(move |j| values[j * mu + i]))
                .collect()
        };
        Patch {
            u_order: self.v_order,
            v_order: self.u_order,
            u_knots: self.v_knots.clone(),
            v_knots: self.u_knots.clone(),
            coefficients: transpose(&self.coefficients),
            weights: self.weights.as_ref().map(|w| {
                (0..mu)
                    .flat_map(|i| (0..mv).map(move |j| w[j * mu + i]))
                    .collect()
            }),
        }
    }

    /// The closest point of the patch to q by projected Gauss-Newton from (s0, t0) in [0, 1]² parameters
    /// of a unit-knot patch: (s, t, distance).
    pub fn closest(&self, q: DVec3, s0: f64, t0: f64, iterations: usize) -> (f64, f64, f64) {
        let [u0, u1, v0, v1] = self.range();
        let (mut s, mut t) = (s0.clamp(u0, u1), t0.clamp(v0, v1));
        for _ in 0..iterations {
            let (x, xu, xv) = self.frame(s, t);
            let r = x - q;
            let (mut a, b, mut c) = (xu.dot(xu), xu.dot(xv), xv.dot(xv));
            let (gu, gv) = (xu.dot(r), xv.dot(r));
            let mut det = a * c - b * b;
            if det <= 1e-12 * a * c {
                let damping = 1e-12 * (a + c).max(1e-300);
                a += damping;
                c += damping;
                det = a * c - b * b;
            }
            if det.abs() <= 1e-300 {
                det = 1e-300;
            }
            let (mut ds, mut dt) = (-(c * gu - b * gv) / det, -(a * gv - b * gu) / det);
            if s + ds < u0 || s + ds > u1 || t + dt < v0 || t + dt > v1 {
                // Solve the linearized problem on the box edges as well; clipping alone can stagnate.
                let score = |su: f64, tv: f64| {
                    gu * su + gv * tv + 0.5 * (a * su * su + 2. * b * su * tv + c * tv * tv)
                };
                let mut steps = vec![((ds).clamp(u0 - s, u1 - s), (dt).clamp(v0 - t, v1 - t))];
                for su in [u0 - s, u1 - s] {
                    steps.push((su, (-(gv + b * su) / c).clamp(v0 - t, v1 - t)));
                }
                for tv in [v0 - t, v1 - t] {
                    steps.push(((-(gu + b * tv) / a).clamp(u0 - s, u1 - s), tv));
                }
                let best = steps
                    .iter()
                    .copied()
                    .min_by(|x, y| score(x.0, x.1).total_cmp(&score(y.0, y.1)))
                    .unwrap_or((0., 0.));
                (ds, dt) = best;
            }
            let (sn, tn) = ((s + ds).clamp(u0, u1), (t + dt).clamp(v0, v1));
            let step = (sn - s).abs().max((tn - t).abs());
            (s, t) = (sn, tn);
            if step < 1e-12 {
                break;
            }
        }
        (s, t, self.at(s, t).distance(q))
    }

    /// Homogeneous control points (w P, w), row-major, for knot operations on rational patches.
    fn homogeneous(&self) -> Vec<[f64; 4]> {
        self.coefficients
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let w = self.weights.as_ref().map_or(1., |ws| ws[k]);
                [p.x * w, p.y * w, p.z * w, w]
            })
            .collect()
    }

    fn with_homogeneous(&self, u_knots: Vec<f64>, v_knots: Vec<f64>, h: Vec<[f64; 4]>) -> Patch {
        let rational = self.weights.is_some();
        Patch {
            u_order: self.u_order,
            v_order: self.v_order,
            u_knots,
            v_knots,
            coefficients: h
                .iter()
                .map(|c| {
                    if rational {
                        DVec3::new(c[0] / c[3], c[1] / c[3], c[2] / c[3])
                    } else {
                        DVec3::new(c[0], c[1], c[2])
                    }
                })
                .collect(),
            weights: rational.then(|| h.iter().map(|c| c[3]).collect()),
        }
    }
}

/// Boehm's insertion of the knot x once into a curve of `order` with control points `c`.
pub fn insert_knot<T: Copy + std::ops::Mul<f64, Output = T> + std::ops::Add<Output = T>>(
    knots: &[f64],
    order: usize,
    c: &[T],
    x: f64,
) -> (Vec<f64>, Vec<T>) {
    let p = order - 1;
    let m = knots.partition_point(|&k| k <= x).saturating_sub(1);
    let m = m.max(p).min(c.len() - 1);
    let mut out = Vec::with_capacity(c.len() + 1);
    for i in 0..=c.len() {
        out.push(if i <= m - p {
            c[i]
        } else if i > m {
            c[i - 1]
        } else {
            let a = (x - knots[i]) / (knots[i + p] - knots[i]);
            c[i - 1] * (1. - a) + c[i] * a
        });
    }
    let mut t = knots.to_vec();
    t.insert(m + 1, x);
    (t, out)
}

#[derive(Clone, Copy)]
struct H([f64; 4]);
impl std::ops::Mul<f64> for H {
    type Output = H;
    fn mul(self, k: f64) -> H {
        H(self.0.map(|v| v * k))
    }
}
impl std::ops::Add for H {
    type Output = H;
    fn add(self, o: H) -> H {
        H([
            self.0[0] + o.0[0],
            self.0[1] + o.0[1],
            self.0[2] + o.0[2],
            self.0[3] + o.0[3],
        ])
    }
}

/// Restrict one clamped curve (knots, control points) to [a, b] by knot insertion.
fn restrict_curve(knots: &[f64], order: usize, c: &[H], a: f64, b: f64) -> (Vec<f64>, Vec<H>) {
    let (mut t, mut c) = (knots.to_vec(), c.to_vec());
    let span = t[t.len() - 1] - t[0];
    let near = |u: f64, x: f64| (u - x).abs() <= 1e-14 * span;
    for x in [a, b] {
        if t[0] < x && x < t[t.len() - 1] {
            while t.iter().filter(|&&u| near(u, x)).count() < order - 1 {
                (t, c) = insert_knot(&t, order, &c, x);
            }
            for u in t.iter_mut() {
                if near(*u, x) {
                    *u = x;
                }
            }
        }
    }
    let first_a = t.iter().position(|&u| u == a).unwrap_or(0);
    let count_a = t.iter().filter(|&&u| u == a).count();
    let first = first_a + count_a - order;
    let inner: Vec<f64> = t.iter().copied().filter(|&u| u > a && u < b).collect();
    let mut sub = vec![a; order];
    sub.extend(&inner);
    sub.extend(std::iter::repeat_n(b, order));
    let count = sub.len() - order;
    (sub, c[first..first + count].to_vec())
}

/// The part of a patch over [u0, u1] x [v0, v1] in its own knot units, exact (rational patches in
/// homogeneous coordinates); the knots stay in those units.
pub fn restrict(patch: &Patch, u0: f64, u1: f64, v0: f64, v1: f64) -> Patch {
    let h: Vec<H> = patch.homogeneous().into_iter().map(H).collect();
    let (mu, mv) = (patch.mu(), patch.mv());
    // along u, row by row
    let mut uk = Vec::new();
    let mut rows = Vec::new();
    for j in 0..mv {
        let (k, row) = restrict_curve(
            &patch.u_knots,
            patch.u_order,
            &h[j * mu..(j + 1) * mu],
            u0,
            u1,
        );
        uk = k;
        rows.push(row);
    }
    let nu = rows[0].len();
    // along v, column by column
    let mut vk = Vec::new();
    let mut columns = Vec::new();
    for i in 0..nu {
        let column: Vec<H> = rows.iter().map(|r| r[i]).collect();
        let (k, col) = restrict_curve(&patch.v_knots, patch.v_order, &column, v0, v1);
        vk = k;
        columns.push(col);
    }
    let nv = columns[0].len();
    let out: Vec<[f64; 4]> = (0..nv)
        .flat_map(|j| columns.iter().map(move |col| col[j].0))
        .collect();
    patch.with_homogeneous(uk, vk, out)
}

/// The same patch with both knot vectors mapped affinely onto [0, 1].
pub fn normalized(patch: &Patch) -> Patch {
    let unit = |t: &[f64]| -> Vec<f64> {
        let (a, b) = (t[0], t[t.len() - 1]);
        t.iter().map(|&x| (x - a) / (b - a)).collect()
    };
    Patch {
        u_knots: unit(&patch.u_knots),
        v_knots: unit(&patch.v_knots),
        ..patch.clone()
    }
}

/// The interior knots where a clamped spline is only C0 (multiplicity >= order - 1).
pub fn c0_knots(knots: &[f64], order: usize) -> Vec<f64> {
    let inner = &knots[order..knots.len() - order];
    let mut out: Vec<f64> = Vec::new();
    let mut k = 0;
    while k < inner.len() {
        let mut n = 1;
        while k + n < inner.len() && inner[k + n] == inner[k] {
            n += 1;
        }
        if n >= order - 1 {
            out.push(inner[k]);
        }
        k += n;
    }
    out
}

/// The patch split at its C0 interior knots into C1 pieces.
pub fn split_c0(patch: &Patch) -> Vec<Patch> {
    let edges = |knots: &[f64], order: usize| {
        let mut v = vec![knots[0]];
        v.extend(c0_knots(knots, order));
        v.push(knots[knots.len() - 1]);
        v
    };
    let us = edges(&patch.u_knots, patch.u_order);
    let vs = edges(&patch.v_knots, patch.v_order);
    if us.len() == 2 && vs.len() == 2 {
        return vec![patch.clone()];
    }
    let mut out = Vec::new();
    for v in vs.windows(2) {
        for u in us.windows(2) {
            out.push(restrict(patch, u[0], u[1], v[0], v[1]));
        }
    }
    out
}

/// The bilinearly blended Coons grid of four boundary polylines: bottom (t = 0) and top (t = 1) with n_s
/// points in s, left (s = 0) and right (s = 1) with n_t points in t. Returns n_t rows of n_s points.
pub fn coons(bottom: &[DVec3], right: &[DVec3], top: &[DVec3], left: &[DVec3]) -> Vec<DVec3> {
    let (ns, nt) = (bottom.len(), left.len());
    let s = unit_grid(ns);
    let t = unit_grid(nt);
    let (p00, p10, p11, p01) = (bottom[0], bottom[ns - 1], top[ns - 1], top[0]);
    let mut out = Vec::with_capacity(ns * nt);
    for j in 0..nt {
        for i in 0..ns {
            let (si, tj) = (s[i], t[j]);
            let ruled = (1. - tj) * bottom[i] + tj * top[i] + (1. - si) * left[j] + si * right[j];
            let corners = (1. - si) * (1. - tj) * p00
                + si * (1. - tj) * p10
                + si * tj * p11
                + (1. - si) * tj * p01;
            out.push(ruled - corners);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: DVec3, b: DVec3, tol: f64) -> bool {
        a.distance(b) <= tol
    }

    /// A bilinear plane patch: evaluation, derivatives and closest points are exact.
    #[test]
    fn plane_fit_reproduces_and_projects() {
        let g = unit_grid(9);
        let grid: Vec<DVec3> = g
            .iter()
            .flat_map(|&t| {
                g.iter()
                    .map(move |&s| DVec3::new(2. * s + t, 3. * t, 1. + s))
            })
            .collect();
        let p = fit_surface(&g, &g, &grid, 3, 4, None).unwrap();
        for (k, x) in p.grid(&g, &g).iter().enumerate() {
            assert!(close(*x, grid[k], 1e-12));
        }
        let (_, xu, xv) = p.frame(0.3, 0.6);
        assert!(
            close(xu, DVec3::new(2., 0., 1.), 1e-12) && close(xv, DVec3::new(1., 3., 0.), 1e-12)
        );
        let q = p.at(0.25, 0.75) + p.normal(0.25, 0.75) * 0.5;
        let (s, t, d) = p.closest(q, 0.5, 0.5, 30);
        assert!((s - 0.25).abs() < 1e-10 && (t - 0.75).abs() < 1e-10 && (d - 0.5).abs() < 1e-10);
    }

    /// A rational quarter cylinder: the radius holds everywhere, knot insertion and restriction keep it.
    #[test]
    fn rational_quarter_cylinder_restricts_exactly() {
        let h = std::f64::consts::FRAC_1_SQRT_2;
        let p = Patch {
            u_order: 3,
            v_order: 2,
            u_knots: vec![0., 0., 0., 1., 1., 1.],
            v_knots: vec![0., 0., 1., 1.],
            coefficients: vec![
                DVec3::new(1., 0., 0.),
                DVec3::new(1., 1., 0.),
                DVec3::new(0., 1., 0.),
                DVec3::new(1., 0., 2.),
                DVec3::new(1., 1., 2.),
                DVec3::new(0., 1., 2.),
            ],
            weights: Some(vec![1., h, 1., 1., h, 1.]),
        };
        assert!(p.rational());
        for &(s, t) in &[(0., 0.), (0.3, 0.5), (1., 1.), (0.7, 0.2)] {
            let x = p.at(s, t);
            assert!(((x.x * x.x + x.y * x.y).sqrt() - 1.).abs() < 1e-14);
        }
        let q = restrict(&p, 0.25, 0.75, 0.5, 1.);
        let q = normalized(&q);
        for &(s, t) in &[(0., 0.), (0.5, 0.5), (1., 1.)] {
            let x = q.at(s, t);
            assert!(((x.x * x.x + x.y * x.y).sqrt() - 1.).abs() < 1e-13);
            assert!(close(x, p.at(0.25 + 0.5 * s, 0.5 + 0.5 * t), 1e-13));
        }
        let pieces = split_c0(&Patch {
            u_knots: vec![0., 0., 0., 0.5, 0.5, 1., 1., 1.],
            coefficients: [
                p.coefficients[..3].to_vec(),
                p.coefficients[1..3].to_vec(),
                p.coefficients[3..].to_vec(),
                p.coefficients[4..].to_vec(),
            ]
            .concat(),
            weights: Some(vec![1., h, 1., h, 1., 1., h, 1., h, 1.]),
            ..p.clone()
        });
        assert_eq!(pieces.len(), 2);
    }

    #[test]
    fn curve_fit_interpolates_ends_and_fits_a_circle_closely() {
        let g = unit_grid(21);
        let points: Vec<DVec3> = g
            .iter()
            .map(|&s| DVec3::new((s * 1.5).cos(), (s * 1.5).sin(), 0.))
            .collect();
        let c = fit_curve(&g, &points, 3, 4).unwrap();
        assert!(close(c[0], points[0], 0.) && close(c[c.len() - 1], points[20], 0.));
        let knots = uniform_knots(3, 4);
        let worst = g
            .iter()
            .zip(&points)
            .map(|(&s, p)| {
                let b = basis(&knots, 4, s, false);
                b.iter()
                    .zip(&c)
                    .map(|(w, q)| *q * *w)
                    .sum::<DVec3>()
                    .distance(*p)
            })
            .fold(0., f64::max);
        assert!((worst - 1.0067527504606327e-4).abs() < 1e-12, "{worst}");
        // Coefficients of an independent least-squares fit of the same samples.
        assert!(close(c[1], DVec3::new(1.00020317, 0.16661843, 0.), 1e-8));
        assert!(close(c[3], DVec3::new(0.56336836, 0.87728019, 0.), 1e-8));
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Exact rational patches for the common analytic pieces.
//!
//! A patch whose two opposite edges are circular arcs or straight lines and whose other two edges are
//! straight is the ruled surface between the two: a quarter of a cylinder or a cone, of an annulus, pieces
//! of a disc or a polygon with arc edges. A surface of revolution bounded by parallels and meridians (a
//! sphere, a torus) is the profile arc swept about its axis. A circular arc of angle t is the rational
//! quadratic with the middle control point at the tangents' intersection and middle weight cos(t / 2)
//! (Piegl & Tiller 1997, sections 7.3 and 8.5).

use super::bspline::{Patch, unit_grid};
use glam::DVec3;

const QUAD: [f64; 6] = [0., 0., 0., 1., 1., 1.];
const LINE: [f64; 4] = [0., 0., 1., 1.];

/// A rational quadratic: three control points and their weights.
#[derive(Clone, Debug)]
pub struct Conic {
    pub points: [DVec3; 3],
    pub weights: [f64; 3],
}

impl Conic {
    fn straight(&self) -> bool {
        self.weights.iter().all(|&w| (w - 1.).abs() <= 1e-8)
    }
}

/// The rational quadratic through sampled points of a straight segment or a circular arc (at most
/// `max_angle` degrees) from the first to the last point, or None when the samples are neither within tol.
pub fn conic(p: &[DVec3], tol: f64, max_angle: f64) -> Option<Conic> {
    let (a, b) = (p[0], p[p.len() - 1]);
    let chord = b - a;
    let length = chord.length();
    if length <= tol {
        return None;
    }
    let direction = chord / length;
    let off: Vec<f64> = p
        .iter()
        .map(|x| (*x - a).cross(direction).length())
        .collect();
    let (far, far_off) =
        off.iter()
            .copied()
            .enumerate()
            .fold(
                (0, f64::NEG_INFINITY),
                |acc, (k, v)| if v > acc.1 { (k, v) } else { acc },
            );
    if far_off <= tol {
        return Some(Conic {
            points: [a, (a + b) / 2., b],
            weights: [1.; 3],
        });
    }
    let m = p[far];
    let (u, v) = (m - a, b - a);
    let n = u.cross(v);
    let nn = n.dot(n);
    if nn <= 0. {
        return None;
    }
    let c = a + (n.cross(u) * v.dot(v) + v.cross(n) * u.dot(u)) / (2. * nn);
    let radius = (a - c).length();
    let nh = n / nn.sqrt();
    if p.iter()
        .any(|x| ((*x - c).length() - radius).abs() > tol || (*x - c).dot(nh).abs() > tol)
    {
        return None;
    }
    let e1 = (a - c) / radius;
    let e2 = nh.cross(e1);
    let mut angles: Vec<f64> = p
        .iter()
        .map(|x| {
            (*x - c)
                .dot(e2)
                .atan2((*x - c).dot(e1))
                .rem_euclid(std::f64::consts::TAU)
        })
        .collect();
    angles[0] = 0.;
    let theta = angles[angles.len() - 1];
    let increasing = angles.windows(2).all(|w| w[1] - w[0] >= -1e-12);
    if !(increasing && 0. < theta && theta <= max_angle.to_radians() + 1e-12) {
        return None;
    }
    let h = (theta / 2.).cos();
    let mid = c + radius / h * ((theta / 2.).cos() * e1 + (theta / 2.).sin() * e2);
    Some(Conic {
        points: [a, mid, b],
        weights: [1., h, 1.],
    })
}

/// The ruled patch (u order 3, v order 2) between two rational quadratics.
pub fn ruled(bottom: &Conic, top: &Conic) -> Patch {
    let weights: Vec<f64> = bottom.weights.iter().chain(&top.weights).copied().collect();
    let polynomial = weights
        .iter()
        .all(|&w| (w - 1.).abs() <= 1e-8 * w.abs().max(1.));
    Patch {
        u_order: 3,
        v_order: 2,
        u_knots: QUAD.to_vec(),
        v_knots: LINE.to_vec(),
        coefficients: bottom.points.iter().chain(&top.points).copied().collect(),
        weights: (!polynomial).then_some(weights),
    }
}

/// The largest distance of sampled edges (bottom, right, top, left) to the patch: each sample projected
/// from the nearest of n points along the patch's own edge.
pub fn edge_error(patch: &Patch, edges: [&[DVec3]; 4], n: usize) -> f64 {
    let g = unit_grid(n);
    let mut error: f64 = 0.;
    for (k, edge) in edges.iter().enumerate() {
        let starts: Vec<(f64, f64)> = g
            .iter()
            .map(|&x| match k {
                0 => (x, 0.),
                1 => (1., x),
                2 => (x, 1.),
                _ => (0., x),
            })
            .collect();
        let points: Vec<DVec3> = starts.iter().map(|&(s, t)| patch.at(s, t)).collect();
        for &q in *edge {
            let nearest = (0..n)
                .min_by(|&i, &j| points[i].distance(q).total_cmp(&points[j].distance(q)))
                .unwrap_or(0);
            let (s, t) = starts[nearest];
            error = error.max(patch.closest(q, s, t, 30).2);
        }
    }
    error
}

/// The circle through sampled points of an arc: centre, unit normal (turning the first point towards the
/// last), radius; a single point (a pole) gives no normal; None when the samples are no arc within tol.
fn circle(p: &[DVec3], tol: f64) -> Option<(DVec3, Option<DVec3>, f64)> {
    if p.iter().all(|x| x.distance(p[0]) <= tol) {
        return Some((p[0], None, 0.));
    }
    let arc = conic(p, tol, 120.)?;
    if arc.straight() {
        return None;
    }
    let [a, m, b] = arc.points;
    let h = arc.weights[1];
    let mid = (a + b) / 2.;
    let radius = (a - mid).length() / (1. - h * h).sqrt();
    let c = mid + (mid - m).normalize() * radius * h;
    let n = (a - c).cross(b - c);
    Some((c, Some(n.normalize()), (a - c).length()))
}

/// Points rotated about the axis through `origin` by `angle` (Rodrigues).
fn rotate(x: DVec3, origin: DVec3, axis: DVec3, angle: f64) -> DVec3 {
    let r = x - origin;
    let (c, s) = (angle.cos(), angle.sin());
    origin + r * c + axis.cross(r) * s + axis * r.dot(axis) * (1. - c)
}

/// The rational patch of a profile (a rational quadratic or a line) turned by `angle` (at most 120 degrees)
/// about the axis through `origin`: u along the rotation, v along the profile.
pub fn revolved(profile: &Conic, line: bool, origin: DVec3, axis: DVec3, angle: f64) -> Patch {
    let h = (angle / 2.).cos();
    let (q, w): (Vec<DVec3>, Vec<f64>) = if line {
        (vec![profile.points[0], profile.points[2]], vec![1., 1.])
    } else {
        (profile.points.to_vec(), profile.weights.to_vec())
    };
    let mut coefficients = Vec::new();
    let mut weights = Vec::new();
    for (point, weight) in q.iter().zip(&w) {
        let on_axis = origin + axis * (*point - origin).dot(axis);
        let end = rotate(*point, origin, axis, angle);
        let mid = on_axis + (rotate(*point, origin, axis, angle / 2.) - on_axis) / h;
        coefficients.extend([*point, mid, end]);
        weights.extend([*weight, weight * h, *weight]);
    }
    Patch {
        u_order: 3,
        v_order: q.len(),
        u_knots: QUAD.to_vec(),
        v_knots: if line { LINE.to_vec() } else { QUAD.to_vec() },
        coefficients,
        weights: Some(weights),
    }
}

/// The largest distance of points to a patch (projected from the nearest of an n x n grid).
fn point_error(patch: &Patch, points: &[DVec3], n: usize) -> f64 {
    if points.is_empty() {
        return 0.;
    }
    let g = unit_grid(n);
    let starts: Vec<(f64, f64)> = g
        .iter()
        .flat_map(|&t| g.iter().map(move |&s| (s, t)))
        .collect();
    let samples: Vec<DVec3> = starts.iter().map(|&(s, t)| patch.at(s, t)).collect();
    points
        .iter()
        .map(|&q| {
            let k = (0..samples.len())
                .min_by(|&i, &j| samples[i].distance(q).total_cmp(&samples[j].distance(q)))
                .unwrap_or(0);
            patch.closest(q, starts[k].0, starts[k].1, 30).2
        })
        .fold(0., f64::max)
}

/// A sampled n x n grid (rows along v) with its four edges.
pub struct Grid<'a> {
    pub points: &'a [DVec3],
    pub n: usize,
}

impl Grid<'_> {
    fn row(&self, j: usize) -> Vec<DVec3> {
        self.points[j * self.n..(j + 1) * self.n].to_vec()
    }

    fn column(&self, i: usize) -> Vec<DVec3> {
        (0..self.n).map(|j| self.points[j * self.n + i]).collect()
    }

    /// Bottom, right, top, left.
    pub fn edges(&self) -> [Vec<DVec3>; 4] {
        [
            self.row(0),
            self.column(self.n - 1),
            self.row(self.n - 1),
            self.column(0),
        ]
    }

    fn transposed(&self) -> Vec<DVec3> {
        (0..self.n).flat_map(|i| self.column(i)).collect()
    }
}

/// The exact ruled patch of a sampled grid (its boundary rows: the CAD edges), u along the grid's columns,
/// or None. Either the bottom and top edges are arcs or lines and the left and right edges lines, or the
/// other way round.
pub fn exact_patch(grid: &Grid, tol: f64) -> Option<Patch> {
    let edges = grid.edges();
    let refs = [&edges[0][..], &edges[1][..], &edges[2][..], &edges[3][..]];
    for k in 0..2 {
        let (c0, c1, straight) = if k == 0 {
            (&edges[0], &edges[2], [&edges[1], &edges[3]])
        } else {
            (&edges[3], &edges[1], [&edges[0], &edges[2]])
        };
        let (Some(a), Some(b)) = (conic(c0, tol, 120.), conic(c1, tol, 120.)) else {
            continue;
        };
        if straight
            .iter()
            .any(|e| conic(e, tol, 120.).is_none_or(|c| !c.straight()))
        {
            continue;
        }
        let p = if k == 0 {
            ruled(&a, &b)
        } else {
            ruled(&a, &b).swapped()
        };
        if edge_error(&p, refs, 201) <= tol {
            return Some(p);
        }
    }
    None
}

/// The exact patch of a sampled grid that is a surface of revolution with its rows (or columns) on circles
/// about one axis and the other edges profiles: a piece of a sphere or a torus (or a cylinder, a cone)
/// bounded by parallels and meridians, a parallel collapsed to a pole included; or None.
pub fn revolved_patch(grid: &Grid, tol: f64) -> Option<Patch> {
    let original = grid.edges();
    let refs = [
        &original[0][..],
        &original[1][..],
        &original[2][..],
        &original[3][..],
    ];
    let n = grid.n;
    for transposed in [false, true] {
        let data = if transposed {
            grid.transposed()
        } else {
            grid.points.to_vec()
        };
        let g = Grid { points: &data, n };
        let mut picks: Vec<usize> = (0..5)
            .map(|k| (k as f64 * (n - 1) as f64 / 4.).round() as usize)
            .collect();
        picks.dedup();
        let rows: Vec<Vec<DVec3>> = picks.iter().map(|&j| g.row(j)).collect();
        let circles: Option<Vec<_>> = rows.iter().map(|r| circle(r, tol)).collect();
        let Some(circles) = circles else { continue };
        let Some(real) = circles.iter().position(|c| c.1.is_some()) else {
            continue;
        };
        let (origin, axis) = (circles[real].0, circles[real].1.unwrap_or(DVec3::Z));
        if circles
            .iter()
            .any(|c| c.1.is_some_and(|nk| nk.dot(axis) < 1. - 1e-9))
        {
            continue;
        }
        let off_axis = circles.iter().any(|c| {
            let d = c.0 - origin;
            (d - axis * d.dot(axis)).abs().max_element() > tol
        });
        if off_axis {
            continue;
        }
        let r = &rows[real];
        let (mut e1, mut e2) = (r[0] - origin, r[r.len() - 1] - origin);
        e1 -= axis * e1.dot(axis);
        e2 -= axis * e2.dot(axis);
        let angle = e1.cross(e2).dot(axis).atan2(e1.dot(e2));
        if !(0. < angle && angle <= 120f64.to_radians() + 1e-12) {
            continue;
        }
        let Some(profile) = conic(&g.column(0), tol, 120.) else {
            continue;
        };
        let mut p = revolved(&profile, profile.straight(), origin, axis, angle);
        if transposed {
            p = p.swapped();
        }
        let inner: Vec<DVec3> = (1..n - 1)
            .step_by(4)
            .flat_map(|j| (1..n - 1).step_by(4).map(move |i| grid.points[j * n + i]))
            .collect();
        if edge_error(&p, refs, 201) <= tol && point_error(&p, &inner, 41) <= tol {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(n: usize, f: impl Fn(f64, f64) -> DVec3) -> Vec<DVec3> {
        let g = unit_grid(n);
        g.iter()
            .flat_map(|&t| g.iter().map(|&s| f(s, t)).collect::<Vec<_>>())
            .collect()
    }

    #[test]
    fn quarter_cylinder_becomes_an_exact_ruled_patch() {
        let n = 21;
        let points = grid(n, |s, t| {
            let a = s * std::f64::consts::FRAC_PI_2;
            DVec3::new(5. * a.cos(), 5. * a.sin(), -10. * t)
        });
        let p = exact_patch(&Grid { points: &points, n }, 1e-9).expect("ruled");
        assert!(p.rational());
        for &(s, t) in &[(0.1, 0.2), (0.5, 0.5), (0.9, 0.7)] {
            let x = p.at(s, t);
            assert!(((x.x * x.x + x.y * x.y).sqrt() - 5.).abs() < 1e-12);
        }
    }

    #[test]
    fn sphere_octant_piece_becomes_an_exact_revolved_patch() {
        let n = 21;
        // rows: parallels (longitude along s), columns: meridians from the equator up to 60 degrees
        let points = grid(n, |s, t| {
            let (lon, lat) = (
                s * std::f64::consts::FRAC_PI_2,
                t * std::f64::consts::FRAC_PI_3,
            );
            3. * DVec3::new(lat.cos() * lon.cos(), lat.cos() * lon.sin(), -lat.sin())
        });
        let p = revolved_patch(&Grid { points: &points, n }, 1e-9).expect("revolved");
        for &(s, t) in &[(0.2, 0.3), (0.6, 0.9)] {
            assert!((p.at(s, t).length() - 3.).abs() < 1e-12);
        }
        assert!(exact_patch(&Grid { points: &points, n }, 1e-9).is_none());
    }
}

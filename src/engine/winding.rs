// SPDX-License-Identifier: GPL-3.0-or-later
//! Global face orientation by the generalized winding number, for faces the CAD solids do not decide.
//!
//! The generalized winding number of a triangulated surface at a point p (Jacobson, Kavan &
//! Sorkine-Hornung 2013) is the sum of the signed solid angles of its triangles over 4 pi: 1 inside and 0
//! outside a closed surface with outward normals. The wetted surface is closed by its images in the
//! symmetry planes and by its mirror in z = 0 when it is open at the water. With an unknown sign s_j per
//! face (+1: its provisional normals point out of the body), w = sum_j s_j w_j, and of two points offset to
//! both sides of a face one is inside and one outside for the correct signs:
//!     sum_j s_j (w_j(p+) + w_j(p-)) = 1,
//! a linear system in s solved by least squares (a ridge start, then single sign flips while they lower
//! the residual). A face's own term is odd across it, so the system decides each face from all the others.

use super::panels::Panel;
use anyhow::{Result, ensure};
use glam::DVec3;
use serde_json::{Value, json};

/// Points per face at which the two offset points are taken.
const POINTS: usize = 5;
/// The first offset, relative to the face diameter; halved until the points keep clear of the surface.
const OFFSET: f64 = 0.1;
/// A face whose mean residual exceeds this is reported as a possible sheet (both sides in or out).
const SHEET: f64 = 0.5;

/// One face with its panels oriented provisionally (any consistent choice, here along the CAD normal).
pub struct Face<'a> {
    pub panels: &'a [Panel],
}

/// The result: per face +1 when its provisional normals point out of the body, and the report record.
pub struct Winding {
    pub out_of_body: Vec<bool>,
    pub record: Value,
}

/// The triangles of a panel (one for a triangle panel).
fn triangles(p: &Panel) -> Vec<[DVec3; 3]> {
    let q = p.map(DVec3::from_array);
    if p[2] == p[3] {
        vec![[q[0], q[1], q[2]]]
    } else {
        vec![[q[0], q[1], q[2]], [q[0], q[2], q[3]]]
    }
}

/// The image sign vectors that close the wetted surface: symmetry planes and the mirror in z = 0.
fn images(axes: &[usize], mirror_z: bool) -> Vec<DVec3> {
    let sx: &[f64] = if axes.contains(&0) { &[1., -1.] } else { &[1.] };
    let sy: &[f64] = if axes.contains(&1) { &[1., -1.] } else { &[1.] };
    let sz: &[f64] = if mirror_z { &[1., -1.] } else { &[1.] };
    let mut out = Vec::new();
    for &a in sx {
        for &b in sy {
            for &c in sz {
                out.push(DVec3::new(a, b, c));
            }
        }
    }
    out
}

/// A face's triangles and their images, each reflected image with its winding reversed.
fn closed(face: &Face, signs: &[DVec3]) -> Vec<[DVec3; 3]> {
    let mut out = Vec::new();
    for &s in signs {
        let reflection = s.x * s.y * s.z < 0.;
        for p in face.panels {
            for [a, b, c] in triangles(p) {
                let t = [a * s, b * s, c * s];
                out.push(if reflection { [t[0], t[2], t[1]] } else { t });
            }
        }
    }
    out
}

/// The generalized winding numbers of triangles at the points (van Oosterom & Strackee's solid angle / 4 pi).
fn solid_angles(points: &[DVec3], tris: &[[DVec3; 3]]) -> Vec<f64> {
    points
        .iter()
        .map(|&p| {
            let mut sum = 0.;
            for t in tris {
                let (a, b, c) = (t[0] - p, t[1] - p, t[2] - p);
                let (la, lb, lc) = (a.length(), b.length(), c.length());
                let det = a.dot(b.cross(c));
                let den = la * lb * lc + a.dot(b) * lc + b.dot(c) * la + c.dot(a) * lb;
                sum += det.atan2(den);
            }
            sum / (2. * std::f64::consts::PI)
        })
        .collect()
}

/// The distance from a point to a triangle (Ericson, Real-Time Collision Detection, 5.1.5).
fn triangle_distance(p: DVec3, [a, b, c]: &[DVec3; 3]) -> f64 {
    let (ab, ac, ap) = (*b - *a, *c - *a, p - *a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0. && d2 <= 0. {
        return p.distance(*a);
    }
    let bp = p - *b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0. && d4 <= d3 {
        return p.distance(*b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0. && d1 >= 0. && d3 <= 0. {
        return p.distance(*a + ab * (d1 / (d1 - d3)));
    }
    let cp = p - *c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0. && d5 <= d6 {
        return p.distance(*c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0. && d2 >= 0. && d6 <= 0. {
        return p.distance(*a + ac * (d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0. && (d4 - d3) >= 0. && (d5 - d6) >= 0. {
        return p.distance(*b + (*c - *b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denominator = 1. / (va + vb + vc);
    p.distance(*a + ab * (vb * denominator) + ac * (vc * denominator))
}

/// Solve the symmetric positive definite system A x = b by Cholesky factorization.
#[allow(clippy::needless_range_loop)] // matrix indices read as the textbook algorithm
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Result<Vec<f64>> {
    let n = b.len();
    for j in 0..n {
        let mut d = a[j][j];
        for k in 0..j {
            d -= a[j][k] * a[j][k];
        }
        ensure!(d > 0., "Winding system is not positive definite");
        let d = d.sqrt();
        a[j][j] = d;
        for i in j + 1..n {
            let mut s = a[i][j];
            for k in 0..j {
                s -= a[i][k] * a[j][k];
            }
            a[i][j] = s / d;
        }
    }
    for i in 0..n {
        for k in 0..i {
            b[i] -= a[i][k] * b[k];
        }
        b[i] /= a[i][i];
    }
    for i in (0..n).rev() {
        for k in i + 1..n {
            b[i] -= a[k][i] * b[k];
        }
        b[i] /= a[i][i];
    }
    Ok(b)
}

/// Up to `POINTS` well-separated panel indices of a face: the panel nearest its centre, then the farthest.
fn spread(centres: &[DVec3]) -> Vec<usize> {
    let mean = centres.iter().sum::<DVec3>() / centres.len() as f64;
    let mut chosen = vec![
        (0..centres.len())
            .min_by(|&i, &j| {
                centres[i]
                    .distance(mean)
                    .total_cmp(&centres[j].distance(mean))
            })
            .unwrap_or(0),
    ];
    let mut distance: Vec<f64> = centres
        .iter()
        .map(|c| c.distance(centres[chosen[0]]))
        .collect();
    while chosen.len() < POINTS.min(centres.len()) {
        for &c in &chosen {
            distance[c] = -1.;
        }
        let k = (0..centres.len())
            .max_by(|&i, &j| distance[i].total_cmp(&distance[j]))
            .unwrap_or(0);
        chosen.push(k);
        for (d, c) in distance.iter_mut().zip(centres) {
            *d = d.min(c.distance(centres[k]));
        }
    }
    chosen
}

/// Orient the faces globally. `axes` are the symmetry plane normals.
pub fn orient(faces: &[Face], axes: &[usize]) -> Result<Winding> {
    ensure!(!faces.is_empty(), "No faces to orient");
    let nodes: Vec<DVec3> = faces
        .iter()
        .flat_map(|f| f.panels.iter().flatten().map(|&p| DVec3::from_array(p)))
        .collect();
    let lo = nodes.iter().fold(DVec3::INFINITY, |a, &p| a.min(p));
    let hi = nodes.iter().fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
    let tol = 1e-4 * (hi - lo).max_element();
    // The surface is open at the water unless it reaches above it or has a face lying in z = 0 (a deck).
    let deck = faces
        .iter()
        .any(|f| f.panels.iter().flatten().all(|p| p[2].abs() <= tol));
    let mirror_z = hi.z <= tol && !deck;
    let signs = images(axes, mirror_z);
    let closed: Vec<Vec<[DVec3; 3]>> = faces.iter().map(|f| closed(f, &signs)).collect();
    // Every triangle of every image: an offset point must keep away from the whole surface, not only its nodes.
    let surface: Vec<[DVec3; 3]> = closed.iter().flatten().copied().collect();
    let (mut plus, mut minus) = (Vec::new(), Vec::new());
    let mut rows_per_face = Vec::new();
    for face in faces {
        let centres: Vec<DVec3> = face
            .panels
            .iter()
            .map(|p| p.map(DVec3::from_array).into_iter().sum::<DVec3>() / 4.)
            .collect();
        let points: Vec<DVec3> = face
            .panels
            .iter()
            .flatten()
            .map(|&p| DVec3::from_array(p))
            .collect();
        let lo = points.iter().fold(DVec3::INFINITY, |a, &p| a.min(p));
        let hi = points.iter().fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
        let h0 = OFFSET * (hi - lo).length();
        let chosen = spread(&centres);
        for &k in &chosen {
            let n = super::panels::normal(&face.panels[k]).normalize();
            let x = centres[k];
            let mut h = h0;
            for _ in 0..8 {
                let near = [x + h * n, x - h * n].iter().all(|q| {
                    surface
                        .iter()
                        .map(|t| triangle_distance(*q, t))
                        .fold(f64::INFINITY, f64::min)
                        >= 0.5 * h
                });
                if near {
                    break;
                }
                h *= 0.5;
            }
            plus.push(x + h * n);
            minus.push(x - h * n);
        }
        rows_per_face.push(chosen.len());
    }
    let rows = plus.len();
    let both: Vec<DVec3> = plus.iter().chain(&minus).copied().collect();
    // M[row][face] = w_face(p+) + w_face(p-)
    let columns: Vec<Vec<f64>> = closed
        .iter()
        .map(|tris| {
            let w = solid_angles(&both, tris);
            (0..rows).map(|i| w[i] + w[rows + i]).collect()
        })
        .collect();
    let m = |i: usize, j: usize| columns[j][i];
    let n = faces.len();
    let mut g = vec![vec![0.; n]; n];
    for (a, row) in g.iter_mut().enumerate() {
        for (b, value) in row.iter_mut().enumerate() {
            *value = (0..rows).map(|i| m(i, a) * m(i, b)).sum();
        }
    }
    let trace: f64 = (0..n).map(|j| g[j][j]).sum();
    let mut ridge = g.clone();
    for (j, row) in ridge.iter_mut().enumerate() {
        row[j] += 1e-3 * trace / n as f64;
    }
    let rhs: Vec<f64> = (0..n).map(|j| (0..rows).map(|i| m(i, j)).sum()).collect();
    let s = solve(ridge, rhs)?;
    let mut sign: Vec<f64> = s.iter().map(|&v| if v >= 0. { 1. } else { -1. }).collect();
    let column_norms: Vec<f64> = (0..n)
        .map(|j| (0..rows).map(|i| m(i, j) * m(i, j)).sum())
        .collect();
    let residual = |sign: &[f64]| -> Vec<f64> {
        (0..rows)
            .map(|i| (0..n).map(|j| m(i, j) * sign[j]).sum::<f64>() - 1.)
            .collect()
    };
    let gains = |sign: &[f64], r: &[f64]| -> Vec<f64> {
        (0..n)
            .map(|j| {
                let mtr: f64 = (0..rows).map(|i| m(i, j) * r[i]).sum();
                4. * sign[j] * mtr - 4. * column_norms[j]
            })
            .collect()
    };
    let mut flips = 0;
    for _ in 0..4 * n {
        let r = residual(&sign);
        let gain = gains(&sign, &r);
        let (j, best) =
            gain.iter()
                .copied()
                .enumerate()
                .fold(
                    (0, f64::NEG_INFINITY),
                    |a, (j, g)| if g > a.1 { (j, g) } else { a },
                );
        if best <= 1e-12 {
            break;
        }
        sign[j] = -sign[j];
        flips += 1;
    }
    let r = residual(&sign);
    let margin: Vec<f64> = gains(&sign, &r).iter().map(|g| -g / rows as f64).collect();
    let mean_square = r.iter().map(|x| x * x).sum::<f64>() / rows as f64;
    let mut per_face = Vec::new();
    let mut start = 0;
    for &k in &rows_per_face {
        per_face.push(r[start..start + k].iter().sum::<f64>() / k as f64);
        start += k;
    }
    let least = (0..n)
        .min_by(|&a, &b| margin[a].total_cmp(&margin[b]))
        .unwrap_or(0);
    let margin_min = margin[least];
    let sheets_left: Vec<usize> = (0..n).filter(|&j| per_face[j].abs() > SHEET).collect();
    let record = json!({
        "ridge_sign_min_abs": s.iter().map(|v| v.abs()).fold(f64::INFINITY, f64::min),
        "flips": flips, "residual_rms": mean_square.sqrt(), "flip_margin_min": margin_min,
        "flip_margin_ratio": margin_min / mean_square.max(1e-300), "least_sure_face": least,
        "mirror_z": mirror_z, "sheets_left_faces": sheets_left,
        "face_residual_max": per_face.iter().map(|v| v.abs()).fold(0., f64::max),
    });
    Ok(Winding {
        out_of_body: sign.iter().map(|&v| v > 0.).collect(),
        record,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3]) -> Panel {
        [a, b, c, d]
    }

    /// A unit cube below z = 0, its six faces with outward normals, some flipped: the winding decides them.
    #[test]
    fn cube_faces_are_oriented_out_of_the_body() {
        let (x0, x1, y0, y1, z0, z1) = (1., 2., 1., 2., -2., -1.);
        let faces: Vec<Panel> = vec![
            quad([x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]), // bottom, out (-z)
            quad([x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]), // top, out (+z)
            quad([x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]), // y0, out (-y)
            quad([x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]), // y1, out (+y)
            quad([x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]), // x0, out (-x)
            quad([x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]), // x1, out (+x)
        ];
        let flipped = [false, true, false, true, true, false];
        let panels: Vec<[Panel; 1]> = faces
            .iter()
            .zip(flipped)
            .map(|(p, f)| {
                [if f {
                    super::super::panels::reversed(p)
                } else {
                    *p
                }]
            })
            .collect();
        let input: Vec<Face> = panels.iter().map(|p| Face { panels: p }).collect();
        let result = orient(&input, &[]).unwrap();
        let expected: Vec<bool> = flipped.iter().map(|f| !f).collect();
        assert_eq!(result.out_of_body, expected);
        assert!(result.record["flip_margin_min"].as_f64().unwrap() > 0.);
    }
}

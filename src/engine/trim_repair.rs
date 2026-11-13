// SPDX-License-Identifier: GPL-3.0-or-later
//! Repair of folded quads on trimmed cylinders.
//!
//! A split face meshes triangles into three quads each; on a cylinder trimmed by a concave curve some quads
//! fold. The outer boundary of each folded region (its triangles' quads and their neighbours) is unrolled into
//! a non-periodic angular chart, simplified within the fit tolerance, meshed as a planar polygon with quality
//! triangles, convex triangle pairs merged into quads, and every chart cell becomes an exact rational bicubic
//! cylinder patch (triangles have one collapsed edge). A region that cannot be repaired keeps its patches and
//! their failed checks.

use super::{
    bspline::{Patch, basis, uniform_knots, unit_grid},
    checks,
    patches::QuadPatch,
    prepare::linspace,
};
use crate::cad::session::Session;
use anyhow::{Result, bail, ensure};
use glam::{DVec2, DVec3};
use nalgebra::DMatrix;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A cylinder frame: origin on the axis, two radial vectors at angle 0 and 90 degrees, the axis (x_v), and
/// the parameter bounds.
#[derive(Clone)]
pub struct Frame {
    pub origin: DVec3,
    pub ex: DVec3,
    pub ey: DVec3,
    pub axis: DVec3,
    pub lo: [f64; 2],
    pub hi: [f64; 2],
}

/// The frame of a cylindrical face of the open model.
pub fn frame(gmsh: &Session, face: i32) -> Result<Frame> {
    let x: Vec<DVec3> = gmsh
        .value(
            2,
            face,
            &[
                0.,
                0.,
                std::f64::consts::PI,
                0.,
                std::f64::consts::FRAC_PI_2,
                0.,
            ],
        )?
        .into_iter()
        .map(DVec3::from_array)
        .collect();
    let origin = (x[0] + x[1]) / 2.;
    let d = gmsh.derivative(2, face, &[0., 0.])?;
    let (lo, hi) = gmsh.parametrization_bounds(2, face)?;
    Ok(Frame {
        origin,
        ex: x[0] - origin,
        ey: x[2] - origin,
        axis: DVec3::new(d[3], d[4], d[5]),
        lo: [lo[0], lo[1]],
        hi: [hi[0], hi[1]],
    })
}

fn cross(a: DVec2, b: DVec2) -> f64 {
    a.x * b.y - a.y * b.x
}

/// Signed polygon area, translated to avoid cancellation far from the origin.
fn area(points: &[DVec2]) -> f64 {
    let p0 = points[0];
    let n = points.len();
    (0..n)
        .map(|k| cross(points[k] - p0, points[(k + 1) % n] - p0))
        .sum::<f64>()
        / 2.
}

/// Polyline vertices within tol of the original samples (Ramer-Douglas-Peucker).
fn simplify_open(points: &[DVec2], tol: f64) -> Vec<DVec2> {
    let mut keep: BTreeSet<usize> = [0, points.len() - 1].into_iter().collect();
    let mut pending = vec![(0, points.len() - 1)];
    while let Some((a, b)) = pending.pop() {
        if b <= a + 1 {
            continue;
        }
        let edge = points[b] - points[a];
        let (mut far, mut distance) = (0, -1.);
        for (k, p) in points[a + 1..b].iter().enumerate() {
            let delta = *p - points[a];
            let t = (delta.dot(edge) / edge.dot(edge).max(1e-300)).clamp(0., 1.);
            let d = (delta - t * edge).length();
            if d > distance {
                (far, distance) = (k, d);
            }
        }
        if distance > tol {
            let j = a + 1 + far;
            keep.insert(j);
            pending.push((a, j));
            pending.push((j, b));
        }
    }
    keep.into_iter().map(|k| points[k]).collect()
}

/// Simplify a closed polygon using two non-degenerate open chains.
fn simplify_closed(points: &[DVec2], tol: f64) -> Vec<DVec2> {
    let k = (0..points.len())
        .max_by(|&a, &b| {
            points[a]
                .distance(points[0])
                .total_cmp(&points[b].distance(points[0]))
        })
        .unwrap_or(0);
    let a = simplify_open(&points[..=k], tol);
    let mut tail = points[k..].to_vec();
    tail.push(points[0]);
    let b = simplify_open(&tail, tol);
    let mut out = a[..a.len() - 1].to_vec();
    out.extend(&b[..b.len() - 1]);
    out
}

/// Clip a polygon at an angular coordinate.
fn clip(points: &[DVec2], bound: f64, keep_above: bool) -> Vec<DVec2> {
    let mut out = Vec::new();
    let n = points.len();
    for k in 0..n {
        let (a, b) = (points[k], points[(k + 1) % n]);
        let inside = |p: DVec2| {
            if keep_above {
                p.x >= bound
            } else {
                p.x <= bound
            }
        };
        if inside(a) {
            out.push(a);
        }
        if inside(a) != inside(b) {
            out.push(a + (b - a) * ((bound - a.x) / (b.x - a.x)));
        }
    }
    out
}

/// Validate that the polygon is simple (no crossing boundary segments) and orientable by ear clipping.
fn validate_polygon(p: &[DVec2]) -> Result<()> {
    let n = p.len();
    let lo = p.iter().fold(DVec2::INFINITY, |a, &x| a.min(x));
    let hi = p.iter().fold(DVec2::NEG_INFINITY, |a, &x| a.max(x));
    let span = (hi - lo).max_element().max(1e-15);
    let tolerance = 1e-13 * span * span;
    for i in 0..n {
        let (a, b) = (p[i], p[(i + 1) % n]);
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            let (c, d) = (p[j], p[(j + 1) % n]);
            let (ca, cb) = (cross(b - a, c - a), cross(b - a, d - a));
            let (cc, cd) = (cross(d - c, a - c), cross(d - c, b - c));
            if ca * cb < -tolerance * tolerance && cc * cd < -tolerance * tolerance {
                bail!("cylindrical trim polygon has crossing boundary segments");
            }
        }
    }
    Ok(())
}

/// Merge neighbouring triangles whose union is a strictly convex quadrilateral (best-shaped first).
fn quad_cells(triangles: &[[DVec2; 3]]) -> Vec<Vec<DVec2>> {
    let key = |a: DVec2, b: DVec2| {
        let (x, y) = (
            (a.x.to_bits(), a.y.to_bits()),
            (b.x.to_bits(), b.y.to_bits()),
        );
        if x <= y { (x, y) } else { (y, x) }
    };
    let mut owners: BTreeMap<_, Vec<usize>> = BTreeMap::new();
    for (i, t) in triangles.iter().enumerate() {
        for k in 0..3 {
            owners.entry(key(t[k], t[(k + 1) % 3])).or_default().push(i);
        }
    }
    let mut candidates = Vec::new();
    for list in owners.values() {
        let [a, b] = list[..] else { continue };
        let mut points: Vec<DVec2> = triangles[a].iter().chain(&triangles[b]).copied().collect();
        points.sort_by(|p, q| {
            (p.x, p.y)
                .partial_cmp(&(q.x, q.y))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        points.dedup();
        if points.len() != 4 {
            continue;
        }
        let centre = points.iter().sum::<DVec2>() / 4.;
        points.sort_by(|p, q| {
            (p.y - centre.y)
                .atan2(p.x - centre.x)
                .total_cmp(&(q.y - centre.y).atan2(q.x - centre.x))
        });
        let edges: Vec<DVec2> = (0..4).map(|k| points[(k + 1) % 4] - points[k]).collect();
        let lo = points.iter().fold(DVec2::INFINITY, |acc, &x| acc.min(x));
        let hi = points
            .iter()
            .fold(DVec2::NEG_INFINITY, |acc, &x| acc.max(x));
        let extent = (hi - lo).max_element();
        if (0..4).any(|k| cross(edges[k], edges[(k + 1) % 4]) <= 1e-14 * extent * extent) {
            continue;
        }
        let score = (0..4)
            .map(|k| {
                let (e, f) = (edges[k], edges[(k + 3) % 4]);
                1. - (-e.dot(f) / (e.length() * f.length())).abs()
            })
            .fold(f64::INFINITY, f64::min);
        candidates.push((score, a, b, points));
    }
    candidates.sort_by(|x, y| y.0.total_cmp(&x.0));
    let mut used = BTreeSet::new();
    let mut out = Vec::new();
    for (_, a, b, quad) in candidates {
        if !used.contains(&a) && !used.contains(&b) {
            used.insert(a);
            used.insert(b);
            out.push(quad);
        }
    }
    out.extend(
        triangles
            .iter()
            .enumerate()
            .filter(|(i, _)| !used.contains(i))
            .map(|(_, t)| t.to_vec()),
    );
    out
}

/// Constrained planar quality triangles of a simple polygon, meshed in a temporary model.
fn quality_triangles(gmsh: &Session, points: &[DVec2]) -> Result<Vec<[DVec2; 3]>> {
    validate_polygon(points)?;
    let p: Vec<DVec2> = if area(points) > 0. {
        points.to_vec()
    } else {
        points.iter().rev().copied().collect()
    };
    let names = [
        "Mesh.Algorithm",
        "Mesh.RecombineAll",
        "Mesh.SubdivisionAlgorithm",
        "Mesh.MeshSizeMin",
        "Mesh.MeshSizeMax",
        "Mesh.MeshSizeFromPoints",
        "Mesh.MeshSizeFromCurvature",
        "Mesh.MeshSizeExtendFromBoundary",
    ];
    let saved: Vec<f64> = names
        .iter()
        .map(|n| gmsh.option_number(n))
        .collect::<Result<_>>()?;
    let current = gmsh.current_model()?;
    gmsh.add_model(&format!("{current}_trim_chart"))?;
    let result = (|| -> Result<Vec<[DVec2; 3]>> {
        let lo = p.iter().fold(DVec2::INFINITY, |a, &x| a.min(x));
        let hi = p.iter().fold(DVec2::NEG_INFINITY, |a, &x| a.max(x));
        let size = ((hi - lo).max_element() / 3.).max(1e-12);
        let flat: Vec<[f64; 2]> = p.iter().map(|x| [x.x, x.y]).collect();
        let surface = gmsh.geo_polygon(&flat, size)?;
        for (name, value) in names.iter().zip([6., 0., 0., 0., size, 1., 0., 1.]) {
            gmsh.number(name, value)?;
        }
        gmsh.generate(2)?;
        let (tags, xyz) = gmsh.nodes()?;
        let position: HashMap<usize, DVec2> = tags
            .into_iter()
            .zip(xyz)
            .map(|(t, x)| (t, DVec2::new(x[0], x[1])))
            .collect();
        let mut out = Vec::new();
        for (kind, nodes) in gmsh.elements(2, surface)? {
            ensure!(
                gmsh.element_nodes(kind)? == 3,
                "trim chart mesher did not return triangles"
            );
            for row in nodes.as_chunks::<3>().0 {
                let t = [position[&row[0]], position[&row[1]], position[&row[2]]];
                out.push(if area(&t) > 0. { t } else { [t[2], t[1], t[0]] });
            }
        }
        let total: f64 = out.iter().map(|t| area(t)).sum();
        ensure!(
            (total - area(&p).abs()).abs() <= 1e-9 * area(&p).abs() + 1e-12,
            "trim chart mesher changed the polygon area"
        );
        Ok(out)
    })();
    gmsh.remove_model()?;
    if !current.is_empty() {
        gmsh.set_current_model(&current)?;
    } else {
        gmsh.set_current_model("")?;
    }
    for (name, value) in names.iter().zip(saved) {
        gmsh.number(name, value)?;
    }
    // Gmsh keeps one global model size (General.BoundingBoxSize, its tolerances scale with it), set by the
    // last synchronized model: the chart left its own. Synchronizing the body again restores the body's, so
    // the next chart starts as this one did and a face's repair does not depend on the faces repaired before.
    gmsh.synchronize()?;
    result
}

/// The exact bicubic rational cylinder patch over a convex chart triangle or quadrilateral.
fn cylinder_chart_patch(corners: &[DVec2], frame: &Frame, u0: f64, u1: f64) -> Result<Patch> {
    let half = (u1 - u0) / 2.;
    let middle = (u0 + u1) / 2.;
    let weights = [1., half.cos(), 1.];
    let radial = [
        frame.ex * u0.cos() + frame.ey * u0.sin(),
        (frame.ex * middle.cos() + frame.ey * middle.sin()) / weights[1],
        frame.ex * u1.cos() + frame.ey * u1.sin(),
    ];
    let vref = corners.iter().map(|c| c.y).sum::<f64>() / corners.len() as f64;
    let tri: Vec<DVec2> = corners
        .iter()
        .map(|c| DVec2::new(c.x, c.y - vref))
        .collect();
    let origin = frame.origin + frame.axis * vref;
    let homogeneous: Vec<[f64; 4]> = (0..3)
        .map(|k| {
            let p = (origin + radial[k]) * weights[k];
            [p.x, p.y, p.z, weights[k]]
        })
        .collect();
    let g = unit_grid(4);
    // h[j][i]: homogeneous points of the chart grid at (s = g[i], t = g[j])
    let mut h = vec![[0.; 4]; 16];
    for (j, &t) in g.iter().enumerate() {
        for (i, &s) in g.iter().enumerate() {
            let top = if tri.len() == 3 {
                tri[2]
            } else {
                tri[3] * (1. - s) + tri[2] * s
            };
            let chart = (tri[0] * (1. - s) + tri[1] * s) * (1. - t) + top * t;
            let (u, v) = (chart.x, chart.y);
            let bernstein = [(1. - u) * (1. - u), 2. * u * (1. - u), u * u];
            let mut point = [0.; 4];
            for (b, hk) in bernstein.iter().zip(&homogeneous) {
                for d in 0..4 {
                    point[d] += b * hk[d];
                }
            }
            for d in 0..3 {
                point[d] += v * frame.axis[d] * point[3];
            }
            h[j * 4 + i] = point;
        }
    }
    let knots = uniform_knots(1, 4);
    let b = DMatrix::from_fn(4, 4, |r, c| basis(&knots, 4, g[r], false)[c]);
    let inverse = b
        .try_inverse()
        .ok_or_else(|| anyhow::anyhow!("singular cubic basis"))?;
    // coefficients[a][b] = sum_i sum_j inverse[a][i] h[i][j] inverse[b][j]  (a along t rows, b along s)
    let mut coefficients = vec![[0.; 4]; 16];
    for a in 0..4 {
        for bb in 0..4 {
            for i in 0..4 {
                for j in 0..4 {
                    let w = inverse[(a, i)] * inverse[(bb, j)];
                    for d in 0..4 {
                        coefficients[a * 4 + bb][d] += w * h[i * 4 + j][d];
                    }
                }
            }
        }
    }
    if tri.len() == 3 {
        for bb in 0..4 {
            coefficients[12 + bb] = coefficients[12]; // exactly one collapsed edge
        }
    }
    ensure!(
        coefficients.iter().all(|c| c[3] > 0.),
        "nonpositive cylinder-triangle weights"
    );
    Ok(Patch {
        u_order: 4,
        v_order: 4,
        u_knots: knots.clone(),
        v_knots: knots,
        coefficients: coefficients
            .iter()
            .map(|c| DVec3::new(c[0] / c[3], c[1] / c[3], c[2] / c[3]))
            .collect(),
        weights: Some(coefficients.iter().map(|c| c[3]).collect()),
    })
}

/// The outer boundary of a group of split quads in continuous angular/axial coordinates.
fn boundary(group: &[&QuadPatch], frame: &Frame) -> Result<Vec<DVec2>> {
    let mut count: HashMap<(usize, usize), usize> = HashMap::new();
    for q in group {
        let n = q.nodes.unwrap_or_default();
        for k in 0..4 {
            let (a, b) = (n[k], n[(k + 1) % 4]);
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let g = unit_grid(201);
    let mut edges: BTreeMap<usize, (usize, Vec<DVec3>)> = BTreeMap::new();
    for q in group {
        let n = q.nodes.unwrap_or_default();
        for k in 0..4 {
            let (a, b) = (n[k], n[(k + 1) % 4]);
            if count[&(a.min(b), a.max(b))] != 1 {
                continue;
            }
            let points: Vec<DVec3> = g
                .iter()
                .map(|&x| match k {
                    0 => q.patch.at(x, 0.),
                    1 => q.patch.at(1., x),
                    2 => q.patch.at(1. - x, 1.),
                    _ => q.patch.at(0., 1. - x),
                })
                .collect();
            edges.insert(a, (b, points));
        }
    }
    ensure!(
        edges.len() == count.values().filter(|&&c| c == 1).count(),
        "trim repair has a non-manifold boundary vertex"
    );
    let start = *edges
        .keys()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty trim region"))?;
    let (mut node, mut chain, mut visited): (usize, Vec<DVec3>, BTreeSet<usize>) =
        (start, Vec::new(), BTreeSet::new());
    for _ in 0..edges.len() {
        ensure!(
            edges.contains_key(&node) && visited.insert(node),
            "trim repair boundary has multiple loops"
        );
        let (next, points) = &edges[&node];
        chain.extend(&points[..points.len() - 1]);
        node = *next;
    }
    ensure!(node == start, "tri3 boundary does not close");
    let centre = (frame.lo[0] + frame.hi[0]) / 2.;
    let mut out: Vec<DVec2> = chain
        .iter()
        .map(|p| {
            let d = *p - frame.origin;
            let mut u = (d.dot(frame.ey) / frame.ey.dot(frame.ey))
                .atan2(d.dot(frame.ex) / frame.ex.dot(frame.ex));
            u += std::f64::consts::TAU
                * ((centre - u + std::f64::consts::PI) / std::f64::consts::TAU).floor();
            DVec2::new(u, d.dot(frame.axis) / frame.axis.dot(frame.axis))
        })
        .collect();
    // unwrap the angle
    for k in 1..out.len() {
        let jump = out[k].x - out[k - 1].x;
        out[k].x -=
            std::f64::consts::TAU * ((jump + std::f64::consts::PI) / std::f64::consts::TAU).floor();
    }
    Ok(out)
}

/// Replace a folded group with a boundary-controlled cylindrical triangulation of exact patches.
fn repair_group(
    gmsh: &Session,
    group: &[&QuadPatch],
    frame: &Frame,
    tolerance: f64,
) -> Result<Vec<QuadPatch>> {
    let polygon = boundary(group, frame)?;
    let radius = frame.ex.length();
    let low = polygon.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let high = polygon
        .iter()
        .map(|p| p.x)
        .fold(f64::NEG_INFINITY, f64::max);
    let cuts = linspace(
        low,
        high,
        (((high - low) / std::f64::consts::FRAC_PI_2).ceil() as usize).max(1) + 1,
    );
    let mut patches = Vec::new();
    for u in cuts.windows(2) {
        let (u0, u1) = (u[0], u[1]);
        let piece = clip(&clip(&polygon, u0, true), u1, false);
        if piece.len() < 3 || area(&piece).abs() < 1e-16 {
            continue;
        }
        let half = (u1 - u0) / 2.;
        let metric = 4. * radius * (half / 2.).tan();
        let chart: Vec<DVec2> = piece
            .iter()
            .map(|p| {
                let k = ((p.x - u0) / 2.).tan();
                let tau = k / (half.sin() + k * (1. - half.cos()));
                DVec2::new(tau * metric, p.y)
            })
            .collect();
        let chart = simplify_closed(&chart, tolerance);
        for cell in quad_cells(&quality_triangles(gmsh, &chart)?) {
            let cell: Vec<DVec2> = cell.iter().map(|c| DVec2::new(c.x / metric, c.y)).collect();
            let patch = cylinder_chart_patch(&cell, frame, u0, u1)?;
            ensure!(
                checks::jacobian(&patch, 21).1 == 0,
                "cylinder trim repair still folds"
            );
            patches.push(patch);
        }
    }
    ensure!(!patches.is_empty(), "cylinder trim repair produced no area");
    let g = unit_grid(21);
    let mut out = Vec::new();
    for patch in patches {
        let grid = patch.grid(&g, &g);
        let error = grid
            .iter()
            .map(|x| {
                let d = *x - frame.origin;
                let axial = d.dot(frame.axis) / frame.axis.dot(frame.axis);
                ((d - axial * frame.axis).length() - frame.ex.length()).abs()
            })
            .fold(0., f64::max);
        out.push(QuadPatch {
            samples: grid,
            n: 21,
            patch,
            sample_error: 0.,
            cad_error: error,
            exact: Some("rational"),
            nodes: None,
            ..group[0].clone()
        });
    }
    Ok(out)
}

/// Repair folded tri3 groups (three quads of one parent triangle) on the cylindrical faces with frames.
pub fn repair(
    gmsh: &Session,
    qp: Vec<QuadPatch>,
    frames: &BTreeMap<i32, Frame>,
    tolerance: f64,
) -> Vec<QuadPatch> {
    let mut by_face: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (i, q) in qp.iter().enumerate() {
        if frames.contains_key(&q.face) && q.path == "split" {
            by_face.entry(q.face).or_default().push(i);
        }
    }
    let mut replacements: BTreeMap<usize, Vec<QuadPatch>> = BTreeMap::new();
    let mut removed: BTreeSet<usize> = BTreeSet::new();
    let mut errors: BTreeMap<usize, String> = BTreeMap::new();
    for ids in by_face.values() {
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for members in ids.chunks(3) {
            if members.len() != 3 || members.iter().any(|&i| qp[i].nodes.is_none()) {
                continue;
            }
            let sets: Vec<BTreeSet<usize>> = members
                .iter()
                .map(|&i| qp[i].nodes.unwrap_or_default().into_iter().collect())
                .collect();
            let common: BTreeSet<usize> = sets[0].intersection(&sets[1]).copied().collect();
            if common.intersection(&sets[2]).count() != 1 {
                continue;
            }
            groups.push(members.to_vec());
        }
        let mut owners: HashMap<(usize, usize), BTreeSet<usize>> = HashMap::new();
        for (j, members) in groups.iter().enumerate() {
            for &i in members {
                let n = qp[i].nodes.unwrap_or_default();
                for k in 0..4 {
                    let (a, b) = (n[k], n[(k + 1) % 4]);
                    owners.entry((a.min(b), a.max(b))).or_default().insert(j);
                }
            }
        }
        let mut neighbours: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        for pair in owners.values() {
            if pair.len() == 2 {
                let v: Vec<usize> = pair.iter().copied().collect();
                neighbours.entry(v[0]).or_default().insert(v[1]);
                neighbours.entry(v[1]).or_default().insert(v[0]);
            }
        }
        let bad: Vec<usize> = (0..groups.len())
            .filter(|&j| {
                groups[j]
                    .iter()
                    .any(|&i| checks::jacobian(&qp[i].patch, 21).1 > 0)
            })
            .collect();
        let mut clusters: Vec<BTreeSet<usize>> = bad
            .iter()
            .map(|&j| {
                let mut c: BTreeSet<usize> = neighbours.get(&j).cloned().unwrap_or_default();
                c.insert(j);
                c
            })
            .collect();
        loop {
            let mut merged = false;
            'outer: for i in 0..clusters.len() {
                for j in i + 1..clusters.len() {
                    if !clusters[i].is_disjoint(&clusters[j]) {
                        let other = clusters.remove(j);
                        clusters[i].extend(other);
                        merged = true;
                        break 'outer;
                    }
                }
            }
            if !merged {
                break;
            }
        }
        for cluster in clusters {
            let mut members: Vec<usize> = cluster.iter().flat_map(|&j| groups[j].clone()).collect();
            members.sort_unstable();
            let group: Vec<&QuadPatch> = members.iter().map(|&i| &qp[i]).collect();
            match repair_group(gmsh, &group, &frames[&group[0].face], tolerance) {
                Ok(mut repaired) => {
                    for q in repaired.iter_mut() {
                        q.disjoint_group = members[0] as i64;
                    }
                    replacements.insert(members[0], repaired);
                    removed.extend(&members);
                }
                Err(error) => {
                    // Unsupported topology keeps its original failed checks, never a partial surface.
                    for &i in &members {
                        errors.insert(i, format!("{error:#}"));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for (i, mut q) in qp.into_iter().enumerate() {
        if removed.contains(&i) {
            if let Some(r) = replacements.remove(&i) {
                out.extend(r);
            }
        } else {
            q.repair_error = errors.remove(&i);
            out.push(q);
        }
    }
    out
}

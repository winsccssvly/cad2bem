// SPDX-License-Identifier: GPL-3.0-or-later
//! The interior free surface: the z = 0 section of the solids, or the waterlines closed along the symmetry
//! planes.

use super::*;

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
pub(super) fn waterplane_from_waterlines(
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
pub(super) fn origin_point(gmsh: &Session, tol: f64) -> Result<i32> {
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
pub(super) fn curve_ends(gmsh: &Session, c: i32) -> Result<Vec<i32>> {
    Ok(gmsh
        .boundary(&[(1, c)], true, false, false)?
        .into_iter()
        .map(|(_, p)| p.abs())
        .collect())
}

/// The closed loops of curves chained by their end points; empty when a chain does not close.
pub(super) fn waterline_loops(gmsh: &Session, curves: &[i32]) -> Result<Vec<Vec<i32>>> {
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
pub(super) fn loop_points(gmsh: &Session, lp: &[i32]) -> Result<Vec<[f64; 2]>> {
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
pub(super) fn loop_area(points: &[[f64; 2]]) -> f64 {
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
pub(super) fn inside(a: &[[f64; 2]], b: &[[f64; 2]]) -> bool {
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

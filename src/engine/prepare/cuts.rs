// SPDX-License-Identifier: GPL-3.0-or-later
//! Face cuts: planar rectangles that fragment the faces they cross, optionally only those on one surface.

use super::*;

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
pub(super) fn on_surface(gmsh: &Session, face: i32, surface: &Surface, tol: f64) -> Result<bool> {
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
pub(super) fn rectangle(
    gmsh: &Session,
    centre: DVec3,
    n: DVec3,
    u: DVec3,
    half: [f64; 2],
) -> Result<i32> {
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

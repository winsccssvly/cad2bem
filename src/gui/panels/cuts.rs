// SPDX-License-Identifier: GPL-3.0-or-later
//! Cut placement: the cut modes and scopes, and the planar cuts they make from picked points, faces and
//! cylinders.

use super::*;

/// The corners of a cut's rectangle (m).
#[cfg(test)]
pub(super) fn cut_corners(cut: &Cut) -> [DVec3; 4] {
    let n = DVec3::from(cut.normal).normalize();
    let u = DVec3::from(cut.axis).normalize();
    let v = n.cross(u);
    let c = DVec3::from(cut.centre);
    [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
        .map(|(a, b)| c + a * cut.half[0] * u + b * cut.half[1] * v)
}

/// Which faces a cut of the cut mode splits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum CutScope {
    /// The face under the mouse.
    Face,
    /// The faces on its plane or cylinder.
    Surface,
    /// Every face the cut passes.
    All,
}

/// How the cut line of the cut mode runs on the face under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum CutMode {
    /// Around a cylinder (across its axis); horizontal on a wall, along y on a deck.
    Across,
    /// Along a cylinder's axis (a half plane from the axis); vertical on a wall, along x on a deck.
    Along,
    /// Through two clicked points, square to the face at the first.
    Points,
    /// Parallel to a reference face clicked first (its plane, or the tangent plane where a cylinder was
    /// clicked), through the point clicked on the face to cut.
    Parallel,
}

/// The cut through `point` (m) of a face on `surface` (or of unknown surface), `extent` its points (m),
/// `size` the model size; limited to the surface when `on`.
pub(super) fn make_cut(
    mode: CutMode,
    surface: Option<&Surface>,
    point: DVec3,
    extent: &[DVec3],
    size: f64,
    on: bool,
) -> Option<Cut> {
    let make = |normal: DVec3, axis: DVec3, centre: DVec3, half: [f64; 2]| Cut {
        centre: centre.to_array(),
        normal: normal.to_array(),
        axis: axis.to_array(),
        half,
        on: surface.filter(|_| on).cloned(),
        face_at: None,
    };
    // A plane through the point containing the line direction `d` and the face normal `n`: as large as the
    // face (a little more) when the cut keeps to its surface, otherwise across the model.
    let line = |d: DVec3, n: DVec3| {
        let normal = d.cross(n).try_normalize()?;
        let other = normal.cross(d);
        let reach = |e: DVec3| {
            extent
                .iter()
                .map(|x| (*x - point).dot(e).abs())
                .fold(0., f64::max)
                * 1.05
                + 1e-3 * size
        };
        let half = if on && surface.is_some() && !extent.is_empty() {
            [reach(d), reach(other)]
        } else {
            [size, size]
        };
        Some(make(normal, d, point, half))
    };
    match surface {
        Some(Surface::Cylinder {
            point: p0,
            axis,
            radius,
        }) => {
            let a = DVec3::from(*axis).normalize();
            let base = DVec3::from(*p0);
            match mode {
                CutMode::Across => {
                    let centre = base + (point - base).dot(a) * a;
                    let u = a.any_orthonormal_vector();
                    Some(make(a, u, centre, [1.5 * radius, 1.5 * radius]))
                }
                CutMode::Along => {
                    let off = point - base;
                    let r = (off - off.dot(a) * a).try_normalize()?;
                    // Over the face's length along the axis, so that the cut ends where the face ends.
                    let (lo, hi) = extent
                        .iter()
                        .map(|x| (*x - base).dot(a))
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), t| {
                            (l.min(t), h.max(t))
                        });
                    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
                        return None;
                    }
                    let centre = base + 0.5 * (lo + hi) * a + 0.75 * radius * r;
                    Some(make(
                        a.cross(r),
                        a,
                        centre,
                        [0.5 * (hi - lo), 0.75 * radius],
                    ))
                }
                CutMode::Points | CutMode::Parallel => None,
            }
        }
        Some(Surface::Plane { normal, .. }) => {
            let n = DVec3::from(*normal).normalize();
            // On a wall "along" is vertical; on a deck, along x.
            let up = if n.z.abs() < 0.9 { DVec3::Z } else { DVec3::X };
            let along = (up - up.dot(n) * n).try_normalize()?;
            let across = n.cross(along);
            line(
                if mode == CutMode::Along {
                    along
                } else {
                    across
                },
                n,
            )
        }
        None => match mode {
            CutMode::Across => Some(make(DVec3::Z, DVec3::X, point, [size, size])),
            CutMode::Along => Some(make(DVec3::X, DVec3::Y, point, [size, size])),
            CutMode::Points | CutMode::Parallel => None,
        },
    }
}

/// The normal of a plane, or of a cylinder at `at` (radial).
pub(super) fn surface_normal(surface: &Surface, at: DVec3) -> Option<DVec3> {
    match surface {
        Surface::Plane { normal, .. } => DVec3::from(*normal).try_normalize(),
        Surface::Cylinder { point, axis, .. } => {
            let axis = DVec3::from(*axis).try_normalize()?;
            let off = at - DVec3::from(*point);
            (off - off.dot(axis) * axis).try_normalize()
        }
    }
}

/// The cut with normal `n` (parallel to a reference face) through `point` (m) of a face on `surface`: over
/// that face (a little more, `extent` its points) when kept to its surface (`on`), otherwise across the model.
pub(super) fn parallel_cut(
    n: DVec3,
    point: DVec3,
    surface: Option<&Surface>,
    extent: &[DVec3],
    size: f64,
    on: bool,
) -> Option<Cut> {
    let n = n.try_normalize()?;
    let u = n.any_orthonormal_vector();
    let v = n.cross(u);
    let reach = |e: DVec3| {
        extent
            .iter()
            .map(|x| (*x - point).dot(e).abs())
            .fold(0., f64::max)
            * 1.05
            + 1e-3 * size
    };
    let half = if on && !extent.is_empty() {
        [reach(u), reach(v)]
    } else {
        [size, size]
    };
    Some(Cut {
        centre: point.to_array(),
        normal: n.to_array(),
        axis: u.to_array(),
        half,
        on: surface.filter(|_| on).cloned(),
        face_at: None,
    })
}

/// The cut through `a` and `b` (m), square to the face at `a` (on `surface`, or to the view direction `eye`
/// when the surface is not known): over that face (a little more) when kept to its surface (`on`, `extent` its
/// points), otherwise across the model (`size`).
pub(super) fn points_cut(
    a: DVec3,
    b: DVec3,
    surface: Option<&Surface>,
    extent: &[DVec3],
    size: f64,
    on: bool,
    eye: DVec3,
) -> Option<Cut> {
    let n = match surface {
        Some(Surface::Plane { normal, .. }) => DVec3::from(*normal).try_normalize()?,
        Some(Surface::Cylinder { point, axis, .. }) => {
            let axis = DVec3::from(*axis).try_normalize()?;
            let off = a - DVec3::from(*point);
            (off - off.dot(axis) * axis).try_normalize()?
        }
        None => eye.try_normalize()?,
    };
    let chord = b - a;
    let d = (chord - chord.dot(n) * n).try_normalize()?;
    let normal = d.cross(n).try_normalize()?;
    let other = normal.cross(d);
    let reach = |e: DVec3| {
        extent
            .iter()
            .map(|x| (*x - a).dot(e).abs())
            .fold(0., f64::max)
            * 1.05
            + 1e-3 * size
    };
    let half = if on && surface.is_some() && !extent.is_empty() {
        [reach(d), reach(other)]
    } else {
        [size, size]
    };
    Some(Cut {
        centre: a.to_array(),
        normal: normal.to_array(),
        axis: d.to_array(),
        half,
        on: surface.filter(|_| on).cloned(),
        face_at: None,
    })
}

/// The point where the plane through `c` with unit normal `n` meets a cylinder, near `h`, a crossing of the plane
/// with a flat display triangle on that cylinder. Flat triangles cut at a grazing angle (a plane nearly tangent to
/// the cylinder) put their crossings far off the true curve, so the line zigzags: the point is taken on the
/// cylinder instead, at the height of `h` where the curve runs along the axis, at its angle where it runs around.
/// `h` itself when the plane does not reach the cylinder there.
pub(super) fn on_cylinder(
    h: DVec3,
    c: DVec3,
    n: DVec3,
    point: DVec3,
    axis: DVec3,
    radius: f64,
) -> DVec3 {
    use std::f64::consts::{PI, TAU};
    let Some(a) = axis.try_normalize() else {
        return h;
    };
    let e1 = a.any_orthonormal_vector();
    let e2 = a.cross(e1);
    let d = h - point;
    let t = d.dot(a);
    let theta = d.dot(e2).atan2(d.dot(e1));
    let at = |theta: f64, t: f64| point + t * a + radius * (theta.cos() * e1 + theta.sin() * e2);
    // The plane at (θ, t): k + t (a·n) + A cos θ + B sin θ = 0.
    let k = (point - c).dot(n);
    let (an, big_a, big_b) = (a.dot(n), radius * e1.dot(n), radius * e2.dot(n));
    let radial = theta.cos() * e1 + theta.sin() * e2;
    let tangent = n.cross(radial);
    if tangent.dot(a).abs() >= tangent.dot(a.cross(radial)).abs() {
        // Along the axis: keep the height, solve the angle; of the two, the one nearer to h.
        let r = big_a.hypot(big_b);
        let rhs = -(k + t * an);
        if r < 1e-12 || rhs.abs() > r {
            return h;
        }
        let phi = big_b.atan2(big_a);
        let spread = (rhs / r).acos();
        let off = |x: f64| ((x - theta + PI).rem_euclid(TAU) - PI).abs();
        let best = if off(phi + spread) <= off(phi - spread) {
            phi + spread
        } else {
            phi - spread
        };
        at(best, t)
    } else {
        // Around the axis: keep the angle, solve the height.
        if an.abs() < 1e-12 {
            return h;
        }
        at(theta, -(k + big_a * theta.cos() + big_b * theta.sin()) / an)
    }
}

/// Where a cut crosses the display triangles of `scene` (m): segments inside the cut's rectangle, on the
/// triangles that lie on the cut's surface (or on every triangle without one), and only on `face` when given.
/// On a cylinder face the crossings lie on the exact curve ([`on_cylinder`]).
pub(super) fn cut_segments(
    cut: &Cut,
    scene: &cad2bem::cad::scene::Scene,
    face: Option<i32>,
) -> Vec<[DVec3; 2]> {
    let n = DVec3::from(cut.normal).normalize();
    let u = DVec3::from(cut.axis).normalize();
    let v = n.cross(u);
    let c = DVec3::from(cut.centre);
    let mut cylinders = std::collections::HashMap::new();
    for f in &scene.faces {
        if let Some(Surface::Cylinder {
            point,
            axis,
            radius,
        }) = &f.surface
        {
            cylinders.insert(f.tag, (DVec3::from(*point), DVec3::from(*axis), *radius));
        }
    }
    let size = {
        let b = scene.bounds_m;
        (b[3] - b[0]).max(b[4] - b[1]).max(b[5] - b[2])
    };
    let on_surface = |p: DVec3| {
        cut.on
            .as_ref()
            .is_none_or(|s| s.distance(p.to_array()) <= 2e-3 * size)
    };
    let inside = |p: DVec3| {
        let d = p - c;
        d.dot(u).abs() <= cut.half[0] * 1.0001 && d.dot(v).abs() <= cut.half[1] * 1.0001
    };
    let mut out = Vec::new();
    for (t, &tag) in scene.triangles.iter().zip(&scene.face_tags) {
        if face.is_some_and(|f| f != tag) {
            continue;
        }
        let p = t.map(|i| DVec3::from(scene.positions_m[i as usize]));
        let s = p.map(|x| (x - c).dot(n));
        let mut hits = Vec::new();
        for k in 0..3 {
            let (a, b) = (k, (k + 1) % 3);
            if (s[a] < 0.) != (s[b] < 0.) {
                let w = s[a] / (s[a] - s[b]);
                hits.push(p[a] + w * (p[b] - p[a]));
            }
        }
        if let Some(&(point, axis, radius)) = cylinders.get(&tag) {
            for h in &mut hits {
                *h = on_cylinder(*h, c, n, point, axis, radius);
            }
        }
        if hits.len() == 2 && p.iter().all(|&x| on_surface(x)) && hits.iter().all(|&h| inside(h)) {
            out.push([hits[0], hits[1]]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn a_parallel_cut_keeps_the_reference_normal_and_passes_the_point() {
        // Reference: a column wall at angle 0.3 about z (its radial normal there); cut a deck z = -2 face.
        let column = Surface::Cylinder {
            point: [0.; 3],
            axis: [0., 0., 1.],
            radius: 6.,
        };
        let n = surface_normal(
            &column,
            DVec3::new(6. * 0.3_f64.cos(), 6. * 0.3_f64.sin(), -5.),
        )
        .unwrap();
        assert!((n - DVec3::new(0.3_f64.cos(), 0.3_f64.sin(), 0.)).length() < 1e-12);
        let deck = Surface::Plane {
            point: [0., 0., -2.],
            normal: [0., 0., 1.],
        };
        let p = DVec3::new(10., 3., -2.);
        let extent = [DVec3::new(8., 0., -2.), DVec3::new(14., 6., -2.)];
        let cut = parallel_cut(n, p, Some(&deck), &extent, 100., true).unwrap();
        assert!((DVec3::from(cut.normal) - n).length() < 1e-12);
        assert_eq!(cut.centre, p.to_array());
        assert_eq!(cut.on, Some(deck));
        // Over the face: the rectangle reaches every point of it along its axis.
        let u = DVec3::from(cut.axis);
        assert!(extent.iter().all(|x| (*x - p).dot(u).abs() <= cut.half[0]));
        assert!(cut.half[0] < 10., "over the face only: {:?}", cut.half);
    }

    #[test]
    fn a_two_point_cut_passes_both_points_square_to_the_face() {
        let on_plane = |cut: &Cut, p: DVec3| {
            (p - DVec3::from(cut.centre))
                .dot(DVec3::from(cut.normal))
                .abs()
                < 1e-9
        };
        // A wall x = 0: a slanted line between two of its points.
        let wall = Surface::Plane {
            point: [0.; 3],
            normal: [1., 0., 0.],
        };
        let (a, b) = (DVec3::new(0., 1., -2.), DVec3::new(0., 4., -6.));
        let extent = [DVec3::new(0., 0., 0.), DVec3::new(0., 10., -10.)];
        let cut = points_cut(a, b, Some(&wall), &extent, 100., true, DVec3::X).unwrap();
        assert!(on_plane(&cut, a) && on_plane(&cut, b));
        assert!(
            DVec3::from(cut.normal).dot(DVec3::X).abs() < 1e-12,
            "square to the wall"
        );
        // A cylinder about z of radius 3: the plane holds both points and the radial direction at the first.
        let column = Surface::Cylinder {
            point: [0.; 3],
            axis: [0., 0., 1.],
            radius: 3.,
        };
        let a = DVec3::new(3., 0., -1.);
        let b = DVec3::new(3. * 0.5_f64.cos(), 3. * 0.5_f64.sin(), -4.);
        let cut = points_cut(a, b, Some(&column), &[a, b], 100., true, DVec3::X).unwrap();
        assert!(on_plane(&cut, a) && on_plane(&cut, b));
        assert!(DVec3::from(cut.normal).dot(DVec3::X).abs() < 1e-12);
    }

    #[test]
    fn cut_modes_go_around_and_along_a_cylinder_and_across_a_wall() {
        let column = Surface::Cylinder {
            point: [0., 0., 0.],
            axis: [0., 0., 1.],
            radius: 3.,
        };
        let extent = [DVec3::new(3., 0., -10.), DVec3::new(0., 3., 0.)];
        let point = DVec3::new(3., 0., -5.);
        let across = make_cut(CutMode::Across, Some(&column), point, &extent, 100., true).unwrap();
        assert!(close(across.centre, [0., 0., -5.]) && close(across.normal, [0., 0., 1.]));
        assert_eq!(across.on, Some(column.clone()));
        let along = make_cut(CutMode::Along, Some(&column), point, &extent, 100., true).unwrap();
        assert!(close(along.normal, [0., 1., 0.]));
        assert!(close(along.centre, [2.25, 0., -5.]));
        assert_eq!(along.half, [5., 2.25]);
        assert!(
            cut_corners(&along).iter().all(|c| c.x > -1e-9),
            "from the axis outward"
        );
        // A wall in the plane y = 0: across is horizontal (z = -2), along is vertical (x = 4).
        let wall = Surface::Plane {
            point: [0., 0., 0.],
            normal: [0., 1., 0.],
        };
        let corners = [DVec3::new(0., 0., -10.), DVec3::new(10., 0., 0.)];
        let p = DVec3::new(4., 0., -2.);
        let h = make_cut(CutMode::Across, Some(&wall), p, &corners, 100., true).unwrap();
        assert!(
            DVec3::from(h.normal).cross(DVec3::Z).length() < 1e-9,
            "{:?}",
            h.normal
        );
        let v = make_cut(CutMode::Along, Some(&wall), p, &corners, 100., true).unwrap();
        assert!(
            DVec3::from(v.normal).cross(DVec3::X).length() < 1e-9,
            "{:?}",
            v.normal
        );
        assert!(h.half[0] < 20. && h.half[1] < 20.);
    }

    #[test]
    fn cut_line_on_a_cylinder_follows_the_exact_curve() {
        // A unit cylinder about z; planes from nearly tangent (a grazing cut) to steep, through crossings off the curve.
        let (point, axis, radius) = (DVec3::ZERO, DVec3::Z, 1.);
        for (c, n) in [
            (DVec3::new(0.995, 0., 0.), DVec3::new(1., 0., 0.02)),
            (DVec3::new(0.9, 0., 0.), DVec3::new(1., 0.3, 0.)),
            (DVec3::new(0., 0., 0.4), DVec3::new(0.2, 0., 1.)),
        ] {
            let n = n.normalize();
            // Heights where every plane meets the cylinder (the grazing one leaves it below z = -0.25).
            for z in [-0.2, 0., 0.7] {
                // A point near the curve, as a flat triangle gives it: inside the cylinder, near the plane.
                let guess = DVec3::new(0.97, 0.12, z);
                let guess = guess - (guess - c).dot(n) * n;
                let p = on_cylinder(guess, c, n, point, axis, radius);
                assert!((p - c).dot(n).abs() < 1e-9, "off the plane: {p}");
                assert!(
                    (p.truncate().length() - 1.).abs() < 1e-9,
                    "off the cylinder: {p}"
                );
                assert!(
                    p.distance(guess) < 0.5,
                    "far from the crossing: {p} {guess}"
                );
            }
        }
    }
}

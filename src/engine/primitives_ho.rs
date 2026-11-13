// SPDX-License-Identifier: GPL-3.0-or-later
//! Bodies made from their dimensions as exact spline patches (high order): the shapes of `primitives` as
//! rational quadratic pieces, written as HBG `spline_patches`.
//!
//! Walls of revolution are ruled between two circular arcs, a disc is a square at its centre and two ring
//! pieces in each 45 degrees between the square's side and the rim, an annulus is ruled between its two
//! circles, a sphere is a quarter meridian turned about the axis (its pole a collapsed edge, as in the ho
//! conversion of a CAD sphere) and the faces of a box are flat. Every piece spans 45 degrees of the stored
//! part, so the pieces of a wall and of its disc meet at their corners. Normals x_u x x_v point into the body
//! (the HBG convention for patches). Nothing is approximated: the volume of the patches is the exact one.

use super::{
    bspline::Patch,
    exact::{Conic, revolved, ruled},
    ho::Splines,
    primitives::{Request, Shape, azimuths},
};
use crate::conversion::Symmetry;
use anyhow::Result;
use glam::DVec3;
use std::f64::consts::FRAC_PI_4;

/// A body made of spline patches: the body, the interior surface (in z = 0), and the exact volume and
/// waterplane of the whole body.
pub struct MadePatches {
    pub body: Splines,
    pub interior: Option<Splines>,
    pub volume_exact: f64,
    pub waterplane_exact: Option<f64>,
}

impl MadePatches {
    /// The patches of the file: the body, and the interior surface after it (in z = 0, where the solver's
    /// interior surface "detect" finds it).
    pub fn merged(&self) -> Splines {
        let mut out = Splines {
            patches: self.body.patches.clone(),
            names: self.body.names.clone(),
            symmetry: self.body.symmetry,
            label: self.body.label.clone(),
            panel_size: self.body.panel_size,
        };
        if let Some(interior) = &self.interior {
            out.patches.extend(interior.patches.iter().cloned());
            out.names.extend(interior.names.iter().cloned());
        }
        out
    }
}

fn point(r: f64, phi: f64, z: f64) -> DVec3 {
    DVec3::new(r * phi.cos(), r * phi.sin(), z)
}

/// The circular arc of radius `r` about the z axis at height `z`, from azimuth `a` to `b` (at most 120 degrees).
fn arc(r: f64, a: f64, b: f64, z: f64) -> Conic {
    let half = (b - a) / 2.;
    Conic {
        points: [
            point(r, a, z),
            point(r / half.cos(), a + half, z),
            point(r, b, z),
        ],
        weights: [1., half.cos(), 1.],
    }
}

/// The straight segment from `a` to `b` as a rational quadratic.
fn segment(a: DVec3, b: DVec3) -> Conic {
    Conic {
        points: [a, (a + b) / 2., b],
        weights: [1.; 3],
    }
}

/// The patch with its normal turned along `inward` (the direction into the body at its centre).
fn oriented(patch: Patch, inward: DVec3) -> Patch {
    let [u0, u1, v0, v1] = patch.range();
    let (_, xu, xv) = patch.frame((u0 + u1) / 2., (v0 + v1) / 2.);
    if xu.cross(xv).dot(inward) < 0. {
        patch.swapped()
    } else {
        patch
    }
}

/// The patch's centre.
fn centre(patch: &Patch) -> DVec3 {
    let [u0, u1, v0, v1] = patch.range();
    patch.at((u0 + u1) / 2., (v0 + v1) / 2.)
}

/// The 45-degree pieces of the stored azimuths: (start, end).
fn pieces(symmetry: Symmetry) -> Vec<(f64, f64)> {
    let (start, span, _) = azimuths(symmetry);
    let n = (span / FRAC_PI_4).round() as usize;
    (0..n)
        .map(|k| {
            (
                start + FRAC_PI_4 * k as f64,
                start + FRAC_PI_4 * (k + 1) as f64,
            )
        })
        .collect()
}

/// A disc (inner radius 0) or an annulus at height `z`, piece by piece; the normals along `inward`.
fn disc(radius: f64, inner: f64, z: f64, symmetry: Symmetry, inward: DVec3) -> Vec<Patch> {
    let mut out = Vec::new();
    let c = radius / 2.;
    for (k, (a, b)) in pieces(symmetry).into_iter().enumerate() {
        let rim = arc(radius, a, b, z);
        if inner > 0. {
            out.push(oriented(ruled(&arc(inner, a, b, z), &rim), inward));
            continue;
        }
        // The square at the centre has its corners on the axes and on the diagonals of each quadrant: a piece
        // starting on an axis (k even) runs from the axis corner to the diagonal corner, the next one back.
        let axis_corner = |phi: f64| point(c, phi, z);
        let diagonal_corner = |phi: f64| point(c * std::f64::consts::SQRT_2, phi, z);
        let (p, q) = if k % 2 == 0 {
            (axis_corner(a), diagonal_corner(b))
        } else {
            (diagonal_corner(a), axis_corner(b))
        };
        out.push(oriented(ruled(&segment(p, q), &rim), inward));
        // One square per quadrant, with the piece that starts on its axis.
        if k % 2 == 0 {
            let o = DVec3::new(0., 0., z);
            let far = axis_corner(a + 2. * (b - a));
            out.push(oriented(ruled(&segment(o, p), &segment(far, q)), inward));
        }
    }
    out
}

/// A vertical wall of radius `radius` from z = 0 down to `-draft`, piece by piece; the body lies towards the axis
/// (an outer wall) or away from it (a moonpool wall).
fn wall(radius: f64, draft: f64, symmetry: Symmetry, towards_axis: bool) -> Vec<Patch> {
    pieces(symmetry)
        .into_iter()
        .map(|(a, b)| {
            let patch = ruled(&arc(radius, a, b, 0.), &arc(radius, a, b, -draft));
            let c = centre(&patch);
            let radial = DVec3::new(c.x, c.y, 0.);
            oriented(patch, if towards_axis { -radial } else { radial })
        })
        .collect()
}

/// The part of a sphere of centre (0, 0, `z`) between the polar angles `from` and `to` (from the south pole,
/// at most 90 degrees apart), piece by piece.
fn sphere(radius: f64, z: f64, from: f64, to: f64, symmetry: Symmetry) -> Vec<Patch> {
    let origin = DVec3::new(0., 0., z);
    let meridian =
        |theta: f64, phi: f64| point(radius * theta.sin(), phi, z - radius * theta.cos());
    pieces(symmetry)
        .into_iter()
        .map(|(a, b)| {
            // The profile arc in the meridian plane of azimuth a, about the centre.
            let half = (to - from) / 2.;
            let profile = Conic {
                points: [
                    meridian(from, a),
                    origin + (meridian(from + half, a) - origin) / half.cos(),
                    meridian(to, a),
                ],
                weights: [1., half.cos(), 1.],
            };
            let patch = revolved(&profile, false, origin, DVec3::Z, b - a);
            let c = centre(&patch);
            oriented(patch, origin - c)
        })
        .collect()
}

/// A flat rectangle with the corners `p`, `p + u`, `p + u + v`, `p + v`.
fn rectangle(p: DVec3, u: DVec3, v: DVec3, inward: DVec3) -> Patch {
    oriented(
        ruled(&segment(p, p + u), &segment(p + v, p + u + v)),
        inward,
    )
}

/// Named patches: `<name>_<k>`.
fn named(name: &str, patches: Vec<Patch>, out: &mut (Vec<Patch>, Vec<String>)) {
    for (k, patch) in patches.into_iter().enumerate() {
        out.1.push(format!("{name}_{}", k + 1));
        out.0.push(patch);
    }
}

/// Make the spline patches of a body; the request's panel size is the potential panel size written to the file.
pub fn make(request: &Request) -> Result<MadePatches> {
    let shape = request.shape;
    shape.check()?;
    let h = request.panel_size;
    anyhow::ensure!(h.is_finite() && h > 0., "The panel size must be positive");
    let symmetry = request.symmetry;
    let mut body = (Vec::new(), Vec::new());
    let mut interior = Vec::new();
    match shape {
        Shape::Cylinder { radius, draft } => {
            named("side", wall(radius, draft, symmetry, true), &mut body);
            named(
                "bottom",
                disc(radius, 0., -draft, symmetry, DVec3::Z),
                &mut body,
            );
            interior = disc(radius, 0., 0., symmetry, DVec3::NEG_Z);
        }
        Shape::Moonpool {
            radius,
            inner_radius,
            draft,
        } => {
            named("outer_side", wall(radius, draft, symmetry, true), &mut body);
            named(
                "moonpool_wall",
                wall(inner_radius, draft, symmetry, false),
                &mut body,
            );
            named(
                "bottom",
                disc(radius, inner_radius, -draft, symmetry, DVec3::Z),
                &mut body,
            );
            interior = disc(radius, inner_radius, 0., symmetry, DVec3::NEG_Z);
        }
        Shape::Sphere { radius, depth } => {
            let half = std::f64::consts::FRAC_PI_2;
            named(
                "lower",
                sphere(radius, -depth, 0., half, symmetry),
                &mut body,
            );
            named(
                "upper",
                sphere(radius, -depth, half, 2. * half, symmetry),
                &mut body,
            );
        }
        Shape::Hemisphere { radius } => {
            let half = std::f64::consts::FRAC_PI_2;
            named("hull", sphere(radius, 0., 0., half, symmetry), &mut body);
            interior = disc(radius, 0., 0., symmetry, DVec3::NEG_Z);
        }
        Shape::Barge {
            length,
            beam,
            draft,
        } => {
            let x0 = if symmetry.axes().contains(&0) {
                0.
            } else {
                -length / 2.
            };
            let y0 = if symmetry.axes().contains(&1) {
                0.
            } else {
                -beam / 2.
            };
            let (x1, y1) = (length / 2., beam / 2.);
            let (dx, dy) = (DVec3::X * (x1 - x0), DVec3::Y * (y1 - y0));
            let dz = DVec3::Z * draft;
            let corner = DVec3::new(x0, y0, -draft);
            named(
                "bottom",
                vec![rectangle(corner, dx, dy, DVec3::Z)],
                &mut body,
            );
            let mut sides = vec![
                rectangle(DVec3::new(x1, y0, -draft), dy, dz, DVec3::NEG_X),
                rectangle(DVec3::new(x0, y1, -draft), dx, dz, DVec3::NEG_Y),
            ];
            if x0 < 0. {
                sides.push(rectangle(corner, dy, dz, DVec3::X));
            }
            if y0 < 0. {
                sides.push(rectangle(corner, dx, dz, DVec3::Y));
            }
            named("side", sides, &mut body);
            interior = vec![rectangle(DVec3::new(x0, y0, 0.), dx, dy, DVec3::NEG_Z)];
        }
    }
    let label = shape.name().to_lowercase();
    let splines = |patches: Vec<Patch>, names: Vec<String>, label: String| Splines {
        patches,
        names,
        symmetry,
        label,
        panel_size: h,
    };
    let interior = (request.interior && !interior.is_empty()).then(|| {
        let names = (1..=interior.len())
            .map(|k| format!("waterplane_{k}"))
            .collect();
        splines(interior, names, format!("{label} interior"))
    });
    Ok(MadePatches {
        body: splines(body.0, body.1, label),
        interior,
        volume_exact: shape.volume(),
        waterplane_exact: shape.waterplane(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::checks;

    #[test]
    fn every_shape_has_its_exact_volume_and_interior_in_z0() {
        for shape in Shape::ALL {
            for symmetry in Symmetry::ALL {
                let request = Request {
                    shape,
                    panel_size: 2.,
                    symmetry,
                    interior: true,
                    high_order: true,
                    triangles: false,
                };
                let made = make(&request).unwrap();
                let name = format!("{} {}", shape.name(), symmetry.name());
                let v = checks::volumes(&made.body.patches, symmetry);
                for (k, v) in v.iter().enumerate() {
                    assert!(
                        (v / made.volume_exact - 1.).abs() < 1e-9,
                        "{name}: volume {k} {v}, exact {}",
                        made.volume_exact
                    );
                }
                match (&made.interior, made.waterplane_exact) {
                    (Some(interior), Some(waterplane)) => {
                        let area: f64 = interior.patches.iter().map(checks::area).sum::<f64>()
                            * symmetry.images();
                        assert!((area / waterplane - 1.).abs() < 1e-9, "{name}: area {area}");
                        for patch in &interior.patches {
                            assert!(patch.coefficients.iter().all(|c| c.z == 0.), "{name}");
                            // Into the body: down.
                            assert!(checks::center_normal(patch).1.z < 0., "{name}");
                        }
                    }
                    (None, None) => {}
                    _ => panic!("{name}: an interior for a floating body only"),
                }
            }
        }
    }
}

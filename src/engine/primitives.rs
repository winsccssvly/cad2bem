// SPDX-License-Identifier: GPL-3.0-or-later
//! Bodies made from their dimensions alone, without CAD: low-order panels of a vertical cylinder, a cylinder with
//! a moonpool, a submerged sphere, a floating hemisphere and a box barge.
//!
//! The panels cover the wetted surface below z = 0 (the stored part of the symmetry: x ≥ 0 for the plane x = 0,
//! y ≥ 0 for y = 0), on grids of the shape's own coordinates: around and down a wall, rings and rays on a disc
//! (triangles at its centre), latitude and longitude on a sphere (triangles at the poles), rows and columns on a
//! box. Normals point out of the body, into the fluid. The interior surface (the waterplane inside the
//! waterline, normals up) uses the body's waterline nodes. Nothing is approximated but the flat panels: the
//! exact volume is known and compared.

use super::panels::{Panel, Panels, normal, reversed};
use crate::conversion::Symmetry;
use anyhow::{Result, ensure};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::f64::consts::{FRAC_PI_2, PI, TAU};

/// The most panels a body is made of (it is made again at every change of its dimensions).
pub const MAX_PANELS: usize = 200_000;

/// A body by its dimensions (m).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum Shape {
    /// A vertical circular cylinder, floating.
    Cylinder { radius: f64, draft: f64 },
    /// A vertical cylinder with a concentric moonpool (a hole of `inner_radius` through it), floating.
    Moonpool {
        radius: f64,
        inner_radius: f64,
        draft: f64,
    },
    /// A sphere wholly under water, its centre at `depth` below z = 0.
    Sphere { radius: f64, depth: f64 },
    /// The lower half of a sphere, floating with its centre at z = 0.
    Hemisphere { radius: f64 },
    /// A rectangular box, floating.
    Barge { length: f64, beam: f64, draft: f64 },
}

impl Shape {
    /// The shapes with typical dimensions, in the order offered.
    pub const ALL: [Shape; 5] = [
        Shape::Cylinder {
            radius: 10.,
            draft: 20.,
        },
        Shape::Moonpool {
            radius: 10.,
            inner_radius: 4.,
            draft: 12.,
        },
        Shape::Sphere {
            radius: 10.,
            depth: 20.,
        },
        Shape::Hemisphere { radius: 10. },
        Shape::Barge {
            length: 60.,
            beam: 20.,
            draft: 10.,
        },
    ];

    /// The name of the shape.
    pub fn name(&self) -> &'static str {
        match self {
            Shape::Cylinder { .. } => "Cylinder",
            Shape::Moonpool { .. } => "Cylinder with moonpool",
            Shape::Sphere { .. } => "Sphere (submerged)",
            Shape::Hemisphere { .. } => "Hemisphere",
            Shape::Barge { .. } => "Barge (box)",
        }
    }

    /// Whether the shape is the same kind as `other` (whatever its dimensions).
    pub fn same_kind(&self, other: &Shape) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// The displaced volume of the whole body (m³).
    pub fn volume(&self) -> f64 {
        match *self {
            Shape::Cylinder { radius, draft } => PI * radius * radius * draft,
            Shape::Moonpool {
                radius,
                inner_radius,
                draft,
            } => PI * (radius * radius - inner_radius * inner_radius) * draft,
            Shape::Sphere { radius, .. } => 4. / 3. * PI * radius.powi(3),
            Shape::Hemisphere { radius } => 2. / 3. * PI * radius.powi(3),
            Shape::Barge {
                length,
                beam,
                draft,
            } => length * beam * draft,
        }
    }

    /// The wetted area of the whole body with its waterplane (m²): what the panels cover.
    fn area(&self) -> f64 {
        let waterplane = self.waterplane().unwrap_or(0.);
        waterplane
            + match *self {
                Shape::Cylinder { radius, draft } => TAU * radius * draft + PI * radius * radius,
                Shape::Moonpool {
                    radius,
                    inner_radius,
                    draft,
                } => {
                    TAU * (radius + inner_radius) * draft
                        + PI * (radius * radius - inner_radius * inner_radius)
                }
                Shape::Sphere { radius, .. } => 4. * PI * radius * radius,
                Shape::Hemisphere { radius } => TAU * radius * radius,
                Shape::Barge {
                    length,
                    beam,
                    draft,
                } => length * beam + 2. * (length + beam) * draft,
            }
    }

    /// The waterplane area of the whole body (m²); none for a submerged body.
    pub fn waterplane(&self) -> Option<f64> {
        match *self {
            Shape::Cylinder { radius, .. } | Shape::Hemisphere { radius } => {
                Some(PI * radius * radius)
            }
            Shape::Moonpool {
                radius,
                inner_radius,
                ..
            } => Some(PI * (radius * radius - inner_radius * inner_radius)),
            Shape::Sphere { .. } => None,
            Shape::Barge { length, beam, .. } => Some(length * beam),
        }
    }

    /// Check that the shape's dimensions are positive and finite and consistent with each other.
    pub(crate) fn check(&self) -> Result<()> {
        let positive = |x: f64, what: &str| -> Result<()> {
            ensure!(x.is_finite() && x > 0., "The {what} must be positive");
            Ok(())
        };
        match *self {
            Shape::Cylinder { radius, draft } => {
                positive(radius, "radius")?;
                positive(draft, "draft")
            }
            Shape::Moonpool {
                radius,
                inner_radius,
                draft,
            } => {
                positive(radius, "radius")?;
                positive(inner_radius, "moonpool radius")?;
                positive(draft, "draft")?;
                ensure!(
                    inner_radius < radius,
                    "The moonpool radius must be smaller than the radius"
                );
                Ok(())
            }
            Shape::Sphere { radius, depth } => {
                positive(radius, "radius")?;
                ensure!(
                    depth.is_finite() && depth > radius,
                    "The centre must lie deeper than the radius (the sphere under water)"
                );
                Ok(())
            }
            Shape::Hemisphere { radius } => positive(radius, "radius"),
            Shape::Barge {
                length,
                beam,
                draft,
            } => {
                positive(length, "length")?;
                positive(beam, "beam")?;
                positive(draft, "draft")
            }
        }
    }
}

/// A body to make: its shape, the panel size (m), the symmetry planes of the stored part, and whether the
/// interior surface is made.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub shape: Shape,
    pub panel_size: f64,
    pub symmetry: Symmetry,
    pub interior: bool,
    /// Exact spline patches (`primitives_ho`) instead of panels; the panel size is then the potential panel size.
    #[serde(default)]
    pub high_order: bool,
    /// Triangles only: every quadrilateral panel split in two along its shorter diagonal (low order).
    #[serde(default)]
    pub triangles: bool,
}

/// A body made: its panels (one face tag per part, named in `parts`), the interior surface, and the exact
/// volume and waterplane of the whole body.
pub struct Made {
    pub body: Panels,
    pub interior: Option<Panels>,
    /// The name of each part, by face tag (tag 1 is `parts[0]`).
    pub parts: Vec<&'static str>,
    pub volume_exact: f64,
    pub waterplane_exact: Option<f64>,
}

impl Made {
    /// The panels of the file: the body, and the interior surface after it (its panels lie in z = 0 with normals
    /// up, where the solver's interior surface "detect" finds them), its own face tag after the parts'.
    pub fn merged(&self) -> Panels {
        let mut panels = Panels {
            vertices: self.body.vertices.clone(),
            faces: self.body.faces.clone(),
            symmetry: self.body.symmetry,
            label: self.body.label.clone(),
        };
        if let Some(interior) = &self.interior {
            let tag = self.parts.len() as i32 + 1;
            panels.vertices.extend(&interior.vertices);
            panels
                .faces
                .extend(std::iter::repeat_n(tag, interior.vertices.len()));
        }
        panels
    }
}

/// The panels of one part, each turned so that its normal points along `outward` (a function of the panel
/// centre).
struct Part {
    name: &'static str,
    panels: Vec<Panel>,
}

fn point(r: f64, phi: f64, z: f64) -> [f64; 3] {
    [r * phi.cos(), r * phi.sin(), z]
}

/// Turn every panel of `panels` along the direction `outward` gives at its centre.
fn oriented(panels: Vec<Panel>, outward: impl Fn(DVec3) -> DVec3) -> Vec<Panel> {
    panels
        .into_iter()
        .map(|p| {
            let centre = p.map(DVec3::from_array).into_iter().sum::<DVec3>() / 4.;
            if normal(&p).dot(outward(centre)) < 0. {
                reversed(&p)
            } else {
                p
            }
        })
        .collect()
}

/// Every quadrilateral split in two triangles along its shorter diagonal (a triangle repeats its last vertex);
/// the winding, and so the normal, stays.
fn triangulated(panels: Vec<Panel>) -> Vec<Panel> {
    let mut out = Vec::with_capacity(2 * panels.len());
    for p in panels {
        if p[2] == p[3] {
            out.push(p);
            continue;
        }
        let d = |a: usize, b: usize| DVec3::from_array(p[a]).distance(DVec3::from_array(p[b]));
        if d(0, 2) <= d(1, 3) {
            out.push([p[0], p[1], p[2], p[2]]);
            out.push([p[0], p[2], p[3], p[3]]);
        } else {
            out.push([p[0], p[1], p[3], p[3]]);
            out.push([p[1], p[2], p[3], p[3]]);
        }
    }
    out
}

/// Divisions of a length at the panel size: at least `min`.
fn divisions(length: f64, size: f64, min: usize) -> usize {
    ((length / size - 1e-9).ceil() as usize).max(min)
}

/// The azimuth range of the stored part, its span, and whether it is the whole circle.
pub(crate) fn azimuths(symmetry: Symmetry) -> (f64, f64, bool) {
    match symmetry {
        Symmetry::None => (0., TAU, true),
        Symmetry::Yz => (-FRAC_PI_2, PI, false),
        Symmetry::Xz => (0., PI, false),
        Symmetry::XzAndYz => (0., FRAC_PI_2, false),
    }
}

/// The azimuths of `n` divisions of the stored part (the first again at the end of a whole circle).
fn angles(symmetry: Symmetry, n: usize) -> Vec<f64> {
    let (start, span, whole) = azimuths(symmetry);
    (0..=n)
        .map(|k| {
            if whole && k == n {
                start
            } else {
                start + span * k as f64 / n as f64
            }
        })
        .collect()
}

/// A wall of revolution between the profile points `(r, z)`, around the azimuths.
fn wall(profile: &[(f64, f64)], phi: &[f64]) -> Vec<Panel> {
    let mut out = Vec::new();
    for w in profile.windows(2) {
        let ((r0, z0), (r1, z1)) = (w[0], w[1]);
        for a in phi.windows(2) {
            let (p0, p1) = (a[0], a[1]);
            let q = [
                point(r0, p0, z0),
                point(r0, p1, z0),
                point(r1, p1, z1),
                point(r1, p0, z1),
            ];
            // A point of the axis closes the panel as a triangle (its last vertex repeated).
            out.push(if r1 == 0. {
                [q[0], q[1], q[2], q[2]]
            } else if r0 == 0. {
                [q[2], q[3], q[0], q[0]]
            } else {
                q
            });
        }
    }
    out
}

/// The radii of rings from `outer` to `inner` at the panel size (0 for a whole disc).
fn rings(outer: f64, inner: f64, size: f64) -> Vec<(f64, f64)> {
    let n = divisions(outer - inner, size, 1);
    (0..=n)
        .map(|k| (outer + (inner - outer) * k as f64 / n as f64, 0.))
        .collect()
}

/// Make the panels of a body.
pub fn make(request: &Request) -> Result<Made> {
    let shape = request.shape;
    shape.check()?;
    let h = request.panel_size;
    ensure!(h.is_finite() && h > 0., "The panel size must be positive");
    let symmetry = request.symmetry;
    let estimate = shape.area() / symmetry.images() / (h * h);
    ensure!(
        estimate <= MAX_PANELS as f64,
        "About {:.0} panels at {h} m: more than {MAX_PANELS}; use a larger panel size",
        estimate
    );
    let (_, span, _) = azimuths(symmetry);
    // At least four divisions a quadrant around a circle.
    let around =
        |radius: f64| divisions(radius * span, h, (span / FRAC_PI_2 * 4.).round() as usize);
    let at_z = |z: f64| {
        move |profile: Vec<(f64, f64)>| profile.into_iter().map(|(r, _)| (r, z)).collect::<Vec<_>>()
    };
    let radial = |c: DVec3| DVec3::new(c.x, c.y, 0.);
    let down = |_: DVec3| DVec3::NEG_Z;
    let up = |_: DVec3| DVec3::Z;
    let mut parts = Vec::new();
    let mut interior = None;
    match shape {
        Shape::Cylinder { radius, draft } => {
            let phi = angles(symmetry, around(radius));
            let nz = divisions(draft, h, 1);
            let side: Vec<(f64, f64)> = (0..=nz)
                .map(|k| (radius, -draft * k as f64 / nz as f64))
                .collect();
            parts.push(Part {
                name: "Side",
                panels: oriented(wall(&side, &phi), radial),
            });
            let disc = rings(radius, 0., h);
            parts.push(Part {
                name: "Bottom",
                panels: oriented(wall(&at_z(-draft)(disc.clone()), &phi), down),
            });
            if request.interior {
                interior = Some(oriented(wall(&at_z(0.)(disc), &phi), up));
            }
        }
        Shape::Moonpool {
            radius,
            inner_radius,
            draft,
        } => {
            let phi = angles(symmetry, around(radius));
            let nz = divisions(draft, h, 1);
            let side = |r: f64| -> Vec<(f64, f64)> {
                (0..=nz)
                    .map(|k| (r, -draft * k as f64 / nz as f64))
                    .collect()
            };
            parts.push(Part {
                name: "Outer side",
                panels: oriented(wall(&side(radius), &phi), radial),
            });
            parts.push(Part {
                name: "Moonpool wall",
                panels: oriented(wall(&side(inner_radius), &phi), |c| -radial(c)),
            });
            let annulus = rings(radius, inner_radius, h);
            parts.push(Part {
                name: "Bottom",
                panels: oriented(wall(&at_z(-draft)(annulus.clone()), &phi), down),
            });
            if request.interior {
                interior = Some(oriented(wall(&at_z(0.)(annulus), &phi), up));
            }
        }
        Shape::Sphere { radius, depth } => {
            let phi = angles(symmetry, around(radius));
            let n = divisions(PI * radius, h, 8);
            let profile: Vec<(f64, f64)> = (0..=n)
                .map(|k| {
                    let theta = PI * k as f64 / n as f64;
                    // The poles exactly on the axis, so that their panels close as triangles.
                    let r = if k == 0 || k == n {
                        0.
                    } else {
                        radius * theta.sin()
                    };
                    (r, -depth + radius * theta.cos())
                })
                .collect();
            let centre = DVec3::new(0., 0., -depth);
            parts.push(Part {
                name: "Sphere",
                panels: oriented(wall(&profile, &phi), move |c| c - centre),
            });
        }
        Shape::Hemisphere { radius } => {
            let phi = angles(symmetry, around(radius));
            let n = divisions(FRAC_PI_2 * radius, h, 4);
            let profile: Vec<(f64, f64)> = (0..=n)
                .map(|k| {
                    let theta = FRAC_PI_2 * k as f64 / n as f64;
                    let r = if k == n { 0. } else { radius * theta.cos() };
                    (r, -radius * theta.sin())
                })
                .collect();
            parts.push(Part {
                name: "Hull",
                panels: oriented(wall(&profile, &phi), |c| c),
            });
            if request.interior {
                interior = Some(oriented(wall(&at_z(0.)(rings(radius, 0., h)), &phi), up));
            }
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
            let split = |a: f64, b: f64| -> Vec<f64> {
                let n = divisions(b - a, h, 1);
                (0..=n).map(|k| a + (b - a) * k as f64 / n as f64).collect()
            };
            let (xs, ys, zs) = (split(x0, x1), split(y0, y1), split(-draft, 0.));
            // A grid of panels between two lists of coordinates, placed by `at`.
            let grid = |u: &[f64], v: &[f64], at: &dyn Fn(f64, f64) -> [f64; 3]| -> Vec<Panel> {
                let mut out = Vec::new();
                for a in u.windows(2) {
                    for b in v.windows(2) {
                        out.push([
                            at(a[0], b[0]),
                            at(a[1], b[0]),
                            at(a[1], b[1]),
                            at(a[0], b[1]),
                        ]);
                    }
                }
                out
            };
            parts.push(Part {
                name: "Bottom",
                panels: oriented(grid(&xs, &ys, &|x, y| [x, y, -draft]), down),
            });
            let mut sides = Vec::new();
            sides.extend(oriented(grid(&ys, &zs, &|y, z| [x1, y, z]), |_| DVec3::X));
            sides.extend(oriented(grid(&xs, &zs, &|x, z| [x, y1, z]), |_| DVec3::Y));
            if x0 < 0. {
                sides.extend(oriented(grid(&ys, &zs, &|y, z| [x0, y, z]), |_| {
                    DVec3::NEG_X
                }));
            }
            if y0 < 0. {
                sides.extend(oriented(grid(&xs, &zs, &|x, z| [x, y0, z]), |_| {
                    DVec3::NEG_Y
                }));
            }
            parts.push(Part {
                name: "Sides",
                panels: sides,
            });
            if request.interior {
                interior = Some(oriented(grid(&xs, &ys, &|x, y| [x, y, 0.]), up));
            }
        }
    }
    if request.triangles {
        for part in &mut parts {
            part.panels = triangulated(std::mem::take(&mut part.panels));
        }
        interior = interior.map(triangulated);
    }
    let mut body = Panels {
        vertices: Vec::new(),
        faces: Vec::new(),
        symmetry,
        label: shape.name().to_owned(),
    };
    let mut names = Vec::new();
    for (k, part) in parts.into_iter().enumerate() {
        body.faces
            .extend(std::iter::repeat_n(k as i32 + 1, part.panels.len()));
        body.vertices.extend(part.panels);
        names.push(part.name);
    }
    body.validate()?;
    let interior = interior
        .map(|vertices| -> Result<Panels> {
            let panels = Panels {
                faces: vec![1; vertices.len()],
                vertices,
                symmetry,
                label: format!("{} interior", shape.name()),
            };
            panels.validate()?;
            Ok(panels)
        })
        .transpose()?;
    Ok(Made {
        body,
        interior,
        parts: names,
        volume_exact: shape.volume(),
        waterplane_exact: shape.waterplane(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    fn key(p: [f64; 3]) -> [i64; 3] {
        p.map(|x| (x * 1e6).round() as i64)
    }

    /// The edges used by one panel only, less those on z = 0 or a symmetry plane.
    fn free_edges(panels: &[Panel], symmetry: Symmetry) -> usize {
        let mut edges: HashMap<([i64; 3], [i64; 3]), usize> = HashMap::new();
        for p in panels {
            for k in 0..4 {
                let (a, b) = (key(p[k]), key(p[(k + 1) % 4]));
                if a != b {
                    *edges.entry((a.min(b), a.max(b))).or_default() += 1;
                }
            }
        }
        let on_plane = |a: [i64; 3], b: [i64; 3]| {
            (a[2] == 0 && b[2] == 0) || symmetry.axes().iter().any(|&k| a[k] == 0 && b[k] == 0)
        };
        edges
            .iter()
            .filter(|&(&(a, b), &n)| n == 1 && !on_plane(a, b))
            .count()
    }

    #[test]
    fn triangles_close_every_shape_with_the_volume_of_the_quadrilaterals() {
        for shape in Shape::ALL {
            for symmetry in Symmetry::ALL {
                let request = Request {
                    shape,
                    panel_size: 1.,
                    symmetry,
                    interior: true,
                    high_order: false,
                    triangles: false,
                };
                let quads = make(&request).unwrap();
                let made = make(&Request {
                    triangles: true,
                    ..request
                })
                .unwrap();
                let name = format!("{} {}", shape.name(), symmetry.name());
                assert!(
                    made.body.vertices.iter().all(|p| p[2] == p[3]),
                    "{name}: a quadrilateral"
                );
                assert!(
                    made.body.vertices.len() > quads.body.vertices.len(),
                    "{name}"
                );
                assert_eq!(free_edges(&made.body.vertices, symmetry), 0, "{name}: open");
                // Parts keep their panels: every quadrilateral became two triangles of its part.
                assert_eq!(made.parts, quads.parts, "{name}");
                let [a, b] =
                    [&made, &quads].map(|m| m.body.volumes().unwrap().iter().sum::<f64>() / 3.);
                assert!((a / b - 1.).abs() < 1e-3, "{name}: {a} against {b}");
                if let Some(interior) = &made.interior {
                    assert!(
                        interior
                            .vertices
                            .iter()
                            .all(|p| p[2] == p[3] && normal(p).z > 0.),
                        "{name}: interior"
                    );
                }
            }
        }
    }

    #[test]
    fn every_shape_is_closed_with_its_volume_and_shares_its_waterline() {
        for shape in Shape::ALL {
            for symmetry in Symmetry::ALL {
                let made = make(&Request {
                    shape,
                    panel_size: 1.,
                    symmetry,
                    interior: true,
                    high_order: false,
                    triangles: false,
                })
                .unwrap();
                let name = format!("{} {}", shape.name(), symmetry.name());
                assert_eq!(free_edges(&made.body.vertices, symmetry), 0, "{name}: open");
                let volumes = made.body.volumes().unwrap();
                for v in volumes {
                    // Flat panels on a curve lose a little volume: within 1 % at panels of 1 m on 10 m radii.
                    assert!(
                        (v / made.volume_exact - 1.).abs() < 0.01,
                        "{name}: {v} of {}",
                        made.volume_exact
                    );
                }
                match (&made.interior, made.waterplane_exact) {
                    (Some(interior), Some(_)) => {
                        assert!(
                            interior.vertices.iter().all(|p| normal(p).z > 0.),
                            "{name}: interior down"
                        );
                        // Every waterline node of the interior is a node of the body.
                        let body: BTreeSet<_> = made
                            .body
                            .vertices
                            .iter()
                            .flatten()
                            .filter(|p| p[2] == 0.)
                            .map(|&p| key(p))
                            .collect();
                        let mut edges: HashMap<([i64; 3], [i64; 3]), usize> = HashMap::new();
                        for p in &interior.vertices {
                            for k in 0..4 {
                                let (a, b) = (key(p[k]), key(p[(k + 1) % 4]));
                                if a != b {
                                    *edges.entry((a.min(b), a.max(b))).or_default() += 1;
                                }
                            }
                        }
                        for (&(a, b), &n) in &edges {
                            if n == 1 && !symmetry.axes().iter().any(|&k| a[k] == 0 && b[k] == 0) {
                                assert!(
                                    body.contains(&a) && body.contains(&b),
                                    "{name}: waterline apart"
                                );
                            }
                        }
                    }
                    (None, None) => {}
                    _ => panic!(
                        "{name}: an interior for a submerged body, or none for a floating one"
                    ),
                }
            }
        }
    }

    #[test]
    fn dimensions_that_cannot_be_are_refused() {
        let request = |shape| Request {
            shape,
            panel_size: 1.,
            symmetry: Symmetry::None,
            interior: false,
            high_order: false,
            triangles: false,
        };
        for shape in [
            Shape::Cylinder {
                radius: -1.,
                draft: 1.,
            },
            Shape::Moonpool {
                radius: 2.,
                inner_radius: 3.,
                draft: 1.,
            },
            Shape::Sphere {
                radius: 2.,
                depth: 1.,
            },
        ] {
            assert!(make(&request(shape)).is_err(), "{shape:?}");
        }
    }
}

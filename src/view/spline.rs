// SPDX-License-Identifier: GPL-3.0-or-later
//! Exact HBG spline documents and bounded display-only sampling. Never used by an exporter.

use crate::conversion::HBG_VERSION;
use anyhow::{Result, ensure};
use serde::Deserialize;

/// An HBG geometry file of spline patches (version 3, kind `spline_patches`).
#[derive(Clone, Deserialize)]
pub struct Document {
    format: String,
    version: u32,
    kind: String,
    pub patches: Vec<Patch>,
}

/// A B-spline patch as the geometry file holds it: control points (and weights) in rows of u.
#[derive(Clone, Deserialize)]
pub struct Patch {
    /// The patch name (optional in a geometry file).
    #[serde(default)]
    pub name: String,
    pub u_order: usize,
    pub v_order: usize,
    pub u_knots: Vec<f64>,
    pub v_knots: Vec<f64>,
    pub coefficients: Vec<Vec<[f64; 3]>>,
    pub weights: Option<Vec<Vec<f64>>>,
}

/// Display cells (sampled quads) of a whole spline document at most: 500,000 quads are 1.5 million triangles,
/// 170 MB of vertices.
pub const DISPLAY_CELLS: usize = 500_000;

/// Display panels sampled from spline patches, with the owner, the patch and the edge flags of each.
pub struct Samples {
    pub vertices: Vec<[[f64; 3]; 4]>,
    pub owners: Vec<i32>,
    /// The patch each display panel samples.
    pub patch: Vec<usize>,
    /// AB, BC, CD, DA. Internal display-grid edges remain hidden.
    pub edges: Vec<[bool; 4]>,
    /// AB, BC, CD, DA on a line between two potential spans of the patch (inside it).
    pub spans: Vec<[bool; 4]>,
}

impl Document {
    /// Read and check a spline document, and map every knot vector to [0, 1].
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let document: Self = serde_json::from_slice(bytes)?;
        ensure!(
            document.format == "hbg"
                && document.version == HBG_VERSION
                && document.kind == "spline_patches",
            "Expected HBG version 3 spline patches"
        );
        document.checked()
    }

    /// A document of `patches` read from another file (a higher-order GDF), checked like an HBG one.
    pub fn from_patches(patches: Vec<Patch>) -> Result<Self> {
        Self {
            format: "hbg".into(),
            version: HBG_VERSION,
            kind: "spline_patches".into(),
            patches,
        }
        .checked()
    }

    /// Every patch checked, its knots on [0, 1] (sampling runs over [0, 1]).
    fn checked(mut self) -> Result<Self> {
        let document = &mut self;
        ensure!(!document.patches.is_empty(), "No spline patches");
        for patch in &mut document.patches {
            patch.validate()?;
            // Sampling runs over [0, 1]: a file may use any knot range (the same patch).
            for knots in [&mut patch.u_knots, &mut patch.v_knots] {
                let (a, b) = (knots[0], knots[knots.len() - 1]);
                if (a, b) != (0., 1.) {
                    knots.iter_mut().for_each(|k| *k = (*k - a) / (b - a));
                }
            }
        }
        Ok(self)
    }

    /// The patches where `pick` is true, as a document of their own (none when no patch is picked).
    pub fn select(&self, pick: &[bool]) -> Option<Self> {
        let patches: Vec<Patch> = self
            .patches
            .iter()
            .zip(pick)
            .filter(|(_, p)| **p)
            .map(|(patch, _)| patch.clone())
            .collect();
        (!patches.is_empty()).then(|| Self {
            format: self.format.clone(),
            version: self.version,
            kind: self.kind.clone(),
            patches,
        })
    }

    /// Sample the patches for display, one span each; `owners` gives the face of each patch.
    pub fn sample(&self, owners: &[i32]) -> Result<Samples> {
        self.sample_spans(owners, &[])
    }

    /// Samples whose grid lines include the lines between `spans` (u, v counts per patch; none: one span).
    pub fn sample_spans(&self, owners: &[i32], spans: &[[usize; 2]]) -> Result<Samples> {
        ensure!(
            owners.len() == self.patches.len(),
            "Spline ownership/count mismatch"
        );
        let mut sampled = Samples {
            vertices: Vec::new(),
            owners: Vec::new(),
            patch: Vec::new(),
            edges: Vec::new(),
            spans: Vec::new(),
        };
        // Per patch: spans and samples per span in u and v. At least the usual resolution, a whole number of
        // samples per span.
        let mut plans: Vec<[usize; 4]> = self
            .patches
            .iter()
            .enumerate()
            .map(|(index, patch)| {
                let [su, sv] = spans
                    .get(index)
                    .copied()
                    .unwrap_or([1, 1])
                    .map(|s| s.clamp(1, 64));
                let mu = (patch.coefficients[0].len() * 2)
                    .clamp(12, 32)
                    .div_ceil(su)
                    .max(1);
                let mv = (patch.coefficients.len() * 2)
                    .clamp(12, 32)
                    .div_ceil(sv)
                    .max(1);
                [su, sv, mu, mv]
            })
            .collect();
        // A large model is sampled coarser, within what the GPU can hold (a vertex buffer of 268 MB at most):
        // fewer samples per span first, then without the span lines.
        let cells = |plans: &[[usize; 4]]| {
            plans
                .iter()
                .map(|p| p[0] * p[1] * p[2] * p[3])
                .sum::<usize>()
        };
        let total = cells(&plans);
        if total > DISPLAY_CELLS {
            let f = (DISPLAY_CELLS as f64 / total as f64).sqrt();
            for p in &mut plans {
                p[2] = ((p[2] as f64 * f).floor() as usize).max(1);
                p[3] = ((p[3] as f64 * f).floor() as usize).max(1);
            }
            if cells(&plans) > DISPLAY_CELLS {
                let n = ((DISPLAY_CELLS as f64 / plans.len().max(1) as f64)
                    .sqrt()
                    .floor() as usize)
                    .max(2);
                plans.fill([1, 1, n, n]);
            }
        }
        for (index, (patch, &owner)) in self.patches.iter().zip(owners).enumerate() {
            let [su, sv, mu, mv] = plans[index];
            let (nu, nv) = (su * mu, sv * mv);
            let bu: Vec<_> = (0..=nu)
                .map(|i| basis(&patch.u_knots, patch.u_order, i as f64 / nu as f64))
                .collect();
            let bv: Vec<_> = (0..=nv)
                .map(|i| basis(&patch.v_knots, patch.v_order, i as f64 / nv as f64))
                .collect();
            let grid = bv
                .iter()
                .map(|v| {
                    bu.iter()
                        .map(|u| patch.point(u, v))
                        .collect::<Result<Vec<_>>>()
                })
                .collect::<Result<Vec<_>>>()?;
            for v in 0..nv {
                for u in 0..nu {
                    sampled.vertices.push([
                        grid[v][u],
                        grid[v][u + 1],
                        grid[v + 1][u + 1],
                        grid[v + 1][u],
                    ]);
                    sampled.owners.push(owner);
                    sampled.patch.push(index);
                    sampled
                        .edges
                        .push([v == 0, u + 1 == nu, v + 1 == nv, u == 0]);
                    sampled.spans.push([
                        v > 0 && v % mv == 0,
                        u + 1 < nu && (u + 1) % mu == 0,
                        v + 1 < nv && (v + 1) % mv == 0,
                        u > 0 && u % mu == 0,
                    ]);
                }
            }
        }
        Ok(sampled)
    }
}

impl Patch {
    fn validate(&self) -> Result<()> {
        let nv = self.coefficients.len();
        let nu = self.coefficients.first().map_or(0, Vec::len);
        ensure!(
            self.coefficients.iter().all(|r| r.len() == nu)
                && self
                    .coefficients
                    .iter()
                    .flatten()
                    .flatten()
                    .all(|c| c.is_finite()),
            "Invalid spline coefficient grid"
        );
        for (knots, order, n) in [
            (&self.u_knots, self.u_order, nu),
            (&self.v_knots, self.v_order, nv),
        ] {
            ensure!(
                (2..=16).contains(&order) && n >= order && knots.len() == n + order,
                "Invalid spline order/knot count"
            );
            ensure!(
                knots.iter().all(|k| k.is_finite()) && knots.windows(2).all(|w| w[0] <= w[1]),
                "Invalid spline knots"
            );
            ensure!(
                knots[0] < knots[n]
                    && knots[..order].iter().all(|&x| x == knots[0])
                    && knots[n..].iter().all(|&x| x == knots[n]),
                "Display requires clamped spline knots"
            );
        }
        if let Some(weights) = &self.weights {
            ensure!(
                weights.len() == nv
                    && weights.iter().all(|r| r.len() == nu)
                    && weights.iter().flatten().all(|w| w.is_finite() && *w > 0.),
                "Invalid rational weight grid"
            );
        }
        Ok(())
    }

    fn point(&self, u: &[f64], v: &[f64]) -> Result<[f64; 3]> {
        let mut point = [0.; 3];
        let mut denominator = 0.;
        for (j, &vj) in v.iter().enumerate() {
            if vj == 0. {
                continue;
            }
            for (i, &ui) in u.iter().enumerate() {
                if ui == 0. {
                    continue;
                }
                let coefficient = ui * vj * self.weights.as_ref().map_or(1., |w| w[j][i]);
                denominator += coefficient;
                for (axis, coordinate) in point.iter_mut().enumerate() {
                    *coordinate += coefficient * self.coefficients[j][i][axis];
                }
            }
        }
        ensure!(
            denominator.is_finite() && denominator > 0.,
            "Invalid rational display denominator"
        );
        point.iter_mut().for_each(|p| *p /= denominator);
        ensure!(
            point.iter().all(|p| p.is_finite()),
            "Non-finite spline display point"
        );
        Ok(point)
    }
}

fn basis(knots: &[f64], order: usize, parameter: f64) -> Vec<f64> {
    let count = knots.len() - order;
    if parameter == 1. {
        let mut last = vec![0.; count];
        last[count - 1] = 1.;
        return last;
    }
    let x = knots[0] + parameter * (knots[count] - knots[0]);
    let mut values: Vec<_> = knots
        .windows(2)
        .map(|w| f64::from(w[0] <= x && x < w[1]))
        .collect();
    for degree in 1..order {
        for i in 0..knots.len() - degree - 1 {
            let left = knots[i + degree] - knots[i];
            let right = knots[i + degree + 1] - knots[i + 1];
            values[i] = if left > 0. {
                (x - knots[i]) / left * values[i]
            } else {
                0.
            } + if right > 0. {
                (knots[i + degree + 1] - x) / right * values[i + 1]
            } else {
                0.
            };
        }
    }
    values.truncate(count);
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonuniform_multiple_spans_reproduce_linear_coordinates_and_validate_weights() {
        let knots = vec![0., 0., 0., 0.2, 0.7, 1., 1., 1.];
        let x: Vec<_> = (0..5)
            .map(|i| (knots[i + 1] + knots[i + 2]) * 0.5)
            .collect();
        let mut patch = Patch {
            name: "face_2".into(),
            u_order: 3,
            v_order: 2,
            u_knots: knots,
            v_knots: vec![0., 0., 1., 1.],
            coefficients: vec![
                x.iter().map(|&x| [x, 0., 0.]).collect(),
                x.iter().map(|&x| [x, 0., 1.]).collect(),
            ],
            weights: None,
        };
        patch.validate().unwrap();
        for parameter in [0., 0.1, 0.2, 0.45, 0.7, 0.9, 1.] {
            let point = patch
                .point(&basis(&patch.u_knots, 3, parameter), &[0.25, 0.75])
                .unwrap();
            assert!((point[0] - parameter).abs() < 1e-14 && (point[2] - 0.75).abs() < 1e-14);
        }
        patch.weights = Some(vec![vec![1.; 5], vec![1., 1., 0., 1., 1.]]);
        assert!(patch.validate().is_err());
        patch.weights = None;
        patch.u_knots[3] = 1.2;
        assert!(patch.validate().is_err());
    }

    #[test]
    fn rational_quarter_cylinder_keeps_radius_and_patch_boundaries() {
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let patch = Patch {
            name: "face_7".into(),
            u_order: 3,
            v_order: 2,
            u_knots: vec![2., 2., 2., 5., 5., 5.],
            v_knots: vec![-1., -1., 3., 3.],
            coefficients: vec![
                vec![[1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
                vec![[1., 0., 2.], [1., 1., 2.], [0., 1., 2.]],
            ],
            weights: Some(vec![vec![1., w, 1.], vec![1., w, 1.]]),
        };
        patch.validate().unwrap();
        let point = patch
            .point(
                &basis(&patch.u_knots, 3, 0.5),
                &basis(&patch.v_knots, 2, 0.5),
            )
            .unwrap();
        assert!(
            (point[0] - w).abs() < 1e-14
                && (point[1] - w).abs() < 1e-14
                && (point[2] - 1.).abs() < 1e-14
        );
        let document = Document {
            format: "hbg".into(),
            version: HBG_VERSION,
            kind: "spline_patches".into(),
            patches: vec![patch],
        };
        let sampled = document.sample(&[7]).unwrap();
        assert!(sampled.owners.iter().all(|&owner| owner == 7));
        assert_eq!(
            sampled.edges.iter().flatten().filter(|&&edge| edge).count(),
            48
        );
        for point in sampled.vertices.iter().flatten() {
            assert!((point[0].hypot(point[1]) - 1.).abs() < 1e-14);
        }
    }
}

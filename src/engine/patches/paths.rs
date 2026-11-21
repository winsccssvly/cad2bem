// SPDX-License-Identifier: GPL-3.0-or-later
//! The paths a face tries before split: direct, native, triangle, single, polygon, bridge, O-grid and ring.

use super::*;

impl Converter<'_> {
    /// Patches in the face's own parameters, the box cut evenly into pieces whose normal turns by at most
    /// 90 degrees.
    pub(super) fn direct(&mut self, f: i32) -> Result<Fits> {
        let (lo, hi) = self.gmsh.parametrization_bounds(2, f)?;
        let cuts: Vec<usize> = (0..2)
            .map(|a| -> Result<usize> {
                Ok(((self.turning(f, &lo, &hi, a)? / 90. - 1e-6).ceil() as usize).max(1))
            })
            .collect::<Result<_>>()?;
        let us = linspace(lo[0], hi[0], cuts[0] + 1);
        let vs = linspace(lo[1], hi[1], cuts[1] + 1);
        self.adaptive(f, |this, spans, n| {
            let g = unit_grid(n);
            let mut fits = Vec::new();
            for v in vs.windows(2) {
                for u in us.windows(2) {
                    let uv: Vec<f64> = g
                        .iter()
                        .flat_map(|&t| {
                            g.iter().flat_map(move |&s| {
                                [u[0] + s * (u[1] - u[0]), v[0] + t * (v[1] - v[0])]
                            })
                        })
                        .collect();
                    let grid = this.values(2, f, &uv)?;
                    let edges = [
                        row(&grid, n, 0),
                        column(&grid, n, n - 1),
                        row(&grid, n, n - 1),
                        column(&grid, n, 0),
                    ];
                    let coefs: Vec<Vec<DVec3>> = edges
                        .iter()
                        .map(|e| this.fit(n, spans, e))
                        .collect::<Result<_>>()?;
                    let p = fit_surface(
                        &g,
                        &g,
                        &grid,
                        spans,
                        this.settings.order,
                        Some([&coefs[0], &coefs[1], &coefs[2], &coefs[3]]),
                    )?;
                    let error = p
                        .grid(&g, &g)
                        .iter()
                        .zip(&grid)
                        .map(|(a, b)| a.distance(*b))
                        .fold(0., f64::max);
                    fits.push((p, grid, error));
                }
            }
            Ok(fits)
        })
    }

    /// The poles of the file's B-spline surface under an untrimmed face, cut to its parameter box and split at
    /// C0 knots; None when no surface matches it (scale fitted to the samples, then checked within the fit
    /// tolerance).
    pub(super) fn native(&self, f: i32) -> Result<Option<Fits>> {
        let (lo, hi) = self.gmsh.parametrization_bounds(2, f)?;
        let (lo, hi) = ([lo[0], lo[1]], [hi[0], hi[1]]);
        let g = [0., 0.37, 1.];
        let uv: Vec<(f64, f64)> = g
            .iter()
            .flat_map(|&b| {
                g.iter()
                    .map(move |&a| (lo[0] + a * (hi[0] - lo[0]), lo[1] + b * (hi[1] - lo[1])))
            })
            .collect();
        let flat: Vec<f64> = uv.iter().flat_map(|&(u, v)| [u, v]).collect();
        let x = self.values(2, f, &flat)?;
        let eps = 1e-9 * 1f64.max((hi[0] - lo[0]).abs()).max((hi[1] - lo[1]).abs());
        for surface in &self.surfaces {
            let p = &surface.patch;
            let [u0, u1, v0, v1] = p.range();
            if !(u0 - eps <= lo[0] && hi[0] <= u1 + eps && v0 - eps <= lo[1] && hi[1] <= v1 + eps) {
                continue;
            }
            let raw: Vec<DVec3> = uv.iter().map(|&(u, v)| p.at(u, v)).collect();
            let k = x.iter().zip(&raw).map(|(a, b)| a.dot(*b)).sum::<f64>()
                / raw.iter().map(|b| b.dot(*b)).sum::<f64>().max(1e-300);
            if k.is_nan() || k <= 0. {
                continue;
            }
            let mut scaled = p.clone();
            for c in scaled.coefficients.iter_mut() {
                *c *= k;
            }
            if uv
                .iter()
                .zip(&x)
                .any(|(&(u, v), y)| scaled.at(u, v).distance(*y) > self.tol())
            {
                continue;
            }
            let (a0, a1) = (lo[0].max(u0), hi[0].min(u1));
            let (b0, b1) = (lo[1].max(v0), hi[1].min(v1));
            let pieces: Vec<Patch> = bspline::split_c0(&bspline::restrict(&scaled, a0, a1, b0, b1))
                .iter()
                .map(bspline::normalized)
                .collect();
            let n = 2 * self.settings.nsample - 1;
            let g = unit_grid(self.settings.nsample);
            let mut fits = Vec::new();
            let mut cads = Vec::new();
            for q in pieces {
                cads.push(cad_distance(self.gmsh, f, &q, n)?);
                let grid = q.grid(&g, &g);
                fits.push((q, grid, 0.));
            }
            return Ok(Some((fits, cads, 0)));
        }
        Ok(None)
    }

    /// A trimmed four-sided face as one Coons patch of its boundary curves.
    /// A four-sided face as one Coons patch of its sides, or, when a pair of opposite sides turns by more than
    /// 90 degrees (a half ring, a long arc), as strips across it of at most 90 degrees each, so that its
    /// patches meet those of the neighbours (split at quarter turns) corner to corner and can become exact.
    /// A three-sided face as one Coons patch whose fourth side is collapsed into its sharpest corner (a triangle
    /// patch): its corners are the face's own.
    pub(super) fn triangle(&mut self, f: i32, curves: &[i32]) -> Result<Option<Fits>> {
        let curves = curves.to_vec();
        let mut failed = false;
        let result = self.adaptive(f, |this, spans, n| {
            let Some(c) = this.chain(&curves, n)? else {
                failed = true;
                return Ok(Vec::new());
            };
            if c.len() != 3 {
                failed = true;
                return Ok(Vec::new());
            }
            // The interior angle at corner k, between the side that ends there and the side that starts there.
            let angle = |k: usize| {
                let (before, at) = (&c[(k + 2) % 3], c[k][0]);
                angle_degrees(before[n - 2] - at, c[k][1] - at)
            };
            let apex = (0..3)
                .min_by(|&a, &b| angle(a).total_cmp(&angle(b)))
                .unwrap_or(0);
            // Corners V_i, V_i+1 and the apex V_i+2; the top edge is the apex itself.
            let i = (apex + 1) % 3;
            let edges = [
                c[i].clone(),
                c[(i + 1) % 3].clone(),
                vec![c[apex][0]; n],
                reversed(&c[apex]),
            ];
            Ok(vec![this.fit_four(f, &edges, n, spans)?])
        })?;
        Ok((!failed).then_some(result))
    }

    pub(super) fn single(&mut self, f: i32, curves: &[i32]) -> Result<Option<Fits>> {
        let curves = curves.to_vec();
        let mut failed = false;
        let result = self.adaptive(f, |this, spans, n| {
            let Some(c) = this.chain(&curves, n)? else {
                failed = true;
                return Ok(Vec::new());
            };
            let turns: Vec<f64> = c.iter().map(|side| polyline_turning(side)).collect();
            let (pair, turn) = if turns[0].max(turns[2]) >= turns[1].max(turns[3]) {
                (0, turns[0].max(turns[2]))
            } else {
                (1, turns[1].max(turns[3]))
            };
            let strips = ((turn / 90. - 1e-6).ceil() as usize).max(1);
            if strips == 1 {
                let edges = [c[0].clone(), c[1].clone(), reversed(&c[2]), reversed(&c[3])];
                return Ok(vec![this.fit_four(f, &edges, n, spans)?]);
            }
            // The turning pair becomes sides 0 and 2, sampled finely enough to give each strip n points.
            let fine = this
                .chain(&curves, strips * (n - 1) + 1)?
                .context("the boundary closed at one sampling but not at another")?;
            let rotate = |s: &Vec<Vec<DVec3>>| -> Vec<Vec<DVec3>> {
                (0..4).map(|k| s[(k + pair) % 4].clone()).collect()
            };
            let (c, fine) = (rotate(&c), rotate(&fine));
            let bottom = &fine[0];
            let top = reversed(&fine[2]);
            let mut spokes = vec![reversed(&c[3])];
            for j in 1..strips {
                let k = j * (n - 1);
                spokes.push(this.chord(f, bottom[k], top[k], n)?);
            }
            spokes.push(c[1].clone());
            let mut fits = Vec::new();
            for j in 0..strips {
                let range = j * (n - 1)..=(j + 1) * (n - 1);
                let edges = [
                    bottom[range.clone()].to_vec(),
                    spokes[j + 1].clone(),
                    top[range].to_vec(),
                    spokes[j].clone(),
                ];
                fits.push(this.fit_four(f, &edges, n, spans)?);
            }
            Ok(fits)
        })?;
        Ok((!failed).then_some(result))
    }

    /// An N-sided face as N quads: corner k, the midpoint of side k, the face centre and the midpoint of
    /// side k - 1.
    pub(super) fn polygon(&mut self, f: i32, curves: &[i32]) -> Result<Option<Fits>> {
        let centre = self.project(2, f, &[DVec3::from_array(self.gmsh.center_of_mass(2, f)?)])?[0];
        let curves = curves.to_vec();
        let mut failed = false;
        let result = self.adaptive(f, |this, spans, n| {
            let Some(c) = this.chain(&curves, 2 * n - 1)? else {
                failed = true;
                return Ok(Vec::new());
            };
            let g = unit_grid(n);
            let mut spokes = Vec::new();
            for e in &c {
                let chord: Vec<DVec3> = g
                    .iter()
                    .map(|&x| e[n - 1] * (1. - x) + centre * x)
                    .collect();
                let mut x = this.project(2, f, &chord)?;
                x[0] = e[n - 1];
                x[n - 1] = centre;
                let coef = this.fit(n, spans, &x)?;
                spokes.push((x, coef));
            }
            let mut fits = Vec::new();
            for k in 0..c.len() {
                let previous = (k + c.len() - 1) % c.len();
                let first = c[k][..n].to_vec();
                let last = reversed(&c[previous][n - 1..]);
                let coefs = [
                    this.fit(n, spans, &first)?,
                    spokes[k].1.clone(),
                    spokes[previous].1.clone(),
                    this.fit(n, spans, &last)?,
                ];
                fits.push(this.fit_edges(
                    f,
                    [&first, &spokes[k].0, &spokes[previous].0, &last],
                    [&coefs[0], &coefs[1], &coefs[2], &coefs[3]],
                    spans,
                )?);
            }
            Ok(fits)
        })?;
        Ok((!failed).then_some(result))
    }

    /// A five- or six-sided face as two quads split by one line across it: from a corner to the opposite
    /// corner (six sides) or to the middle of the opposite side (five sides). Splits are tried shortest first
    /// and the first whose quads are fair within the fit tolerance is kept. A long piece of a pipe junction
    /// window is such a face: the centre spokes of [`Self::polygon`] meet its long sides at sharp angles.
    pub(super) fn bridge(&mut self, f: i32, curves: &[i32]) -> Result<Option<Fits>> {
        let curves = curves.to_vec();
        let Some(c) = self.chain(&curves, 3)? else {
            return Ok(None);
        };
        let k = c.len();
        if !(5..=6).contains(&k) {
            return Ok(None);
        }
        let mut splits: Vec<(f64, usize)> = (0..k)
            .map(|i| {
                let from = c[i][0];
                let to = if k == 6 {
                    c[(i + 3) % k][0]
                } else {
                    c[(i + 2) % k][1]
                };
                (from.distance(to), i)
            })
            .collect();
        if k == 6 {
            splits.truncate(3); // corner i to i + 3 is the split from i + 3 to i
        }
        splits.sort_by(|a, b| a.0.total_cmp(&b.0));
        let tol = self.tol();
        for (_, i) in splits {
            let mut make = |this: &mut Self, spans: usize, n: usize| -> Result<Vec<Fit>> {
                let Some(c) = this.chain(&curves, 2 * n - 1)? else {
                    return Ok(Vec::new());
                };
                // A whole side with n points (every other of its 2n - 1), from its first corner.
                let side =
                    |j: usize| -> Vec<DVec3> { c[j % k].iter().step_by(2).copied().collect() };
                // Coons edges: bottom A -> B, right B -> C, top D -> C, left A -> D.
                let mut quads: Vec<[Vec<DVec3>; 4]> = Vec::new();
                if k == 6 {
                    let across = this.chord(f, c[i][0], c[(i + 3) % k][0], n)?;
                    quads.push([side(i), side(i + 1), reversed(&side(i + 2)), across.clone()]);
                    quads.push([
                        side(i + 3),
                        side(i + 4),
                        reversed(&side(i + 5)),
                        reversed(&across),
                    ]);
                } else {
                    let split = &c[(i + 2) % k];
                    let across = this.chord(f, c[i][0], split[n - 1], n)?;
                    quads.push([side(i), side(i + 1), reversed(&split[..n]), across.clone()]);
                    quads.push([
                        split[n - 1..].to_vec(),
                        side(i + 3),
                        reversed(&side(i + 4)),
                        reversed(&across),
                    ]);
                }
                let mut fits = Vec::new();
                for q in &quads {
                    let coefs: Vec<Vec<DVec3>> = q
                        .iter()
                        .map(|e| this.fit(n, spans, e))
                        .collect::<Result<_>>()?;
                    fits.push(this.fit_edges(
                        f,
                        [&q[0], &q[1], &q[2], &q[3]],
                        [&coefs[0], &coefs[1], &coefs[2], &coefs[3]],
                        spans,
                    )?);
                }
                Ok(fits)
            };
            // The sides and the split line set the corners, the spans hardly move them: a split whose
            // coarsest quads have a corner far outside the fair range would fail after every refinement,
            // so it is not refined (most of the time of a five- or six-sided face went there).
            let coarse = SPANS[0];
            let quick = make(self, coarse, self.settings.nsample.max(4 * coarse + 1))?;
            if quick.is_empty()
                || quick.iter().any(|(p, _, _)| {
                    checks::corner_angles(p)
                        .iter()
                        .any(|&a| !(BRIDGE_HOPELESS.0..=BRIDGE_HOPELESS.1).contains(&a))
                })
            {
                continue;
            }
            let result = self.adaptive(f, &mut make)?;
            let patches: Vec<&Patch> = result.0.iter().map(|x| &x.0).collect();
            if !patches.is_empty()
                && result.1.iter().copied().fold(0., f64::max) <= tol
                && fair(&patches)
            {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }

    /// n points of a dense closed loop from arc length s0 to s1 (wrapping), projected onto their curves.
    pub(super) fn stretch(&self, lp: &Loop, s0: f64, s1: f64, n: usize) -> Result<Vec<DVec3>> {
        let mut closed = lp.points.clone();
        closed.push(lp.points[0]);
        let arc = lp.arc();
        let total = arc[arc.len() - 1];
        let mut x = Vec::with_capacity(n);
        let mut who = Vec::with_capacity(n);
        for s in linspace(s0, s1, n) {
            let target = s.rem_euclid(total);
            let (p, _) = interpolate(&arc, &closed, target);
            x.push(p);
            let k = arc
                .partition_point(|&a| a <= target)
                .saturating_sub(1)
                .min(lp.owner.len() - 1);
            who.push(lp.owner[k]);
        }
        let mut curves: Vec<i32> = who.clone();
        curves.sort_unstable();
        curves.dedup();
        for cv in curves {
            let selected: Vec<usize> = (0..n).filter(|&i| who[i] == cv).collect();
            let projected =
                self.project(1, cv, &selected.iter().map(|&i| x[i]).collect::<Vec<_>>())?;
            for (i, p) in selected.into_iter().zip(projected) {
                x[i] = p;
            }
        }
        Ok(x)
    }

    /// n points of the chord a -> b projected onto the face, the ends kept.
    pub(super) fn chord(&self, f: i32, a: DVec3, b: DVec3, n: usize) -> Result<Vec<DVec3>> {
        let g = unit_grid(n);
        let mut x = self.project(
            2,
            f,
            &g.iter().map(|&t| a * (1. - t) + b * t).collect::<Vec<_>>(),
        )?;
        x[0] = a;
        x[n - 1] = b;
        Ok(x)
    }

    /// A disc-like face (one smooth loop) as five quads: a centre quad and four around it.
    pub(super) fn ogrid(&mut self, f: i32, lp: &Loop) -> Result<Fits> {
        let start = lp.layout_start();
        let arc = lp.arc();
        let total = arc[arc.len() - 1];
        let s0 = arc[start];
        let centre = self.project(2, f, &[DVec3::from_array(self.gmsh.center_of_mass(2, f)?)])?[0];
        let lp = lp.clone();
        self.adaptive(f, |this, spans, n| {
            let arcs: Vec<Vec<DVec3>> = (0..4)
                .map(|k| {
                    this.stretch(
                        &lp,
                        s0 + k as f64 * total / 4.,
                        s0 + (k + 1) as f64 * total / 4.,
                        n,
                    )
                })
                .collect::<Result<_>>()?;
            let p: Vec<DVec3> = arcs.iter().map(|a| a[0]).collect();
            let q: Vec<DVec3> = p
                .iter()
                .map(|&x| Ok(this.project(2, f, &[centre + 0.5 * (x - centre)])?[0]))
                .collect::<Result<_>>()?;
            let spoke: Vec<Vec<DVec3>> = (0..4)
                .map(|k| this.chord(f, p[k], q[k], n))
                .collect::<Result<_>>()?;
            let inner: Vec<Vec<DVec3>> = (0..4)
                .map(|k| this.chord(f, q[k], q[(k + 1) % 4], n))
                .collect::<Result<_>>()?;
            let mut fits = Vec::new();
            for k in 0..4 {
                let edges = [
                    arcs[k].clone(),
                    spoke[(k + 1) % 4].clone(),
                    inner[k].clone(),
                    spoke[k].clone(),
                ];
                fits.push(this.fit_four(f, &edges, n, spans)?);
            }
            let edges = [
                inner[0].clone(),
                inner[1].clone(),
                reversed(&inner[2]),
                reversed(&inner[3]),
            ];
            fits.push(this.fit_four(f, &edges, n, spans)?);
            Ok(fits)
        })
    }

    pub(super) fn fit_four(
        &self,
        f: i32,
        edges: &[Vec<DVec3>; 4],
        n: usize,
        spans: usize,
    ) -> Result<Fit> {
        let coefs: Vec<Vec<DVec3>> = edges
            .iter()
            .map(|e| self.fit(n, spans, e))
            .collect::<Result<_>>()?;
        self.fit_edges(
            f,
            [&edges[0], &edges[1], &edges[2], &edges[3]],
            [&coefs[0], &coefs[1], &coefs[2], &coefs[3]],
            spans,
        )
    }

    /// A face between two smooth loops (an annulus) as four quads.
    pub(super) fn ring(&mut self, f: i32, outer: &Loop, inner: &Loop) -> Result<Option<Fits>> {
        let start = outer.layout_start();
        let (lo, li) = (outer.length(), inner.length());
        let ao = outer.arc();
        let so: Vec<f64> = (0..5).map(|k| ao[start] + k as f64 * lo / 4.).collect();
        let mut closed = outer.points.clone();
        closed.push(outer.points[0]);
        let xo: Vec<DVec3> = so[..4]
            .iter()
            .map(|&s| interpolate(&ao, &closed, s.rem_euclid(lo)).0)
            .collect();
        let quarters = |lp: &Loop| -> Vec<f64> {
            let a = lp.arc();
            let s: Vec<f64> = xo
                .iter()
                .map(|x| {
                    let k = (0..lp.points.len())
                        .min_by(|&i, &j| {
                            lp.points[i]
                                .distance(*x)
                                .total_cmp(&lp.points[j].distance(*x))
                        })
                        .unwrap_or(0);
                    // A CAD vertex of the inner loop within a few samples is where its neighbours have a
                    // corner: the quad corner goes there.
                    let reach = 3. * a[a.len() - 1] / lp.points.len() as f64;
                    let vertex = lp
                        .vertices()
                        .into_iter()
                        .filter(|&v| lp.points[v].distance(*x) <= reach)
                        .min_by(|&i, &j| {
                            lp.points[i]
                                .distance(*x)
                                .total_cmp(&lp.points[j].distance(*x))
                        });
                    a[vertex.unwrap_or(k)]
                })
                .collect();
            let mut out: Vec<f64> = s
                .iter()
                .map(|&x| s[0] + (x - s[0]).rem_euclid(li))
                .collect();
            out.push(s[0] + li);
            out
        };
        let mut inner = inner.clone();
        let mut si = quarters(&inner);
        if !si.windows(2).all(|w| w[1] > w[0]) {
            inner = inner.reversed();
            si = quarters(&inner);
            if !si.windows(2).all(|w| w[1] > w[0]) {
                return Ok(None);
            }
        }
        let outer = outer.clone();
        let result = self.adaptive(f, |this, spans, n| {
            let oa: Vec<Vec<DVec3>> = (0..4)
                .map(|k| this.stretch(&outer, so[k], so[k + 1], n))
                .collect::<Result<_>>()?;
            let ia: Vec<Vec<DVec3>> = (0..4)
                .map(|k| this.stretch(&inner, si[k], si[k + 1], n))
                .collect::<Result<_>>()?;
            let spoke: Vec<Vec<DVec3>> = (0..4)
                .map(|k| this.chord(f, oa[k][0], ia[k][0], n))
                .collect::<Result<_>>()?;
            let mut fits = Vec::new();
            for k in 0..4 {
                let edges = [
                    oa[k].clone(),
                    spoke[(k + 1) % 4].clone(),
                    ia[k].clone(),
                    spoke[k].clone(),
                ];
                fits.push(this.fit_four(f, &edges, n, spans)?);
            }
            Ok(fits)
        })?;
        Ok(Some(result))
    }
}

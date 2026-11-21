// SPDX-License-Identifier: GPL-3.0-or-later
//! Face boundaries: curve chains, smooth loops and sides, and the fitted edges the paths share.

use super::*;

impl Converter<'_> {
    /// The boundary curves of a face with one loop, degenerate curves (poles) left out; None with holes.
    pub(super) fn boundary(&self, f: i32) -> Result<Option<Vec<i32>>> {
        let loops = self.gmsh.curve_loops(f)?;
        if loops.len() != 1 {
            return Ok(None);
        }
        let tol = 1e-9 * self.settings.scale;
        let mut out = Vec::new();
        for c in loops[0].iter().map(|c| c.abs()) {
            let (lo, hi) = self.gmsh.parametrization_bounds(1, c)?;
            let x = self.values(1, c, &linspace(lo[0], hi[0], 5))?;
            if x.iter().any(|p| p.distance(x[0]) > tol) {
                out.push(c);
            }
        }
        Ok(Some(out))
    }

    /// The curves of a loop chained by their end points into one closed loop, m samples each in loop
    /// direction; None when they do not close.
    pub(super) fn dense_chain(&self, curves: &[i32], m: usize) -> Result<Option<Chain>> {
        let mut c = Vec::new();
        for &cv in curves {
            let (lo, hi) = self.gmsh.parametrization_bounds(1, cv)?;
            c.push((cv, self.values(1, cv, &linspace(lo[0], hi[0], m))?));
        }
        if c.is_empty() {
            return Ok(None);
        }
        let tol = 1e-5 * self.settings.scale;
        let near = |a: DVec3, b: DVec3| a.distance(b) < tol;
        let mut out = vec![c[0].clone()];
        let mut rest: Vec<(i32, Vec<DVec3>)> = c[1..].to_vec();
        if !rest.is_empty() {
            let end = *out[0].1.last().unwrap_or(&DVec3::ZERO);
            if !rest
                .iter()
                .any(|(_, r)| near(end, r[0]) || near(end, r[r.len() - 1]))
            {
                out[0].1.reverse();
            }
        }
        while !rest.is_empty() {
            let end = *out[out.len() - 1].1.last().unwrap_or(&DVec3::ZERO);
            let touch: Vec<usize> = rest
                .iter()
                .enumerate()
                .filter(|(_, (_, r))| near(r[0], end) || near(r[r.len() - 1], end))
                .map(|(k, _)| k)
                .collect();
            if touch.is_empty() {
                return Ok(None);
            }
            // A seam is in the loop twice: not back along the curve just walked when another goes on.
            let last = out[out.len() - 1].0;
            let k = touch
                .iter()
                .copied()
                .find(|&k| rest[k].0 != last)
                .unwrap_or(touch[0]);
            let (cv, mut r) = rest.remove(k);
            if !near(r[0], end) {
                r.reverse();
            }
            out.push((cv, r));
        }
        let first = out[0].1[0];
        let end = *out[out.len() - 1].1.last().unwrap_or(&DVec3::ZERO);
        Ok(near(end, first).then_some(out))
    }

    /// A closed loop without corners as a dense polyline, or None.
    pub(super) fn smooth_loop(&self, curves: &[i32]) -> Result<Option<Loop>> {
        let Some(out) = self.dense_chain(curves, 65)? else {
            return Ok(None);
        };
        for k in 0..out.len() {
            let a = &out[(k + out.len() - 1) % out.len()].1;
            let b = &out[k].1;
            if angle_degrees(a[a.len() - 2] - a[a.len() - 1], b[1] - b[0]) <= SMOOTH {
                return Ok(None);
            }
        }
        let mut points = Vec::new();
        let mut owner = Vec::new();
        let mut vertex = Vec::new();
        for (cv, x) in &out {
            points.extend(&x[..x.len() - 1]);
            owner.extend(std::iter::repeat_n(*cv, x.len() - 1));
            vertex.push(true);
            vertex.extend(std::iter::repeat_n(false, x.len() - 2));
        }
        Ok(Some(Loop {
            points,
            owner,
            vertex,
        }))
    }

    /// The boundary loop as sides between corners; None when the curves do not close or have no corner.
    pub(super) fn sides(&mut self, curves: &[i32]) -> Result<Option<Sides>> {
        if let Some(cached) = self.side_cache.get(curves) {
            return Ok(cached.clone());
        }
        let result = (|| -> Result<Option<Sides>> {
            let Some(out) = self.dense_chain(curves, 33)? else {
                return Ok(None);
            };
            let corner: Vec<bool> = (0..out.len())
                .map(|k| {
                    let a = &out[(k + out.len() - 1) % out.len()].1;
                    let b = &out[k].1;
                    angle_degrees(a[a.len() - 2] - a[a.len() - 1], b[1] - b[0]) <= SMOOTH
                })
                .collect();
            let Some(start) = corner.iter().position(|&c| c) else {
                return Ok(None);
            };
            let mut sides: Sides = Vec::new();
            for k in 0..out.len() {
                let index = (start + k) % out.len();
                if corner[index] {
                    sides.push(Vec::new());
                }
                if let Some(side) = sides.last_mut() {
                    side.push(out[index].clone());
                }
            }
            Ok(Some(sides))
        })()?;
        self.side_cache.insert(curves.to_vec(), result.clone());
        Ok(result)
    }

    /// The sides of the boundary loop sampled at n points each, evenly in arc length and projected onto
    /// their curves, each from its corner to the next.
    pub(super) fn chain(&mut self, curves: &[i32], n: usize) -> Result<Option<Vec<Vec<DVec3>>>> {
        let Some(sides) = self.sides(curves)? else {
            return Ok(None);
        };
        let mut out = Vec::new();
        for side in &sides {
            let mut dense: Vec<DVec3> = Vec::new();
            let mut owner = Vec::new();
            for (k, (_, x)) in side.iter().enumerate() {
                let skip = usize::from(k > 0);
                dense.extend(&x[skip..]);
                owner.extend(std::iter::repeat_n(k, x.len() - skip));
            }
            let mut arc = vec![0.];
            for w in dense.windows(2) {
                arc.push(arc[arc.len() - 1] + w[0].distance(w[1]));
            }
            let target = linspace(0., arc[arc.len() - 1], n);
            let mut points = Vec::with_capacity(n);
            let mut which = Vec::with_capacity(n);
            for &s in &target {
                let (p, _) = interpolate(&arc, &dense, s);
                points.push(p);
                let k = arc.partition_point(|&a| a < s).min(owner.len() - 1);
                which.push(owner[k]);
            }
            for (k, (cv, _)) in side.iter().enumerate() {
                let selected: Vec<usize> = (0..n).filter(|&i| which[i] == k).collect();
                if selected.is_empty() {
                    continue;
                }
                let projected = self.project(
                    1,
                    *cv,
                    &selected.iter().map(|&i| points[i]).collect::<Vec<_>>(),
                )?;
                for (i, p) in selected.into_iter().zip(projected) {
                    points[i] = p;
                }
            }
            points[0] = dense[0];
            points[n - 1] = dense[dense.len() - 1];
            out.push(points);
        }
        Ok(Some(out))
    }

    pub(super) fn revolution(&self, f: i32) -> Result<bool> {
        Ok(matches!(
            self.gmsh.kind(2, f)?.as_str(),
            "Sphere" | "Torus" | "Surface of Revolution"
        ))
    }

    /// Whether the sides of the face's parameter box lie on its boundary curves.
    pub(super) fn untrimmed(&self, f: i32, curves: &[i32]) -> Result<bool> {
        let (lo, hi) = self.gmsh.parametrization_bounds(2, f)?;
        let g = unit_grid(7);
        let tol = self.tol() * 10.;
        for side in 0..4 {
            let uv: Vec<f64> = g
                .iter()
                .flat_map(|&x| {
                    let (a, b) = match side {
                        0 => (x, 0.),
                        1 => (1., x),
                        2 => (x, 1.),
                        _ => (0., x),
                    };
                    [lo[0] + a * (hi[0] - lo[0]), lo[1] + b * (hi[1] - lo[1])]
                })
                .collect();
            let x = self.values(2, f, &uv)?;
            let mut d = vec![f64::INFINITY; x.len()];
            for &c in curves {
                for (k, p) in self.project(1, c, &x)?.into_iter().enumerate() {
                    d[k] = d[k].min(p.distance(x[k]));
                }
            }
            if d.iter().any(|&v| v > tol) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // -- fits ---------------------------------------------------------------------------------

    /// The patch of four sampled edges (bottom, right, top, left) with fixed edge coefficients: the Coons
    /// grid projected onto the face, the interior fitted.
    pub(super) fn fit_edges(
        &self,
        f: i32,
        edges: [&[DVec3]; 4],
        coefs: [&[DVec3]; 4],
        spans: usize,
    ) -> Result<Fit> {
        let n = edges[0].len();
        let s = unit_grid(n);
        let mut grid = bspline::coons(edges[0], edges[1], edges[2], edges[3]);
        let inner: Vec<DVec3> = (1..n - 1)
            .flat_map(|j| (1..n - 1).map(move |i| (j, i)))
            .map(|(j, i)| grid[j * n + i])
            .collect();
        let projected = self.project(2, f, &inner)?;
        let mut k = 0;
        for j in 1..n - 1 {
            for i in 1..n - 1 {
                grid[j * n + i] = projected[k];
                k += 1;
            }
        }
        let p = fit_surface(&s, &s, &grid, spans, self.settings.order, Some(coefs))?;
        let error = p
            .grid(&s, &s)
            .iter()
            .zip(&grid)
            .map(|(a, b)| a.distance(*b))
            .fold(0., f64::max);
        Ok((p, grid, error))
    }

    pub(super) fn fit(&self, n: usize, spans: usize, edge: &[DVec3]) -> Result<Vec<DVec3>> {
        fit_curve(&unit_grid(n), edge, spans, self.settings.order)
    }

    /// The first fit over SPANS whose patches all meet the fit tolerance (the last one otherwise).
    pub(super) fn adaptive(
        &mut self,
        f: i32,
        mut make: impl FnMut(&mut Self, usize, usize) -> Result<Vec<Fit>>,
    ) -> Result<Fits> {
        let mut last = None;
        let mut previous = f64::INFINITY;
        for spans in SPANS {
            let n = self.settings.nsample.max(4 * spans + 1);
            let fits = make(self, spans, n)?;
            let mut cads = Vec::new();
            for (p, _, error) in &fits {
                cads.push(cad_distance(self.gmsh, f, p, 2 * n - 1)?.max(*error));
            }
            let worst = cads.iter().copied().fold(0., f64::max);
            last = Some((fits, cads, spans));
            // More spans no longer help when the error hardly falls: the layout does not lie on the face.
            if worst <= self.tol() || worst > STALLED * previous {
                break;
            }
            previous = worst;
        }
        last.ok_or_else(|| anyhow::anyhow!("No spans tried"))
    }

    /// The angle (degrees) the normal turns along the middle iso-line of the parameter box.
    pub(super) fn turning(&self, f: i32, lo: &[f64], hi: &[f64], axis: usize) -> Result<f64> {
        let g = unit_grid(65);
        let mid = 0.5 * (lo[1 - axis] + hi[1 - axis]);
        let uv: Vec<f64> = g
            .iter()
            .flat_map(|&x| {
                let along = lo[axis] + x * (hi[axis] - lo[axis]);
                if axis == 0 {
                    [along, mid]
                } else {
                    [mid, along]
                }
            })
            .collect();
        let normals: Vec<DVec3> = self
            .gmsh
            .normal(f, &uv)?
            .into_iter()
            .map(DVec3::from_array)
            .collect();
        Ok(normals
            .windows(2)
            .map(|w| w[1].dot(w[0]).clamp(-1., 1.).acos().to_degrees())
            .sum())
    }
}

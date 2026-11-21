// SPDX-License-Identifier: GPL-3.0-or-later
//! The split path: a quad mesh of the faces, its quads fitted with their edges shared between neighbours,
//! remeshed finer while quads fold.

use super::*;

impl Converter<'_> {
    /// Mesh the model with patch sizes per face (other faces one element across) and keep the quads of
    /// `faces` (triangles split in three quads through their centre and edge midpoints).
    pub(super) fn mesh(&mut self, faces: &[i32], sizes: &BTreeMap<i32, f64>) -> Result<()> {
        let gmsh = self.gmsh;
        // The split (tri3) and the subdivision halve the element size.
        let k = if self.settings.method == PatchMethod::Qqs {
            1.
        } else {
            2.
        };
        gmsh.clear_mesh()?;
        self.edges.clear();
        self.curves.clear();
        self.face_curves.clear();
        let mut table: SizeTable = HashMap::new();
        for f in gmsh.tags(2)? {
            table.insert(
                (2, f),
                k * sizes.get(&f).copied().unwrap_or(self.settings.scale),
            );
        }
        // A curve takes the size of its faces of the highest priority (the smallest of those).
        let priority = |f: i32| self.settings.faces.get(&f).map_or(0, |s| s.priority);
        for c in gmsh.tags(1)? {
            let mut up = gmsh.adjacencies(1, c)?.0;
            if self.alone {
                // Meshed alone: the face's own curves at its own size, whatever its neighbours want.
                up.retain(|f| faces.contains(f));
            }
            let top = up.iter().map(|&f| priority(f)).max().unwrap_or(0);
            let size = up
                .iter()
                .filter(|&&f| priority(f) == top)
                .filter_map(|f| table.get(&(2, *f)))
                .copied()
                .fold(f64::INFINITY, f64::min);
            table.insert(
                (1, c),
                if size.is_finite() {
                    size
                } else {
                    k * self.settings.size
                },
            );
        }
        for p in gmsh.tags(0)? {
            let up = gmsh.adjacencies(0, p)?.0;
            let size = up
                .iter()
                .filter_map(|c| table.get(&(1, *c)))
                .copied()
                .fold(f64::INFINITY, f64::min);
            table.insert(
                (0, p),
                if size.is_finite() {
                    size
                } else {
                    k * self.settings.size
                },
            );
        }
        let lo = table.values().copied().fold(f64::INFINITY, f64::min);
        let hi = table.values().copied().fold(0., f64::max);
        gmsh.set_sizes(table)?;
        for (key, value) in [
            ("Mesh.MeshSizeMin", lo),
            ("Mesh.MeshSizeMax", hi),
            ("Mesh.MeshSizeFromCurvature", 0.),
            ("Mesh.MeshSizeFromPoints", 0.),
        ] {
            gmsh.number(key, value)?;
        }
        let method: &[(&str, f64)] = match self.settings.method {
            PatchMethod::Qqs => &[("Mesh.Algorithm", 11.)],
            PatchMethod::Subdiv => &[
                ("Mesh.Algorithm", 8.),
                ("Mesh.RecombineAll", 1.),
                ("Mesh.RecombinationAlgorithm", 1.),
                ("Mesh.SubdivisionAlgorithm", 1.),
            ],
            PatchMethod::Tri3 => &[
                ("Mesh.Algorithm", 6.),
                ("Mesh.RecombineAll", 0.),
                ("Mesh.SubdivisionAlgorithm", 0.),
            ],
        };
        for &(key, value) in method {
            gmsh.number(key, value)?;
        }
        if self.alone {
            // Only these faces (and their curves) are meshed.
            let all: Vec<_> = gmsh.tags(2)?.into_iter().map(|t| (2, t)).collect();
            gmsh.set_visibility(&all, false, false)?;
            let shown: Vec<_> = faces.iter().map(|&t| (2, t)).collect();
            gmsh.set_visibility(&shown, true, true)?;
            gmsh.number("Mesh.MeshOnlyVisible", 1.)?;
        }
        let generated = gmsh.generate(2);
        if self.alone {
            let all: Vec<_> = gmsh.tags(2)?.into_iter().map(|t| (2, t)).collect();
            gmsh.set_visibility(&all, true, true)?;
            gmsh.number("Mesh.MeshOnlyVisible", 0.)?;
        }
        generated?;
        gmsh.clear_sizes()?;
        let (tags, xyz) = gmsh.nodes()?;
        self.xyz = tags
            .into_iter()
            .zip(xyz.into_iter().map(DVec3::from_array))
            .collect();
        // snap nodes onto their curve or face
        for dim in [1, 2] {
            for e in gmsh.tags(dim)? {
                let (nt, nx) = gmsh.entity_nodes(dim, e)?;
                if nt.is_empty() {
                    continue;
                }
                let snapped = gmsh.closest_point(dim, e, &nx)?.0;
                for (t, c) in nt.into_iter().zip(snapped) {
                    self.xyz.insert(t, DVec3::from_array(c));
                }
            }
        }
        for c in gmsh.tags(1)? {
            let faces_of_curve = gmsh.adjacencies(1, c)?.0;
            for (_, nodes) in gmsh.elements(1, c)? {
                for pair in nodes.as_chunks::<2>().0 {
                    let ends = (pair[0].min(pair[1]), pair[0].max(pair[1]));
                    self.curves.insert(ends, c);
                    for &face in &faces_of_curve {
                        self.face_curves.insert((face, ends.0, ends.1), c);
                    }
                }
            }
        }
        self.quads.clear();
        let mut cells = Vec::new();
        for &f in faces {
            for (kind, nodes) in gmsh.elements(2, f)? {
                let nn = gmsh.element_nodes(kind)?;
                if nn == 4 {
                    self.quads
                        .extend(nodes.as_chunks::<4>().0.iter().map(|q| (f, *q)));
                } else if nn == 3 && self.settings.method == PatchMethod::Tri3 {
                    cells.extend(nodes.as_chunks::<3>().0.iter().map(|t| (f, *t)));
                }
            }
        }
        self.renumber_by_position(&mut cells);
        self.tri3(&cells)
    }

    /// Number the mesh nodes in the order of their positions, start every element at its smallest node (its
    /// orientation kept) and sort the elements. Gmsh numbers the same mesh differently after other meshes of the
    /// session (and in another worker process), and the patches followed those numbers: their order, their first
    /// corners and the direction in which each shared edge is sampled, whose last bits then decided folds.
    pub(super) fn renumber_by_position(&mut self, cells: &mut [(i32, [usize; 3])]) {
        let mut tags: Vec<usize> = self.xyz.keys().copied().collect();
        tags.sort_by(|a, b| {
            let (p, q) = (self.xyz[a], self.xyz[b]);
            p.x.total_cmp(&q.x)
                .then(p.y.total_cmp(&q.y))
                .then(p.z.total_cmp(&q.z))
                .then(a.cmp(b))
        });
        let new: HashMap<usize, usize> =
            tags.iter().enumerate().map(|(i, &t)| (t, i + 1)).collect();
        self.xyz = tags
            .iter()
            .enumerate()
            .map(|(i, t)| (i + 1, self.xyz[t]))
            .collect();
        let pair = |a: usize, b: usize| {
            let (a, b) = (new[&a], new[&b]);
            (a.min(b), a.max(b))
        };
        self.curves = self
            .curves
            .iter()
            .map(|(&(a, b), &c)| (pair(a, b), c))
            .collect();
        self.face_curves = self
            .face_curves
            .iter()
            .map(|(&(f, a, b), &c)| {
                let (a, b) = pair(a, b);
                ((f, a, b), c)
            })
            .collect();
        for q in &mut self.quads {
            q.1 = start_at_smallest(q.1.map(|t| new[&t]));
        }
        self.quads.sort_unstable();
        for c in cells.iter_mut() {
            c.1 = start_at_smallest(c.1.map(|t| new[&t]));
        }
        cells.sort_unstable();
    }

    /// The mean of node parameters on the owning entity, evaluated forward (periodic angles unwrapped).
    pub(super) fn mid_point(
        &self,
        face: i32,
        nodes: &[usize],
        curve: Option<i32>,
    ) -> Result<DVec3> {
        let xyz: Vec<[f64; 3]> = nodes.iter().map(|k| self.xyz[k].to_array()).collect();
        let (dim, tag) = curve.map_or((2, face), |c| (1, c));
        let mut uv: Vec<Vec<f64>> = if dim == 1 {
            xyz.iter()
                .map(|x| Ok(vec![self.gmsh.parametrization(1, tag, &[*x])?[0]]))
                .collect::<Result<_>>()?
        } else {
            self.gmsh
                .closest_point(2, tag, &xyz)?
                .1
                .chunks(2)
                .map(|c| c.to_vec())
                .collect()
        };
        let (lo, hi) = self.gmsh.parametrization_bounds(dim, tag)?;
        let kind = self.gmsh.kind(dim, tag)?;
        if matches!(kind.as_str(), "Circle" | "Ellipse" | "Cylinder")
            && hi[0] - lo[0] > std::f64::consts::PI + 1e-8
        {
            let u0 = uv[0][0];
            for p in uv.iter_mut() {
                p[0] = u0 + (p[0] - u0 + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
                    - std::f64::consts::PI;
            }
        }
        let mean: Vec<f64> = (0..uv[0].len())
            .map(|d| uv.iter().map(|p| p[d]).sum::<f64>() / uv.len() as f64)
            .collect();
        Ok(self.values(dim, tag, &mean)?[0])
    }

    pub(super) fn tri3(&mut self, cells: &[(i32, [usize; 3])]) -> Result<()> {
        let mut midpoints: HashMap<(bool, i32, usize, usize), usize> = HashMap::new();
        let mut next = self.xyz.keys().copied().max().unwrap_or(0) + 1;
        for &(face, [a, b, c]) in cells {
            let mut mids = Vec::new();
            for (u, v) in [(a, b), (b, c), (c, a)] {
                let ends = (u.min(v), u.max(v));
                let curve = self.face_curves.get(&(face, ends.0, ends.1)).copied();
                let key = match curve {
                    Some(cv) => (true, cv, ends.0, ends.1),
                    None => (false, face, ends.0, ends.1),
                };
                if let std::collections::hash_map::Entry::Vacant(entry) = midpoints.entry(key) {
                    entry.insert(next);
                    let p = self.mid_point(face, &[u, v], curve)?;
                    self.xyz.insert(next, p);
                    next += 1;
                }
                let midpoint = midpoints[&key];
                if let Some(cv) = curve {
                    for end in [u, v] {
                        let segment = (end.min(midpoint), end.max(midpoint));
                        self.curves.insert(segment, cv);
                        self.face_curves.insert((face, segment.0, segment.1), cv);
                    }
                }
                mids.push(midpoint);
            }
            let centre = next;
            let p = self.mid_point(face, &[a, b, c], None)?;
            self.xyz.insert(centre, p);
            next += 1;
            let (ab, bc, ca) = (mids[0], mids[1], mids[2]);
            self.quads.extend([
                (face, [a, ab, centre, ca]),
                (face, [b, bc, centre, ab]),
                (face, [c, ca, centre, bc]),
            ]);
        }
        Ok(())
    }

    /// The points of the mesh edge a -> b and their fitted B-spline coefficients, shared by both quads.
    pub(super) fn edge(
        &mut self,
        face: i32,
        a: usize,
        b: usize,
    ) -> Result<(Vec<DVec3>, Vec<DVec3>, &'static str)> {
        let ends = (a.min(b), a.max(b));
        let curve = self.face_curves.get(&(face, ends.0, ends.1)).copied();
        let key = match curve {
            Some(cv) => (true, cv, ends.0, ends.1),
            None => (false, face, ends.0, ends.1),
        };
        if !self.edges.contains_key(&key) {
            let n = self.settings.nsample;
            let tol = 1e-6 * self.settings.scale;
            let (pa, pb) = (self.xyz[&ends.0], self.xyz[&ends.1]);
            let u = unit_grid(n);
            let (mut points, mut how) = if let Some(cv) = curve {
                let ta = self.gmsh.parametrization(1, cv, &[pa.to_array()])?[0];
                let tb = self.gmsh.parametrization(1, cv, &[pb.to_array()])?[0];
                let param: Vec<f64> = u.iter().map(|&x| (1. - x) * ta + x * tb).collect();
                (self.values(1, cv, &param)?, "curve")
            } else {
                let uva = self.gmsh.parametrization(2, face, &[pa.to_array()])?;
                let mut uvb = self.gmsh.parametrization(2, face, &[pb.to_array()])?;
                let (lo, hi) = self.gmsh.parametrization_bounds(2, face)?;
                if self.gmsh.kind(2, face)? == "Cylinder"
                    && hi[0] - lo[0] > std::f64::consts::PI + 1e-8
                {
                    uvb[0] = uva[0]
                        + (uvb[0] - uva[0] + std::f64::consts::PI)
                            .rem_euclid(std::f64::consts::TAU)
                        - std::f64::consts::PI;
                }
                let param: Vec<f64> = u
                    .iter()
                    .flat_map(|&x| {
                        [
                            (1. - x) * uva[0] + x * uvb[0],
                            (1. - x) * uva[1] + x * uvb[1],
                        ]
                    })
                    .collect();
                (self.values(2, face, &param)?, "face")
            };
            if points[0].distance(pa) > tol || points[n - 1].distance(pb) > tol {
                // periodic or ill-posed parameters: the chord projected onto the face
                let chord: Vec<DVec3> = u.iter().map(|&x| pa * (1. - x) + pb * x).collect();
                points = self.project(2, face, &chord)?;
                how = "chord";
            }
            points[0] = pa;
            points[n - 1] = pb;
            let coef = self.fit(n, self.settings.spans, &points)?;
            self.edges.insert(key, (points, coef, how));
        }
        let (points, coef, how) = self.edges[&key].clone();
        Ok(if a == ends.0 {
            (points, coef, how)
        } else {
            (reversed(&points), reversed(&coef), how)
        })
    }

    /// One patch per quad of the mesh; folded patches get more interior spans when that removes folds.
    pub(super) fn quad_patches(&mut self) -> Result<Vec<QuadPatch>> {
        let mut out = Vec::new();
        let quads = self.quads.clone();
        let (spans, order, nsample) = (
            self.settings.spans,
            self.settings.order,
            self.settings.nsample,
        );
        for (face, [n0, n1, n2, n3]) in quads {
            let e = [
                self.edge(face, n0, n1)?,
                self.edge(face, n1, n2)?,
                self.edge(face, n3, n2)?,
                self.edge(face, n0, n3)?,
            ];
            let (mut p, grid, mut error) = self.fit_edges(
                face,
                [&e[0].0, &e[1].0, &e[2].0, &e[3].0],
                [&e[0].1, &e[1].1, &e[2].1, &e[3].1],
                spans,
            )?;
            let mut folds = checks::jacobian(&p, 21).1;
            if folds > 0 {
                let g = unit_grid(nsample);
                let mut distance = cad_distance(self.gmsh, face, &p, 2 * nsample - 1)?;
                for more in [2 * spans, 4 * spans] {
                    if more + order > nsample {
                        break;
                    }
                    let mut boundaries = Vec::new();
                    for (_, coefficients, _) in &e {
                        let mut knots = bspline::uniform_knots(spans, order);
                        let mut c = coefficients.clone();
                        for knot in &linspace(0., 1., more + 1)[1..more] {
                            if !knots.iter().any(|k| (k - knot).abs() <= 1e-12) {
                                (knots, c) = bspline::insert_knot(&knots, order, &c, *knot);
                            }
                        }
                        boundaries.push(c);
                    }
                    let candidate = fit_surface(
                        &g,
                        &g,
                        &grid,
                        more,
                        order,
                        Some([
                            &boundaries[0],
                            &boundaries[1],
                            &boundaries[2],
                            &boundaries[3],
                        ]),
                    )?;
                    let candidate_folds = checks::jacobian(&candidate, 21).1;
                    let candidate_distance =
                        cad_distance(self.gmsh, face, &candidate, 2 * nsample - 1)?;
                    if candidate_folds < folds && candidate_distance <= distance {
                        error = candidate
                            .grid(&g, &g)
                            .iter()
                            .zip(&grid)
                            .map(|(a, b)| a.distance(*b))
                            .fold(0., f64::max);
                        p = candidate;
                        folds = candidate_folds;
                        distance = candidate_distance;
                    }
                    if folds == 0 {
                        break;
                    }
                }
            }
            let cad_error = cad_distance(self.gmsh, face, &p, 2 * nsample - 1)?;
            out.push(QuadPatch {
                face,
                samples: grid,
                n: nsample,
                patch: p,
                sample_error: error,
                cad_error,
                path: "split",
                face_type: self.gmsh.kind(2, face)?,
                exact: None,
                nodes: Some([n0, n1, n2, n3]),
                disjoint_group: -1,
                repair_error: None,
            });
        }
        Ok(out)
    }

    /// Mesh the faces and refine at most twice, preferring fewer folded patches before unfair corners.
    /// [`Self::split`] of all the faces together, or of each on its own (`split_each`).
    pub(super) fn split_faces(&mut self, faces: &[i32]) -> Result<Vec<QuadPatch>> {
        if !self.split_each {
            return self.split(faces);
        }
        self.alone = true;
        let mut out = Vec::new();
        let result = (|| -> Result<()> {
            for &f in faces {
                out.extend(self.split(&[f])?);
            }
            Ok(())
        })();
        self.alone = false;
        result.map(|()| out)
    }

    pub(super) fn split(&mut self, faces: &[i32]) -> Result<Vec<QuadPatch>> {
        if faces.is_empty() {
            return Ok(Vec::new());
        }
        let mut sizes: BTreeMap<i32, f64> = faces
            .iter()
            .map(|&f| {
                let own = self.settings.faces.get(&f).and_then(|s| s.patch_size);
                (f, own.unwrap_or(self.settings.size))
            })
            .collect();
        let first = sizes.clone();
        let mut best: BTreeMap<i32, Best> = BTreeMap::new();
        for _ in 0..3 {
            self.mesh(faces, &sizes)?;
            let mut by_face: BTreeMap<i32, Vec<QuadPatch>> = BTreeMap::new();
            for q in self.quad_patches()? {
                by_face.entry(q.face).or_default().push(q);
            }
            for &f in faces {
                let qs = by_face.remove(&f).unwrap_or_default();
                let bad = qs.iter().filter(|q| !fair(&[&q.patch])).count();
                let folded = qs
                    .iter()
                    .filter(|q| checks::jacobian(&q.patch, 21).1 > 0)
                    .count();
                let rank = (folded, bad);
                if best.get(&f).is_none_or(|b| rank < b.0) {
                    best.insert(f, (rank, qs, sizes[&f]));
                }
            }
            let retry: Vec<i32> = faces.iter().copied().filter(|f| best[f].0.1 > 0).collect();
            if retry.is_empty() {
                break;
            }
            for f in retry {
                *sizes.get_mut(&f).unwrap_or(&mut 0.) *= 0.7;
            }
        }
        let mut out = Vec::new();
        for &f in faces {
            let (_, qs, size) = best.remove(&f).unwrap_or(((0, 0), Vec::new(), first[&f]));
            out.extend(qs);
            if size != first[&f] {
                self.remeshed.insert(f, (size * 1000.).round() / 1000.);
            }
        }
        Ok(out)
    }
}

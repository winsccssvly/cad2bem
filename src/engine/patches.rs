// SPDX-License-Identifier: GPL-3.0-or-later
//! Four-sided B-spline patches of prepared CAD faces.
//!
//! Each face tries, in order: an O-grid (one smooth loop: a disc) or a ring (two smooth loops), a fit in the
//! face's own parameters (`direct`: untrimmed four sides, or three with a pole), one Coons patch of its four
//! trimmed sides (`single`), N quads of an N-sided face (`polygon`); a path must meet the fit tolerance and,
//! except `direct`, have fair corners and no fold. Every other face is meshed with quads (`split`) and its
//! quads are fitted with their edges shared exactly between neighbours, remeshed finer while quads are folded
//! or unfair. Fitted patches that lie on an analytic surface become exact rational patches.
//! Untrimmed faces with a B-spline surface stored in the file keep its poles (`native`); folded quads on
//! trimmed cylinders are repaired by [`super::trim_repair`].

use super::{
    bspline::{self, Patch, fit_curve, fit_surface, unit_grid},
    checks,
    exact::{Grid, edge_error, exact_patch, revolved_patch},
    prepare::linspace,
};
use crate::{
    cad::session::{Session, SizeTable},
    conversion::{FacePath, FaceSetting, PatchMethod},
};
use anyhow::{Context, Result, ensure};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

/// The span counts the adaptive paths try, coarse first.
const SPANS: [usize; 6] = [3, 4, 6, 8, 12, 16];
/// Corner angles (degrees) of the coarsest bridge quads outside which a split is not refined: 10 degrees
/// beyond the fair range (in the examples, no split skipped this way would have been kept).
const BRIDGE_HOPELESS: (f64, f64) = (20., 160.);
/// An adaptive fit stops when one more span step leaves more than this share of the error. Fits that met
/// the tolerance in the examples fell to at most 0.68 of it at every step; the ones that never met it mostly
/// stayed flat or grew.
const STALLED: f64 = 0.9;
/// Interior angle (degrees) above which two curves meet smoothly (no corner): the largest corner a fair patch
/// may have, so that a face is never given a corner no patch can take (a kink of 160 degrees made a four-sided
/// face that no layout fitted, and it was split into small patches).
const SMOOTH: f64 = 150.;

/// A fitted patch of a face, with its samples and errors.
#[derive(Clone, Debug)]
pub struct QuadPatch {
    pub face: i32,
    /// The sample grid (n rows of n) the patch was fitted to.
    pub samples: Vec<DVec3>,
    pub n: usize,
    pub patch: Patch,
    pub sample_error: f64,
    /// The largest distance of the patch to its CAD face.
    pub cad_error: f64,
    pub path: &'static str,
    pub face_type: String,
    /// `rational` or `polynomial` once replaced by an exact patch.
    pub exact: Option<&'static str>,
    /// The mesh nodes of a split quad (its corners), for the trim repair.
    pub nodes: Option<[usize; 4]>,
    /// Patches of one repaired region: they meet without overlap by construction (-1: none).
    pub disjoint_group: i64,
    /// Why a folded region could not be repaired.
    pub repair_error: Option<String>,
}

/// Settings of the patch conversion.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Patch size (m) of the faces that must be meshed.
    pub size: f64,
    /// The body size: tolerances are relative to it.
    pub scale: f64,
    /// Fit tolerance relative to the body size.
    pub fit_tol: f64,
    pub exact: bool,
    pub spans: usize,
    pub order: usize,
    pub nsample: usize,
    /// The quad mesher of split faces.
    pub method: PatchMethod,
    /// Settings of single faces: a forced path, the patch size of a split face, the priority on shared curves.
    pub faces: BTreeMap<i32, FaceSetting>,
}

impl Settings {
    /// Settings of patch size `size` (m) for a body of size `scale` with the default fit, order and sampling;
    /// `exact` replaces fitted patches by exact ones where the CAD surface allows.
    pub fn new(size: f64, scale: f64, exact: bool) -> Self {
        Self {
            size,
            scale,
            fit_tol: 1e-6,
            exact,
            spans: 3,
            order: 4,
            nsample: 21,
            method: PatchMethod::Tri3,
            faces: BTreeMap::new(),
        }
    }
}

/// One fitted patch: (patch, sample grid, sample error).
type Fit = (Patch, Vec<DVec3>, f64);
/// The fits of a face, their CAD errors and the spans used.
type Fits = (Vec<Fit>, Vec<f64>, usize);
/// Curves of a loop with dense samples in loop direction.
type Chain = Vec<(i32, Vec<DVec3>)>;
/// A mesh edge by owner (on a curve, or inside a face) and end nodes.
type EdgeKey = (bool, i32, usize, usize);
/// The samples of a mesh edge, their curve coefficients and how they were taken.
type EdgeFit = (Vec<DVec3>, Vec<DVec3>, &'static str);
/// The best quads of a face so far: (folded, unfair) counts, patches and the size used.
type Best = ((usize, usize), Vec<QuadPatch>, f64);

fn row(grid: &[DVec3], n: usize, j: usize) -> Vec<DVec3> {
    grid[j * n..(j + 1) * n].to_vec()
}

fn column(grid: &[DVec3], n: usize, i: usize) -> Vec<DVec3> {
    (0..n).map(|j| grid[j * n + i]).collect()
}

fn reversed<T: Clone>(v: &[T]) -> Vec<T> {
    v.iter().rev().cloned().collect()
}

fn angle_degrees(u: DVec3, v: DVec3) -> f64 {
    (u.dot(v) / (u.length() * v.length()))
        .clamp(-1., 1.)
        .acos()
        .to_degrees()
}

/// The largest distance of an n x n grid of the patch to its CAD face.
pub fn cad_distance(gmsh: &Session, face: i32, patch: &Patch, n: usize) -> Result<f64> {
    let g = unit_grid(n);
    let x = patch.grid(&g, &g);
    let (closest, _) =
        gmsh.closest_point(2, face, &x.iter().map(|p| p.to_array()).collect::<Vec<_>>())?;
    Ok(x.iter()
        .zip(closest)
        .map(|(p, c)| p.distance(DVec3::from_array(c)))
        .fold(0., f64::max))
}

/// Whether patches have fair corners (30 to 150 degrees) and no fold.
fn fair(patches: &[&Patch]) -> bool {
    fair_down_to(patches, 30.)
}

/// [`fair`], with corners down to `low` degrees allowed: a face whose own corner is sharper than 30 degrees
/// gives that corner to one of its patches whatever the layout.
fn fair_down_to(patches: &[&Patch], low: f64) -> bool {
    patches.iter().all(|p| {
        let a = checks::corner_angles(p);
        a.iter().all(|&x| (low..=150.).contains(&x)) && checks::jacobian(p, 21).1 == 0
    })
}

/// The cycle of element nodes started at its smallest node (the orientation kept).
fn start_at_smallest<const N: usize>(nodes: [usize; N]) -> [usize; N] {
    let k = (0..N).min_by_key(|&i| nodes[i]).unwrap_or(0);
    std::array::from_fn(|i| nodes[(i + k) % N])
}

/// The interior angles (degrees) at the corners of a boundary chain (sides from corner to corner).
fn chain_corners(c: &[Vec<DVec3>]) -> Vec<f64> {
    (0..c.len())
        .map(|k| {
            let before = &c[(k + c.len() - 1) % c.len()];
            let at = c[k][0];
            angle_degrees(before[before.len() - 2] - at, c[k][1] - at)
        })
        .collect()
}

/// A dense closed polyline with the curve owning each point.
#[derive(Clone)]
struct Loop {
    points: Vec<DVec3>,
    owner: Vec<i32>,
    /// Whether each point is a CAD vertex (where one curve of the loop ends and the next begins).
    vertex: Vec<bool>,
}

impl Loop {
    /// The indices of the CAD vertices on the loop.
    fn vertices(&self) -> Vec<usize> {
        (0..self.vertex.len()).filter(|&k| self.vertex[k]).collect()
    }

    /// Where a four-sided layout of the loop starts: the CAD vertex with the largest x (y breaking ties), so
    /// that its corners land exactly where the faces sharing the loop have theirs (a wall split from its
    /// seam), not at a sample point near them.
    fn layout_start(&self) -> usize {
        let key = |p: DVec3| p.x + 1e-3 * p.y;
        self.vertices()
            .into_iter()
            .max_by(|&a, &b| key(self.points[a]).total_cmp(&key(self.points[b])))
            .unwrap_or(0)
    }

    fn arc(&self) -> Vec<f64> {
        let mut out = vec![0.];
        for k in 0..self.points.len() {
            let next = self.points[(k + 1) % self.points.len()];
            out.push(out[k] + self.points[k].distance(next));
        }
        out
    }

    fn length(&self) -> f64 {
        *self.arc().last().unwrap_or(&0.)
    }

    fn reversed(&self) -> Loop {
        Loop {
            points: reversed(&self.points),
            owner: reversed(&self.owner),
            vertex: reversed(&self.vertex),
        }
    }
}

/// Linear interpolation of a polyline (closed: the first point repeated at the end) at arc lengths.
fn interpolate(arc: &[f64], closed: &[DVec3], target: f64) -> (DVec3, usize) {
    let k = arc
        .partition_point(|&a| a <= target)
        .saturating_sub(1)
        .min(arc.len() - 2);
    let span = arc[k + 1] - arc[k];
    let f = if span > 0. {
        ((target - arc[k]) / span).clamp(0., 1.)
    } else {
        0.
    };
    (closed[k] + (closed[k + 1] - closed[k]) * f, k)
}

/// Sampled sides of a boundary loop: per side, (curve, dense samples in loop direction).
type Sides = Vec<Vec<(i32, Vec<DVec3>)>>;

/// The patch conversion of the prepared faces of the open model.
pub struct Converter<'a> {
    gmsh: &'a Session,
    settings: Settings,
    xyz: HashMap<usize, DVec3>,
    curves: HashMap<(usize, usize), i32>,
    face_curves: HashMap<(i32, usize, usize), i32>,
    edges: HashMap<EdgeKey, EdgeFit>,
    quads: Vec<(i32, [usize; 4])>,
    side_cache: HashMap<Vec<i32>, Option<Sides>>,
    /// The path of every face.
    pub paths: BTreeMap<i32, &'static str>,
    /// Faces meshed at a finer size than asked, and that size.
    pub remeshed: BTreeMap<i32, f64>,
    /// Faces whose forced path did not fit: they were split instead.
    pub forced_failed: Vec<i32>,
    /// The split faces were made with tri3 instead of the chosen method, which left folded patches.
    pub method_fallback: bool,
    /// Split each face on its own (its curves at its own size, the other faces hidden from the mesher), so that
    /// a face does not change with its neighbours; otherwise all split faces in one mesh with shared edges.
    pub split_each: bool,
    /// Inside [`Self::split_faces`] with `split_each`: the faces being meshed alone.
    alone: bool,
    /// The B-spline surfaces stored in the CAD file (the `native` path).
    pub surfaces: Vec<super::surfaces::Surface>,
}

impl<'a> Converter<'a> {
    /// A converter of the faces of the model in this Gmsh session.
    pub fn new(gmsh: &'a Session, settings: Settings) -> Self {
        Self {
            gmsh,
            settings,
            xyz: HashMap::new(),
            curves: HashMap::new(),
            face_curves: HashMap::new(),
            edges: HashMap::new(),
            quads: Vec::new(),
            side_cache: HashMap::new(),
            paths: BTreeMap::new(),
            remeshed: BTreeMap::new(),
            forced_failed: Vec::new(),
            method_fallback: false,
            split_each: true,
            alone: false,
            surfaces: Vec::new(),
        }
    }

    fn tol(&self) -> f64 {
        self.settings.fit_tol * self.settings.scale
    }

    fn values(&self, dim: i32, tag: i32, param: &[f64]) -> Result<Vec<DVec3>> {
        Ok(self
            .gmsh
            .value(dim, tag, param)?
            .into_iter()
            .map(DVec3::from_array)
            .collect())
    }

    fn project(&self, dim: i32, tag: i32, points: &[DVec3]) -> Result<Vec<DVec3>> {
        let input: Vec<[f64; 3]> = points.iter().map(|p| p.to_array()).collect();
        Ok(self
            .gmsh
            .closest_point(dim, tag, &input)?
            .0
            .into_iter()
            .map(DVec3::from_array)
            .collect())
    }

    /// The patches of every face.
    pub fn patches(&mut self, faces: &[i32]) -> Result<Vec<QuadPatch>> {
        let mut out = Vec::new();
        let mut split = Vec::new();
        let tol = self.tol();
        for &f in faces {
            let forced = self
                .settings
                .faces
                .get(&f)
                .map(|s| s.path)
                .unwrap_or_default();
            if forced == FacePath::Split {
                split.push(f);
                self.paths.insert(f, "split");
                continue;
            }
            let curves = self.boundary(f)?;
            let loops = self.gmsh.curve_loops(f)?;
            let smooth: Vec<Option<Loop>> = if loops.len() <= 2 {
                loops
                    .iter()
                    .map(|lp| self.smooth_loop(&lp.iter().map(|c| c.abs()).collect::<Vec<_>>()))
                    .collect::<Result<_>>()?
            } else {
                vec![None]
            };
            let layouts = matches!(forced, FacePath::Auto | FacePath::Ogrid | FacePath::Ring);
            if layouts && smooth.iter().all(Option::is_some) && !smooth.is_empty() {
                let smooth: Vec<Loop> = smooth.into_iter().flatten().collect();
                let (name, result) = if smooth.len() == 1 {
                    ("ogrid", Some(self.ogrid(f, &smooth[0])?))
                } else {
                    let (outer, inner) = if smooth[0].length() >= smooth[1].length() {
                        (&smooth[0], &smooth[1])
                    } else {
                        (&smooth[1], &smooth[0])
                    };
                    ("ring", self.ring(f, outer, inner)?)
                };
                let wanted = forced == FacePath::Auto || forced.name() == name;
                if wanted && let Some((fits, cads, _)) = result {
                    let patches: Vec<&Patch> = fits.iter().map(|x| &x.0).collect();
                    if cads.iter().copied().fold(0., f64::max) <= tol && fair(&patches) {
                        self.add(&mut out, f, name, fits, cads)?;
                        self.paths.insert(f, name);
                        continue;
                    }
                }
            }
            let sides = match &curves {
                Some(c) => self.sides(c)?,
                None => None,
            };
            let mut path = "split";
            if let (Some(sides), Some(curves)) = (&sides, &curves)
                && (3..=8).contains(&sides.len())
            {
                let mut tries = Vec::new();
                // A corner of the face sharper than 30 degrees is the face's own: every layout gives it to a
                // patch, so the layouts are judged down to it (otherwise all fail and the face is split into
                // small patches with sharper corners still).
                let c = self.chain(curves, 9)?.unwrap_or_default();
                let sharpest = chain_corners(&c).into_iter().fold(180., f64::min);
                let low = (sharpest - 1.).min(30.);
                if sides.len() == 4 {
                    if self.untrimmed(f, curves)? {
                        if !self.surfaces.is_empty() {
                            tries.push("native");
                        }
                        tries.push("direct");
                    } else if c.len() == 4 {
                        tries.push("single");
                    }
                } else if sides.len() == 3 && self.revolution(f)? && self.untrimmed(f, curves)? {
                    tries.push("direct"); // a side of the parameter box collapsed to a pole
                }
                if (5..=6).contains(&sides.len()) {
                    tries.push("bridge");
                }
                tries.push("polygon");
                if sides.len() == 3 {
                    // After polygon: three quads of a disc piece become exact rational patches, a folded one does not.
                    tries.push("triangle");
                }
                // A forced path is the only one tried; single may be forced on a face with sharp corners.
                if forced == FacePath::Single && sides.len() == 4 && !tries.contains(&"single") {
                    tries = vec!["single"];
                } else if forced != FacePath::Auto {
                    tries.retain(|t| *t == forced.name());
                }
                for name in tries {
                    let result = match name {
                        "native" => self.native(f)?,
                        "direct" => Some(self.direct(f)?),
                        "single" => self.single(f, curves)?,
                        "triangle" => self.triangle(f, curves)?,
                        "bridge" => self.bridge(f, curves)?,
                        _ => self.polygon(f, curves)?,
                    };
                    let Some((fits, cads, _)) = result else {
                        continue;
                    };
                    let patches: Vec<&Patch> = fits.iter().map(|x| &x.0).collect();
                    if cads.iter().copied().fold(0., f64::max) <= tol
                        && (name == "direct" || name == "native" || fair_down_to(&patches, low))
                    {
                        self.add(&mut out, f, name, fits, cads)?;
                        path = name;
                        break;
                    }
                }
            }
            if path == "split" {
                split.push(f);
                if forced != FacePath::Auto {
                    self.forced_failed.push(f);
                }
            }
            self.paths.insert(f, path);
        }
        let mut split_out = self.split_faces(&split)?;
        if self.settings.method != PatchMethod::Tri3
            && split_out
                .iter()
                .any(|q| checks::jacobian(&q.patch, 21).1 > 0)
        {
            // Folded patches of another method cannot be repaired: all split faces are made again with tri3
            // (together, so that they still meet), whose folds on cylinders are repaired below.
            let method = self.settings.method;
            self.settings.method = PatchMethod::Tri3;
            for f in &split {
                self.remeshed.remove(f);
            }
            let again = self.split_faces(&split);
            self.settings.method = method;
            split_out = again?;
            self.method_fallback = true;
        }
        out.extend(split_out);
        if !self.settings.exact {
            return Ok(out);
        }
        self.make_exact(&mut out)?;
        if self.settings.method != PatchMethod::Tri3 && !self.method_fallback {
            return Ok(out);
        }
        // Folded quads of split cylinders: repair them in an unrolled chart.
        let mut frames = BTreeMap::new();
        for q in &out {
            if q.path == "split"
                && q.face_type == "Cylinder"
                && !frames.contains_key(&q.face)
                && checks::jacobian(&q.patch, 21).1 > 0
            {
                frames.insert(q.face, super::trim_repair::frame(self.gmsh, q.face)?);
            }
        }
        if frames.is_empty() {
            return Ok(out);
        }
        Ok(super::trim_repair::repair(
            self.gmsh,
            out,
            &frames,
            self.tol(),
        ))
    }

    fn add(
        &self,
        out: &mut Vec<QuadPatch>,
        f: i32,
        path: &'static str,
        fits: Vec<Fit>,
        cads: Vec<f64>,
    ) -> Result<()> {
        let face_type = self.gmsh.kind(2, f)?;
        for ((patch, samples, error), cad) in fits.into_iter().zip(cads) {
            let n = (samples.len() as f64).sqrt().round() as usize;
            out.push(QuadPatch {
                face: f,
                samples,
                n,
                patch,
                sample_error: error,
                cad_error: cad,
                path,
                face_type: face_type.clone(),
                exact: None,
                nodes: None,
                disjoint_group: -1,
                repair_error: None,
            });
        }
        Ok(())
    }

    // -- boundary loops ---------------------------------------------------------------------

    /// The boundary curves of a face with one loop, degenerate curves (poles) left out; None with holes.
    fn boundary(&self, f: i32) -> Result<Option<Vec<i32>>> {
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
    fn dense_chain(&self, curves: &[i32], m: usize) -> Result<Option<Chain>> {
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
    fn smooth_loop(&self, curves: &[i32]) -> Result<Option<Loop>> {
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
    fn sides(&mut self, curves: &[i32]) -> Result<Option<Sides>> {
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
    fn chain(&mut self, curves: &[i32], n: usize) -> Result<Option<Vec<Vec<DVec3>>>> {
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

    fn revolution(&self, f: i32) -> Result<bool> {
        Ok(matches!(
            self.gmsh.kind(2, f)?.as_str(),
            "Sphere" | "Torus" | "Surface of Revolution"
        ))
    }

    /// Whether the sides of the face's parameter box lie on its boundary curves.
    fn untrimmed(&self, f: i32, curves: &[i32]) -> Result<bool> {
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
    fn fit_edges(
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

    fn fit(&self, n: usize, spans: usize, edge: &[DVec3]) -> Result<Vec<DVec3>> {
        fit_curve(&unit_grid(n), edge, spans, self.settings.order)
    }

    /// The first fit over SPANS whose patches all meet the fit tolerance (the last one otherwise).
    fn adaptive(
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
    fn turning(&self, f: i32, lo: &[f64], hi: &[f64], axis: usize) -> Result<f64> {
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

    /// Patches in the face's own parameters, the box cut evenly into pieces whose normal turns by at most
    /// 90 degrees.
    fn direct(&mut self, f: i32) -> Result<Fits> {
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
    fn native(&self, f: i32) -> Result<Option<Fits>> {
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
    fn triangle(&mut self, f: i32, curves: &[i32]) -> Result<Option<Fits>> {
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

    fn single(&mut self, f: i32, curves: &[i32]) -> Result<Option<Fits>> {
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
    fn polygon(&mut self, f: i32, curves: &[i32]) -> Result<Option<Fits>> {
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
    fn bridge(&mut self, f: i32, curves: &[i32]) -> Result<Option<Fits>> {
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
    fn stretch(&self, lp: &Loop, s0: f64, s1: f64, n: usize) -> Result<Vec<DVec3>> {
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
    fn chord(&self, f: i32, a: DVec3, b: DVec3, n: usize) -> Result<Vec<DVec3>> {
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
    fn ogrid(&mut self, f: i32, lp: &Loop) -> Result<Fits> {
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

    fn fit_four(&self, f: i32, edges: &[Vec<DVec3>; 4], n: usize, spans: usize) -> Result<Fit> {
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
    fn ring(&mut self, f: i32, outer: &Loop, inner: &Loop) -> Result<Option<Fits>> {
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

    // -- quad meshes ---------------------------------------------------------------------------

    /// Mesh the model with patch sizes per face (other faces one element across) and keep the quads of
    /// `faces` (triangles split in three quads through their centre and edge midpoints).
    fn mesh(&mut self, faces: &[i32], sizes: &BTreeMap<i32, f64>) -> Result<()> {
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
    fn renumber_by_position(&mut self, cells: &mut [(i32, [usize; 3])]) {
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
    fn mid_point(&self, face: i32, nodes: &[usize], curve: Option<i32>) -> Result<DVec3> {
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

    fn tri3(&mut self, cells: &[(i32, [usize; 3])]) -> Result<()> {
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
    fn edge(
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
    fn quad_patches(&mut self) -> Result<Vec<QuadPatch>> {
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
    fn split_faces(&mut self, faces: &[i32]) -> Result<Vec<QuadPatch>> {
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

    fn split(&mut self, faces: &[i32]) -> Result<Vec<QuadPatch>> {
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

    /// Replace each fitted patch by the exact one when it lies on its face (within 1e-9 of the size), or
    /// within 1e-6 when the exact patch is no farther from the face and its edges than the fit.
    fn make_exact(&self, out: &mut [QuadPatch]) -> Result<()> {
        let scale = self.settings.scale;
        for q in out.iter_mut() {
            if q.path == "native" {
                continue; // the file's own surface is kept as it is
            }
            let grid = Grid {
                points: &q.samples,
                n: q.n,
            };
            let revolution = self.revolution(q.face)?;
            for t in [1e-9, 1e-6] {
                let mut p = exact_patch(&grid, t * scale);
                if p.is_none() && revolution {
                    p = revolved_patch(&grid, t * scale);
                }
                let Some(p) = p else { continue };
                let mut cad = cad_distance(self.gmsh, q.face, &p, 41)?;
                if t == 1e-6 {
                    let e = grid.edges();
                    cad = cad.max(edge_error(&p, [&e[0], &e[1], &e[2], &e[3]], 201));
                }
                if cad <= 10. * t * scale && (t == 1e-9 || cad <= q.cad_error) {
                    q.exact = Some(if p.rational() {
                        "rational"
                    } else {
                        "polynomial"
                    });
                    q.patch = p;
                    q.cad_error = cad;
                    break;
                }
            }
        }
        Ok(())
    }
}

/// Check a patch set for basic consistency (finite coordinates).
pub fn validate(patches: &[QuadPatch]) -> Result<()> {
    for q in patches {
        ensure!(
            q.patch.coefficients.iter().all(|c| c.is_finite()),
            "Face {}: non-finite patch coefficients",
            q.face
        );
    }
    Ok(())
}

/// The total turning of a polyline's direction, in degrees.
fn polyline_turning(points: &[DVec3]) -> f64 {
    points
        .windows(3)
        .map(|w| {
            let (a, b) = (w[1] - w[0], w[2] - w[1]);
            if a.length() < 1e-12 || b.length() < 1e-12 {
                0.
            } else {
                angle_degrees(a, b)
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::start_at_smallest;

    #[test]
    fn an_element_starts_at_its_smallest_node_and_keeps_its_cycle() {
        assert_eq!(start_at_smallest([7, 3, 9, 5]), [3, 9, 5, 7]);
        assert_eq!(start_at_smallest([2, 8, 4]), [2, 8, 4]);
        assert_eq!(start_at_smallest([6, 9, 1]), [1, 6, 9]);
    }
}

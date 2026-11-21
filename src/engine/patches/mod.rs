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

mod edges;
mod paths;
mod split;

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

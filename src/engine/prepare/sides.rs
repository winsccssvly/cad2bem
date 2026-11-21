// SPDX-License-Identifier: GPL-3.0-or-later
//! The side of every face from the CAD solids, probed by points offset along its normal.

use super::*;

/// The sides of the prepared faces decided by the CAD solids.
pub struct Sides {
    /// Per face: +1 or -1 when its CAD normal points into or out of the body, None when undecided.
    pub inward: BTreeMap<i32, Option<i32>>,
    /// The number of offset points classified.
    pub tested: usize,
}

/// The inward side of every prepared face from the original CAD solids.
///
/// Five well-separated points inside each trimmed face are offset along the CAD normal both ways and
/// classified in a separate copy of the solids (denser sample grids for thin faces). A probe is clear when
/// exactly one of its two points is inside. A face is decided when at least three probes are clear and all
/// clear probes agree; otherwise (too few samples, a face where solids touch, disagreeing probes) it is left
/// undecided, and the whole model is then oriented by the generalized winding
/// number of its mesh (see `winding`).
pub fn face_sides(gmsh: &Session, path: &Path, prepared: &Prepared) -> Result<Sides> {
    if !prepared.has_solids {
        return Ok(Sides {
            inward: prepared.faces.iter().map(|&f| (f, None)).collect(),
            tested: 0,
        });
    }
    let mut probes = BTreeMap::new();
    for &face in &prepared.faces {
        let (lo, hi) = gmsh.parametrization_bounds(2, face)?;
        // A thin piece (a cut band, a short pipe end) can miss a coarse grid: denser grids are tried next.
        let mut uv = Vec::new();
        for (n, margin) in [(9, 0.1), (17, 0.03), (33, 0.01)] {
            uv.clear();
            let fractions = linspace(margin, 1. - margin, n);
            for &u in &fractions {
                for &v in &fractions {
                    let p = [lo[0] + u * (hi[0] - lo[0]), lo[1] + v * (hi[1] - lo[1])];
                    if gmsh.inside(2, face, &p, true)? != 0 {
                        uv.push(p);
                    }
                }
            }
            if uv.len() >= 5 {
                break;
            }
        }
        if uv.len() < 5 {
            probes.insert(face, Vec::new());
            continue;
        }
        let xyz = gmsh.value(2, face, uv.as_flattened())?;
        let normals = gmsh.normal(face, uv.as_flattened())?;
        let points: Vec<DVec3> = xyz.iter().map(|&p| DVec3::from_array(p)).collect();
        let mean = points.iter().sum::<DVec3>() / points.len() as f64;
        let first = argmin(points.iter().map(|p| p.distance(mean)));
        let mut chosen = vec![first];
        let mut distance: Vec<f64> = points.iter().map(|p| p.distance(points[first])).collect();
        for _ in 0..4 {
            for &c in &chosen {
                distance[c] = -1.;
            }
            let k = argmax(distance.iter().copied());
            chosen.push(k);
            for (d, p) in distance.iter_mut().zip(&points) {
                *d = d.min(p.distance(points[k]));
            }
        }
        let lo = points.iter().fold(DVec3::INFINITY, |a, &p| a.min(p));
        let hi = points.iter().fold(DVec3::NEG_INFINITY, |a, &p| a.max(p));
        let size = (hi - lo).length();
        probes.insert(
            face,
            chosen
                .into_iter()
                .map(|k| (points[k], DVec3::from_array(normals[k]), size))
                .collect::<Vec<_>>(),
        );
    }
    let current = gmsh.current_model()?;
    gmsh.add_model(&format!("{current}_solid_orientation"))?;
    let decided = (|| -> Result<Sides> {
        import(gmsh, path, prepared.scaling)?;
        let solids = gmsh.tags(3)?;
        let mut sides = Sides {
            inward: BTreeMap::new(),
            tested: 0,
        };
        for (&face, candidates) in &probes {
            let mut votes = Vec::new();
            for &(x, normal, size) in candidates {
                let h = (1e-4 * prepared.scale).max((0.01 * size).min(1e-3 * prepared.scale));
                let mut inside = [false; 2];
                for (side, s) in inside.iter_mut().zip([1., -1.]) {
                    let point = (x + s * h * normal).to_array();
                    for &tag in &solids {
                        if gmsh.inside(3, tag, &point, false)? != 0 {
                            *side = true;
                            break;
                        }
                    }
                }
                sides.tested += 2;
                if inside[0] != inside[1] {
                    votes.push(if inside[0] { 1 } else { -1 });
                }
            }
            // A probe whose two offset points are both inside (or both outside) a solid says nothing; at least
            // three clear probes must agree and none may disagree.
            let unanimous = votes.len() >= 3 && votes.iter().all(|&v| v == votes[0]);
            sides.inward.insert(face, unanimous.then(|| votes[0]));
        }
        Ok(sides)
    })();
    gmsh.remove_model()?;
    gmsh.set_current_model(&current)?;
    // Gmsh's global model size (its tolerances scale with it) is the last synchronized model's: the whole
    // file imported above. Synchronizing the body again gives it back the body's own.
    gmsh.synchronize()?;
    decided
}

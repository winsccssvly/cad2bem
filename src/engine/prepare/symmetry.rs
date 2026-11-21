// SPDX-License-Identifier: GPL-3.0-or-later
//! The symmetry check and cut: the mirror distance of the faces on the two sides, and the cut at the plane.

use super::*;

/// Mirror distances met before in this process, by the faces' geometry on both sides and the axis.
pub(super) static MIRRORS: LazyLock<Mutex<HashMap<MirrorKey, f64>>> =
    LazyLock::new(Default::default);
pub(super) type MirrorKey = (Vec<Vec<u64>>, Vec<Vec<u64>>, usize);

/// The largest distance of the samples of faces `a`, mirrored in the plane normal to axis k, to the nearest
/// of faces `b` (infinite when only one side has faces); remembered by the faces' geometry.
pub(super) fn mirror_distance(gmsh: &Session, a: &[i32], b: &[i32], k: usize) -> Result<f64> {
    let keys = |faces: &[i32]| -> Result<Vec<Vec<u64>>> {
        let mut keys = faces
            .iter()
            .map(|&f| crate::cad::gmsh::face_key(gmsh, f))
            .collect::<Result<Vec<_>>>()?;
        keys.sort();
        Ok(keys)
    };
    let key = (keys(a)?, keys(b)?, k);
    if let Some(&d) = MIRRORS.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(d);
    }
    let d = measure_mirror_distance(gmsh, a, b, k)?;
    let mut cache = MIRRORS.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 1000 {
        cache.clear();
    }
    cache.insert(key, d);
    Ok(d)
}

pub(super) fn measure_mirror_distance(
    gmsh: &Session,
    a: &[i32],
    b: &[i32],
    k: usize,
) -> Result<f64> {
    let mut points = Vec::new();
    for &f in a {
        points.extend(samples(gmsh, f, 7)?.x);
    }
    if points.is_empty() || b.is_empty() {
        return Ok(if points.is_empty() && b.is_empty() {
            0.
        } else {
            f64::INFINITY
        });
    }
    for p in &mut points {
        p[k] = -p[k];
    }
    let mut d = vec![f64::INFINITY; points.len()];
    for &f in b {
        let (closest, _) = gmsh.closest_point(2, f, &points)?;
        for (i, c) in closest.iter().enumerate() {
            d[i] = d[i].min(distance(*c, points[i]));
        }
    }
    Ok(d.into_iter().fold(0., f64::max))
}

/// Cut a whole body at the symmetry planes it reaches across and keep x >= 0, y >= 0, after checking per
/// plane that it is symmetric. Returns the report's `symmetry_cut` record.
pub(super) fn cut_symmetry(
    gmsh: &Session,
    path: &Path,
    symmetry: Symmetry,
    halves: &[usize],
    scale: f64,
) -> Result<Value> {
    let tol = 1e-6 * scale;
    let (mut planes, mut faces_cut, mut faces_dropped) = (Vec::new(), 0, 0);
    let (mut mirror_error, mut area_difference) = (0_f64, 0_f64);
    for &k in halves {
        let [x0, y0, z0, x1, y1, z1] = gmsh.bounding_box(-1, -1)?;
        for f in gmsh.tags(2)? {
            let x = samples(gmsh, f, 7)?.x;
            let across = !x.is_empty()
                && x.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min) < -tol
                && x.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) > tol;
            if !across {
                continue;
            }
            let cutter = if k == 0 {
                let c = gmsh.add_rectangle([z0 - 1., y0 - 1., 0.], z1 - z0 + 2., y1 - y0 + 2.)?;
                gmsh.rotate(
                    &[(2, c)],
                    [0.; 3],
                    [0., 1., 0.],
                    -std::f64::consts::FRAC_PI_2,
                )?;
                c
            } else {
                let c = gmsh.add_rectangle([x0 - 1., z0 - 1., 0.], x1 - x0 + 2., z1 - z0 + 2.)?;
                gmsh.rotate(
                    &[(2, c)],
                    [0.; 3],
                    [1., 0., 0.],
                    std::f64::consts::FRAC_PI_2,
                )?;
                c
            };
            let pieces = gmsh.fragment(&[(2, f)], &[(2, cutter)])?.map;
            gmsh.remove(&pieces[1], true)?;
            gmsh.synchronize()?;
            faces_cut += usize::from(pieces[0].len() > 1);
        }
        let (mut negative, mut positive) = (Vec::new(), Vec::new());
        for f in gmsh.tags(2)? {
            let b = gmsh.bounding_box(2, f)?;
            if b[k].abs() < tol && b[k + 3].abs() < tol {
                continue;
            }
            if gmsh.center_of_mass(2, f)?[k] > 0. {
                positive.push(f);
            } else {
                negative.push(f);
            }
        }
        let mut area = [0.; 2];
        for (sum, faces) in area.iter_mut().zip([&negative, &positive]) {
            for &f in faces {
                *sum += gmsh.mass(2, f)?;
            }
        }
        let error = mirror_distance(gmsh, &negative, &positive, k)?
            .max(mirror_distance(gmsh, &positive, &negative, k)?);
        let difference = (area[0] - area[1]).abs() / area[1].max(1e-300);
        if error > MIRROR_TOL * scale || difference > AREA_TOL {
            let rest: Vec<_> = symmetry
                .axes()
                .iter()
                .filter(|&&a| a != k)
                .map(|&a| plane_name(a))
                .collect();
            let rest = if rest.is_empty() {
                "none".into()
            } else {
                rest.join("+")
            };
            bail!(
                "{}: the body is not symmetric about {} = 0 (mirror distance {} m, area difference {}); \
                 convert with symmetry '{rest}'",
                display(path),
                ["x", "y"][k],
                general(error, 3),
                general(difference, 3)
            );
        }
        let removed: Vec<DimTag> = negative.iter().map(|&f| (2, f)).collect();
        gmsh.remove(&removed, true)?;
        gmsh.synchronize()?;
        planes.push(plane_name(k));
        faces_dropped += negative.len();
        mirror_error = mirror_error.max(error);
        area_difference = area_difference.max(difference);
    }
    Ok(
        json!({"planes": planes, "faces_cut": faces_cut, "faces_dropped": faces_dropped,
        "mirror_error_m": mirror_error, "area_difference_rel": area_difference}),
    )
}

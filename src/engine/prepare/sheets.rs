// SPDX-License-Identifier: GPL-3.0-or-later
//! Sheet removal: faces that are not part of the closed body, by the rules or by keeping the largest group.

use super::*;

/// Remove face f as a CAD sheet and record why.
pub(super) fn remove_sheet(
    gmsh: &Session,
    f: i32,
    parent: &HashMap<i32, i32>,
    reason: String,
    sheets: &mut Vec<Value>,
) -> Result<()> {
    let area = (gmsh.mass(2, f)? * 1000.).round() / 1000.;
    sheets.push(json!({"face": f, "parent": parent.get(&f).copied().unwrap_or(f), "area": area, "reason": reason}));
    gmsh.remove(&[(2, f)], true)?;
    gmsh.synchronize()
}

/// Remove the pieces that are not part of the closed wetted surface, by two rules on the fragmented faces:
/// (1) a piece with a free boundary curve (a curve of no other piece, not on the waterline or a symmetry
/// plane, not a seam or a pole) is a sheet, repeatedly; (2) where three or more pieces meet along a curve, a
/// face the fragment split there keeps its largest piece (the others ran past the intersection).
pub(super) fn remove_sheets(
    gmsh: &Session,
    parent: &HashMap<i32, i32>,
    symmetry: Symmetry,
    scale: f64,
    sheets: &mut Vec<Value>,
) -> Result<()> {
    let tol = 1e-6 * scale;
    let mut planes = vec![2];
    planes.extend(symmetry.axes());
    let on_plane = |c: i32| -> Result<bool> {
        let b = gmsh.bounding_box(1, c)?;
        Ok(planes
            .iter()
            .any(|&k| b[k].abs() < tol && b[k + 3].abs() < tol))
    };
    let point = |c: i32| -> Result<bool> {
        let b = gmsh.bounding_box(1, c)?;
        Ok((0..3).all(|k| b[k + 3] - b[k] < tol))
    };
    let same_place = |a: i32, b: i32| -> Result<bool> {
        let (lo, hi) = gmsh.parametrization_bounds(1, a)?;
        let x = gmsh.value(1, a, &linspace(lo[0], hi[0], 9))?;
        let (cx, _) = gmsh.closest_point(1, b, &x)?;
        let worst = x
            .iter()
            .zip(cx)
            .flat_map(|(p, q)| (0..3).map(move |d| (p[d] - q[d]).abs()))
            .fold(0., f64::max);
        Ok(worst < 1e-5 * scale)
    };
    // The curves that bound a face on both sides (a seam): one curve twice in its loops, or two curves of it
    // in the same place (a face that was not sewn).
    let seams = |f: i32| -> Result<Vec<i32>> {
        let mut count: Vec<(i32, usize)> = Vec::new();
        for c in gmsh.curve_loops(f)?.into_iter().flatten().map(|c| c.abs()) {
            match count.iter_mut().find(|(k, _)| *k == c) {
                Some(entry) => entry.1 += 1,
                None => count.push((c, 1)),
            }
        }
        let mut out: Vec<i32> = count
            .iter()
            .filter(|(_, n)| *n > 1)
            .map(|(c, _)| *c)
            .collect();
        let mut lines = Vec::new();
        for &(c, _) in &count {
            if !out.contains(&c) && !point(c)? {
                lines.push(c);
            }
        }
        for (k, &a) in lines.iter().enumerate() {
            for &b in &lines[k + 1..] {
                if same_place(a, b)? && same_place(b, a)? {
                    out.extend([a, b]);
                }
            }
        }
        Ok(out)
    };
    loop {
        let mut changed = false;
        for f in gmsh.tags(2)? {
            let curves: Vec<i32> = gmsh
                .boundary(&[(2, f)], true, false, false)?
                .into_iter()
                .map(|(_, c)| c.abs())
                .collect();
            let seam = seams(f)?;
            let mut free = false;
            for c in curves {
                if gmsh.adjacencies(1, c)?.0.len() == 1
                    && !on_plane(c)?
                    && !seam.contains(&c)
                    && !point(c)?
                {
                    free = true;
                    break;
                }
            }
            if free {
                remove_sheet(gmsh, f, parent, "free edge".into(), sheets)?;
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    for c in gmsh.tags(1)? {
        if !gmsh.tags(1)?.contains(&c) {
            continue; // removed with a face already
        }
        let faces = gmsh.adjacencies(1, c)?.0;
        if faces.len() < 3 {
            continue;
        }
        let mut groups: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        for f in faces {
            groups
                .entry(parent.get(&f).copied().unwrap_or(f))
                .or_default()
                .push(f);
        }
        for (origin, kids) in groups {
            if kids.len() < 2 {
                continue;
            }
            let mut keep = kids[0];
            let mut largest = f64::NEG_INFINITY;
            for &k in &kids {
                let area = gmsh.mass(2, k)?;
                if area > largest {
                    (keep, largest) = (k, area);
                }
            }
            for f in kids {
                if f != keep {
                    remove_sheet(
                        gmsh,
                        f,
                        parent,
                        format!("overshoot of face {origin} at curve {c}"),
                        sheets,
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Remove every connected group of faces (faces sharing a curve) but the one of the largest area.
pub(super) fn keep_largest(
    gmsh: &Session,
    parent: &HashMap<i32, i32>,
    sheets: &mut Vec<Value>,
) -> Result<()> {
    let faces = gmsh.tags(2)?;
    let mut group: HashMap<i32, i32> = faces.iter().map(|&f| (f, f)).collect();
    fn find(group: &mut HashMap<i32, i32>, mut f: i32) -> i32 {
        while group[&f] != f {
            let up = group[&group[&f]];
            group.insert(f, up);
            f = up;
        }
        f
    }
    for c in gmsh.tags(1)? {
        let fs: Vec<i32> = gmsh
            .adjacencies(1, c)?
            .0
            .into_iter()
            .filter(|f| group.contains_key(f))
            .collect();
        for &f in fs.iter().skip(1) {
            let (a, b) = (find(&mut group, f), find(&mut group, fs[0]));
            group.insert(a, b);
        }
    }
    let mut area: BTreeMap<i32, f64> = BTreeMap::new();
    for &f in &faces {
        let root = find(&mut group, f);
        *area.entry(root).or_default() += gmsh.mass(2, f)?;
    }
    if area.len() < 2 {
        return Ok(());
    }
    let best = area
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(k, _)| *k)
        .unwrap_or(0);
    for f in faces {
        if find(&mut group, f) != best {
            remove_sheet(gmsh, f, parent, "not the largest body".into(), sheets)?;
        }
    }
    Ok(())
}

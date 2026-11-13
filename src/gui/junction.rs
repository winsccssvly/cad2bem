// SPDX-License-Identifier: GPL-3.0-or-later
//! Cuts for the ends of a pipe (a brace or a pontoon) where it meets another body.
//!
//! The end of a cylindrical face that is trimmed by another surface is neither a circle across its axis nor a
//! line along it, so it has five or more sides and is split into many small patches. Two kinds of cut give it
//! four-sided pieces: one across the axis a little beyond the farthest point where the pipe touches the other
//! body (on a sloping pipe the upper and lower lines touch at different places), and cuts along the axis of the
//! short end piece left between them, through the corners of the junction curve and at most 120 degrees apart.
//! Every cut keeps to that face (`face_at`) and its cylinder (`on`).

use cad2bem::{
    cad::scene::Scene,
    conversion::{Cut, Surface},
};
use glam::DVec3;
use std::collections::BTreeMap;

/// The cut across the axis lies this many radii beyond the farthest point of the junction.
const MARGIN: f64 = 0.4;
/// An end piece is cut lengthwise at least every this many degrees.
const MAX_GAP: f64 = 120.;
/// Lengthwise cuts closer than this (degrees) to another, or to the side of a part cylinder, are left out.
const MIN_GAP: f64 = 12.;

/// A cylinder's frame: a point on the axis, the unit axis, two radial directions and the radius.
struct Frame {
    base: DVec3,
    a: DVec3,
    e1: DVec3,
    e2: DVec3,
    r: f64,
}

impl Frame {
    fn t(&self, p: DVec3) -> f64 {
        (p - self.base).dot(self.a)
    }

    /// The angle about the axis (degrees, 0..360).
    fn azimuth(&self, p: DVec3) -> f64 {
        let d = p - self.base;
        d.dot(self.e2)
            .atan2(d.dot(self.e1))
            .to_degrees()
            .rem_euclid(360.)
    }

    fn radial(&self, degrees: f64) -> DVec3 {
        let x = degrees.to_radians();
        x.cos() * self.e1 + x.sin() * self.e2
    }
}

/// The cuts for the junction ends of a cylindrical face of `scene`, or why there are none.
pub(super) fn cuts(scene: &Scene, tag: i32) -> Result<Vec<Cut>, &'static str> {
    let face = scene
        .faces
        .iter()
        .find(|f| f.tag == tag)
        .ok_or("No such face")?;
    let Some(
        surface @ Surface::Cylinder {
            point,
            axis,
            radius,
        },
    ) = &face.surface
    else {
        return Err("Junction cuts are only made for pipe ends, and this face is not a cylinder");
    };
    let a = DVec3::from(*axis).normalize();
    let e1 = a.any_orthonormal_vector();
    let frame = Frame {
        base: DVec3::from(*point),
        a,
        e1,
        e2: a.cross(e1),
        r: *radius,
    };
    // The face's boundary in its display triangles: the edges of one triangle only.
    let mut count: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    let mut points = Vec::new();
    for (tri, _) in scene
        .triangles
        .iter()
        .zip(&scene.face_tags)
        .filter(|(_, f)| **f == tag)
    {
        for k in 0..3 {
            let (i, j) = (tri[k], tri[(k + 1) % 3]);
            *count.entry((i.min(j), i.max(j))).or_default() += 1;
            points.push(DVec3::from(scene.positions_m[i as usize]));
        }
    }
    let at = |i: u32| DVec3::from(scene.positions_m[i as usize]);
    let (lo, hi) = range(points.iter().map(|&p| frame.t(p)));
    let flat = 1e-3 * frame.r;
    // Junction edges: neither along the axis nor across it.
    let mut junction = Vec::new();
    for (&(i, j), _) in count.iter().filter(|(_, n)| **n == 1) {
        let (p, q) = (at(i), at(j));
        let d = q - p;
        let along = d.dot(frame.a).abs() > 0.99 * d.length();
        let across = (frame.t(p) - frame.t(q)).abs() < flat;
        if !along && !across {
            junction.extend([p, q]);
        }
    }
    if junction.is_empty() {
        return Err("Both ends of this pipe are plain circles, so there is no junction to cut");
    }
    // The junctions at each end of the face (a hole in the middle of a column wall is not a pipe end).
    let middle = (lo + hi) / 2.;
    let (low, high): (Vec<DVec3>, Vec<DVec3>) =
        junction.iter().partition(|&&p| frame.t(p) < middle);
    let low_range = (!low.is_empty()).then(|| range(low.iter().map(|&p| frame.t(p))));
    let high_range = (!high.is_empty()).then(|| range(high.iter().map(|&p| frame.t(p))));
    let mut out = Vec::new();
    if let Some((near, far)) = low_range.filter(|r| (r.0 - lo).abs() < flat) {
        let free = high_range.map_or(hi, |h| h.0);
        end(
            &frame,
            surface,
            face,
            &low,
            &points,
            (1., near, far, free),
            &mut out,
        );
    }
    if let Some((far, near)) = high_range.filter(|r| (r.1 - hi).abs() < flat) {
        let free = low_range.map_or(lo, |l| l.1);
        end(
            &frame,
            surface,
            face,
            &high,
            &points,
            (-1., near, far, free),
            &mut out,
        );
    }
    if out.is_empty() {
        return Err("This pipe does not end at the junction, so there is nothing to cut");
    }
    Ok(out)
}

/// The smallest and largest value.
fn range(values: impl Iterator<Item = f64>) -> (f64, f64) {
    values.fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), x| {
        (l.min(x), h.max(x))
    })
}

/// The cuts of one junction end: `s` +1 when the face goes on toward larger axial positions, `near` and
/// `far` the axial extent of the junction, `free` where the face's other end (or junction) begins.
fn end(
    frame: &Frame,
    surface: &Surface,
    face: &cad2bem::cad::scene::Face,
    junction: &[DVec3],
    points: &[DVec3],
    (s, near, far, free): (f64, f64, f64, f64),
    out: &mut Vec<Cut>,
) {
    let r = frame.r;
    let flat = 1e-3 * r;
    // Across, beyond the farthest contact; when that leaves too short a rest, the end piece reaches the free end.
    let across = far + s * MARGIN * r;
    let stop = if s * (free - across) > 0.2 * r {
        let side = frame.azimuth(junction[0]);
        out.push(Cut {
            centre: (frame.base + across * frame.a).to_array(),
            normal: frame.a.to_array(),
            axis: frame.e1.to_array(),
            half: [1.5 * r, 1.5 * r],
            on: Some(surface.clone()),
            face_at: Some((frame.base + across * frame.a + r * frame.radial(side)).to_array()),
        });
        across
    } else {
        free
    };
    // Along, through the corners of the junction curve (the face's CAD vertices on it) and its extreme points.
    let mut angles: Vec<f64> = face
        .corners
        .iter()
        .map(|&c| DVec3::from(c))
        .filter(|&c| s * (frame.t(c) - far) <= flat && s * (frame.t(c) - near) >= -flat)
        .map(|c| frame.azimuth(c))
        .collect();
    for &p in junction {
        if (frame.t(p) - near).abs() < flat || (frame.t(p) - far).abs() < flat {
            angles.push(frame.azimuth(p));
        }
    }
    // The angles the face covers: a part cylinder (cut at a symmetry plane) has sides not to cut along.
    let mut covered: Vec<f64> = points.iter().map(|&p| frame.azimuth(p)).collect();
    covered.sort_by(f64::total_cmp);
    let (gap, after) = covered
        .windows(2)
        .map(|w| (w[1] - w[0], w[0]))
        .chain(
            covered
                .last()
                .zip(covered.first())
                .map(|(l, f)| (f + 360. - l, *l)),
        )
        .fold((0., 0.), |m, x| if x.0 > m.0 { x } else { m });
    let part = gap > 20.;
    let (start, span) = if part {
        ((after + gap).rem_euclid(360.), 360. - gap)
    } else {
        (0., 360.)
    };
    let mut chosen: Vec<f64> = angles
        .into_iter()
        .map(|x| (x - start).rem_euclid(360.))
        .filter(|&x| !part || (x > MIN_GAP && x < span - MIN_GAP))
        .collect();
    chosen.sort_by(f64::total_cmp);
    chosen.dedup_by(|x, y| (*x - *y).abs() < MIN_GAP);
    // Fill gaps wider than MAX_GAP (and a full circle without any angle) evenly.
    let mut ends = chosen.clone();
    if part {
        ends.insert(0, 0.);
        ends.push(span);
    } else if let Some(&first) = chosen.first() {
        ends.push(first + 360.);
    } else {
        ends = vec![0., 360.];
        chosen.push(0.);
    }
    for w in ends.windows(2) {
        let n = ((w[1] - w[0]) / MAX_GAP).ceil() as usize;
        for k in 1..n {
            chosen.push(w[0] + (w[1] - w[0]) * k as f64 / n as f64);
        }
    }
    // Lengthwise over the end piece only: from before the junction to the cut across (or the free end).
    let (from, to) = (near - s * 0.1 * r, stop);
    let inside = (far + stop) / 2.;
    for x in chosen {
        let radial = frame.radial(x + start);
        out.push(Cut {
            centre: (frame.base + (from + to) / 2. * frame.a + 0.75 * r * radial).to_array(),
            normal: frame.a.cross(radial).to_array(),
            axis: frame.a.to_array(),
            half: [(to - from).abs() / 2., 0.75 * r],
            on: Some(surface.clone()),
            face_at: Some((frame.base + inside * frame.a + r * radial).to_array()),
        });
    }
}

/// The cylindrical faces of `scene` that have a junction end, with their cuts.
pub(super) fn all(scene: &Scene) -> Vec<(i32, Vec<Cut>)> {
    scene
        .faces
        .iter()
        .filter_map(|f| cuts(scene, f.tag).ok().map(|c| (f.tag, c)))
        .collect()
}

/// The cuts around the holes that pipes make in cylinder walls (a column where braces and pontoons meet it),
/// so that every piece has the hole's curve on its outer boundary instead of around a hole: circles across the
/// wall a little above and below each hole and through its corners, lines along the axis from its top up and
/// from its bottom down to those circles, and the window sides between neighbouring holes (or opposite a single
/// hole). The cuts keep to the wall's cylinder (`on`), across all its faces.
pub(super) fn windows(scene: &Scene) -> Vec<Cut> {
    let size = {
        let b = scene.bounds_m;
        (b[3] - b[0]).max(b[4] - b[1]).max(b[5] - b[2])
    };
    // The faces of each cylinder (a wall is often split into several faces).
    let mut walls: Vec<(Surface, Vec<&cad2bem::cad::scene::Face>)> = Vec::new();
    for face in &scene.faces {
        let Some(surface @ Surface::Cylinder { .. }) = &face.surface else {
            continue;
        };
        if cuts(scene, face.tag).is_ok() {
            continue; // a pipe with a junction end: the pipe rule cuts it
        }
        match walls
            .iter_mut()
            .find(|(s, _)| same_cylinder(s, surface, 1e-4 * size))
        {
            Some((_, faces)) => faces.push(face),
            None => walls.push((surface.clone(), vec![face])),
        }
    }
    let mut out = Vec::new();
    for (surface, faces) in &walls {
        let Surface::Cylinder {
            point,
            axis,
            radius,
        } = surface
        else {
            continue;
        };
        let a = DVec3::from(*axis).normalize();
        let e1 = a.any_orthonormal_vector();
        let frame = Frame {
            base: DVec3::from(*point),
            a,
            e1,
            e2: a.cross(e1),
            r: *radius,
        };
        out.extend(wall_cuts(scene, surface, &frame, faces, size));
    }
    out
}

/// Whether two cylinders are the same within `tol`.
fn same_cylinder(a: &Surface, b: &Surface, tol: f64) -> bool {
    let (
        Surface::Cylinder {
            point: p,
            axis: u,
            radius: r,
        },
        Surface::Cylinder {
            point: q,
            axis: v,
            radius: s,
        },
    ) = (a, b)
    else {
        return false;
    };
    let (u, v) = (DVec3::from(*u).normalize(), DVec3::from(*v).normalize());
    let d = DVec3::from(*q) - DVec3::from(*p);
    u.cross(v).length() < 1e-6 && (d - d.dot(u) * u).length() < tol && (r - s).abs() < tol
}

/// The window cuts of one cylinder wall made of `faces`.
fn wall_cuts(
    scene: &Scene,
    surface: &Surface,
    frame: &Frame,
    faces: &[&cad2bem::cad::scene::Face],
    size: f64,
) -> Vec<Cut> {
    let r = frame.r;
    let flat = 1e-3 * r;
    // The wall's boundary: the triangle edges that are on the boundary of one of its faces and not shared
    // with another of its faces (by position: the faces of a wall meet at seams and cuts).
    let key = |p: DVec3| (p / (1e-6 * size)).round().as_ivec3().to_array();
    type Edge = ([i32; 3], [i32; 3]);
    let mut boundary: BTreeMap<Edge, (DVec3, DVec3, usize)> = BTreeMap::new();
    let mut points = Vec::new();
    for face in faces {
        let mut count: BTreeMap<(u32, u32), usize> = BTreeMap::new();
        for (tri, _) in scene
            .triangles
            .iter()
            .zip(&scene.face_tags)
            .filter(|(_, f)| **f == face.tag)
        {
            for k in 0..3 {
                let (i, j) = (tri[k], tri[(k + 1) % 3]);
                *count.entry((i.min(j), i.max(j))).or_default() += 1;
                points.push(DVec3::from(scene.positions_m[i as usize]));
            }
        }
        for (&(i, j), _) in count.iter().filter(|(_, n)| **n == 1) {
            let (p, q) = (
                DVec3::from(scene.positions_m[i as usize]),
                DVec3::from(scene.positions_m[j as usize]),
            );
            let (kp, kq) = (key(p), key(q));
            let edge = if kp <= kq { (kp, kq) } else { (kq, kp) };
            boundary.entry(edge).or_insert((p, q, 0)).2 += 1;
        }
    }
    let (lo, hi) = range(points.iter().map(|&p| frame.t(p)));
    // The sides of a part cylinder (cut at a symmetry plane): the edges of the widest angle it does not cover.
    let covered: Vec<f64> = points.iter().map(|&p| frame.azimuth(p)).collect();
    let (gap, after) = widest_gap(&covered);
    let sides = if gap > 20. {
        vec![after, (after + gap).rem_euclid(360.)]
    } else {
        Vec::new()
    };
    let near = |x: f64, y: f64| {
        let d = (x - y).rem_euclid(360.);
        d.min(360. - d) < 0.5
    };
    // Holes: the boundary but the wall's end circles and sides, joined by shared points.
    let mut parent: BTreeMap<[i32; 3], [i32; 3]> = BTreeMap::new();
    fn find(parent: &mut BTreeMap<[i32; 3], [i32; 3]>, k: [i32; 3]) -> [i32; 3] {
        let up = *parent.entry(k).or_insert(k);
        if up == k {
            return k;
        }
        let root = find(parent, up);
        parent.insert(k, root);
        root
    }
    let mut position: BTreeMap<[i32; 3], DVec3> = BTreeMap::new();
    for (&(kp, kq), &(p, q, n)) in &boundary {
        if n != 1 {
            continue;
        }
        let end = [lo, hi]
            .iter()
            .any(|&e| (frame.t(p) - e).abs() < flat && (frame.t(q) - e).abs() < flat);
        let d = q - p;
        let side = d.dot(frame.a).abs() > 0.99 * d.length()
            && sides
                .iter()
                .any(|&s| near(frame.azimuth(p), s) && near(frame.azimuth(q), s));
        if end || side {
            continue;
        }
        position.insert(kp, p);
        position.insert(kq, q);
        let (rp, rq) = (find(&mut parent, kp), find(&mut parent, kq));
        if rp != rq {
            parent.insert(rp, rq);
        }
    }
    let keys: Vec<[i32; 3]> = position.keys().copied().collect();
    let mut holes: BTreeMap<[i32; 3], Vec<DVec3>> = BTreeMap::new();
    for k in keys {
        let root = find(&mut parent, k);
        holes.entry(root).or_default().push(position[&k]);
    }
    // Holes inside the wall only: a junction at an end of the wall is a pipe end.
    let holes: Vec<Vec<DVec3>> = holes
        .into_values()
        .filter(|h| {
            let (a, b) = range(h.iter().map(|&p| frame.t(p)));
            a - lo > flat && hi - b > flat && h.len() >= 4
        })
        .collect();
    if holes.is_empty() {
        return Vec::new();
    }
    let corners: Vec<DVec3> = faces
        .iter()
        .flat_map(|f| f.corners.iter().map(|&c| DVec3::from(c)))
        .collect();
    let margin = 0.2 * r;
    let mut circles: Vec<f64> = Vec::new();
    // Azimuth, from, to (axial positions), and which ends lie on a circle.
    let mut lines: Vec<(f64, f64, f64, [bool; 2])> = Vec::new();
    let mut bands = Vec::new();
    for hole in &holes {
        let (hlo, hhi) = range(hole.iter().map(|&p| frame.t(p)));
        let (top, bottom) = ((hhi + margin).min(hi - flat), (hlo - margin).max(lo + flat));
        circles.extend([top, bottom]);
        // Through the hole's corners (CAD vertices on it) inside its height.
        for c in &corners {
            let on_hole = hole.iter().any(|p| p.distance(*c) < 1e-4 * size);
            let t = frame.t(*c);
            if on_hole && t > hlo + 0.1 * r && t < hhi - 0.1 * r {
                circles.push(t);
            }
        }
        let highest = hole
            .iter()
            .copied()
            .max_by(|p, q| frame.t(*p).total_cmp(&frame.t(*q)))
            .unwrap_or_default();
        let lowest = hole
            .iter()
            .copied()
            .min_by(|p, q| frame.t(*p).total_cmp(&frame.t(*q)))
            .unwrap_or_default();
        lines.push((frame.azimuth(highest), hhi - 0.05 * r, top, [false, true]));
        lines.push((frame.azimuth(lowest), bottom, hlo + 0.05 * r, [true, false]));
        let azimuths: Vec<f64> = hole.iter().map(|&p| frame.azimuth(p)).collect();
        bands.push((centre_angle(&azimuths), bottom, top));
    }
    // The window sides: between neighbouring holes (by angle), over the band both cover.
    bands.sort_by(|a, b| a.0.total_cmp(&b.0));
    let covered: Vec<f64> = points.iter().map(|&p| frame.azimuth(p)).collect();
    let full = largest_gap(&covered) < 20.;
    if bands.len() == 1 && full {
        let (c, b, t) = bands[0];
        lines.push(((c + 180.).rem_euclid(360.), b, t, [true, true]));
    } else {
        for k in 0..bands.len() {
            let (c0, b0, t0) = bands[k];
            let Some(&(c1, b1, t1)) = bands.get(k + 1).or(if full { bands.first() } else { None })
            else {
                continue;
            };
            if bands.len() == 1 {
                continue;
            }
            let gap = (c1 - c0).rem_euclid(360.);
            lines.push((
                (c0 + gap / 2.).rem_euclid(360.),
                b0.min(b1),
                t0.max(t1),
                [true, true],
            ));
        }
    }
    circles.sort_by(f64::total_cmp);
    circles.dedup_by(|x, y| (*x - *y).abs() < 0.1 * r);
    let circles_kept = circles.clone();
    // The circles first: the lines end on them.
    let mut out = Vec::new();
    for t in circles {
        out.push(Cut {
            centre: (frame.base + t * frame.a).to_array(),
            normal: frame.a.to_array(),
            axis: frame.e1.to_array(),
            half: [1.5 * r, 1.5 * r],
            on: Some(surface.clone()),
            face_at: None,
        });
    }
    // A line ends exactly on a circle that is kept: one that ended on a circle merged into another (two holes
    // at nearly the same height) would run past it and slit the face beyond.
    let kept = circles_kept.clone();
    let snap = |x: f64| {
        kept.iter()
            .copied()
            .min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs()))
            .unwrap_or(x)
    };
    for (angle, from, to, ends) in lines {
        let from = if ends[0] { snap(from) } else { from };
        let to = if ends[1] { snap(to) } else { to };
        if (to - from).abs() < 0.05 * r {
            continue;
        }
        let radial = frame.radial(angle);
        out.push(Cut {
            centre: (frame.base + (from + to) / 2. * frame.a + 0.75 * r * radial).to_array(),
            normal: frame.a.cross(radial).to_array(),
            axis: frame.a.to_array(),
            half: [(to - from).abs() / 2., 0.75 * r],
            on: Some(surface.clone()),
            face_at: None,
        });
    }
    out
}

/// The mean direction of angles (degrees).
fn centre_angle(angles: &[f64]) -> f64 {
    let (s, c) = angles.iter().fold((0., 0.), |(s, c), x: &f64| {
        (s + x.to_radians().sin(), c + x.to_radians().cos())
    });
    s.atan2(c).to_degrees().rem_euclid(360.)
}

/// The widest gap (degrees) between angles around the circle, and the angle it starts after.
fn widest_gap(angles: &[f64]) -> (f64, f64) {
    let mut sorted = angles.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mut best = (0., 0.);
    for w in sorted.windows(2) {
        if w[1] - w[0] > best.0 {
            best = (w[1] - w[0], w[0]);
        }
    }
    if let (Some(first), Some(last)) = (sorted.first(), sorted.last())
        && first + 360. - last > best.0
    {
        best = (first + 360. - last, *last);
    }
    best
}

/// The widest gap (degrees) between angles around the circle.
fn largest_gap(angles: &[f64]) -> f64 {
    let mut sorted = angles.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mut gap: f64 = 0.;
    for w in sorted.windows(2) {
        gap = gap.max(w[1] - w[0]);
    }
    if let (Some(first), Some(last)) = (sorted.first(), sorted.last()) {
        gap = gap.max(first + 360. - last);
    }
    gap
}

#[cfg(test)]
mod tests {
    use super::*;
    use cad2bem::cad::scene::{Face, Mode, SCHEMA};

    /// A pipe of radius 1 about z, from a slanted end z = 0.5 cos(theta) (where it meets another body) to a
    /// straight end z = 5; `seam` closes it around.
    fn pipe() -> Scene {
        let (n, m) = (36, 12);
        let mut positions = Vec::new();
        for j in 0..=m {
            for i in 0..n {
                let theta = std::f64::consts::TAU * i as f64 / n as f64;
                let bottom = 0.5 * theta.cos();
                let z = bottom + (5. - bottom) * j as f64 / m as f64;
                positions.push([theta.cos(), theta.sin(), z]);
            }
        }
        let id = |i: usize, j: usize| (j * n + i % n) as u32;
        let mut triangles = Vec::new();
        for j in 0..m {
            for i in 0..n {
                triangles.push([id(i, j), id(i + 1, j), id(i + 1, j + 1)]);
                triangles.push([id(i, j), id(i + 1, j + 1), id(i, j + 1)]);
            }
        }
        let count = triangles.len();
        Scene {
            open: None,
            schema: SCHEMA,
            source: "pipe.step".into(),
            sdk_version: "test".into(),
            mode: Mode::Preview,
            bounds_m: [-1., -1., -0.5, 1., 1., 5.],
            positions_m: positions,
            triangles,
            face_tags: vec![7; count],
            faces: vec![Face {
                tag: 7,
                kind: "Cylinder".into(),
                area_m2: 30.,
                triangles: count,
                surface: Some(Surface::Cylinder {
                    point: [0.; 3],
                    axis: [0., 0., 1.],
                    radius: 1.,
                }),
                corners: Vec::new(),
            }],
            prepared: None,
        }
    }

    #[test]
    fn a_slanted_pipe_end_is_cut_across_beyond_it_and_along_its_end_piece() {
        let scene = pipe();
        let made = cuts(&scene, 7).unwrap();
        // Across: normal along the axis, beyond the highest point of the end (z 0.5) by 0.4 radii.
        let across = &made[0];
        assert!((DVec3::from(across.normal) - DVec3::Z).length() < 1e-12);
        assert!((across.centre[2] - 0.9).abs() < 0.05, "{:?}", across.centre);
        // Along: at least three, each containing the axis and ending at the cut across.
        let along = &made[1..];
        assert!(along.len() >= 3, "{} cuts along", along.len());
        for cut in along {
            assert!(DVec3::from(cut.normal).dot(DVec3::Z).abs() < 1e-12);
            let top = cut.centre[2] + cut.half[0];
            assert!((top - across.centre[2]).abs() < 1e-9);
            assert!(cut.face_at.is_some() && cut.on.is_some());
        }
        // A pipe with two straight ends has no junction.
        let mut straight = pipe();
        for p in &mut straight.positions_m {
            p[2] = p[2].max(0.6);
        }
        assert!(cuts(&straight, 7).is_err());
    }

    #[test]
    fn a_hole_in_a_wall_is_framed_by_circles_and_lines() {
        // A wall of radius 3 about z, z -10..10, with a hole where |theta| < 20 deg and |z| < 2.
        let (n, m) = (72, 40);
        let mut positions = Vec::new();
        for j in 0..=m {
            for i in 0..n {
                let theta = std::f64::consts::TAU * i as f64 / n as f64;
                positions.push([
                    3. * theta.cos(),
                    3. * theta.sin(),
                    -10. + 20. * j as f64 / m as f64,
                ]);
            }
        }
        let id = |i: usize, j: usize| (j * n + i % n) as u32;
        let mut triangles = Vec::new();
        for j in 0..m {
            for i in 0..n {
                let theta = (i as f64 + 0.5) * 360. / n as f64;
                let z = -10. + 20. * (j as f64 + 0.5) / m as f64;
                if (theta < 20. || theta > 340.) && z.abs() < 2. {
                    continue;
                }
                triangles.push([id(i, j), id(i + 1, j), id(i + 1, j + 1)]);
                triangles.push([id(i, j), id(i + 1, j + 1), id(i, j + 1)]);
            }
        }
        let count = triangles.len();
        let scene = Scene {
            open: None,
            schema: SCHEMA,
            source: "wall.step".into(),
            sdk_version: "test".into(),
            mode: Mode::Preview,
            bounds_m: [-3., -3., -10., 3., 3., 10.],
            positions_m: positions,
            triangles,
            face_tags: vec![3; count],
            faces: vec![Face {
                tag: 3,
                kind: "Cylinder".into(),
                area_m2: 360.,
                triangles: count,
                surface: Some(Surface::Cylinder {
                    point: [0.; 3],
                    axis: [0., 0., 1.],
                    radius: 3.,
                }),
                corners: Vec::new(),
            }],
            prepared: None,
        };
        let made = windows(&scene);
        let circles: Vec<f64> = made
            .iter()
            .filter(|c| DVec3::from(c.normal).cross(DVec3::Z).length() < 1e-9)
            .map(|c| c.centre[2])
            .collect();
        let lines = made.len() - circles.len();
        // Above and below the hole (|z| 2, 0.2 radii away), and its top, bottom and opposite side.
        assert!(circles.iter().any(|z| (z - 2.6).abs() < 0.3), "{circles:?}");
        assert!(circles.iter().any(|z| (z + 2.6).abs() < 0.3), "{circles:?}");
        assert!(lines >= 3, "{lines} lines");
        // The pipe rule leaves this face alone: its ends are circles.
        assert!(cuts(&scene, 3).is_err());
    }
}

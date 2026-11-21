// SPDX-License-Identifier: GPL-3.0-or-later
//! CAD preparation before panel meshing: import, waterline and symmetry cuts, faces on the cut planes,
//! overlapping faces, fragment, and the side of every face from the CAD solids.
//!
//! Faces without CAD solids, or whose side the solids cannot decide, are left undecided by [`face_sides`];
//! the generalized winding number ([`super::winding`]) orients them.

use super::units::{FileUnit, file_unit};
use crate::{
    cad::{
        gmsh::import_metres,
        session::{DimTag, Session},
    },
    conversion::{Cut, Sheets, Surface, Symmetry},
};
use anyhow::{Result, bail};
use glam::DVec3;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{LazyLock, Mutex},
};

mod cuts;
mod overlaps;
mod sheets;
mod sides;
mod symmetry;
mod waterplane;

use overlaps::{boxes_overlap, distance, overlaps, samples};
use sheets::{keep_largest, remove_sheets};
use symmetry::cut_symmetry;

pub(crate) use cuts::apply_cuts;
pub use sides::{Sides, face_sides};
pub use waterplane::{waterplane, waterplane_on_body};

/// Relative distance within which a sample lies on another face (`overlap_tol`).
const OVERLAP_TOL: f64 = 1e-4;
/// Relative mirror distance and relative area difference a symmetric body may have.
const MIRROR_TOL: f64 = 1e-4;
const AREA_TOL: f64 = 1e-3;

/// Progress callback: stage name and data.
pub type Progress<'a> = &'a mut dyn FnMut(&str, Value);

/// What the preparation needs from a conversion request.
#[derive(Clone, Debug)]
pub struct Options {
    /// Factor to metres of the file's numbers, replacing the unit the file states (None: the file's own).
    pub unit: Option<f64>,
    pub symmetry: Symmetry,
    /// Cut at the waterline z = 0 and keep the part below.
    pub waterline: bool,
    /// Faces removed as sheets after the fragment.
    pub sheets: Sheets,
    /// Planar cuts that partition the faces after the waterline and symmetry cuts.
    pub cuts: Vec<Cut>,
}

/// The prepared model state and the facts of the report's `cad` record.
pub struct Prepared {
    pub file_unit: FileUnit,
    pub unit_m: Option<f64>,
    /// Scaling applied at import (the requested unit over the file's unit).
    pub scaling: f64,
    pub bounding_box: [f64; 6],
    /// The body size: the longest side of the bounding box before any cut. Tolerances are relative to it.
    pub scale: f64,
    /// Symmetry axes the whole body reaches across (cut there).
    pub halves: Vec<usize>,
    /// Volume of the CAD solids below the water (all of it without the waterline cut), one side of each
    /// symmetry plane the body was cut at.
    pub volume_cad: Option<f64>,
    pub has_solids: bool,
    pub faces: Vec<i32>,
    imported: usize,
    cut_at_waterline: usize,
    not_cut_at_waterline: usize,
    dropped: usize,
    overlaps: Vec<Value>,
    symmetry_cut: Option<Value>,
    cuts: Value,
    cad_sheets: Vec<Value>,
}

impl Prepared {
    /// The report's `cad` record of a free panel conversion.
    pub fn cad_record(&self) -> Value {
        json!({
            "file_unit": self.file_unit.name, "file_unit_m": self.file_unit.metres, "unit_m": self.unit_m,
            "faces_imported": self.imported, "faces_cut_at_waterline": self.cut_at_waterline,
            "faces_not_cut_at_waterline": self.not_cut_at_waterline, "faces_dropped": self.dropped,
            "face_overlaps": self.overlaps, "cad_sheets": self.cad_sheets, "recipe_faces_unknown": [],
            "symmetry_cut": self.symmetry_cut, "cuts": self.cuts, "remeshed": {}, "face_paths": {}, "faces_unsided": [],
        })
    }
}

fn plane_name(axis: usize) -> &'static str {
    if axis == 0 { "yz" } else { "xz" }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// Read a file into the current model in metres, with triangulation bounding boxes.
fn import(gmsh: &Session, path: &Path, scaling: f64) -> Result<()> {
    // Boxes from the triangulation: OpenCASCADE's own are widened by the shape tolerance, which made a face
    // on z = 0 look as if it crossed the waterline, and a trimmed piece keeps the box of its untrimmed surface.
    gmsh.number("Geometry.OCCBoundsUseStl", 1.)?;
    import_metres(gmsh, path, scaling)
}

/// A file read into the model: its unit, the import scaling, the bounding box and the body size.
struct Opened {
    unit: FileUnit,
    unit_m: Option<f64>,
    scaling: f64,
    bounding_box: [f64; 6],
    scale: f64,
}

/// Unit, import, size and the `read` progress event shared by the body and the waterplane.
fn open(
    gmsh: &Session,
    path: &Path,
    options: &Options,
    progress: Option<Progress>,
) -> Result<Opened> {
    let unit = file_unit(path)?;
    let (unit_m, scaling) = match (options.unit, unit.metres) {
        (None, metres) => (metres, 1.),
        (Some(_), None) => {
            let why = if unit.name.as_deref() == Some("mixed") {
                "states different length units"
            } else {
                "states no length unit that cad2bem reads"
            };
            bail!(
                "{}: the file {why}, so a unit cannot replace it; convert with unit = 'file'",
                display(path)
            )
        }
        (Some(requested), Some(metres)) => (Some(requested), requested / metres),
    };
    import(gmsh, path, scaling)?;
    let b = gmsh.bounding_box(-1, -1)?;
    let scale = (b[3] - b[0]).max(b[4] - b[1]).max(b[5] - b[2]);
    if let Some(progress) = progress {
        progress(
            "read",
            json!({"file_unit": unit.name, "file_unit_m": unit.metres, "unit_m": unit_m,
                "body_size_m": scale, "bounding_box_m": b}),
        );
    }
    Ok(Opened {
        unit,
        unit_m,
        scaling,
        bounding_box: b,
        scale,
    })
}

fn halves(symmetry: Symmetry, b: &[f64; 6], scale: f64) -> Vec<usize> {
    symmetry
        .axes()
        .iter()
        .copied()
        .filter(|&k| b[k] < -1e-6 * scale)
        .collect()
}

/// A box around the model below z = 0 (`whole`: at every height), on the positive side of `halves`.
fn below_water_box(gmsh: &Session, halves: &[usize], whole: bool) -> Result<i32> {
    let [x0, y0, z0, x1, y1, z1] = gmsh.bounding_box(-1, -1)?;
    let lo: [f64; 2] = std::array::from_fn(|k| {
        if halves.contains(&k) {
            0.
        } else {
            [x0, y0][k] - 1.
        }
    });
    let top = if whole { z1.max(0.) + 1. } else { 0. };
    gmsh.add_box(
        [lo[0], lo[1], z0 - 1.],
        [x1 + 1. - lo[0], y1 + 1. - lo[1], top - (z0 - 1.)],
    )
}

/// The volume of solids below z = 0, measured on copies; the solids are left as they are.
fn volume_below_water(
    gmsh: &Session,
    solids: &[DimTag],
    halves: &[usize],
    whole: bool,
) -> Result<f64> {
    let copies = gmsh.copy(solids)?;
    let tool = below_water_box(gmsh, halves, whole)?;
    let out = gmsh.intersect(&copies, &[(3, tool)])?.out;
    gmsh.synchronize()?;
    let mut volume = 0.;
    for &(_, t) in &out {
        volume += gmsh.mass(3, t)?;
    }
    gmsh.remove(&out, true)?;
    gmsh.synchronize()?;
    Ok(volume)
}

/// Import a body and prepare its wetted faces for free panels.
pub fn body(
    gmsh: &Session,
    path: &Path,
    options: &Options,
    progress: Progress,
) -> Result<Prepared> {
    let Opened {
        unit,
        unit_m,
        scaling,
        bounding_box,
        scale,
    } = open(gmsh, path, options, Some(progress))?;
    let halves = halves(options.symmetry, &bounding_box, scale);
    let solids = gmsh.entities(3)?;
    let has_solids = !solids.is_empty();
    let volume_cad = if has_solids {
        Some(volume_below_water(
            gmsh,
            &solids,
            &halves,
            !options.waterline,
        )?)
    } else {
        None
    };
    if has_solids {
        gmsh.remove(&gmsh.entities(3)?, false)?; // keep the faces only
        gmsh.synchronize()?;
    } else if gmsh.tags(2)?.len() > 1 {
        // Loose faces that only touch along their edges share no curve, and the fragment does not join them:
        // they are sewn first. A single face is not: sewing would drop its poles and close a closed face.
        gmsh.sew_faces(1e-5 * scale)?;
        gmsh.synchronize()?;
    }
    let imported = gmsh.tags(2)?.len();
    if imported == 0 {
        bail!("{}: no surfaces in the file", display(path));
    }
    let tol = 1e-6 * scale;
    let (mut cut_at_waterline, mut not_cut_at_waterline) = (0, 0);
    if options.waterline {
        for f in gmsh.tags(2)? {
            let [x0, y0, z0, x1, y1, z1] = gmsh.bounding_box(2, f)?;
            if !(z0 < -tol && z1 > tol) {
                continue;
            }
            let plane = gmsh.add_rectangle([x0 - 1., y0 - 1., 0.], x1 - x0 + 2., y1 - y0 + 2.)?;
            let pieces = gmsh.fragment(&[(2, f)], &[(2, plane)])?.map;
            gmsh.synchronize()?;
            // A piece keeps the bounding box of its untrimmed surface: its centre decides.
            let mut drop = pieces[1].clone();
            for &piece in &pieces[0] {
                if gmsh.center_of_mass(piece.0, piece.1)?[2] > 0. {
                    drop.push(piece);
                }
            }
            gmsh.remove(&drop, true)?;
            gmsh.synchronize()?;
            if pieces[0].len() > 1 {
                cut_at_waterline += 1;
            } else {
                not_cut_at_waterline += 1;
            }
        }
    }
    let symmetry_cut = if halves.is_empty() {
        None
    } else {
        Some(cut_symmetry(gmsh, path, options.symmetry, &halves, scale)?)
    };
    let mut planes: Vec<usize> = if options.waterline { vec![2] } else { vec![] };
    planes.extend(options.symmetry.axes());
    let mut drop = Vec::new();
    for f in gmsh.tags(2)? {
        let b = gmsh.bounding_box(2, f)?;
        if (options.waterline && b[2] > -tol)
            || planes
                .iter()
                .any(|&k| b[k].abs() < tol && b[k + 3].abs() < tol)
        {
            drop.push((2, f));
        }
    }
    gmsh.remove(&drop, true)?;
    gmsh.synchronize()?;
    let cuts = apply_cuts(gmsh, &options.cuts, scale)?;
    let overlaps = overlaps(gmsh, scale)?;
    let before = gmsh.entities(2)?;
    // The face each fragment piece came from.
    let mut parent: HashMap<i32, i32> = before.iter().map(|&(_, t)| (t, t)).collect();
    if before.len() > 1 {
        let pieces = gmsh.fragment(&before, &[])?;
        gmsh.synchronize()?;
        parent = before
            .iter()
            .zip(&pieces.map)
            .flat_map(|(&(_, p), kids)| kids.iter().map(move |&(_, t)| (t, p)))
            .collect();
    }
    let mut cad_sheets = Vec::new();
    if options.sheets != Sheets::None {
        remove_sheets(gmsh, &parent, options.symmetry, scale, &mut cad_sheets)?;
    }
    if options.sheets == Sheets::Largest {
        keep_largest(gmsh, &parent, &mut cad_sheets)?;
    }
    Ok(Prepared {
        file_unit: unit,
        unit_m,
        scaling,
        bounding_box,
        scale,
        halves,
        volume_cad,
        has_solids,
        faces: gmsh.tags(2)?,
        imported,
        cut_at_waterline,
        not_cut_at_waterline,
        dropped: drop.len(),
        overlaps,
        symmetry_cut,
        cuts,
        cad_sheets,
    })
}

/// Index of the first smallest value.
fn argmin(values: impl Iterator<Item = f64>) -> usize {
    let mut best = (0, f64::INFINITY);
    for (i, v) in values.enumerate() {
        if v < best.1 {
            best = (i, v);
        }
    }
    best.0
}

/// Index of the first largest value.
fn argmax(values: impl Iterator<Item = f64>) -> usize {
    let mut best = (0, f64::NEG_INFINITY);
    for (i, v) in values.enumerate() {
        if v > best.1 {
            best = (i, v);
        }
    }
    best.0
}

/// Evenly spaced values as NumPy's `linspace` computes them: `i * step + start`, the last one `stop`.
pub fn linspace(start: f64, stop: f64, n: usize) -> Vec<f64> {
    let step = (stop - start) / (n - 1) as f64;
    let mut out: Vec<f64> = (0..n).map(|i| i as f64 * step + start).collect();
    if let Some(last) = out.last_mut() {
        *last = stop;
    }
    out
}

/// A number with `digits` significant digits in the `%g` style of C's printf (no trailing zeros,
/// exponent below 1e-4 or from 10^digits), for messages.
pub fn general(x: f64, digits: usize) -> String {
    if x == 0. || !x.is_finite() {
        return format!("{x}");
    }
    let exponent = x.abs().log10().floor() as i32;
    let rounded: f64 = format!("{x:.*e}", digits - 1).parse().unwrap_or(x);
    let exponent = if rounded.abs() >= 10f64.powi(exponent + 1) {
        exponent + 1
    } else {
        exponent
    };
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s
        }
    };
    if exponent < -4 || exponent >= digits as i32 {
        let mantissa = rounded / 10f64.powi(exponent);
        let sign = if exponent < 0 { '-' } else { '+' };
        format!(
            "{}e{sign}{:02}",
            trim(format!("{mantissa:.*}", digits - 1)),
            exponent.abs()
        )
    } else {
        let decimals = (digits as i32 - 1 - exponent).max(0) as usize;
        trim(format!("{rounded:.decimals$}"))
    }
}

#[cfg(test)]
mod tests {
    use super::general;

    #[test]
    fn general_matches_percent_g() {
        assert_eq!(general(11.2, 3), "11.2");
        assert_eq!(general(0.4230, 3), "0.423");
        assert_eq!(general(1.23456e-5, 3), "1.23e-05");
        assert_eq!(general(123456., 3), "1.23e+05");
        assert_eq!(general(99.96, 3), "100");
        assert_eq!(general(2.0, 3), "2");
    }
}

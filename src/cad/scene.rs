// SPDX-License-Identifier: GPL-3.0-or-later
//! Inspection scenes owned by the application. CAD face identity is kept separate from display triangles.

use std::{collections::HashMap, path::PathBuf};

use crate::conversion::{Cut, Settings, Sheets, Symmetry};

use anyhow::{Result, ensure};
use glam::DVec3;
use serde::{Deserialize, Serialize};

/// The schema version of view requests and scenes.
pub const SCHEMA: u32 = 1;

/// What a scene shows: the CAD preview, a Gmsh surface mesh, or BEM panels or patches (these two come from a
/// conversion, not a view job).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Preview,
    SurfaceMesh,
    BemPanels,
    BemPatches,
}

/// A view job for the worker: the CAD file, the Gmsh SDK, what to show and the surface mesh size (m).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u32,
    pub source: PathBuf,
    pub library: Option<PathBuf>,
    pub mode: Mode,
    pub size_m: Option<f64>,
    /// Show the CAD as the conversion prepares it (cuts, sheet removal) instead of as imported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<Preparation>,
    /// Also write the CAD as shown (prepared or imported) to this STEP file, in metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_cad: Option<PathBuf>,
    /// Resident view server: when it holds the prepared model of this preparation without its last cut, cut
    /// only that model and mark the view provisional (`prepared.provisional`); otherwise a full view.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quick: bool,
}

/// The CAD preparation of a conversion request: the same steps, in the same order, as the conversion runs
/// before it meshes, so that a preview of it shows the faces (and face numbers) the conversion will use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    pub unit: String,
    pub cut_waterline: bool,
    pub symmetry: Symmetry,
    #[serde(default)]
    pub remove_sheets: Sheets,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<Cut>,
}

impl Preparation {
    /// The preparation part of conversion settings.
    pub fn of(settings: &Settings) -> Self {
        Self {
            unit: settings.unit.clone(),
            cut_waterline: settings.cut_waterline,
            symmetry: settings.symmetry,
            remove_sheets: settings.remove_sheets,
            cuts: settings.cuts.clone(),
        }
    }
}

impl Request {
    /// Check the request before a worker runs it: a STEP save path, a preview or surface mesh mode, the schema,
    /// an existing STEP or IGES source, the cuts of a prepared preview and a positive surface mesh size.
    pub fn validate(&self) -> Result<()> {
        if let Some(path) = &self.save_cad {
            let step = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("step") || e.eq_ignore_ascii_case("stp"));
            ensure!(step, "The CAD is saved as STEP (.step or .stp)");
        }
        ensure!(
            matches!(self.mode, Mode::Preview | Mode::SurfaceMesh),
            "BEM geometry requires a conversion job"
        );
        ensure!(
            self.schema == SCHEMA,
            "Unsupported job schema {}",
            self.schema
        );
        ensure!(
            self.source.is_file(),
            "CAD file does not exist: {}",
            self.source.display()
        );
        let extension = self
            .source
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        ensure!(
            ["step", "stp", "iges", "igs"].contains(&extension.as_str()),
            "Input must be STEP or IGES"
        );
        if let Some(preparation) = &self.preparation {
            ensure!(
                self.mode == Mode::Preview,
                "A prepared CAD view is a preview"
            );
            for cut in &preparation.cuts {
                cut.validate()?;
            }
        }
        if self.mode == Mode::SurfaceMesh {
            ensure!(
                self.size_m.is_some_and(|s| s.is_finite() && s > 0.0),
                "Surface mesh size must be finite and positive (metres)"
            );
        }
        Ok(())
    }
}

/// A CAD face of a scene: its tag, surface kind, area (m^2) and number of display triangles.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Face {
    pub tag: i32,
    pub kind: String,
    pub area_m2: f64,
    pub triangles: usize,
    /// The cylinder or plane the face lies on, when it is one (a CAD preview), for cuts through it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<crate::conversion::Surface>,
    /// The CAD vertices of the face (m).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corners: Vec<[f64; 3]>,
}

/// A display mesh of a CAD file: positions (m), triangles, the CAD face of each triangle, and the faces.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scene {
    pub schema: u32,
    pub source: PathBuf,
    pub sdk_version: String,
    pub mode: Mode,
    pub bounds_m: [f64; 6],
    pub positions_m: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub face_tags: Vec<i32>,
    pub faces: Vec<Face>,
    /// For a prepared CAD view: what the preparation did (the report's `cad` record of a conversion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<serde_json::Value>,
    /// CAD views: the open (unshared) edges, by where they lie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<OpenEdges>,
}

/// The edges of a CAD view that belong to one face only: those on the coordinate planes x = 0, y = 0 and z = 0
/// (a model cut at a symmetry plane or at the waterline) and the others.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenEdges {
    pub on_x0: usize,
    pub on_y0: usize,
    pub on_z0: usize,
    pub elsewhere: usize,
}

impl Scene {
    /// Check a scene from a worker: the schema, finite bounds and positions, indices in range, and triangle
    /// counts that match the faces.
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "Unsupported scene schema");
        ensure!(
            !self.positions_m.is_empty() && !self.triangles.is_empty(),
            "CAD contains no displayable surface mesh"
        );
        ensure!(
            self.bounds_m.iter().all(|v| v.is_finite()),
            "Non-finite CAD bounds"
        );
        ensure!(
            (0..3).all(|i| self.bounds_m[i] <= self.bounds_m[i + 3]),
            "Invalid CAD bounds"
        );
        ensure!(
            self.positions_m.iter().flatten().all(|v| v.is_finite()),
            "Non-finite mesh coordinates"
        );
        ensure!(
            self.triangles.len() == self.face_tags.len(),
            "Triangle/face ownership mismatch"
        );
        ensure!(
            self.triangles.len() <= u32::MAX as usize / 3,
            "Mesh exceeds viewport vertex capacity"
        );
        ensure!(
            self.triangles
                .iter()
                .flatten()
                .all(|&i| (i as usize) < self.positions_m.len()),
            "Mesh index outside position buffer"
        );
        let counts =
            self.face_tags
                .iter()
                .fold(HashMap::<i32, usize>::new(), |mut counts, &tag| {
                    *counts.entry(tag).or_default() += 1;
                    counts
                });
        let faces: HashMap<_, _> = self.faces.iter().map(|f| (f.tag, f)).collect();
        ensure!(faces.len() == self.faces.len(), "Duplicate CAD face tag");
        ensure!(
            counts.keys().all(|tag| faces.contains_key(tag)),
            "Triangle references unknown CAD face"
        );
        for f in &self.faces {
            ensure!(f.tag > 0, "CAD face tags must be positive");
            ensure!(
                f.area_m2.is_finite() && f.area_m2 >= 0.,
                "Invalid CAD face area"
            );
            ensure!(
                counts.get(&f.tag).copied().unwrap_or(0) == f.triangles,
                "CAD face triangle count mismatch"
            );
        }
        ensure!(
            self.radius() > 0. && self.radius().is_finite(),
            "Collapsed CAD extents"
        );
        Ok(())
    }

    /// The centre of the bounding box (m).
    pub fn centre(&self) -> DVec3 {
        (DVec3::from_slice(&self.bounds_m[..3]) + DVec3::from_slice(&self.bounds_m[3..])) * 0.5
    }

    /// Half the largest bounding-box extent (m).
    pub fn radius(&self) -> f64 {
        (0..3)
            .map(|i| self.bounds_m[i + 3] - self.bounds_m[i])
            .fold(0., f64::max)
            * 0.5
    }

    /// The positions centred on the bounding box and divided by [`Self::radius`], for display.
    pub fn normalized_positions(&self) -> Vec<glam::Vec3> {
        let centre = self.centre();
        let radius = self.radius();
        self.positions_m
            .iter()
            .map(|&p| ((DVec3::from_array(p) - centre) / radius).as_vec3())
            .collect()
    }

    /// The shape quality of triangle `index`.
    pub fn quality(&self, index: usize) -> Quality {
        let p = self.triangles[index].map(|i| DVec3::from_array(self.positions_m[i as usize]));
        let lengths = [
            (p[1] - p[0]).length(),
            (p[2] - p[1]).length(),
            (p[0] - p[2]).length(),
        ];
        let area = (p[1] - p[0]).cross(p[2] - p[0]).length() * 0.5;
        let mut min_angle = 180_f64;
        for i in 0..3 {
            let a = p[(i + 1) % 3] - p[i];
            let b = p[(i + 2) % 3] - p[i];
            let denominator = a.length() * b.length();
            let angle = if denominator > 0. {
                (a.dot(b) / denominator).clamp(-1., 1.).acos().to_degrees()
            } else {
                0.
            };
            min_angle = min_angle.min(angle);
        }
        let longest = lengths.into_iter().fold(0., f64::max);
        Quality {
            min_angle,
            aspect: if area > 0. {
                longest * longest / (2. * area)
            } else {
                f64::INFINITY
            },
            area,
        }
    }
}

/// The shape quality of a display triangle: the smallest angle (degrees), the aspect ratio (longest edge
/// squared over twice the area) and the area (m^2).
#[derive(Clone, Copy, Debug)]
pub struct Quality {
    pub min_angle: f64,
    pub aspect: f64,
    pub area: f64,
}

impl Quality {
    /// Whether the triangle is poor: an angle below 20 degrees, an infinite aspect ratio or no area.
    pub fn warned(self) -> bool {
        self.min_angle < 20. || !self.aspect.is_finite() || self.area <= 0.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> Scene {
        Scene {
            open: None,
            schema: SCHEMA,
            source: "test.step".into(),
            sdk_version: "test".into(),
            mode: Mode::Preview,
            bounds_m: [0., 0., 0., 1., 1., 0.],
            positions_m: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            triangles: vec![[0, 1, 2]],
            face_tags: vec![17],
            faces: vec![Face {
                tag: 17,
                kind: "Plane".into(),
                area_m2: 0.5,
                triangles: 1,
                surface: None,
                corners: Vec::new(),
            }],
            prepared: None,
        }
    }

    #[test]
    fn rejects_broken_ownership_and_nonfinite_geometry() {
        let mut scene = triangle();
        scene.validate().unwrap();
        scene.face_tags[0] = 99;
        assert!(scene.validate().is_err());
        scene.face_tags[0] = 17;
        scene.triangles[0][0] = 3;
        assert!(scene.validate().is_err());
        scene.triangles[0][0] = 0;
        scene.positions_m[0][0] = f64::NAN;
        assert!(scene.validate().is_err());
    }

    #[test]
    fn triangle_quality_and_large_coordinate_normalization() {
        let mut scene = triangle();
        assert!((scene.quality(0).min_angle - 45.).abs() < 1e-10);
        assert!(!scene.quality(0).warned());
        for p in &mut scene.positions_m {
            p[0] += 1e9;
        }
        scene.bounds_m[0] += 1e9;
        scene.bounds_m[3] += 1e9;
        let positions = scene.normalized_positions();
        assert!((positions[1].x - positions[0].x - 2.).abs() < 1e-6);
    }
}

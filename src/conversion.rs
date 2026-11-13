// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared conversion requests, jobs and validated artifacts used by both the GUI and the CLI.
//!
//! A conversion runs in a private child process, this executable started as `--convert-worker`, which writes
//! the request, progress, result and artifact files into a private directory. This module owns the request
//! schema, the worker lifecycle, result validation and the no-clobber publication of the staged files. It has
//! no GUI or graphics dependency.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Instant,
};

/// Version of the private request/result files exchanged with the conversion worker.
pub const SCHEMA: u32 = 1;
/// Version of the geometry files (`.hbg`) written and read; a file of another version is refused.
pub const HBG_VERSION: u32 = 3;
/// Panel size in metres used when a mode needs panels and none is given.
pub const DEFAULT_PANEL_SIZE_M: f64 = 2.;
/// Patch size in metres used when a mode needs spline patches and none is given.
pub const DEFAULT_PATCH_SIZE_M: f64 = 8.;

/// Conversion mode: free low-order panels, structured low-order panels, or high-order spline patches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    #[serde(rename = "lo-free")]
    LoFree,
    #[serde(rename = "lo-str")]
    LoStr,
    #[serde(rename = "ho")]
    Ho,
}

impl Mode {
    /// Every mode, in the order the command line and the GUI list them.
    pub const ALL: [Self; 3] = [Self::LoFree, Self::LoStr, Self::Ho];

    /// The command-line name of the mode: `lo-free`, `lo-str` or `ho`.
    pub fn name(self) -> &'static str {
        match self {
            Self::LoFree => "lo-free",
            Self::LoStr => "lo-str",
            Self::Ho => "ho",
        }
    }

    /// The mode of a command-line name (`--mode`).
    pub fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.name() == value)
            .context("--mode: choose lo-free, lo-str or ho")
    }

    /// Whether the mode meshes low-order panels and therefore needs a panel size.
    pub fn uses_panel_size(self) -> bool {
        self != Self::Ho
    }

    /// Whether the mode builds spline patches and therefore needs a patch size.
    pub fn uses_patch_size(self) -> bool {
        self != Self::LoFree
    }

    /// Exported element name in summaries and messages: `patches` for ho, `panels` otherwise.
    pub fn elements(self) -> &'static str {
        if self == Self::Ho {
            "patches"
        } else {
            "panels"
        }
    }

    /// Reject an output format the mode cannot write; GDF holds low-order panels only.
    pub fn check_output(self, output: &Path) -> Result<()> {
        ensure!(
            self != Self::Ho || Format::from_path(output)? == Format::Hbg,
            "HO exports require .hbg; GDF and STL take low-order panels only"
        );
        Ok(())
    }
}

/// Symmetry planes of the body by plane name, as the geometry file names them: xz is y = 0, yz is x = 0,
/// xz_and_yz both vertical planes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Symmetry {
    #[default]
    None,
    Xz,
    Yz,
    XzAndYz,
}

impl Symmetry {
    /// Every choice of symmetry planes.
    pub const ALL: [Self; 4] = [Self::None, Self::Xz, Self::Yz, Self::XzAndYz];

    /// The name of these planes, the one the geometry file (HBG version 3) and the command line use: `none`,
    /// `yz` (x = 0), `xz` (y = 0) or `xz_and_yz` (both).
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Xz => "xz",
            Self::Yz => "yz",
            Self::XzAndYz => "xz_and_yz",
        }
    }

    /// The planes of a name (`name`), if it is one.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Coordinate indices normal to the planes: x for yz, y for xz.
    pub fn axes(self) -> &'static [usize] {
        match self {
            Self::None => &[],
            Self::Yz => &[0],
            Self::Xz => &[1],
            Self::XzAndYz => &[0, 1],
        }
    }

    /// The copies of the body its symmetry planes make.
    pub fn images(self) -> f64 {
        f64::from(1_u32 << self.axes().len())
    }

    /// The plane name with the coordinate planes it stands for, for GUI labels.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Xz => "xz (y = 0)",
            Self::Yz => "yz (x = 0)",
            Self::XzAndYz => "xz_and_yz (x = 0 and y = 0)",
        }
    }

    /// The planes of a command-line name (`--symmetry`).
    pub fn parse(value: &str) -> Result<Self> {
        Self::from_name(value).context("--symmetry: choose none, xz, yz or xz_and_yz")
    }
}

/// Which CAD faces are removed as sheets after the fragment: none, the topological rules (a piece with a
/// free edge; a face split where three or more pieces meet keeps its largest piece), or also every connected
/// group of faces but the largest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sheets {
    #[default]
    None,
    Rules,
    Largest,
}

impl Sheets {
    /// Every sheet-removal choice.
    pub const ALL: [Self; 3] = [Self::None, Self::Rules, Self::Largest];

    /// The command-line name of the choice: `none`, `rules` or `largest`.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rules => "rules",
            Self::Largest => "largest",
        }
    }

    /// The choice of a command-line name (`--remove-sheets`).
    pub fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.name() == value)
            .context("--remove-sheets: choose none, rules or largest")
    }

    fn is_none(&self) -> bool {
        *self == Self::None
    }
}

/// The quad mesher of faces split into patches: triangles split in three quads (tri3), Gmsh's quasi-structured
/// quads (qqs), or recombined triangles each split in four (subdiv).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatchMethod {
    #[default]
    Tri3,
    Qqs,
    Subdiv,
}

impl PatchMethod {
    /// Every patch quad mesher.
    pub const ALL: [Self; 3] = [Self::Tri3, Self::Qqs, Self::Subdiv];

    /// The command-line name of the mesher: `tri3`, `qqs` or `subdiv`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Tri3 => "tri3",
            Self::Qqs => "qqs",
            Self::Subdiv => "subdiv",
        }
    }

    /// The mesher of a command-line name (`--patch-method`).
    pub fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.name() == value)
            .context("--patch-method: choose tri3, qqs or subdiv")
    }

    fn is_default(&self) -> bool {
        *self == Self::Tri3
    }
}

/// A planar cut of the prepared faces: a rectangle with its centre, its normal, its first axis (the second is
/// normal × axis) and its half sizes along the two axes, in metres of the model after import. Every face whose
/// bounding box meets the rectangle is split along their intersection, after the waterline and symmetry cuts.
/// Cuts partition faces that have no four-sided layout (trimmed junctions) into pieces that have; a cut should
/// cross the faces it meets from boundary to boundary. `on` limits a cut to the faces lying on one surface (a
/// column wall, a pipe), so that it can pass through a point where that surface meets another without cutting it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cut {
    pub centre: [f64; 3],
    pub normal: [f64; 3],
    pub axis: [f64; 3],
    pub half: [f64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Surface>,
    /// Cut only the one face through this point (m), as the faces are when the cut is made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_at: Option<[f64; 3]>,
}

/// A surface that limits a cut, in metres of the model: a cylinder (a point on its axis, the axis direction,
/// the radius) or a plane (a point and the normal).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Surface {
    Cylinder {
        point: [f64; 3],
        axis: [f64; 3],
        radius: f64,
    },
    Plane {
        point: [f64; 3],
        normal: [f64; 3],
    },
}

impl Surface {
    /// The distance of a point from the surface.
    pub fn distance(&self, p: [f64; 3]) -> f64 {
        let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        match self {
            Self::Cylinder {
                point,
                axis,
                radius,
            } => {
                let d = sub(p, *point);
                let along = dot(d, *axis) / dot(*axis, *axis);
                let off = [
                    d[0] - along * axis[0],
                    d[1] - along * axis[1],
                    d[2] - along * axis[2],
                ];
                (dot(off, off).sqrt() - radius).abs()
            }
            Self::Plane { point, normal } => {
                (dot(sub(p, *point), *normal) / dot(*normal, *normal).sqrt()).abs()
            }
        }
    }

    fn validate(&self) -> Result<()> {
        let (values, direction, positive): (Vec<f64>, [f64; 3], bool) = match self {
            Self::Cylinder {
                point,
                axis,
                radius,
            } => (
                [&point[..], &axis[..], &[*radius]].concat(),
                *axis,
                *radius > 0.,
            ),
            Self::Plane { point, normal } => ([&point[..], &normal[..]].concat(), *normal, true),
        };
        ensure!(
            values.iter().all(|x| x.is_finite()),
            "Cut surface values must be finite"
        );
        ensure!(
            direction.iter().any(|&x| x != 0.) && positive,
            "Cut surface needs a non-zero axis or normal and a positive radius"
        );
        Ok(())
    }
}

impl Cut {
    /// Reject non-finite values, a zero normal or axis, an axis not in the plane and non-positive sizes.
    pub fn validate(&self) -> Result<()> {
        let finite = self
            .centre
            .iter()
            .chain(&self.normal)
            .chain(&self.axis)
            .chain(&self.half)
            .chain(self.face_at.iter().flatten())
            .all(|x| x.is_finite());
        ensure!(finite, "Cut values must be finite");
        let length = |v: &[f64; 3]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
        let (n, a) = (length(&self.normal), length(&self.axis));
        ensure!(n > 0. && a > 0., "Cut normal and axis must not be zero");
        let dot: f64 = self.normal.iter().zip(&self.axis).map(|(x, y)| x * y).sum();
        ensure!(
            dot.abs() <= 1e-9 * n * a,
            "Cut axis must lie in the cut plane (perpendicular to its normal)"
        );
        ensure!(
            self.half.iter().all(|&h| h > 0.),
            "Cut half sizes must be positive"
        );
        if let Some(surface) = &self.on {
            surface.validate()?;
        }
        Ok(())
    }
}

/// How lo-free meshes the CAD faces: mostly quadrilaterals with some triangles (recombined), quasi-structured
/// quadrilaterals, all quadrilaterals (recombined, then each element split into quadrilaterals), or triangles
/// only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelMethod {
    #[default]
    Recombined,
    Qqs,
    Quads,
    /// Triangles only (the triangulation, not recombined).
    Triangles,
}

impl PanelMethod {
    /// Every panel method.
    pub const ALL: [Self; 4] = [Self::Recombined, Self::Qqs, Self::Quads, Self::Triangles];

    /// The command-line name of the method: `recombined`, `qqs`, `quads` or `triangles`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Recombined => "recombined",
            Self::Qqs => "qqs",
            Self::Quads => "quads",
            Self::Triangles => "triangles",
        }
    }

    /// The method of a command-line name (`--panel-method`).
    pub fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.name() == value)
            .context("--panel-method: choose recombined, qqs, quads or triangles")
    }

    fn is_default(&self) -> bool {
        *self == Self::Recombined
    }
}

/// How a prepared face is made into spline patches. `Auto` tries the layouts in order and splits the face
/// when none fits; the others force one layout (a face it does not fit is split and the report says so).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FacePath {
    #[default]
    Auto,
    Direct,
    Single,
    Polygon,
    Ogrid,
    Ring,
    Split,
}

impl FacePath {
    /// Every face path, `Auto` first.
    pub const ALL: [Self; 7] = [
        Self::Auto,
        Self::Direct,
        Self::Single,
        Self::Polygon,
        Self::Ogrid,
        Self::Ring,
        Self::Split,
    ];

    /// The name used in requests and reports.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Direct => "direct",
            Self::Single => "single",
            Self::Polygon => "polygon",
            Self::Ogrid => "ogrid",
            Self::Ring => "ring",
            Self::Split => "split",
        }
    }

    fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
}

/// The settings of one prepared face, by its number (the number of the prepared view and of the converted
/// result). `priority`: where faces with different sizes share a curve, the higher priority sets its mesh.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaceSetting {
    pub face: i32,
    #[serde(default, skip_serializing_if = "FacePath::is_auto")]
    pub path: FacePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_size: Option<f64>,
    /// lo-free: the panel size of this face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_size: Option<f64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub priority: i32,
}

impl FaceSetting {
    /// Drop what `mode` does not use (a layout and patch size for lo-free, a panel size otherwise).
    pub fn for_mode(&self, mode: Mode) -> Self {
        let mut out = self.clone();
        if mode == Mode::LoFree {
            out.path = FacePath::Auto;
            out.patch_size = None;
        } else {
            out.panel_size = None;
        }
        out
    }

    /// Whether the setting changes nothing.
    pub fn is_default(&self) -> bool {
        self.path == FacePath::Auto
            && self.patch_size.is_none()
            && self.panel_size.is_none()
            && self.priority == 0
    }
}

fn is_zero(x: &i32) -> bool {
    *x == 0
}

/// The potential panel size (m) when a request sets none: written to every ho HBG file as `panel_size` (so a
/// reader needs no discretization of its own) and used by the span preview.
pub const DEFAULT_POTENTIAL_PANEL_SIZE_M: f64 = 2.;

/// The potential spans (u, v) of a patch by the solver's panel-size rule: floor(L / size + 1e-5) + 1, with L
/// the longer arc length of the patch's two edges in that direction (`lengths`, metres).
pub fn potential_spans(lengths: [f64; 2], size: f64) -> [usize; 2] {
    lengths.map(|l| (l / size + 1e-5) as usize + 1)
}

/// Conversion settings shared by the GUI and the CLI. Sizes are present exactly when the mode uses them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_size: Option<f64>,
    /// ho: the potential panel size (m) written to the HBG files (`panel_size`): the solver sets the spans of
    /// each patch from it, as the span preview shows. Without it the files carry no discretization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub potential_panel_size: Option<f64>,
    pub unit: String,
    pub cut_waterline: bool,
    pub symmetry: Symmetry,
    pub interior: bool,
    /// Faces removed as sheets after the fragment.
    #[serde(default, skip_serializing_if = "Sheets::is_none")]
    pub remove_sheets: Sheets,
    /// The quad mesher of split faces (ho and lo-str).
    #[serde(default, skip_serializing_if = "PatchMethod::is_default")]
    pub patch_method: PatchMethod,
    /// Planar cuts that partition the prepared faces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<Cut>,
    /// lo-free: how the CAD faces are meshed.
    #[serde(default, skip_serializing_if = "PanelMethod::is_default")]
    pub panel_method: PanelMethod,
    /// Settings of single prepared faces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faces: Vec<FaceSetting>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            panel_size: Some(DEFAULT_PANEL_SIZE_M),
            patch_size: None,
            potential_panel_size: None,
            unit: "file".into(),
            cut_waterline: false,
            symmetry: Symmetry::None,
            interior: false,
            remove_sheets: Sheets::None,
            patch_method: PatchMethod::Tri3,
            cuts: Vec::new(),
            faces: Vec::new(),
            panel_method: PanelMethod::Recombined,
        }
    }
}

impl Settings {
    /// Set the sizes a mode uses from explicit values or defaults; reject sizes the mode does not use.
    pub fn set_sizes(&mut self, mode: Mode, panel: Option<f64>, patch: Option<f64>) -> Result<()> {
        ensure!(
            mode.uses_panel_size() || panel.is_none(),
            "Panel size is not used by {}",
            mode.name()
        );
        ensure!(
            mode.uses_patch_size() || patch.is_none(),
            "Patch size is not used by {}",
            mode.name()
        );
        self.panel_size = mode
            .uses_panel_size()
            .then(|| panel.unwrap_or(DEFAULT_PANEL_SIZE_M));
        self.patch_size = mode
            .uses_patch_size()
            .then(|| patch.unwrap_or(DEFAULT_PATCH_SIZE_M));
        Ok(())
    }

    /// Keep the current sizes when the GUI switches mode, filling defaults and dropping unused sizes.
    pub fn adapt_sizes(&mut self, mode: Mode) {
        self.panel_size = mode
            .uses_panel_size()
            .then(|| self.panel_size.unwrap_or(DEFAULT_PANEL_SIZE_M));
        self.patch_size = mode
            .uses_patch_size()
            .then(|| self.patch_size.unwrap_or(DEFAULT_PATCH_SIZE_M));
    }
}

/// A complete conversion request: the CAD source, the mode and its settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub schema: u32,
    pub source: PathBuf,
    pub mode: Mode,
    pub settings: Settings,
}

impl Spec {
    /// Check the request before a worker starts: the schema, an existing STEP or IGES source, the sizes and options
    /// its mode uses (and only those), the face settings and the cuts.
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "Unsupported conversion schema");
        ensure!(
            self.source.is_file(),
            "CAD file does not exist: {}",
            self.source.display()
        );
        let extension = self
            .source
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        ensure!(
            ["step", "stp", "iges", "igs"].contains(&extension.as_str()),
            "Input must be STEP or IGES"
        );
        for (name, size, required) in [
            (
                "Panel",
                self.settings.panel_size,
                self.mode.uses_panel_size(),
            ),
            (
                "Patch",
                self.settings.patch_size,
                self.mode.uses_patch_size(),
            ),
        ] {
            ensure!(
                size.is_some() == required,
                "{name} size {} for {}",
                if required {
                    "is required"
                } else {
                    "is not used"
                },
                self.mode.name()
            );
            ensure!(
                size.is_none_or(|s| s.is_finite() && s > 0.),
                "{name} size must be positive and finite"
            );
        }
        ensure!(
            self.mode == Mode::Ho || self.settings.potential_panel_size.is_none(),
            "Potential panel size is not used by {}",
            self.mode.name()
        );
        ensure!(
            self.settings
                .potential_panel_size
                .is_none_or(|s| s.is_finite() && s > 0.),
            "Potential panel size must be positive and finite"
        );
        ensure!(
            self.mode.uses_patch_size() || self.settings.patch_method == PatchMethod::Tri3,
            "Patch method is not used by {}",
            self.mode.name()
        );
        ensure!(
            !self.settings.interior || self.settings.cut_waterline,
            "--interior requires --cut-waterline"
        );
        ensure!(
            self.mode == Mode::LoFree || self.settings.panel_method == PanelMethod::Recombined,
            "Panel method is not used by {}",
            self.mode.name()
        );
        let mut seen = std::collections::BTreeSet::new();
        for setting in &self.settings.faces {
            ensure!(
                seen.insert(setting.face),
                "Face {} has two settings",
                setting.face
            );
            for size in [setting.patch_size, setting.panel_size]
                .into_iter()
                .flatten()
            {
                ensure!(
                    size.is_finite() && size > 0.,
                    "Face {}: sizes must be positive and finite",
                    setting.face
                );
            }
            ensure!(
                *setting == setting.for_mode(self.mode),
                "Face {}: {} uses {}",
                setting.face,
                self.mode.name(),
                if self.mode == Mode::LoFree {
                    "a panel size and a priority, not a layout or a patch size"
                } else {
                    "a layout, a patch size and a priority, not a panel size"
                }
            );
            ensure!(
                (-100..=100).contains(&setting.priority),
                "Face {}: the priority must be within -100 to 100",
                setting.face
            );
        }
        for cut in &self.settings.cuts {
            cut.validate()?;
        }
        Ok(())
    }
}

/// Export file format, chosen by the output extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Hbg,
    Gdf,
    /// ASCII STL triangles of low-order panels, written from the HBG artifacts (the whole body, its symmetry
    /// images included; the interior surface a second solid).
    Stl,
}

impl Format {
    /// The format of an output path by its extension (`.hbg`, `.gdf` or `.stl`).
    pub fn from_path(path: &Path) -> Result<Self> {
        match path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("hbg") => Ok(Self::Hbg),
            Some("gdf") => Ok(Self::Gdf),
            Some("stl") => Ok(Self::Stl),
            _ => bail!("Output must end in .hbg, .gdf or .stl"),
        }
    }
}

/// The surface an artifact holds: the body, or the optional interior surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Body,
    Interior,
}

/// One staged file in the private job directory, relative to that directory.
#[derive(Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub role: Role,
    pub format: Format,
    pub path: PathBuf,
}

/// Worker result: echoed request, engine checks, prepared-face ownership and staged artifacts.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConversionResult {
    pub schema: u32,
    pub request: Spec,
    pub mode: Mode,
    pub engine_version: String,
    pub state: String,
    pub summary: Value,
    pub body: Value,
    pub cad: Value,
    pub body_faces: Vec<i32>,
    #[serde(default)]
    pub patch_checks: Vec<Value>,
    /// ho: the checks of every interior patch, in the order they follow the body's elements in the published HBG.
    #[serde(default)]
    pub interior_patch_checks: Vec<Value>,
    pub artifacts: Vec<Artifact>,
    /// Why the quality checks failed (state `fail`): the result can be shown but is never published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Completed { result: Box<ConversionResult> },
    Failed { message: String },
}

/// One progress event appended by the worker to `progress.jsonl`.
#[derive(Debug, Deserialize, Serialize)]
pub struct Progress {
    pub stage: String,
    pub seconds: f64,
    pub data: Value,
}

/// A published output file and its role.
#[derive(Debug, Serialize)]
pub struct Delivered {
    pub role: Role,
    pub path: PathBuf,
}

/// A running or finished conversion worker. It owns the child process and the private directory;
/// keep it alive while reading or publishing its artifacts. Dropping it kills and reaps the child.
pub struct Job {
    child: Child,
    directory: tempfile::TempDir,
    progress_offset: u64,
    spec: Spec,
    worker: Worker,
    /// Why the quasi-structured mesher was given up for the default one (the worker is then running that).
    fallback: Option<String>,
    pub started: Instant,
}

/// Where a conversion worker runs: this executable (`--convert-worker`) unless another is named, with the
/// Gmsh SDK the worker should load in dynamic builds (otherwise `CAD2BEM_GMSH_LIBRARY` or the default search).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Worker {
    pub executable: Option<PathBuf>,
    pub gmsh_library: Option<PathBuf>,
}

impl Job {
    /// Start a conversion in a private worker process; it writes the request, progress, result and
    /// artifact files into a private directory owned by the job.
    pub fn start(spec: &Spec, worker: &Worker) -> Result<Self> {
        spec.validate()?;
        let mut spec = spec.clone();
        spec.source = spec.source.canonicalize()?;
        let (child, directory) = Self::spawn(&spec, worker)?;
        Ok(Self {
            child,
            directory,
            progress_offset: 0,
            spec,
            worker: worker.clone(),
            fallback: None,
            started: Instant::now(),
        })
    }

    /// A worker process converting `spec` in a new private directory.
    fn spawn(spec: &Spec, worker: &Worker) -> Result<(Child, tempfile::TempDir)> {
        let directory = tempfile::Builder::new()
            .prefix("cad2bem-convert-")
            .tempdir()?;
        let input = directory.path().join("request.json");
        let output = directory.path().join("result.json");
        fs::write(&input, serde_json::to_vec(&spec)?)?;
        let executable = match &worker.executable {
            Some(path) => path.clone(),
            None => std::env::current_exe()?,
        };
        let mut command = Command::new(&executable);
        command
            .arg("--convert-worker")
            .arg(&input)
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(directory.path().join("stderr.txt"))?);
        if let Some(library) = &worker.gmsh_library {
            command.env("CAD2BEM_GMSH_LIBRARY", library);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let child = command
            .spawn()
            .with_context(|| format!("Cannot launch the conversion worker {executable:?}"))?;
        Ok((child, directory))
    }

    /// When a lo-free conversion with the quasi-structured mesher failed, convert again with the default mesher
    /// (mostly quadrilaterals): Gmsh's quasi-structured pipeline can fail, or crash the worker, in its topology
    /// optimisation of a face, and it runs over the whole model, so no single face can be given another mesher.
    /// Whether the new worker started.
    fn fall_back(&mut self, error: &anyhow::Error) -> Result<bool> {
        if self.fallback.is_some()
            || self.spec.mode != Mode::LoFree
            || self.spec.settings.panel_method != PanelMethod::Qqs
        {
            return Ok(false);
        }
        let reason: String = format!("{error:#}")
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        let mut spec = self.spec.clone();
        spec.settings.panel_method = PanelMethod::Recombined;
        let (child, directory) = Self::spawn(&spec, &self.worker)?;
        self.child = child;
        self.directory = directory;
        self.progress_offset = 0;
        self.spec = spec;
        self.fallback = Some(reason);
        Ok(true)
    }

    /// Poll a private conversion. File decoding is synchronous: GUI callers must use a background dispatcher.
    pub fn poll(&mut self) -> Option<Result<ConversionResult>> {
        let outcome = self.finished()?;
        match outcome {
            Err(error) => match self.fall_back(&error) {
                Ok(true) => None,
                Ok(false) => Some(Err(error)),
                Err(again) => Some(Err(again.context(format!("{error:#}")))),
            },
            Ok(mut result) => {
                if let Some(reason) = &self.fallback {
                    // The settings the user asked for, and what was used and why.
                    result.summary["panel_method_fallback"] = serde_json::json!({
                        "requested": PanelMethod::Qqs.name(),
                        "used": PanelMethod::Recombined.name(),
                        "reason": reason,
                    });
                }
                Some(Ok(result))
            }
        }
    }

    /// The outcome of the worker, once it has exited.
    fn finished(&mut self) -> Option<Result<ConversionResult>> {
        match self.child.try_wait() {
            Ok(None) => None,
            Err(error) => Some(Err(error.into())),
            Ok(Some(status)) => Some((|| -> Result<ConversionResult> {
                let stderr = || {
                    fs::read_to_string(self.directory.path().join("stderr.txt"))
                        .unwrap_or_default()
                        .chars()
                        .take(8000)
                        .collect::<String>()
                };
                ensure!(
                    status.success(),
                    "Conversion worker exited with {status}: {}",
                    stderr()
                );
                let path = self.directory.path().join("result.json");
                ensure!(
                    path.is_file(),
                    "Conversion worker produced no result: {}",
                    stderr()
                );
                ensure!(
                    fs::metadata(&path)?.len() <= 128 * 1024 * 1024,
                    "Conversion metadata exceeds 128 MiB"
                );
                match serde_json::from_reader(std::io::BufReader::new(File::open(path)?))? {
                    Response::Failed { message } => bail!(message),
                    Response::Completed { result } => {
                        self.validate_result(&result)?;
                        Ok(*result)
                    }
                }
            })()),
        }
    }

    /// The progress events the worker has written since the last call (complete lines only).
    pub fn progress(&mut self) -> Result<Vec<Progress>> {
        let path = self.directory.path().join("progress.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        ensure!(
            fs::metadata(&path)?.len() <= 1024 * 1024,
            "Worker progress exceeded 1 MiB"
        );
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(self.progress_offset))?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let mut events = Vec::new();
        for line in text.split_inclusive('\n') {
            if !line.ends_with('\n') {
                break;
            }
            self.progress_offset += line.len() as u64;
            events.push(serde_json::from_str(line)?);
        }
        Ok(events)
    }

    fn validate_result(&self, result: &ConversionResult) -> Result<()> {
        ensure!(
            result.request == self.spec,
            "Worker returned a result for different conversion settings"
        );
        ensure!(
            result.schema == SCHEMA && result.mode == self.spec.mode,
            "Worker result schema/mode mismatch"
        );
        ensure!(
            matches!(result.state.as_str(), "ok" | "warn")
                || (result.state == "fail" && result.failure.is_some()),
            "Worker returned failed conversion checks without the reason"
        );
        ensure!(
            result.summary["state"].as_str() == Some(&result.state),
            "Worker summary state mismatch"
        );
        ensure!(
            result.summary[self.spec.mode.elements()].as_u64()
                == Some(result.body_faces.len() as u64)
                && !result.body_faces.is_empty(),
            "Panel ownership/count mismatch"
        );
        ensure!(
            result.body_faces.iter().all(|&tag| tag > 0),
            "Invalid prepared CAD face tags"
        );
        let root = self.directory.path().canonicalize()?;
        let mut keys = HashSet::new();
        for artifact in &result.artifacts {
            ensure!(
                !artifact.path.is_absolute()
                    && artifact
                        .path
                        .components()
                        .all(|c| matches!(c, Component::Normal(_))),
                "Artifact path escapes the private job"
            );
            let path = self.directory.path().join(&artifact.path).canonicalize()?;
            ensure!(
                path.starts_with(&root) && path.is_file() && fs::metadata(&path)?.len() > 0,
                "Invalid staged artifact"
            );
            ensure!(
                Format::from_path(&path)? == artifact.format,
                "Artifact format/extension mismatch"
            );
            ensure!(
                keys.insert((artifact.role, artifact.format)),
                "Duplicate conversion artifact"
            );
        }
        let roles = if self.spec.settings.interior {
            vec![Role::Body, Role::Interior]
        } else {
            vec![Role::Body]
        };
        let formats = if self.spec.mode == Mode::Ho {
            vec![Format::Hbg]
        } else {
            vec![Format::Hbg, Format::Gdf]
        };
        ensure!(
            keys.len() == roles.len() * formats.len()
                && roles
                    .iter()
                    .all(|&role| formats.iter().all(|&format| keys.contains(&(role, format)))),
            "Incomplete conversion artifacts"
        );
        Ok(())
    }

    /// Read a validated private artifact while this job retains ownership of its directory.
    pub fn read_artifact(
        &self,
        result: &ConversionResult,
        role: Role,
        format: Format,
    ) -> Result<Vec<u8>> {
        self.validate_result(result)?;
        let artifact = result
            .artifacts
            .iter()
            .find(|a| a.role == role && a.format == format)
            .context("Missing conversion artifact")?;
        let path = self.directory.path().join(&artifact.path);
        ensure!(
            fs::metadata(&path)?.len() <= 512 * 1024 * 1024,
            "Artifact exceeds display limit"
        );
        Ok(fs::read(path)?)
    }

    /// Publish prevalidated bytes without clobbering existing files; output paths never reach the worker.
    pub fn publish(&self, result: &ConversionResult, output: &Path) -> Result<Vec<Delivered>> {
        self.publish_with(
            result,
            output,
            result.request.settings.potential_panel_size,
            false,
        )
    }

    /// [`Job::publish`], with the potential panel size the HBG files of an ho result carry (`panel_size`) set to
    /// `panel_size` ([`DEFAULT_POTENTIAL_PANEL_SIZE_M`] for none): it changes only that member, so it can change
    /// after the conversion. `replace`: existing files are replaced (the GUI, whose save dialog asked), else
    /// refused (the command line).
    pub fn publish_with(
        &self,
        result: &ConversionResult,
        output: &Path,
        panel_size: Option<f64>,
        replace: bool,
    ) -> Result<Vec<Delivered>> {
        self.spec.mode.check_output(output)?;
        ensure!(
            panel_size.is_none() || result.mode == Mode::Ho,
            "Potential panel size is not used by {}",
            result.mode.name()
        );
        ensure!(
            panel_size.is_none_or(|s| s.is_finite() && s > 0.),
            "Potential panel size must be positive and finite"
        );
        // None is the default size, which the worker already wrote.
        let size = panel_size.unwrap_or(DEFAULT_POTENTIAL_PANEL_SIZE_M);
        let rewrite = result.mode == Mode::Ho
            && size
                != result
                    .request
                    .settings
                    .potential_panel_size
                    .unwrap_or(DEFAULT_POTENTIAL_PANEL_SIZE_M);
        self.validate_result(result)?;
        if let Some(failure) = &result.failure {
            bail!("{failure}");
        }
        let (format, target) = output_targets(output, replace)?;
        let artifact = |role: Role, format: Format| -> Result<Vec<u8>> {
            let artifact = result
                .artifacts
                .iter()
                .find(|a| a.role == role && a.format == format)
                .context("Missing output artifact")?;
            Ok(fs::read(self.directory.path().join(&artifact.path))?)
        };
        // One file: the body, and the interior surface after it (its elements lie in z = 0, where the solver's
        // interior surface "detect" finds them).
        let interior = self.spec.settings.interior;
        let mut bytes = if format == Format::Stl {
            let interior = interior
                .then(|| artifact(Role::Interior, Format::Hbg))
                .transpose()?;
            stl(&artifact(Role::Body, Format::Hbg)?, interior.as_deref())?
        } else {
            artifact(Role::Body, format)?
        };
        if interior && format != Format::Stl {
            bytes = merged(format, &bytes, &artifact(Role::Interior, format)?)?;
        }
        if rewrite && format == Format::Hbg {
            let mut document: Value = serde_json::from_slice(&bytes)?;
            let object = document
                .as_object_mut()
                .context("The HBG artifact is not an object")?;
            object.insert("panel_size".into(), size.into());
            bytes = serde_json::to_vec(&document)?;
        }
        let parent = target.path.parent().context("Output has no parent")?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.as_file_mut().write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        let persisted = if replace {
            temporary.persist(&target.path)
        } else {
            temporary.persist_noclobber(&target.path)
        };
        if let Err(error) = persisted {
            bail!("Cannot publish {}: {}", target.path.display(), error.error);
        }
        Ok(vec![target])
    }

    /// Stop the worker process if it still runs, and reap it.
    pub fn cancel(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Validate all requested destinations before doing any conversion or publishing any file; an existing one is
/// refused unless `replace`. One file holds the body and, when asked, the interior surface.
pub fn output_targets(output: &Path, replace: bool) -> Result<(Format, Delivered)> {
    let format = Format::from_path(output)?;
    let target = Delivered {
        role: Role::Body,
        path: std::path::absolute(output)?,
    };
    ensure!(
        replace || !target.path.exists(),
        "Output already exists: {} (choose a new path)",
        target.path.display()
    );
    Ok((format, target))
}

/// An ASCII STL file of the HBG panel files `body` and `interior`: the whole body (the symmetry images of the
/// stored part made too), and the interior surface as a second solid.
pub fn stl(body: &[u8], interior: Option<&[u8]>) -> Result<Vec<u8>> {
    let read = |bytes: &[u8]| -> Result<crate::engine::panels::Panels> {
        let document: Value = serde_json::from_slice(bytes)?;
        ensure!(
            document["kind"] == "panels",
            "STL takes panels (not spline patches)"
        );
        let vertices: Vec<crate::engine::panels::Panel> =
            serde_json::from_value(document["vertices"].clone()).context("HBG panel vertices")?;
        let symmetry = document["symmetry"]
            .as_str()
            .and_then(Symmetry::from_name)
            .unwrap_or_default();
        Ok(crate::engine::panels::Panels {
            faces: vec![1; vertices.len()],
            vertices,
            symmetry,
            label: document["label"].as_str().unwrap_or_default().to_owned(),
        })
    };
    let body = read(body)?;
    let interior = interior.map(read).transpose()?;
    Ok(body.to_stl(interior.as_ref())?.into_bytes())
}

/// The body file `body` with the elements of the interior surface file `interior` after its own, in one file of
/// `format`: HBG panels or spline patches (the interior's `vertices` or `patches` appended), or GDF panels (its
/// panels appended, the count of the header updated).
pub fn merged(format: Format, body: &[u8], interior: &[u8]) -> Result<Vec<u8>> {
    match format {
        Format::Hbg => {
            let mut body: Value = serde_json::from_slice(body)?;
            let interior: Value = serde_json::from_slice(interior)?;
            ensure!(
                body["kind"] == interior["kind"],
                "The interior surface is of another kind than the body"
            );
            let key = if body["kind"] == "spline_patches" {
                "patches"
            } else {
                "vertices"
            };
            let more = interior[key]
                .as_array()
                .context("The interior surface has no elements")?
                .clone();
            body[key]
                .as_array_mut()
                .context("The body has no elements")?
                .extend(more);
            Ok(serde_json::to_vec(&body)?)
        }
        Format::Gdf => {
            // Title, ULEN GRAV, ISX ISY, NPAN, then the panels' vertices (the writer's layout).
            let lines = |bytes: &[u8]| -> Result<Vec<String>> {
                let text = std::str::from_utf8(bytes)?;
                let lines: Vec<String> = text.lines().map(str::to_owned).collect();
                ensure!(lines.len() >= 4, "A GDF file without its header");
                Ok(lines)
            };
            let (body, interior) = (lines(body)?, lines(interior)?);
            let count = |lines: &[String]| -> Result<usize> {
                lines[3].trim().parse().context("A GDF panel count")
            };
            let mut out = body[..3].to_vec();
            out.push((count(&body)? + count(&interior)?).to_string());
            out.extend(body[4..].iter().cloned());
            out.extend(interior[4..].iter().cloned());
            let mut text = out.join("\n");
            text.push('\n');
            Ok(text.into_bytes())
        }
        Format::Stl => bail!("STL is written from the HBG panels (stl), not merged"),
    }
}

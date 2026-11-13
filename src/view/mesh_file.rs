// SPDX-License-Identifier: GPL-3.0-or-later
//! Opening BEM geometry files to view them: a geometry file (`.hbg`, version 3: panels, flat or spline patches)
//! or a GDF file (`.gdf`: low-order panels, or higher-order flat or B-spline patches with the discretization of
//! the SPL file beside it), or an STL file (`.stl`, ASCII or binary triangles), its elements in z = 0 as the
//! interior surface, and what
//! they hold: counts, symmetry, discretization, volumes and, for spline patches, the checks of every patch.

use super::{
    conversion::{Geometry, Panels, Surface, mesh_view},
    spline,
    viewport::MeshView,
};
use crate::{
    conversion::{DEFAULT_POTENTIAL_PANEL_SIZE_M, HBG_VERSION, Symmetry},
    engine::{bspline, checks, ho},
};
use anyhow::{Context, Result, bail, ensure};
use glam::DVec3;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The extensions of the geometry files the viewer opens (an `.spl` file opens the higher-order GDF beside it).
pub const EXTENSIONS: [&str; 4] = ["hbg", "gdf", "spl", "stl"];

/// Whether `path` names a geometry file the viewer opens (by its extension).
pub fn is_mesh_file(path: &Path) -> bool {
    path.extension()
        .and_then(|x| x.to_str())
        .is_some_and(|x| EXTENSIONS.contains(&x.to_ascii_lowercase().as_str()))
}

/// The geometry of one file.
pub struct MeshFile {
    pub path: PathBuf,
    /// What the file is: "HBG panels", "HBG flat patches", "HBG spline patches" or "GDF panels".
    pub format: &'static str,
    pub geometry: Geometry,
    pub symmetry: Symmetry,
    /// The potential panel size of an HBG file (`panel_size`, m).
    pub panel_size: Option<f64>,
    /// The discretization rows (nu, nv, ku, kv, outer, inner) of an HBG file: one per patch.
    pub discretization: Option<Vec<[usize; 6]>>,
}

impl MeshFile {
    /// Read a geometry file by its extension: `.hbg` (version 3), `.gdf` (low or higher order; a higher-order file
    /// takes its discretization from the `.spl` beside it), `.spl` (the `.gdf` beside it) or `.stl` (triangles).
    pub fn read(path: &Path) -> Result<Self> {
        if path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("spl"))
        {
            let gdf = beside(path, "gdf").with_context(|| {
                format!(
                    "{}: an SPL file holds only the discretization; its GDF file (the same name, .gdf) is missing",
                    path.display()
                )
            })?;
            return Self::read(&gdf);
        }
        let bytes =
            std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
        let extension = path
            .extension()
            .and_then(|x| x.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("hbg") => read_hbg(path, &bytes),
            Some("gdf") => read_gdf(path, &bytes),
            Some("stl") => read_stl(path, &bytes),
            _ => bail!("{} is not an .hbg, .gdf, .spl or .stl file", path.display()),
        }
        .with_context(|| path.display().to_string())
    }

    fn count(&self) -> usize {
        match &self.geometry {
            Geometry::Panels(p) => p.vertices.len(),
            Geometry::Splines(d) => d.patches.len(),
        }
    }
}

fn read_hbg(path: &Path, bytes: &[u8]) -> Result<MeshFile> {
    let document: Value =
        serde_json::from_slice(bytes).context("This is not a JSON geometry file")?;
    ensure!(
        document["format"] == "hbg",
        "The member \"format\" must be \"hbg\""
    );
    match document["version"].as_u64() {
        Some(v) if v == u64::from(HBG_VERSION) => {}
        Some(v) => bail!("This file is HBG version {v}; cad2bem reads version {HBG_VERSION}"),
        None => bail!("The member \"version\" must be a number"),
    }
    let symmetry = match document.get("symmetry") {
        None => Symmetry::None,
        Some(Value::String(name)) => Symmetry::from_name(name)
            .with_context(|| format!("Unknown symmetry \"{name}\" (none, yz, xz or xz_and_yz)"))?,
        Some(_) => bail!("The member \"symmetry\" must be a string"),
    };
    let panel_size = match document.get("panel_size") {
        None => None,
        Some(v) => Some(
            v.as_f64()
                .filter(|s| s.is_finite() && *s > 0.)
                .context("panel_size must be a positive number")?,
        ),
    };
    let kind = document["kind"].as_str().unwrap_or_default();
    let (format, geometry) = match kind {
        "panels" | "flat_patches" => {
            let vertices: Vec<[[f64; 3]; 4]> = serde_json::from_value(
                document
                    .get("vertices")
                    .cloned()
                    .context("No \"vertices\"")?,
            )
            .context("\"vertices\" must be P x 4 x 3 numbers")?;
            ensure!(
                !vertices.is_empty() && vertices.iter().flatten().flatten().all(|v| v.is_finite()),
                "No panels, or a coordinate that is not finite"
            );
            let format = if kind == "panels" {
                "HBG panels"
            } else {
                "HBG flat patches"
            };
            (format, Geometry::Panels(Panels::from_vertices(vertices)))
        }
        "spline_patches" => (
            "HBG spline patches",
            Geometry::Splines(spline::Document::read(bytes)?),
        ),
        "analytic_patches" => bail!("Files of analytic patches cannot be shown yet"),
        "trimmed_patches" => bail!("A trimmed mesh is made by the solver, not read"),
        other => bail!(
            "Unknown kind \"{other}\" (panels, flat_patches, spline_patches, analytic_patches)"
        ),
    };
    let count = match &geometry {
        Geometry::Panels(p) => p.vertices.len(),
        Geometry::Splines(d) => d.patches.len(),
    };
    let discretization = discretization(&document, count)?;
    ensure!(
        discretization.is_none()
            || matches!(geometry, Geometry::Splines(_))
            || kind == "flat_patches",
        "A panels mesh has no discretization"
    );
    Ok(MeshFile {
        path: path.to_path_buf(),
        format,
        geometry,
        symmetry,
        panel_size,
        discretization,
    })
}

/// The discretization rows of a geometry file: the member `discretization` (one row for every patch, or a row
/// per patch), or a `discretization` in every patch.
fn discretization(document: &Value, count: usize) -> Result<Option<Vec<[usize; 6]>>> {
    let row = |v: &Value| -> Result<[usize; 6]> {
        serde_json::from_value(v.clone()).context("A discretization row is six integers")
    };
    if let Some(rows) = document.get("discretization") {
        let first = rows.get(0).context("An empty discretization")?;
        let rows: Vec<[usize; 6]> = if first.is_array() {
            rows.as_array()
                .unwrap()
                .iter()
                .map(row)
                .collect::<Result<_>>()?
        } else {
            vec![row(rows)?; count]
        };
        ensure!(
            rows.len() == count,
            "{} discretization rows for {count} patches",
            rows.len()
        );
        return Ok(Some(rows));
    }
    let patches = document["patches"].as_array();
    let own: Vec<&Value> = patches
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("discretization"))
        .collect();
    if own.is_empty() {
        return Ok(None);
    }
    ensure!(
        own.len() == count,
        "A discretization for every patch or for none"
    );
    Ok(Some(own.into_iter().map(row).collect::<Result<_>>()?))
}

/// A low-order GDF file: a title, ULEN and GRAV, ISX and ISY, NPAN, then four vertices (x, y, z) per panel.
fn read_gdf(path: &Path, bytes: &[u8]) -> Result<MeshFile> {
    let text = std::str::from_utf8(bytes).context("Not a text file")?;
    let mut lines = text.lines();
    lines.next().context("No title line")?;
    // The numbers that start a header line; words after them are comments (`1.0 9.80665  ULEN GRAV`).
    let numbers = |line: Option<&str>, what: &str| -> Result<Vec<f64>> {
        let numbers: Vec<f64> = line
            .with_context(|| format!("No {what} line"))?
            .split_whitespace()
            .map_while(|t| number(t).ok())
            .collect();
        ensure!(!numbers.is_empty(), "The {what} line starts with no number");
        Ok(numbers)
    };
    let lengths = numbers(lines.next(), "ULEN GRAV")?;
    ensure!(lengths.len() >= 2, "The second line holds ULEN and GRAV");
    let flags = numbers(lines.next(), "ISX ISY")?;
    ensure!(flags.len() >= 2, "The third line holds ISX and ISY");
    let count = numbers(lines.next(), "NPAN")?;
    let symmetry = match (flags[0] != 0., flags[1] != 0.) {
        (false, false) => Symmetry::None,
        (true, false) => Symmetry::Yz,
        (false, true) => Symmetry::Xz,
        (true, true) => Symmetry::XzAndYz,
    };
    if count.len() >= 2 {
        ensure!(
            count[1] >= 0.,
            "Analytic shapes of a higher-order GDF file (IGDEF {}) cannot be shown",
            count[1]
        );
        let values: Vec<f64> = lines
            .flat_map(str::split_whitespace)
            .map(number)
            .collect::<Result<_>>()?;
        return read_gdf_patches(path, count, &values, symmetry);
    }
    ensure!(
        count.len() == 1,
        "The fourth line holds NPAN (or NPATCH and IGDEF)"
    );
    let npan = count[0];
    ensure!(
        npan >= 1. && npan.fract() == 0.,
        "NPAN must be a positive integer"
    );
    let npan = npan as usize;
    let values: Vec<f64> = lines
        .flat_map(str::split_whitespace)
        .map(number)
        .collect::<Result<_>>()?;
    ensure!(
        values.len() >= 12 * npan,
        "{npan} panels need {} coordinates, the file has {}",
        12 * npan,
        values.len()
    );
    let vertices: Vec<[[f64; 3]; 4]> = values[..12 * npan]
        .chunks(12)
        .map(|c| std::array::from_fn(|k| [c[3 * k], c[3 * k + 1], c[3 * k + 2]]))
        .collect();
    Ok(MeshFile {
        path: path.to_path_buf(),
        format: "GDF panels",
        geometry: Geometry::Panels(Panels::from_vertices(vertices)),
        symmetry,
        panel_size: None,
        discretization: None,
    })
}

/// The file beside `path` with the same name and the extension `extension` (either case), if there is one.
fn beside(path: &Path, extension: &str) -> Option<PathBuf> {
    [extension.to_owned(), extension.to_ascii_uppercase()]
        .into_iter()
        .map(|x| path.with_extension(x))
        .find(|p| p.is_file())
}

/// A higher-order GDF file after its fourth line (`NPATCH IGDEF`): flat patches of four vertices (IGDEF 0) or
/// B-spline patches (IGDEF 1: spans nu nv, orders ku kv, the knot vectors of nu + 2 ku - 1 and nv + 2 kv - 1
/// values, then the control points, u fastest). Analytic shapes (IGDEF < 0) are not shown. The discretization
/// comes from the SPL file beside it.
fn read_gdf_patches(
    path: &Path,
    head: Vec<f64>,
    values: &[f64],
    symmetry: Symmetry,
) -> Result<MeshFile> {
    let (npatch, igdef) = (head[0], head[1]);
    ensure!(
        npatch >= 1. && npatch.fract() == 0.,
        "NPATCH must be a positive integer"
    );
    let npatch = npatch as usize;
    ensure!(
        igdef >= 0.,
        "Analytic shapes of a higher-order GDF file (IGDEF {igdef}) cannot be shown"
    );
    let (format, geometry) = match igdef as i64 {
        0 => {
            ensure!(
                values.len() >= 12 * npatch,
                "{npatch} flat patches need {} coordinates, the file has {}",
                12 * npatch,
                values.len()
            );
            let vertices = values[..12 * npatch]
                .chunks(12)
                .map(|c| std::array::from_fn(|k| [c[3 * k], c[3 * k + 1], c[3 * k + 2]]))
                .collect();
            (
                "GDF flat patches",
                Geometry::Panels(Panels::from_vertices(vertices)),
            )
        }
        1 => {
            let mut at = 0;
            let mut take = |n: usize, what: &str| -> Result<&[f64]> {
                let slice = values
                    .get(at..at + n)
                    .with_context(|| format!("The file ends in the {what} of a patch"))?;
                at += n;
                Ok(slice)
            };
            let integer = |x: f64, what: &str| -> Result<usize> {
                ensure!(
                    x >= 1. && x.fract() == 0.,
                    "{what} must be a positive integer"
                );
                Ok(x as usize)
            };
            let mut patches = Vec::with_capacity(npatch);
            for k in 1..=npatch {
                let head = take(4, "spans and orders")?;
                let (nu, nv) = (integer(head[0], "nu")?, integer(head[1], "nv")?);
                let (ku, kv) = (integer(head[2], "ku")?, integer(head[3], "kv")?);
                let u_knots = take(nu + 2 * ku - 1, "u knots")?.to_vec();
                let v_knots = take(nv + 2 * kv - 1, "v knots")?.to_vec();
                let (mu, mv) = (nu + ku - 1, nv + kv - 1);
                let points = take(3 * mu * mv, "control points")?;
                let coefficients = (0..mv)
                    .map(|j| {
                        (0..mu)
                            .map(|i| {
                                let c = &points[3 * (j * mu + i)..];
                                [c[0], c[1], c[2]]
                            })
                            .collect()
                    })
                    .collect();
                patches.push(spline::Patch {
                    name: format!("patch {k}"),
                    u_order: ku,
                    v_order: kv,
                    u_knots,
                    v_knots,
                    coefficients,
                    weights: None,
                });
            }
            (
                "GDF spline patches",
                Geometry::Splines(spline::Document::from_patches(patches)?),
            )
        }
        other => bail!("IGDEF {other} is not a geometry the viewer knows (0 flat, 1 B-spline)"),
    };
    let discretization = match beside(path, "spl") {
        Some(spl) => Some(read_spl(&spl, npatch).with_context(|| spl.display().to_string())?),
        None => None,
    };
    Ok(MeshFile {
        path: path.to_path_buf(),
        format,
        geometry,
        symmetry,
        panel_size: None,
        discretization,
    })
}

/// The discretization rows (nu, nv, ku, kv, outer, inner) of an SPL file: a header line, then per patch the spans
/// and, when the file gives them, the orders and the outer and inner Gauss orders, a pair per line. What a file
/// leaves out takes the solver's defaults (orders 3, Gauss orders 3 and 4).
fn read_spl(path: &Path, patches: usize) -> Result<Vec<[usize; 6]>> {
    let text = std::fs::read_to_string(path).context("Cannot read the SPL file")?;
    let lines: Vec<&str> = text
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .collect();
    ensure!(
        !lines.is_empty() && lines.len().is_multiple_of(patches) && lines.len() / patches <= 4,
        "{} lines for {patches} patches: an SPL file has one to four lines per patch after its header",
        lines.len()
    );
    let per = lines.len() / patches;
    let pair = |line: &str| -> Result<[usize; 2]> {
        let numbers: Vec<usize> = line
            .split_whitespace()
            .take(2)
            .map(|t| {
                t.parse()
                    .with_context(|| format!("\"{t}\" is not a whole number"))
            })
            .collect::<Result<_>>()?;
        ensure!(
            numbers.len() == 2,
            "A line of an SPL file holds a pair of numbers"
        );
        Ok([numbers[0], numbers[1]])
    };
    lines
        .chunks(per)
        .map(|group| -> Result<[usize; 6]> {
            let mut row = [0, 0, 3, 3, 3, 4];
            for (k, line) in group.iter().enumerate() {
                let [a, b] = pair(line)?;
                match k {
                    0 | 1 => [row[2 * k], row[2 * k + 1]] = [a, b],
                    2 => row[4] = a,
                    _ => row[5] = a,
                }
            }
            ensure!(row[0] >= 1 && row[1] >= 1, "Spans must be at least one");
            Ok(row)
        })
        .collect()
}

/// An STL file, binary (an 80-byte header, the triangle count, then 50 bytes a triangle) or ASCII (`solid`, then
/// `facet`s of three `vertex` lines): its triangles as panels, each in its own vertex order (STL turns it out of
/// the solid, into the fluid, as panels are). The facet normals are not used; triangles of no area are left out.
/// STL has no symmetry: the file is taken as the whole surface.
fn read_stl(path: &Path, bytes: &[u8]) -> Result<MeshFile> {
    let binary = bytes.len() >= 84 && {
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
        count.checked_mul(50).and_then(|n| n.checked_add(84)) == Some(bytes.len())
    };
    let mut triangles: Vec<[[f64; 3]; 3]> = Vec::new();
    if binary {
        for record in bytes[84..].as_chunks::<50>().0 {
            let value = |k: usize| {
                f64::from(f32::from_le_bytes(
                    record[4 * k..4 * k + 4].try_into().unwrap(),
                ))
            };
            // Bytes 0..12 hold the normal, then three vertices.
            triangles.push(std::array::from_fn(|v| {
                std::array::from_fn(|c| value(3 + 3 * v + c))
            }));
        }
    } else {
        let text = std::str::from_utf8(bytes).context(
            "Neither a binary STL file (its size does not match its triangle count) nor a text one",
        )?;
        ensure!(
            text.trim_start().starts_with("solid"),
            "An ASCII STL file starts with \"solid\""
        );
        let mut tokens = text.split_whitespace();
        let mut corners: Vec<[f64; 3]> = Vec::new();
        while let Some(token) = tokens.next() {
            if token == "vertex" {
                let mut xyz = [0.; 3];
                for x in &mut xyz {
                    *x = number(tokens.next().context("A vertex without its coordinates")?)?;
                }
                corners.push(xyz);
            }
        }
        ensure!(
            corners.len().is_multiple_of(3),
            "{} vertices: not three a facet",
            corners.len()
        );
        triangles = corners.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
    }
    ensure!(
        triangles.iter().flatten().flatten().all(|x| x.is_finite()),
        "A coordinate that is not finite"
    );
    let vertices: Vec<[[f64; 3]; 4]> = triangles
        .into_iter()
        .filter(|[a, b, c]| {
            let [a, b, c] = [a, b, c].map(|p| DVec3::from_array(*p));
            (b - a).cross(c - a).length() > 0.
        })
        .map(|[a, b, c]| [a, b, c, c])
        .collect();
    ensure!(!vertices.is_empty(), "No triangles");
    Ok(MeshFile {
        path: path.to_path_buf(),
        format: if binary {
            "STL triangles (binary)"
        } else {
            "STL triangles"
        },
        geometry: Geometry::Panels(Panels::from_vertices(vertices)),
        symmetry: Symmetry::None,
        panel_size: None,
        discretization: None,
    })
}

/// A number of a GDF file (Fortran exponents `D` taken as `E`).
fn number(token: &str) -> Result<f64> {
    let x: f64 = token
        .replace(['D', 'd'], "E")
        .parse()
        .with_context(|| format!("\"{token}\" is not a number"))?;
    ensure!(x.is_finite(), "\"{token}\" is not finite");
    Ok(x)
}

/// The engine's patch of a displayed spline patch (control points u fastest).
fn engine_patch(patch: &spline::Patch) -> bspline::Patch {
    bspline::Patch {
        u_order: patch.u_order,
        v_order: patch.v_order,
        u_knots: patch.u_knots.clone(),
        v_knots: patch.v_knots.clone(),
        coefficients: patch
            .coefficients
            .iter()
            .flatten()
            .map(|&c| DVec3::from_array(c))
            .collect(),
        weights: patch
            .weights
            .as_ref()
            .map(|w| w.iter().flatten().copied().collect()),
    }
}

/// The three divergence-theorem volumes of panels (normals into the fluid) with their symmetry images.
fn panel_volumes(vertices: &[[[f64; 3]; 4]], symmetry: Symmetry) -> [f64; 3] {
    let mut volume = DVec3::ZERO;
    for panel in vertices {
        let p = panel.map(DVec3::from_array);
        for [a, b, c] in [[0, 1, 2], [0, 2, 3]] {
            let area = (p[b] - p[a]).cross(p[c] - p[a]) * 0.5;
            volume += (p[a] + p[b] + p[c]) / 3. * area;
        }
    }
    (volume * symmetry.images()).to_array()
}

/// What a geometry file holds.
#[derive(Clone, Debug)]
pub struct Summary {
    /// The file name.
    pub name: String,
    /// What the file is ([`MeshFile::format`]).
    pub format: &'static str,
    /// Spline patches (else panels).
    pub patches: bool,
    /// Patches, or panels.
    pub count: usize,
    /// Panels given as triangles (a repeated vertex).
    pub triangles: usize,
    pub symmetry: Symmetry,
    /// Spline patches: the discretization rows of the file.
    pub rows: bool,
    /// Spline patches: the `panel_size` of the file (m).
    pub panel_size: Option<f64>,
    /// A body: the three divergence-theorem volumes (m³), with the symmetry copies.
    pub volumes: Option<[f64; 3]>,
    /// An interior surface: its area (m²), with the symmetry copies.
    pub area: Option<f64>,
    /// Spline patches: how many pass, warn and fail the checks (fold, Jacobian, corner angle).
    pub checks: Option<[usize; 3]>,
}

impl Summary {
    /// The mean of the three volume estimates.
    pub fn volume(&self) -> Option<f64> {
        self.volumes.map(|v| v.iter().sum::<f64>() / 3.)
    }

    /// The summary in a line, for a log.
    pub fn line(&self) -> String {
        let mut line = format!(
            "{}: {}, {} {}, symmetry {}",
            self.name,
            self.format,
            self.count,
            if self.patches { "patches" } else { "panels" },
            self.symmetry.name()
        );
        if let Some(v) = self.volume() {
            line.push_str(&format!(", volume {v:.6} m³"));
        }
        if let Some(a) = self.area {
            line.push_str(&format!(", area {a:.6} m²"));
        }
        if let Some([ok, warn, fail]) = self.checks {
            line.push_str(&format!(", patches {ok} ok {warn} warn {fail} fail"));
        }
        line
    }
}

/// A geometry file opened in the viewer: its views (the body, and the body with the interior surface found in
/// it) and what the file holds.
pub struct Opened {
    pub path: PathBuf,
    /// Spline patches (else panels).
    pub patches: bool,
    pub body: MeshView,
    pub with_interior: Option<MeshView>,
    /// What the body holds; `interior`: what the interior surface holds, when the file has one.
    pub summary: Summary,
    pub interior: Option<Summary>,
}

/// The body and the interior surface of a file: its elements lying wholly in z = 0 are the interior surface (as
/// the solver's interior surface "detect" takes them, of either orientation), the others the body. No interior when no
/// element lies there, or when every one does.
fn split_interior(file: MeshFile) -> (MeshFile, Option<MeshFile>) {
    let size = |points: &mut dyn Iterator<Item = [f64; 3]>| {
        points.flat_map(|p| p.map(f64::abs)).fold(1., f64::max)
    };
    let pick: Vec<bool> = match &file.geometry {
        Geometry::Panels(panels) => {
            let tol = 1e-9 * size(&mut panels.vertices.iter().flatten().copied());
            panels
                .vertices
                .iter()
                .map(|p| p.iter().all(|v| v[2].abs() <= tol))
                .collect()
        }
        Geometry::Splines(document) => {
            let tol = 1e-9
                * size(
                    &mut document
                        .patches
                        .iter()
                        .flat_map(|p| p.coefficients.iter().flatten().copied()),
                );
            document
                .patches
                .iter()
                .map(|patch| {
                    // A patch lies in its control points' hull: all of them in z = 0.
                    patch
                        .coefficients
                        .iter()
                        .flatten()
                        .all(|c| c[2].abs() <= tol)
                })
                .collect()
        }
    };
    let found = pick.iter().filter(|&&p| p).count();
    if found == 0 || found == pick.len() {
        return (file, None);
    }
    let not: Vec<bool> = pick.iter().map(|p| !p).collect();
    let rows = |keep: &[bool]| {
        file.discretization.as_ref().map(|rows| {
            rows.iter()
                .zip(keep)
                .filter(|(_, k)| **k)
                .map(|(r, _)| *r)
                .collect()
        })
    };
    let part = |keep: &[bool]| -> Geometry {
        match &file.geometry {
            Geometry::Panels(panels) => Geometry::Panels(Panels::from_vertices(
                panels
                    .vertices
                    .iter()
                    .zip(keep)
                    .filter(|(_, k)| **k)
                    .map(|(p, _)| *p)
                    .collect(),
            )),
            Geometry::Splines(document) => {
                Geometry::Splines(document.select(keep).expect("picked patches"))
            }
        }
    };
    let interior = MeshFile {
        path: file.path.clone(),
        format: file.format,
        geometry: part(&pick),
        symmetry: file.symmetry,
        panel_size: file.panel_size,
        discretization: rows(&pick),
    };
    let body = MeshFile {
        geometry: part(&not),
        discretization: rows(&not),
        ..file
    };
    (body, Some(interior))
}

/// What a file holds (the volume of a body, the area of an interior surface), and its surface for display
/// (owners from `first_tag`, one per patch, or one for all panels).
fn surface_of(file: MeshFile, first_tag: i32, interior: bool) -> Result<(Surface, i32, Summary)> {
    let images = file.symmetry.images();
    let count = file.count();
    let mut summary = Summary {
        name: file
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        format: file.format,
        patches: matches!(file.geometry, Geometry::Splines(_)),
        count,
        triangles: 0,
        symmetry: file.symmetry,
        rows: file.discretization.is_some(),
        panel_size: file.panel_size,
        volumes: None,
        area: None,
        checks: None,
    };
    match file.geometry {
        Geometry::Panels(panels) => {
            summary.triangles = panels.vertices.iter().filter(|p| p[2] == p[3]).count();
            if interior {
                let area: f64 = panels
                    .vertices
                    .iter()
                    .map(|panel| {
                        let p = panel.map(DVec3::from_array);
                        ((p[1] - p[0]).cross(p[2] - p[0]) + (p[2] - p[0]).cross(p[3] - p[0]))
                            .length()
                            / 2.
                    })
                    .sum();
                summary.area = Some(area * images);
            } else {
                summary.volumes = Some(panel_volumes(&panels.vertices, file.symmetry));
            }
            let surface = Surface::new(
                Geometry::Panels(panels),
                vec![first_tag; count],
                Vec::new(),
                None,
                vec![0; count],
                DEFAULT_POTENTIAL_PANEL_SIZE_M,
            )?;
            Ok((surface, first_tag + 1, summary))
        }
        Geometry::Splines(document) => {
            let patches: Vec<bspline::Patch> = document.patches.iter().map(engine_patch).collect();
            let lengths: Vec<[f64; 2]> =
                checks::par_map(patches.len(), |k| ho::edge_lengths(&patches[k]));
            let levels: Vec<u8> = checks::par_map(patches.len(), |k| {
                let (jacobian, folds) = checks::jacobian(&patches[k], 21);
                let angles = checks::corner_angles(&patches[k]);
                if folds > 0 {
                    2
                } else if jacobian < ho::JACOBIAN_WARN
                    || angles
                        .iter()
                        .any(|&a| a < ho::CORNER_WARN.0 || a > ho::CORNER_WARN.1)
                {
                    1
                } else {
                    0
                }
            });
            let fails = levels.iter().filter(|&&l| l == 2).count();
            let warns = levels.iter().filter(|&&l| l == 1).count();
            summary.checks = Some([count - warns - fails, warns, fails]);
            let fixed_spans = file
                .discretization
                .as_ref()
                .map(|rows| rows.iter().map(|r| [r[0], r[1]]).collect::<Vec<_>>());
            let span = file.panel_size.unwrap_or(DEFAULT_POTENTIAL_PANEL_SIZE_M);
            if interior {
                let area = checks::par_map(patches.len(), |k| checks::area(&patches[k]));
                summary.area = Some(area.iter().sum::<f64>() * images);
            } else {
                summary.volumes = Some(checks::volumes(&patches, file.symmetry));
            }
            // A body's patches by record; an interior surface is one group, as in a conversion.
            let owners: Vec<i32> = if interior {
                vec![first_tag; count]
            } else {
                (0..count as i32).map(|k| first_tag + k).collect()
            };
            let surface = Surface::new(
                Geometry::Splines(document),
                owners,
                lengths,
                fixed_spans,
                levels,
                span,
            )?;
            Ok((surface, first_tag + count as i32, summary))
        }
    }
}

impl Opened {
    /// Read the file at `path`, find its interior surface (its elements lying in z = 0), and make the
    /// views.
    pub fn open(path: &Path) -> Result<Self> {
        // An SPL file opens its GDF file: the views and the name are the GDF's.
        let file = MeshFile::read(path)?;
        Self::of(&file.path.clone(), file)
    }

    /// The views of an HBG document in memory (a body made from its dimensions), named `path`.
    pub fn hbg(path: &Path, bytes: &[u8]) -> Result<Self> {
        Self::of(path, read_hbg(path, bytes)?)
    }

    fn of(path: &Path, file: MeshFile) -> Result<Self> {
        let patches = matches!(file.geometry, Geometry::Splines(_));
        let (file, interior) = split_interior(file);
        let (body, interior_tag, summary) = surface_of(file, 1, false)?;
        // The interior is drawn as one group, after the body's.
        let (interior, interior_summary) = match interior {
            Some(file) => {
                let (surface, _, summary) = surface_of(file, interior_tag, true)?;
                (Some(surface), Some(summary))
            }
            None => (None, None),
        };
        let (kind, version) = (
            if patches { "Spline patch" } else { "Panels" },
            env!("CARGO_PKG_VERSION"),
        );
        let body_view = mesh_view(path, version, kind, (&body, None), None, interior_tag)?;
        let with_interior = interior
            .as_ref()
            .map(|s| {
                mesh_view(
                    path,
                    version,
                    kind,
                    (&body, None),
                    Some((s, None)),
                    interior_tag,
                )
            })
            .transpose()?;
        Ok(Self {
            path: path.to_path_buf(),
            patches,
            body: body_view,
            with_interior,
            summary,
            interior: interior_summary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_order_gdf_reads_its_panels_and_symmetry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("box.gdf");
        std::fs::write(
            &path,
            "title\n1.0 9.80665\n1 0\n2\n0 0 0 1 0 0 1 1 0 0 1 0\n0 0 -1 1 0 -1\n1 1 -1 1.0D0 1 -1\n",
        )
        .unwrap();
        let file = MeshFile::read(&path).unwrap();
        assert_eq!(file.symmetry, Symmetry::Yz);
        assert_eq!(file.count(), 2);
        let Geometry::Panels(panels) = &file.geometry else {
            panic!("panels expected");
        };
        assert_eq!(panels.vertices[1][3], [1., 1., -1.]);
        std::fs::write(&path, "title\n1 9.81\n0 0\n3 1\n").unwrap();
        let refused = MeshFile::read(&path).err().unwrap();
        assert!(
            format!("{refused:#}").contains("The file ends"),
            "{refused:#}"
        );
    }

    #[test]
    fn geometry_file_reads_symmetry_size_and_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("side.hbg");
        // The side of a floating cylinder (quadrant) as one exact rational patch, knots on [0, 2].
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let text = serde_json::json!({
            "format": "hbg", "version": 3, "kind": "spline_patches", "symmetry": "xz_and_yz",
            "discretization": [4, 2, 3, 3, 3, 4],
            "patches": [{"u_order": 3, "v_order": 2, "u_knots": [0, 0, 0, 2, 2, 2], "v_knots": [0, 0, 2, 2],
                "coefficients": [[[1, 0, 0], [1, 1, 0], [0, 1, 0]], [[1, 0, -0.5], [1, 1, -0.5], [0, 1, -0.5]]],
                "weights": [[1, w, 1], [1, w, 1]]}]
        });
        std::fs::write(&path, text.to_string()).unwrap();
        let file = MeshFile::read(&path).unwrap();
        assert_eq!(file.symmetry, Symmetry::XzAndYz);
        assert_eq!(file.discretization, Some(vec![[4, 2, 3, 3, 3, 4]]));
        let opened = Opened::open(&path).unwrap();
        assert!(opened.patches);
        let summary = &opened.summary;
        assert_eq!(
            (summary.count, summary.symmetry, summary.rows),
            (1, Symmetry::XzAndYz, true)
        );
        assert_eq!(summary.checks, Some([1, 0, 0]));
        assert!(summary.volumes.is_some() && summary.area.is_none());
        assert!(opened.with_interior.is_none() && opened.interior.is_none());
        // Another version is refused, naming the one read.
        std::fs::write(
            &path,
            text.to_string().replace("\"version\":3", "\"version\":2"),
        )
        .unwrap();
        let refused = format!("{:#}", MeshFile::read(&path).err().unwrap());
        assert!(refused.contains("version 3"), "{refused}");
    }
}

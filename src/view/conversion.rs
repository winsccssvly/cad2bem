// SPDX-License-Identifier: GPL-3.0-or-later
//! Background GUI adapter: exact artifacts survive display tessellation and repeated exports.

use crate::{
    cad::scene::{Face, Mode, SCHEMA, Scene},
    conversion::{
        ConversionResult, Delivered, Format, HBG_VERSION, Job, Progress, Role, Spec, Worker,
    },
    view::{spline, viewport::MeshView},
};
use anyhow::{Context, Result, bail, ensure};
use glam::DVec3;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The panels of an HBG geometry file (version 3, kind `panels`).
#[derive(Deserialize)]
pub struct Panels {
    format: String,
    version: u32,
    kind: String,
    pub vertices: Vec<[[f64; 3]; 4]>,
}

impl Panels {
    /// Panels of these vertices (P x 4 x 3, m), as a geometry file holds them.
    pub fn from_vertices(vertices: Vec<[[f64; 3]; 4]>) -> Self {
        Self {
            format: "hbg".into(),
            version: HBG_VERSION,
            kind: "panels".into(),
            vertices,
        }
    }

    fn read(bytes: &[u8]) -> Result<Self> {
        let panels: Self = serde_json::from_slice(bytes)?;
        ensure!(
            panels.format == "hbg" && panels.version == HBG_VERSION && panels.kind == "panels",
            "Expected HBG version 3 panels"
        );
        ensure!(
            !panels.vertices.is_empty()
                && panels
                    .vertices
                    .iter()
                    .flatten()
                    .flatten()
                    .all(|v| v.is_finite()),
            "Empty or non-finite panels"
        );
        Ok(panels)
    }
}

/// The geometry of a surface: low-order panels or spline patches.
pub enum Geometry {
    Panels(Panels),
    Splines(spline::Document),
}

/// A converted surface (body or interior) for display, with the CAD face and the check level of each panel or
/// patch.
pub struct Surface {
    pub geometry: Geometry,
    sampled: Option<spline::Samples>,
    /// Spline patches: the longer edge arc lengths (u, v) of each patch (m), for the span preview.
    lengths: Vec<[f64; 2]>,
    /// Spline patches: spans fixed by a file's own discretization rows, whatever the panel size.
    fixed_spans: Option<Vec<[usize; 2]>>,
    owners: Vec<i32>,
    /// The check level of each patch or panel: 0 ok, 1 warn, 2 fail.
    levels: Vec<u8>,
}

impl Surface {
    fn load(
        bytes: &[u8],
        mode: crate::conversion::Mode,
        owners: Option<&[i32]>,
        interior_tag: i32,
        checks: &[serde_json::Value],
        span: f64,
    ) -> Result<Self> {
        let geometry = if mode == crate::conversion::Mode::Ho {
            Geometry::Splines(spline::Document::read(bytes)?)
        } else {
            Geometry::Panels(Panels::read(bytes)?)
        };
        let count = match &geometry {
            Geometry::Panels(p) => p.vertices.len(),
            Geometry::Splines(p) => p.patches.len(),
        };
        let owners = owners.map_or_else(|| vec![interior_tag; count], |tags| tags.to_vec());
        ensure!(owners.len() == count, "Geometry ownership/count mismatch");
        // The edge lengths of each patch, from which the potential spans follow for any panel size.
        let lengths: Vec<[f64; 2]> = if checks.len() == count {
            checks
                .iter()
                .map(|c| {
                    let l = |k: usize| c["edge_lengths_m"][k].as_f64().unwrap_or(0.);
                    [l(0), l(1)]
                })
                .collect()
        } else {
            Vec::new()
        };
        // Patch checks colour their patches; panels and unmatched lists stay ok.
        let levels = if checks.len() == count {
            checks
                .iter()
                .map(|c| match c["state"].as_str() {
                    Some("fail") => 2,
                    Some("warn") => 1,
                    _ => 0,
                })
                .collect()
        } else {
            vec![0; count]
        };
        Self::new(geometry, owners, lengths, None, levels, span)
    }

    /// A surface of `geometry` with the owner (display group) and check level of every patch or panel; spline
    /// patches are sampled with their spans: `fixed_spans` when given, else from their edge `lengths` at the
    /// potential panel size `span` (m).
    pub fn new(
        geometry: Geometry,
        owners: Vec<i32>,
        lengths: Vec<[f64; 2]>,
        fixed_spans: Option<Vec<[usize; 2]>>,
        levels: Vec<u8>,
        span: f64,
    ) -> Result<Self> {
        let count = match &geometry {
            Geometry::Panels(p) => p.vertices.len(),
            Geometry::Splines(p) => p.patches.len(),
        };
        ensure!(
            owners.len() == count && levels.len() == count,
            "Geometry ownership/count mismatch"
        );
        let mut surface = Self {
            geometry,
            sampled: None,
            lengths,
            fixed_spans,
            owners,
            levels,
        };
        surface.sampled = surface.sample_at(span)?;
        Ok(surface)
    }

    /// Spline patches sampled with the lines between their potential spans at the panel size `span` (m).
    fn sample_at(&self, span: f64) -> Result<Option<spline::Samples>> {
        let Geometry::Splines(document) = &self.geometry else {
            return Ok(None);
        };
        let spans: Vec<[usize; 2]> = match &self.fixed_spans {
            Some(spans) => spans.clone(),
            None => self
                .lengths
                .iter()
                .map(|&l| crate::conversion::potential_spans(l, span))
                .collect(),
        };
        Ok(Some(document.sample_spans(&self.owners, &spans)?))
    }

    /// The number of panels or patches.
    pub fn count(&self) -> usize {
        self.owners.len()
    }

    /// The panels to draw: the panels themselves, or the samples of the spline patches.
    pub fn display_vertices(&self) -> &[[[f64; 3]; 4]] {
        match &self.geometry {
            Geometry::Panels(p) => &p.vertices,
            Geometry::Splines(_) => &self.sampled.as_ref().unwrap().vertices,
        }
    }

    /// Display panels with their owner, the weight of each edge (1 a patch or panel edge, 0.6 a span line,
    /// 0 hidden) and the check level.
    fn cells<'a>(
        &'a self,
        sampled: Option<&'a spline::Samples>,
    ) -> impl Iterator<Item = (&'a [[f64; 3]; 4], i32, [f32; 4], u8)> {
        let vertices = match (&self.geometry, sampled) {
            (Geometry::Splines(_), Some(s)) => &s.vertices,
            _ => self.display_vertices(),
        };
        vertices.iter().enumerate().map(move |(i, panel)| {
            if let Some(sampled) = sampled {
                let level = self.levels[sampled.patch[i]];
                let weight = std::array::from_fn(|k| {
                    if sampled.edges[i][k] {
                        1.
                    } else if sampled.spans[i][k] {
                        0.6
                    } else {
                        0.
                    }
                });
                (panel, sampled.owners[i], weight, level)
            } else {
                (panel, self.owners[i], [1.; 4], self.levels[i])
            }
        })
    }
}

/// A finished conversion held for display and export: the worker job, its result and the surfaces.
pub struct Session {
    job: Job,
    pub result: ConversionResult,
    pub body: Surface,
    pub interior: Option<Surface>,
}

impl Session {
    fn load(job: Job, result: ConversionResult) -> Result<Self> {
        let interior_tag = result
            .body_faces
            .iter()
            .max()
            .context("No prepared faces")?
            .checked_add(1)
            .context("Prepared face tag overflow")?;
        let body = Surface::load(
            &job.read_artifact(&result, Role::Body, Format::Hbg)?,
            result.mode,
            Some(&result.body_faces),
            interior_tag,
            &result.patch_checks,
            Self::span_of(&result),
        )?;
        let interior = result
            .request
            .settings
            .interior
            .then(|| {
                Surface::load(
                    &job.read_artifact(&result, Role::Interior, Format::Hbg)?,
                    result.mode,
                    None,
                    interior_tag,
                    &result.interior_patch_checks,
                    Self::span_of(&result),
                )
            })
            .transpose()?;
        Ok(Self {
            job,
            result,
            body,
            interior,
        })
    }

    /// The potential panel size of the span preview of a result: its request's, or the preview default.
    pub fn span_of(result: &ConversionResult) -> f64 {
        result
            .request
            .settings
            .potential_panel_size
            .unwrap_or(crate::conversion::DEFAULT_POTENTIAL_PANEL_SIZE_M)
    }

    /// Publish the result files to `path`, never replacing existing files (tests and the smoke export; the GUI
    /// uses [`Session::publish_with`]).
    pub fn publish(&self, path: &Path) -> Result<Vec<Delivered>> {
        self.job.publish(&self.result, path)
    }

    /// The GUI's export: [`Session::publish`] with the potential panel size of the HBG files set to `panel_size`
    /// (ho), replacing existing files (the save dialog asked first).
    pub fn publish_with(&self, path: &Path, panel_size: Option<f64>) -> Result<Vec<Delivered>> {
        self.job.publish_with(&self.result, path, panel_size, true)
    }

    /// The views of the body and of the body with its interior, sampled with the span lines at the potential
    /// panel size `span` (m): the preview follows a new size without converting again.
    pub fn views_at(&self, span: f64) -> Result<(MeshView, Option<MeshView>)> {
        let body = self.body.sample_at(span)?;
        let interior = self
            .interior
            .as_ref()
            .map(|s| s.sample_at(span))
            .transpose()?
            .flatten();
        let samples = (body.as_ref(), interior.as_ref());
        let with_interior = self
            .interior
            .as_ref()
            .map(|_| self.view(true, samples))
            .transpose()?;
        Ok((self.view(false, samples)?, with_interior))
    }

    fn view(
        &self,
        show_interior: bool,
        samples: (Option<&spline::Samples>, Option<&spline::Samples>),
    ) -> Result<MeshView> {
        let interior_tag = self
            .result
            .body_faces
            .iter()
            .max()
            .context("No prepared faces")?
            .checked_add(1)
            .context("Prepared face tag overflow")?;
        mesh_view(
            &self.result.request.source,
            &self.result.engine_version,
            if matches!(self.body.geometry, Geometry::Splines(_)) {
                "Spline patches"
            } else {
                "Panels"
            },
            (&self.body, samples.0),
            self.interior
                .as_ref()
                .filter(|_| show_interior)
                .map(|s| (s, samples.1)),
            interior_tag,
        )
    }
}

/// The display of a body surface and, when given, its interior surface (each with display samples that
/// replace its own): panels or sampled patches with their edges and check levels, grouped by owner (`body_kind`
/// names a group of the body, `interior_tag` the interior's).
pub(super) fn mesh_view(
    source: &Path,
    sdk_version: &str,
    body_kind: &str,
    body: (&Surface, Option<&spline::Samples>),
    interior: Option<(&Surface, Option<&spline::Samples>)>,
    interior_tag: i32,
) -> Result<MeshView> {
    let patches = matches!(body.0.geometry, Geometry::Splines(_));
    let mut scene = Scene {
        open: None,
        schema: SCHEMA,
        source: source.to_path_buf(),
        sdk_version: sdk_version.to_string(),
        mode: if patches {
            Mode::BemPatches
        } else {
            Mode::BemPanels
        },
        bounds_m: [
            f64::INFINITY,
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        positions_m: Vec::new(),
        triangles: Vec::new(),
        face_tags: Vec::new(),
        faces: Vec::new(),
        prepared: None,
    };
    let mut edges = Vec::new();
    let mut levels = Vec::new();
    let mut faces = BTreeMap::<i32, Face>::new();
    let (body, body_samples) = body;
    let panels = body.cells(body_samples.or(body.sampled.as_ref())).chain(
        interior
            .into_iter()
            .flat_map(|(s, samples)| s.cells(samples.or(s.sampled.as_ref()))),
    );
    for (panel, tag, boundary, level) in panels {
        let start = u32::try_from(scene.positions_m.len())?;
        ensure!(
            start <= u32::MAX - 4,
            "Panel display exceeds index capacity"
        );
        scene.positions_m.extend(panel);
        for p in panel {
            for (axis, &coordinate) in p.iter().enumerate() {
                scene.bounds_m[axis] = scene.bounds_m[axis].min(coordinate);
                scene.bounds_m[axis + 3] = scene.bounds_m[axis + 3].max(coordinate);
            }
        }
        let points = panel.map(DVec3::from_array);
        let area1 = (points[1] - points[0])
            .cross(points[2] - points[0])
            .length()
            * 0.5;
        let area2 = (points[2] - points[0])
            .cross(points[3] - points[0])
            .length()
            * 0.5;
        let triangle = panel[2] == panel[3];
        if area1 > 0. {
            scene.triangles.push([start, start + 1, start + 2]);
            edges.push(if triangle {
                [1.; 3]
            } else {
                [boundary[1], 0., boundary[0]]
            });
            levels.push(level);
            scene.face_tags.push(tag);
        }
        if area2 > 0. {
            scene.triangles.push([start, start + 2, start + 3]);
            edges.push([boundary[2], boundary[3], 0.]);
            levels.push(level);
            scene.face_tags.push(tag);
        }
        let area = area1 + area2;
        let face = faces.entry(tag).or_insert_with(|| Face {
            tag,
            kind: if tag == interior_tag {
                super::viewport::INTERIOR_KIND
            } else {
                body_kind
            }
            .into(),
            area_m2: 0.,
            triangles: 0,
            surface: None,
            corners: Vec::new(),
        });
        face.area_m2 += area;
        face.triangles += usize::from(area1 > 0.) + usize::from(area2 > 0.);
    }
    scene.faces = faces.into_values().collect();
    MeshView::panels(scene, edges, levels)
}

/// A loaded conversion: the session and its views without and with the interior surface.
pub struct Ready {
    pub session: Arc<Session>,
    pub body: MeshView,
    pub with_interior: Option<MeshView>,
}

/// A message from the conversion thread: worker progress, or the loaded result or the error.
pub enum Event {
    Progress(Progress),
    Finished(Result<Box<Ready>, String>),
}

/// Owns a worker thread. Cancelling never waits on the rendering thread; drop reaps on close.
pub struct Task {
    pub events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub started: Instant,
}

impl Task {
    /// Convert in a private worker process, watched from a background thread.
    pub fn start(spec: Spec, worker: Worker) -> Result<Self> {
        spec.validate()?;
        let (sender, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let thread = thread::Builder::new()
            .name("bem-conversion".into())
            .spawn(move || {
                let result = (|| -> Result<Ready> {
                    let mut job = Job::start(&spec, &worker)?;
                    let result = loop {
                        if signal.load(Ordering::Relaxed) {
                            job.cancel();
                            bail!("Conversion cancelled. The previous result is still shown.");
                        }
                        for progress in job.progress()? {
                            let _ = sender.send(Event::Progress(progress));
                        }
                        if let Some(result) = job.poll() {
                            break result?;
                        }
                        thread::sleep(Duration::from_millis(20));
                    };
                    let session = Session::load(job, result)?;
                    let body = session.view(false, (None, None))?;
                    let with_interior = session
                        .interior
                        .as_ref()
                        .map(|_| session.view(true, (None, None)))
                        .transpose()?;
                    ensure!(
                        !signal.load(Ordering::Relaxed),
                        "Conversion cancelled. The previous result is still shown."
                    );
                    Ok(Ready {
                        session: Arc::new(session),
                        body,
                        with_interior,
                    })
                })()
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(Event::Finished(result.map(Box::new)));
            })?;
        Ok(Self {
            events,
            cancel,
            thread: Some(thread),
            started: Instant::now(),
        })
    }

    /// Ask the conversion thread to stop; it does not wait for it.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Publication keeps the session alive even if the displayed scene is replaced.
pub struct ExportTask {
    pub events: Receiver<Result<Vec<Delivered>, String>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for ExportTask {
    fn drop(&mut self) {
        // Window close waits for the transactional publication to finish or roll back.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Publish `session` to `path` in the background, with the potential panel size `panel_size` (ho).
pub fn export(session: Arc<Session>, path: PathBuf, panel_size: Option<f64>) -> Result<ExportTask> {
    let (sender, receiver) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("bem-export".into())
        .spawn(move || {
            let _ = sender.send(
                session
                    .publish_with(&path, panel_size)
                    .map_err(|error| format!("{error:#}")),
            );
        })?;
    Ok(ExportTask {
        events: receiver,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_kind_and_empty_panel_artifacts() {
        assert!(
            Panels::read(br#"{"format":"hbg","version":3,"kind":"spline_patches","vertices":[]}"#)
                .is_err()
        );
        assert!(
            Panels::read(br#"{"format":"hbg","version":3,"kind":"panels","vertices":[]}"#).is_err()
        );
    }
}

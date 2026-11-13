// SPDX-License-Identifier: GPL-3.0-or-later
//! One-shot GUI: CAD inspection, shared BEM conversion jobs and validated artifact export.
//!
//! This module holds the application state and its jobs. `panels` draws the window, `diagnostics` keeps the
//! session log and saves diagnostics, `history` undoes and redoes settings, `junction` makes the pipe-junction
//! cuts, `smoke` runs the scripted visual checks and `startup` parses the GUI options.

mod about;
mod diagnostics;
mod fonts;
mod history;
mod junction;
mod panels;
mod smoke;
mod startup;

pub use startup::Startup;

use anyhow::Result;
use cad2bem::{
    cad::{
        scene::{Mode, Preparation, Request, SCHEMA},
        worker::Job,
    },
    conversion,
    engine::{primitives, primitives_ho},
    view::{
        conversion::{self as session, Event, Session, Task},
        fast, mesh_file,
        viewport::{self, Camera, MeshView},
    },
};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

/// A geometry file shown in the Mesh Viewer tab: its path, whether it holds spline patches, and what its body
/// and its interior surface (its elements in z = 0) hold.
pub(super) struct OpenedFile {
    path: PathBuf,
    patches: bool,
    summary: mesh_file::Summary,
    interior: Option<mesh_file::Summary>,
}

/// The views of a result sampled again at another potential panel size, in the background.
struct Respan {
    session: Arc<Session>,
    span: f64,
    views: std::sync::mpsc::Receiver<Result<(MeshView, Option<MeshView>), String>>,
}

/// File extensions the GUI opens.
const CAD_EXTENSIONS: [&str; 4] = ["step", "stp", "iges", "igs"];
/// Why a cut waits: the cut before it is still being applied to the faces.
const CUT_APPLYING: &str =
    "Wait for the last cut to finish: the next cut can be added once its faces are shown";

/// The tabs: preparing the CAD and meshing it (the two steps of the work), and, apart from them, viewing a BEM
/// geometry file (Mesh Viewer) and making a body from its dimensions (Fast Mesh). Each keeps its own view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    Cad,
    Mesh,
    View,
    Fast,
}

impl Stage {
    /// A tab apart from the two steps, with a view, interior switch and camera of its own.
    fn apart(self) -> bool {
        matches!(self, Stage::View | Stage::Fast)
    }
}

/// The view of a tab apart (with its interior view and switch, camera and fit) while another tab is shown, and
/// the shown tab's while it is.
struct Slot {
    view: Option<MeshView>,
    alternate: Option<MeshView>,
    interior: bool,
    camera: Camera,
    fit: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            view: None,
            alternate: None,
            interior: false,
            camera: Camera::default(),
            fit: true,
        }
    }
}

/// The Fast Mesh tab: the body to make, and the body made last with the request it was made from.
pub(super) struct Fast {
    request: primitives::Request,
    made: Option<(primitives::Request, FastMade)>,
}

/// A Fast Mesh body: panels, or exact spline patches with what the viewer reads of them (counts, volume,
/// interior area).
pub(super) enum FastMade {
    Panels(primitives::Made),
    Patches {
        made: primitives_ho::MadePatches,
        summary: Box<mesh_file::Summary>,
        interior: Option<Box<mesh_file::Summary>>,
    },
}

impl FastMade {
    /// Make the body of `request` and its views (the body, and with the interior surface).
    fn make(request: &primitives::Request) -> Result<(Self, MeshView, Option<MeshView>)> {
        if !request.high_order {
            let made = primitives::make(request)?;
            let (body, with_interior) = fast::views(&made)?;
            return Ok((Self::Panels(made), body, with_interior));
        }
        let made = primitives_ho::make(request)?;
        let bytes = serde_json::to_vec(&made.merged().to_hbg())?;
        let opened = mesh_file::Opened::hbg(std::path::Path::new(&made.body.label), &bytes)?;
        Ok((
            Self::Patches {
                made,
                summary: Box::new(opened.summary),
                interior: opened.interior.map(Box::new),
            },
            opened.body,
            opened.with_interior,
        ))
    }

    /// The elements of the body (panels or patches) and their name.
    pub(super) fn count(&self) -> (usize, &'static str) {
        match self {
            Self::Panels(made) => (made.body.vertices.len(), "panels"),
            Self::Patches { made, .. } => (made.body.patches.len(), "patches"),
        }
    }
}

/// Application state: the displayed scene, the running jobs and the settings for the next conversion.
pub struct Desktop {
    // Displayed scene and its interaction state.
    view: Option<MeshView>,
    /// The other converted view (body only, or body with interior), swapped in by the "Interior surface" switch.
    alternate_view: Option<MeshView>,
    /// The stage shown, and the view of the other stage (with its interior view and switch), kept aside.
    stage: Stage,
    stashed: Option<MeshView>,
    stashed_alternate: Option<MeshView>,
    stashed_interior: bool,
    /// The views of the tabs apart (Mesh Viewer, Fast Mesh), and the step tab shown before one of them.
    viewer: Slot,
    fast_slot: Slot,
    before_view: Stage,
    /// The Fast Mesh tab's body.
    fast: Fast,
    show_interior: bool,
    camera: Camera,
    fit_pending: bool,
    /// Bumped whenever `view` changes so the GPU buffers are rebuilt.
    revision: u64,
    /// Selected CAD face tag, or prepared-face tag for a converted result.
    selected: Option<i32>,
    wire: bool,
    colors: bool,
    /// Show the lines between the potential spans of spline patches (off by default).
    spans: bool,
    /// The potential panel size (m) of the span lines on screen, and the views being sampled at another.
    shown_span: f64,
    respan: Option<Respan>,

    // Cut mode (CAD tab): the line under the mouse is the cut a click adds; it snaps to the CAD corners.
    picked_point: Option<[f64; 3]>,
    cut_mode: Option<panels::CutMode>,
    cut_scope: panels::CutScope,
    /// The face the previewed cut is limited to (scope Face).
    cut_face: Option<i32>,
    cut_preview: Option<conversion::Cut>,
    /// The corner the cut snapped to (m).
    cut_snap: Option<[f64; 3]>,
    /// Two-point cut: the first point (snapped), the surface point hit there and its face.
    cut_first: Option<([f64; 3], [f64; 3], i32)>,
    /// The cut mode the first point (or the reference face) was picked in.
    cut_first_mode: Option<panels::CutMode>,
    /// Parallel cuts: the normal of the reference face where it was clicked (kept when cuts renumber faces).
    cut_normal: [f64; 3],
    /// Junction cuts to add after the panels are drawn: what they are for, and the cuts.
    junction_cuts: Option<(String, Vec<conversion::Cut>)>,
    /// The file the running preview job also writes the CAD to.
    saving: Option<PathBuf>,
    /// A cut was added in the viewport: show it at once (a quick view), then the exact one.
    cut_added: bool,
    /// The cuts changed and the exact prepared view of them is not shown yet: no other cut until it is (one
    /// cut at a time, each seen on the faces it made).
    cut_applying: bool,
    /// The next view job is quick (see `Request::quick`).
    quick: bool,
    /// The session log.
    log: diagnostics::Log,
    /// The diagnostics folder waiting for its screenshot.
    diagnostics_pending: Option<PathBuf>,
    /// Smoke: the diagnostics were saved.
    diagnostics_saved: bool,
    /// Smoke: the cut was clicked.
    clicked_cut: bool,
    /// Undo and redo of the settings.
    history: history::History,

    // Native CAD inspection.
    source: Option<PathBuf>,
    library: String,
    /// Untrimmed surface mesh size in metres.
    size_m: f64,
    job: Option<Job>,
    /// The preparation the running inspection job applies, and the one the shown CAD view was made with
    /// (None: the CAD as imported).
    pending_preparation: Option<Preparation>,
    shown_preparation: Option<Preparation>,
    /// The preparation settings last seen, and when they last changed.
    live_seen: Option<Preparation>,
    live_changed: Option<Instant>,

    // Conversion and export; `mode` and `settings` are for the next run, `converted` is the last result.
    mode: conversion::Mode,
    settings: conversion::Settings,
    conversion: Option<Task>,
    converted: Option<Arc<Session>>,
    /// A geometry file opened to view it (in place of a CAD file and its conversion).
    opened: Option<OpenedFile>,
    exporting: Option<session::ExportTask>,

    status: String,
    error: Option<String>,
    /// The About window, open with the licence file it shows.
    about: Option<usize>,

    // Startup automation and visual smoke checks.
    startup: Startup,
    stable_frames: u32,
    screenshot_requested: bool,
    picked: bool,
    started: Instant,
    verified: Arc<AtomicBool>,
}

impl Desktop {
    /// The application: installs the viewport renderer and the light style and applies the startup options;
    /// `verified` is set when a smoke check passes.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        startup: Startup,
        verified: Arc<AtomicBool>,
    ) -> Result<Self> {
        viewport::install(cc)?;
        fonts::install(&cc.egui_ctx)?;
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        cc.egui_ctx.set_theme(egui::ThemePreference::Light);
        cc.egui_ctx.style_mut(|style| {
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(14.));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(14.));
            style
                .text_styles
                .insert(egui::TextStyle::Small, egui::FontId::proportional(12.));
        });
        let source = startup.open.clone();
        let library = startup
            .library
            .clone()
            .or_else(|| std::env::var_os("CAD2BEM_GMSH_LIBRARY").map(PathBuf::from));
        let mut desktop = Self {
            view: None,
            camera: Camera::default(),
            fit_pending: true,
            revision: 0,
            selected: None,
            source,
            library: library
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            size_m: startup.mesh_size.unwrap_or(2.),
            wire: false,
            colors: true,
            spans: startup.show_spans,
            picked_point: None,
            cut_mode: match startup.cut_mode.as_deref() {
                Some("across") => Some(panels::CutMode::Across),
                Some("along") => Some(panels::CutMode::Along),
                Some("points") => Some(panels::CutMode::Points),
                Some("parallel") => Some(panels::CutMode::Parallel),
                _ => None,
            },
            cut_scope: panels::CutScope::Face,
            cut_face: None,
            cut_preview: None,
            cut_snap: None,
            cut_first: None,
            cut_first_mode: None,
            cut_normal: [0.; 3],
            junction_cuts: None,
            saving: None,
            cut_added: false,
            cut_applying: false,
            quick: false,
            log: diagnostics::Log::open(),
            diagnostics_pending: None,
            diagnostics_saved: false,
            clicked_cut: false,
            history: Default::default(),
            job: None,
            pending_preparation: None,
            shown_preparation: None,
            live_seen: None,
            live_changed: None,
            conversion: None,
            converted: None,
            opened: None,
            alternate_view: None,
            shown_span: conversion::DEFAULT_POTENTIAL_PANEL_SIZE_M,
            respan: None,
            stage: Stage::Cad,
            stashed: None,
            stashed_alternate: None,
            stashed_interior: false,
            viewer: Slot::default(),
            fast_slot: Slot::default(),
            before_view: Stage::Cad,
            fast: Fast {
                request: primitives::Request {
                    // The first shape, or the one a smoke check names.
                    shape: ["cylinder", "moonpool", "sphere", "hemisphere", "barge"]
                        .iter()
                        .position(|s| Some(*s) == startup.fast_shape.as_deref())
                        .map_or(primitives::Shape::ALL[0], |k| primitives::Shape::ALL[k]),
                    panel_size: 1.,
                    symmetry: conversion::Symmetry::XzAndYz,
                    interior: true,
                    high_order: startup.fast_high_order,
                    triangles: startup.fast_triangles,
                },
                made: None,
            },
            show_interior: false,
            settings: startup.settings.clone(),
            mode: startup.mode,
            exporting: None,
            status: "Open a STEP or IGES model, or a BEM geometry file (HBG, GDF, STL), to begin."
                .into(),
            error: None,
            about: startup.about.then_some(0),
            startup,
            stable_frames: 0,
            screenshot_requested: false,
            picked: false,
            started: Instant::now(),
            verified,
        };
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        desktop.log.add(format!("start: {}", arguments.join(" ")));
        desktop.log.add(format!(
            "console window: {}",
            if crate::console::window() {
                "yes"
            } else {
                "none"
            }
        ));
        if let Some(path) = desktop
            .source
            .clone()
            .filter(|p| mesh_file::is_mesh_file(p))
        {
            // A geometry file to view: not a CAD source.
            desktop.source = None;
            desktop.open_mesh(path);
        } else if desktop.source.is_some() {
            if desktop.startup.convert {
                desktop.convert();
            } else {
                // The CAD tab always shows the CAD as the conversion prepares it.
                desktop.prepare_cad();
            }
        }
        Ok(desktop)
    }

    fn start(&mut self, mode: Mode) {
        self.start_with(mode, None);
    }

    /// Add cuts made for the user at once (junction cuts): one undo step, one view.
    fn add_cuts(&mut self, what: &str, cuts: Vec<conversion::Cut>) {
        if self.cut_applying {
            self.status = CUT_APPLYING.into();
            return;
        }
        if cuts.is_empty() {
            self.status = format!("No junction cuts are needed for {what}");
            return;
        }
        self.log.add(format!(
            "junction cuts for {what}: {}",
            serde_json::to_string(&cuts).unwrap_or_default()
        ));
        self.status = format!("Added {} for {what}", panels::counted(cuts.len(), "cut"));
        self.settings.cuts.extend(cuts);
        self.cut_applying = true;
        if !self.settings.faces.is_empty() {
            // Cuts renumber the prepared faces.
            self.settings.faces.clear();
        }
    }

    /// Show the CAD as the next conversion will prepare it (cuts, sheet removal).
    fn prepare_cad(&mut self) {
        self.start_with(Mode::Preview, Some(Preparation::of(&self.settings)));
    }

    fn start_with(&mut self, mode: Mode, preparation: Option<Preparation>) {
        if self.busy() {
            self.saving = None;
            self.quick = false;
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        let request = Request {
            schema: SCHEMA,
            source,
            library: (!self.library.trim().is_empty()).then(|| PathBuf::from(self.library.trim())),
            mode,
            size_m: (mode == Mode::SurfaceMesh).then_some(self.size_m),
            preparation: preparation.clone(),
            save_cad: self.saving.clone(),
            quick: std::mem::take(&mut self.quick),
        };
        self.log.add(format!(
            "view {:?}{}{}",
            mode,
            preparation.as_ref().map_or(String::new(), |p| format!(
                ", prepared: {}",
                serde_json::to_string(p).unwrap_or_default()
            )),
            self.saving
                .as_ref()
                .map_or(String::new(), |p| format!(", save CAD to {}", p.display()))
        ));
        match Job::start(&request) {
            Ok(job) => {
                self.job = Some(job);
                self.pending_preparation = preparation;
                self.error = None;
                self.stable_frames = 0;
                let after_quick = self.view.as_ref().is_some_and(|v| {
                    v.scene
                        .prepared
                        .as_ref()
                        .is_some_and(|p| p["provisional"] == true)
                });
                self.status = match mode {
                    Mode::Preview if after_quick => "Cut drawn; working out the new faces…",
                    Mode::Preview if self.pending_preparation.is_some() => {
                        "Preparing the CAD (cuts, extra faces)…"
                    }
                    Mode::Preview => "Reading the CAD file…",
                    Mode::SurfaceMesh => "Making the untrimmed surface mesh…",
                    Mode::BemPanels | Mode::BemPatches => unreachable!(),
                }
                .into();
            }
            Err(error) => {
                self.saving = None;
                self.failed(format!("{error:#}"));
            }
        }
    }

    fn open(&mut self) {
        let all: Vec<&str> = CAD_EXTENSIONS
            .iter()
            .chain(&mesh_file::EXTENSIONS)
            .copied()
            .collect();
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("CAD and BEM geometry", &all)
            .add_filter("CAD (STEP, IGES)", &CAD_EXTENSIONS)
            .add_filter("BEM geometry (HBG, GDF, SPL, STL)", &mesh_file::EXTENSIONS)
            .pick_file()
        {
            self.open_path(path);
        }
    }

    /// Open a geometry file (`.hbg`, `.gdf`) in the Mesh Viewer tab, with its interior surface (its elements in
    /// z = 0). The CAD and Mesh tabs keep their file and result; a file that cannot be read leaves the tab as it was.
    fn open_mesh(&mut self, path: PathBuf) {
        self.log.add(format!("open geometry {}", path.display()));
        let opened = match mesh_file::Opened::open(&path) {
            Ok(opened) => opened,
            Err(error) => {
                self.log.add(format!("not opened: {error:#}"));
                self.error = Some(format!("{error:#}"));
                return;
            }
        };
        for summary in std::iter::once(&opened.summary).chain(&opened.interior) {
            self.log.add(summary.line());
        }
        self.set_stage(Stage::View);
        self.view = Some(opened.body);
        self.alternate_view = opened.with_interior;
        // An interior surface found in the file is shown with it (the switch can hide it).
        self.show_interior = false;
        if self.alternate_view.is_some() {
            std::mem::swap(&mut self.view, &mut self.alternate_view);
            self.show_interior = true;
        }
        self.selected = None;
        self.cut_preview = None;
        self.wire = true;
        self.colors = true;
        self.fit_pending = true;
        self.revision += 1;
        self.error = None;
        self.status = format!("Opened {}", opened.summary.name);
        self.opened = Some(OpenedFile {
            path: opened.path,
            patches: opened.patches,
            summary: opened.summary,
            interior: opened.interior,
        });
    }

    /// Open a CAD file and start its preview, or a geometry file to view it.
    fn open_path(&mut self, path: PathBuf) {
        if mesh_file::is_mesh_file(&path) {
            self.open_mesh(path);
            return;
        }
        self.live_seen = None;
        self.live_changed = None;
        // A new file starts at the CAD stage with the default settings: the results, settings, cuts and face
        // settings of the last one do not apply to it.
        self.set_stage(Stage::Cad);
        self.converted = None;
        self.stashed = None;
        self.stashed_alternate = None;
        self.mode = conversion::Mode::default();
        self.settings = conversion::Settings::default();
        self.settings.adapt_sizes(self.mode);
        self.cut_mode = None;
        self.cut_first = None;
        self.cut_preview = None;
        self.cut_added = false;
        self.cut_applying = false;
        self.selected = None;
        self.log.add(format!("open {}", path.display()));
        self.source = Some(path);
        self.history.clear();
        self.shown_preparation = None;
        self.startup.mesh_size = None;
        self.prepare_cad();
    }

    /// Open a STEP or IGES file dropped on the window; show a hint while files are dragged over it.
    fn dropped(&mut self, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            let rect = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("file-drop"),
            ));
            painter.rect_filled(rect, 0., egui::Color32::from_black_alpha(60));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop a STEP, IGES, HBG, GDF, SPL or STL file to open it",
                egui::FontId::proportional(24.),
                egui::Color32::WHITE,
            );
        }
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        let Some(file) = files.first() else {
            return;
        };
        let Some(path) = file.path.clone() else {
            self.error = Some("The dropped item is not a file on disk".into());
            return;
        };
        let cad = path
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| CAD_EXTENSIONS.contains(&x.to_ascii_lowercase().as_str()));
        if !cad && !mesh_file::is_mesh_file(&path) {
            self.error = Some(format!(
                "{} cannot be opened: cad2bem opens STEP, IGES, HBG, GDF, SPL and STL files (.step, .stp, .iges, .igs, .hbg, \
                 .gdf, .spl, .stl)",
                path.display()
            ));
        } else if self.busy() {
            self.error =
                Some("Wait for the running job, or cancel it, before opening another file".into());
        } else {
            self.error = None;
            self.open_path(path);
        }
    }

    /// Take the results of the running jobs. While a tab apart is shown, they go to the CAD and Mesh tabs as if
    /// those were shown, and the tab apart stays.
    fn poll(&mut self) {
        let shown = self.stage;
        if !shown.apart() {
            self.poll_jobs();
            return;
        }
        self.swap_slot(shown);
        self.stage = self.before_view;
        self.poll_jobs();
        self.before_view = self.stage;
        self.swap_slot(shown);
        self.stage = shown;
    }

    /// Trade the view, interior switch, camera and fit of the tab apart `stage` with the shown ones.
    fn swap_slot(&mut self, stage: Stage) {
        let slot = if stage == Stage::Fast {
            &mut self.fast_slot
        } else {
            &mut self.viewer
        };
        std::mem::swap(&mut self.view, &mut slot.view);
        std::mem::swap(&mut self.alternate_view, &mut slot.alternate);
        std::mem::swap(&mut self.show_interior, &mut slot.interior);
        std::mem::swap(&mut self.camera, &mut slot.camera);
        std::mem::swap(&mut self.fit_pending, &mut slot.fit);
    }

    /// Make the Fast Mesh tab's body again when its request changed (while the tab is shown): a few
    /// milliseconds, so it follows every change. The camera fits a new kind of shape.
    fn follow_fast(&mut self) {
        if self.stage != Stage::Fast
            || self
                .fast
                .made
                .as_ref()
                .is_some_and(|(r, _)| *r == self.fast.request)
        {
            return;
        }
        let request = self.fast.request;
        let kind_changed = self.fast.made.as_ref().is_none_or(|(r, _)| {
            !r.shape.same_kind(&request.shape) || r.symmetry != request.symmetry
        });
        match FastMade::make(&request) {
            Ok((made, body, with_interior)) => {
                let interior = with_interior.is_some();
                self.view = Some(body);
                self.alternate_view = with_interior;
                // The interior surface shown when it is made, as after a conversion.
                self.show_interior = false;
                if interior {
                    std::mem::swap(&mut self.view, &mut self.alternate_view);
                    self.show_interior = true;
                }
                self.selected = None;
                self.wire = true;
                self.colors = false;
                self.fit_pending |= kind_changed;
                self.revision += 1;
                self.error = None;
                let (count, kind) = made.count();
                self.status = format!("{} · {count} {kind}", request.shape.name());
                self.fast.made = Some((request, made));
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                // Not tried again until the request changes.
                if let Some((r, _)) = &mut self.fast.made {
                    *r = request;
                } else {
                    self.view = None;
                }
            }
        }
    }

    /// Write the Fast Mesh body to `path` (`.gdf`, `.hbg` or `.stl`): one file, with the interior surface after
    /// the body when it has one. An existing file is replaced (the save dialog asked first).
    fn export_fast(&mut self, path: std::path::PathBuf) {
        let Some((request, made)) = &self.fast.made else {
            return;
        };
        let write = || -> Result<()> {
            let format = conversion::Format::from_path(&path)?;
            let text = match (made, format) {
                (FastMade::Panels(made), conversion::Format::Gdf) => made.merged().to_gdf()?,
                (FastMade::Panels(made), conversion::Format::Stl) => {
                    made.body.to_stl(made.interior.as_ref())?
                }
                (FastMade::Panels(made), conversion::Format::Hbg) => {
                    serde_json::to_string(&made.merged().to_hbg()?)?
                }
                (FastMade::Patches { made, .. }, conversion::Format::Hbg) => {
                    serde_json::to_string(&made.merged().to_hbg())?
                }
                (FastMade::Patches { .. }, _) => {
                    anyhow::bail!("Spline patches can only be saved as HBG")
                }
            };
            std::fs::write(&path, text)?;
            Ok(())
        };
        match write() {
            Ok(()) => {
                self.log.add(format!(
                    "fast mesh {}: {}",
                    serde_json::to_string(request).unwrap_or_default(),
                    path.display()
                ));
                self.status = format!("Wrote {}", path.display());
                self.error = None;
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    fn poll_jobs(&mut self) {
        self.poll_conversion();
        let Some(result) = self.job.as_mut().and_then(Job::poll) else {
            return;
        };
        let duration = self.job.as_ref().unwrap().started.elapsed();
        self.job = None;
        let saving = self.saving.take();
        match result.and_then(MeshView::new) {
            Ok(view) => {
                // The camera follows into a view that is shown now; a CAD view that ends while the Mesh tab is
                // shown is stashed, and the tab change carries the camera into it.
                if self.stage != Stage::Mesh {
                    self.carry_camera(&view);
                }
                let mode = view.scene.mode;
                self.status = format!(
                    "{} faces · {} triangles · {:.2} s",
                    view.scene.faces.len(),
                    view.scene.triangles.len(),
                    duration.as_secs_f64()
                );
                self.shown_preparation = self.pending_preparation.take();
                let provisional = view
                    .scene
                    .prepared
                    .as_ref()
                    .is_some_and(|p| p["provisional"] == true);
                if provisional {
                    // The cut is shown on the held model; the exact view (final face numbers) follows.
                    self.shown_preparation = None;
                    let now = Instant::now();
                    self.live_changed = Some(
                        now.checked_sub(std::time::Duration::from_millis(400))
                            .unwrap_or(now),
                    );
                    self.status = format!(
                        "Cut drawn in {:.2} s; working out the new faces…",
                        duration.as_secs_f64()
                    );
                } else if !self.cut_added
                    && self
                        .shown_preparation
                        .as_ref()
                        .is_some_and(|p| p.cuts == self.settings.cuts)
                {
                    // The exact faces of the cuts are shown: the next cut may be made.
                    self.cut_applying = false;
                }
                if let Some(path) = saving {
                    self.status = format!("Saved {}", path.display());
                }
                if self.stage == Stage::Mesh {
                    // Finished after the switch to the mesh stage: it is the CAD stage's view.
                    self.stashed = Some(view);
                    self.stashed_alternate = None;
                    self.stashed_interior = false;
                    return;
                }
                self.wire = matches!(mode, Mode::Preview | Mode::SurfaceMesh);
                self.selected = None;
                self.view = Some(view);
                self.alternate_view = None;
                self.show_interior = false;
                self.revision += 1;
                self.stable_frames = 0;
                if mode == Mode::Preview && self.startup.mesh_size.take().is_some() {
                    self.start(Mode::SurfaceMesh);
                }
            }
            Err(error) => self.failed(format!("{error:#}")),
        }
    }

    /// A view job failed. The opened file stays the source, so new settings (the symmetry the error names, say)
    /// try it again; a view of another file is dropped, since it would be shown under this file's name.
    fn failed(&mut self, error: String) {
        self.pending_preparation = None;
        self.cut_applying = false;
        if let (Some(view), Some(source)) = (&self.view, &self.source)
            && !same_file(&view.scene.source, source)
        {
            self.view = None;
            self.alternate_view = None;
            self.show_interior = false;
            self.shown_preparation = None;
            self.selected = None;
            self.revision += 1;
        }
        self.status = "The view could not be made; the error is shown above".into();
        self.error = Some(error);
    }

    /// In a CAD view, show the prepared CAD again 0.4 s after the preparation settings last changed; a
    /// preparation still running for older settings finishes first. A converted result is left as it is.
    fn live_prepare(&mut self, ctx: &egui::Context) {
        let cad_view = self
            .view
            .as_ref()
            .is_none_or(|v| v.scene.mode == Mode::Preview);
        if self.source.is_none() || !cad_view {
            return;
        }
        if self.conversion.is_some() || self.exporting.is_some() {
            return;
        }
        let wanted = Preparation::of(&self.settings);
        match &self.live_seen {
            None => {
                // The settings at opening are the baseline: only a change starts a preparation.
                self.live_seen = Some(wanted);
                return;
            }
            Some(seen) if *seen != wanted => {
                self.live_seen = Some(wanted.clone());
                self.live_changed = Some(Instant::now());
            }
            Some(_) => {}
        }
        let Some(changed) = self.live_changed else {
            return;
        };
        let wait = std::time::Duration::from_millis(400);
        if changed.elapsed() < wait {
            ctx.request_repaint_after(wait - changed.elapsed());
            return;
        }
        if self.job.is_some() {
            if self.pending_preparation.as_ref() == Some(&wanted) {
                self.live_changed = None;
            } else {
                // The view server keeps its work (and caches): the newer settings follow when it is done.
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            return;
        }
        self.live_changed = None;
        if self.shown_preparation.as_ref() != Some(&wanted) {
            self.prepare_cad();
        }
    }

    /// Keep the view on the same place when the shown scene is replaced by another of the same file (a
    /// prepared view, a conversion, a remesh): each scene has its own normalized coordinates, so the camera
    /// is carried over through metres. A new file is fitted.
    fn carry_camera(&mut self, new: &MeshView) {
        let Some(old) = self
            .view
            .as_ref()
            .filter(|old| same_file(&old.scene.source, &new.scene.source))
        else {
            self.camera = Camera::default();
            self.fit_pending = true;
            return;
        };
        let (old_centre, old_radius) = (old.scene.centre(), old.scene.radius());
        let (new_centre, new_radius) = (new.scene.centre(), new.scene.radius());
        if !(old_radius > 0. && new_radius > 0.) {
            return;
        }
        let target = old_centre + self.camera.target.as_dvec3() * old_radius;
        self.camera.target = ((target - new_centre) / new_radius).as_vec3();
        self.camera.distance = (f64::from(self.camera.distance) * old_radius / new_radius) as f32;
    }

    /// Show `stage`: its view and the other stage's view trade places.
    pub(super) fn set_stage(&mut self, stage: Stage) {
        if stage == self.stage {
            return;
        }
        // A tab apart trades its own view; the CAD and Mesh tabs trade theirs with each other.
        if self.stage.apart() {
            let shown = self.stage;
            self.swap_slot(shown);
            self.stage = self.before_view;
        }
        if stage.apart() {
            self.before_view = self.stage;
            self.swap_slot(stage);
            self.stage = stage;
        }
        if stage == self.stage {
            self.selected = None;
            self.cut_preview = None;
            self.wire = true;
            self.revision += 1;
            return;
        }
        if let Some(new) = self.stashed.take() {
            self.carry_camera(&new);
            self.stashed = Some(new);
        }
        std::mem::swap(&mut self.view, &mut self.stashed);
        std::mem::swap(&mut self.alternate_view, &mut self.stashed_alternate);
        std::mem::swap(&mut self.show_interior, &mut self.stashed_interior);
        self.stage = stage;
        self.selected = None;
        self.cut_preview = None;
        self.wire = true;
        self.revision += 1;
    }

    /// Whether the shown view is the mesh of a conversion.
    pub(super) fn mesh_shown(&self) -> bool {
        self.stage == Stage::Mesh && self.converted.is_some()
    }

    fn busy(&self) -> bool {
        self.job.is_some() || self.conversion.is_some() || self.exporting.is_some()
    }

    /// Cancel the running inspection or conversion job; the previous scene and result stay.
    fn cancel(&mut self) {
        self.job = None;
        self.cut_applying = false;
        if let Some(task) = &self.conversion {
            task.cancel();
        }
        self.startup.mesh_size = None;
        self.status = "Cancelled. The previous view is still shown.".into();
    }

    /// The conversion request of the current settings.
    fn spec(&self) -> Option<conversion::Spec> {
        let source = self.source.clone()?;
        // The face settings keep every field across modes; the request carries only what this mode uses.
        let mut settings = self.settings.clone();
        settings.faces = settings
            .faces
            .iter()
            .map(|s| s.for_mode(self.mode))
            .filter(|s| !s.is_default())
            .collect();
        if self.mode != conversion::Mode::Ho {
            settings.potential_panel_size = None;
        }
        Some(conversion::Spec {
            schema: conversion::SCHEMA,
            source,
            mode: self.mode,
            settings,
        })
    }

    fn convert(&mut self) {
        if self.busy() {
            return;
        }
        let Some(spec) = self.spec() else {
            return;
        };
        self.log.add(format!(
            "convert {}: {}",
            self.mode.name(),
            serde_json::to_string(&spec.settings).unwrap_or_default()
        ));
        let worker = conversion::Worker {
            executable: None,
            gmsh_library: (!self.library.trim().is_empty())
                .then(|| PathBuf::from(self.library.trim())),
        };
        match Task::start(spec, worker) {
            Ok(task) => {
                self.conversion = Some(task);
                self.error = None;
                self.status = format!("Starting {} conversion…", self.mode.name());
                self.stable_frames = 0;
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    /// Keep the span lines of the shown ho result at the potential panel size of the settings: sample the
    /// views again in the background when it changes (not while a value is being dragged).
    fn follow_span(&mut self, ctx: &egui::Context) {
        if let Some(respan) = &self.respan
            && let Ok(views) = respan.views.try_recv()
        {
            let respan = self.respan.take().expect("checked");
            let current = self
                .converted
                .as_ref()
                .is_some_and(|s| Arc::ptr_eq(s, &respan.session));
            match views {
                // A result replaced meanwhile, or the CAD tab shown: dropped (sampled again when needed).
                Ok((body, with_interior)) if current && self.stage == Stage::Mesh => {
                    if self.show_interior && with_interior.is_some() {
                        self.view = with_interior;
                        self.alternate_view = Some(body);
                    } else {
                        self.view = Some(body);
                        self.alternate_view = with_interior;
                    }
                    self.shown_span = respan.span;
                    self.revision += 1;
                    self.stable_frames = 0;
                }
                Ok(_) => {}
                Err(error) => {
                    self.error = Some(error);
                    // Not tried again until the size changes.
                    self.shown_span = respan.span;
                }
            }
        }
        let Some(session) = self.converted.clone() else {
            return;
        };
        let span = self
            .settings
            .potential_panel_size
            .unwrap_or(conversion::DEFAULT_POTENTIAL_PANEL_SIZE_M);
        if self.respan.is_some()
            || !self.spans
            || self.stage != Stage::Mesh
            || session.result.mode != conversion::Mode::Ho
            || span == self.shown_span
            || ctx.input(|i| i.pointer.any_down())
        {
            if self.respan.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            return;
        }
        let (sender, views) = std::sync::mpsc::channel();
        let worker = session.clone();
        if std::thread::Builder::new()
            .name("respan".into())
            .spawn(move || {
                let _ = sender.send(worker.views_at(span).map_err(|e| format!("{e:#}")));
            })
            .is_ok()
        {
            self.status = format!("Showing the spans at {span} m");
            self.respan = Some(Respan {
                session,
                span,
                views,
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn export(&mut self, path: PathBuf) {
        let Some(session) = &self.converted else {
            return;
        };
        let panel_size = (session.result.mode == conversion::Mode::Ho)
            .then_some(self.settings.potential_panel_size)
            .flatten();
        match session::export(session.clone(), path, panel_size) {
            Ok(receiver) => {
                self.exporting = Some(receiver);
                self.error = None;
                self.status = "Writing the file…".into();
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    fn poll_conversion(&mut self) {
        let events: Vec<_> = self
            .conversion
            .as_ref()
            .map(|task| task.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Progress(progress) => {
                    self.status = format!("{} · {:.2} s", progress.stage, progress.seconds)
                }
                Event::Finished(result) => {
                    let seconds = self
                        .conversion
                        .take()
                        .map(|task| task.started.elapsed().as_secs_f64())
                        .unwrap_or_default();
                    match result {
                        Ok(ready) => {
                            self.status = format!(
                                "{} · {} body {} · {} · {seconds:.2} s",
                                ready.session.result.mode.name(),
                                ready.session.body.count(),
                                ready.session.result.mode.elements(),
                                ready.session.result.state
                            );
                            if ready
                                .session
                                .result
                                .summary
                                .get("panel_method_fallback")
                                .is_some()
                            {
                                self.status.push_str(
                                    " · The quasi-structured mesher failed on this model, so the default mesher \
                                     (mostly quads) was used",
                                );
                            }
                            self.source = Some(ready.session.result.request.source.clone());
                            // The camera follows from the view on screen: from the CAD view on the first
                            // conversion (the Mesh tab had none), from the last result on a remesh.
                            self.carry_camera(&ready.body);
                            if self.stage == Stage::Cad {
                                // The old result is replaced: no camera change through it on the tab change.
                                self.stashed = None;
                                self.stashed_alternate = None;
                                self.set_stage(Stage::Mesh);
                            }
                            self.view = Some(ready.body);
                            self.alternate_view = ready.with_interior;
                            self.shown_span = Session::span_of(&ready.session.result);
                            self.respan = None;
                            self.converted = Some(ready.session);
                            // A conversion that wrote the interior surface shows it; one without has none.
                            self.show_interior = false;
                            if self.alternate_view.is_some() {
                                std::mem::swap(&mut self.view, &mut self.alternate_view);
                                self.show_interior = true;
                            }
                            self.selected = None;
                            self.wire = true;
                            self.colors = true;
                            if let Some(failure) =
                                &self.converted.as_ref().expect("set").result.failure
                            {
                                self.error = Some(format!(
                                    "{failure}. The failed patches are red and listed first under Checks; hover \
                                     over a check to see what it means."
                                ));
                            }
                            self.revision += 1;
                            self.stable_frames = 0;
                            if let Some(path) = self.startup.export.take() {
                                self.export(path);
                            }
                        }
                        Err(error) => {
                            self.error = Some(error);
                            self.status =
                                "The conversion did not finish. The previous result is still shown.".into();
                        }
                    }
                }
            }
        }
        if let Some(result) = self
            .exporting
            .as_ref()
            .and_then(|task| task.events.try_recv().ok())
        {
            self.exporting = None;
            match result {
                Ok(files) => {
                    self.status = format!(
                        "Exported {}",
                        files
                            .iter()
                            .map(|f| f
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned())
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                }
                Err(error) => self.error = Some(error),
            }
        }
    }
}

impl eframe::App for Desktop {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.93, 0.94, 0.96, 1.]
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        self.dropped(ctx);
        self.diagnostics_screenshot(ctx);
        self.undo_keys(ctx);
        self.toolbar(ctx);
        self.about_window(ctx);
        self.model_panel(ctx);
        self.properties_panel(ctx);
        self.quality_panel(ctx);
        if std::mem::take(&mut self.cut_added) && self.job.is_none() {
            self.quick = true;
            self.prepare_cad();
        }
        if let Some((what, cuts)) = self.junction_cuts.take() {
            self.add_cuts(&what, cuts);
        }
        self.live_prepare(ctx);
        self.follow_fast();
        self.follow_span(ctx);
        let viewport = self.viewport(ctx);
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.track_history(ctx);
        self.watch_log();
        self.smoke(ctx, viewport);
    }
}

/// Whether two paths name the same file: a conversion reports its source in the full Windows form
/// (`\?\C:\...`) and a CAD view in the form it was opened with.
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_is_the_same_in_its_full_and_plain_path_forms() {
        let plain = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let full = std::fs::canonicalize(&plain).unwrap();
        assert!(super::same_file(&plain, &full));
        assert!(!super::same_file(&plain, &plain.with_file_name("LICENSE")));
    }
}

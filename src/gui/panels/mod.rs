// SPDX-License-Identifier: GPL-3.0-or-later
//! Window layout: the toolbar (actions), the settings of the tab (left), the model list with the selection
//! (right), the checks (bottom), and the viewport with what it shows and the status line.

use super::Desktop;
use cad2bem::{
    cad::scene::{Mode, Preparation},
    conversion::{
        self, Cut, FacePath, FaceSetting, PanelMethod, PatchMethod, Sheets, Surface, Symmetry,
    },
    view::viewport::{Appearance, INTERIOR_COLOUR, MeshView, WETTED_COLOUR},
};
use eframe::egui;
use glam::DVec3;

mod cuts;
mod model;
mod properties;
mod quality;
mod viewport;

use cuts::*;
pub(in crate::gui) use cuts::{CutMode, CutScope};

/// Units offered in the GUI; `file` keeps the unit written in the CAD file.
const UNITS: [&str; 6] = ["file", "m", "mm", "cm", "ft", "in"];
/// Range of the size fields in metres.
const SIZE_RANGE_M: std::ops::RangeInclusive<f64> = 0.001..=10000.;
/// The label column and the control width of the settings grids.
const LABEL_WIDTH: f32 = 96.;
const CONTROL_WIDTH: f32 = 160.;

/// A group of settings: a heading over a grid of labels (left) and controls (right), one setting a row.
fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.);
    ui.label(egui::RichText::new(title).strong());
    ui.add_space(2.);
    egui::Grid::new(("settings", title))
        .num_columns(2)
        .spacing([10., 6.])
        .min_col_width(LABEL_WIDTH)
        .show(ui, add);
}

/// The width of a control: the usual one, or what is left of a narrow panel.
fn control_width(ui: &egui::Ui) -> f32 {
    CONTROL_WIDTH.min(ui.available_width() - 4.)
}

/// A size row of a settings grid, in metres.
fn size_row(ui: &mut egui::Ui, label: &str, hover: &str, size: &mut f64) {
    ui.label(label).on_hover_text(hover);
    ui.add_sized(
        [control_width(ui), ui.spacing().interact_size.y],
        egui::DragValue::new(size)
            .speed(0.1)
            .range(SIZE_RANGE_M)
            .suffix(" m"),
    );
    ui.end_row();
}

/// A choice row of a settings grid.
fn choice_row<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    label: &str,
    hover: &str,
    value: &mut T,
    values: impl IntoIterator<Item = T>,
    name: impl Fn(T) -> &'static str,
) {
    ui.label(label).on_hover_text(hover);
    egui::ComboBox::from_id_salt(label)
        .truncate()
        .width(control_width(ui))
        .selected_text(name(*value))
        .show_ui(ui, |ui| {
            for v in values {
                ui.selectable_value(value, v, name(v));
            }
        });
    ui.end_row();
}

/// A row of a read-only table: a weak label and its value.
fn info_row(ui: &mut egui::Ui, label: &str, value: impl Into<egui::WidgetText>) {
    ui.weak(label);
    // Cut to the panel's width (in full on hover): a long value (a file name) would widen the panel.
    let value: egui::WidgetText = value.into();
    let full = value.text().to_owned();
    ui.add(egui::Label::new(value).truncate())
        .on_hover_text(full);
    ui.end_row();
}

impl Desktop {
    /// Mode of the last completed conversion shown in the viewport, if any.
    fn result_mode(&self) -> Option<conversion::Mode> {
        if self.stage == super::Stage::Fast {
            // A Fast Mesh body of spline patches shows its spans like a converted one.
            return self.fast.made.as_ref().map(|(_, made)| match made {
                super::FastMade::Patches { .. } => conversion::Mode::Ho,
                super::FastMade::Panels(_) => conversion::Mode::LoFree,
            });
        }
        if let Some(opened) = self.opened_shown() {
            return Some(if opened.patches {
                conversion::Mode::Ho
            } else {
                conversion::Mode::LoFree
            });
        }
        self.converted
            .as_ref()
            .filter(|_| self.stage == super::Stage::Mesh)
            .map(|session| session.result.mode)
    }

    /// What a group of the shown view is called: a prepared CAD face (also in a conversion, whose groups are the
    /// faces it meshed), a patch of a viewed file, or a part of a viewed panel file or a Fast Mesh body.
    fn group_word(&self) -> &'static str {
        match self.opened_shown() {
            Some(opened) if opened.patches => "Patch",
            Some(_) => "Part",
            None if self.stage == super::Stage::Fast => "Part",
            None => "Face",
        }
    }

    /// The geometry file shown in the Mesh tab, if one was opened.
    fn opened_shown(&self) -> Option<&super::OpenedFile> {
        self.opened
            .as_ref()
            .filter(|_| self.stage == super::Stage::View)
    }

    fn high_order(&self) -> bool {
        self.result_mode() == Some(conversion::Mode::Ho)
    }

    pub(super) fn toolbar(&mut self, ctx: &egui::Context) {
        let busy = self.busy();
        // Actions only; what is shown and what is happening are in the main area.
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!busy, egui::Button::new("Open…"))
                    .on_hover_text(
                        "A CAD file (STEP, IGES) to prepare and convert, or a BEM geometry file (HBG, GDF, STL) to view",
                    )
                    .clicked()
                {
                    self.open();
                }
                if ui
                    .add_enabled(
                        self.source.is_some() && !busy,
                        egui::Button::new("Save prepared CAD…"),
                    )
                    .on_hover_text(
                        "Save the prepared faces (after the cuts, symmetry and waterline) as a STEP file in metres",
                    )
                    .on_disabled_hover_text("Open a STEP or IGES file first")
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("STEP", &["step", "stp"])
                        .set_file_name("prepared.step")
                        .save_file()
                {
                    self.saving = Some(path);
                    self.prepare_cad();
                }
                // Export writes the last conversion (spline patches to HBG, panels to GDF, HBG or STL), or in the Fast
                // Mesh tab its body; not a viewed file.
                let fast = self.stage == super::Stage::Fast;
                let high_order = !fast
                    && self
                        .converted
                        .as_ref()
                        .is_some_and(|s| s.result.mode == conversion::Mode::Ho);
                let ready = if fast {
                    self.fast.made.is_some() && self.error.is_none()
                } else {
                    self.stage != super::Stage::View
                        && self
                            .converted
                            .as_ref()
                            .is_some_and(|s| s.result.failure.is_none())
                };
                if ui
                    .add_enabled(!busy && ready, egui::Button::new("Export mesh…"))
                    .on_hover_text(if fast {
                        "Save the Fast Mesh body with its interior surface in one file (HBG, GDF or STL; spline \
                         patches as HBG only)"
                    } else if high_order {
                        "Save the converted spline patches (HBG)"
                    } else {
                        "Save the converted panels (HBG, GDF or STL)"
                    })
                    .on_disabled_hover_text(
                        "Convert first. Export saves the last conversion or the Fast Mesh body; a result that \
                         failed its checks cannot be saved",
                    )
                    .clicked()
                {
                    if fast {
                        let dialog = if self.fast.request.high_order {
                            rfd::FileDialog::new().add_filter("HBG spline patches", &["hbg"])
                        } else {
                            rfd::FileDialog::new()
                                .add_filter("HBG panels", &["hbg"])
                                .add_filter("GDF panels", &["gdf"])
                                .add_filter("STL triangles", &["stl"])
                        };
                        if let Some(path) = dialog.set_file_name("body.hbg").save_file() {
                            self.export_fast(path);
                        }
                    } else {
                    let dialog = if high_order {
                        rfd::FileDialog::new()
                            .add_filter("HBG spline patches", &["hbg"])
                            .set_file_name("body.hbg")
                    } else {
                        rfd::FileDialog::new()
                            .add_filter("HBG panels", &["hbg"])
                            .add_filter("GDF panels", &["gdf"])
                            .add_filter("STL triangles", &["stl"])
                            .set_file_name("body.hbg")
                    };
                    if let Some(path) = dialog.save_file() {
                        self.export(path);
                    }
                    }
                }
                ui.separator();
                if ui
                    .add_enabled(self.history.can_undo(), egui::Button::new("Undo"))
                    .on_hover_text("Undo a settings change (Ctrl+Z)")
                    .clicked()
                {
                    self.undo();
                }
                if ui
                    .add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                    .on_hover_text("Redo (Ctrl+Y)")
                    .clicked()
                {
                    self.redo();
                }
                ui.separator();
                if ui
                    .button("Save diagnostics")
                    .on_hover_text(
                        "For reporting a problem: saves the settings, the conversion request and result, the log, \
                         a screenshot and the CAD file to a folder, and copies the folder's path",
                    )
                    .clicked()
                {
                    self.save_diagnostics(ui.ctx());
                }
                if ui
                    .button("About")
                    .on_hover_text("The version, the licence, and the licences of everything cad2bem carries")
                    .clicked()
                {
                    self.about = Some(0);
                }
            });
        });
    }

    /// The bottom line of the main area: the running job (with Cancel), the last status and any error.
    fn status_line(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let started = self
                .job
                .as_ref()
                .map(|j| j.started)
                .or(self.conversion.as_ref().map(|t| t.started));
            if let Some(started) = started {
                ui.spinner();
                ui.label(format!("{:.1} s", started.elapsed().as_secs_f64()));
                if ui.small_button("Cancel").clicked() {
                    self.cancel();
                }
            } else if self.exporting.is_some() {
                ui.spinner();
            }
            ui.label(&self.status);
        });
        if let Some(error) = &self.error {
            ui.colored_label(ERROR, error);
        }
    }

    /// What the main area shows, in a line: the kind of view and its parts.
    fn shown_kind(&self, view: &MeshView) -> String {
        if let Some(opened) = self.opened_shown() {
            return format!(
                "{} · {} {}",
                opened.summary.format,
                opened.summary.count,
                if opened.patches { "patches" } else { "panels" }
            );
        }
        if let Some((_, made)) = self
            .fast
            .made
            .as_ref()
            .filter(|_| self.stage == super::Stage::Fast)
        {
            let (count, kind) = made.count();
            return format!("Fast Mesh · {count} {kind}");
        }
        let prepared = view.scene.prepared.is_some();
        let faces = view.scene.faces.len();
        match view.scene.mode {
            Mode::Preview if prepared => format!("Prepared CAD · {faces} faces"),
            Mode::Preview => format!("CAD · {faces} faces"),
            Mode::SurfaceMesh => format!("Surface mesh, untrimmed · {faces} faces"),
            Mode::BemPanels | Mode::BemPatches => match &self.converted {
                Some(session) if self.mesh_shown() => format!(
                    "{} · {} {}",
                    mode_label(session.result.mode),
                    session.body.count(),
                    session.result.mode.elements()
                ),
                _ => format!("BEM geometry · {faces} groups"),
            },
        }
    }
}

/// Where a point (m) of a scene with `centre` and `radius` is on the screen, when it is in front.
fn to_screen(
    matrix: glam::Mat4,
    rect: egui::Rect,
    centre: DVec3,
    radius: f64,
    x: DVec3,
) -> Option<egui::Pos2> {
    let clip = matrix * ((x - centre) / radius).as_vec3().extend(1.);
    (clip.w > 1e-6).then(|| {
        egui::pos2(
            rect.left() + (clip.x / clip.w + 1.) / 2. * rect.width(),
            rect.top() + (1. - clip.y / clip.w) / 2. * rect.height(),
        )
    })
}

const ERROR: egui::Color32 = egui::Color32::from_rgb(190, 30, 30);

fn state_colour(state: &str) -> egui::Color32 {
    match state {
        "ok" => egui::Color32::from_rgb(28, 128, 56),
        "warn" => egui::Color32::from_rgb(176, 104, 0),
        _ => ERROR,
    }
}

/// What the checks of a patch that are not ok mean, and what usually fixes them.
fn check_help(check: &serde_json::Value) -> String {
    check["checks"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, s)| *s != "ok")
        .map(|(name, _)| match name.as_str() {
            "fold" => "fold: the patch turns inside out somewhere. This usually means a face was forced into one \
             patch it does not fit: cut the face, or set its Layout to Split.",
            "jacobian" => "jacobian: the patch is strongly squeezed somewhere (a sliver or a sharp corner).",
            "corner_angle" => "corner_angle: a patch corner is far from a right angle.",
            "cad_error" => "cad_error: the patch lies far from the CAD surface.",
            "gap" => "gap: a patch edge does not meet its neighbour. Edges on y = 0, x = 0 or z = 0 of a model cut 
                      there are open unless Symmetry or Cut at waterline says so (CAD tab).",
            "overlap" => "overlap: the patch overlaps another one.",
            _ => "",
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The checks of a patch that are not ok.
fn not_ok(check: &serde_json::Value) -> Vec<String> {
    check["checks"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, s)| *s != "ok")
        .map(|(name, s)| format!("{name} ({})", s.as_str().unwrap_or("?")))
        .collect()
}

/// The measures behind a patch's checks.
fn patch_numbers(check: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    if let Some(angles) = check["corner_angles"].as_array() {
        let a: Vec<f64> = angles.iter().filter_map(|x| x.as_f64()).collect();
        if !a.is_empty() {
            let lo = a.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = a.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            parts.push(format!("corners {lo:.0}°–{hi:.0}°"));
        }
    }
    if let Some(j) = check["jacobian_min_rel"].as_f64() {
        parts.push(format!("Jacobian {j:.2}"));
    }
    if let Some(e) = check["cad_error_m"].as_f64() {
        parts.push(format!("CAD error {}", metres(e)));
    }
    if let Some(g) = check["gap_m"].as_f64()
        && g > 0.
    {
        parts.push(format!("gap {}", metres(g)));
    }
    parts.join(" · ")
}

/// The scalar measures of a body check, in words.
fn details(check: &serde_json::Value) -> String {
    let Some(fields) = check.as_object() else {
        return String::new();
    };
    fields
        .iter()
        .filter(|(name, _)| name.as_str() != "state")
        .filter_map(|(name, value)| {
            let text = match value {
                serde_json::Value::Number(n) => {
                    let x = n.as_f64()?;
                    if name.ends_with("_m") {
                        metres(x)
                    } else if name.ends_with("_m3") {
                        format!("{x:.4} m³")
                    } else if name.ends_with("_m2") {
                        format!("{x:.4} m²")
                    } else if name.ends_with("_rel") {
                        relative(x)
                    } else if n.is_u64() || n.is_i64() {
                        format!("{x}")
                    } else {
                        format!("{x:.4}")
                    }
                }
                serde_json::Value::String(s) => match s.as_str() {
                    "cad_solid" => "the CAD solids".into(),
                    "winding" => "the winding number".into(),
                    other => other.replace('_', " "),
                },
                serde_json::Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
                serde_json::Value::Array(a) if a.is_empty() => return None,
                serde_json::Value::Array(a) if a.len() <= 3 => a
                    .iter()
                    .map(|v| match v.as_f64() {
                        Some(x) => format!("{x:.4}"),
                        None => v.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                serde_json::Value::Array(a) => format!("{} items", a.len()),
                _ => return None,
            };
            Some(format!("{} {text}", field_label(name)))
        })
        .take(6)
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The words for a field of a check record (the JSON names stay as they are).
fn field_label(name: &str) -> String {
    match name {
        "area_m2" => "area",
        "area_cad_m2" => "area from the CAD",
        "area_difference_rel" | "cad_difference_rel" => "differs from the CAD by",
        "volume_cad_m3" => "CAD volume",
        "volumes_m3" => "estimates (x, y, z)",
        "spread_rel" => "spread of the estimates",
        "free_edges" => "open edges",
        "hanging_edges" => "T-junction edges",
        "gap_max_m" => "largest gap",
        "waterline_gap_max_m" => "largest gap to the waterline",
        "overlaps" => "overlapping patches",
        "method" => "decided by",
        "tested_points" => "points tested",
        "sheets" => "sheets",
        "sheets_left" => "sheets left",
        "cut_waterline" => "cut at the waterline:",
        "z_max_m" => "highest z",
        other => {
            return other
                .trim_end_matches("_m3")
                .trim_end_matches("_m2")
                .trim_end_matches("_m")
                .replace('_', " ");
        }
    }
    .into()
}

/// A relative difference as a percentage (in scientific notation when below 0.001 %).
fn relative(x: f64) -> String {
    if x == 0. || (x * 100.).abs() >= 0.001 {
        format!("{:.3} %", x * 100.)
    } else {
        format!("{:.1e} %", x * 100.)
    }
}

fn metres(x: f64) -> String {
    if x.abs() >= 0.1 || x == 0. {
        format!("{x:.3} m")
    } else {
        format!("{:.3} mm", x * 1000.)
    }
}

/// The unit names shown in the GUI; the request keeps the short names.
fn unit_label(unit: &str) -> &'static str {
    match unit {
        "file" => "From file",
        "m" => "m",
        "mm" => "mm",
        "cm" => "cm",
        "ft" => "ft",
        "in" => "in",
        _ => "Other",
    }
}

/// `n` and the noun, plural unless `n` is 1 ("1 face", "3 faces", "2 patches").
pub(super) fn counted(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else if noun.ends_with("ch") || noun.ends_with('s') {
        format!("{n} {noun}es")
    } else {
        format!("{n} {noun}s")
    }
}

/// A colour swatch and its name at `at` (left top), for a legend in the view; the height it takes.
fn legend(painter: &egui::Painter, at: egui::Pos2, colour: [f32; 3], name: &str) -> f32 {
    let [r, g, b] = colour.map(|c| (c * 255.).round() as u8);
    let swatch = egui::Rect::from_min_size(at + egui::vec2(0., 2.), egui::vec2(12., 12.));
    painter.rect_filled(swatch, 2., egui::Color32::from_rgb(r, g, b));
    let text = painter.text(
        at + egui::vec2(18., 0.),
        egui::Align2::LEFT_TOP,
        name,
        egui::FontId::proportional(13.),
        egui::Color32::from_gray(60),
    );
    text.height().max(16.)
}

/// The volume of a made body against the exact one, with the three estimates and a note on hover.
fn volume_row(ui: &mut egui::Ui, volumes: [f64; 3], exact: f64, note: &str) {
    let volume = volumes.iter().sum::<f64>() / 3.;
    ui.weak("Volume");
    ui.label(format!(
        "{volume:.3} m³ ({:+.2} %)",
        100. * (volume / exact - 1.)
    ))
    .on_hover_text(format!(
        "Exact {exact:.3} m³; estimates x {:.3}, y {:.3}, z {:.3} m³ ({note})",
        volumes[0], volumes[1], volumes[2]
    ));
    ui.end_row();
}

/// The interior area of a made body against the exact waterplane.
fn area_row(ui: &mut egui::Ui, area: f64, exact: f64) {
    ui.weak("Interior area");
    ui.label(format!(
        "{area:.3} m² ({:+.2} %)",
        100. * (area / exact - 1.)
    ))
    .on_hover_text(format!("Exact {exact:.3} m²"));
    ui.end_row();
}

fn sheets_label(sheets: Sheets) -> &'static str {
    match sheets {
        Sheets::None => "Keep all",
        Sheets::Rules => "Remove plates",
        Sheets::Largest => "Largest body only",
    }
}

fn mode_label(mode: conversion::Mode) -> &'static str {
    match mode {
        conversion::Mode::LoFree => "Panels (lo-free)",
        conversion::Mode::LoStr => "Patch panels (lo-str)",
        conversion::Mode::Ho => "Spline patches (ho)",
    }
}

fn mode_help(mode: conversion::Mode) -> &'static str {
    match mode {
        conversion::Mode::LoFree => "Flat panels meshed on each CAD face (GDF, HBG or STL)",
        conversion::Mode::LoStr => "Flat panels on the spline patch grids (GDF, HBG or STL)",
        conversion::Mode::Ho => "Curved B-spline patches (HBG)",
    }
}

fn patch_method_label(method: PatchMethod) -> &'static str {
    match method {
        PatchMethod::Tri3 => "3 quads per triangle",
        PatchMethod::Qqs => "Quasi-structured",
        PatchMethod::Subdiv => "4 quads per triangle",
    }
}

fn panel_method_label(method: PanelMethod) -> &'static str {
    match method {
        PanelMethod::Recombined => "Mostly quads",
        PanelMethod::Qqs => "Quasi-structured quads",
        PanelMethod::Quads => "All quads",
        PanelMethod::Triangles => "Triangles",
    }
}

fn path_label(path: FacePath) -> &'static str {
    match path {
        FacePath::Auto => "Auto",
        FacePath::Direct => "Face parameters",
        FacePath::Single => "One patch",
        FacePath::Polygon => "Polygon (N quads)",
        FacePath::Ogrid => "O-grid (disc)",
        FacePath::Ring => "Ring (annulus)",
        FacePath::Split => "Split into quads",
    }
}

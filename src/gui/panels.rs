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

    /// Step 1: how the CAD is prepared before meshing (unit, cuts, sheets), and a view of the result.
    fn preparation_settings(&mut self, ui: &mut egui::Ui) {
        self.open_model_hint(ui);
        section(ui, "Model", |ui| {
            ui.label("Unit")
                .on_hover_text("The length unit of the numbers in the file");
            egui::ComboBox::from_id_salt("unit")
                .truncate()
                .width(control_width(ui))
                .selected_text(unit_label(&self.settings.unit))
                .show_ui(ui, |ui| {
                    for unit in UNITS {
                        ui.selectable_value(&mut self.settings.unit, unit.into(), unit_label(unit));
                    }
                });
            ui.end_row();
            choice_row(
                ui,
                "Symmetry",
                "Mesh only one side of a symmetric body; the solver mirrors it",
                &mut self.settings.symmetry,
                Symmetry::ALL,
                Symmetry::label,
            );
            ui.label("Waterline")
                .on_hover_text("Cut the model at z = 0 and keep only the part under water");
            ui.checkbox(&mut self.settings.cut_waterline, "Cut at z = 0");
            ui.end_row();
            choice_row(
                ui,
                "Extra faces",
                "Remove plates, fins or duplicate faces that are not part of the closed hull",
                &mut self.settings.remove_sheets,
                Sheets::ALL,
                sheets_label,
            );
        });
        section(ui, "Face cuts", |ui| {
            ui.label("Pipe junctions").on_hover_text("Adds the cuts that turn pipe junctions into four-sided pieces: at each pipe end that meets another body, a cut around the pipe just past the junction and cuts along the short end piece; on a wall with pipe holes, circles above, below and through each hole and lines out from them");
            let preview = self.view.as_ref().filter(|v| v.scene.mode == Mode::Preview);
            if ui
                .add_enabled(
                    preview.is_some() && !self.cut_applying,
                    egui::Button::new("Cut all"),
                )
                .clicked()
                && let Some(view) = preview
            {
                let found = super::junction::all(&view.scene);
                let ends = found.len();
                let mut cuts: Vec<Cut> = found.into_iter().flat_map(|(_, c)| c).collect();
                cuts.extend(super::junction::windows(&view.scene));
                self.junction_cuts = Some((format!("{ends} pipe faces"), cuts));
            }
            ui.end_row();
            ui.label("Cuts").on_hover_text(
                "Flat cuts that split faces into simpler pieces before meshing. Draw them with Cut in the view \
                 bar, or load a JSON file",
            );
            self.cut_file_buttons(ui);
            ui.end_row();
        });
        self.cut_list(ui);
        let wanted = Preparation::of(&self.settings);
        let shown = self.shown_preparation.clone();
        if self.source.is_some() && !self.mesh_shown() && shown.as_ref() != Some(&wanted) {
            ui.small("Updating the view…");
        }
    }

    /// The number of face cuts with their file buttons: load, save, clear.
    fn cut_file_buttons(&mut self, ui: &mut egui::Ui) {
        // Wrapped: on a narrow panel the buttons go to a second line instead of widening it.
        ui.horizontal_wrapped(|ui| {
            ui.label(match self.settings.cuts.len() {
                0 => "none".to_owned(),
                n => n.to_string(),
            });
            let free = !self.cut_applying;
            if ui
                .add_enabled(free, egui::Button::new("Load…").small())
                .on_disabled_hover_text(super::CUT_APPLYING)
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Cuts", &["json"])
                    .pick_file()
            {
                match std::fs::read_to_string(&path)
                    .map_err(anyhow::Error::from)
                    .and_then(|text| Ok(serde_json::from_str(&text)?))
                {
                    Ok(cuts) => {
                        // The same cuts start no view, so nothing to wait for.
                        self.cut_applying = cuts != self.settings.cuts;
                        self.settings.cuts = cuts;
                    }
                    Err(error) => {
                        self.error = Some(format!("{}: {error:#}", path.display()));
                    }
                }
            }
            if !self.settings.cuts.is_empty() {
                if ui.small_button("Save…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Cuts", &["json"])
                        .set_file_name("cuts.json")
                        .save_file()
                    && let Err(error) = serde_json::to_vec_pretty(&self.settings.cuts)
                        .map_err(anyhow::Error::from)
                        .and_then(|bytes| Ok(std::fs::write(&path, bytes)?))
                {
                    self.error = Some(format!("{}: {error:#}", path.display()));
                }
                if ui
                    .add_enabled(free, egui::Button::new("Clear").small())
                    .on_disabled_hover_text(super::CUT_APPLYING)
                    .clicked()
                {
                    self.settings.cuts.clear();
                    self.cut_applying = true;
                }
            }
        });
    }

    /// The face cuts, each with its place and a button that removes it.
    fn cut_list(&mut self, ui: &mut egui::Ui) {
        if !self.settings.cuts.is_empty() {
            egui::CollapsingHeader::new("Cut list")
                .id_salt("cut-list")
                .show(ui, |ui| {
                    let mut remove = None;
                    let free = !self.cut_applying;
                    for (k, cut) in self.settings.cuts.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(free, egui::Button::new("×").small())
                                .on_hover_text("Remove this cut")
                                .on_disabled_hover_text(super::CUT_APPLYING)
                                .clicked()
                            {
                                remove = Some(k);
                            }
                            let c = cut.centre;
                            let n = cut.normal;
                            let text = format!(
                                "{} at ({:.2}, {:.2}, {:.2}), normal ({:.2}, {:.2}, {:.2}){}",
                                k + 1,
                                c[0],
                                c[1],
                                c[2],
                                n[0],
                                n[1],
                                n[2],
                                if cut.on.is_some() {
                                    ", one surface"
                                } else {
                                    ""
                                }
                            );
                            // Cut to the panel's width, in full on hover: a long row would widen the panel.
                            ui.add(egui::Label::new(egui::RichText::new(&text).small()).truncate())
                                .on_hover_text(text);
                        });
                    }
                    if let Some(k) = remove {
                        self.settings.cuts.remove(k);
                        self.cut_applying = true;
                    }
                });
        }
    }

    /// A model open along y = 0, x = 0 or z = 0 (a half or quarter model, or one cut at the waterline) whose
    /// settings do not say so: its cut edges would count as gaps. Offer the settings that match.
    fn open_model_hint(&mut self, ui: &mut egui::Ui) {
        let Some(open) = self
            .view
            .as_ref()
            .filter(|v| v.scene.mode == Mode::Preview)
            .and_then(|v| v.scene.open.clone())
        else {
            return;
        };
        let symmetry = self.settings.symmetry;
        let has_y = matches!(symmetry, Symmetry::Xz | Symmetry::XzAndYz);
        let has_x = matches!(symmetry, Symmetry::Yz | Symmetry::XzAndYz);
        let need_y = open.on_y0 > 0 && !has_y;
        let need_x = open.on_x0 > 0 && !has_x;
        let need_z = open.on_z0 > 0 && !self.settings.cut_waterline;
        if !(need_y || need_x || need_z) {
            return;
        }
        let mut planes = Vec::new();
        if need_y {
            planes.push("y = 0");
        }
        if need_x {
            planes.push("x = 0");
        }
        if need_z {
            planes.push("z = 0");
        }
        ui.colored_label(
            state_colour("warn"),
            format!(
                "The model has open edges on the {} {}. It looks like part of a symmetric body, or a model already \
                 cut at the waterline; unless the matching settings are on, those edges are reported as gaps.",
                if planes.len() > 1 { "planes" } else { "plane" },
                planes.join(" and ")
            ),
        );
        if ui
            .button("Turn on the matching settings")
            .on_hover_text("Set Symmetry and Waterline to match the planes the model is open on")
            .clicked()
        {
            self.settings.symmetry = match (has_y || need_y, has_x || need_x) {
                (true, true) => Symmetry::XzAndYz,
                (true, false) => Symmetry::Xz,
                (false, true) => Symmetry::Yz,
                (false, false) => symmetry,
            };
            if need_z {
                self.settings.cut_waterline = true;
            }
        }
        ui.separator();
    }

    /// Step 2: the BEM geometry made from the prepared faces. Sizes follow the selected mode.
    fn mesh_settings(&mut self, ui: &mut egui::Ui) {
        // A result made from other CAD settings than the current ones.
        if let Some(session) = &self.converted
            && Preparation::of(&session.result.request.settings) != Preparation::of(&self.settings)
        {
            ui.colored_label(
                state_colour("warn"),
                "The CAD settings have changed since this conversion. Convert again to update the result.",
            );
        }
        let previous = self.mode;
        section(ui, "Geometry", |ui| {
            ui.label("Type").on_hover_text(mode_help(self.mode));
            egui::ComboBox::from_id_salt("mode")
                .truncate()
                .width(control_width(ui))
                .selected_text(mode_label(self.mode))
                .show_ui(ui, |ui| {
                    for mode in conversion::Mode::ALL {
                        ui.selectable_value(&mut self.mode, mode, mode_label(mode));
                    }
                });
            ui.end_row();
            if previous != self.mode {
                self.settings.adapt_sizes(self.mode);
            }
            if let Some(size) = &mut self.settings.patch_size {
                size_row(
                    ui,
                    "Patch size",
                    "The approximate size of the spline patches",
                    size,
                );
            }
            if let Some(size) = &mut self.settings.panel_size {
                size_row(ui, "Panel size", "The approximate size of the panels", size);
            }
            if self.mode == conversion::Mode::Ho {
                let default = conversion::DEFAULT_POTENTIAL_PANEL_SIZE_M;
                let mut size = self.settings.potential_panel_size.unwrap_or(default);
                size_row(
                    ui,
                    "Potential panels",
                    "Saved in the HBG file as panel_size. The solver divides each patch into spans of about this \
                     size (shown by Spans); changing it does not need a new conversion.",
                    &mut size,
                );
                // The default stays unset, as a request without the option.
                self.settings.potential_panel_size = (size != default).then_some(size);
            }
            if self.mode == conversion::Mode::LoFree {
                self.settings.patch_method = PatchMethod::Tri3;
                choice_row(
                    ui,
                    "Panel mesher",
                    "How the CAD faces are divided into panels",
                    &mut self.settings.panel_method,
                    PanelMethod::ALL,
                    panel_method_label,
                );
            } else {
                self.settings.panel_method = PanelMethod::Recombined;
                choice_row(
                    ui,
                    "Split faces",
                    "How faces with no simple four-sided layout are divided into quadrilaterals",
                    &mut self.settings.patch_method,
                    PatchMethod::ALL,
                    patch_method_label,
                );
            }
        });
        if !self.settings.cut_waterline {
            self.settings.interior = false;
        }
        section(ui, "Output", |ui| {
            ui.label("Interior surface").on_hover_text(
                "Also write the free surface at z = 0 inside the body, in the same file after the body; the solver \
                 recognizes it there",
            );
            ui.add_enabled(
                self.settings.cut_waterline,
                egui::Checkbox::new(&mut self.settings.interior, "Write"),
            )
            .on_disabled_hover_text("Turn on Waterline (Cut at z = 0) in the CAD tab first");
            ui.end_row();
            ui.label("Face settings")
                .on_hover_text("Select a face to give it its own layout, size and priority");
            ui.horizontal_wrapped(|ui| {
                ui.label(match self.settings.faces.len() {
                    0 => "none".to_owned(),
                    n => format!("{n} faces"),
                });
                if !self.settings.faces.is_empty() && ui.small_button("Clear").clicked() {
                    self.settings.faces.clear();
                }
            });
            ui.end_row();
        });
        ui.add_space(10.);
        if ui
            .add_enabled(
                self.source.is_some(),
                egui::Button::new(egui::RichText::new("Convert to BEM geometry").strong())
                    .min_size(egui::vec2(ui.available_width(), 30.)),
            )
            .on_disabled_hover_text("Open a STEP or IGES file first")
            .clicked()
        {
            self.convert();
        }
    }

    /// The settings of the selected prepared face: layout, size and priority, used by the next conversion.
    fn face_settings(&mut self, ui: &mut egui::Ui, face: i32) {
        let index = self.settings.faces.iter().position(|s| s.face == face);
        let mut setting = index.map_or_else(
            || FaceSetting {
                face,
                path: FacePath::Auto,
                patch_size: None,
                panel_size: None,
                priority: 0,
            },
            |i| self.settings.faces[i].clone(),
        );
        let lo_free = self.mode == conversion::Mode::LoFree;
        let (general, default_size) = if lo_free {
            (self.settings.panel_size, conversion::DEFAULT_PANEL_SIZE_M)
        } else {
            (self.settings.patch_size, conversion::DEFAULT_PATCH_SIZE_M)
        };
        section(ui, "Face settings (next conversion)", |ui| {
            if !lo_free {
                choice_row(
                    ui,
                    "Layout",
                    "Auto tries each layout in turn. A layout you choose that does not fit the face is replaced by \
                     a split into quadrilaterals",
                    &mut setting.path,
                    FacePath::ALL,
                    path_label,
                );
            }
            let own = if lo_free {
                &mut setting.panel_size
            } else {
                &mut setting.patch_size
            };
            let mut on = own.is_some();
            ui.checkbox(&mut on, if lo_free { "Panel size" } else { "Patch size" })
                .on_hover_text("A size for this face only; when off, the face uses the size set in the Mesh tab");
            let mut size = own.or(general).unwrap_or(default_size);
            ui.add_enabled(
                on,
                egui::DragValue::new(&mut size)
                    .speed(0.1)
                    .range(SIZE_RANGE_M)
                    .suffix(" m"),
            );
            *own = on.then_some(size);
            ui.end_row();
            ui.label("Priority").on_hover_text(
                "Where this face shares an edge with another face, the face with the higher priority decides how \
                 the edge is divided",
            );
            ui.add(egui::DragValue::new(&mut setting.priority).range(-100..=100));
            ui.end_row();
        });
        let default = setting.is_default();
        match (index, default) {
            (Some(i), true) => {
                self.settings.faces.remove(i);
            }
            (Some(i), false) => self.settings.faces[i] = setting,
            (None, false) => self.settings.faces.push(setting),
            (None, true) => {}
        }
    }

    /// What the preparation of the shown prepared CAD did, from its report record.
    fn preparation_summary(ui: &mut egui::Ui, record: &serde_json::Value) {
        let count = |key: &str| record[key].as_u64().unwrap_or(0);
        let mut lines = Vec::new();
        if count("faces_cut_at_waterline") > 0 {
            lines.push(format!(
                "{} cut at the waterline",
                counted(count("faces_cut_at_waterline") as usize, "face")
            ));
        }
        if let Some(planes) = record["symmetry_cut"]["planes"].as_array()
            && !planes.is_empty()
        {
            let names: Vec<&str> = planes.iter().filter_map(|p| p.as_str()).collect();
            lines.push(format!(
                "Cut at the symmetry plane{} {}",
                if names.len() > 1 { "s" } else { "" },
                names.join(", ")
            ));
        }
        if count("faces_dropped") > 0 {
            lines.push(format!(
                "{} removed (above water or on a cut plane)",
                counted(count("faces_dropped") as usize, "face")
            ));
        }
        if let Some(sheets) = record["cad_sheets"].as_array()
            && !sheets.is_empty()
        {
            lines.push(format!("{} removed", counted(sheets.len(), "extra face")));
        }
        if let Some(split) = record["cuts"]["faces_split"].as_array() {
            let faces: u64 = split.iter().filter_map(|s| s.as_u64()).sum();
            let unused = record["cuts"]["cuts_unused"]
                .as_array()
                .map_or(0, |u| u.len());
            lines.push(format!(
                "{} split {}{}",
                counted(split.len(), "cut"),
                counted(faces as usize, "face"),
                if unused > 0 {
                    format!(" ({unused} did not cross any face)")
                } else {
                    String::new()
                }
            ));
        }
        if lines.is_empty() {
            lines.push("Nothing cut or removed".into());
        }
        for line in lines {
            ui.small(line);
        }
    }

    pub(super) fn model_panel(&mut self, ctx: &egui::Context) {
        let busy = self.busy();
        // The user sets the width (drag the edge); the rows wrap or truncate to it, so no tab widens the panel (a
        // panel that grew with the widest row of a tab moved the view sideways on a tab change).
        egui::SidePanel::left("settings")
            .resizable(true)
            .default_width(290.)
            .width_range(250.0..=520.)
            .show(ctx, |ui| {
                // Fill the panel's width: egui keeps the width of the content, so a frame with little in it
                // (before a file is shown) would narrow the panel for good.
                ui.set_min_width(ui.available_width());
                ui.add_space(4.);
                // The two steps of the work, then the tabs apart from them.
                for row in [
                    [
                        (super::Stage::Cad, "1  CAD", "Prepare the CAD: unit, waterline, symmetry, extra faces and cuts"),
                        (super::Stage::Mesh, "2  Mesh", "Convert the prepared CAD to BEM geometry"),
                    ],
                    [
                        (
                            super::Stage::View,
                            "Mesh Viewer",
                            "View a BEM geometry file (HBG, GDF, STL), separately from the CAD and its conversion",
                        ),
                        (
                            super::Stage::Fast,
                            "Fast Mesh",
                            "Make a simple body from its dimensions: cylinder, moonpool, sphere, hemisphere or barge",
                        ),
                    ],
                ] {
                    ui.horizontal(|ui| {
                        for (stage, name, hover) in row {
                            if ui
                                .add(egui::Button::selectable(
                                    self.stage == stage,
                                    egui::RichText::new(name).strong().size(16.),
                                ))
                                .on_hover_text(hover)
                                .clicked()
                            {
                                self.set_stage(stage);
                            }
                        }
                    });
                    ui.separator();
                }
                egui::ScrollArea::vertical()
                    .id_salt("settings")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_enabled_ui(!busy, |ui| match self.stage {
                            super::Stage::Cad => self.preparation_settings(ui),
                            super::Stage::Mesh => self.mesh_settings(ui),
                            super::Stage::View => self.viewed_file(ui),
                            super::Stage::Fast => self.fast_settings(ui),
                        });
                    });
            });
    }

    /// The Fast Mesh tab: the shape and its dimensions, the panels, and what was made (against the exact values).
    fn fast_settings(&mut self, ui: &mut egui::Ui) {
        use cad2bem::engine::primitives::Shape;
        let request = &mut self.fast.request;
        section(ui, "Shape", |ui| {
            ui.label("Shape");
            egui::ComboBox::from_id_salt("fast-shape")
                .truncate()
                .width(control_width(ui))
                .selected_text(request.shape.name())
                .show_ui(ui, |ui| {
                    for shape in Shape::ALL {
                        let same = request.shape.same_kind(&shape);
                        if ui.selectable_label(same, shape.name()).clicked() && !same {
                            request.shape = shape;
                        }
                    }
                });
            ui.end_row();
            match &mut request.shape {
                Shape::Cylinder { radius, draft } => {
                    size_row(ui, "Radius", "Radius of the cylinder", radius);
                    size_row(ui, "Draft", "Depth of the bottom below z = 0", draft);
                }
                Shape::Moonpool {
                    radius,
                    inner_radius,
                    draft,
                } => {
                    size_row(ui, "Radius", "Radius of the outer wall", radius);
                    size_row(
                        ui,
                        "Moonpool radius",
                        "Radius of the hole through the cylinder",
                        inner_radius,
                    );
                    size_row(ui, "Draft", "Depth of the bottom below z = 0", draft);
                }
                Shape::Sphere { radius, depth } => {
                    size_row(ui, "Radius", "Of the sphere", radius);
                    size_row(
                        ui,
                        "Centre depth",
                        "Depth of the centre below z = 0 (must be more than the radius, so the sphere is under water)",
                        depth,
                    );
                }
                Shape::Hemisphere { radius } => {
                    size_row(
                        ui,
                        "Radius",
                        "Radius of the hemisphere (its centre is at z = 0)",
                        radius,
                    );
                }
                Shape::Barge {
                    length,
                    beam,
                    draft,
                } => {
                    size_row(ui, "Length", "Length along x", length);
                    size_row(ui, "Beam", "Width along y", beam);
                    size_row(ui, "Draft", "Depth of the bottom below z = 0", draft);
                }
            }
        });
        let submerged = matches!(request.shape, Shape::Sphere { .. });
        section(ui, "Mesh", |ui| {
            choice_row(
                ui,
                "Order",
                "Low: flat panels. High: exact curved spline patches (HBG only), which the solver divides at the \
                 potential panel size",
                &mut request.high_order,
                [false, true],
                |high| {
                    if high {
                        "High (spline patches)"
                    } else {
                        "Low (panels)"
                    }
                },
            );
            if !request.high_order {
                choice_row(
                    ui,
                    "Panel shape",
                    "Quadrilaterals (with triangles only where a disc or a sphere meets its centre or pole), or \
                     triangles only: every quadrilateral split in two along its shorter diagonal",
                    &mut request.triangles,
                    [false, true],
                    |triangles| {
                        if triangles {
                            "Triangles"
                        } else {
                            "Quadrilaterals"
                        }
                    },
                );
            }
            if request.high_order {
                size_row(
                    ui,
                    "Potential panels",
                    "Saved in the HBG file as panel_size: the solver divides each patch into spans of about this \
                     size (shown by Spans)",
                    &mut request.panel_size,
                );
            } else {
                size_row(
                    ui,
                    "Panel size",
                    "The approximate size of every panel",
                    &mut request.panel_size,
                );
            }
            choice_row(
                ui,
                "Symmetry",
                "Make one side only; the solver mirrors it",
                &mut request.symmetry,
                Symmetry::ALL,
                Symmetry::label,
            );
            ui.label("Interior surface").on_hover_text(
                "Also make the free surface at z = 0 inside the body, sharing the body's nodes along the waterline",
            );
            ui.add_enabled(
                !submerged,
                egui::Checkbox::new(&mut request.interior, "Make"),
            )
            .on_disabled_hover_text("A fully submerged body has no free surface inside it");
            ui.end_row();
        });
        if submerged {
            request.interior = false;
        }
        let Some((_, made)) = &self.fast.made else {
            return;
        };
        section(ui, "Made", |ui| match made {
            super::FastMade::Panels(made) => {
                let body = &made.body;
                let triangles = body.vertices.iter().filter(|p| p[2] == p[3]).count();
                info_row(
                    ui,
                    "Panels",
                    format!("{} ({triangles} triangles)", body.vertices.len()),
                );
                if let Ok(volumes) = body.volumes() {
                    volume_row(
                        ui,
                        volumes,
                        made.volume_exact,
                        "flat panels on a curved surface enclose slightly less than the exact shape",
                    );
                }
                if let (Some(interior), Some(exact)) = (&made.interior, made.waterplane_exact) {
                    let images = interior.symmetry.images();
                    let area: f64 = interior
                        .vertices
                        .iter()
                        .map(|p| cad2bem::engine::panels::normal(p).length() / 2.)
                        .sum::<f64>()
                        * images;
                    info_row(ui, "Interior panels", interior.vertices.len().to_string());
                    area_row(ui, area, exact);
                }
            }
            super::FastMade::Patches {
                made,
                summary,
                interior,
            } => {
                info_row(ui, "Patches", summary.count.to_string());
                if let Some(volumes) = summary.volumes {
                    volume_row(ui, volumes, made.volume_exact, "exact patches");
                }
                if let (Some(interior), Some(exact)) = (interior, made.waterplane_exact) {
                    info_row(ui, "Interior patches", interior.count.to_string());
                    if let Some(area) = interior.area {
                        area_row(ui, area, exact);
                    }
                }
            }
        });
        ui.add_space(6.);
        ui.weak(if self.fast.request.high_order {
            "Export mesh… in the toolbar saves it as HBG spline patches, with the interior surface in the same file."
        } else {
            "Export mesh… in the toolbar saves it as GDF, HBG or STL, with the interior surface in the same file."
        });
    }

    /// The Mesh Viewer tab: what the viewed file holds, its body and the interior surface found in it (its
    /// elements in z = 0).
    fn viewed_file(&mut self, ui: &mut egui::Ui) {
        let Some(opened) = &self.opened else {
            ui.add_space(6.);
            ui.label("No geometry file opened.");
            ui.weak(
                "Open an HBG, GDF or STL file (a higher-order GDF takes its spans from the SPL file beside it) with Open… or by \
                 dropping it on the window. Elements lying in z = 0 are \
                 shown as the interior surface. The CAD and Mesh tabs keep their work.",
            );
            return;
        };
        for (title, summary) in [
            ("File", Some(&opened.summary)),
            ("Interior surface", opened.interior.as_ref()),
        ] {
            let Some(s) = summary else {
                continue;
            };
            section(ui, title, |ui| {
                info_row(ui, "Name", s.name.as_str());
                info_row(ui, "Format", s.format);
                let count = if s.triangles > 0 {
                    format!("{} ({} triangles)", s.count, s.triangles)
                } else {
                    s.count.to_string()
                };
                info_row(ui, if s.patches { "Patches" } else { "Panels" }, count);
                info_row(ui, "Symmetry", s.symmetry.label());
                if s.patches {
                    let discretization = match (s.rows, s.panel_size) {
                        (true, _) if s.format.starts_with("GDF") => "from the SPL file".to_owned(),
                        (true, _) => "given in the file".to_owned(),
                        (false, Some(size)) => format!("panel_size {size} m"),
                        (false, None) => "none".to_owned(),
                    };
                    ui.weak("Discretization");
                    ui.label(discretization).on_hover_text(
                        "How the solver divides each patch. None means the program reading the file must supply \
                         it. Spans shows it.",
                    );
                    ui.end_row();
                }
                if let (Some(volumes), Some(volume)) = (s.volumes, s.volume()) {
                    ui.weak("Volume");
                    ui.label(format!("{volume:.3} m³")).on_hover_text(format!(
                        "Three estimates (from x, y and z): {:.6}, {:.6}, {:.6} m³, including the mirrored copies",
                        volumes[0], volumes[1], volumes[2]
                    ));
                    ui.end_row();
                }
                if let Some(area) = s.area {
                    info_row(ui, "Area", format!("{area:.3} m²"));
                }
                if let Some([ok, warn, fail]) = s.checks {
                    ui.weak("Patch checks");
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{ok} ok"));
                        ui.colored_label(state_colour("warn"), format!("{warn} warn"));
                        ui.colored_label(state_colour("fail"), format!("{fail} fail"));
                    })
                    .response
                    .on_hover_text(
                        "The fold, Jacobian and corner-angle checks, as in a conversion",
                    );
                    ui.end_row();
                }
            });
        }
        if opened.interior.is_none() {
            ui.add_space(4.);
            ui.weak("No interior surface in the file (no elements lie in z = 0).");
        }
    }

    /// The right panel: the faces (patches, groups) of the shown view, and the selected one with its settings.
    pub(super) fn properties_panel(&mut self, ctx: &egui::Context) {
        let converted = self.mesh_shown();
        let high_order = self.high_order();
        egui::SidePanel::right("model")
            .resizable(true)
            .default_width(260.)
            .width_range(200.0..=520.)
            .show(ctx, |ui| {
                // Fill the panel's width (see the left panel).
                ui.set_min_width(ui.available_width());
                ui.add_space(4.);
                ui.strong("Model");
                let Some(view) = &self.view else {
                    ui.weak(match (&self.source, self.stage) {
                        (_, super::Stage::View) => "No geometry file opened",
                        (Some(_), _) if self.error.is_some() => {
                            "This file could not be shown. Change the settings to try again."
                        }
                        (Some(_), super::Stage::Mesh) => "Not converted yet",
                        (Some(_), _) => "Reading…",
                        (None, _) => "No CAD file opened",
                    });
                    return;
                };
                if let Some(record) = view.scene.prepared.as_ref().filter(|_| !converted) {
                    Self::preparation_summary(ui, record);
                }
                let kind = self.group_word();
                ui.add_space(2.);
                egui::ScrollArea::vertical()
                    .id_salt("faces")
                    .max_height(ui.available_height() * 0.45)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for face in &view.scene.faces {
                            let label = format!("{kind} {}   {}", face.tag, face.kind);
                            if ui
                                .add(
                                    egui::Button::selectable(
                                        self.selected == Some(face.tag),
                                        label,
                                    )
                                    .truncate(),
                                )
                                .clicked()
                            {
                                self.selected = Some(face.tag);
                            }
                        }
                    });
                ui.separator();
                ui.strong("Selection");
                if let Some(face) = view
                    .scene
                    .faces
                    .iter()
                    .find(|face| Some(face.tag) == self.selected)
                {
                    let prepared = converted || view.scene.prepared.is_some();
                    let name = match (self.opened_shown(), prepared) {
                        (Some(_), _) => kind,
                        (None, _) if self.stage == super::Stage::Fast => kind,
                        (None, true) => "Prepared face",
                        (None, false) => "CAD face",
                    };
                    egui::Grid::new("selection")
                        .num_columns(2)
                        .spacing([10., 4.])
                        .min_col_width(70.)
                        .show(ui, |ui| {
                            info_row(ui, name, face.tag.to_string());
                            info_row(ui, "Type", face.kind.as_str());
                            ui.weak("Area");
                            ui.label(format!("{:.4} m²", face.area_m2)).on_hover_text(
                                if high_order {
                                    "Area of the displayed surface (sampled)"
                                } else if converted {
                                    "Area of the panels"
                                } else {
                                    "Area of the displayed triangles"
                                },
                            );
                            ui.end_row();
                        });
                    if let Some(session) = self.converted.as_ref().filter(|_| converted) {
                        // The patches of this face that are not ok, with their checks.
                        for (check, _) in session
                            .result
                            .patch_checks
                            .iter()
                            .zip(&session.result.body_faces)
                            .filter(|(c, f)| **f == face.tag && c["state"] != "ok")
                        {
                            ui.small(format!(
                                "{}: {}",
                                check["name"].as_str().unwrap_or("patch"),
                                not_ok(check).join(", ")
                            ));
                        }
                    }
                    if self.stage == super::Stage::Cad && view.scene.mode == Mode::Preview {
                        match super::junction::cuts(&view.scene, face.tag) {
                            Ok(cuts) => {
                                if ui
                                    .add_enabled(
                                        !self.cut_applying,
                                        egui::Button::new("Cut junction"),
                                    )
                                    .on_hover_text(format!(
                                        "{} cuts: one around the pipe just past the junction, and cuts along its \
                                         end piece",
                                        cuts.len()
                                    ))
                                    .clicked()
                                {
                                    self.junction_cuts = Some((format!("face {}", face.tag), cuts));
                                }
                            }
                            Err(why) => {
                                ui.add_enabled(false, egui::Button::new("Cut junction"))
                                    .on_disabled_hover_text(why);
                            }
                        }
                    }
                    if ui.small_button("Clear selection").clicked() {
                        self.selected = None;
                    }
                } else {
                    ui.weak("Click a surface in the view or in the list above.");
                }
                let bounds = view.scene.bounds_m;
                // Face settings belong to the mesh stage: the prepared faces of a converted result (not the
                // records of a viewed file).
                let prepared_face = self.selected.filter(|&tag| {
                    self.view.as_ref().is_some_and(|v| {
                        self.stage == super::Stage::Mesh
                            && self.opened_shown().is_none()
                            && v.scene
                                .faces
                                .iter()
                                .any(|x| x.tag == tag && x.kind != "Interior surface")
                    })
                });
                if let Some(face) = prepared_face {
                    self.face_settings(ui, face);
                }
                ui.add_space(6.);
                ui.label(egui::RichText::new("Extent").strong());
                ui.add_space(2.);
                // A narrow label column: the labels are x, y and z.
                egui::Grid::new("extent").num_columns(2).spacing([10., 6.]).min_col_width(12.).show(ui, |ui| {
                    for (i, axis) in ["x", "y", "z"].into_iter().enumerate() {
                        info_row(
                            ui,
                            axis,
                            format!("{:.3} … {:.3} m", bounds[i], bounds[i + 3]),
                        );
                    }
                });
                #[cfg(not(feature = "static-gmsh"))]
                self.sdk_settings(ui);
            });
    }

    /// Development builds load a Gmsh SDK; the integrated build has none to choose.
    #[cfg(not(feature = "static-gmsh"))]
    fn sdk_settings(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        ui.separator();
        ui.collapsing("Development: Gmsh SDK", |ui| {
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut self.library).hint_text("SDK shared library path"),
            );
            if ui
                .add_enabled(!busy, egui::Button::new("Choose SDK library…"))
                .clicked()
                && let Some(path) = rfd::FileDialog::new().pick_file()
            {
                self.library = path.to_string_lossy().into_owned();
            }
        });
    }

    /// View buttons and display switches above the viewport.
    fn view_toolbar(&mut self, ui: &mut egui::Ui) {
        let converted = self.mesh_shown();
        let high_order = self.high_order();
        let surface_mesh = self
            .view
            .as_ref()
            .is_some_and(|v| v.scene.mode == Mode::SurfaceMesh);
        // Wrapped: a row wider than the viewport made the panel wider than the window, and the view was drawn
        // shifted, squeezed and out of step with picking.
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Fit")
                .on_hover_text("Fit the model in view")
                .clicked()
            {
                self.fit_pending = true;
            }
            for (name, yaw, pitch) in [
                ("Iso", -0.8, 0.45),
                ("Front", -std::f32::consts::FRAC_PI_2, 0.),
                ("Side", 0., 0.),
                ("Top", -std::f32::consts::FRAC_PI_2, 1.55),
            ] {
                if ui.button(name).clicked() {
                    self.camera = Default::default();
                    self.camera.yaw = yaw;
                    self.camera.pitch = pitch;
                    self.fit_pending = true;
                }
            }
            ui.separator();
            let edges = if high_order {
                "Patch edges"
            } else if converted || self.stage.apart() {
                "Panel edges"
            } else if surface_mesh {
                "Triangle edges"
            } else {
                "Face edges"
            };
            ui.checkbox(&mut self.wire, edges);
            if surface_mesh {
                ui.checkbox(&mut self.colors, "Quality colours");
            } else if high_order {
                ui.checkbox(&mut self.spans, "Spans").on_hover_text(
                    "Show how the solver divides each patch (at the potential panel size of the Mesh tab)",
                );
                ui.checkbox(&mut self.colors, "Check colours")
                    .on_hover_text("Colour patches whose checks warn (orange) or fail (red)");
                if self.colors {
                    ui.colored_label(state_colour("warn"), "warn");
                    ui.colored_label(state_colour("fail"), "fail");
                }
            }
            if self.alternate_view.is_some()
                && ui
                    .checkbox(&mut self.show_interior, "Interior surface")
                    .changed()
            {
                std::mem::swap(&mut self.view, &mut self.alternate_view);
                self.revision += 1;
                self.selected = None;
            }
            let cad_view = self
                .view
                .as_ref()
                .is_some_and(|v| v.scene.mode == Mode::Preview);
            if self.stage == super::Stage::Cad && cad_view {
                ui.separator();
                ui.label("Cut");
                ui.selectable_value(&mut self.cut_mode, None, "Off");
                ui.selectable_value(&mut self.cut_mode, Some(CutMode::Across), "Across")
                    .on_hover_text("Cut around a cylinder, or horizontally across a wall");
                ui.selectable_value(&mut self.cut_mode, Some(CutMode::Along), "Along")
                    .on_hover_text("Cut along a cylinder's axis, or vertically across a wall");
                ui.selectable_value(&mut self.cut_mode, Some(CutMode::Points), "Two points")
                    .on_hover_text(
                        "Click two points on a face: the cut runs through both, at right angles to the face",
                    );
                ui.selectable_value(&mut self.cut_mode, Some(CutMode::Parallel), "Parallel")
                    .on_hover_text(
                        "Click a reference face (a plane or a cylinder), then click where to cut: the cut runs \
                         parallel to the reference",
                    );
                if self.cut_mode.is_some() {
                    ui.separator();
                    for (scope, name, hover) in [
                        (CutScope::Face, "Face", "Cut only the face under the mouse"),
                        (
                            CutScope::Surface,
                            "Surface",
                            "Cut every face that lies on the same plane or cylinder",
                        ),
                        (CutScope::All, "All", "Cut every face the cut crosses"),
                    ] {
                        ui.selectable_value(&mut self.cut_scope, scope, name)
                            .on_hover_text(hover);
                    }
                }
            } else {
                self.cut_mode = None;
            }
            ui.separator();
            ui.weak(if self.cut_mode.is_some() {
                "Move over a face to see the cut in red · Click: add the cut · Esc: stop cutting"
            } else {
                "Drag: rotate · Right-drag: pan · Wheel: zoom · Click: select"
            });
        });
    }

    pub(super) fn quality_panel(&mut self, ctx: &egui::Context) {
        // A viewed file's checks, and a Fast Mesh body's volume, are in the tab's own table.
        if self.stage.apart() {
            return;
        }
        egui::TopBottomPanel::bottom("quality")
            .min_height(120.)
            .default_height(200.)
            .resizable(true)
            .show(ctx, |ui| {
                let Some(session) = self
                    .converted
                    .clone()
                    .filter(|_| self.stage == super::Stage::Mesh)
                else {
                    ui.horizontal(|ui| {
                        ui.heading("Checks");
                        ui.weak(if self.view.is_some() {
                            "Convert to see the checks of the result."
                        } else {
                            "Open a STEP or IGES model."
                        });
                    });
                    if self
                        .view
                        .as_ref()
                        .is_some_and(|v| v.scene.mode == Mode::SurfaceMesh)
                    {
                        self.mesh_warnings(ui);
                    }
                    return;
                };
                let result = &session.result;
                let failing: Vec<(usize, &serde_json::Value)> = result
                    .patch_checks
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c["state"] != "ok")
                    .collect();
                ui.horizontal(|ui| {
                    ui.heading("Checks");
                    ui.colored_label(state_colour(&result.state), result.state.to_uppercase());
                    ui.label(format!(
                        "{} {} · {}",
                        session.body.count(),
                        result.mode.elements(),
                        result.mode.name()
                    ));
                    if !result.patch_checks.is_empty() {
                        ui.label(format!("· {} with warnings or failures", counted(failing.len(), "patch")));
                    }
                });
                if result.failure.is_some() {
                    ui.colored_label(
                        ERROR,
                        "The checks failed, so this result cannot be exported. Failed patches are red in the view \
                         and listed first below. Click one to select its face, then cut that face (CAD tab) or \
                         change its Layout (face settings) and convert again.",
                    );
                }
                if result.cad["patch_method_used"] == "tri3" {
                    ui.colored_label(
                        state_colour("warn"),
                        "The chosen split method produced folded patches, so the split faces were remade with 3 \
                         quads per triangle",
                    );
                }
                let failed: Vec<String> = result.cad["face_settings"]["forced_path_failed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|f| f.to_string())
                    .collect();
                if !failed.is_empty() {
                    ui.colored_label(
                        state_colour("warn"),
                        format!(
                            "The layout chosen for face {} did not fit, so the face was split instead",
                            failed.join(", ")
                        ),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt("checks")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("body-checks")
                            .striped(true)
                            .num_columns(3)
                            .spacing([16., 4.])
                            .show(ui, |ui| {
                                let mut rows: Vec<(String, &serde_json::Value)> = result
                                    .body
                                    .as_object()
                                    .into_iter()
                                    .flatten()
                                    .map(|(name, check)| (format!("Body {name}"), check))
                                    .collect();
                                if result.summary["interior"].is_object() {
                                    rows.push((
                                        "Interior surface".into(),
                                        &result.summary["interior"],
                                    ));
                                }
                                for (name, check) in rows {
                                    let state = check["state"].as_str().unwrap_or("?");
                                    ui.label(name);
                                    ui.colored_label(state_colour(state), state);
                                    ui.label(details(check));
                                    ui.end_row();
                                }
                            });
                        if failing.is_empty() {
                            return;
                        }
                        ui.add_space(6.);
                        ui.strong("Patches with warnings or failures (click one to select its face)");
                        let mut rows = failing;
                        rows.sort_by_key(|(_, c)| c["state"] != "fail");
                        egui::Grid::new("patch-checks")
                            .striped(true)
                            .num_columns(4)
                            .spacing([16., 2.])
                            .show(ui, |ui| {
                                for (index, check) in rows.into_iter().take(500) {
                                    let face = result.body_faces.get(index).copied();
                                    let name = check["name"].as_str().unwrap_or("patch");
                                    if ui
                                        .selectable_label(
                                            face.is_some() && self.selected == face,
                                            name,
                                        )
                                        .clicked()
                                    {
                                        self.selected = face;
                                    }
                                    let state = check["state"].as_str().unwrap_or("?");
                                    ui.colored_label(state_colour(state), state);
                                    ui.label(not_ok(check).join(", "))
                                        .on_hover_text(check_help(check));
                                    ui.label(patch_numbers(check));
                                    ui.end_row();
                                }
                            });
                    });
            });
    }

    /// Triangle warnings of an untrimmed surface mesh (`gui --mesh-size`); not BEM quality.
    fn mesh_warnings(&mut self, ui: &mut egui::Ui) {
        let Some(view) = &self.view else {
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("warnings")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(format!(
                    "{} of {} triangles have an angle below 20°",
                    view.warnings.len(),
                    view.scene.triangles.len()
                ));
                for &(index, quality) in view.warnings.iter().take(100) {
                    let face = view.scene.face_tags[index];
                    let label = format!(
                        "Face {face} · triangle {} · min angle {:.1}° · aspect {:.2}",
                        index + 1,
                        quality.min_angle,
                        quality.aspect
                    );
                    if ui
                        .selectable_label(self.selected == Some(face), label)
                        .clicked()
                    {
                        self.selected = Some(face);
                    }
                }
            });
    }

    /// Draw the scene and handle camera and picking input. Returns the viewport rectangle.
    pub(super) fn viewport(&mut self, ctx: &egui::Context) -> egui::Rect {
        let mut viewport = egui::Rect::NOTHING;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                // The status line under the view: the running job, the last status and any error.
                let status = egui::TopBottomPanel::bottom("status")
                    .frame(
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(8, 4))
                            .fill(ui.visuals().panel_fill),
                    )
                    .show_inside(ui, |ui| self.status_line(ui));
                let status_top = status.response.rect.top();
                if self.view.is_some() {
                    // The bar keeps two rows in every tab and cut mode: a bar of another height changed the view's
                    // size, and with it the zoom, when the tab changed.
                    let rows = 2. * (ui.spacing().interact_size.y + ui.spacing().item_spacing.y) + 8.;
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .fill(ui.visuals().panel_fill)
                        .show(ui, |ui| {
                            ui.set_min_height(rows - 8.);
                            self.view_toolbar(ui)
                        });
                }
                // Exactly the visible rest of the panel (above the status line): the camera, the drawing and
                // picking share this rectangle.
                let clip = ui.clip_rect();
                let rect = egui::Rect::from_min_max(
                    ui.cursor().min,
                    egui::pos2(clip.max.x, clip.max.y.min(status_top)),
                );
                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                viewport = rect;
                let Some(view) = &self.view else {
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if self.stage == super::Stage::View {
                            "Open an HBG, GDF or STL file, or drop one here"
                        } else if self.stage == super::Stage::Fast {
                            "Set the dimensions on the left"
                        } else if self.stage == super::Stage::Mesh && self.source.is_some() {
                            "Convert to see the BEM geometry"
                        } else {
                            "Open a STEP or IGES model, or drop one here"
                        },
                        egui::FontId::proportional(22.),
                        egui::Color32::GRAY,
                    );
                    return;
                };
                if rect.width() <= 1. || rect.height() <= 1. {
                    return;
                }
                if self.fit_pending {
                    self.camera.fit(view, rect.aspect_ratio());
                    self.fit_pending = false;
                }
                self.camera.interact(&response, view);
                self.cut_preview = None;
                self.cut_snap = None;
                self.cut_face = None;
                if !matches!(self.cut_mode, Some(CutMode::Points | CutMode::Parallel))
                    || self.cut_first_mode != self.cut_mode
                {
                    self.cut_first = None;
                }
                if self.cut_mode.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    // Esc drops a first point, then leaves the cut mode.
                    if self.cut_first.take().is_none() {
                        self.cut_mode = None;
                    }
                }
                let mut hovered_point = None;
                // The face under the mouse is drawn lighter, as in a CAD program (not while dragging the view).
                let hovered = response
                    .hover_pos()
                    .or_else(|| {
                        (self.startup.smoke && self.cut_mode.is_some()).then(|| rect.center())
                    })
                    .filter(|_| !response.dragged())
                    .and_then(|mouse| {
                        let (origin, direction) = self.camera.ray(rect, mouse);
                        view.hit(origin, direction).map(|(_, face)| face)
                    });
                let cutting = self.cut_mode.filter(|_| view.scene.mode == Mode::Preview);
                if let Some(mode) = cutting
                    && let Some(mouse) = response
                        .hover_pos()
                        .or_else(|| self.startup.smoke.then(|| rect.center()))
                {
                    let (origin, direction) = self.camera.ray(rect, mouse);
                    if let Some((p, tag)) = view.hit(origin, direction) {
                        let scene = &view.scene;
                        let (centre, radius) = (scene.centre(), scene.radius());
                        let hit = centre + p.as_dvec3() * radius;
                        let mut point = hit;
                        let face = scene.faces.iter().find(|f| f.tag == tag);
                        // Snap to a CAD corner of the face within 12 px.
                        let matrix = self.camera.matrix(rect.aspect_ratio());
                        if let Some(face) = face {
                            let near = face
                                .corners
                                .iter()
                                .map(|&c| DVec3::from(c))
                                .filter_map(|c| {
                                    Some((
                                        c,
                                        to_screen(matrix, rect, centre, radius, c)?.distance(mouse),
                                    ))
                                })
                                .filter(|(_, d)| *d < 12.)
                                .min_by(|a, b| a.1.total_cmp(&b.1));
                            if let Some((c, _)) = near {
                                point = c;
                                self.cut_snap = Some(c.to_array());
                            }
                        }
                        let extent: Vec<DVec3> = scene
                            .triangles
                            .iter()
                            .zip(&scene.face_tags)
                            .filter(|(_, t)| **t == tag)
                            .flat_map(|(t, _)| {
                                t.map(|i| DVec3::from(scene.positions_m[i as usize]))
                            })
                            .collect();
                        hovered_point = Some((point, hit, tag));
                        let made = if mode == CutMode::Parallel {
                            // Parallel to the reference face, through the point on the face to cut.
                            self.cut_first.and_then(|_| {
                                parallel_cut(
                                    DVec3::from(self.cut_normal),
                                    point,
                                    face.and_then(|f| f.surface.as_ref()),
                                    &extent,
                                    2. * radius,
                                    self.cut_scope != CutScope::All,
                                )
                            })
                        } else if mode == CutMode::Points {
                            // The second point: the cut through both, square to the first point's face.
                            self.cut_first.and_then(|(a, a_hit, a_tag)| {
                                let a_face = scene.faces.iter().find(|f| f.tag == a_tag);
                                let a_extent: Vec<DVec3> = scene
                                    .triangles
                                    .iter()
                                    .zip(&scene.face_tags)
                                    .filter(|(_, t)| **t == a_tag)
                                    .flat_map(|(t, _)| {
                                        t.map(|i| DVec3::from(scene.positions_m[i as usize]))
                                    })
                                    .collect();
                                points_cut(
                                    DVec3::from(a),
                                    point,
                                    a_face.and_then(|f| f.surface.as_ref()),
                                    &a_extent,
                                    2. * radius,
                                    self.cut_scope != CutScope::All,
                                    direction.as_dvec3(),
                                )
                                .map(|mut cut| {
                                    if self.cut_scope == CutScope::Face {
                                        cut.face_at = Some(a_hit);
                                        self.cut_face = Some(a_tag);
                                    }
                                    cut
                                })
                            })
                        } else {
                            make_cut(
                                mode,
                                face.and_then(|f| f.surface.as_ref()),
                                point,
                                &extent,
                                2. * radius,
                                self.cut_scope != CutScope::All,
                            )
                        };
                        self.cut_preview = made.map(|mut cut| {
                            // The hit point, not a snapped corner (a corner is on several faces); a two-point cut
                            // keeps the face of its first point, set above.
                            if self.cut_scope == CutScope::Face && mode != CutMode::Points {
                                cut.face_at = Some(hit.to_array());
                                self.cut_face = Some(tag);
                            }
                            cut
                        });
                    }
                }
                if cutting.is_some() {
                    if response.clicked()
                        && matches!(cutting, Some(CutMode::Points | CutMode::Parallel))
                        && self.cut_first.is_none()
                        && let Some((point, hit, tag)) = hovered_point
                    {
                        let surface = view
                            .scene
                            .faces
                            .iter()
                            .find(|f| f.tag == tag)
                            .and_then(|f| f.surface.as_ref());
                        let normal = surface.and_then(|s| surface_normal(s, hit));
                        if cutting == Some(CutMode::Parallel) && normal.is_none() {
                            self.status = "The reference must be a flat or a cylindrical face".into();
                        } else {
                            self.cut_first = Some((point.to_array(), hit.to_array(), tag));
                            self.cut_first_mode = cutting;
                            if cutting == Some(CutMode::Parallel)
                                && let Some(n) = normal
                            {
                                self.cut_normal = n.to_array();
                                // The reference face is shown selected while cuts parallel to it are made.
                                self.selected = Some(tag);
                                self.status = format!("Reference: face {tag}. Now click where to cut (Esc picks another reference)");
                            }
                        }
                    } else if response.clicked() && self.cut_applying {
                        self.status = super::CUT_APPLYING.into();
                    } else if response.clicked()
                        && let Some(cut) = self.cut_preview.clone()
                    {
                        // A two-point cut starts again; the reference of parallel cuts stays for the next one.
                        if cutting == Some(CutMode::Points) {
                            self.cut_first = None;
                        }
                        self.log.add(format!(
                            "cut added: {}",
                            serde_json::to_string(&cut).unwrap_or_default()
                        ));
                        self.settings.cuts.push(cut);
                        self.cut_added = true;
                        self.cut_applying = true;
                        if !self.settings.faces.is_empty() {
                            // Cuts renumber the prepared faces.
                            self.settings.faces.clear();
                            self.status =
                                "The face settings were cleared, because a cut renumbers the faces".into();
                        }
                    }
                } else if response.clicked()
                    && let Some(point) = response.interact_pointer_pos()
                {
                    let (origin, direction) = self.camera.ray(rect, point);
                    let hit = view.hit(origin, direction);
                    self.selected = hit.map(|(_, face)| face);
                    // The clicked point in metres, for a cut through it.
                    self.picked_point = hit.map(|(p, _)| {
                        let centre = view.scene.centre();
                        let radius = view.scene.radius();
                        (centre + p.as_dvec3() * radius).to_array()
                    });
                }
                view.paint(
                    ui,
                    rect,
                    &self.camera,
                    self.revision,
                    Appearance {
                        selected: self.selected,
                        hovered,
                        wire: self.wire,
                        quality: self.colors,
                        spans: self.spans && self.high_order(),
                    },
                );
                self.camera.overlay(ui.painter(), rect, view);
                // What is shown, in the top left corner: the file and the kind of view.
                let corner = rect.left_top() + egui::vec2(12., 10.);
                let name = view
                    .scene
                    .source
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let painter = ui.painter().with_clip_rect(rect);
                let ink = egui::Color32::from_gray(40);
                let title = painter.text(
                    corner,
                    egui::Align2::LEFT_TOP,
                    name,
                    egui::FontId::proportional(17.),
                    ink,
                );
                let kind = painter.text(
                    title.left_bottom() + egui::vec2(0., 2.),
                    egui::Align2::LEFT_TOP,
                    self.shown_kind(view),
                    egui::FontId::proportional(13.),
                    egui::Color32::from_gray(90),
                );
                if self.show_interior {
                    // The legend of the two surfaces' colours, under the kind of view.
                    let mut at = kind.left_bottom() + egui::vec2(0., 6.);
                    for (colour, name) in [
                        (WETTED_COLOUR, "Wetted body"),
                        (INTERIOR_COLOUR, "Interior surface"),
                    ] {
                        at.y += legend(&painter, at, colour, name) + 3.;
                    }
                }
                // The name of the face under the mouse next to it.
                if let (Some(face), Some(mouse)) = (hovered, response.hover_pos())
                    && let Some(info) = view.scene.faces.iter().find(|f| f.tag == face)
                {
                    let text = format!("{} {} · {}", self.group_word(), info.tag, info.kind);
                    let painter = ui.painter().with_clip_rect(rect);
                    let galley = painter.layout_no_wrap(
                        text,
                        egui::FontId::proportional(13.),
                        egui::Color32::from_gray(30),
                    );
                    let at = mouse + egui::vec2(16., 16.);
                    let frame = egui::Rect::from_min_size(at, galley.size()).expand(4.);
                    painter.rect_filled(
                        frame,
                        3.,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 230),
                    );
                    painter.galley(at, galley, egui::Color32::from_gray(30));
                }
                if let Some(cut) = &self.cut_preview {
                    // The cut line on the surface in red, drawn over the model.
                    let (centre, radius) = (view.scene.centre(), view.scene.radius());
                    let matrix = self.camera.matrix(rect.aspect_ratio());
                    let screen = |x: DVec3| to_screen(matrix, rect, centre, radius, x);
                    let red = egui::Stroke::new(2.5_f32, egui::Color32::from_rgb(220, 30, 30));
                    let painter = ui.painter().with_clip_rect(rect);
                    for [a, b] in cut_segments(cut, &view.scene, self.cut_face) {
                        if let (Some(a), Some(b)) = (screen(a), screen(b)) {
                            painter.line_segment([a, b], red);
                        }
                    }
                    if let Some(p) = self.cut_snap.and_then(|c| screen(DVec3::from(c))) {
                        painter.circle_stroke(p, 6., red);
                    }
                }
                // The first point of a two-point cut, and the corner snapped to before it.
                if self.cut_mode == Some(CutMode::Points) {
                    let (centre, radius) = (view.scene.centre(), view.scene.radius());
                    let matrix = self.camera.matrix(rect.aspect_ratio());
                    let painter = ui.painter().with_clip_rect(rect);
                    let red = egui::Color32::from_rgb(220, 30, 30);
                    if let Some(p) = self.cut_first.and_then(|(a, _, _)| {
                        to_screen(matrix, rect, centre, radius, DVec3::from(a))
                    }) {
                        painter.circle_filled(p, 5., red);
                    }
                    if self.cut_first.is_none()
                        && let Some(p) = self
                            .cut_snap
                            .and_then(|c| to_screen(matrix, rect, centre, radius, DVec3::from(c)))
                    {
                        painter.circle_stroke(p, 6., egui::Stroke::new(2.5_f32, red));
                    }
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                }
            });
        viewport
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

/// The corners of a cut's rectangle (m).
#[cfg(test)]
fn cut_corners(cut: &Cut) -> [DVec3; 4] {
    let n = DVec3::from(cut.normal).normalize();
    let u = DVec3::from(cut.axis).normalize();
    let v = n.cross(u);
    let c = DVec3::from(cut.centre);
    [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
        .map(|(a, b)| c + a * cut.half[0] * u + b * cut.half[1] * v)
}

/// Which faces a cut of the cut mode splits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CutScope {
    /// The face under the mouse.
    Face,
    /// The faces on its plane or cylinder.
    Surface,
    /// Every face the cut passes.
    All,
}

/// How the cut line of the cut mode runs on the face under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CutMode {
    /// Around a cylinder (across its axis); horizontal on a wall, along y on a deck.
    Across,
    /// Along a cylinder's axis (a half plane from the axis); vertical on a wall, along x on a deck.
    Along,
    /// Through two clicked points, square to the face at the first.
    Points,
    /// Parallel to a reference face clicked first (its plane, or the tangent plane where a cylinder was
    /// clicked), through the point clicked on the face to cut.
    Parallel,
}

/// The cut through `point` (m) of a face on `surface` (or of unknown surface), `extent` its points (m),
/// `size` the model size; limited to the surface when `on`.
fn make_cut(
    mode: CutMode,
    surface: Option<&Surface>,
    point: DVec3,
    extent: &[DVec3],
    size: f64,
    on: bool,
) -> Option<Cut> {
    let make = |normal: DVec3, axis: DVec3, centre: DVec3, half: [f64; 2]| Cut {
        centre: centre.to_array(),
        normal: normal.to_array(),
        axis: axis.to_array(),
        half,
        on: surface.filter(|_| on).cloned(),
        face_at: None,
    };
    // A plane through the point containing the line direction `d` and the face normal `n`: as large as the
    // face (a little more) when the cut keeps to its surface, otherwise across the model.
    let line = |d: DVec3, n: DVec3| {
        let normal = d.cross(n).try_normalize()?;
        let other = normal.cross(d);
        let reach = |e: DVec3| {
            extent
                .iter()
                .map(|x| (*x - point).dot(e).abs())
                .fold(0., f64::max)
                * 1.05
                + 1e-3 * size
        };
        let half = if on && surface.is_some() && !extent.is_empty() {
            [reach(d), reach(other)]
        } else {
            [size, size]
        };
        Some(make(normal, d, point, half))
    };
    match surface {
        Some(Surface::Cylinder {
            point: p0,
            axis,
            radius,
        }) => {
            let a = DVec3::from(*axis).normalize();
            let base = DVec3::from(*p0);
            match mode {
                CutMode::Across => {
                    let centre = base + (point - base).dot(a) * a;
                    let u = a.any_orthonormal_vector();
                    Some(make(a, u, centre, [1.5 * radius, 1.5 * radius]))
                }
                CutMode::Along => {
                    let off = point - base;
                    let r = (off - off.dot(a) * a).try_normalize()?;
                    // Over the face's length along the axis, so that the cut ends where the face ends.
                    let (lo, hi) = extent
                        .iter()
                        .map(|x| (*x - base).dot(a))
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), t| {
                            (l.min(t), h.max(t))
                        });
                    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
                        return None;
                    }
                    let centre = base + 0.5 * (lo + hi) * a + 0.75 * radius * r;
                    Some(make(
                        a.cross(r),
                        a,
                        centre,
                        [0.5 * (hi - lo), 0.75 * radius],
                    ))
                }
                CutMode::Points | CutMode::Parallel => None,
            }
        }
        Some(Surface::Plane { normal, .. }) => {
            let n = DVec3::from(*normal).normalize();
            // On a wall "along" is vertical; on a deck, along x.
            let up = if n.z.abs() < 0.9 { DVec3::Z } else { DVec3::X };
            let along = (up - up.dot(n) * n).try_normalize()?;
            let across = n.cross(along);
            line(
                if mode == CutMode::Along {
                    along
                } else {
                    across
                },
                n,
            )
        }
        None => match mode {
            CutMode::Across => Some(make(DVec3::Z, DVec3::X, point, [size, size])),
            CutMode::Along => Some(make(DVec3::X, DVec3::Y, point, [size, size])),
            CutMode::Points | CutMode::Parallel => None,
        },
    }
}

/// The normal of a plane, or of a cylinder at `at` (radial).
fn surface_normal(surface: &Surface, at: DVec3) -> Option<DVec3> {
    match surface {
        Surface::Plane { normal, .. } => DVec3::from(*normal).try_normalize(),
        Surface::Cylinder { point, axis, .. } => {
            let axis = DVec3::from(*axis).try_normalize()?;
            let off = at - DVec3::from(*point);
            (off - off.dot(axis) * axis).try_normalize()
        }
    }
}

/// The cut with normal `n` (parallel to a reference face) through `point` (m) of a face on `surface`: over
/// that face (a little more, `extent` its points) when kept to its surface (`on`), otherwise across the model.
fn parallel_cut(
    n: DVec3,
    point: DVec3,
    surface: Option<&Surface>,
    extent: &[DVec3],
    size: f64,
    on: bool,
) -> Option<Cut> {
    let n = n.try_normalize()?;
    let u = n.any_orthonormal_vector();
    let v = n.cross(u);
    let reach = |e: DVec3| {
        extent
            .iter()
            .map(|x| (*x - point).dot(e).abs())
            .fold(0., f64::max)
            * 1.05
            + 1e-3 * size
    };
    let half = if on && !extent.is_empty() {
        [reach(u), reach(v)]
    } else {
        [size, size]
    };
    Some(Cut {
        centre: point.to_array(),
        normal: n.to_array(),
        axis: u.to_array(),
        half,
        on: surface.filter(|_| on).cloned(),
        face_at: None,
    })
}

/// The cut through `a` and `b` (m), square to the face at `a` (on `surface`, or to the view direction `eye`
/// when the surface is not known): over that face (a little more) when kept to its surface (`on`, `extent` its
/// points), otherwise across the model (`size`).
fn points_cut(
    a: DVec3,
    b: DVec3,
    surface: Option<&Surface>,
    extent: &[DVec3],
    size: f64,
    on: bool,
    eye: DVec3,
) -> Option<Cut> {
    let n = match surface {
        Some(Surface::Plane { normal, .. }) => DVec3::from(*normal).try_normalize()?,
        Some(Surface::Cylinder { point, axis, .. }) => {
            let axis = DVec3::from(*axis).try_normalize()?;
            let off = a - DVec3::from(*point);
            (off - off.dot(axis) * axis).try_normalize()?
        }
        None => eye.try_normalize()?,
    };
    let chord = b - a;
    let d = (chord - chord.dot(n) * n).try_normalize()?;
    let normal = d.cross(n).try_normalize()?;
    let other = normal.cross(d);
    let reach = |e: DVec3| {
        extent
            .iter()
            .map(|x| (*x - a).dot(e).abs())
            .fold(0., f64::max)
            * 1.05
            + 1e-3 * size
    };
    let half = if on && surface.is_some() && !extent.is_empty() {
        [reach(d), reach(other)]
    } else {
        [size, size]
    };
    Some(Cut {
        centre: a.to_array(),
        normal: normal.to_array(),
        axis: d.to_array(),
        half,
        on: surface.filter(|_| on).cloned(),
        face_at: None,
    })
}

/// The point where the plane through `c` with unit normal `n` meets a cylinder, near `h`, a crossing of the plane
/// with a flat display triangle on that cylinder. Flat triangles cut at a grazing angle (a plane nearly tangent to
/// the cylinder) put their crossings far off the true curve, so the line zigzags: the point is taken on the
/// cylinder instead, at the height of `h` where the curve runs along the axis, at its angle where it runs around.
/// `h` itself when the plane does not reach the cylinder there.
fn on_cylinder(h: DVec3, c: DVec3, n: DVec3, point: DVec3, axis: DVec3, radius: f64) -> DVec3 {
    use std::f64::consts::{PI, TAU};
    let Some(a) = axis.try_normalize() else {
        return h;
    };
    let e1 = a.any_orthonormal_vector();
    let e2 = a.cross(e1);
    let d = h - point;
    let t = d.dot(a);
    let theta = d.dot(e2).atan2(d.dot(e1));
    let at = |theta: f64, t: f64| point + t * a + radius * (theta.cos() * e1 + theta.sin() * e2);
    // The plane at (θ, t): k + t (a·n) + A cos θ + B sin θ = 0.
    let k = (point - c).dot(n);
    let (an, big_a, big_b) = (a.dot(n), radius * e1.dot(n), radius * e2.dot(n));
    let radial = theta.cos() * e1 + theta.sin() * e2;
    let tangent = n.cross(radial);
    if tangent.dot(a).abs() >= tangent.dot(a.cross(radial)).abs() {
        // Along the axis: keep the height, solve the angle; of the two, the one nearer to h.
        let r = big_a.hypot(big_b);
        let rhs = -(k + t * an);
        if r < 1e-12 || rhs.abs() > r {
            return h;
        }
        let phi = big_b.atan2(big_a);
        let spread = (rhs / r).acos();
        let off = |x: f64| ((x - theta + PI).rem_euclid(TAU) - PI).abs();
        let best = if off(phi + spread) <= off(phi - spread) {
            phi + spread
        } else {
            phi - spread
        };
        at(best, t)
    } else {
        // Around the axis: keep the angle, solve the height.
        if an.abs() < 1e-12 {
            return h;
        }
        at(theta, -(k + big_a * theta.cos() + big_b * theta.sin()) / an)
    }
}

/// Where a cut crosses the display triangles of `scene` (m): segments inside the cut's rectangle, on the
/// triangles that lie on the cut's surface (or on every triangle without one), and only on `face` when given.
/// On a cylinder face the crossings lie on the exact curve ([`on_cylinder`]).
fn cut_segments(
    cut: &Cut,
    scene: &cad2bem::cad::scene::Scene,
    face: Option<i32>,
) -> Vec<[DVec3; 2]> {
    let n = DVec3::from(cut.normal).normalize();
    let u = DVec3::from(cut.axis).normalize();
    let v = n.cross(u);
    let c = DVec3::from(cut.centre);
    let mut cylinders = std::collections::HashMap::new();
    for f in &scene.faces {
        if let Some(Surface::Cylinder {
            point,
            axis,
            radius,
        }) = &f.surface
        {
            cylinders.insert(f.tag, (DVec3::from(*point), DVec3::from(*axis), *radius));
        }
    }
    let size = {
        let b = scene.bounds_m;
        (b[3] - b[0]).max(b[4] - b[1]).max(b[5] - b[2])
    };
    let on_surface = |p: DVec3| {
        cut.on
            .as_ref()
            .is_none_or(|s| s.distance(p.to_array()) <= 2e-3 * size)
    };
    let inside = |p: DVec3| {
        let d = p - c;
        d.dot(u).abs() <= cut.half[0] * 1.0001 && d.dot(v).abs() <= cut.half[1] * 1.0001
    };
    let mut out = Vec::new();
    for (t, &tag) in scene.triangles.iter().zip(&scene.face_tags) {
        if face.is_some_and(|f| f != tag) {
            continue;
        }
        let p = t.map(|i| DVec3::from(scene.positions_m[i as usize]));
        let s = p.map(|x| (x - c).dot(n));
        let mut hits = Vec::new();
        for k in 0..3 {
            let (a, b) = (k, (k + 1) % 3);
            if (s[a] < 0.) != (s[b] < 0.) {
                let w = s[a] / (s[a] - s[b]);
                hits.push(p[a] + w * (p[b] - p[a]));
            }
        }
        if let Some(&(point, axis, radius)) = cylinders.get(&tag) {
            for h in &mut hits {
                *h = on_cylinder(*h, c, n, point, axis, radius);
            }
        }
        if hits.len() == 2 && p.iter().all(|&x| on_surface(x)) && hits.iter().all(|&h| inside(h)) {
            out.push([hits[0], hits[1]]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn a_parallel_cut_keeps_the_reference_normal_and_passes_the_point() {
        // Reference: a column wall at angle 0.3 about z (its radial normal there); cut a deck z = -2 face.
        let column = Surface::Cylinder {
            point: [0.; 3],
            axis: [0., 0., 1.],
            radius: 6.,
        };
        let n = surface_normal(
            &column,
            DVec3::new(6. * 0.3_f64.cos(), 6. * 0.3_f64.sin(), -5.),
        )
        .unwrap();
        assert!((n - DVec3::new(0.3_f64.cos(), 0.3_f64.sin(), 0.)).length() < 1e-12);
        let deck = Surface::Plane {
            point: [0., 0., -2.],
            normal: [0., 0., 1.],
        };
        let p = DVec3::new(10., 3., -2.);
        let extent = [DVec3::new(8., 0., -2.), DVec3::new(14., 6., -2.)];
        let cut = parallel_cut(n, p, Some(&deck), &extent, 100., true).unwrap();
        assert!((DVec3::from(cut.normal) - n).length() < 1e-12);
        assert_eq!(cut.centre, p.to_array());
        assert_eq!(cut.on, Some(deck));
        // Over the face: the rectangle reaches every point of it along its axis.
        let u = DVec3::from(cut.axis);
        assert!(extent.iter().all(|x| (*x - p).dot(u).abs() <= cut.half[0]));
        assert!(cut.half[0] < 10., "over the face only: {:?}", cut.half);
    }

    #[test]
    fn a_two_point_cut_passes_both_points_square_to_the_face() {
        let on_plane = |cut: &Cut, p: DVec3| {
            (p - DVec3::from(cut.centre))
                .dot(DVec3::from(cut.normal))
                .abs()
                < 1e-9
        };
        // A wall x = 0: a slanted line between two of its points.
        let wall = Surface::Plane {
            point: [0.; 3],
            normal: [1., 0., 0.],
        };
        let (a, b) = (DVec3::new(0., 1., -2.), DVec3::new(0., 4., -6.));
        let extent = [DVec3::new(0., 0., 0.), DVec3::new(0., 10., -10.)];
        let cut = points_cut(a, b, Some(&wall), &extent, 100., true, DVec3::X).unwrap();
        assert!(on_plane(&cut, a) && on_plane(&cut, b));
        assert!(
            DVec3::from(cut.normal).dot(DVec3::X).abs() < 1e-12,
            "square to the wall"
        );
        // A cylinder about z of radius 3: the plane holds both points and the radial direction at the first.
        let column = Surface::Cylinder {
            point: [0.; 3],
            axis: [0., 0., 1.],
            radius: 3.,
        };
        let a = DVec3::new(3., 0., -1.);
        let b = DVec3::new(3. * 0.5_f64.cos(), 3. * 0.5_f64.sin(), -4.);
        let cut = points_cut(a, b, Some(&column), &[a, b], 100., true, DVec3::X).unwrap();
        assert!(on_plane(&cut, a) && on_plane(&cut, b));
        assert!(DVec3::from(cut.normal).dot(DVec3::X).abs() < 1e-12);
    }

    #[test]
    fn cut_modes_go_around_and_along_a_cylinder_and_across_a_wall() {
        let column = Surface::Cylinder {
            point: [0., 0., 0.],
            axis: [0., 0., 1.],
            radius: 3.,
        };
        let extent = [DVec3::new(3., 0., -10.), DVec3::new(0., 3., 0.)];
        let point = DVec3::new(3., 0., -5.);
        let across = make_cut(CutMode::Across, Some(&column), point, &extent, 100., true).unwrap();
        assert!(close(across.centre, [0., 0., -5.]) && close(across.normal, [0., 0., 1.]));
        assert_eq!(across.on, Some(column.clone()));
        let along = make_cut(CutMode::Along, Some(&column), point, &extent, 100., true).unwrap();
        assert!(close(along.normal, [0., 1., 0.]));
        assert!(close(along.centre, [2.25, 0., -5.]));
        assert_eq!(along.half, [5., 2.25]);
        assert!(
            cut_corners(&along).iter().all(|c| c.x > -1e-9),
            "from the axis outward"
        );
        // A wall in the plane y = 0: across is horizontal (z = -2), along is vertical (x = 4).
        let wall = Surface::Plane {
            point: [0., 0., 0.],
            normal: [0., 1., 0.],
        };
        let corners = [DVec3::new(0., 0., -10.), DVec3::new(10., 0., 0.)];
        let p = DVec3::new(4., 0., -2.);
        let h = make_cut(CutMode::Across, Some(&wall), p, &corners, 100., true).unwrap();
        assert!(
            DVec3::from(h.normal).cross(DVec3::Z).length() < 1e-9,
            "{:?}",
            h.normal
        );
        let v = make_cut(CutMode::Along, Some(&wall), p, &corners, 100., true).unwrap();
        assert!(
            DVec3::from(v.normal).cross(DVec3::X).length() < 1e-9,
            "{:?}",
            v.normal
        );
        assert!(h.half[0] < 20. && h.half[1] < 20.);
    }

    #[test]
    fn cut_line_on_a_cylinder_follows_the_exact_curve() {
        // A unit cylinder about z; planes from nearly tangent (a grazing cut) to steep, through crossings off the curve.
        let (point, axis, radius) = (DVec3::ZERO, DVec3::Z, 1.);
        for (c, n) in [
            (DVec3::new(0.995, 0., 0.), DVec3::new(1., 0., 0.02)),
            (DVec3::new(0.9, 0., 0.), DVec3::new(1., 0.3, 0.)),
            (DVec3::new(0., 0., 0.4), DVec3::new(0.2, 0., 1.)),
        ] {
            let n = n.normalize();
            // Heights where every plane meets the cylinder (the grazing one leaves it below z = -0.25).
            for z in [-0.2, 0., 0.7] {
                // A point near the curve, as a flat triangle gives it: inside the cylinder, near the plane.
                let guess = DVec3::new(0.97, 0.12, z);
                let guess = guess - (guess - c).dot(n) * n;
                let p = on_cylinder(guess, c, n, point, axis, radius);
                assert!((p - c).dot(n).abs() < 1e-9, "off the plane: {p}");
                assert!(
                    (p.truncate().length() - 1.).abs() < 1e-9,
                    "off the cylinder: {p}"
                );
                assert!(
                    p.distance(guess) < 0.5,
                    "far from the crossing: {p} {guess}"
                );
            }
        }
    }
}

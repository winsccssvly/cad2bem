// SPDX-License-Identifier: GPL-3.0-or-later
//! The settings of the Model tab (preparation, cuts, mesh, the selected face) and of the Fast Mesh tab, and the
//! model panel.

use super::*;

impl Desktop {
    /// Step 1: how the CAD is prepared before meshing (unit, cuts, sheets), and a view of the result.
    pub(super) fn preparation_settings(&mut self, ui: &mut egui::Ui) {
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
                let found = crate::gui::junction::all(&view.scene);
                let ends = found.len();
                let mut cuts: Vec<Cut> = found.into_iter().flat_map(|(_, c)| c).collect();
                cuts.extend(crate::gui::junction::windows(&view.scene));
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
    pub(super) fn cut_file_buttons(&mut self, ui: &mut egui::Ui) {
        // Wrapped: on a narrow panel the buttons go to a second line instead of widening it.
        ui.horizontal_wrapped(|ui| {
            ui.label(match self.settings.cuts.len() {
                0 => "none".to_owned(),
                n => n.to_string(),
            });
            let free = !self.cut_applying;
            if ui
                .add_enabled(free, egui::Button::new("Load…").small())
                .on_disabled_hover_text(crate::gui::CUT_APPLYING)
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
                    .on_disabled_hover_text(crate::gui::CUT_APPLYING)
                    .clicked()
                {
                    self.settings.cuts.clear();
                    self.cut_applying = true;
                }
            }
        });
    }

    /// The face cuts, each with its place and a button that removes it.
    pub(super) fn cut_list(&mut self, ui: &mut egui::Ui) {
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
                                .on_disabled_hover_text(crate::gui::CUT_APPLYING)
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
    pub(super) fn open_model_hint(&mut self, ui: &mut egui::Ui) {
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
    pub(super) fn mesh_settings(&mut self, ui: &mut egui::Ui) {
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
    pub(super) fn face_settings(&mut self, ui: &mut egui::Ui, face: i32) {
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
    pub(super) fn preparation_summary(ui: &mut egui::Ui, record: &serde_json::Value) {
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

    pub(in crate::gui) fn model_panel(&mut self, ctx: &egui::Context) {
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
                        (crate::gui::Stage::Cad, "1  CAD", "Prepare the CAD: unit, waterline, symmetry, extra faces and cuts"),
                        (crate::gui::Stage::Mesh, "2  Mesh", "Convert the prepared CAD to BEM geometry"),
                    ],
                    [
                        (
                            crate::gui::Stage::View,
                            "Mesh Viewer",
                            "View a BEM geometry file (HBG, GDF, STL), separately from the CAD and its conversion",
                        ),
                        (
                            crate::gui::Stage::Fast,
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
                            crate::gui::Stage::Cad => self.preparation_settings(ui),
                            crate::gui::Stage::Mesh => self.mesh_settings(ui),
                            crate::gui::Stage::View => self.viewed_file(ui),
                            crate::gui::Stage::Fast => self.fast_settings(ui),
                        });
                    });
            });
    }

    /// The Fast Mesh tab: the shape and its dimensions, the panels, and what was made (against the exact values).
    pub(super) fn fast_settings(&mut self, ui: &mut egui::Ui) {
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
            crate::gui::FastMade::Panels(made) => {
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
            crate::gui::FastMade::Patches {
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
}

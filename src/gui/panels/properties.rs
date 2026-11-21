// SPDX-License-Identifier: GPL-3.0-or-later
//! The properties panel: the file the Mesh Viewer shows, the properties of the selection, the Gmsh SDK settings.

use super::*;

impl Desktop {
    /// The Mesh Viewer tab: what the viewed file holds, its body and the interior surface found in it (its
    /// elements in z = 0).
    pub(super) fn viewed_file(&mut self, ui: &mut egui::Ui) {
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
    pub(in crate::gui) fn properties_panel(&mut self, ctx: &egui::Context) {
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
                        (_, crate::gui::Stage::View) => "No geometry file opened",
                        (Some(_), _) if self.error.is_some() => {
                            "This file could not be shown. Change the settings to try again."
                        }
                        (Some(_), crate::gui::Stage::Mesh) => "Not converted yet",
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
                        (None, _) if self.stage == crate::gui::Stage::Fast => kind,
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
                    if self.stage == crate::gui::Stage::Cad && view.scene.mode == Mode::Preview {
                        match crate::gui::junction::cuts(&view.scene, face.tag) {
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
                        self.stage == crate::gui::Stage::Mesh
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
    pub(super) fn sdk_settings(&mut self, ui: &mut egui::Ui) {
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
}

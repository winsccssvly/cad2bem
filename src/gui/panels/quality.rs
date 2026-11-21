// SPDX-License-Identifier: GPL-3.0-or-later
//! The checks panel: the checks of the result with their details, and the warnings of a mesh.

use super::*;

impl Desktop {
    pub(in crate::gui) fn quality_panel(&mut self, ctx: &egui::Context) {
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
                    .filter(|_| self.stage == crate::gui::Stage::Mesh)
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
    pub(super) fn mesh_warnings(&mut self, ui: &mut egui::Ui) {
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
}

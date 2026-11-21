// SPDX-License-Identifier: GPL-3.0-or-later
//! The viewport: its toolbar, the 3D view with picking, the legend and the placing of cuts.

use super::*;

impl Desktop {
    /// View buttons and display switches above the viewport.
    pub(super) fn view_toolbar(&mut self, ui: &mut egui::Ui) {
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
            if self.stage == crate::gui::Stage::Cad && cad_view {
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

    /// Draw the scene and handle camera and picking input. Returns the viewport rectangle.
    pub(in crate::gui) fn viewport(&mut self, ctx: &egui::Context) -> egui::Rect {
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
                        if self.stage == crate::gui::Stage::View {
                            "Open an HBG, GDF or STL file, or drop one here"
                        } else if self.stage == crate::gui::Stage::Fast {
                            "Set the dimensions on the left"
                        } else if self.stage == crate::gui::Stage::Mesh && self.source.is_some() {
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
                        self.status = crate::gui::CUT_APPLYING.into();
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

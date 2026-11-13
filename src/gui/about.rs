// SPDX-License-Identifier: GPL-3.0-or-later
//! The About window: the version, the licence of cad2bem, where its source is, and the licence files of every
//! component the executable carries (the same files `cad2bem licenses` prints).

use super::Desktop;
use crate::licenses;
use eframe::egui;

impl Desktop {
    /// Draw the About window while it is open (`self.about` holds the licence file shown).
    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        let Some(mut shown) = self.about else {
            return;
        };
        let mut open = true;
        egui::Window::new("About cad2bem")
            .open(&mut open)
            .default_size([760., 520.])
            .collapsible(false)
            .show(ctx, |ui| {
                ui.heading(format!("cad2bem {}", env!("CARGO_PKG_VERSION")));
                ui.label(
                    "CAD (STEP, IGES) to BEM surface geometry (HBG, GDF, STL). cad2bem is free software: you can \
                     redistribute it and change it under the GNU General Public License, version 3 or later. It \
                     comes with no warranty.",
                );
                ui.label(
                    "The executable carries Gmsh, OpenCASCADE, LEMON and Rust crates. Their licence files are \
                     below, and the command cad2bem licenses prints them. The source of this version, with the \
                     sources of those components, is published with the release.",
                );
                ui.separator();
                let files: Vec<(&str, &[u8])> = licenses::files().collect();
                egui::SidePanel::left("about_files")
                    .resizable(true)
                    .default_width(270.)
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("about_files")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (k, (path, _)) in files.iter().enumerate() {
                                    if ui.selectable_label(k == shown, *path).clicked() {
                                        shown = k;
                                    }
                                }
                            });
                    });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::both()
                        .id_salt("about_text")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let text = licenses::text(files[shown.min(files.len() - 1)].1);
                            ui.add(egui::Label::new(egui::RichText::new(text).monospace().size(12.)).extend());
                        });
                });
            });
        self.about = open.then_some(shown);
    }
}

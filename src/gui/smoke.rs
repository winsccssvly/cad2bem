// SPDX-License-Identifier: GPL-3.0-or-later
//! Scripted visual smoke checks: wait for jobs, optionally pick the centre, save a screenshot and close.

use super::Desktop;
use anyhow::Result;
use eframe::egui;
use std::{sync::atomic::Ordering, time::Duration};

/// A smoke run that has not finished after this long fails and closes the window.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(90);

impl Desktop {
    pub(super) fn smoke(&mut self, ctx: &egui::Context, viewport: egui::Rect) {
        if !self.startup.smoke {
            return;
        }
        ctx.request_repaint();
        if self.started.elapsed() > SMOKE_TIMEOUT || self.error.is_some() {
            eprintln!(
                "Desktop smoke failed: {}",
                self.error.as_deref().unwrap_or("90-second timeout")
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.busy() {
            return;
        }
        self.stable_frames += 1;
        if self.stable_frames < 4 {
            return;
        }
        if self.startup.pick_centre && !self.picked {
            let Some(view) = &self.view else {
                self.error = Some("Picking requires a CAD scene".into());
                return;
            };
            let (origin, direction) = self.camera.ray(viewport, viewport.center());
            let hit = view.hit(origin, direction);
            self.selected = hit.map(|(_, face)| face);
            self.picked_point = hit.map(|(p, _)| {
                (view.scene.centre() + p.as_dvec3() * view.scene.radius()).to_array()
            });
            if let Some(face) = self.selected {
                eprintln!("Smoke picked surface group {face}");
            } else {
                self.error = Some("Centre ray missed every CAD face".into());
            }
            self.picked = true;
            self.stable_frames = 0;
            return;
        }
        if self.startup.click_cut && !self.clicked_cut {
            self.clicked_cut = true;
            match self.cut_preview.clone() {
                Some(cut) => {
                    self.settings.cuts.push(cut);
                    self.cut_added = true;
                    self.cut_applying = true;
                    eprintln!("Smoke added a cut");
                }
                None => self.error = Some("No cut under the view centre".into()),
            }
            self.stable_frames = 0;
            return;
        }
        if let Some(path) = self.startup.write_junction_cuts.take() {
            let Some(view) = &self.view else {
                self.error = Some("Junction cuts need a CAD view".into());
                return;
            };
            let found = super::junction::all(&view.scene);
            for (face, cuts) in &found {
                eprintln!("Junction face {face}: {} cuts", cuts.len());
            }
            let mut cuts: Vec<_> = found.into_iter().flat_map(|(_, c)| c).collect();
            let windows = super::junction::windows(&view.scene);
            eprintln!("Window cuts: {}", windows.len());
            cuts.extend(windows);
            match serde_json::to_vec_pretty(&cuts)
                .map_err(anyhow::Error::from)
                .and_then(|b| Ok(std::fs::write(&path, b)?))
            {
                Ok(()) => eprintln!(
                    "Junction cuts written: {} to {}",
                    cuts.len(),
                    path.display()
                ),
                Err(error) => self.error = Some(format!("{error:#}")),
            }
        }
        if self.startup.write_diagnostics && !self.diagnostics_saved {
            self.diagnostics_saved = true;
            self.save_diagnostics(ctx);
            self.stable_frames = 0;
            return;
        }
        if self.diagnostics_pending.is_some() {
            return; // its screenshot comes first
        }
        if self.startup.then_convert && self.view.is_some() {
            self.startup.then_convert = false;
            self.convert();
            self.stable_frames = 0;
            return;
        }
        if let Some(tab) = self.startup.final_tab.take() {
            let stage = match tab.as_str() {
                "mesh" => super::Stage::Mesh,
                "view" => super::Stage::View,
                "fast" => super::Stage::Fast,
                _ => super::Stage::Cad,
            };
            self.set_stage(stage);
            self.stable_frames = 0;
            return;
        }
        if self.stage == super::Stage::Fast
            && self.fast.made.is_some()
            && let Some(path) = self.startup.export.take()
        {
            self.export_fast(path);
            eprintln!("Fast Mesh export: {}", self.status);
        }
        if self.startup.smoke && !self.screenshot_requested {
            eprintln!(
                "Viewport {:?}; camera target {:?} distance {} yaw {} pitch {}; scene centre {:?} radius {:?}",
                viewport,
                self.camera.target,
                self.camera.distance,
                self.camera.yaw,
                self.camera.pitch,
                self.view.as_ref().map(|v| v.scene.centre()),
                self.view.as_ref().map(|v| v.scene.radius())
            );
        }
        let Some(path) = &self.startup.screenshot else {
            self.verified.store(true, Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        let screenshot = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot { image, .. } = event {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(image) = screenshot {
            let result = (|| -> Result<()> {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                image::save_buffer(
                    path,
                    &rgba,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    self.verified.store(true, Ordering::Relaxed);
                    eprintln!("Screenshot saved: {}", path.display());
                }
                Err(error) => eprintln!("Screenshot failed: {error:#}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.screenshot_requested {
            self.screenshot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }
}

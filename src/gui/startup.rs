// SPDX-License-Identifier: GPL-3.0-or-later
//! GUI startup options: a file to open, an automatic conversion/export and visual smoke hooks.

use crate::args::{Conversion, Options};
use anyhow::{Context, Result, bail, ensure};
use cad2bem::conversion::{self, Mode, Settings};
use std::{ffi::OsString, path::PathBuf};

/// Options given to `cad2bem [gui]`, validated before graphics start.
#[derive(Default)]
pub struct Startup {
    pub open: Option<PathBuf>,
    pub library: Option<PathBuf>,
    pub screenshot: Option<PathBuf>,
    /// Untrimmed surface mesh size to generate after the first preview.
    pub mesh_size: Option<f64>,
    pub convert: bool,
    /// Show the CAD as prepared by the conversion settings (cuts, sheets) instead of as imported.
    pub prepare: bool,
    pub mode: Mode,
    pub settings: Settings,
    pub export: Option<PathBuf>,
    pub show_interior: bool,
    /// Show the potential span lines of spline patches.
    pub show_spans: bool,
    /// Start in the cut mode (`across` or `along`); a smoke run shows the cut under the centre.
    pub cut_mode: Option<String>,
    pub pick_centre: bool,
    /// Smoke: save the diagnostics before the screenshot.
    pub write_diagnostics: bool,
    /// Smoke: write the junction cuts of the first view to this JSON file.
    pub write_junction_cuts: Option<PathBuf>,
    /// Smoke: click once in the cut mode (the cut under the view centre), then wait for the views.
    pub click_cut: bool,
    /// Smoke: show this tab (`cad`, `mesh`, `view` or `fast`) before the screenshot, and print the viewport and
    /// camera.
    pub final_tab: Option<String>,
    /// Smoke: the shape of the Fast Mesh tab (`cylinder`, `moonpool`, `sphere`, `hemisphere`, `barge`), with its
    /// typical dimensions.
    pub fast_shape: Option<String>,
    /// Smoke: the Fast Mesh body as exact spline patches (high order).
    pub fast_high_order: bool,
    /// Smoke: the Fast Mesh body as triangles (low order).
    pub fast_triangles: bool,
    /// Open the About window at start.
    pub about: bool,
    /// Smoke: convert once the CAD view is shown (as a user would), not at start.
    pub then_convert: bool,
    /// Close the window automatically once the scripted steps finish (visual smoke checks).
    pub smoke: bool,
}

impl Startup {
    /// The startup options of these command-line arguments, validated.
    pub fn parse(arguments: &[OsString]) -> Result<Self> {
        let mut startup = Self::default();
        let (mut panel_size, mut patch_size) = (None, None);
        let mut options = Options::new(arguments);
        while let Some(argument) = options.next_argument() {
            let name = argument
                .to_str()
                .context("Option name is not valid Unicode")?;
            if options.conversion(
                name,
                Conversion {
                    mode: &mut startup.mode,
                    settings: &mut startup.settings,
                    panel_size: &mut panel_size,
                    patch_size: &mut patch_size,
                },
            )? {
                continue;
            }
            match name {
                "--open" => startup.open = Some(options.path("--open", "a CAD or geometry file")?),
                "--gmsh-library" => {
                    startup.library = Some(options.path("--gmsh-library", "a shared library")?)
                }
                "--screenshot" => {
                    startup.screenshot = Some(options.path("--screenshot", "a PNG path")?);
                    startup.smoke = true;
                }
                "--mesh-size" => {
                    let size = options.metres("--mesh-size")?;
                    ensure!(
                        size.is_finite() && size > 0.,
                        "--mesh-size must be positive and finite"
                    );
                    startup.mesh_size = Some(size);
                }
                "--pick-centre" => startup.pick_centre = true,
                "--write-diagnostics" => startup.write_diagnostics = true,
                "--write-junction-cuts" => {
                    startup.write_junction_cuts =
                        Some(options.path("--write-junction-cuts", "a JSON path")?);
                    startup.smoke = true;
                }
                "--click-cut" => startup.click_cut = true,
                "--then-convert" => startup.then_convert = true,
                "--final-tab" => {
                    startup.final_tab = Some(
                        options
                            .text("--final-tab", "cad, mesh, view or fast")?
                            .to_owned(),
                    )
                }
                "--fast-shape" => {
                    let shape = options
                        .text(
                            "--fast-shape",
                            "cylinder, moonpool, sphere, hemisphere or barge",
                        )?
                        .to_owned();
                    ensure!(
                        matches!(
                            shape.as_str(),
                            "cylinder" | "moonpool" | "sphere" | "hemisphere" | "barge"
                        ),
                        "--fast-shape must be cylinder, moonpool, sphere, hemisphere or barge"
                    );
                    startup.fast_shape = Some(shape);
                }
                "--fast-high-order" => startup.fast_high_order = true,
                "--fast-triangles" => startup.fast_triangles = true,
                "--about" => startup.about = true,
                "--cut-mode" => {
                    let mode = options.text("--cut-mode", "across, along, points or parallel")?;
                    ensure!(
                        matches!(mode, "across" | "along" | "points" | "parallel"),
                        "--cut-mode must be across, along, points or parallel"
                    );
                    startup.cut_mode = Some(mode.to_owned());
                }
                "--convert" => startup.convert = true,
                "--prepare" => startup.prepare = true,
                "--show-interior" => startup.show_interior = true,
                "--show-spans" => startup.show_spans = true,
                "--export" => startup.export = Some(options.path("--export", "an output path")?),
                "--smoke" => startup.smoke = true,
                "--diagnostics" => {
                    let state = super::diagnostics::State::read(
                        &options.path("--diagnostics", "a diagnostics folder")?,
                    )?;
                    startup.open = Some(state.source);
                    startup.mode = state.mode;
                    panel_size = panel_size.or(state.settings.panel_size);
                    patch_size = patch_size.or(state.settings.patch_size);
                    startup.settings = state.settings;
                }
                other => {
                    bail!("Unknown GUI option {other}. Use gui --help for conversion options.")
                }
            }
        }
        startup
            .settings
            .set_sizes(startup.mode, panel_size, patch_size)?;
        if startup.convert {
            let spec = conversion::Spec {
                schema: conversion::SCHEMA,
                source: startup
                    .open
                    .clone()
                    .context("--convert requires --open CAD")?,
                mode: startup.mode,
                settings: startup.settings.clone(),
            };
            spec.validate()?;
            if let Some(path) = &startup.export {
                startup.mode.check_output(path)?;
            }
        }
        ensure!(
            startup.export.is_none()
                || startup.convert
                || startup.final_tab.as_deref() == Some("fast"),
            "--export requires --convert or --final-tab fast"
        );
        ensure!(
            !startup.show_interior
                || (startup.convert && startup.settings.interior)
                || startup
                    .open
                    .as_deref()
                    .is_some_and(cad2bem::view::mesh_file::is_mesh_file),
            "--show-interior requires --convert --interior, or a geometry file to open (.hbg, .gdf)"
        );
        Ok(startup)
    }
}

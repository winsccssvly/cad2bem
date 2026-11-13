// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared helpers for the hand-written GUI and CLI option parsers.

use anyhow::{Context, Result};
use cad2bem::conversion::{Mode, PanelMethod, PatchMethod, Settings, Sheets, Symmetry};
use std::{ffi::OsString, path::PathBuf, slice::Iter};

/// Command-line options read in order; each value accessor names the option in its error.
pub struct Options<'a>(Iter<'a, OsString>);

/// What the conversion options set, the same for `convert` and `gui`: the mode, the settings, and the panel
/// and patch sizes (applied to the settings with the mode once every option is read).
pub struct Conversion<'s> {
    pub mode: &'s mut Mode,
    pub settings: &'s mut Settings,
    pub panel_size: &'s mut Option<f64>,
    pub patch_size: &'s mut Option<f64>,
}

impl<'a> Options<'a> {
    /// The options of `arguments`, read in order.
    pub fn new(arguments: &'a [OsString]) -> Self {
        Self(arguments.iter())
    }

    /// The next raw argument, an option name or a positional value.
    pub fn next_argument(&mut self) -> Option<&'a OsString> {
        self.0.next()
    }

    /// The value of `option` as a path; `what` describes the expected value.
    pub fn path(&mut self, option: &str, what: &str) -> Result<PathBuf> {
        Ok(self
            .0
            .next()
            .with_context(|| format!("{option} requires {what}"))?
            .into())
    }

    /// The value of `option` as Unicode text; `what` describes the expected value.
    pub fn text(&mut self, option: &str, what: &str) -> Result<&'a str> {
        self.0
            .next()
            .with_context(|| format!("{option} requires {what}"))?
            .to_str()
            .with_context(|| format!("{option}: the value is not valid Unicode"))
    }

    /// The value of `option`: a JSON file holding a list (of cuts, of face settings).
    pub fn json<T: serde::de::DeserializeOwned>(&mut self, option: &str) -> Result<T> {
        let path = self.path(option, "a JSON file")?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("{option}: cannot read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| {
            format!(
                "{option}: {} does not hold what {option} takes",
                path.display()
            )
        })
    }

    /// Read the conversion option `name` and its value into `into`; false when `name` is not one.
    pub fn conversion(&mut self, name: &str, into: Conversion) -> Result<bool> {
        let settings = into.settings;
        match name {
            "--mode" => *into.mode = Mode::parse(self.text(name, "lo-free, lo-str or ho")?)?,
            "--panel-size" => *into.panel_size = Some(self.metres(name)?),
            "--patch-size" => *into.patch_size = Some(self.metres(name)?),
            "--potential-panel-size" => settings.potential_panel_size = Some(self.metres(name)?),
            "--unit" => settings.unit = self.text(name, "a unit")?.into(),
            "--symmetry" => settings.symmetry = Symmetry::parse(self.text(name, "a plane name")?)?,
            "--panel-method" => {
                settings.panel_method =
                    PanelMethod::parse(self.text(name, "recombined, qqs, quads or triangles")?)?
            }
            "--patch-method" => {
                settings.patch_method = PatchMethod::parse(self.text(name, "tri3, qqs or subdiv")?)?
            }
            "--remove-sheets" => {
                settings.remove_sheets = Sheets::parse(self.text(name, "none, rules or largest")?)?
            }
            "--cuts" => settings.cuts = self.json(name)?,
            "--faces" => settings.faces = self.json(name)?,
            "--cut-waterline" => settings.cut_waterline = true,
            "--interior" => settings.interior = true,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The value of `option` as a number of metres. Range checks belong to the request validation.
    pub fn metres(&mut self, option: &str) -> Result<f64> {
        let text = self.text(option, "metres")?;
        text.parse()
            .with_context(|| format!("{option}: {text} is not a number"))
    }
}

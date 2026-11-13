// SPDX-License-Identifier: GPL-3.0-or-later
//! The session log and saved diagnostics.
//!
//! The log keeps what the user did and what the jobs answered (status lines, errors, panics) in memory and
//! in `logs/` of the data folder. Saved diagnostics are a folder under `diagnostics/` with the GUI
//! state (`state.json`, which `gui --diagnostics DIR` opens again), the conversion request the current settings make
//! (`request.json`, the private worker's input), the last result, the log, a screenshot and a copy of the
//! CAD file.

use super::Desktop;
use anyhow::{Context, Result};
use cad2bem::conversion::{Mode, Settings};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Instant,
};

/// Lines kept in memory (the file keeps all of them).
const LINES: usize = 5000;
/// Log files kept in `logs/`.
const LOG_FILES: usize = 20;
/// A CAD file up to this size is copied into saved diagnostics.
const COPY_LIMIT: u64 = 200 * 1024 * 1024;

/// The log file of this session, for the panic hook.
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Where cad2bem keeps its logs and saved diagnostics: `%LOCALAPPDATA%\cad2bem`, `$XDG_STATE_HOME/cad2bem`,
/// `~/.local/state/cad2bem`, or the temporary folder.
pub(super) fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_STATE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("cad2bem")
}

/// The time now in UTC as `YYYYMMDD-HHMMSS`.
fn stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// The session log.
pub(super) struct Log {
    lines: VecDeque<String>,
    file: Option<File>,
    pub(super) path: Option<PathBuf>,
    started: Instant,
    /// The last status logged, without its trailing figures (progress repeats a stage).
    last_status: String,
    last_error: Option<String>,
}

impl Log {
    /// Start a log file in `logs/` (older ones beyond the last few are removed) and log panics into it.
    pub(super) fn open() -> Self {
        let directory = data_dir().join("logs");
        let path = std::fs::create_dir_all(&directory).ok().map(|()| {
            prune(&directory, LOG_FILES - 1);
            directory.join(format!("cad2bem-{}-{}.log", stamp(), std::process::id()))
        });
        let file = path.as_ref().and_then(|p| File::create(p).ok());
        let path = path.filter(|_| file.is_some());
        if let Some(path) = &path
            && LOG_PATH.set(path.clone()).is_ok()
        {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                if let Some(path) = LOG_PATH.get()
                    && let Ok(mut file) = OpenOptions::new().append(true).open(path)
                {
                    let backtrace = std::backtrace::Backtrace::force_capture();
                    let _ = writeln!(file, "PANIC: {info}\n{backtrace}");
                }
                previous(info);
            }));
        }
        let mut log = Self {
            lines: VecDeque::new(),
            file,
            path,
            started: Instant::now(),
            last_status: String::new(),
            last_error: None,
        };
        log.add(format!(
            "cad2bem {} ({}), {} {}",
            env!("CARGO_PKG_VERSION"),
            if cfg!(feature = "static-gmsh") {
                "static CAD"
            } else {
                "external Gmsh SDK"
            },
            std::env::consts::OS,
            stamp()
        ));
        log
    }

    pub(super) fn add(&mut self, line: impl AsRef<str>) {
        let line = format!(
            "[{:8.2} s] {}",
            self.started.elapsed().as_secs_f64(),
            line.as_ref()
        );
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
        self.lines.push_back(line);
        if self.lines.len() > LINES {
            self.lines.pop_front();
        }
    }

    /// Log the status line and the error when they change.
    fn watch(&mut self, status: &str, error: Option<&String>) {
        // "mesh · 3.21 s" and "mesh · 3.40 s" are one stage.
        let stage = status.split(" · ").next().unwrap_or(status);
        if stage != self.last_status {
            self.last_status = stage.to_owned();
            self.add(format!("status: {status}"));
        }
        if error != self.last_error.as_ref() {
            self.last_error = error.cloned();
            if let Some(error) = error {
                self.add(format!("ERROR: {error}"));
            }
        }
    }

    fn text(&self) -> String {
        self.lines.iter().map(|l| format!("{l}\n")).collect()
    }
}

/// Remove all but the newest `keep` files of a folder.
fn prune(directory: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();
    let extra = files.len().saturating_sub(keep);
    for (_, path) in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(path);
    }
}

/// What the GUI was showing and set, as `state.json` of saved diagnostics.
#[derive(Serialize, Deserialize)]
pub struct State {
    pub version: String,
    /// The CAD file as the user opened it, or the geometry file viewed in its place.
    pub source: PathBuf,
    /// The copy of it in the diagnostics folder, when it was copied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_copy: Option<String>,
    pub mode: Mode,
    /// `cad` or `mesh`.
    pub stage: String,
    pub settings: Settings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<i32>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl State {
    /// Read `state.json` of a diagnostics folder; the source is the copy when the original is gone.
    pub fn read(directory: &Path) -> Result<Self> {
        let path = directory.join("state.json");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("--diagnostics: cannot read {}", path.display()))?;
        let mut state: Self = serde_json::from_str(&text).with_context(|| {
            format!(
                "--diagnostics: {} is not a diagnostics state",
                path.display()
            )
        })?;
        if !state.source.exists()
            && let Some(copy) = &state.source_copy
        {
            state.source = directory.join(copy);
        }
        Ok(state)
    }
}

impl Desktop {
    /// Log status and error changes; run once a frame after the panels.
    pub(super) fn watch_log(&mut self) {
        let error = self.error.clone();
        self.log.watch(&self.status.clone(), error.as_ref());
    }

    /// Save the diagnostics folder, copy its path to the clipboard and take a screenshot into it.
    pub(super) fn save_diagnostics(&mut self, ctx: &egui::Context) {
        match self.write_diagnostics() {
            Ok(directory) => {
                self.log
                    .add(format!("diagnostics: {}", directory.display()));
                ctx.copy_text(directory.display().to_string());
                self.status = format!(
                    "Diagnostics saved to {} (the path is on the clipboard)",
                    directory.display()
                );
                if self.startup.smoke {
                    eprintln!("Diagnostics saved: {}", directory.display());
                }
                self.diagnostics_pending = Some(directory);
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                    "diagnostics",
                )));
            }
            Err(error) => self.error = Some(format!("Could not save the diagnostics: {error:#}")),
        }
    }

    fn write_diagnostics(&mut self) -> Result<PathBuf> {
        let directory = data_dir()
            .join("diagnostics")
            .join(format!("diagnostics-{}", stamp()));
        // Two saves in one second get folders of their own.
        let directory = (0..)
            .map(|k| {
                if k == 0 {
                    directory.clone()
                } else {
                    directory.with_file_name(format!(
                        "{}-{k}",
                        directory.file_name().unwrap_or_default().to_string_lossy()
                    ))
                }
            })
            .find(|d| !d.exists())
            .unwrap_or(directory);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("Could not create {}", directory.display()))?;
        let write = |name: &str, bytes: Vec<u8>| -> Result<()> {
            std::fs::write(directory.join(name), bytes)
                .with_context(|| format!("Could not write {name}"))
        };
        // Diagnostics from the Mesh Viewer tab are about the geometry file shown there (`--diagnostics` opens it
        // again).
        let viewed = self
            .opened
            .as_ref()
            .filter(|_| self.stage == super::Stage::View)
            .map(|o| o.path.clone());
        let source = viewed.or_else(|| self.source.clone());
        let source_copy = match &source {
            Some(source) if std::fs::metadata(source).is_ok_and(|m| m.len() <= COPY_LIMIT) => {
                let name = format!(
                    "source.{}",
                    source.extension().and_then(|e| e.to_str()).unwrap_or("cad")
                );
                std::fs::copy(source, directory.join(&name))
                    .ok()
                    .map(|_| name)
            }
            _ => None,
        };
        let state = State {
            version: env!("CARGO_PKG_VERSION").into(),
            source: source.unwrap_or_default(),
            source_copy,
            mode: self.mode,
            stage: match self.stage {
                super::Stage::Cad => "cad",
                super::Stage::Mesh => "mesh",
                super::Stage::View => "view",
                super::Stage::Fast => "fast",
            }
            .into(),
            settings: self.settings.clone(),
            selected: self.selected,
            status: self.status.clone(),
            error: self.error.clone(),
        };
        write("state.json", serde_json::to_vec_pretty(&state)?)?;
        if let Some(spec) = self.spec() {
            write("request.json", serde_json::to_vec_pretty(&spec)?)?;
        }
        if let Some(session) = &self.converted {
            write(
                "last_result.json",
                serde_json::to_vec_pretty(&session.result)?,
            )?;
        }
        write("log.txt", self.log.text().into_bytes())?;
        write(
            "README.txt",
            format!(
                "cad2bem diagnostics\n\n\
                 state.json   what the GUI showed and set; `cad2bem gui --diagnostics THIS_FOLDER` opens it again\n\
                 request.json the conversion request of these settings\n\
                 last_result.json  the last conversion result (if any)\n\
                 log.txt      the session log (full log: {})\n\
                 screenshot.png    the window\n",
                self.log
                    .path
                    .as_ref()
                    .map_or("none".into(), |p| p.display().to_string())
            )
            .into_bytes(),
        )?;
        Ok(directory)
    }

    /// Save the screenshot the saved diagnostics asked for.
    pub(super) fn diagnostics_screenshot(&mut self, ctx: &egui::Context) {
        if self.diagnostics_pending.is_none() {
            return;
        }
        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = image else {
            return;
        };
        let directory = self.diagnostics_pending.take().expect("checked");
        let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
        if let Err(error) = image::save_buffer(
            directory.join("screenshot.png"),
            &rgba,
            image.size[0] as u32,
            image.size[1] as u32,
            image::ColorType::Rgba8,
        ) {
            self.log
                .add(format!("diagnostics screenshot failed: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stamps_are_civil_utc_times() {
        let s = super::stamp();
        assert_eq!(s.len(), 15);
        assert!(s.starts_with("20"));
        assert_eq!(&s[8..9], "-");
    }
}

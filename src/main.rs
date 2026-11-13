// SPDX-License-Identifier: GPL-3.0-or-later
//! Executable entry point: dispatches the private CAD worker, the CLI and the GUI. Only the GUI starts graphics.
mod args;
mod cli;
mod console;
mod gui;
mod licenses;
use anyhow::{Result, bail};
use eframe::egui;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
fn main() {
    let arguments: Vec<_> = std::env::args_os().collect();
    if let Err(error) = run(&arguments) {
        let message = format!("{error:#}");
        eprintln!("Error: {message}");
        // Opened from Explorer there is no console to read: the GUI's error is shown in a message box.
        if opens_gui(&arguments) && console::unseen() {
            console::message_box(&message);
        }
        std::process::exit(1);
    }
}

/// Whether the arguments start the GUI (no command, `gui`, a CAD file, or GUI options).
fn opens_gui(arguments: &[std::ffi::OsString]) -> bool {
    !matches!(
        arguments.get(1).and_then(|arg| arg.to_str()),
        Some(
            "--worker"
                | "--convert-worker"
                | "--patch-worker"
                | "--view-server"
                | "help"
                | "--help"
                | "-h"
                | "version"
                | "--version"
                | "convert"
                | "licenses"
        )
    )
}

fn run(arguments: &[std::ffi::OsString]) -> Result<()> {
    // Private worker processes: CAD inspection and native conversion. Neither starts graphics.
    type Worker = fn(&Path, &Path) -> Result<()>;
    for (flag, worker) in [
        ("--worker", cad2bem::cad::worker::execute as Worker),
        ("--convert-worker", cad2bem::engine::worker::execute),
        ("--patch-worker", cad2bem::engine::fanout::execute),
    ] {
        if arguments.get(1).is_some_and(|arg| arg == flag) {
            if arguments.len() != 4 {
                bail!("Usage: cad2bem {flag} REQUEST.json RESULT.json");
            }
            return worker(Path::new(&arguments[2]), Path::new(&arguments[3]));
        }
    }
    if arguments.get(1).is_some_and(|arg| arg == "--view-server") {
        return cad2bem::cad::worker::serve();
    }
    match arguments.get(1).and_then(|arg| arg.to_str()) {
        Some("help" | "--help" | "-h") => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        Some("version" | "--version") => {
            println!(
                "cad2bem {} (conversion jobs {}, native preview jobs {})",
                env!("CARGO_PKG_VERSION"),
                cad2bem::conversion::SCHEMA,
                cad2bem::cad::scene::SCHEMA
            );
            return Ok(());
        }
        Some("convert") => return cli::run(&arguments[2..]),
        Some("licenses") => {
            return match arguments.get(2).and_then(|arg| arg.to_str()) {
                None => {
                    // A reader that stops early (a pager, `head`) is not an error.
                    use std::io::Write as _;
                    match std::io::stdout().write_all(licenses::printed().as_bytes()) {
                        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
                        _ => Ok(()),
                    }
                }
                Some("--write") if arguments.len() == 4 => {
                    licenses::write(Path::new(&arguments[3]))
                }
                _ => bail!("Use: cad2bem licenses [--write FOLDER]"),
            };
        }
        Some("gui")
            if arguments
                .get(2)
                .is_some_and(|arg| arg == "--help" || arg == "-h") =>
        {
            print!("{}", cli::HELP);
            return Ok(());
        }
        _ => {}
    }
    // The GUI alone on a console Windows made for it (opened from Explorer before Windows 11 24H2): start again
    // without one and exit, so the console closes now. If that fails, the console is released once the GUI is up.
    if console::owns_alone() && console::relaunch_detached(&arguments[1..]).is_ok() {
        return Ok(());
    }
    let start = if arguments.get(1).is_some_and(|arg| arg == "gui") {
        2
    } else {
        1
    };
    let mut options = arguments[start..].to_vec();
    // A CAD file given alone (dropped on the icon, "Open with") is the file to open.
    if let Some(first) = options
        .first()
        .filter(|a| !a.to_string_lossy().starts_with('-'))
    {
        if !Path::new(first).is_file() {
            bail!(
                "{} is neither a command nor an existing file. Use --help for the commands.",
                first.to_string_lossy()
            );
        }
        options.insert(0, "--open".into());
    }
    let startup = gui::Startup::parse(&options)?;
    let smoke = startup.smoke;
    let verified = Arc::new(AtomicBool::new(false));
    let marker = verified.clone();
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        depth_buffer: 32,
        viewport: window_icon().into_iter().fold(
            egui::ViewportBuilder::default()
                .with_inner_size([1400.0, 920.0])
                .with_drag_and_drop(true),
            |builder, icon| builder.with_icon(icon),
        ),
        ..Default::default()
    };
    eframe::run_native(
        "cad2bem",
        options,
        Box::new(move |cc| {
            let desktop = gui::Desktop::new(cc, startup, marker)
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { error.into() })?;
            console::gui_ready();
            Ok(Box::new(desktop))
        }),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if smoke && !verified.load(Ordering::Relaxed) {
        bail!("Desktop smoke did not complete successfully");
    }
    Ok(())
}

/// The window icon: the logo (`resources/logo-256.png`, made by `scripts/make_icons.ps1`), none if it cannot be read.
fn window_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory(include_bytes!("../resources/logo-256.png"))
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

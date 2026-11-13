// SPDX-License-Identifier: GPL-3.0-or-later
//! Public one-shot conversion CLI. Parsing, help and conversion never initialize graphics.

use crate::args::{Conversion, Options};
use anyhow::{Context, Result, bail, ensure};
use cad2bem::conversion::{Job, Mode, SCHEMA, Settings, Spec, Worker, output_targets};
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// The command-line usage text (`--help`).
pub const HELP: &str = "cad2bem - CAD to HBG/GDF/STL surface geometry

Usage:
  cad2bem                         Open the GUI
  cad2bem gui [--open FILE]       Open the GUI, optionally with a CAD or geometry file
  cad2bem convert CAD --out FILE.hbg|FILE.gdf|FILE.stl [OPTIONS]
  cad2bem version
  cad2bem licenses [--write FOLDER]
                                  The licences of cad2bem and of everything it carries:
                                  printed, or written as files into a new FOLDER

Conversion options:
  --mode lo-free|lo-str|ho    CAD panels, structured panels, or spline patches
  --panel-size METRES         Panel size for lo-free/lo-str (default 2)
  --patch-size METRES         Patch size for ho/lo-str (default 8)
  --potential-panel-size METRES
                              ho: write this potential panel size to the HBG file;
                              the solver sets each patch's spans from it
  --unit file|UNIT            File units, or a unit override (default file)
  --cut-waterline             Keep wetted geometry below z=0
  --symmetry none|xz|yz|xz_and_yz    xz: y=0, yz: x=0, xz_and_yz: both
  --interior                  Also write the interior surface, in FILE after the body; needs --cut-waterline
  --panel-method recombined|qqs|quads|triangles
                              Panel mesher of lo-free: mostly quads, quasi-structured
                              quads, all quads, or triangles (default recombined)
  --patch-method tri3|qqs|subdiv
                              Quad mesher of split faces for ho/lo-str (default tri3)
  --remove-sheets none|rules|largest
                              Remove CAD sheets after the fragment (default none)
  --cuts FILE.json            Planar cuts that partition faces (JSON array of
                              {centre, normal, axis, half}, metres)
  --faces FILE.json           Settings of single faces (JSON array of {face, priority,
                              panel_size (lo-free), path, patch_size (ho, lo-str)})
  --json                      Final result JSON on stdout; progress/errors on stderr
  --help                      Show help without starting a window or runtime

HO writes exact spline patches to HBG only; display sampling never changes export.
Existing output files are preserved. The GUI accepts --open FILE (CAD or geometry file),
--convert and the same conversion options; --export FILE automates export and
--diagnostics DIR reopens saved diagnostics.
The conversion runs in a private worker process of this executable; a build that
does not link Gmsh statically needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY). Ctrl+C
cancels and reaps the worker.
";

struct Arguments {
    spec: Spec,
    output: PathBuf,
    json: bool,
}

fn parse(arguments: &[OsString]) -> Result<Arguments> {
    let mut spec = Spec {
        schema: SCHEMA,
        source: PathBuf::new(),
        mode: Mode::LoFree,
        settings: Settings::default(),
    };
    let (mut source, mut output, mut json) = (None, None, false);
    let (mut panel_size, mut patch_size) = (None, None);
    let mut options = Options::new(arguments);
    while let Some(argument) = options.next_argument() {
        let name = argument.to_str().unwrap_or_default();
        if options.conversion(
            name,
            Conversion {
                mode: &mut spec.mode,
                settings: &mut spec.settings,
                panel_size: &mut panel_size,
                patch_size: &mut patch_size,
            },
        )? {
            continue;
        }
        match argument.to_str() {
            Some("--out") => output = Some(options.path("--out", "an HBG, GDF or STL path")?),
            Some("--json") => json = true,
            Some(flag) if flag.starts_with('-') => bail!("Unknown conversion option {flag}"),
            _ => {
                ensure!(source.is_none(), "Only one input CAD file is accepted");
                source = Some(PathBuf::from(argument));
            }
        }
    }
    spec.source = source.context("convert requires a STEP or IGES file")?;
    let output = output.context("convert requires --out FILE.hbg, FILE.gdf or FILE.stl")?;
    spec.settings.set_sizes(spec.mode, panel_size, patch_size)?;
    spec.validate()?;
    spec.mode.check_output(&output)?;
    output_targets(&output, false)?;
    Ok(Arguments { spec, output, json })
}

/// Run the `convert` command line: print the help, or convert in a worker process and publish the files
/// (Ctrl+C cancels).
pub fn run(arguments: &[OsString]) -> Result<()> {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print!("{HELP}");
        return Ok(());
    }
    let arguments = parse(arguments)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed))
        .context("Cannot install CLI cancellation handler")?;
    let mut job = Job::start(&arguments.spec, &Worker::default())?;
    let print_progress = |job: &mut Job| -> Result<()> {
        for progress in job.progress()? {
            eprintln!("[{:.2}s] {}", progress.seconds, progress.stage);
        }
        Ok(())
    };
    let result = loop {
        if cancelled.load(Ordering::Relaxed) {
            job.cancel();
            bail!("Conversion cancelled; no outputs published");
        }
        print_progress(&mut job)?;
        if let Some(result) = job.poll() {
            break result?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    print_progress(&mut job)?;
    if let Some(fallback) = result.summary.get("panel_method_fallback") {
        eprintln!(
            "The quasi-structured mesher failed ({}); converted with the default mesher (mostly quads)",
            fallback["reason"].as_str().unwrap_or("?")
        );
    }
    ensure!(
        !cancelled.load(Ordering::Relaxed),
        "Conversion cancelled; no outputs published"
    );
    let files = job.publish(&result, &arguments.output)?;
    let seconds = job.started.elapsed().as_secs_f64();
    if arguments.json {
        let response = serde_json::json!({
            "interface": 3,
            "mode": result.mode,
            "state": result.state,
            "engine_version": result.engine_version,
            "summary": result.summary,
            "body": result.body,
            "cad": result.cad,
            "patch_checks": result.patch_checks,
            "interior_patch_checks": result.interior_patch_checks,
            "files": files,
            "seconds": seconds,
        });
        serde_json::to_writer(std::io::stdout().lock(), &response)?;
        println!();
    } else {
        let elements = result.mode.elements();
        println!(
            "{}: {} {elements}; {seconds:.2} s",
            result.state, result.summary[elements]
        );
        for file in files {
            println!("Wrote {}", file.path.display());
        }
    }
    Ok(())
}

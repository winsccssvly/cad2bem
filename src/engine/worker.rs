// SPDX-License-Identifier: GPL-3.0-or-later
//! Private conversion worker: it reads the request and writes progress, the result and the artifact files
//! (job schema 1) that [`crate::conversion::Job`] validates and publishes.
//!
//! Run as `cad2bem --convert-worker REQUEST.json RESULT.json` inside the job directory. Every staged
//! file is read back before the result reports success.

use super::{ho, lofree, panels::Panels, structured};
use crate::conversion::{HBG_VERSION, Mode, SCHEMA, Spec};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::Instant,
};

/// The version reported as `engine_version`.
pub fn engine_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Run one request/result transaction. Errors become a failed result; only I/O on the result fails here.
pub fn execute(request: &Path, result: &Path) -> Result<()> {
    ensure!(!result.exists(), "worker result path already exists");
    let outcome = (|| -> Result<Value> {
        ensure!(
            fs::metadata(request)?.len() <= 1024 * 1024,
            "conversion request is too large"
        );
        let text = fs::read_to_string(request)?;
        let spec: Spec = serde_json::from_str(&text).context("invalid conversion request")?;
        run(&spec, result.parent().context("result has no directory")?)
    })();
    let response = match outcome {
        Ok(result) => json!({"status": "completed", "result": result}),
        Err(error) => json!({"status": "failed", "message": format!("{error:#}")}),
    };
    let pending = result.with_extension("pending");
    fs::write(&pending, serde_json::to_vec(&response)?)?;
    fs::rename(pending, result)?;
    Ok(())
}

/// The failure of a conversion whose quality checks fail (none when they pass). Its result is still
/// written, so that the GUI can show where it failed, but it is never published. The message names the
/// failed body checks, the interior when it fails, and the first failing patches with their checks.
fn failure(summary: &Value, checks: &Value, patch_checks: &[Value]) -> Option<String> {
    if summary["state"] != "fail" {
        return None;
    }
    let mut failures: Vec<String> = checks
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, c)| c["state"] == "fail")
        .map(|(name, _)| name.clone())
        .collect();
    if summary["interior"]["state"] == "fail" {
        failures.push("interior".into());
    }
    let patches: Vec<String> = patch_checks
        .iter()
        .filter(|p| p["state"] == "fail")
        .map(|p| {
            let failed: Vec<&str> = p["checks"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, s)| *s == "fail")
                .map(|(name, _)| name.as_str())
                .collect();
            format!("{} {}", p["name"].as_str().unwrap_or("?"), failed.join("+"))
        })
        .collect();
    if !patches.is_empty() {
        let shown = patches
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = patches.len().saturating_sub(8);
        failures.push(if more > 0 {
            format!("{} patches: {shown} and {more} more", patches.len())
        } else {
            format!("{} patches: {shown}", patches.len())
        });
    }
    if failures.is_empty() {
        failures.push("elements".into());
    }
    Some(format!(
        "conversion quality checks failed ({}); no outputs published",
        failures.join("; ")
    ))
}

fn run(spec: &Spec, directory: &Path) -> Result<Value> {
    ensure!(spec.schema == SCHEMA, "unsupported conversion job schema");
    spec.validate()?;
    let started = Instant::now();
    let progress_path = directory.join("progress.jsonl");
    let mut progress = |stage: &str, data: Value| {
        let event =
            json!({"stage": stage, "seconds": started.elapsed().as_secs_f64(), "data": data});
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&progress_path)
        {
            let _ = writeln!(file, "{event}");
        }
    };
    let artifacts_dir = directory.join("artifacts");
    let mut artifacts = Vec::new();
    let mut record = |role: &str, format: &str, path: &Path| -> Result<()> {
        let relative = path
            .strip_prefix(directory)?
            .to_string_lossy()
            .replace('\\', "/");
        artifacts.push(json!({"role": role, "format": format, "path": relative}));
        Ok(())
    };
    let failed;
    let (summary, checks, cad, faces, patch_checks, interior_patch_checks) =
        if spec.mode == Mode::Ho {
            let conversion = ho::convert(spec, &mut progress)?;
            failed = failure(
                &conversion.summary,
                &conversion.checks,
                &conversion.patch_checks,
            );
            fs::create_dir(&artifacts_dir)?; // a fresh private job; never reuse another job's artifacts
            progress("validate", json!({}));
            for (role, part) in [
                ("body", Some(&conversion.body)),
                ("interior", conversion.interior.as_ref()),
            ] {
                let Some(part) = part else { continue };
                let hbg = artifacts_dir.join(format!("{role}.hbg"));
                let document = part.to_hbg();
                fs::write(&hbg, serde_json::to_vec(&document)?)?;
                let back: Value = serde_json::from_slice(&fs::read(&hbg)?)?;
                ensure!(back == document, "HBG exact spline round-trip failed");
                record(role, "hbg", &hbg)?;
            }
            (
                conversion.summary,
                conversion.checks,
                conversion.cad,
                conversion.faces,
                conversion.patch_checks,
                conversion.interior_patch_checks,
            )
        } else {
            // Low-order panels: free (on the CAD faces) or structured (on the spline patches).
            let conversion = if spec.mode == Mode::LoFree {
                let c = lofree::convert(spec, &mut progress)?;
                (c.body, c.interior, c.summary, c.checks, c.cad)
            } else {
                let c = structured::convert(spec, &mut progress)?;
                (c.body, c.interior, c.summary, c.checks, c.cad)
            };
            let (body, interior, summary, checks, cad) = conversion;
            failed = failure(&summary, &checks, &[]);
            fs::create_dir(&artifacts_dir)?;
            progress("validate", json!({}));
            let scale = summary["body_size_m"].as_f64().unwrap_or(1.);
            for (role, part) in [("body", Some(&body)), ("interior", interior.as_ref())] {
                let Some(part) = part else { continue };
                let hbg = artifacts_dir.join(format!("{role}.hbg"));
                let gdf = artifacts_dir.join(format!("{role}.gdf"));
                fs::write(&hbg, serde_json::to_vec(&part.to_hbg()?)?)?;
                fs::write(&gdf, part.to_gdf()?)?;
                verify(part, &hbg, &gdf, scale)?;
                record(role, "hbg", &hbg)?;
                record(role, "gdf", &gdf)?;
            }
            let faces = body.faces.clone();
            (summary, checks, cad, faces, Vec::new(), Vec::new())
        };
    progress("complete", json!({}));
    Ok(json!({
        "schema": SCHEMA, "request": spec, "mode": spec.mode, "engine_version": engine_version(),
        "state": summary["state"], "summary": summary, "body": checks, "cad": cad,
        "body_faces": faces, "patch_checks": patch_checks,
        "interior_patch_checks": interior_patch_checks, "artifacts": artifacts, "failure": failed,
    }))
}

/// Read both files back: exact HBG coordinates and symmetry, GDF header, flags, count, coordinates at text
/// precision, normal directions and volumes.
fn verify(panels: &Panels, hbg: &Path, gdf: &Path, scale: f64) -> Result<()> {
    let document: Value = serde_json::from_slice(&fs::read(hbg)?)?;
    ensure!(
        document["format"] == "hbg"
            && document["version"] == HBG_VERSION
            && document["kind"] == "panels",
        "HBG header round-trip failed"
    );
    ensure!(
        document["symmetry"] == panels.symmetry.name(),
        "HBG symmetry round-trip failed"
    );
    let back: Vec<[[f64; 3]; 4]> = serde_json::from_value(document["vertices"].clone())?;
    ensure!(back == panels.vertices, "HBG panel round-trip failed");
    let text = fs::read_to_string(gdf)?;
    let lines: Vec<&str> = text.lines().collect();
    ensure!(
        lines.len() == 4 + 4 * panels.vertices.len(),
        "GDF panel count round-trip failed"
    );
    ensure!(
        lines[1]
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            == [1., 9.80665],
        "GDF units/header round-trip failed"
    );
    let flags: Vec<u8> = lines[2]
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    let axes = panels.symmetry.axes();
    ensure!(
        flags == [u8::from(axes.contains(&0)), u8::from(axes.contains(&1))],
        "GDF symmetry round-trip failed"
    );
    ensure!(
        lines[3].trim().parse::<usize>()? == panels.vertices.len(),
        "GDF panel count round-trip failed"
    );
    let tolerance = scale.max(1.) * 1e-12;
    let mut read = Vec::with_capacity(panels.vertices.len());
    for (k, chunk) in lines[4..].chunks(4).enumerate() {
        let mut panel = [[0.; 3]; 4];
        for (p, line) in panel.iter_mut().zip(chunk) {
            let values: Vec<f64> = line
                .split_whitespace()
                .map(str::parse)
                .collect::<Result<_, _>>()?;
            ensure!(values.len() == 3, "GDF vertex line is malformed");
            *p = [values[0], values[1], values[2]];
        }
        for (a, b) in panel
            .iter()
            .flatten()
            .zip(panels.vertices[k].iter().flatten())
        {
            ensure!(
                (a - b).abs() <= tolerance + 5e-12 * b.abs(),
                "GDF coordinates round-trip failed"
            );
        }
        ensure!(
            super::panels::normal(&panel).dot(super::panels::normal(&panels.vertices[k])) > 0.,
            "GDF reversed a panel normal"
        );
        read.push(panel);
    }
    let original = panels.volumes()?;
    let reread = Panels {
        vertices: read,
        faces: panels.faces.clone(),
        symmetry: panels.symmetry,
        label: panels.label.clone(),
    }
    .volumes()?;
    for (a, b) in reread.iter().zip(original) {
        ensure!(
            (a - b).abs() <= 1e-9 + 1e-9 * b.abs(),
            "GDF volume round-trip failed"
        );
    }
    Ok(())
}

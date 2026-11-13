// SPDX-License-Identifier: GPL-3.0-or-later
//! Every conversion of `tests/data/cases.json` (the OC3 spar, the OC4 columns and the test bodies): the expected state,
//! a closed body, the panel or patch volume against the CAD volume, and an interior surface when one is asked for.

use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn run_case(case: &Value, directory: &Path) -> Result<String, String> {
    let name = case["name"].as_str().unwrap();
    let source = data().join(case["file"].as_str().unwrap());
    let extension = if case["mode"] == "ho" { "hbg" } else { "gdf" };
    let out = directory.join(format!("{name}.{extension}"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_cad2bem"));
    command
        .arg("convert")
        .arg(&source)
        .args(["--mode", case["mode"].as_str().unwrap(), "--json", "--out"])
        .arg(&out);
    for option in case["options"].as_array().unwrap() {
        command.arg(option.as_str().unwrap());
    }
    let output = command.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let result: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let body = &result["body"];
    let volumes = body["volume"]["volumes_m3"].as_array().unwrap();
    let volume = volumes.iter().map(|v| v.as_f64().unwrap()).sum::<f64>() / 3.;
    let cad = body["volume"]["volume_cad_m3"].as_f64();
    let difference = cad.map(|c| volume / c - 1.);
    let report = format!(
        "{name}: state {} closed {} volume {volume:.6} cad {cad:?} rel {difference:?} interior {}",
        result["state"], body["closed"]["state"], result["summary"]["interior"]["state"]
    );
    let mut problems = Vec::new();
    if result["state"] != case["state"] {
        problems.push(format!(
            "state {} (expected {})",
            result["state"], case["state"]
        ));
    }
    let closed = case["closed"].as_str().unwrap_or("ok");
    if body["closed"]["state"] != closed {
        problems.push(format!(
            "closure {} (expected {closed})",
            body["closed"]["state"]
        ));
    }
    if let (Some(limit), Some(d)) = (case["volume_rel"].as_f64(), difference)
        && d.abs() > limit
    {
        problems.push(format!("volume differs from the CAD by {d:.3e}"));
    }
    let asks_interior = case["options"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o == "--interior");
    if asks_interior && !result["summary"]["interior"].is_object() {
        problems.push("no interior surface".into());
    }
    // ho lists the checks of every interior patch (the viewer draws their spans and states from them).
    if asks_interior && result["mode"] == "ho" {
        let listed = result["interior_patch_checks"]
            .as_array()
            .map_or(0, Vec::len);
        let patches = result["summary"]["interior"]["patches"]
            .as_u64()
            .unwrap_or(0) as usize;
        if listed != patches {
            problems.push(format!(
                "{listed} interior patch checks for {patches} interior patches"
            ));
        }
    }
    if problems.is_empty() {
        Ok(report)
    } else {
        Err(format!("{report}\n    {}", problems.join("; ")))
    }
}

#[test]
#[ignore = "slow (minutes): every case in every mode; needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn every_case_converts_as_listed() {
    let cases: Value =
        serde_json::from_slice(&std::fs::read(data().join("cases.json")).unwrap()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let only = std::env::var("CAD2BEM_CASES").ok();
    let mut failures = Vec::new();
    for case in cases["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if only
            .as_deref()
            .is_some_and(|o| !o.split(',').any(|x| x == name))
        {
            continue;
        }
        match run_case(case, directory.path()) {
            Ok(report) => eprintln!("{report}"),
            Err(problem) => {
                eprintln!("FAILED {problem}");
                failures.push(problem);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

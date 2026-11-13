// SPDX-License-Identifier: GPL-3.0-or-later
//! The same native executable provides a headless public converter and the graphical entry point.

mod common;

use std::{
    path::Path,
    process::{Command, Output},
    time::Duration,
};

fn source() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/oc4_only_column.step")
        .to_str()
        .unwrap()
        .to_owned()
}

fn run(arguments: &[&str]) -> Output {
    common::run_within(
        Command::new(env!("CARGO_BIN_EXE_cad2bem")).args(arguments),
        Duration::from_secs(40),
    )
}

#[test]
fn help_and_version_need_no_sdk_runtime_or_window() {
    for arguments in [
        &["--help"][..],
        &["gui", "--help"],
        &["convert", "--help"],
        &["version"],
    ] {
        let output = run(arguments);
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("cad2bem")
        );
    }
}

#[test]
fn unsupported_modes_formats_and_settings_are_rejected_before_conversion() {
    let source = source();
    for (options, message) in [
        (
            vec!["--mode", "unknown", "--out", "unused.hbg"],
            "choose lo-free, lo-str or ho",
        ),
        (vec!["--out", "unused.msh"], ".hbg, .gdf or .stl"),
        (
            vec!["--symmetry", "y", "--out", "unused.hbg"],
            "choose none, xz, yz or xz_and_yz",
        ),
        (
            vec!["--interior", "--out", "unused.hbg"],
            "requires --cut-waterline",
        ),
        (
            vec!["--panel-size", "NaN", "--out", "unused.hbg"],
            "positive and finite",
        ),
        (
            vec!["--patch-size", "6", "--out", "unused.hbg"],
            "not used by lo-free",
        ),
        (
            vec!["--potential-panel-size", "1", "--out", "unused.hbg"],
            "Potential panel size is not used by lo-free",
        ),
        (
            vec!["--python", "python.exe", "--out", "unused.hbg"],
            "Unknown conversion option --python",
        ),
    ] {
        let mut arguments = vec!["convert", source.as_str()];
        arguments.extend(options);
        let output = run(&arguments);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn cli_exports_both_formats_and_preserves_existing_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let hbg = directory.path().join("body with spaces.hbg");
    let convert = |output: &Path, extra: &[&str]| {
        let mut arguments = vec![
            "convert",
            "--panel-size",
            "2",
            "--cut-waterline",
            "--interior",
            "--json",
            "--out",
            output.to_str().unwrap(),
        ];
        arguments.extend(extra);
        let source = source();
        arguments.insert(1, &source);
        run(&arguments)
    };
    let output = convert(&hbg, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["state"], "ok");
    // One file: the interior surface is written after the body.
    assert_eq!(result["interface"], 3);
    assert_eq!(result["files"].as_array().unwrap().len(), 1);
    assert_eq!(result["cad"]["file_unit"], "mm");
    assert!(String::from_utf8_lossy(&output.stderr).contains("complete"));
    let before = std::fs::read(&hbg).unwrap();

    let gdf = directory.path().join("body.gdf");
    let output = convert(&gdf, &[]);
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let text = std::fs::read_to_string(&gdf).unwrap();
    assert_eq!(
        text.lines().nth(3).unwrap().parse::<u64>().unwrap(),
        result["summary"]["panels"].as_u64().unwrap()
            + result["summary"]["interior"]["panels"].as_u64().unwrap()
    );
    assert!(!directory.path().join("body.interior.gdf").exists());

    let again = convert(&hbg, &[]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("Output already exists"));
    assert_eq!(std::fs::read(&hbg).unwrap(), before);

    let asymmetric = directory.path().join("asymmetric.hbg");
    let failure = convert(&asymmetric, &["--symmetry", "yz"]);
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stderr).contains("not symmetric"));
    assert!(!asymmetric.exists());
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn ho_writes_the_potential_panel_size_and_its_spans() {
    let directory = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/column.step")
        .to_str()
        .unwrap()
        .to_owned();
    let convert = |name: &str, extra: &[&str]| {
        let output = directory.path().join(name);
        let mut arguments = vec![
            "convert",
            source.as_str(),
            "--mode",
            "ho",
            "--patch-size",
            "6",
            "--cut-waterline",
            "--interior",
            "--json",
            "--out",
            output.to_str().unwrap(),
        ];
        arguments.extend(extra);
        let run = run(&arguments);
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
        let file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        (result, file)
    };
    let spans = |checks: &serde_json::Value| -> u64 {
        checks
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                c["discretization"][0].as_u64().unwrap() * c["discretization"][1].as_u64().unwrap()
            })
            .sum()
    };
    // Without the option the file carries the default size, so a reader needs no discretization of its own.
    let (plain, file) = convert("plain.hbg", &[]);
    assert_eq!(file["panel_size"], 2.0);
    // With it the file carries it, and the span preview follows it: smaller spans, more of them.
    let (fine, file) = convert("fine.hbg", &["--potential-panel-size", "1"]);
    assert_eq!(file["panel_size"], 1.0);
    assert!(spans(&fine["patch_checks"]) > spans(&plain["patch_checks"]));
    assert!(spans(&fine["interior_patch_checks"]) > spans(&plain["interior_patch_checks"]));
    // The file: the body's patches, then the interior surface's.
    assert_eq!(
        fine["patch_checks"].as_array().unwrap().len()
            + fine["interior_patch_checks"].as_array().unwrap().len(),
        file["patches"].as_array().unwrap().len()
    );
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn triangle_panels_and_an_stl_file_of_the_whole_body() {
    let directory = tempfile::tempdir().unwrap();
    let column = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/column.step")
        .to_str()
        .unwrap()
        .to_owned();
    let convert = |output: &Path, extra: &[&str]| {
        let mut arguments = vec![
            "convert",
            column.as_str(),
            "--panel-size",
            "2",
            "--cut-waterline",
            "--interior",
            "--json",
            "--out",
            output.to_str().unwrap(),
        ];
        arguments.extend(extra);
        let output = run(&arguments);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    // Triangles: every panel repeats its last vertex.
    let hbg = directory.path().join("triangles.hbg");
    convert(&hbg, &["--panel-method", "triangles"]);
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&hbg).unwrap()).unwrap();
    let panels = document["vertices"].as_array().unwrap();
    assert!(!panels.is_empty());
    assert!(panels.iter().all(|p| p[2] == p[3]), "only triangles");

    // STL: two solids, each quad two facets, the symmetry images made too.
    let stl = directory.path().join("body.stl");
    let result = convert(&stl, &["--symmetry", "xz_and_yz"]);
    assert_eq!(result["files"].as_array().unwrap().len(), 1);
    let text = std::fs::read_to_string(&stl).unwrap();
    assert_eq!(text.matches("endsolid ").count(), 2);
    let start = text.find("solid interior_surface").unwrap();
    let facets = text.matches("facet normal").count();
    let interior = text[start..].matches("facet normal").count();
    assert!(facets > interior && interior > 0);
    // The quarter stored, the whole body written: four images of each panel's one or two facets.
    let body_panels = result["summary"]["panels"].as_u64().unwrap() as usize;
    let body_facets = facets - interior;
    assert!(
        body_facets >= 4 * body_panels && body_facets <= 8 * body_panels,
        "{body_facets} facets of {body_panels} panels"
    );
}

/// Convert the test OC4 columns in lo-free with `extra` options to `output`, the quasi-structured mesher made
/// to fail when `fail` is set; the JSON result.
fn convert_oc4(output: &Path, extra: &[&str], fail: bool) -> serde_json::Value {
    let oc4 = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/oc4_only_column.step")
        .to_str()
        .unwrap()
        .to_owned();
    let mut arguments = vec![
        "convert",
        oc4.as_str(),
        "--panel-size",
        "2",
        "--cut-waterline",
        "--interior",
        "--json",
        "--out",
        output.to_str().unwrap(),
    ];
    arguments.extend(extra);
    let mut command = Command::new(env!("CARGO_BIN_EXE_cad2bem"));
    command.args(&arguments);
    if fail {
        command.env("CAD2BEM_TEST_FAIL_QQS", "1");
    } else {
        command.env_remove("CAD2BEM_TEST_FAIL_QQS");
    }
    let output = common::run_within(&mut command, Duration::from_secs(300));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "needs the static build"]
fn the_quasi_structured_mesher_falls_back_and_the_default_mesh_repeats() {
    let directory = tempfile::tempdir().unwrap();
    // Gmsh's quasi-structured pipeline can fail (in its topology optimisation of a junction face); made to fail
    // here, the conversion goes on with the default mesher and says so.
    let qqs = directory.path().join("qqs.hbg");
    let result = convert_oc4(&qqs, &["--panel-method", "qqs"], true);
    let fallback = &result["summary"]["panel_method_fallback"];
    assert_eq!(fallback["requested"], "qqs", "{result}");
    assert_eq!(fallback["used"], "recombined");
    assert!(qqs.is_file());
    // The default mesher (its matching by LEMON) gives the same file every run, and the fallback that file.
    let [first, second] = ["first.hbg", "second.hbg"].map(|name| {
        let path = directory.path().join(name);
        convert_oc4(&path, &[], false);
        std::fs::read(path).unwrap()
    });
    assert!(first == second, "two runs of the same conversion differ");
    assert!(
        std::fs::read(&qqs).unwrap() == first,
        "the fallback is the default conversion"
    );
}

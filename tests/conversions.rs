// SPDX-License-Identifier: GPL-3.0-or-later
//! Real conversions through the public CLI in every mode: the checks of the result, exact volumes of the
//! generated test bodies, repeatable output and the refusal of a body that is not symmetric.

mod common;

use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

fn data(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(path)
}

fn run(arguments: &[&str]) -> Output {
    common::run_within(
        Command::new(env!("CARGO_BIN_EXE_cad2bem")).args(arguments),
        Duration::from_secs(120),
    )
}

/// Convert and return the JSON result.
fn convert(source: &Path, options: &[&str], out: &Path) -> Value {
    let mut arguments = vec!["convert", source.to_str().unwrap()];
    arguments.extend(options);
    arguments.extend(["--json", "--out", out.to_str().unwrap()]);
    let output = run(&arguments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn vertices(hbg: &Path) -> Vec<[[f64; 3]; 4]> {
    let document: Value = serde_json::from_slice(&std::fs::read(hbg).unwrap()).unwrap();
    serde_json::from_value(document["vertices"].clone()).unwrap()
}

/// Every volume estimate of the body within `tolerance` of `exact`.
fn assert_volume(result: &Value, exact: f64, tolerance: f64) {
    for v in result["body"]["volume"]["volumes_m3"].as_array().unwrap() {
        let v = v.as_f64().unwrap();
        assert!(
            (v / exact - 1.).abs() < tolerance,
            "volume {v}, exact {exact}"
        );
    }
}

const COLUMN: [&str; 8] = [
    "--cut-waterline",
    "--symmetry",
    "xz_and_yz",
    "--interior",
    "--panel-size",
    "2",
    "--mode",
    "lo-free",
];

#[test]
#[cfg_attr(
    not(feature = "static-gmsh"),
    ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"
)]
fn column_panels_are_closed_match_the_cad_volume_and_repeat_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let source = data("column.step");
    let first = directory.path().join("first.hbg");
    let result = convert(&source, &COLUMN, &first);
    assert_eq!(result["state"], "ok");
    assert_eq!(result["engine_version"], env!("CARGO_PKG_VERSION"));
    let body = &result["body"];
    assert_eq!(body["closed"]["free_edges"], 0);
    assert_eq!(body["orientation"]["method"], "cad_solid");
    // The test column: 40 x 30 x 6 m base and a radius 8 m column, 20 m draft (whole body).
    let exact = 40. * 30. * 6. + std::f64::consts::PI * 64. * 14.;
    let cad = body["volume"]["volume_cad_m3"].as_f64().unwrap();
    assert!((cad / exact - 1.).abs() < 1e-9, "CAD volume {cad}");
    assert_volume(&result, exact, 5e-3);
    assert_eq!(
        result["summary"]["interior"]["checks"]["plane"], "ok",
        "{}",
        result["summary"]["interior"]
    );
    // One file: the body's panels, then the interior surface's.
    let count = |item: &Value| item["panels"].as_u64().unwrap() as usize;
    assert_eq!(
        count(&result["summary"]) + count(&result["summary"]["interior"]),
        vertices(&first).len()
    );
    assert!(!directory.path().join("first.interior.hbg").exists());
    let second = directory.path().join("second.hbg");
    convert(&source, &COLUMN, &second);
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "the file differs between two runs"
    );
}

#[test]
#[cfg_attr(
    not(feature = "static-gmsh"),
    ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"
)]
fn cylinder_patches_are_exact_and_structured_panels_close_in_both_formats() {
    // A radius 10 m cylinder of 20 m draft, a quadrant cut at z = 0, x = 0 and y = 0.
    let exact = std::f64::consts::PI * 100. * 20.;
    let source = data("cylinder.step");
    let directory = tempfile::tempdir().unwrap();
    let common = [
        "--cut-waterline",
        "--symmetry",
        "xz_and_yz",
        "--interior",
        "--patch-size",
        "5",
    ];

    let hbg = directory.path().join("ho.hbg");
    let mut options = common.to_vec();
    options.extend(["--mode", "ho"]);
    let result = convert(&source, &options, &hbg);
    assert_eq!(result["mode"], "ho");
    assert_eq!(result["state"], "ok", "{}", result["summary"]);
    assert_eq!(result["files"].as_array().unwrap().len(), 1);
    assert_volume(&result, exact, 1e-9);
    let document: Value = serde_json::from_slice(&std::fs::read(&hbg).unwrap()).unwrap();
    assert_eq!(document["kind"], "spline_patches");
    // The interior surface's patches follow the body's in the same file.
    let names: Vec<&str> = document["patches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    let first_interior = names
        .iter()
        .position(|n| n.starts_with("waterplane_"))
        .unwrap();
    assert_eq!(
        first_interior,
        result["summary"]["patches"].as_u64().unwrap() as usize
    );
    assert!(
        names[first_interior..]
            .iter()
            .all(|n| n.starts_with("waterplane_"))
    );
    assert!(
        document["patches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.get("weights").is_some()),
        "the cylinder wall is an exact rational patch"
    );

    let mut options = common.to_vec();
    options.extend(["--mode", "lo-str", "--panel-size", "1"]);
    let mut panels = Vec::new();
    for extension in ["hbg", "gdf"] {
        let out = directory.path().join(format!("lo-str.{extension}"));
        let result = convert(&source, &options, &out);
        assert_eq!(result["state"], "ok", "{}", result["summary"]);
        assert_eq!(result["body"]["closed"]["free_edges"], 0);
        assert_volume(&result, exact, 5e-3);
        assert!(
            !directory
                .path()
                .join(format!("lo-str.interior.{extension}"))
                .exists()
        );
        // The file holds the body's panels and the interior surface's.
        panels.push(
            result["summary"]["panels"].as_u64().unwrap()
                + result["summary"]["interior"]["panels"].as_u64().unwrap(),
        );
    }
    assert_eq!(panels[0], panels[1], "both formats hold the same panels");
    let gdf = std::fs::read_to_string(directory.path().join("lo-str.gdf")).unwrap();
    assert_eq!(
        gdf.lines().nth(3).unwrap().parse::<u64>().unwrap(),
        panels[1]
    );
    assert_eq!(
        gdf.lines().count() as u64,
        4 + 4 * panels[1],
        "four vertex lines a panel"
    );
}

#[test]
#[cfg_attr(
    not(feature = "static-gmsh"),
    ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"
)]
fn asymmetric_body_is_refused_with_the_mirror_distance() {
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("oc4.hbg");
    let output = run(&[
        "convert",
        data("oc4_only_column.step").to_str().unwrap(),
        "--cut-waterline",
        "--symmetry",
        "yz",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "the body is not symmetric about x = 0 (mirror distance 11.2 m, area difference 0.47)"
        ),
        "{stderr}"
    );
    assert!(!out.exists());
}

#[test]
#[cfg_attr(
    not(feature = "static-gmsh"),
    ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"
)]
fn face_settings_force_a_layout_and_report_what_did_not_fit() {
    let source = data("cylinder.step");
    let directory = tempfile::tempdir().unwrap();
    let options = [
        "--cut-waterline",
        "--symmetry",
        "xz_and_yz",
        "--mode",
        "ho",
        "--patch-size",
        "5",
    ];
    let first = convert(&source, &options, &directory.path().join("first.hbg"));
    let paths = first["cad"]["face_paths"].as_object().unwrap();
    // The wall: a face that takes its own parameters.
    let wall: i32 = paths
        .iter()
        .find(|(_, p)| *p == "direct")
        .map(|(f, _)| f.parse().unwrap())
        .unwrap();
    let faces = directory.path().join("faces.json");
    std::fs::write(
        &faces,
        format!(r#"[{{"face": {wall}, "path": "split", "patch_size": 2.5, "priority": 1}}, {{"face": {wall_plus}, "path": "ring"}}]"#, wall_plus = wall + 1000),
    )
    .unwrap();
    let mut forced = options.to_vec();
    forced.extend(["--faces", faces.to_str().unwrap()]);
    let second = convert(&source, &forced, &directory.path().join("second.hbg"));
    assert_eq!(second["cad"]["face_paths"][wall.to_string()], "split");
    assert_eq!(
        second["cad"]["face_settings"]["faces_unknown"][0],
        wall + 1000
    );
    let count = |r: &Value| {
        r["patch_checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["face"] == wall)
            .count()
    };
    assert!(
        count(&second) > count(&first),
        "the forced split makes more patches"
    );
    let exact = std::f64::consts::PI * 100. * 20.;
    assert_volume(&second, exact, 1e-6);
}

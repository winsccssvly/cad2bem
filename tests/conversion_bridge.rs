// SPDX-License-Identifier: GPL-3.0-or-later
//! Conversion job tests: the private worker of the built executable, result validation and no-clobber
//! publication.

use cad2bem::conversion::{Job, Mode, Role, SCHEMA, Settings, Spec, Worker, output_targets};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// The worker of the built executable; the Gmsh SDK comes from `CAD2BEM_GMSH_LIBRARY` unless it is linked.
fn worker() -> Worker {
    Worker {
        executable: Some(env!("CARGO_BIN_EXE_cad2bem").into()),
        gmsh_library: None,
    }
}

fn spec() -> Spec {
    Spec {
        schema: SCHEMA,
        source: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/oc4_only_column.step"),
        mode: Mode::LoFree,
        settings: Settings {
            cut_waterline: true,
            interior: true,
            ..Default::default()
        },
    }
}

#[test]
fn rejects_unsupported_output_and_preserves_existing_destinations() {
    let directory = tempfile::tempdir().unwrap();
    assert!(output_targets(&directory.path().join("mesh.msh"), false).is_err());
    let output = directory.path().join("body.hbg");
    std::fs::write(&output, "existing body").unwrap();
    assert!(output_targets(&output, false).is_err());
    // The GUI replaces it (its save dialog asked).
    assert!(output_targets(&output, true).is_ok());
    assert_eq!(std::fs::read_to_string(output).unwrap(), "existing body");
    let mut bad = spec();
    bad.settings.cut_waterline = false;
    assert!(bad.validate().is_err());
}

#[test]
fn a_missing_worker_is_reported_when_the_job_starts() {
    let missing = Worker {
        executable: Some("deliberately-missing-worker.exe".into()),
        gmsh_library: None,
    };
    let error = Job::start(&spec(), &missing).err().unwrap();
    assert!(format!("{error:#}").contains("conversion worker"));
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn converts_once_and_publishes_both_formats_with_an_interior() {
    let mut request = spec();
    request.settings.panel_size = Some(1.2345678901234567);
    let mut job = Job::start(&request, &worker()).unwrap();
    let start = Instant::now();
    let mut stages = Vec::new();
    let result = loop {
        stages.extend(job.progress().unwrap().into_iter().map(|e| e.stage));
        if let Some(result) = job.poll() {
            break result.unwrap();
        }
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(10));
    };
    stages.extend(job.progress().unwrap().into_iter().map(|e| e.stage));
    assert!(stages.contains(&"complete".to_owned()));
    assert_eq!(result.state, "ok");
    let directory = tempfile::tempdir().unwrap();
    let hbg = directory.path().join("output with spaces.HBG");
    // One file: the body's panels, then the interior surface's.
    let files = job.publish(&result, &hbg).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].role, Role::Body);
    let body: serde_json::Value = serde_json::from_slice(&std::fs::read(&hbg).unwrap()).unwrap();
    assert_eq!(body["kind"], "panels");
    let interior = result.summary["interior"]["panels"].as_u64().unwrap() as usize;
    assert!(interior > 0);
    assert_eq!(
        body["vertices"].as_array().unwrap().len(),
        result.body_faces.len() + interior
    );
    let saved = std::fs::read(&hbg).unwrap();
    assert!(job.publish(&result, &hbg).is_err());
    assert_eq!(std::fs::read(&hbg).unwrap(), saved);
    let gdf = directory.path().join("body.gdf");
    assert_eq!(job.publish(&result, &gdf).unwrap().len(), 1);
    let text = std::fs::read_to_string(gdf).unwrap();
    assert!(text.contains("1 9.80665"));
    assert_eq!(
        text.lines().nth(3).unwrap().parse::<usize>().unwrap(),
        result.body_faces.len() + interior
    );
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn cancel_reaps_the_worker_before_any_output_publication() {
    let mut request = spec();
    request.settings.panel_size = Some(0.01);
    let mut job = Job::start(&request, &worker()).unwrap();
    job.cancel();
    assert!(job.poll().unwrap().is_err());
}

#[test]
fn cuts_are_validated_and_limited_to_their_surface() {
    use cad2bem::conversion::{Cut, Surface};
    let column = Surface::Cylinder {
        point: [0., 0., 0.],
        axis: [0., 0., 2.],
        radius: 3.,
    };
    assert!((column.distance([3., 0., 5.])).abs() < 1e-12);
    assert!((column.distance([0., 4., -1.]) - 1.).abs() < 1e-12);
    let plane = Surface::Plane {
        point: [0., 0., -1.],
        normal: [0., 0., 3.],
    };
    assert!((plane.distance([5., 5., 1.]) - 2.).abs() < 1e-12);
    let cut = Cut {
        centre: [0., 0., -1.],
        normal: [0., 0., 1.],
        axis: [1., 0., 0.],
        half: [4., 4.],
        on: Some(column),
        face_at: None,
    };
    let mut request = spec();
    request.settings.cuts = vec![cut.clone()];
    assert!(request.validate().is_ok());
    for bad in [
        Cut {
            axis: [0., 1., 1.],
            ..cut.clone()
        },
        Cut {
            half: [0., 1.],
            ..cut.clone()
        },
        Cut {
            on: Some(Surface::Cylinder {
                point: [0.; 3],
                axis: [0., 0., 1.],
                radius: -1.,
            }),
            ..cut.clone()
        },
    ] {
        request.settings.cuts = vec![bad];
        assert!(request.validate().is_err());
    }
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn a_cut_with_face_at_splits_only_that_face() {
    use cad2bem::conversion::Cut;
    let split = |face_at: Option<[f64; 3]>| {
        let mut request = Spec {
            source: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/column.step"),
            ..spec()
        };
        request.settings.cuts = vec![Cut {
            centre: [10., 7.5, -10.],
            normal: [0., 0., 1.],
            axis: [1., 0., 0.],
            half: [30., 30.],
            on: None,
            face_at,
        }];
        let mut job = Job::start(&request, &worker()).unwrap();
        let result = loop {
            if let Some(result) = job.poll() {
                break result.unwrap();
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        result.cad["cuts"]["faces_split"][0].as_u64().unwrap()
    };
    assert_eq!(split(None), 3, "the walls around z = -10");
    assert_eq!(split(Some([0., 7., -10.])), 1, "only the wall x = 0");
}

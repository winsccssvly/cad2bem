// SPDX-License-Identifier: GPL-3.0-or-later
//! Real child-process integration. SDK cases are explicit opt-in, not silently skipped successes.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use cad2bem::cad::{
    scene::{Mode, Request, SCHEMA},
    worker::{Job, Response},
};

/// Tests that use the resident view server (one per test process) take turns.
static RESIDENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/column.step")
        .canonicalize()
        .unwrap()
}

fn request() -> Request {
    Request {
        schema: SCHEMA,
        source: source(),
        library: None,
        mode: Mode::Preview,
        size_m: None,
        preparation: None,
        save_cad: None,
        quick: false,
    }
}

fn run(request: &Request) -> Response {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("request.json");
    let output = directory.path().join("result.json");
    std::fs::write(&input, serde_json::to_vec(request).unwrap()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cad2bem"))
        .args([
            std::ffi::OsStr::new("--worker"),
            input.as_os_str(),
            output.as_os_str(),
        ])
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Native SDK worker exceeded 30 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap()
}

#[test]
fn external_sdk_errors_are_actionable_without_starting_a_window() {
    let mut request = request();
    request.library = Some(source().with_file_name("deliberately-missing-sdk.dll"));
    match run(&request) {
        Response::Failed { message } => {
            assert!(message.contains(if cfg!(feature = "static-gmsh") {
                "Static CAD build rejects external SDK paths"
            } else {
                "CAD2BEM_GMSH_LIBRARY"
            }))
        }
        other => panic!("Expected a structured loader failure, got {other:?}"),
    }
}

#[test]
#[ignore = "requires an external or statically linked Gmsh 4.15 SDK"]
fn real_sdk_preserves_faces_and_meshes_in_metres() {
    let mut request = request();
    let preview = match run(&request) {
        Response::Completed { scene } => scene,
        other => panic!("{other:?}"),
    };
    preview.validate().unwrap();
    assert!(preview.faces.len() >= 4);
    assert!(preview.sdk_version.starts_with("4.15."));
    request.mode = Mode::SurfaceMesh;
    request.size_m = Some(0.5);
    let fine = match run(&request) {
        Response::Completed { scene } => scene,
        other => panic!("{other:?}"),
    };
    fine.validate().unwrap();
    assert!(fine.triangles.len() > preview.triangles.len());
    assert_eq!(
        fine.faces.iter().map(|f| f.tag).collect::<Vec<_>>(),
        preview.faces.iter().map(|f| f.tag).collect::<Vec<_>>()
    );
    assert_eq!(fine.bounds_m, preview.bounds_m);

    // This supplied STEP states millimetres. Rendering coordinates must be metres.
    request.source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/oc4_only_column.step");
    request.mode = Mode::Preview;
    let millimetres = match run(&request) {
        Response::Completed { scene } => scene,
        other => panic!("{other:?}"),
    };
    millimetres.validate().unwrap();
    let width = millimetres.bounds_m[4] - millimetres.bounds_m[1];
    assert!(
        (width - 73.99944).abs() < 0.01,
        "Unexpected model width {width} m"
    );
    request.source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/iges_wigley.igs");
    let iges = match run(&request) {
        Response::Completed { scene } => scene,
        other => panic!("{other:?}"),
    };
    iges.validate().unwrap();
    assert!(!iges.faces.is_empty());
}

#[test]
#[ignore = "requires an external or statically linked Gmsh 4.15 SDK"]
fn cancelled_worker_is_reaped_and_next_job_is_usable() {
    let mut input = request();
    input.mode = Mode::SurfaceMesh;
    input.size_m = Some(0.005);
    let mut job =
        Job::start_with_program(&input, Path::new(env!("CARGO_BIN_EXE_cad2bem"))).unwrap();
    job.cancel();
    assert!(job.poll().unwrap().is_err());
    drop(job);
    let mut next =
        Job::start_with_program(&request(), Path::new(env!("CARGO_BIN_EXE_cad2bem"))).unwrap();
    let start = Instant::now();
    loop {
        if let Some(scene) = next.poll() {
            scene.unwrap().validate().unwrap();
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "Worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires an external or statically linked Gmsh 4.15 SDK"]
fn a_saved_prepared_cad_opens_with_the_same_faces_in_metres() {
    let directory = tempfile::tempdir().unwrap();
    // The millimetre STEP, cut at the waterline and in half: the saved file must read back in metres.
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/oc4_only_column.step");
    for name in ["prepared.step"] {
        let saved = directory.path().join(name);
        let mut request = request();
        request.source = source.clone();
        request.preparation = Some(cad2bem::cad::scene::Preparation {
            unit: "file".into(),
            cut_waterline: true,
            symmetry: cad2bem::conversion::Symmetry::Xz,
            remove_sheets: Default::default(),
            cuts: Vec::new(),
        });
        request.save_cad = Some(saved.clone());
        let prepared = match run(&request) {
            Response::Completed { scene } => scene,
            other => panic!("{other:?}"),
        };
        let mut reopen = self::request();
        reopen.source = saved;
        let again = match run(&reopen) {
            Response::Completed { scene } => scene,
            other => panic!("{other:?}"),
        };
        assert_eq!(again.faces.len(), prepared.faces.len(), "{name}");
        for (a, b) in again.bounds_m.iter().zip(&prepared.bounds_m) {
            // Boxes come from the display tessellation, which differs slightly between the two models.
            assert!(
                (a - b).abs() < 1e-2,
                "{name}: {:?} vs {:?}",
                again.bounds_m,
                prepared.bounds_m
            );
        }
        let area = |s: &cad2bem::cad::scene::Scene| s.faces.iter().map(|f| f.area_m2).sum::<f64>();
        assert!((area(&again) / area(&prepared) - 1.).abs() < 1e-6, "{name}");
    }
}

#[test]
#[ignore = "requires an external or statically linked Gmsh 4.15 SDK"]
fn the_resident_server_gives_the_scenes_of_separate_workers() {
    let _turn = RESIDENT.lock().unwrap_or_else(|e| e.into_inner());
    let program = Path::new(env!("CARGO_BIN_EXE_cad2bem"));
    let wait = |mut job: Job| loop {
        if let Some(scene) = job.poll() {
            return scene.unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let column = request();
    let mut prepared = request();
    prepared.source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/oc4_only_column.step");
    prepared.preparation = Some(cad2bem::cad::scene::Preparation {
        unit: "file".into(),
        cut_waterline: true,
        symmetry: cad2bem::conversion::Symmetry::Xz,
        remove_sheets: Default::default(),
        cuts: Vec::new(),
    });
    // The same with one cut more: the faces it does not split come from the cache.
    let mut cut = prepared.clone();
    cut.preparation.as_mut().unwrap().cuts = vec![cad2bem::conversion::Cut {
        centre: [14.435, 25., -5.],
        normal: [0., 0., 1.],
        axis: [1., 0., 0.],
        half: [9., 9.],
        on: None,
        face_at: None,
    }];
    // One server for all of them, in an order that leaves state behind if a request did not start afresh.
    for request in [&column, &prepared, &column, &cut, &prepared] {
        if std::ptr::eq(request, &cut) {
            let scene = wait(Job::start_with_program(request, program).unwrap());
            let split = &scene.prepared.as_ref().unwrap()["cuts"]["faces_split"][0];
            assert!(
                split.as_u64().unwrap() > 0,
                "the cut splits a face: {split}"
            );
        }
        let resident = wait(Job::start_resident(request, program).unwrap());
        let separate = wait(Job::start_with_program(request, program).unwrap());
        let tags =
            |s: &cad2bem::cad::scene::Scene| s.faces.iter().map(|f| f.tag).collect::<Vec<_>>();
        assert_eq!(tags(&resident), tags(&separate));
        // A face meshed again next to cached ones may get another triangle pattern (Gmsh meshes a face a little
        // differently with other faces in the same run): the faces and their facts are compared.
        let facts = |s: &cad2bem::cad::scene::Scene| {
            let faces: Vec<_> = s
                .faces
                .iter()
                .map(|f| cad2bem::cad::scene::Face {
                    triangles: 0,
                    ..f.clone()
                })
                .collect();
            serde_json::to_string(&faces).unwrap()
        };
        assert_eq!(facts(&resident), facts(&separate));
        assert_eq!(resident.bounds_m, separate.bounds_m);
        assert_eq!(resident.prepared, separate.prepared);
    }
}

#[test]
#[ignore = "requires an external or statically linked Gmsh 4.15 SDK"]
fn a_quick_cut_is_provisional_and_the_full_view_follows() {
    let _turn = RESIDENT.lock().unwrap_or_else(|e| e.into_inner());
    let program = Path::new(env!("CARGO_BIN_EXE_cad2bem"));
    let wait = |mut job: Job| loop {
        if let Some(scene) = job.poll() {
            return scene.unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut base = request();
    base.preparation = Some(cad2bem::cad::scene::Preparation {
        unit: "file".into(),
        cut_waterline: false,
        symmetry: Default::default(),
        remove_sheets: Default::default(),
        cuts: Vec::new(),
    });
    // Twice: the second view has every face cached, so they are hidden from the mesher in the held model.
    wait(Job::start_resident(&base, program).unwrap());
    let first = wait(Job::start_resident(&base, program).unwrap());
    // The wall x = 0 of the column, cut at z = -10 (only that face).
    let mut cut = base.clone();
    cut.preparation.as_mut().unwrap().cuts = vec![cad2bem::conversion::Cut {
        centre: [10., 7.5, -10.],
        normal: [0., 0., 1.],
        axis: [1., 0., 0.],
        half: [30., 30.],
        on: None,
        face_at: Some([0., 7., -10.]),
    }];
    cut.quick = true;
    let quick = wait(Job::start_resident(&cut, program).unwrap());
    assert_eq!(quick.prepared.as_ref().unwrap()["provisional"], true);
    assert_eq!(quick.faces.len(), first.faces.len() + 1);
    // Not held any more: the next quick request is a full view, the same as a separate worker's.
    let full = wait(Job::start_resident(&cut, program).unwrap());
    assert!(full.prepared.as_ref().unwrap().get("provisional").is_none());
    cut.quick = false;
    let separate = wait(Job::start_with_program(&cut, program).unwrap());
    let tags = |s: &cad2bem::cad::scene::Scene| s.faces.iter().map(|f| f.tag).collect::<Vec<_>>();
    assert_eq!(tags(&full), tags(&separate));
    assert_eq!(full.faces.len(), quick.faces.len());
}

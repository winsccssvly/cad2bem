// SPDX-License-Identifier: GPL-3.0-or-later
//! Exercise the GUI's real background adapter without requiring a graphics device.

use cad2bem::{
    cad::scene,
    conversion::{Mode, SCHEMA, Settings, Spec, Worker},
    view::conversion::{self as conversion_view, Event, Ready, Task},
};
use std::{path::PathBuf, process::Command, time::Duration};

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
        source: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/column.step"),
        mode: Mode::LoFree,
        settings: Settings {
            cut_waterline: true,
            interior: true,
            ..Default::default()
        },
    }
}

fn wait(task: &Task) -> Result<Box<Ready>, String> {
    loop {
        match task
            .events
            .recv_timeout(Duration::from_secs(30))
            .expect("GUI adapter stopped responding")
        {
            Event::Progress(_) => (),
            Event::Finished(result) => return result,
        }
    }
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn gui_retains_original_panels_and_exports_identically_to_cli() {
    let task = Task::start(spec(), worker()).unwrap();
    let ready = wait(&task).unwrap();
    drop(task); // Finished-task cleanup must not delete artifacts retained by the session.
    let session = ready.session;
    let panels = session.body.display_vertices();
    assert_eq!(ready.body.scene.mode, scene::Mode::BemPanels);
    assert_eq!(ready.body.scene.positions_m.len(), panels.len() * 4);
    assert_eq!(ready.body.scene.positions_m[0], panels[0][0]);
    let triangles = panels.iter().filter(|p| p[2] == p[3]).count();
    assert_eq!(
        ready.body.scene.triangles.len(),
        2 * panels.len() - triangles
    );
    assert!(
        ready.body.warnings.is_empty(),
        "GPU diagonal angles are not panel quality"
    );
    assert!(
        ready
            .body
            .scene
            .face_tags
            .iter()
            .all(|tag| session.result.body_faces.contains(tag))
    );
    let combined = ready.with_interior.unwrap();
    assert!(combined.scene.triangles.len() > ready.body.scene.triangles.len());
    assert!(
        combined
            .scene
            .faces
            .iter()
            .any(|face| face.kind == "Interior surface")
    );

    let directory = tempfile::tempdir().unwrap();
    for extension in ["hbg", "gdf"] {
        let gui = directory.path().join(format!("gui.{extension}"));
        let cli = directory.path().join(format!("cli.{extension}"));
        let export = conversion_view::export(session.clone(), gui.clone(), None).unwrap();
        assert_eq!(
            export
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap()
                .len(),
            1,
            "one file: the body and its interior surface"
        );
        let status = Command::new(env!("CARGO_BIN_EXE_cad2bem"))
            .args(["convert"])
            .arg(&spec().source)
            .args(["--cut-waterline", "--interior", "--out"])
            .arg(&cli)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert_eq!(std::fs::read(&gui).unwrap(), std::fs::read(&cli).unwrap());
        assert!(
            session.publish(&gui).is_err(),
            "Never overwrite an existing export"
        );
    }
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn cancellation_then_recovery_keeps_the_previous_artifacts_usable() {
    let previous_task = Task::start(spec(), worker()).unwrap();
    let previous = wait(&previous_task).unwrap();
    let mut fine = spec();
    fine.settings.panel_size = Some(0.001);
    let cancelled = Task::start(fine, worker()).unwrap();
    cancelled.cancel();
    assert!(wait(&cancelled).err().unwrap().contains("cancelled"));
    drop(cancelled);
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        previous
            .session
            .publish(&directory.path().join("previous.hbg"))
            .unwrap()
            .len(),
        1
    );
    let recovered = Task::start(spec(), worker()).unwrap();
    assert_eq!(
        wait(&recovered).unwrap().session.body.count(),
        previous.session.body.count()
    );
}

#[test]
fn a_missing_worker_is_reported_by_the_background_task() {
    let task = Task::start(
        spec(),
        Worker {
            executable: Some("missing-worker-for-test.exe".into()),
            gmsh_library: None,
        },
    )
    .unwrap();
    assert!(wait(&task).is_err());
}

#[test]
#[ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"]
fn gui_spline_and_structured_sessions_preserve_exact_exports() {
    use cad2bem::view::conversion::Geometry;
    for mode in [Mode::Ho, Mode::LoStr] {
        let mut request = spec();
        request.mode = mode;
        request.settings.patch_size = Some(6.);
        request.settings.symmetry = cad2bem::conversion::Symmetry::XzAndYz;
        request.settings.panel_size = (mode != Mode::Ho).then_some(2.);
        let task = Task::start(request.clone(), worker()).unwrap();
        let ready = wait(&task).unwrap();
        let session = ready.session;
        drop(task);
        assert_eq!(session.body.count(), session.result.body_faces.len());
        assert!(ready.body.warnings.is_empty());
        assert!(
            ready.with_interior.unwrap().scene.triangles.len() > ready.body.scene.triangles.len()
        );
        if mode == Mode::Ho {
            assert_eq!(ready.body.scene.mode, scene::Mode::BemPatches);
            let Geometry::Splines(document) = &session.body.geometry else {
                panic!("Exact splines were discarded");
            };
            assert!(document.patches.iter().any(|p| p.weights.is_some()));
            assert!(session.body.display_vertices().len() > session.body.count());
            // A new potential panel size: views sampled again, and the size written on export only.
            let (body, with_interior) = session.views_at(0.5).unwrap();
            assert!(body.warnings.is_empty() && with_interior.is_some());
            let directory = tempfile::tempdir().unwrap();
            let sized = directory.path().join("sized.hbg");
            assert_eq!(session.publish_with(&sized, Some(1.5)).unwrap().len(), 1);
            let read = |path: PathBuf| -> serde_json::Value {
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
            };
            let mut body = read(sized.clone());
            assert_eq!(body["panel_size"], 1.5);
            let plain = directory.path().join("plain.hbg");
            session.publish(&plain).unwrap();
            // Without a size the default is written.
            body["panel_size"] = 2.0.into();
            assert_eq!(body, read(plain.clone()), "only panel_size differs");
            // The GUI's export replaces existing files (its save dialog asked); the command line's refuses them.
            assert!(session.publish(&plain).is_err());
            assert_eq!(session.publish_with(&sized, Some(3.)).unwrap().len(), 1);
            assert_eq!(read(sized.clone())["panel_size"], 3.0);
            assert!(
                ready
                    .body
                    .scene
                    .face_tags
                    .iter()
                    .all(|tag| session.result.body_faces.contains(tag))
            );
        } else {
            assert!(matches!(&session.body.geometry, Geometry::Panels(_)));
        }
        let directory = tempfile::tempdir().unwrap();
        let extensions = if mode == Mode::Ho {
            vec!["hbg"]
        } else {
            vec!["hbg", "gdf"]
        };
        for extension in extensions {
            let gui = directory.path().join(format!("gui.{extension}"));
            let cli = directory.path().join(format!("cli.{extension}"));
            assert_eq!(session.publish(&gui).unwrap().len(), 1);
            let mut command = Command::new(env!("CARGO_BIN_EXE_cad2bem"));
            command
                .arg("convert")
                .arg(&request.source)
                .args([
                    "--mode",
                    mode.name(),
                    "--patch-size",
                    "6",
                    "--cut-waterline",
                    "--interior",
                    "--symmetry",
                    "xz_and_yz",
                    "--out",
                ])
                .arg(&cli);
            let status = command.output().unwrap();
            assert!(
                status.status.success(),
                "{}",
                String::from_utf8_lossy(&status.stderr)
            );
            assert_eq!(std::fs::read(&gui).unwrap(), std::fs::read(&cli).unwrap());
        }
        if mode == Mode::Ho {
            assert!(
                session
                    .publish(&directory.path().join("never.gdf"))
                    .is_err()
            );
            assert!(!directory.path().join("never.gdf").exists());
        }
    }
}

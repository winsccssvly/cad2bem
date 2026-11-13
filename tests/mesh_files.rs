// SPDX-License-Identifier: GPL-3.0-or-later
//! The geometry files the converter writes open in the viewer's reader: spline patches and panels, HBG and GDF,
//! with the interior surface found in them, and the volume and interior area the reader finds are the
//! converter's; higher-order GDF files open with the spans of their SPL file.

mod common;

use cad2bem::view::mesh_file::Opened;
use serde_json::Value;
use std::{path::Path, process::Command, time::Duration};

/// Convert the test column to `out` with `options`; the volume of the body and the area of the interior surface
/// the converter reports.
fn convert(out: &Path, mode: &str, options: &[&str]) -> (f64, f64) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/column.step");
    let output = common::run_within(
        Command::new(env!("CARGO_BIN_EXE_cad2bem"))
            .arg("convert")
            .arg(&source)
            .args(["--mode", mode, "--cut-waterline", "--symmetry", "xz_and_yz"])
            .args(["--interior", "--json", "--out"])
            .arg(out)
            .args(options),
        Duration::from_secs(120),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let volumes = result["body"]["volume"]["volumes_m3"].as_array().unwrap();
    let volume = volumes.iter().map(|v| v.as_f64().unwrap()).sum::<f64>() / 3.;
    let area = result["summary"]["interior"]["area_m2"].as_f64().unwrap();
    (volume, area)
}

/// The body's patches (each its own group in the view).
fn summary_count(opened: &Opened) -> usize {
    opened.summary.count
}

#[test]
#[cfg_attr(
    not(feature = "static-gmsh"),
    ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"
)]
fn converted_files_open_with_their_interior_and_volume() {
    let directory = tempfile::tempdir().unwrap();
    for (name, mode, options) in [
        ("patches.hbg", "ho", &["--patch-size", "6"][..]),
        ("panels.gdf", "lo-free", &["--panel-size", "2"][..]),
        (
            "panels.hbg",
            "lo-str",
            &["--patch-size", "6", "--panel-size", "2"][..],
        ),
    ] {
        let out = directory.path().join(name);
        let (volume, area) = convert(&out, mode, options);
        let opened = Opened::open(&out).unwrap();
        assert_eq!(opened.patches, mode == "ho", "{name}");
        let with_interior = opened
            .with_interior
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no interior found"));
        // The interior is one group of the view, after the body's groups.
        let faces = &with_interior.scene.faces;
        let interior = faces
            .iter()
            .filter(|f| f.kind == "Interior surface")
            .count();
        let body = if opened.patches {
            summary_count(&opened)
        } else {
            1
        };
        assert_eq!(
            (interior, faces.len()),
            (1, body + 1),
            "{name}: groups {:?}",
            faces.iter().map(|f| &f.kind).collect::<Vec<_>>()
        );
        let summary = &opened.summary;
        let read = summary.volume().unwrap();
        assert!(
            (read / volume - 1.).abs() < 1e-6,
            "{name}: volume {read} read, {volume} converted"
        );
        let read = opened.interior.as_ref().and_then(|s| s.area).unwrap();
        assert!(
            (read / area - 1.).abs() < 1e-4,
            "{name}: interior area {read} read, {area} converted"
        );
        if mode == "ho" {
            // The size cad2bem writes when none is chosen, and every patch of the column fair.
            assert_eq!(summary.panel_size, Some(2.), "{}", summary.line());
            assert!(
                summary
                    .checks
                    .is_some_and(|[_, warn, fail]| warn == 0 && fail == 0),
                "{}",
                summary.line()
            );
        }
    }
}

#[test]
fn fast_mesh_bodies_written_open_in_the_viewer() {
    use cad2bem::{
        conversion::Symmetry,
        engine::primitives::{Request, Shape, make},
    };
    let directory = tempfile::tempdir().unwrap();
    for (k, shape) in Shape::ALL.into_iter().enumerate() {
        let made = make(&Request {
            shape,
            panel_size: 2.,
            symmetry: Symmetry::XzAndYz,
            interior: true,
            high_order: false,
            triangles: false,
        })
        .unwrap();
        // As Export mesh... writes it: one file, the interior surface after the body.
        let panels = made.merged();
        for (extension, text) in [
            (
                "hbg",
                serde_json::to_string(&panels.to_hbg().unwrap()).unwrap(),
            ),
            ("gdf", panels.to_gdf().unwrap()),
        ] {
            let path = directory.path().join(format!("body{k}.{extension}"));
            std::fs::write(&path, text).unwrap();
            let opened = Opened::open(&path).unwrap();
            let name = format!("{} {extension}", shape.name());
            // The viewer finds the interior again: the panels in z = 0.
            assert_eq!(opened.summary.count, made.body.vertices.len(), "{name}");
            assert_eq!(
                opened.interior.as_ref().map(|i| i.count),
                made.interior.as_ref().map(|i| i.vertices.len()),
                "{name}"
            );
            let volume = opened.summary.volume().unwrap();
            // Flat panels of 2 m on radii of 10 m hold a little less than the exact volume.
            assert!(
                (volume / made.volume_exact - 1.).abs() < 0.02,
                "{name}: {volume} of {}",
                made.volume_exact
            );
            assert_eq!(
                opened.interior.is_some(),
                made.waterplane_exact.is_some(),
                "{name}"
            );
        }
    }
}

#[test]
fn fast_mesh_spline_patches_open_in_the_viewer_with_exact_volume_and_clean_checks() {
    use cad2bem::{
        conversion::Symmetry,
        engine::{
            primitives::{Request, Shape},
            primitives_ho::make,
        },
    };
    let directory = tempfile::tempdir().unwrap();
    for (k, shape) in Shape::ALL.into_iter().enumerate() {
        for symmetry in Symmetry::ALL {
            let made = make(&Request {
                shape,
                panel_size: 2.,
                symmetry,
                interior: true,
                high_order: true,
                triangles: false,
            })
            .unwrap();
            let name = format!("{} {}", shape.name(), symmetry.name());
            let path = directory
                .path()
                .join(format!("body{k}{}.hbg", symmetry.name()));
            std::fs::write(
                &path,
                serde_json::to_string(&made.merged().to_hbg()).unwrap(),
            )
            .unwrap();
            let opened = Opened::open(&path).unwrap();
            assert!(opened.patches, "{name}");
            assert_eq!(opened.summary.count, made.body.patches.len(), "{name}");
            assert_eq!(
                opened.interior.as_ref().map(|i| i.count),
                made.interior.as_ref().map(|i| i.patches.len()),
                "{name}"
            );
            let volume = opened.summary.volume().unwrap();
            assert!(
                (volume / made.volume_exact - 1.).abs() < 1e-9,
                "{name}: {volume} of {}",
                made.volume_exact
            );
            // Every patch passes its checks (the poles of a sphere are collapsed edges, as in a conversion).
            assert!(
                opened.summary.checks.is_some_and(|[_, _, fail]| fail == 0),
                "{name}: {}",
                opened.summary.line()
            );
        }
    }
}

/// A higher-order GDF file (IGDEF 1) of the Fast Mesh barge's patches, with words after the header numbers as
/// WAMIT files have them, and its SPL file (spans, orders).
fn write_spline_gdf(
    gdf: &std::path::Path,
    spl: &std::path::Path,
) -> cad2bem::engine::primitives_ho::MadePatches {
    use cad2bem::{
        conversion::Symmetry,
        engine::{
            primitives::{Request, Shape},
            primitives_ho::make,
        },
    };
    use std::fmt::Write;
    let made = make(&Request {
        shape: Shape::ALL[4],
        panel_size: 2.,
        symmetry: Symmetry::XzAndYz,
        interior: true,
        high_order: true,
        triangles: false,
    })
    .unwrap();
    let patches = made.merged().patches;
    let mut text = format!(
        "barge\n1.0 9.80665  ULEN GRAV\n1 1  ISX ISY\n{} 1  NPATCH IGDEF\n",
        patches.len()
    );
    let mut lines = String::from("barge -- spline control\n");
    for p in &patches {
        assert!(!p.rational(), "the barge's patches are polynomial");
        let (nu, nv) = (p.mu() - p.u_order + 1, p.mv() - p.v_order + 1);
        writeln!(text, "{nu} {nv}\n{} {}", p.u_order, p.v_order).unwrap();
        for knots in [&p.u_knots, &p.v_knots] {
            let row: Vec<String> = knots.iter().map(|k| k.to_string()).collect();
            writeln!(text, "{}", row.join(" ")).unwrap();
        }
        for c in &p.coefficients {
            writeln!(text, "{} {} {}", c.x, c.y, c.z).unwrap();
        }
        writeln!(lines, "{} {}\n{} {}", nu + 2, nv + 1, p.u_order, p.v_order).unwrap();
    }
    std::fs::write(gdf, text).unwrap();
    std::fs::write(spl, lines).unwrap();
    made
}

#[test]
fn higher_order_gdf_opens_with_its_spl_spans_volume_and_interior() {
    let directory = tempfile::tempdir().unwrap();
    let (gdf, spl) = (
        directory.path().join("barge.gdf"),
        directory.path().join("barge.spl"),
    );
    let made = write_spline_gdf(&gdf, &spl);
    // The SPL opens its GDF.
    for path in [&gdf, &spl] {
        let opened = Opened::open(path).unwrap();
        assert_eq!(opened.path, gdf);
        assert!(opened.patches);
        assert_eq!(opened.summary.format, "GDF spline patches");
        assert_eq!(opened.summary.count, made.body.patches.len());
        assert_eq!(
            opened.interior.as_ref().map(|i| i.count),
            made.interior.as_ref().map(|i| i.patches.len())
        );
        assert!(opened.summary.rows, "the spans of the SPL file");
        let volume = opened.summary.volume().unwrap();
        assert!((volume / made.volume_exact - 1.).abs() < 1e-9, "{volume}");
    }
    // Without the SPL file the patches still open, without a discretization.
    std::fs::remove_file(&spl).unwrap();
    assert!(!Opened::open(&gdf).unwrap().summary.rows);
}

#[test]
fn higher_order_gdf_flat_patches_open_and_analytic_shapes_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let flat = directory.path().join("flat.gdf");
    std::fs::write(
        &flat,
        "plate\n1 9.81 ULEN GRAV\n0 0 ISX ISY\n1 0 NPATCH IGDEF\n0 0 -1 1 0 -1 1 1 -1 0 1 -1\n",
    )
    .unwrap();
    let opened = Opened::open(&flat).unwrap();
    assert_eq!(opened.summary.format, "GDF flat patches");
    assert_eq!(opened.summary.count, 1);
    let analytic = directory.path().join("analytic.gdf");
    std::fs::write(
        &analytic,
        "cylinder\n1 9.81\n1 1\n2 -1  NPATCH IGDEF\n2  NLINES\n",
    )
    .unwrap();
    let refused = format!("{:#}", Opened::open(&analytic).err().unwrap());
    assert!(refused.contains("Analytic shapes"), "{refused}");
    // An SPL file without its GDF says so.
    let lone = directory.path().join("lone.spl");
    std::fs::write(&lone, "lone\n2 2\n").unwrap();
    let refused = format!("{:#}", Opened::open(&lone).err().unwrap());
    assert!(refused.contains("GDF file"), "{refused}");
}

#[test]
fn stl_files_open_ascii_and_binary_with_the_whole_body_and_interior() {
    use cad2bem::{
        conversion::Symmetry,
        engine::primitives::{Request, Shape, make},
    };
    let directory = tempfile::tempdir().unwrap();
    let made = make(&Request {
        shape: Shape::ALL[0],
        panel_size: 2.,
        symmetry: Symmetry::XzAndYz,
        interior: true,
        high_order: false,
        triangles: false,
    })
    .unwrap();
    // The volume of the whole body (the stored quarter with its symmetry images).
    let whole = made.body.volumes().unwrap().iter().sum::<f64>() / 3.;
    // The STL writer makes the whole body (four images) and the interior surface as a second solid.
    let ascii = directory.path().join("cylinder.stl");
    let text = made.body.to_stl(made.interior.as_ref()).unwrap();
    std::fs::write(&ascii, &text).unwrap();
    // The same facets as binary STL.
    let mut corners = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("vertex ") {
            let p: Vec<f32> = rest
                .split_whitespace()
                .map(|t| t.parse().unwrap())
                .collect();
            corners.push([p[0], p[1], p[2]]);
        }
    }
    let mut bytes = vec![0_u8; 80];
    bytes.extend((corners.len() as u32 / 3).to_le_bytes());
    for facet in corners.chunks(3) {
        bytes.extend([0_u8; 12]);
        for c in facet {
            for x in c {
                bytes.extend(x.to_le_bytes());
            }
        }
        bytes.extend([0_u8; 2]);
    }
    let binary = directory.path().join("cylinder_binary.stl");
    std::fs::write(&binary, bytes).unwrap();

    let interior_triangles: usize = made
        .interior
        .as_ref()
        .unwrap()
        .vertices
        .iter()
        .map(|p| if p[2] == p[3] { 1 } else { 2 })
        .sum::<usize>()
        * 4;
    for (path, format, tolerance) in [
        (&ascii, "STL triangles", 1e-9),
        (&binary, "STL triangles (binary)", 1e-5),
    ] {
        let opened = Opened::open(path).unwrap();
        assert_eq!(opened.summary.format, format);
        assert_eq!(opened.summary.symmetry, Symmetry::None);
        // The interior surface is found again: its triangles lie in z = 0.
        assert_eq!(
            opened.interior.as_ref().unwrap().count,
            interior_triangles,
            "{format}"
        );
        let volume = opened.summary.volume().unwrap();
        assert!(
            (volume / whole - 1.).abs() < tolerance,
            "{format}: {volume} against {whole}"
        );
    }
}

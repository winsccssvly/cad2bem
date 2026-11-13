// SPDX-License-Identifier: GPL-3.0-or-later
//! Static-CAD capability probe, not a BEM converter: import, Booleans and meshing with no dynamic SDK.

use anyhow::{Context, Result, ensure};
use cad2bem::cad::{
    gmsh,
    scene::{Mode, Request, SCHEMA},
    session::Session,
};
use serde_json::{Value, json};

/// The volumes produced by fragmenting (or intersecting) solid `a` with solid `b`.
fn boolean(gmsh: &Session, a: i32, b: i32, fragment: bool) -> Result<Vec<i32>> {
    let result = if fragment {
        gmsh.fragment(&[(3, a)], &[(3, b)])?
    } else {
        gmsh.intersect(&[(3, a)], &[(3, b)])?
    };
    gmsh.synchronize()?;
    ensure!(
        result.out.iter().all(|&(d, _)| d == 3),
        "Unexpected boolean dimension"
    );
    Ok(result.out.into_iter().map(|(_, t)| t).collect())
}

/// Triangle and quadrilateral counts per face of the current mesh.
fn face_counts(gmsh: &Session) -> Result<Vec<Value>> {
    let faces = gmsh.tags(2)?;
    ensure!(!faces.is_empty(), "No prepared faces");
    let mut out = Vec::new();
    for face in faces {
        let mut counts = [0; 2];
        for (count, (kind, nodes)) in counts.iter_mut().zip([(2, 3), (3, 4)]) {
            let (elements, tags) = gmsh.elements_by_type(kind, face)?;
            ensure!(
                tags.len() == elements.len() * nodes,
                "Invalid element connectivity"
            );
            *count = elements.len();
        }
        out.push(json!({"face": face, "triangles": counts[0], "quadrilaterals": counts[1]}));
    }
    Ok(out)
}

fn mesh(gmsh: &Session, recombine: bool) -> Result<Vec<Value>> {
    for (key, value) in [
        ("Mesh.Algorithm", if recombine { 8. } else { 6. }),
        ("Mesh.RecombineAll", f64::from(recombine)),
        ("Mesh.RecombinationAlgorithm", 1.),
        ("Mesh.ElementOrder", 1.),
        ("Mesh.MeshSizeMin", 0.3),
        ("Mesh.MeshSizeMax", 0.3),
    ] {
        gmsh.number(key, value)?;
    }
    gmsh.clear_mesh()?;
    gmsh.generate(2)?;
    face_counts(gmsh)
}

/// Rewrite the IGES global section so that it states metres: OCCT writes metre coordinates but
/// declares millimetres.
fn state_metres(iges: &std::path::Path) -> Result<()> {
    let text = std::fs::read_to_string(iges)?;
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.as_bytes().get(72) == Some(&b'G'))
        .map(|(i, _)| i)
        .collect();
    let global = indices.iter().map(|&i| &lines[i][..72]).collect::<String>();
    ensure!(
        global.contains(",2,2HMM,") || global.contains(",6,1HM,"),
        "Unrecognized IGES unit encoding"
    );
    let global = global.replace(",2,2HMM,", ",6,1HM,");
    let global = format!("{global:<width$}", width = indices.len() * 72);
    for (part, &i) in indices.iter().enumerate() {
        lines[i] = format!("{}{}", &global[part * 72..(part + 1) * 72], &lines[i][72..]);
    }
    std::fs::write(iges, lines.join("\n") + "\n")?;
    Ok(())
}

fn main() -> Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .context("Usage: static_cad_probe OUTPUT_DIRECTORY")?;
    let output = std::path::PathBuf::from(output);
    std::fs::create_dir_all(&output)?;
    let report = output.join("static-probe.json");
    ensure!(!report.exists(), "Choose a fresh probe directory");
    let full = output.join("box.step");
    let iges = output.join("box.igs");

    let session = Session::open(None)?;
    session.number("General.NumThreads", 1.)?;
    // Written files state metres: set the target unit and let a STEP read define OCCT's unit parameter.
    session.string("Geometry.OCCTargetUnit", "M")?;
    let bootstrap = output.join("units.step");
    std::fs::write(
        &bootstrap,
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('','',(''),(''),'','','');\n\
         FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n\
         #1=CARTESIAN_POINT('',(0.,0.,0.));\nENDSEC;\nEND-ISO-10303-21;\n",
    )?;
    session.import_shapes(&bootstrap)?;
    session.reset("probe")?;
    let a = session.add_box([-2., -1., -3.], [4., 2., 6.])?;
    session.synchronize()?;
    ensure!(
        (session.mass(3, a)? - 48.).abs() < 1e-9,
        "Incorrect box volume"
    );
    session.write(&full)?;
    session.write(&iges)?;
    state_metres(&iges)?;

    session.reset("probe")?;
    let a = session.add_box([-2., -1., -3.], [4., 2., 6.])?;
    let b = session.add_box([-2., -1., -3.], [4., 2., 3.])?;
    let fragments = boolean(&session, a, b, true)?;
    ensure!(fragments.len() == 2, "Fragment did not split box");
    let volumes = fragments
        .iter()
        .map(|&t| session.mass(3, t))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        volumes.iter().all(|v| (v - 24.).abs() < 1e-9),
        "Incorrect fragment volumes"
    );

    session.reset("probe")?;
    let a = session.add_box([-2., -1., -3.], [4., 2., 6.])?;
    let b = session.add_box([0., 0., -3.], [2., 1., 3.])?;
    let quarter = boolean(&session, a, b, false)?;
    ensure!(quarter.len() == 1, "Quarter cut failed");
    let volume = session.mass(3, quarter[0])?;
    ensure!(
        (volume - 6.).abs() < 1e-9,
        "Waterline/symmetry cut volume incorrect"
    );
    let triangles = mesh(&session, false)?;
    let quads = mesh(&session, true)?;
    let total =
        |faces: &[Value], key: &str| faces.iter().map(|v| v[key].as_u64().unwrap()).sum::<u64>();
    ensure!(total(&triangles, "triangles") > 0, "No triangular mesh");
    ensure!(total(&quads, "quadrilaterals") > 0, "No quadrilateral mesh");
    drop(session);

    // The same-EXE worker adapter's unit/import/ownership path in both formats.
    let mut imports = Vec::new();
    for source in [&full, &iges] {
        let scene = gmsh::run(&Request {
            schema: SCHEMA,
            source: source.clone(),
            library: None,
            mode: Mode::SurfaceMesh,
            size_m: Some(0.5),
            preparation: None,
            save_cad: None,
            quick: false,
        })?;
        ensure!(scene.faces.len() == 6, "Box face ownership lost");
        let extent: Vec<_> = (0..3)
            .map(|i| scene.bounds_m[i + 3] - scene.bounds_m[i])
            .collect();
        for (actual, expected) in extent.iter().zip([4., 2., 6.]) {
            ensure!(
                (actual - expected).abs() < 1e-4,
                "Incorrect STEP/IGES metre scale"
            );
        }
        imports.push(
            json!({"format": source.extension().unwrap().to_string_lossy(),
            "faces": scene.faces.len(), "triangles": scene.triangles.len(), "extent_m": extent,
            "gmsh": scene.sdk_version}),
        );
    }
    let value = json!({"backend": "static-gmsh-occt", "imports": imports, "fragment_volumes_m3": volumes,
        "quarter_wetted_volume_m3": volume, "triangle_faces": triangles, "quad_faces": quads});
    std::fs::write(&report, serde_json::to_vec_pretty(&value)?)?;
    println!(
        "Static STEP/IGES, units, booleans, ownership and triangle/quad checks passed: {}",
        report.display()
    );
    Ok(())
}

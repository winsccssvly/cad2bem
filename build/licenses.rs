// SPDX-License-Identifier: GPL-3.0-or-later
//! The licence files the executable carries (`cad2bem licenses`, the GUI's About window, the `licenses/` folder of
//! a release): cad2bem's own notices, the licence files of every Rust crate linked into it and, in the static
//! build, those of Gmsh with the bundled code it compiles, LEMON and OpenCASCADE. Each distinct text is stored once.

use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The code of Gmsh's `contrib/` the static build compiles (Blossom IV is left out: LEMON replaces it).
const GMSH_CONTRIB: &[&str] = &[
    "ALGLIB",
    "ANN",
    "bamg",
    "DiscreteIntegration",
    "domhex",
    "eigen",
    "gmm",
    "HighOrderMeshOptimizer",
    "kbipack",
    "MathEx",
    "MeshOptimizer",
    "MeshQualityOptimizer",
    "nii2mesh",
    "onelab",
    "QuadMeshingTools",
    "QuadTri",
    "tinyxml2",
    "untangle",
    "voro++",
    "WinslowUntangler",
];

/// Whether a file name starts (in any letter case) with one of `prefixes`.
fn starts(name: &str, prefixes: &[&str]) -> bool {
    let name = name.to_ascii_lowercase();
    prefixes.iter().any(|p| name.starts_with(p))
}

/// The files of a folder, sorted by name.
fn files_in(folder: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The name of a path's last component.
fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// Read a file the bundle needs; a missing one stops the build with what to do.
fn read(path: &Path, hint: &str) -> Vec<u8> {
    println!("cargo:rerun-if-changed={}", path.display());
    fs::read(path).unwrap_or_else(|e| panic!("Cannot read {} ({e}): {hint}", path.display()))
}

/// Gmsh, LEMON and OpenCASCADE, from the unpacked source archives the static build was made from.
fn native(root: &Path, files: &mut Vec<(String, Vec<u8>)>) {
    println!("cargo:rerun-if-env-changed=CAD2BEM_NATIVE_SOURCES");
    let sources = env::var_os("CAD2BEM_NATIVE_SOURCES")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("out/native-sources"));
    let hint = "the static build takes the licences of Gmsh, LEMON and OpenCASCADE from the unpacked source \
                archives of scripts/build_static_cad.ps1 (out/native-sources, or CAD2BEM_NATIVE_SOURCES)";
    let pins: Value =
        serde_json::from_slice(&read(&root.join("scripts/native-sources.json"), hint)).unwrap();
    let folder = |key: &str| {
        sources.join(
            pins[key]["directory"]
                .as_str()
                .expect("native-sources.json"),
        )
    };
    let (gmsh, lemon, occt) = (folder("gmsh"), folder("lemon"), folder("occt"));
    for file in ["LICENSE.txt", "CREDITS.txt"] {
        files.push((format!("gmsh/{file}"), read(&gmsh.join(file), hint)));
    }
    for code in GMSH_CONTRIB {
        let directory = gmsh.join("contrib").join(code);
        assert!(
            directory.is_dir(),
            "Gmsh has no contrib/{code}: update GMSH_CONTRIB for this Gmsh"
        );
        for path in files_in(&directory) {
            let file = name(&path);
            if starts(
                &file,
                &[
                    "licen",
                    "copying",
                    "copyright",
                    "authors",
                    "lesser",
                    "readme",
                ],
            ) {
                files.push((format!("gmsh/contrib/{code}/{file}"), read(&path, hint)));
            }
        }
    }
    // gmm refers to the LGPL-3.0, whose text Gmsh does not ship.
    let lgpl3 = read(
        &root.join("resources/licenses/LGPL-3.0.txt"),
        "part of the repository",
    );
    files.push(("gmsh/contrib/gmm/LGPL-3.0.txt".into(), lgpl3));
    files.push(("lemon/LICENSE".into(), read(&lemon.join("LICENSE"), hint)));
    for file in ["LICENSE_LGPL_21.txt", "OCCT_LGPL_EXCEPTION.txt"] {
        files.push((format!("opencascade/{file}"), read(&occt.join(file), hint)));
    }
}

/// The Rust crates linked into the executable (the normal dependencies for the target), each with the licence
/// files it ships, and an index of them.
fn crates(root: &Path, static_cad: bool, files: &mut Vec<(String, Vec<u8>)>) {
    for file in ["Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    let target = env::var("TARGET").unwrap();
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(root)
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
        ])
        .arg(&target);
    if static_cad {
        command.args(["--features", "static-gmsh"]);
    }
    let output = command.output().expect("cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages: HashMap<&str, &Value> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p))
        .collect();
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["id"].as_str().unwrap(), n))
        .collect();
    let mut linked = HashSet::new();
    let mut queue = VecDeque::from([metadata["resolve"]["root"].as_str().unwrap()]);
    while let Some(id) = queue.pop_front() {
        if !linked.insert(id) {
            continue;
        }
        for dependency in nodes[id]["deps"].as_array().unwrap() {
            if dependency["dep_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|k| k["kind"].is_null())
            {
                queue.push_back(dependency["pkg"].as_str().unwrap());
            }
        }
    }
    let mut crates: Vec<&Value> = linked
        .into_iter()
        .map(|id| packages[id])
        .filter(|p| !p["source"].is_null())
        .collect();
    crates.sort_by_key(|p| (p["name"].as_str().unwrap(), p["version"].as_str().unwrap()));
    let apache = read(
        &root.join("resources/licenses/Apache-2.0.txt"),
        "part of the repository",
    );
    let mut rows = Vec::new();
    for package in &crates {
        let (crate_name, version) = (
            package["name"].as_str().unwrap(),
            package["version"].as_str().unwrap(),
        );
        let licence = package["license"].as_str().unwrap_or("");
        let folder = Path::new(package["manifest_path"].as_str().unwrap())
            .parent()
            .unwrap();
        let target = format!("crates/{crate_name}-{version}");
        let mut own = 0;
        for path in files_in(folder) {
            let file = name(&path);
            if starts(
                &file,
                &["licen", "copying", "unlicense", "notice", "copyright"],
            ) {
                files.push((
                    format!("{target}/{file}"),
                    read(&path, "the crate's licence file"),
                ));
                own += 1;
            }
        }
        // Licence files one level down (some crates keep them with their data).
        let mut folders: Vec<PathBuf> = fs::read_dir(folder)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        folders.sort();
        for sub in folders {
            for path in files_in(&sub) {
                let file = name(&path).to_ascii_lowercase();
                if file.contains("licen") || file.contains("copying") {
                    files.push((
                        format!("{target}/{}/{}", name(&sub), name(&path)),
                        read(&path, ""),
                    ));
                }
            }
        }
        let mut note = String::new();
        if own == 0 {
            if licence.contains("Apache-2.0") {
                files.push((format!("{target}/LICENSE-APACHE"), apache.clone()));
                note = " (no licence file in the crate: used under Apache-2.0)".into();
            } else if licence == "BSL-1.0" || licence == "CC0-1.0" {
                note = " (no licence file in the crate; none required in object code)".into();
            } else {
                panic!("{crate_name} {version} ({licence}) has no licence file");
            }
        }
        let authors: Vec<&str> = package["authors"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        rows.push(format!(
            "| {crate_name} | {version} | {licence} | {}{note} |",
            authors.join(", ")
        ));
    }
    let mut index = format!(
        "# Rust crates in cad2bem.exe\n\nThe crates linked into cad2bem {} for {target}, from Cargo.lock.\n\
         Each folder holds the licence files the crate ships.\n\n| Crate | Version | Licence | Authors |\n|---|---|---|---|\n",
        env::var("CARGO_PKG_VERSION").unwrap()
    );
    for row in rows {
        index.push_str(&row);
        index.push('\n');
    }
    files.push(("crates/README.md".into(), index.into_bytes()));
}

/// Collect the bundle and write `licenses.rs` into `OUT_DIR`: every distinct text once, and the files by path.
pub fn generate() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let static_cad = env::var_os("CARGO_FEATURE_STATIC_GMSH").is_some();
    let mut files = Vec::new();
    for file in ["LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md"] {
        files.push((
            file.to_string(),
            read(&root.join(file), "part of the repository"),
        ));
    }
    if static_cad {
        native(&root, &mut files);
    }
    crates(&root, static_cad, &mut files);
    let texts = out.join("licenses");
    let _ = fs::remove_dir_all(&texts);
    fs::create_dir_all(&texts).unwrap();
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    let mut seen: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut table = String::new();
    for (path, bytes) in &files {
        let index = *seen.entry(bytes.clone()).or_insert_with(|| {
            blobs.push(bytes.clone());
            blobs.len() - 1
        });
        writeln!(table, "    ({path:?}, {index}),").unwrap();
    }
    let mut code = format!(
        "/// Every distinct licence text.\nstatic TEXTS: [&[u8]; {}] = [\n",
        blobs.len()
    );
    for (k, blob) in blobs.iter().enumerate() {
        fs::write(texts.join(k.to_string()), blob).unwrap();
        writeln!(
            code,
            "    include_bytes!(concat!(env!(\"OUT_DIR\"), \"/licenses/{k}\")),"
        )
        .unwrap();
    }
    write!(
        code,
        "];\n/// The licence files by path, each with its text in `TEXTS`.\nstatic FILES: [(&str, usize); {}] = [\n{table}];\n",
        files.len()
    )
    .unwrap();
    fs::write(out.join("licenses.rs"), code).unwrap();
}

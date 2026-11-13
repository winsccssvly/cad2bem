// SPDX-License-Identifier: GPL-3.0-or-later
//! The Windows manifest of the executable, the licence files it carries, and explicit opt-in static CAD linkage.
//! Default bridge builds need no native build environment.

#[path = "build/licenses.rs"]
mod licenses;

use std::{env, fs, path::PathBuf, process::Command};

/// The executable's icon: `resources/cad2bem.rc` compiled with the Windows SDK's `rc.exe` (from `RC`, or the newest
/// SDK under Windows Kits) and linked as a resource. Without `rc.exe` the build goes on without an icon.
fn icon() {
    let resources = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("resources");
    for name in ["cad2bem.rc", "cad2bem.ico"] {
        println!("cargo:rerun-if-changed={}", resources.join(name).display());
    }
    println!("cargo:rerun-if-env-changed=RC");
    let rc = env::var_os("RC").map(PathBuf::from).or_else(|| {
        let kits = PathBuf::from(env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
        let mut versions: Vec<PathBuf> = fs::read_dir(kits)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path().join("x64/rc.exe")))
            .filter(|p| p.is_file())
            .collect();
        versions.sort();
        versions.pop()
    });
    let Some(rc) = rc else {
        println!("cargo:warning=rc.exe not found (set RC): the executable has no icon");
        return;
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("cad2bem.res");
    let status = Command::new(&rc)
        .args(["/nologo", "/fo"])
        .arg(&out)
        .arg(resources.join("cad2bem.rc"))
        .status();
    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg-bin=cad2bem={}", out.display());
        }
        _ => println!(
            "cargo:warning={} failed: the executable has no icon",
            rc.display()
        ),
    }
}

fn main() {
    licenses::generate();
    // One executable for the GUI and the command line: a console program (shells wait for it) whose manifest
    // asks Windows for no console of its own when it is opened from Explorer.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("resources")
            .join("windows.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-bin=cad2bem=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=cad2bem=/MANIFESTINPUT:{}",
            manifest.display()
        );
        icon();
    }
    println!("cargo:rerun-if-env-changed=CAD2BEM_STATIC_CAD_PREFIX");
    if env::var_os("CARGO_FEATURE_STATIC_GMSH").is_none() {
        return;
    }
    assert!(
        env::var("TARGET").unwrap() == "x86_64-pc-windows-msvc",
        "Static CAD currently requires Windows x64 MSVC"
    );
    assert!(
        env::var("CARGO_CFG_TARGET_FEATURE")
            .unwrap_or_default()
            .split(',')
            .any(|f| f == "crt-static"),
        "Static CAD requires RUSTFLAGS=-C target-feature=+crt-static to match the native SDK"
    );
    let prefix = PathBuf::from(
        env::var_os("CAD2BEM_STATIC_CAD_PREFIX")
            .expect("Set CAD2BEM_STATIC_CAD_PREFIX to the output of scripts/build_static_cad.ps1"),
    );
    let manifest = prefix.join("static-link.txt");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let links = fs::read_to_string(&manifest)
        .expect("Static CAD SDK is incomplete: missing static-link.txt");
    println!(
        "cargo:rustc-link-search=native={}",
        prefix.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=gmsh");
    for line in links.lines().filter(|s| !s.is_empty()) {
        if let Some(name) = line.strip_prefix("system:") {
            println!("cargo:rustc-link-lib={name}");
        } else if let Some(name) = line.strip_prefix("static:") {
            assert!(
                prefix.join("lib").join(format!("{name}.lib")).is_file(),
                "Missing static library {name}"
            );
            println!("cargo:rustc-link-lib=static={name}");
        } else {
            panic!("Invalid static SDK link record {line}");
        }
    }
}

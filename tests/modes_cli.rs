// SPDX-License-Identifier: GPL-3.0-or-later
//! Mode-dependent preflight: options a mode does not use and outputs it cannot write fail before any worker.

mod common;

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

fn run(mut command: Command) -> Output {
    common::run_within(&mut command, Duration::from_secs(40))
}

fn command(source: &Path, mode: &str, output: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cad2bem"));
    command
        .arg("convert")
        .arg(source)
        .args(["--mode", mode, "--out"])
        .arg(output);
    command
}

#[test]
fn irrelevant_sizes_and_high_order_gdf_fail_before_any_worker() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/column.step");
    let directory = tempfile::tempdir().unwrap();
    for (mode, suffix, options, message) in [
        ("ho", "gdf", vec![], "HO exports require .hbg"),
        (
            "ho",
            "hbg",
            vec!["--panel-size", "2"],
            "Panel size is not used",
        ),
        (
            "lo-free",
            "hbg",
            vec!["--patch-size", "2"],
            "Patch size is not used",
        ),
        (
            "lo-str",
            "hbg",
            vec!["--patch-size", "NaN"],
            "Patch size must be positive",
        ),
    ] {
        let path = directory.path().join(format!("result.{suffix}"));
        let mut cmd = command(&source, mode, &path);
        cmd.args(options);
        let result = run(cmd);
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!path.exists());
    }
}

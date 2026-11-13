// SPDX-License-Identifier: GPL-3.0-or-later
//! The free panels of the interior surface meet the body at its waterline nodes, with every panel mesher: the
//! interior is meshed with the body on the body's own waterline curves.

mod common;

use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    process::Command,
    time::Duration,
};

type Panel = [[f64; 3]; 4];

fn panels(path: &Path) -> Vec<Panel> {
    let document: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    serde_json::from_value(document["vertices"].clone()).unwrap()
}

/// A vertex as a key, to 1e-6 m.
fn key(p: [f64; 3]) -> [i64; 3] {
    p.map(|x| (x * 1e6).round() as i64)
}

#[test]
#[cfg_attr(
    not(feature = "static-gmsh"),
    ignore = "needs a Gmsh SDK (CAD2BEM_GMSH_LIBRARY) or the static build"
)]
fn interior_panels_share_the_body_waterline_nodes_with_every_mesher() {
    let directory = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/oc3.step");
    for method in ["recombined", "qqs", "quads"] {
        let out = directory.path().join(format!("{method}.hbg"));
        let output = common::run_within(
            Command::new(env!("CARGO_BIN_EXE_cad2bem"))
                .arg("convert")
                .arg(&source)
                .args([
                    "--mode",
                    "lo-free",
                    "--panel-size",
                    "2",
                    "--panel-method",
                    method,
                ])
                .args([
                    "--cut-waterline",
                    "--symmetry",
                    "xz_and_yz",
                    "--interior",
                    "--out",
                ])
                .arg(&out),
            Duration::from_secs(120),
        );
        assert!(
            output.status.success(),
            "{method}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // One file: the interior surface is its panels in z = 0 (as the solver's "detect" finds
        // it), the body the others.
        let up = |p: &Panel| {
            let d = |a: usize, b: usize| [0, 1].map(|k| p[a][k] - p[b][k]);
            let (u, v) = (d(2, 0), d(3, 1));
            p.iter().all(|x| x[2].abs() < 1e-9) && u[0] * v[1] - u[1] * v[0] > 0.
        };
        let (interior, body): (Vec<Panel>, Vec<Panel>) = panels(&out).into_iter().partition(up);
        assert!(
            !interior.is_empty(),
            "{method}: no interior panels in the file"
        );
        let body: BTreeSet<[i64; 3]> = body
            .iter()
            .flatten()
            .filter(|p| p[2].abs() < 1e-9)
            .map(|&p| key(p))
            .collect();
        // The boundary of the interior: the edges of one panel only, less those on the symmetry planes.
        let mut edges: HashMap<([i64; 3], [i64; 3]), usize> = HashMap::new();
        for panel in interior {
            for k in 0..4 {
                let (a, b) = (key(panel[k]), key(panel[(k + 1) % 4]));
                if a != b {
                    *edges.entry((a.min(b), a.max(b))).or_default() += 1;
                }
            }
        }
        let waterline: BTreeSet<[i64; 3]> = edges
            .iter()
            .filter(|&(_, &n)| n == 1)
            .flat_map(|(&(a, b), _)| [a, b])
            .filter(|p| p[0] != 0 && p[1] != 0)
            .collect();
        assert!(
            waterline.len() >= 3,
            "{method}: {} waterline nodes",
            waterline.len()
        );
        let apart: Vec<_> = waterline.difference(&body).collect();
        assert!(
            apart.is_empty(),
            "{method}: {} of {} interior waterline nodes are not body nodes",
            apart.len(),
            waterline.len()
        );
    }
}

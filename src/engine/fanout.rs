// SPDX-License-Identifier: GPL-3.0-or-later
//! Spline patches of the body faces made by several worker processes at once. Each worker opens its own Gmsh,
//! prepares the same CAD (the same preparation gives the same face numbers) and fits the faces it is given;
//! the conversion worker merges their patches in the order one process would have made them.

use super::{
    bspline::Patch,
    ho,
    patches::{Converter, QuadPatch},
    prepare,
};
use crate::{
    cad::session::Session,
    conversion::{PatchMethod, Spec},
};
use anyhow::{Context, Result};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Command, Stdio},
};

/// The environment variable that sets how many patch workers a conversion uses (1 or unset: none).
pub const WORKERS: &str = "CAD2BEM_PATCH_WORKERS";

/// The patch workers a conversion of `faces` faces uses: none (1) unless asked for.
pub fn workers(faces: usize) -> usize {
    std::env::var(WORKERS)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 32)
        .min(faces / 2)
        .max(1)
}

/// What a patch worker is asked to fit.
#[derive(Serialize, Deserialize)]
struct Request {
    spec: Spec,
    faces: Vec<i32>,
    /// Split faces with tri3 (another worker's split faces folded with the chosen method).
    tri3: bool,
}

/// What a patch worker made, as [`Converter`] would hold it after [`Converter::patches`].
struct Outcome {
    patches: Vec<QuadPatch>,
    paths: BTreeMap<i32, &'static str>,
    remeshed: BTreeMap<i32, f64>,
    forced_failed: Vec<i32>,
    method_fallback: bool,
}

/// Run as a patch worker: fit the requested faces and write the patches.
pub fn execute(request: &Path, result: &Path) -> Result<()> {
    let request: Request = serde_json::from_slice(&fs::read(request)?)?;
    let gmsh = Session::open(None)?;
    let options = ho::options(&request.spec)?;
    let prepared = prepare::body(&gmsh, &request.spec.source, &options, &mut |_, _| {})?;
    let mut settings = ho::converter_settings(&request.spec, prepared.scale)?;
    if request.tri3 {
        settings.method = PatchMethod::Tri3;
    }
    let mut converter = Converter::new(&gmsh, settings);
    converter.surfaces = super::surfaces::read(&request.spec.source);
    let patches = converter.patches(&request.faces)?;
    let document = json!({
        "patches": patches.iter().map(quad_to_json).collect::<Vec<_>>(),
        "paths": converter.paths.iter().map(|(f, p)| (f.to_string(), *p)).collect::<BTreeMap<_, _>>(),
        "remeshed": converter.remeshed.iter().map(|(f, s)| (f.to_string(), *s)).collect::<BTreeMap<_, _>>(),
        "forced_failed": converter.forced_failed,
        "method_fallback": converter.method_fallback || request.tri3,
    });
    fs::write(result, serde_json::to_vec(&document)?)?;
    Ok(())
}

/// The patches of `faces` made by `n` patch workers, merged into `converter` (paths, remeshed faces, forced
/// paths that failed, method fallback) as if it had made them itself.
pub fn patches(
    converter: &mut Converter,
    spec: &Spec,
    faces: &[i32],
    n: usize,
) -> Result<Vec<QuadPatch>> {
    // Every n-th face to each worker: neighbouring faces are often alike, so the work spreads evenly.
    let groups: Vec<Vec<i32>> = (0..n)
        .map(|k| faces.iter().skip(k).step_by(n).copied().collect())
        .collect();
    let mut outcomes = run(spec, &groups, &vec![false; n])?;
    // One process makes all split faces again with tri3 when any of them folded: so do the workers.
    if spec.settings.patch_method != PatchMethod::Tri3 && outcomes.iter().any(|o| o.method_fallback)
    {
        let again: Vec<bool> = outcomes
            .iter()
            .map(|o| !o.method_fallback && o.paths.values().any(|p| *p == "split"))
            .collect();
        if again.iter().any(|&a| a) {
            let redone = run(spec, &groups, &again)?;
            for (k, outcome) in redone.into_iter().enumerate() {
                if again[k] {
                    outcomes[k] = outcome;
                }
            }
        }
    }
    let position: BTreeMap<i32, usize> = faces.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let mut merged = Vec::new();
    for (k, outcome) in outcomes.into_iter().enumerate() {
        converter.paths.extend(outcome.paths);
        converter.remeshed.extend(outcome.remeshed);
        converter.forced_failed.extend(outcome.forced_failed);
        converter.method_fallback |= outcome.method_fallback;
        for mut q in outcome.patches {
            // Repaired regions are told apart by number: keep the numbers of different workers apart.
            if q.disjoint_group >= 0 {
                q.disjoint_group = q.disjoint_group * n as i64 + k as i64;
            }
            merged.push(q);
        }
    }
    converter.forced_failed.sort_by_key(|f| position[f]);
    // One process lists the faces that kept a layout first, then the split faces, each in face order.
    merged.sort_by_key(|q| (q.path == "split", position[&q.face]));
    Ok(merged)
}

/// Start one worker per group (split faces with tri3 where its flag says so) and wait for all of them.
fn run(spec: &Spec, groups: &[Vec<i32>], tri3: &[bool]) -> Result<Vec<Outcome>> {
    let directory = tempfile::Builder::new()
        .prefix("cad2bem-patches-")
        .tempdir()?;
    let executable = std::env::current_exe()?;
    let mut children = Vec::new();
    for (k, faces) in groups.iter().enumerate() {
        let input = directory.path().join(format!("request-{k}.json"));
        let output = directory.path().join(format!("result-{k}.json"));
        let errors = directory.path().join(format!("stderr-{k}.txt"));
        let request = Request {
            spec: spec.clone(),
            faces: faces.clone(),
            tri3: tri3[k],
        };
        fs::write(&input, serde_json::to_vec(&request)?)?;
        let mut command = Command::new(&executable);
        command
            .arg("--patch-worker")
            .arg(&input)
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&errors)?);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        children.push((
            command.spawn().context("Cannot start a patch worker")?,
            output,
            errors,
        ));
    }
    let mut outcomes = Vec::new();
    let mut failure = None;
    for (mut child, output, errors) in children {
        let status = child.wait()?;
        if failure.is_some() {
            continue;
        }
        if !status.success() {
            let message: String = fs::read_to_string(&errors)
                .unwrap_or_default()
                .chars()
                .take(4000)
                .collect();
            failure = Some(anyhow::anyhow!(
                "Patch worker exited with {status}: {message}"
            ));
            continue;
        }
        outcomes.push(read_outcome(&output)?);
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(outcomes),
    }
}

fn read_outcome(path: &Path) -> Result<Outcome> {
    let document: Value = serde_json::from_slice(&fs::read(path)?)?;
    let faces = |key: &str| -> Result<Vec<(i32, Value)>> {
        document[key]
            .as_object()
            .context("Malformed patch worker result")?
            .iter()
            .map(|(f, v)| Ok((f.parse::<i32>()?, v.clone())))
            .collect()
    };
    Ok(Outcome {
        patches: document["patches"]
            .as_array()
            .context("Malformed patch worker result")?
            .iter()
            .map(quad_from_json)
            .collect::<Result<_>>()?,
        paths: faces("paths")?
            .into_iter()
            .map(|(f, v)| Ok((f, known(v.as_str().unwrap_or_default())?)))
            .collect::<Result<_>>()?,
        remeshed: faces("remeshed")?
            .into_iter()
            .map(|(f, v)| (f, v.as_f64().unwrap_or_default()))
            .collect(),
        forced_failed: serde_json::from_value(document["forced_failed"].clone())?,
        method_fallback: document["method_fallback"].as_bool().unwrap_or(false),
    })
}

/// The names a patch path or exactness can have (the converter keeps them as static strings).
fn known(name: &str) -> Result<&'static str> {
    const NAMES: [&str; 11] = [
        "ogrid",
        "ring",
        "native",
        "direct",
        "single",
        "triangle",
        "bridge",
        "polygon",
        "split",
        "rational",
        "polynomial",
    ];
    NAMES
        .iter()
        .find(|n| **n == name)
        .copied()
        .with_context(|| format!("Unknown patch path {name:?} from a patch worker"))
}

fn points(x: &[DVec3]) -> Value {
    json!(x.iter().map(|p| p.to_array()).collect::<Vec<_>>())
}

fn points_from(value: &Value) -> Result<Vec<DVec3>> {
    let raw: Vec<[f64; 3]> = serde_json::from_value(value.clone())?;
    Ok(raw.into_iter().map(DVec3::from_array).collect())
}

fn quad_to_json(q: &QuadPatch) -> Value {
    let p = &q.patch;
    json!({
        "face": q.face, "samples": points(&q.samples), "n": q.n, "sample_error": q.sample_error,
        "cad_error": q.cad_error, "path": q.path, "face_type": q.face_type, "exact": q.exact,
        "nodes": q.nodes, "disjoint_group": q.disjoint_group, "repair_error": q.repair_error,
        "u_order": p.u_order, "v_order": p.v_order, "u_knots": p.u_knots, "v_knots": p.v_knots,
        "coefficients": points(&p.coefficients), "weights": p.weights,
    })
}

fn quad_from_json(v: &Value) -> Result<QuadPatch> {
    let number = |key: &str| {
        v[key]
            .as_f64()
            .with_context(|| format!("Patch worker result lacks {key}"))
    };
    let count = |key: &str| {
        v[key]
            .as_u64()
            .map(|x| x as usize)
            .with_context(|| format!("Patch worker result lacks {key}"))
    };
    Ok(QuadPatch {
        face: v["face"]
            .as_i64()
            .context("Patch worker result lacks face")? as i32,
        samples: points_from(&v["samples"])?,
        n: count("n")?,
        patch: Patch {
            u_order: count("u_order")?,
            v_order: count("v_order")?,
            u_knots: serde_json::from_value(v["u_knots"].clone())?,
            v_knots: serde_json::from_value(v["v_knots"].clone())?,
            coefficients: points_from(&v["coefficients"])?,
            weights: serde_json::from_value(v["weights"].clone())?,
        },
        sample_error: number("sample_error")?,
        cad_error: number("cad_error")?,
        path: known(v["path"].as_str().unwrap_or_default())?,
        face_type: v["face_type"].as_str().unwrap_or_default().to_string(),
        exact: match v["exact"].as_str() {
            Some(name) => Some(known(name)?),
            None => None,
        },
        nodes: serde_json::from_value(v["nodes"].clone())?,
        disjoint_group: v["disjoint_group"].as_i64().unwrap_or(-1),
        repair_error: v["repair_error"].as_str().map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_survives_the_worker_result() {
        let q = QuadPatch {
            face: 7,
            samples: vec![DVec3::new(0.1, 1. / 3., -2e-17)],
            n: 1,
            patch: Patch {
                u_order: 2,
                v_order: 2,
                u_knots: vec![0., 0., 1., 1.],
                v_knots: vec![0., 0., 1., 1.],
                coefficients: vec![DVec3::new(1. / 7., 2., 3.); 4],
                weights: Some(vec![1., std::f64::consts::FRAC_1_SQRT_2, 1., 1.]),
            },
            sample_error: 1e-12,
            cad_error: 3.3e-9,
            path: "split",
            face_type: "Cylinder".into(),
            exact: Some("rational"),
            nodes: Some([1, 2, 3, 4]),
            disjoint_group: 5,
            repair_error: None,
        };
        let text = serde_json::to_vec(&quad_to_json(&q)).unwrap();
        let back = quad_from_json(&serde_json::from_slice(&text).unwrap()).unwrap();
        assert_eq!(back.patch, q.patch);
        assert_eq!(back.samples, q.samples);
        assert_eq!(
            (back.face, back.path, back.exact),
            (q.face, q.path, q.exact)
        );
        assert_eq!(
            (back.nodes, back.disjoint_group),
            (q.nodes, q.disjoint_group)
        );
        assert_eq!(back.cad_error.to_bits(), q.cad_error.to_bits());
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! The B-spline surfaces stored in IGES and STEP files, so that an untrimmed face keeps its poles instead of
//! being refitted (Gmsh does not expose them).
//!
//! IGES: entity 128 in fixed 80-column records (parameter data in columns 1-64, the directory pointer in
//! 66-72), default delimiters only. STEP: `B_SPLINE_SURFACE_WITH_KNOTS`, simple or complex with
//! `B_SPLINE_SURFACE` and `RATIONAL_B_SPLINE_SURFACE`. Coordinates are those of the file; the converter fits
//! the scale to the model. A record of an unexpected shape is skipped (its face then takes a fitted path).

use super::bspline::Patch;
use glam::DVec3;
use std::{collections::BTreeMap, path::Path};

/// A surface of the file: where it is, and its patch in the file's units and knot parameters.
pub struct Surface {
    pub label: String,
    pub patch: Patch,
}

/// The B-spline surfaces of a STEP or IGES file (orders 2 to 8).
pub fn read(path: &Path) -> Vec<Surface> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let found = if super::super::cad::gmsh::is_iges(path) {
        iges(&text)
    } else {
        step(&text)
    };
    found
        .into_iter()
        .filter(|s| (2..=8).contains(&s.patch.u_order.max(s.patch.v_order)))
        .collect()
}

fn iges_number(s: &str) -> Option<f64> {
    s.trim().replace(['D', 'd'], "E").parse().ok()
}

fn rational(weights: &[f64]) -> bool {
    weights
        .iter()
        .any(|&w| (w - weights[0]).abs() > 1e-12 * weights[0].abs())
}

fn iges(text: &str) -> Vec<Surface> {
    let mut data: BTreeMap<i64, String> = BTreeMap::new();
    for line in text.lines() {
        if line.len() >= 73
            && line.as_bytes()[72] == b'P'
            && let (Some(body), Some(pointer)) = (line.get(..64), line.get(64..72))
            && let Ok(de) = pointer.trim().parse::<i64>()
        {
            data.entry(de).or_default().push_str(body);
        }
    }
    let mut out = Vec::new();
    for (de, record) in data {
        let record = record.split(';').next().unwrap_or("");
        let f: Vec<&str> = record.split(',').collect();
        if f.first().map(|s| s.trim()) != Some("128") {
            continue;
        }
        let parse = || -> Option<Surface> {
            let int = |k: usize| iges_number(f.get(k)?).map(|x| x as usize);
            let (k1, k2, m1, m2) = (int(1)?, int(2)?, int(3)?, int(4)?);
            let prop3 = int(7)?;
            let v: Vec<f64> = f
                .get(10..)?
                .iter()
                .map(|x| iges_number(x))
                .collect::<Option<_>>()?;
            let (n1, n2) = (k1 + m1 + 2, k2 + m2 + 2);
            let n = (k1 + 1) * (k2 + 1);
            let s = v.get(..n1)?.to_vec();
            let t = v.get(n1..n1 + n2)?.to_vec();
            let w = v.get(n1 + n2..n1 + n2 + n)?.to_vec();
            let points = v.get(n1 + n2 + n..n1 + n2 + 4 * n)?;
            let coefficients = points
                .chunks(3)
                .map(|c| DVec3::new(c[0], c[1], c[2]))
                .collect();
            let rational = prop3 == 0 && rational(&w);
            Some(Surface {
                label: format!("IGES DE {de}"),
                patch: Patch {
                    u_order: m1 + 1,
                    v_order: m2 + 1,
                    u_knots: s,
                    v_knots: t,
                    coefficients,
                    weights: rational.then_some(w),
                },
            })
        };
        if let Some(surface) = parse() {
            out.push(surface);
        }
    }
    out
}

/// A STEP parameter value.
#[derive(Clone, Debug)]
enum Value {
    Ref(u64),
    Number(f64),
    Text,
    Null,
    List(Vec<Value>),
    Typed(String, Vec<Value>),
}

impl Value {
    fn list(&self) -> Option<&[Value]> {
        match self {
            Value::List(v) => Some(v),
            _ => None,
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Value::Number(x) => Some(*x),
            _ => None,
        }
    }
}

/// Parse a parameter list `(...)`-free body into values; typed values are `NAME(args)`.
fn parse(text: &str) -> Option<Vec<Value>> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut stack: Vec<(Vec<Value>, Option<String>)> = Vec::new();
    let mut current: Vec<Value> = Vec::new();
    let mut name: Option<String> = None;
    while pos < bytes.len() {
        let c = bytes[pos];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' | b',' => pos += 1,
            b'#' => {
                let end = pos
                    + 1
                    + bytes[pos + 1..]
                        .iter()
                        .take_while(|b| b.is_ascii_digit())
                        .count();
                current.push(Value::Ref(text[pos + 1..end].parse().ok()?));
                pos = end;
            }
            b'\'' => {
                let mut end = pos + 1;
                loop {
                    let next = end + bytes.get(end..)?.iter().position(|&b| b == b'\'')?;
                    if bytes.get(next + 1) == Some(&b'\'') {
                        end = next + 2;
                    } else {
                        end = next + 1;
                        break;
                    }
                }
                current.push(Value::Text);
                pos = end;
            }
            b'.' if bytes.get(pos + 1).is_some_and(|b| b.is_ascii_uppercase()) => {
                let end = pos + 1 + bytes[pos + 1..].iter().position(|&b| b == b'.')? + 1;
                current.push(Value::Text);
                pos = end;
            }
            b'$' | b'*' => {
                current.push(Value::Null);
                pos += 1;
            }
            b'(' => {
                stack.push((std::mem::take(&mut current), name.take()));
                pos += 1;
            }
            b')' => {
                let done = std::mem::take(&mut current);
                let (previous, typed) = stack.pop()?;
                current = previous;
                current.push(match typed {
                    Some(n) => Value::Typed(n, done),
                    None => Value::List(done),
                });
                pos += 1;
            }
            b'A'..=b'Z' | b'_' => {
                let end = pos
                    + bytes[pos..]
                        .iter()
                        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
                        .count();
                name = Some(text[pos..end].to_owned());
                pos = end;
            }
            b'-' | b'+' | b'0'..=b'9' | b'.' => {
                let end = pos
                    + bytes[pos..]
                        .iter()
                        .take_while(|b| {
                            b.is_ascii_digit()
                                || matches!(b, b'.' | b'-' | b'+' | b'E' | b'e' | b'D' | b'd')
                        })
                        .count();
                current.push(Value::Number(
                    text[pos..end].replace(['D', 'd'], "E").parse().ok()?,
                ));
                pos = end;
            }
            _ => return None,
        }
    }
    stack.is_empty().then_some(current)
}

fn step(text: &str) -> Vec<Surface> {
    let data = match (text.find("DATA;"), text.rfind("ENDSEC")) {
        (Some(a), Some(b)) if b > a => &text[a + 5..b],
        _ => return Vec::new(),
    };
    let mut cleaned = String::with_capacity(data.len());
    let mut rest = data;
    while let Some(start) = rest.find("/*") {
        cleaned.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => rest = "",
        }
    }
    cleaned.push_str(rest);
    // instances: #id = body;
    let mut instances: BTreeMap<u64, Vec<(String, Vec<Value>)>> = BTreeMap::new();
    for record in cleaned.split(';') {
        let record = record.trim();
        let Some((id, body)) = record.split_once('=') else {
            continue;
        };
        let Some(id) = id
            .trim()
            .strip_prefix('#')
            .and_then(|x| x.trim().parse::<u64>().ok())
        else {
            continue;
        };
        let body = body.trim();
        if !(body.contains("B_SPLINE_SURFACE") || body.starts_with("CARTESIAN_POINT")) {
            continue;
        }
        let Some(parsed) = parse(body) else { continue };
        let items: Vec<Value> = match parsed.as_slice() {
            [Value::List(inner)] => inner.clone(),
            _ => parsed,
        };
        let typed = items
            .into_iter()
            .filter_map(|v| match v {
                Value::Typed(n, a) => Some((n, a)),
                _ => None,
            })
            .collect();
        instances.insert(id, typed);
    }
    let mut points: BTreeMap<u64, DVec3> = BTreeMap::new();
    for (id, parts) in &instances {
        if let Some((name, args)) = parts.first()
            && name == "CARTESIAN_POINT"
            && let Some(c) = args.get(1).and_then(Value::list)
            && let [x, y, z] = c
            && let (Some(x), Some(y), Some(z)) = (x.number(), y.number(), z.number())
        {
            points.insert(*id, DVec3::new(x, y, z));
        }
    }
    let mut out = Vec::new();
    for (id, parts) in &instances {
        let kinds: BTreeMap<&str, &Vec<Value>> =
            parts.iter().map(|(n, a)| (n.as_str(), a)).collect();
        let Some(a) = kinds.get("B_SPLINE_SURFACE_WITH_KNOTS") else {
            continue;
        };
        let surface = || -> Option<Surface> {
            let (du, dv, ctrl, mu, mv, ku, kv) = if let Some(b) = kinds.get("B_SPLINE_SURFACE") {
                (
                    b.first()?,
                    b.get(1)?,
                    b.get(2)?,
                    a.first()?,
                    a.get(1)?,
                    a.get(2)?,
                    a.get(3)?,
                )
            } else {
                (
                    a.get(1)?,
                    a.get(2)?,
                    a.get(3)?,
                    a.get(8)?,
                    a.get(9)?,
                    a.get(10)?,
                    a.get(11)?,
                )
            };
            let (pu, pv) = (du.number()? as usize + 1, dv.number()? as usize + 1);
            // control points: a list over u of lists over v
            let rows: Vec<Vec<DVec3>> = ctrl
                .list()?
                .iter()
                .map(|row| {
                    row.list()?
                        .iter()
                        .map(|r| match r {
                            Value::Ref(k) => points.get(k).copied(),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .collect::<Option<_>>()?;
            let (nu, nv) = (rows.len(), rows.first()?.len());
            if rows.iter().any(|r| r.len() != nv) {
                return None;
            }
            let knots = |mults: &Value, values: &Value| -> Option<Vec<f64>> {
                let mut out = Vec::new();
                for (m, x) in mults.list()?.iter().zip(values.list()?) {
                    out.extend(std::iter::repeat_n(x.number()?, m.number()? as usize));
                }
                Some(out)
            };
            let (u, v) = (knots(mu, ku)?, knots(mv, kv)?);
            if u.len() != nu + pu || v.len() != nv + pv {
                return None;
            }
            let mut weights = vec![1.; nu * nv];
            if let Some(r) = kinds.get("RATIONAL_B_SPLINE_SURFACE") {
                let w: Vec<Vec<f64>> = r
                    .first()?
                    .list()?
                    .iter()
                    .map(|row| {
                        row.list()?
                            .iter()
                            .map(Value::number)
                            .collect::<Option<Vec<_>>>()
                    })
                    .collect::<Option<_>>()?;
                if w.len() != nu || w.iter().any(|r| r.len() != nv) {
                    return None;
                }
                weights = w.concat();
            }
            let clamped = |x: &[f64], o: usize| {
                let span = x[x.len() - 1] - x[0];
                x[..o].iter().all(|&k| (k - x[0]).abs() <= 1e-12 * span)
                    && x[x.len() - o..]
                        .iter()
                        .all(|&k| (k - x[x.len() - 1]).abs() <= 1e-12 * span)
            };
            if !clamped(&u, pu) || !clamped(&v, pv) {
                return None;
            }
            // the file lists u rows of v points; the patch holds v rows of u points
            let coefficients = (0..nv)
                .flat_map(|j| rows.iter().map(move |row| row[j]))
                .collect();
            let w: Vec<f64> = (0..nv)
                .flat_map(|j| (0..nu).map(move |i| (i, j)))
                .map(|(i, j)| weights[i * nv + j])
                .collect();
            Some(Surface {
                label: format!("STEP #{id}"),
                patch: Patch {
                    u_order: pu,
                    v_order: pv,
                    u_knots: u,
                    v_knots: v,
                    coefficients,
                    weights: rational(&w).then_some(w),
                },
            })
        };
        if let Some(s) = surface() {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_bilinear_surface_is_read() {
        let text = "DATA;\n#1=CARTESIAN_POINT('',(0.,0.,0.));\n#2=CARTESIAN_POINT('',(0.,1.,0.));\n\
            #3=CARTESIAN_POINT('',(2.,0.,0.));\n#4=CARTESIAN_POINT('',(2.,1.,1.));\n\
            #5=B_SPLINE_SURFACE_WITH_KNOTS('',1,1,((#1,#2),(#3,#4)),.UNSPECIFIED.,.F.,.F.,.F.,(2,2),(2,2),\
            (0.,1.),(0.,1.),.UNSPECIFIED.);\nENDSEC;";
        let found = step(text);
        assert_eq!(found.len(), 1);
        let p = &found[0].patch;
        assert_eq!((p.u_order, p.v_order), (2, 2));
        assert!(p.at(1., 1.).distance(DVec3::new(2., 1., 1.)) < 1e-15);
        assert!(p.at(1., 0.).distance(DVec3::new(2., 0., 0.)) < 1e-15);
    }
}

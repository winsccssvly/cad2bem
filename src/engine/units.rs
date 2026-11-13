// SPDX-License-Identifier: GPL-3.0-or-later
//! The length unit a STEP or IGES file states, and the units a conversion request may name.
//!
//! STEP reads the length unit of the `GLOBAL_UNIT_ASSIGNED_CONTEXT` (an `SI_UNIT` with its prefix, or a `CONVERSION_BASED_UNIT` and its
//! `LENGTH_MEASURE_WITH_UNIT`); IGES reads the unit flag of the global section (parameter 14; flag 3, a named
//! unit, by its name in parameter 15).

use anyhow::{Result, bail};
use std::{collections::HashMap, path::Path};

/// The unit a file states: its name and factor to metres. `mixed` with no factor when a STEP file states
/// different units; both empty when the file states none read here.
#[derive(Clone, Debug, PartialEq)]
pub struct FileUnit {
    pub name: Option<String>,
    pub metres: Option<f64>,
}

/// Units a request may give in place of the file's own: name and factor to metres.
const REQUEST_UNITS: [(&str, f64); 5] = [
    ("m", 1.),
    ("mm", 1e-3),
    ("cm", 1e-2),
    ("in", 0.0254),
    ("ft", 0.3048),
];

/// The factor to metres that replaces the file's unit, or None for `file`.
pub fn request_unit(unit: &str) -> Result<Option<f64>> {
    if unit == "file" {
        return Ok(None);
    }
    match REQUEST_UNITS.iter().find(|(name, _)| *name == unit) {
        Some(&(_, factor)) => Ok(Some(factor)),
        None => bail!("unit: file, m, mm, cm, in or ft, got {unit:?}"),
    }
}

fn prefix(name: &str) -> Option<f64> {
    Some(match name {
        "$" => 1.,
        "EXA" => 1e18,
        "PETA" => 1e15,
        "TERA" => 1e12,
        "GIGA" => 1e9,
        "MEGA" => 1e6,
        "KILO" => 1e3,
        "HECTO" => 1e2,
        "DECA" => 1e1,
        "DECI" => 1e-1,
        "CENTI" => 1e-2,
        "MILLI" => 1e-3,
        "MICRO" => 1e-6,
        "NANO" => 1e-9,
        _ => return None,
    })
}

fn iges_unit(flag: i64) -> Option<(&'static str, f64)> {
    Some(match flag {
        1 => ("in", 0.0254),
        2 => ("mm", 1e-3),
        4 => ("ft", 0.3048),
        5 => ("mi", 1609.344),
        6 => ("m", 1.),
        7 => ("km", 1e3),
        8 => ("mil", 2.54e-5),
        9 => ("um", 1e-6),
        10 => ("cm", 1e-2),
        11 => ("uin", 2.54e-8),
        _ => return None,
    })
}

/// The name of a length unit by its factor to metres.
fn unit_name(factor: f64) -> String {
    const NAMES: [(f64, &str); 7] = [
        (1., "m"),
        (1e-3, "mm"),
        (1e-2, "cm"),
        (1e3, "km"),
        (1e-6, "um"),
        (0.0254, "in"),
        (0.3048, "ft"),
    ];
    NAMES
        .iter()
        .find(|(f, _)| (factor / f - 1.).abs() < 1e-9)
        .map(|(_, n)| (*n).to_owned())
        .unwrap_or_else(|| format!("{factor} m"))
}

/// The unit stated by a STEP or IGES file.
pub fn file_unit(path: &Path) -> Result<FileUnit> {
    let text = String::from_utf8_lossy(&std::fs::read(path)?).into_owned();
    let text = strip_comments(&text);
    let iges = super::super::cad::gmsh::is_iges(path);
    Ok(if iges {
        iges_file_unit(&text)
    } else {
        step_file_unit(&text)
    })
}

fn none() -> FileUnit {
    FileUnit {
        name: None,
        metres: None,
    }
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => {
                out.push(' ');
                rest = &rest[start + 2 + end + 2..];
            }
            None => {
                // An unclosed comment is kept as text, as an unmatched regex would leave it.
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn iges_file_unit(text: &str) -> FileUnit {
    let global: String = text
        .lines()
        .filter(|line| line.len() >= 73 && line.as_bytes()[72] == b'G')
        .filter_map(|line| line.get(..72))
        .collect();
    let params = iges_global(&global);
    let Some(mut flag) = params
        .get(13)
        .and_then(|p| p.trim().parse::<f64>().ok())
        .map(|f| f as i64)
    else {
        return none();
    };
    if flag == 3 && params.len() > 14 {
        flag = match params[14].trim().to_ascii_uppercase().as_str() {
            "IN" | "INCH" => 1,
            "MM" => 2,
            "FT" => 4,
            "MI" => 5,
            "M" => 6,
            "KM" => 7,
            "MIL" => 8,
            "UM" => 9,
            "CM" => 10,
            "UIN" => 11,
            _ => 3,
        };
    }
    match iges_unit(flag) {
        Some((name, metres)) => FileUnit {
            name: Some(name.into()),
            metres: Some(metres),
        },
        None => none(),
    }
}

/// Leading decimal digits of `text` followed by `H`: the Hollerith length and the bytes it used.
fn hollerith(text: &[u8]) -> Option<(usize, usize)> {
    let digits = text.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || text.get(digits) != Some(&b'H') {
        return None;
    }
    let n = std::str::from_utf8(&text[..digits]).ok()?.parse().ok()?;
    Some((n, digits + 1))
}

/// The parameters of an IGES global section, Hollerith strings as their text.
fn iges_global(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let (mut delim, mut end) = (b',', b';');
    let leading = bytes
        .iter()
        .take_while(|&&b| b.is_ascii_whitespace())
        .count();
    let declared = bytes.get(leading..leading + 2) == Some(b"1H") && bytes.len() > leading + 2;
    if declared {
        delim = bytes[leading + 2];
    }
    let mut params: Vec<String> = Vec::new();
    let mut k = 0;
    while k <= bytes.len() {
        while k < bytes.len() && bytes[k] == b' ' {
            k += 1;
        }
        let value;
        if let Some((n, used)) = hollerith(&bytes[k.min(bytes.len())..]) {
            let start = k + used;
            let stop = (start + n).min(bytes.len());
            value = String::from_utf8_lossy(&bytes[start.min(stop)..stop]).into_owned();
            k = start + n;
            while k < bytes.len() && bytes[k] == b' ' {
                k += 1;
            }
        } else {
            let mut j = k;
            while j < bytes.len() && bytes[j] != delim && bytes[j] != end {
                j += 1;
            }
            value = String::from_utf8_lossy(&bytes[k.min(j)..j])
                .trim()
                .to_owned();
            k = j;
        }
        params.push(value);
        if params.len() == 2
            && declared
            && let Some(&first) = params[1].as_bytes().first()
            && first != delim
        {
            end = first;
        }
        if k >= bytes.len() || bytes[k] == end {
            break;
        }
        k += 1;
    }
    params
}

/// Entity number to body (whitespace removed, final `;` dropped) of the DATA records of a STEP file.
fn step_entities(text: &str) -> HashMap<u64, String> {
    let bytes = text.as_bytes();
    // starts: (position of '#', number, position after '=')
    let mut starts = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let digits = bytes[i + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if digits > 0 {
                let mut j = i + 1 + digits;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'=')
                    && let Ok(number) = text[i + 1..i + 1 + digits].parse()
                {
                    starts.push((i, number, j + 1));
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    let mut out = HashMap::new();
    for (k, &(_, number, body)) in starts.iter().enumerate() {
        let stop = match starts.get(k + 1) {
            Some(&(next, _, _)) => next,
            None => text[body..].find("ENDSEC").map_or(text.len(), |e| body + e),
        };
        let segment = text[body..stop].trim_end();
        if let Some(record) = segment.strip_suffix(';') {
            out.insert(
                number,
                record.chars().filter(|c| !c.is_whitespace()).collect(),
            );
        }
    }
    out
}

/// The text between `open` and the next `close` after it, searching from the start of `body`.
fn between<'a>(body: &'a str, open: &str, close: char) -> Option<&'a str> {
    let start = body.find(open)? + open.len();
    let end = body[start..].find(close)?;
    Some(&body[start..start + end])
}

fn reference(text: &str) -> Option<u64> {
    text.strip_prefix('#')?.parse().ok()
}

fn length_unit(entities: &HashMap<u64, String>, k: u64, depth: usize) -> Option<f64> {
    let body = entities.get(&k)?;
    if !body.contains("LENGTH_UNIT") || depth > 8 {
        return None;
    }
    if let Some(start) = body.find("SI_UNIT(") {
        let args = &body[start + "SI_UNIT(".len()..];
        if let Some(comma) = args.find(',')
            && args[comma..].starts_with(",.METRE.)")
        {
            return prefix(args[..comma].trim_matches('.'));
        }
    }
    let conversion = between(body, "CONVERSION_BASED_UNIT('", ')')?;
    let measure = reference(conversion.split_once("',")?.1)?;
    let measure = entities.get(&measure)?;
    let value = between(measure, "LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(", ')')?;
    let rest = measure
        .split_once("LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(")?
        .1;
    let base = reference(rest.split_once("),")?.1.strip_suffix(')')?)?;
    let base = length_unit(entities, base, depth + 1)?;
    Some(value.parse::<f64>().ok()? * base)
}

fn step_file_unit(text: &str) -> FileUnit {
    let entities = step_entities(text);
    let mut found: Vec<f64> = Vec::new();
    for body in entities.values() {
        let Some(context) = between(body, "GLOBAL_UNIT_ASSIGNED_CONTEXT((", ')') else {
            continue;
        };
        for item in context.split(',') {
            if let Some(f) = reference(item).and_then(|k| length_unit(&entities, k, 0))
                && f != 0.
            {
                found.push(f);
            }
        }
    }
    let Some(first) = found.iter().copied().reduce(f64::min) else {
        return none();
    };
    if found.iter().any(|f| (f / first - 1.).abs() > 1e-9) {
        return FileUnit {
            name: Some("mixed".into()),
            metres: None,
        };
    }
    FileUnit {
        name: Some(unit_name(first)),
        metres: Some(first),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_si_and_conversion_based_units() {
        let si = "DATA;\n#1=( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) );\n\
                  #2=( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNIT_ASSIGNED_CONTEXT((#1,#3)) );\n\
                  #3=( NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.) );\nENDSEC;";
        let unit = step_file_unit(si);
        assert_eq!(unit.name.as_deref(), Some("mm"));
        assert_eq!(unit.metres, Some(1e-3));
        let inch = "DATA;\n#1=( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT($,.METRE.) );\n\
                    #2=LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(0.0254),#1);\n\
                    #3=( CONVERSION_BASED_UNIT('INCH',#2) LENGTH_UNIT() NAMED_UNIT(#4) );\n\
                    #5=( GLOBAL_UNIT_ASSIGNED_CONTEXT((#3)) REPRESENTATION_CONTEXT('','') );\nENDSEC;";
        let unit = step_file_unit(inch);
        assert_eq!(unit.name.as_deref(), Some("in"));
        assert!((unit.metres.unwrap() - 0.0254).abs() < 1e-15);
    }

    #[test]
    fn iges_flag_and_named_unit() {
        let global =
            "1H,,1H;,4HTEST,8Hfile.igs,6Hsender,6Hcad2bm,32,38,6,308,15,4HTEST,1.,2,2HMM,1,1.;";
        assert_eq!(iges_global(global)[13], "2");
        assert_eq!(
            iges_file_unit(&iges_lines(global)).name.as_deref(),
            Some("mm")
        );
        let named = global.replace(",2,2HMM,", ",3,4HINCH,");
        assert_eq!(iges_file_unit(&iges_lines(&named)).metres, Some(0.0254));
    }

    fn iges_lines(global: &str) -> String {
        global
            .as_bytes()
            .chunks(72)
            .enumerate()
            .map(|(i, c)| format!("{:<72}G{:>7}\n", String::from_utf8_lossy(c), i + 1))
            .collect()
    }

    #[test]
    fn request_units() {
        assert_eq!(request_unit("file").unwrap(), None);
        assert_eq!(request_unit("ft").unwrap(), Some(0.3048));
        assert!(request_unit("km").is_err());
    }
}

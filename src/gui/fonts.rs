// SPDX-License-Identifier: GPL-3.0-or-later
//! The GUI fonts, read from the operating system when the window opens: the executable carries no font of its
//! own. Windows has Segoe UI and Consolas; other systems the common free families. Fallback fonts add symbols
//! and Korean (file names) where the system has them.

use anyhow::{Result, bail};
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Text fonts by preference: the first one found is used.
const PROPORTIONAL: &[&str] = &[
    "segoeui.ttf",
    "DejaVuSans.ttf",
    "NotoSans-Regular.ttf",
    "LiberationSans-Regular.ttf",
    "Arial.ttf",
];
/// Fixed-width fonts by preference (the text font when none is found).
const MONOSPACE: &[&str] = &[
    "consola.ttf",
    "DejaVuSansMono.ttf",
    "NotoSansMono-Regular.ttf",
    "LiberationMono-Regular.ttf",
];
/// Fonts for the characters the text font lacks, each used when found.
const FALLBACK: &[&str] = &[
    "seguisym.ttf",
    "malgun.ttf",
    "NanumGothic.ttf",
    "DejaVuSans.ttf",
];

/// The folders searched for font files, the system's first.
fn folders() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(windows) = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot")) {
        out.push(Path::new(&windows).join("Fonts"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push(Path::new(&local).join("Microsoft/Windows/Fonts"));
    }
    for folder in [
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/Library/Fonts",
        "/System/Library/Fonts",
    ] {
        out.push(PathBuf::from(folder));
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(Path::new(&home).join(".local/share/fonts"));
        out.push(Path::new(&home).join(".fonts"));
    }
    out
}

/// The path of a font file named `name` (any letter case) in `folder` or a folder below it, three levels deep.
fn find_in(folder: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(folder).ok()?.flatten().collect();
    for entry in &entries {
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
            && entry.path().is_file()
        {
            return Some(entry.path());
        }
    }
    if depth == 0 {
        return None;
    }
    entries
        .iter()
        .filter(|entry| entry.path().is_dir())
        .find_map(|entry| find_in(&entry.path(), name, depth - 1))
}

/// The contents of the first font of `names` found in `folders`, with its name.
fn first(folders: &[PathBuf], names: &[&str]) -> Option<(String, Vec<u8>)> {
    names.iter().find_map(|name| {
        let path = folders.iter().find_map(|folder| find_in(folder, name, 3))?;
        Some((name.to_string(), std::fs::read(path).ok()?))
    })
}

/// Install the system fonts in `context`; an error names the fonts looked for when none is found.
pub fn install(context: &egui::Context) -> Result<()> {
    let folders = folders();
    let Some((text, text_data)) = first(&folders, PROPORTIONAL) else {
        bail!(
            "No system font for the window was found (looked for {} in {})",
            PROPORTIONAL.join(", "),
            folders
                .iter()
                .map(|f| f.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    let mut fonts = FontDefinitions::empty();
    fonts
        .font_data
        .insert(text.clone(), Arc::new(FontData::from_owned(text_data)));
    let mono = match first(&folders, MONOSPACE) {
        Some((name, data)) => {
            fonts
                .font_data
                .insert(name.clone(), Arc::new(FontData::from_owned(data)));
            name
        }
        None => text.clone(),
    };
    let mut fallback = Vec::new();
    for name in FALLBACK {
        if fonts.font_data.contains_key(*name) {
            continue;
        }
        if let Some((name, data)) = first(&folders, &[name]) {
            fonts
                .font_data
                .insert(name.clone(), Arc::new(FontData::from_owned(data)));
            fallback.push(name);
        }
    }
    let proportional = std::iter::once(text.clone()).chain(fallback.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Proportional, proportional.collect());
    let monospace = std::iter::once(mono)
        .chain(std::iter::once(text))
        .chain(fallback);
    let mut monospace: Vec<String> = monospace.collect();
    monospace.dedup();
    fonts.families.insert(FontFamily::Monospace, monospace);
    context.set_fonts(fonts);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_font_is_found_in_any_letter_case_and_below_the_folder() {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("truetype").join("dejavu");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("DejaVuSans.ttf"), b"font").unwrap();
        std::fs::write(root.path().join("SEGOEUI.TTF"), b"segoe").unwrap();
        let folders = vec![root.path().to_path_buf()];
        let (name, data) = first(&folders, PROPORTIONAL).unwrap();
        assert_eq!(
            (name.as_str(), data.as_slice()),
            ("segoeui.ttf", &b"segoe"[..])
        );
        assert_eq!(first(&folders, &["dejavusans.ttf"]).unwrap().1, b"font");
        assert!(first(&folders, MONOSPACE).is_none());
        assert!(find_in(root.path(), "DejaVuSans.ttf", 0).is_none());
    }
}

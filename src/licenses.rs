// SPDX-License-Identifier: GPL-3.0-or-later
//! The licence files the executable carries, collected when it is built (`build/licenses.rs`): cad2bem's own
//! notices, every linked Rust crate's licence files and, in the static build, those of Gmsh with its bundled code,
//! LEMON and OpenCASCADE. `cad2bem licenses` prints them or writes them as a folder; the GUI shows them.

use anyhow::{Context, Result, bail};
use std::{fs, path::Path};

include!(concat!(env!("OUT_DIR"), "/licenses.rs"));

/// The licence files: their paths in a release's `licenses/` folder (cad2bem's own at the top) and texts.
pub fn files() -> impl Iterator<Item = (&'static str, &'static [u8])> {
    FILES.iter().map(|&(path, text)| (path, TEXTS[text]))
}

/// The text of a licence file for display (a file that is not UTF-8 is shown with replacement characters).
pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Every licence file one after the other, a text met before named by the file that first had it.
pub fn printed() -> String {
    let mut out = String::from(
        "cad2bem is free software under the GNU General Public License, version 3 or later. It carries the \
         components below; the licence files follow, by component.\n",
    );
    let mut first: Vec<Option<&str>> = vec![None; TEXTS.len()];
    for &(path, text) in FILES.iter() {
        out.push_str(&format!("\n==== {path} ====\n"));
        match first[text] {
            Some(earlier) => out.push_str(&format!("(the same text as {earlier})\n")),
            None => {
                first[text] = Some(path);
                out.push_str(&self::text(TEXTS[text]));
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Write the licence files into `folder`, which must not exist (nothing is overwritten).
pub fn write(folder: &Path) -> Result<()> {
    if folder.exists() {
        bail!("{} exists: choose a new folder", folder.display());
    }
    for (path, bytes) in files() {
        let target = folder.join(path);
        fs::create_dir_all(target.parent().unwrap())
            .with_context(|| format!("Cannot make the folder of {}", target.display()))?;
        fs::write(&target, bytes).with_context(|| format!("Cannot write {}", target.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_executable_carries_its_notices_and_every_crate_licence() {
        let paths: Vec<&str> = files().map(|(path, _)| path).collect();
        for own in [
            "LICENSE",
            "NOTICE",
            "THIRD_PARTY_NOTICES.md",
            "crates/README.md",
        ] {
            assert!(paths.contains(&own), "{own} missing");
        }
        let licence = files().find(|(p, _)| *p == "LICENSE").unwrap().1;
        assert!(text(licence).contains("GNU GENERAL PUBLIC LICENSE"));
        // A direct dependency and a crate it brings in.
        assert!(paths.iter().any(|p| p.starts_with("crates/eframe-")));
        assert!(paths.iter().any(|p| p.starts_with("crates/wgpu-core-")));
        // No bundled font.
        assert!(
            !paths
                .iter()
                .any(|p| p.contains("epaint_default_fonts") || p.ends_with(".ttf"))
        );
        let all = printed();
        assert!(all.contains("==== crates/README.md ====") && all.contains("(the same text as"));
    }

    #[test]
    fn writing_makes_the_folder_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("licenses");
        write(&folder).unwrap();
        assert!(folder.join("LICENSE").is_file() && folder.join("crates/README.md").is_file());
        assert!(fs::read_dir(folder.join("crates")).unwrap().count() > 50);
        assert!(write(&folder).is_err());
    }
}

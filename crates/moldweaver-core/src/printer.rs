//! Printer profiles (build volumes). Presets ship as data in
//! `data/printers.json`; users can add their own, stored in the same format.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const PRESETS_JSON: &str = include_str!("../data/printers.json");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrinterProfile {
    pub name: String,
    /// Usable plate width (X), mm.
    pub bed_x: f64,
    /// Usable plate depth (Y), mm.
    pub bed_y: f64,
    /// Maximum print height, mm.
    pub max_z: f64,
}

impl PrinterProfile {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("printer name is empty");
        }
        for (what, v) in [("width", self.bed_x), ("depth", self.bed_y), ("height", self.max_z)] {
            if !(v.is_finite() && v > 10.0 && v < 10_000.0) {
                bail!("printer {what} {v} mm is out of range");
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct ProfileFile {
    format: String,
    version: u32,
    printers: Vec<PrinterProfile>,
}

/// Built-in presets.
pub fn presets() -> Vec<PrinterProfile> {
    parse_profiles(PRESETS_JSON).expect("bundled printer presets are valid")
}

pub fn parse_profiles(json: &str) -> Result<Vec<PrinterProfile>> {
    let f: ProfileFile = serde_json::from_str(json).context("invalid printer profile file")?;
    if f.format != "moldweaver-printers" {
        bail!("not a MoldWeaver printer profile file");
    }
    if f.version > 1 {
        bail!("printer profile file version {} is newer than this MoldWeaver", f.version);
    }
    for p in &f.printers {
        p.validate()?;
    }
    Ok(f.printers)
}

pub fn profiles_to_json(printers: &[PrinterProfile]) -> String {
    serde_json::to_string_pretty(&ProfileFile {
        format: "moldweaver-printers".into(),
        version: 1,
        printers: printers.to_vec(),
    })
    .expect("profiles serialize")
}

/// Load user profiles from `path` (missing file = none).
pub fn load_user_profiles(path: &Path) -> Result<Vec<PrinterProfile>> {
    match std::fs::read_to_string(path) {
        Ok(s) => parse_profiles(&s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save_user_profiles(path: &Path, printers: &[PrinterProfile]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, profiles_to_json(printers)).with_context(|| format!("writing {}", path.display()))
}

/// Presets followed by user profiles (a user profile with a preset's name
/// replaces it).
pub fn all_profiles(user: &[PrinterProfile]) -> Vec<PrinterProfile> {
    let mut all: Vec<PrinterProfile> = presets()
        .into_iter()
        .filter(|p| !user.iter().any(|u| u.name == p.name))
        .collect();
    all.extend(user.iter().cloned());
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_load() {
        let p = presets();
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].bed_x, p[0].bed_y, p[0].max_z), (256.0, 256.0, 256.0));
        assert_eq!((p[1].bed_x, p[1].bed_y, p[1].max_z), (220.0, 220.0, 250.0));
    }

    #[test]
    fn user_profiles_roundtrip_and_override() {
        let dir = std::env::temp_dir().join(format!("mw-printers-{}", std::process::id()));
        let path = dir.join("printers.json");
        let mine = vec![PrinterProfile { name: "Bambu Lab P2S".into(), bed_x: 250.0, bed_y: 250.0, max_z: 250.0 }];
        save_user_profiles(&path, &mine).unwrap();
        let loaded = load_user_profiles(&path).unwrap();
        assert_eq!(loaded, mine);
        let all = all_profiles(&loaded);
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().find(|p| p.name == "Bambu Lab P2S").unwrap().bed_x, 250.0);
        std::fs::remove_dir_all(dir).ok();
        assert!(load_user_profiles(Path::new("/nonexistent/x.json")).unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_profiles() {
        assert!(parse_profiles(r#"{"format":"moldweaver-printers","version":1,"printers":[{"name":"x","bed_x":-1,"bed_y":100,"max_z":100}]}"#).is_err());
        assert!(parse_profiles(r#"{"format":"other","version":1,"printers":[]}"#).is_err());
    }
}

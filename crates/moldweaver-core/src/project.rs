//! Versioned project files (JSON).
//!
//! ```json
//! {
//!   "format": "moldweaver-project",
//!   "version": 1,
//!   "model": { "path": "dog.stl", "repair": false },
//!   "plane": { "tilt_x_deg": 0, "tilt_y_deg": 0, "spin_deg": 0, "offset_mm": 0 },
//!   "mold_type": "two_part_box",
//!   "params": { "margin": 10.0, ... },
//!   "printer": "Bambu Lab P2S"
//! }
//! ```
//! The model is referenced by path (relative to the project file) and/or
//! embedded as base64 little-endian `f32` positions and `u32` indices.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use base64::Engine;
use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::frame::PartingPlane;
use crate::mesh::{repair, TriMesh};
use crate::molds::{ParamValues, DEFAULT_MOLD_TYPE};

pub const FORMAT: &str = "moldweaver-project";
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub format: String,
    pub version: u32,
    pub model: ModelSource,
    #[serde(default)]
    pub plane: PartingPlane,
    #[serde(default = "default_mold")]
    pub mold_type: String,
    #[serde(default)]
    pub params: ParamValues,
    #[serde(default)]
    pub printer: String,
}

fn default_mold() -> String {
    DEFAULT_MOLD_TYPE.to_string()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelSource {
    /// Model file, relative to the project file or absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Embedded mesh (used instead of `path` when present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded: Option<EmbeddedMesh>,
    /// Run the basic repair after loading `path`.
    #[serde(default)]
    pub repair: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedMesh {
    /// Always "f32le-u32le-base64" in version 1.
    pub encoding: String,
    pub positions: String,
    pub triangles: String,
}

impl EmbeddedMesh {
    pub const ENCODING: &'static str = "f32le-u32le-base64";

    pub fn encode(mesh: &TriMesh) -> EmbeddedMesh {
        let mut pos = Vec::with_capacity(mesh.positions.len() * 12);
        for p in &mesh.positions {
            for c in [p.x, p.y, p.z] {
                pos.extend_from_slice(&(c as f32).to_le_bytes());
            }
        }
        let mut tri = Vec::with_capacity(mesh.triangles.len() * 12);
        for t in &mesh.triangles {
            for v in t {
                tri.extend_from_slice(&v.to_le_bytes());
            }
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        EmbeddedMesh {
            encoding: Self::ENCODING.into(),
            positions: b64.encode(pos),
            triangles: b64.encode(tri),
        }
    }

    pub fn decode(&self) -> Result<TriMesh> {
        if self.encoding != Self::ENCODING {
            bail!("unknown embedded mesh encoding '{}'", self.encoding);
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        let pos = b64.decode(&self.positions).context("embedded positions are not valid base64")?;
        let tri = b64.decode(&self.triangles).context("embedded triangles are not valid base64")?;
        if pos.len() % 12 != 0 || tri.len() % 12 != 0 {
            bail!("embedded mesh has a truncated array");
        }
        let f = |c: &[u8]| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64;
        let positions: Vec<DVec3> = pos.chunks_exact(12).map(|c| DVec3::new(f(&c[0..4]), f(&c[4..8]), f(&c[8..12]))).collect();
        let u = |c: &[u8]| u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        let triangles: Vec<[u32; 3]> = tri.chunks_exact(12).map(|c| [u(&c[0..4]), u(&c[4..8]), u(&c[8..12])]).collect();
        let n = positions.len() as u32;
        if triangles.iter().flatten().any(|&v| v >= n) {
            bail!("embedded mesh has an index out of range");
        }
        Ok(TriMesh { positions, triangles })
    }
}

impl Project {
    pub fn new(model: ModelSource) -> Project {
        Project {
            format: FORMAT.into(),
            version: VERSION,
            model,
            plane: PartingPlane::default(),
            mold_type: default_mold(),
            params: ParamValues::default(),
            printer: String::new(),
        }
    }

    pub fn from_json(json: &str) -> Result<Project> {
        let value: serde_json::Value = serde_json::from_str(json).context("project file is not valid JSON")?;
        let format = value.get("format").and_then(|v| v.as_str()).unwrap_or_default();
        if format != FORMAT {
            bail!("not a MoldWeaver project (format is '{format}')");
        }
        let version = value.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
        let value = migrate(value, version)?;
        serde_json::from_value(value).context("project file has invalid contents")
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("project serializes")
    }

    pub fn load(path: &Path) -> Result<Project> {
        let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Project::from_json(&s).with_context(|| format!("loading {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_json()).with_context(|| format!("writing {}", path.display()))
    }

    /// Load the model mesh. `base_dir` resolves relative paths.
    pub fn load_mesh(&self, base_dir: &Path) -> Result<TriMesh> {
        if let Some(e) = &self.model.embedded {
            return e.decode();
        }
        let Some(path) = &self.model.path else {
            bail!("the project has no model (neither a path nor an embedded mesh)");
        };
        let full = if path.is_absolute() { path.clone() } else { base_dir.join(path) };
        let mesh = crate::io::load_model(&full)?;
        Ok(if self.model.repair { repair(&mesh).0 } else { mesh })
    }
}

/// Upgrade older project versions to the current one.
fn migrate(value: serde_json::Value, version: u64) -> Result<serde_json::Value> {
    match version {
        1 => Ok(value),
        v if v > VERSION as u64 => bail!("this project was saved by a newer MoldWeaver (format version {v})"),
        v => bail!("unsupported project format version {v}"),
    }
}

/// Express `path` relative to `base` when it lies below it.
pub fn relative_to(path: &Path, base: &Path) -> PathBuf {
    path.strip_prefix(base).map(|p| p.to_path_buf()).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::primitives::uv_sphere;
    use crate::molds::ParamValue;

    #[test]
    fn json_roundtrip_with_embedded_mesh() {
        let mesh = uv_sphere(DVec3::ZERO, 5.0, 12, 6);
        let mut p = Project::new(ModelSource { path: Some("x.stl".into()), embedded: Some(EmbeddedMesh::encode(&mesh)), repair: false });
        p.plane.offset_mm = 1.5;
        p.params.set("margin", ParamValue::Float(12.0));
        let back = Project::from_json(&p.to_json()).unwrap();
        assert_eq!(back, p);
        let m = back.load_mesh(Path::new(".")).unwrap();
        assert_eq!(m.triangles, mesh.triangles);
        assert!((m.positions[3] - mesh.positions[3]).length() < 1e-5);
    }

    #[test]
    fn rejects_wrong_format_and_future_versions() {
        assert!(Project::from_json(r#"{"format":"x","version":1}"#).is_err());
        let err = Project::from_json(r#"{"format":"moldweaver-project","version":99,"model":{}}"#).unwrap_err();
        assert!(format!("{err:#}").contains("newer"));
    }

    #[test]
    fn minimal_project_uses_defaults() {
        let p = Project::from_json(r#"{"format":"moldweaver-project","version":1,"model":{"path":"a.stl"}}"#).unwrap();
        assert_eq!(p.mold_type, DEFAULT_MOLD_TYPE);
        assert_eq!(p.plane, PartingPlane::default());
    }
}

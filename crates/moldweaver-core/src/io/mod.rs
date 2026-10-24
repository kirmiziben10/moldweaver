//! Mesh import (STL, OBJ, 3MF) and export (STL, 3MF).

pub mod obj;
pub mod stl;
pub mod threemf;

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::mesh::TriMesh;

/// File formats we can read models from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelFormat {
    Stl,
    Obj,
    ThreeMf,
}

impl ModelFormat {
    pub fn from_path(path: &Path) -> Option<ModelFormat> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "stl" => Some(ModelFormat::Stl),
            "obj" => Some(ModelFormat::Obj),
            "3mf" => Some(ModelFormat::ThreeMf),
            _ => None,
        }
    }

    pub const EXTENSIONS: &'static [&'static str] = &["stl", "STL", "obj", "OBJ", "3mf", "3MF"];
}

/// Load a model file. The result is welded but otherwise unmodified.
pub fn load_model(path: &Path) -> Result<TriMesh> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let Some(format) = ModelFormat::from_path(path) else {
        bail!("unsupported file type: {} (use STL, OBJ or 3MF)", path.display());
    };
    load_model_bytes(&bytes, format).with_context(|| format!("loading {}", path.display()))
}

pub fn load_model_bytes(bytes: &[u8], format: ModelFormat) -> Result<TriMesh> {
    let mesh = match format {
        ModelFormat::Stl => stl::read_stl(bytes)?,
        ModelFormat::Obj => obj::read_obj(std::str::from_utf8(bytes).context("OBJ is not valid UTF-8")?)?,
        ModelFormat::ThreeMf => threemf::read_3mf(bytes)?,
    };
    if mesh.triangles.is_empty() {
        bail!("the file contains no triangles");
    }
    if mesh.positions.iter().any(|p| !p.is_finite()) {
        bail!("the file contains non-finite vertex coordinates");
    }
    let tol = mesh.default_weld_tolerance();
    Ok(mesh.welded(tol))
}

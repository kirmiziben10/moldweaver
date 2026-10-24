//! Minimal Wavefront OBJ reader: vertices and (polygonal) faces.

use anyhow::{bail, Context, Result};
use glam::DVec3;

use crate::mesh::TriMesh;

pub fn read_obj(text: &str) -> Result<TriMesh> {
    let mut mesh = TriMesh::default();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("");
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let mut c = [0.0; 3];
                for v in &mut c {
                    *v = it
                        .next()
                        .context(format!("line {}: vertex needs 3 coordinates", lineno + 1))?
                        .parse()
                        .context(format!("line {}: bad number", lineno + 1))?;
                }
                mesh.positions.push(DVec3::from_array(c));
            }
            Some("f") => {
                let mut idx = Vec::new();
                for tok in it {
                    let first = tok.split('/').next().unwrap_or("");
                    let i: i64 = first.parse().context(format!("line {}: bad face index '{tok}'", lineno + 1))?;
                    let n = mesh.positions.len() as i64;
                    let resolved = if i < 0 { n + i } else { i - 1 };
                    if resolved < 0 || resolved >= n {
                        bail!("line {}: face index {i} out of range", lineno + 1);
                    }
                    idx.push(resolved as u32);
                }
                for k in 1..idx.len().saturating_sub(1) {
                    mesh.triangles.push([idx[0], idx[k], idx[k + 1]]);
                }
            }
            _ => {}
        }
    }
    Ok(mesh)
}

pub fn write_obj(mesh: &TriMesh) -> String {
    use std::fmt::Write;
    let mut s = String::from("# MoldWeaver\n");
    for p in &mesh.positions {
        let _ = writeln!(s, "v {} {} {}", p.x, p.y, p.z);
    }
    for t in &mesh.triangles {
        let _ = writeln!(s, "f {} {} {}", t[0] + 1, t[1] + 1, t[2] + 1);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quads_slashes_and_negative_indices() {
        let src = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1/1/1 2/2/2 3/3/3 4/4/4\nf -4 -2 -1 # comment\n";
        let m = read_obj(src).unwrap();
        assert_eq!(m.positions.len(), 4);
        assert_eq!(m.triangles, vec![[0, 1, 2], [0, 2, 3], [0, 2, 3]]);
    }

    #[test]
    fn rejects_bad_index() {
        assert!(read_obj("v 0 0 0\nf 1 2 3\n").is_err());
    }
}

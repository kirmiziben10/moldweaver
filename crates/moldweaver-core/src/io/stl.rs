//! STL reader (binary and ASCII) and binary writer.

use anyhow::{bail, Context, Result};
use glam::DVec3;

use crate::mesh::TriMesh;

pub fn read_stl(bytes: &[u8]) -> Result<TriMesh> {
    if is_binary(bytes) {
        read_binary(bytes)
    } else {
        read_ascii(std::str::from_utf8(bytes).context("ASCII STL is not valid UTF-8")?)
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    if bytes.len() < 84 {
        return false;
    }
    let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    if 84 + n * 50 == bytes.len() {
        return true;
    }
    // Some exporters write "solid" into binary headers; anything that does not
    // start with "solid" is treated as binary.
    !bytes.trim_ascii_start().starts_with(b"solid")
}

fn read_binary(bytes: &[u8]) -> Result<TriMesh> {
    let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    if bytes.len() < 84 + n * 50 {
        bail!("binary STL is truncated: header says {n} triangles");
    }
    let mut mesh = TriMesh { positions: Vec::with_capacity(n * 3), triangles: Vec::with_capacity(n) };
    let f = |o: usize| f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as f64;
    for i in 0..n {
        let o = 84 + i * 50 + 12;
        let base = mesh.positions.len() as u32;
        for v in 0..3 {
            let p = o + v * 12;
            mesh.positions.push(DVec3::new(f(p), f(p + 4), f(p + 8)));
        }
        mesh.triangles.push([base, base + 1, base + 2]);
    }
    Ok(mesh)
}

fn read_ascii(text: &str) -> Result<TriMesh> {
    let mut mesh = TriMesh::default();
    let mut face: Vec<DVec3> = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("vertex") => {
                let mut c = [0.0; 3];
                for v in &mut c {
                    *v = it
                        .next()
                        .context(format!("line {}: vertex needs 3 coordinates", lineno + 1))?
                        .parse()
                        .context(format!("line {}: bad number", lineno + 1))?;
                }
                face.push(DVec3::from_array(c));
            }
            Some("endloop") => {
                // Facets are triangles in valid STL; fan-triangulate anything else.
                for k in 1..face.len().saturating_sub(1) {
                    let base = mesh.positions.len() as u32;
                    mesh.positions.extend_from_slice(&[face[0], face[k], face[k + 1]]);
                    mesh.triangles.push([base, base + 1, base + 2]);
                }
                face.clear();
            }
            _ => {}
        }
    }
    Ok(mesh)
}

/// Binary STL of a mesh (used for "export single part" and debugging).
pub fn write_binary(mesh: &TriMesh, name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(84 + mesh.triangles.len() * 50);
    let mut header = [0u8; 80];
    let tag = format!("MoldWeaver {name}");
    let n = tag.len().min(80);
    header[..n].copy_from_slice(&tag.as_bytes()[..n]);
    out.extend_from_slice(&header);
    out.extend_from_slice(&(mesh.triangles.len() as u32).to_le_bytes());
    for i in 0..mesh.triangles.len() {
        let nrm = mesh.tri_normal(i);
        for c in [nrm.x, nrm.y, nrm.z] {
            out.extend_from_slice(&(c as f32).to_le_bytes());
        }
        for p in mesh.tri(i) {
            for c in [p.x, p.y, p.z] {
                out.extend_from_slice(&(c as f32).to_le_bytes());
            }
        }
        out.extend_from_slice(&[0, 0]);
    }
    out
}

/// ASCII STL (used by tests to cover the ASCII reader).
pub fn write_ascii(mesh: &TriMesh, name: &str) -> String {
    use std::fmt::Write;
    let mut s = format!("solid {name}\n");
    for i in 0..mesh.triangles.len() {
        let n = mesh.tri_normal(i);
        let _ = writeln!(s, "  facet normal {} {} {}\n    outer loop", n.x, n.y, n.z);
        for p in mesh.tri(i) {
            let _ = writeln!(s, "      vertex {} {} {}", p.x, p.y, p.z);
        }
        s.push_str("    endloop\n  endfacet\n");
    }
    let _ = writeln!(s, "endsolid {name}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::primitives::cuboid;

    #[test]
    fn binary_and_ascii_roundtrip() {
        let cube = cuboid(DVec3::ZERO, DVec3::new(1.0, 2.0, 3.0));
        for bytes in [write_binary(&cube, "cube"), write_ascii(&cube, "cube").into_bytes()] {
            let m = read_stl(&bytes).unwrap();
            assert_eq!(m.triangles.len(), 12);
            let w = m.welded(1e-6);
            assert_eq!(w.positions.len(), 8);
            assert!(w.topology().is_watertight());
            assert!((w.signed_volume() - 6.0).abs() < 1e-5);
        }
    }

    #[test]
    fn binary_header_starting_with_solid() {
        let cube = cuboid(DVec3::ZERO, DVec3::ONE);
        let mut bytes = write_binary(&cube, "x");
        bytes[..5].copy_from_slice(b"solid");
        assert_eq!(read_stl(&bytes).unwrap().triangles.len(), 12);
    }
}

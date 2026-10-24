//! Turning meshes into GPU vertex arrays.

use glam::{DVec3, Vec3};
use moldweaver_core::analysis::FaceClass;
use moldweaver_core::mesh::TriMesh;

use super::render::Vertex;

pub const MODEL_COLOR: [f32; 4] = [0.82, 0.80, 0.76, 1.0];
pub const UNDERCUT_COLOR: [f32; 4] = [0.90, 0.18, 0.15, 1.0];
pub const UPPER_COLOR: [f32; 4] = [0.45, 0.68, 0.92, 1.0];
pub const LOWER_COLOR: [f32; 4] = [0.95, 0.70, 0.40, 1.0];

/// Palette for printed parts.
pub const PART_COLORS: [[f32; 3]; 8] = [
    [0.30, 0.47, 0.75],
    [0.87, 0.52, 0.32],
    [0.33, 0.66, 0.41],
    [0.77, 0.31, 0.32],
    [0.51, 0.45, 0.70],
    [0.58, 0.47, 0.38],
    [0.85, 0.55, 0.76],
    [0.60, 0.60, 0.60],
];

/// Flat-shaded triangles with a colour per face.
pub fn mesh_vertices(mesh: &TriMesh, face_color: impl Fn(usize) -> [f32; 4]) -> Vec<Vertex> {
    let mut out = Vec::with_capacity(mesh.triangles.len() * 3);
    for i in 0..mesh.triangles.len() {
        let n = mesh.tri_normal(i).as_vec3().to_array();
        let color = face_color(i);
        for p in mesh.tri(i) {
            out.push(Vertex { pos: p.as_vec3().to_array(), normal: n, color });
        }
    }
    out
}

pub fn class_color(c: FaceClass, show_sides: bool) -> [f32; 4] {
    match c {
        FaceClass::UpperUndercut | FaceClass::LowerUndercut => UNDERCUT_COLOR,
        FaceClass::Upper if show_sides => UPPER_COLOR,
        FaceClass::Lower if show_sides => LOWER_COLOR,
        _ => MODEL_COLOR,
    }
}

/// Line segments (pairs of points) with one colour.
pub fn line_vertices(segments: &[(Vec3, Vec3)], color: [f32; 4]) -> Vec<Vertex> {
    segments
        .iter()
        .flat_map(|(a, b)| {
            [
                Vertex { pos: a.to_array(), normal: [0.0, 0.0, 1.0], color },
                Vertex { pos: b.to_array(), normal: [0.0, 0.0, 1.0], color },
            ]
        })
        .collect()
}

/// A square plane of half-size `r` in the XY plane at z = 0 (two triangles).
pub fn quad_vertices(r: f32, color: [f32; 4]) -> Vec<Vertex> {
    let p = |x: f32, y: f32| Vertex { pos: [x * r, y * r, 0.0], normal: [0.0, 0.0, 1.0], color };
    vec![p(-1.0, -1.0), p(1.0, -1.0), p(1.0, 1.0), p(-1.0, -1.0), p(1.0, 1.0), p(-1.0, 1.0)]
}

/// Ground grid lines around the origin.
pub fn grid_segments(half: f32, step: f32) -> Vec<(Vec3, Vec3)> {
    let mut out = Vec::new();
    let n = (half / step).ceil() as i32;
    for i in -n..=n {
        let t = i as f32 * step;
        out.push((Vec3::new(t, -half, 0.0), Vec3::new(t, half, 0.0)));
        out.push((Vec3::new(-half, t, 0.0), Vec3::new(half, t, 0.0)));
    }
    out
}

/// Closest hit of a ray with a mesh (Möller–Trumbore), as distance.
pub fn ray_mesh(mesh: &TriMesh, origin: DVec3, dir: DVec3) -> Option<f64> {
    let mut best: Option<f64> = None;
    for i in 0..mesh.triangles.len() {
        let [a, b, c] = mesh.tri(i);
        let e1 = b - a;
        let e2 = c - a;
        let p = dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-12 {
            continue;
        }
        let inv = 1.0 / det;
        let s = origin - a;
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = dir.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let t = e2.dot(q) * inv;
        if t > 0.0 && best.is_none_or(|b| t < b) {
            best = Some(t);
        }
    }
    best
}

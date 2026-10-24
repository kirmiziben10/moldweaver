//! Thin helpers over `manifold-rust` for building mold geometry.

use glam::{DQuat, DVec2, DVec3};
use manifold_rust::linalg::Vec3;
use manifold_rust::manifold::Manifold;
use manifold_rust::types::{Error as ManifoldError, OpType};

use crate::mesh::TriMesh;

pub fn v3(p: DVec3) -> Vec3 {
    Vec3::new(p.x, p.y, p.z)
}

/// Axis-aligned box.
pub fn cuboid(min: DVec3, max: DVec3) -> Manifold {
    let size = (max - min).max(DVec3::splat(1e-6));
    Manifold::cube(v3(size), false).translate(v3(min))
}

/// Number of facets for a circle of radius `r` (about 0.6 mm per facet).
pub fn circle_segments(r: f64) -> i32 {
    ((std::f64::consts::TAU * r / 0.6).ceil() as i32).clamp(16, 96)
}

/// Vertical cylinder (or cone frustum) from `z0` to `z1` centred at `xy`.
pub fn cylinder_z(xy: DVec2, z0: f64, z1: f64, r0: f64, r1: f64) -> Manifold {
    let segs = circle_segments(r0.max(r1));
    Manifold::cylinder(z1 - z0, r0, r1, segs).translate(Vec3::new(xy.x, xy.y, z0))
}

/// Cylinder radius for a printed hole so the inscribed polygon still has the
/// nominal diameter `d`.
pub fn hole_radius(d: f64) -> f64 {
    let r = d * 0.5;
    r / (std::f64::consts::PI / circle_segments(r) as f64).cos()
}

pub fn sphere(c: DVec3, r: f64) -> Manifold {
    Manifold::sphere(r, circle_segments(r)).translate(v3(c))
}

/// Rectangle `[min, max]` in XY at height `z`, as 4 points.
fn rect_pts(min: DVec2, max: DVec2, z: f64) -> [Vec3; 4] {
    [
        Vec3::new(min.x, min.y, z),
        Vec3::new(max.x, min.y, z),
        Vec3::new(max.x, max.y, z),
        Vec3::new(min.x, max.y, z),
    ]
}

/// Frustum between two axis-aligned rectangles at `z0` and `z1`.
pub fn rect_frustum(z0: f64, min0: DVec2, max0: DVec2, z1: f64, min1: DVec2, max1: DVec2) -> Manifold {
    let mut pts = rect_pts(min0, max0, z0).to_vec();
    pts.extend_from_slice(&rect_pts(min1, max1, z1));
    Manifold::hull(&pts)
}

pub fn union_all(parts: Vec<Manifold>) -> Manifold {
    let parts: Vec<Manifold> = parts.into_iter().filter(|m| !m.is_empty()).collect();
    match parts.len() {
        0 => Manifold::empty(),
        1 => parts.into_iter().next().unwrap(),
        _ => Manifold::batch_boolean(&parts, OpType::Add),
    }
}

pub fn subtract_all(base: Manifold, tools: Vec<Manifold>) -> Manifold {
    let tools: Vec<Manifold> = tools.into_iter().filter(|m| !m.is_empty()).collect();
    if tools.is_empty() {
        return base;
    }
    let mut all = vec![base];
    all.extend(tools);
    Manifold::batch_boolean(&all, OpType::Subtract)
}

/// Keep the part of `m` with `z >= z0` (`above = true`) or `z <= z0`.
pub fn trim_z(m: &Manifold, z0: f64, above: bool) -> Manifold {
    if above {
        m.trim_by_plane(Vec3::new(0.0, 0.0, 1.0), z0)
    } else {
        m.trim_by_plane(Vec3::new(0.0, 0.0, -1.0), -z0)
    }
}

pub fn rotate(m: &Manifold, q: DQuat) -> Manifold {
    if q.angle_between(DQuat::IDENTITY).abs() < 1e-12 {
        return m.clone();
    }
    let mat = glam::DMat3::from_quat(q);
    let t = manifold_rust::linalg::Mat3x4::from_cols(
        v3(mat.x_axis),
        v3(mat.y_axis),
        v3(mat.z_axis),
        Vec3::new(0.0, 0.0, 0.0),
    );
    m.transform(&t)
}

/// Is this solid valid and non-empty?
pub fn ok(m: &Manifold) -> bool {
    m.status() == ManifoldError::NoError && !m.is_empty()
}

pub fn to_mesh(m: &Manifold) -> TriMesh {
    TriMesh::from_manifold(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_keeps_requested_side() {
        let c = cuboid(DVec3::splat(-1.0), DVec3::splat(1.0));
        let up = trim_z(&c, 0.5, true);
        let down = trim_z(&c, 0.5, false);
        assert!((up.volume() - 2.0).abs() < 1e-9, "{}", up.volume());
        assert!((down.volume() - 6.0).abs() < 1e-9, "{}", down.volume());
        assert!(up.bounding_box().min.z > 0.49);
        assert!(down.bounding_box().max.z < 0.51);
    }

    #[test]
    fn frustum_volume() {
        let f = rect_frustum(0.0, DVec2::splat(-1.0), DVec2::splat(1.0), 2.0, DVec2::splat(-1.0), DVec2::splat(1.0));
        assert!((f.volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn rotate_quarter_turn() {
        let c = cuboid(DVec3::ZERO, DVec3::new(10.0, 1.0, 1.0));
        let r = rotate(&c, DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2));
        let b = r.bounding_box();
        assert!((b.max.y - 10.0).abs() < 1e-9 && b.min.x.abs() < 1.0 + 1e-9);
    }
}

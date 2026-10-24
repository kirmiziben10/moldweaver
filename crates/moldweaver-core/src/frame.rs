//! The parting plane and the "mold frame".
//!
//! Every mold generator works in a mold frame where the parting plane is
//! `z = 0`, the pull direction of the upper half is `+Z`, and the model's
//! bounding box is centred on the origin in X and Y. The parting plane is
//! stored relative to the model so it survives reloading the model file.

use glam::{DMat3, DMat4, DQuat, DVec3};
use serde::{Deserialize, Serialize};

use crate::mesh::{Aabb, TriMesh};

/// Parting plane orientation and position, relative to the model.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartingPlane {
    /// Tilt of the pull axis about the model's X axis, degrees.
    pub tilt_x_deg: f64,
    /// Tilt of the pull axis about the model's Y axis, degrees.
    pub tilt_y_deg: f64,
    /// Rotation of the mold box around the pull axis, degrees.
    pub spin_deg: f64,
    /// Distance of the plane from the model's bounding-box centre along the
    /// pull axis, millimetres.
    pub offset_mm: f64,
}

impl Default for PartingPlane {
    fn default() -> Self {
        PartingPlane {
            tilt_x_deg: 0.0,
            tilt_y_deg: 0.0,
            spin_deg: 0.0,
            offset_mm: 0.0,
        }
    }
}

impl PartingPlane {
    /// Rotation taking mold-frame directions to model directions.
    pub fn rotation(&self) -> DQuat {
        DQuat::from_rotation_x(self.tilt_x_deg.to_radians())
            * DQuat::from_rotation_y(self.tilt_y_deg.to_radians())
            * DQuat::from_rotation_z(self.spin_deg.to_radians())
    }

    /// Pull axis (plane normal) in model coordinates.
    pub fn normal(&self) -> DVec3 {
        self.rotation() * DVec3::Z
    }

    /// Plane whose normal is `dir` (model coordinates) at `offset_mm`.
    pub fn from_normal(dir: DVec3, spin_deg: f64, offset_mm: f64) -> PartingPlane {
        let d = dir.normalize();
        // rotation() * Z = (sin ty, -sin tx cos ty, cos tx cos ty)
        let ty = d.x.clamp(-1.0, 1.0).asin();
        let tx = (-d.y).atan2(d.z);
        PartingPlane {
            tilt_x_deg: tx.to_degrees(),
            tilt_y_deg: ty.to_degrees(),
            spin_deg,
            offset_mm,
        }
    }

    /// Range of valid offsets for a model: the plane must cut the model.
    pub fn offset_range(&self, model: &TriMesh) -> (f64, f64) {
        let c = model.bounds().center();
        let n = self.normal();
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for p in &model.positions {
            let d = (*p - c).dot(n);
            lo = lo.min(d);
            hi = hi.max(d);
        }
        if lo > hi {
            (0.0, 0.0)
        } else {
            (lo, hi)
        }
    }
}

/// Transform between model space and the mold frame for a given plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoldFrame {
    /// Maps model coordinates to mold-frame coordinates.
    pub mold_from_model: DMat4,
    /// Maps mold-frame coordinates back to model coordinates.
    pub model_from_mold: DMat4,
}

impl MoldFrame {
    pub fn new(model: &TriMesh, plane: &PartingPlane) -> MoldFrame {
        let c = model.bounds().center();
        let rot_inv = plane.rotation().inverse();
        let rotate = DMat4::from_quat(rot_inv) * DMat4::from_translation(-c);
        // Centre the rotated model in X/Y and put the plane at z = 0.
        let mut b = Aabb::EMPTY;
        for p in &model.positions {
            b.extend(rotate.transform_point3(*p));
        }
        let shift = DVec3::new(-b.center().x, -b.center().y, -plane.offset_mm);
        let mold_from_model = DMat4::from_translation(shift) * rotate;
        MoldFrame {
            mold_from_model,
            model_from_mold: mold_from_model.inverse(),
        }
    }

    pub fn identity() -> MoldFrame {
        MoldFrame {
            mold_from_model: DMat4::IDENTITY,
            model_from_mold: DMat4::IDENTITY,
        }
    }

    pub fn to_mold(&self, model: &TriMesh) -> TriMesh {
        model.transformed(&self.mold_from_model)
    }

    /// Rotation part only (for directions).
    pub fn mold_rotation(&self) -> DMat3 {
        DMat3::from_mat4(self.mold_from_model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::primitives::cuboid;

    #[test]
    fn from_normal_roundtrip() {
        for d in [
            DVec3::Z,
            DVec3::X,
            DVec3::new(0.3, -0.4, 0.8),
            DVec3::new(-0.5, 0.5, -0.2),
            DVec3::NEG_Y,
        ] {
            let p = PartingPlane::from_normal(d, 17.0, 3.0);
            assert!((p.normal() - d.normalize()).length() < 1e-9, "{d:?} -> {:?}", p.normal());
        }
    }

    #[test]
    fn frame_puts_plane_at_zero() {
        let m = cuboid(DVec3::new(10.0, 20.0, 30.0), DVec3::new(20.0, 40.0, 70.0));
        let plane = PartingPlane {
            offset_mm: 5.0,
            ..Default::default()
        };
        let f = MoldFrame::new(&m, &plane);
        let b = f.to_mold(&m).bounds();
        assert!((b.min.z + 25.0).abs() < 1e-9 && (b.max.z - 15.0).abs() < 1e-9);
        assert!(b.center().x.abs() < 1e-9 && b.center().y.abs() < 1e-9);
        // Tilted 90 degrees about Y: the old X axis becomes the pull axis.
        let plane = PartingPlane {
            tilt_y_deg: 90.0,
            ..Default::default()
        };
        let b = MoldFrame::new(&m, &plane).to_mold(&m).bounds();
        assert!((b.size().z - 10.0).abs() < 1e-9, "{b:?}");
    }
}

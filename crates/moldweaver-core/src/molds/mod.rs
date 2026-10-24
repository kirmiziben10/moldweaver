//! Mold types. Each mold type is a module implementing [`MoldType`]:
//! parameters in, a list of named parts plus a silicone volume report out.
//!
//! Phase 1 ships [`two_part::TwoPartBox`] and [`open_tray::OpenTray`]. Later
//! mold types (multi-part jacket molds, slip-casting molds with cores) are
//! added by implementing the trait and registering them in [`registry`]; the
//! UI, project files, CLI and exporter need no changes.

pub mod features;
pub mod open_tray;
pub mod params;
pub mod two_part;

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Result};
use glam::{DQuat, DVec3};
use manifold_rust::manifold::Manifold;
use serde::{Deserialize, Serialize};

use crate::analysis::PlaneUsage;
use crate::mesh::TriMesh;
pub use params::{ParamKind, ParamSpec, ParamValue, ParamValues};

/// What a generated part is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartRole {
    /// Printed on the 3D printer and exported to the 3MF.
    Printed,
    /// The silicone poured in pour number `n` (preview only, not exported).
    Silicone(u8),
}

/// One generated part, in the mold frame.
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub role: PartRole,
    pub mesh: TriMesh,
    pub volume_mm3: f64,
    /// Direction and distance the part moves in the exploded view.
    pub explode: DVec3,
    /// Rotation from the mold frame into the recommended print orientation
    /// (the exporter then drops the part onto the plate).
    pub print_rotation: DQuat,
    /// Short note shown in the parts list (e.g. how to use the part).
    pub note: String,
    /// Mesh to print instead of the rotated assembly mesh (e.g. loose
    /// dowels grouped together rather than spread over the box corners).
    pub print_mesh: Option<TriMesh>,
}

impl Part {
    pub fn from_solid(name: impl Into<String>, role: PartRole, solid: &Manifold) -> Part {
        // Collapse sliver triangles left over from booleans; keep the
        // original if that fails (e.g. results of the robust engine).
        let cleaned = solid.simplify(1e-4);
        let solid = if crate::solid::ok(&cleaned) && (cleaned.volume() - solid.volume()).abs() <= 1e-6 * solid.volume().abs().max(1.0) {
            &cleaned
        } else {
            solid
        };
        Part {
            name: name.into(),
            role,
            mesh: TriMesh::from_manifold(solid),
            volume_mm3: solid.volume(),
            explode: DVec3::ZERO,
            print_rotation: DQuat::IDENTITY,
            note: String::new(),
            print_mesh: None,
        }
    }

    pub fn with_print_mesh(mut self, mesh: TriMesh) -> Part {
        self.print_mesh = Some(mesh);
        self
    }

    pub fn exploded(mut self, by: DVec3) -> Part {
        self.explode = by;
        self
    }

    pub fn print_rotated(mut self, q: DQuat) -> Part {
        self.print_rotation = q;
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Part {
        self.note = note.into();
        self
    }

    pub fn is_printed(&self) -> bool {
        self.role == PartRole::Printed
    }
}

/// Silicone needed for one pour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PourVolume {
    pub name: String,
    pub silicone_ml: f64,
    pub silicone_g: f64,
    pub cost: Option<f64>,
}

/// Volume figures for a generated mold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeReport {
    /// Inside of the box (where silicone can go), mL.
    pub cavity_ml: f64,
    /// The model (the part of it inside the cavity), mL.
    pub model_ml: f64,
    /// Pour spout and vent channels, mL.
    pub channels_ml: f64,
    /// Silicone for all pours = cavity - model - channels, mL.
    pub silicone_ml: f64,
    pub silicone_g: f64,
    pub density_g_per_ml: f64,
    pub cost: Option<f64>,
    /// Casting material for one cast (model + channels), mL.
    pub cast_ml: f64,
    pub pours: Vec<PourVolume>,
}

impl VolumeReport {
    /// Build a report from per-pour silicone volumes (mm³).
    pub fn new(
        cavity_mm3: f64,
        model_mm3: f64,
        channels_mm3: f64,
        pours_mm3: &[(&str, f64)],
        density: f64,
        price_per_kg: f64,
    ) -> VolumeReport {
        let cost = |g: f64| (price_per_kg > 0.0).then(|| g / 1000.0 * price_per_kg);
        let pours: Vec<PourVolume> = pours_mm3
            .iter()
            .map(|(name, v)| {
                let ml = v / 1000.0;
                PourVolume {
                    name: name.to_string(),
                    silicone_ml: ml,
                    silicone_g: ml * density,
                    cost: cost(ml * density),
                }
            })
            .collect();
        let silicone_ml: f64 = pours.iter().map(|p| p.silicone_ml).sum();
        VolumeReport {
            cavity_ml: cavity_mm3 / 1000.0,
            model_ml: model_mm3 / 1000.0,
            channels_ml: channels_mm3 / 1000.0,
            silicone_ml,
            silicone_g: silicone_ml * density,
            density_g_per_ml: density,
            cost: cost(silicone_ml * density),
            cast_ml: (model_mm3 + channels_mm3) / 1000.0,
            pours,
        }
    }
}

/// Everything a generator needs.
pub struct MoldInput<'a> {
    /// Model in the mold frame (parting plane at z = 0, pull up = +Z).
    pub model: &'a Manifold,
    pub model_mesh: &'a TriMesh,
    /// Normalised parameter values for this mold type.
    pub params: &'a ParamValues,
    /// Set when the result is no longer wanted.
    pub cancel: &'a AtomicBool,
}

impl MoldInput<'_> {
    /// Return early with an error if the job was cancelled.
    pub fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            bail!(Cancelled);
        }
        Ok(())
    }
}

/// Error marker for cancelled generation jobs.
#[derive(Debug, thiserror::Error)]
#[error("generation cancelled")]
pub struct Cancelled;

/// Generator output.
#[derive(Clone, Debug)]
pub struct MoldOutput {
    pub parts: Vec<Part>,
    pub report: VolumeReport,
    pub warnings: Vec<String>,
    /// Pour spout entry point and vent points on the model (mold frame), for
    /// display.
    pub markers: Vec<(String, DVec3)>,
}

/// A kind of mold.
pub trait MoldType: Send + Sync {
    /// Stable identifier stored in project files.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// One-paragraph explanation for the UI.
    fn description(&self) -> &'static str;
    /// How the parting plane is used (affects undercut colouring and plane
    /// suggestions).
    fn plane_usage(&self) -> PlaneUsage;
    fn param_specs(&self) -> Vec<ParamSpec>;
    fn generate(&self, input: &MoldInput) -> Result<MoldOutput>;
}

/// All available mold types, in UI order.
pub fn registry() -> Vec<Box<dyn MoldType>> {
    vec![Box::new(two_part::TwoPartBox), Box::new(open_tray::OpenTray)]
}

pub fn find(id: &str) -> Option<Box<dyn MoldType>> {
    registry().into_iter().find(|m| m.id() == id)
}

pub const DEFAULT_MOLD_TYPE: &str = two_part::ID;

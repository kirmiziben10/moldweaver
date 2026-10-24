//! Model -> mold parts -> plate, shared by the GUI and the batch CLI.

use std::sync::atomic::AtomicBool;

use anyhow::{anyhow, Context, Result};

use crate::analysis::{classify_faces, undercut_stats, FaceClass, UndercutStats};
use crate::frame::{MoldFrame, PartingPlane};
use crate::layout::{arrange, export_3mf, Layout};
use crate::mesh::TriMesh;
use crate::molds::{self, MoldInput, MoldOutput, MoldType, ParamValues};
use crate::printer::PrinterProfile;

/// Result of generating a mold.
#[derive(Clone, Debug)]
pub struct Generated {
    pub frame: MoldFrame,
    /// The model in the mold frame.
    pub model_mold: TriMesh,
    pub output: MoldOutput,
    pub undercuts: UndercutStats,
}

/// Undercut classes of the model's faces for a plane (for colouring).
pub fn face_classes(model: &TriMesh, plane: &PartingPlane, mold: &dyn MoldType) -> (Vec<FaceClass>, UndercutStats) {
    let frame = MoldFrame::new(model, plane);
    let m = frame.to_mold(model);
    let classes = classify_faces(&m, mold.plane_usage());
    let stats = undercut_stats(&m, &classes);
    (classes, stats)
}

pub fn generate(
    model: &TriMesh,
    plane: &PartingPlane,
    mold: &dyn MoldType,
    params: &ParamValues,
    cancel: &AtomicBool,
) -> Result<Generated> {
    let frame = MoldFrame::new(model, plane);
    let model_mold = frame.to_mold(model);
    let solid = model_mold.to_manifold().map_err(|e| anyhow!("{e}")).context("the model is not a watertight solid")?;
    let params = params.normalized(&mold.param_specs());
    let input = MoldInput { model: &solid, model_mesh: &model_mold, params: &params, cancel };
    let mut output = mold.generate(&input)?;
    let classes = classify_faces(&model_mold, mold.plane_usage());
    let undercuts = undercut_stats(&model_mold, &classes);
    if undercuts.fraction() > 0.005 {
        output.warnings.insert(
            0,
            format!(
                "{:.1}% of the model surface ({:.0} mm²) is undercut for this parting plane (red in the 3D view). Silicone stretches over small undercuts; for large ones try Suggest plane.",
                undercuts.fraction() * 100.0,
                undercuts.undercut_area_mm2
            ),
        );
    }
    Ok(Generated { frame, model_mold, output, undercuts })
}

/// Arrange the printed parts of a generated mold for `printer`.
pub fn plate_layout(generated: &Generated, printer: &PrinterProfile) -> Layout {
    let printed: Vec<&molds::Part> = generated.output.parts.iter().filter(|p| p.is_printed()).collect();
    arrange(&printed, printer)
}

/// Generate and export in one go (used by the CLI).
pub fn build_3mf(generated: &Generated, printer: &PrinterProfile, title: &str) -> Result<(Vec<u8>, Layout)> {
    let layout = plate_layout(generated, printer);
    let bytes = export_3mf(&layout, title)?;
    Ok((bytes, layout))
}

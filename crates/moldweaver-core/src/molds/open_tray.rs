//! Open tray / one-part box: the model sits flat on a base plate inside a
//! walled box and is covered with silicone in a single pour. The model's
//! back (the part on the base plane) stays open and becomes the pour
//! opening when casting.

use anyhow::{bail, Result};
use glam::{DVec2, DVec3};
use manifold_rust::manifold::Manifold;

use super::features::{self, Rect};
use super::params::{ParamSpec, ParamValue};
use super::{MoldInput, MoldOutput, MoldType, Part, PartRole, VolumeReport};
use crate::analysis::PlaneUsage;
use crate::solid::{self, cuboid, rect_frustum, subtract_all, union_all};

pub const ID: &str = "open_tray";
const G_BOX: &str = "Box";

pub struct OpenTray;

impl MoldType for OpenTray {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Open tray (one-part)"
    }

    fn description(&self) -> &'static str {
        "A walled box on a base plate. The model's flat back sits on the base and everything above it is covered with silicone in one pour. Best for flat-backed models such as reliefs, badges and tiles."
    }

    fn plane_usage(&self) -> PlaneUsage {
        PlaneUsage::Base
    }

    fn param_specs(&self) -> Vec<ParamSpec> {
        let mut v = vec![
            ParamSpec::float(G_BOX, "margin", "Silicone margin", 10.0, (3.0, 60.0, 0.5), "mm",
                "Thickness of silicone between the model and the box walls."),
            ParamSpec::float(G_BOX, "wall_above", "Wall height above model", 10.0, (2.0, 60.0, 0.5), "mm",
                "How far the walls rise above the highest point of the model. Fill the box to the brim, so this is also the thickness of silicone over the top of the model."),
            ParamSpec::float(G_BOX, "wall", "Wall thickness", 3.0, (1.2, 10.0, 0.1), "mm",
                "Thickness of the printed box walls."),
            ParamSpec::float(G_BOX, "floor", "Base thickness", 3.0, (1.2, 10.0, 0.1), "mm",
                "Thickness of the base plate the model sits on."),
            ParamSpec::float(G_BOX, "draft", "Draft angle", 1.0, (0.0, 10.0, 0.5), "°",
                "Tapers the inside of the walls so the cured silicone block slides out easily."),
            ParamSpec::boolean(G_BOX, "detachable", "Detachable base", false,
                "Print the walls and the base as separate parts held together with fasteners. Makes demolding easier, but the seam must be sealed (hot glue or clay) so silicone cannot leak."),
            ParamSpec::float(G_BOX, "clearance", "Part clearance", 0.3, (0.0, 1.5, 0.05), "mm",
                "Gap between the base plate's locating plug and the walls.")
                .when("detachable", ParamValue::Bool(true)),
        ];
        v.extend(features::vent_only_specs());
        v.extend(
            features::clamp_specs()
                .into_iter()
                .map(|s| s.when("detachable", ParamValue::Bool(true))),
        );
        v.extend(features::silicone_specs());
        // Vents default off: the open back usually lets air out on its own.
        for s in &mut v {
            if s.key == "vents" {
                s.default = ParamValue::Bool(false);
                s.tooltip = "Adds pins on the base plate that reach up to the model's local low points (air traps when the mold is flipped for casting), so air can escape through the open back.";
            }
        }
        v
    }

    fn generate(&self, input: &MoldInput) -> Result<MoldOutput> {
        generate(input)
    }
}

fn generate(input: &MoldInput) -> Result<MoldOutput> {
    let p = input.params;
    let full = input.model.bounding_box();
    if full.max.z < 0.5 {
        bail!("The model must sit on top of the base plane. Move the plane down to the bottom of the model.");
    }
    let mut warnings = Vec::new();
    let model = solid::trim_z(input.model, 0.0, true);
    if full.min.z < -0.05 {
        warnings.push(format!(
            "{:.1} mm of the model lies below the base plane and is cut off; the cast gets a flat back there.",
            -full.min.z
        ));
    }
    let back = features::Section::of(&model, 0.01);
    if back.area() < 1.0 {
        warnings.push("The model barely touches the base plane, so the cast would have no open back to pour into. Move the plane up into the model to give it a flat back.".into());
    }
    let mb = model.bounding_box();
    let margin = p.f("margin");
    let wall = p.f("wall");
    let floor = p.f("floor");
    let tan = p.f("draft").to_radians().tan();
    let top = mb.max.z + p.f("wall_above");
    let rect0 = Rect::new(DVec2::new(mb.min.x, mb.min.y) - margin, DVec2::new(mb.max.x, mb.max.y) + margin);
    let at = |z: f64| rect0.inset(-tan * z.max(0.0));
    let cavity = rect_frustum(0.0, rect0.min, rect0.max, top, at(top).min, at(top).max);
    let cut = union_all(vec![
        rect_frustum(0.0, rect0.min, rect0.max, top + 1.0, at(top + 1.0).min, at(top + 1.0).max),
        cuboid(rect0.min.extend(-20.0), rect0.max.extend(0.01)),
    ]);
    let outer = rect0.inset(-(tan * top + wall));
    input.check_cancel()?;

    // Vent pins rise from the base to the model's low points.
    let model_mesh = crate::mesh::TriMesh::from_manifold(&model);
    let plan = features::plan_channels(&model_mesh, p, false, 0.0, false);
    warnings.extend(plan.warnings.iter().cloned());
    let mut markers = Vec::new();
    let pins = union_all(
        plan.vents
            .iter()
            .map(|v| {
                markers.push(("Vent".to_string(), *v));
                let r = p.f("vent_diameter") * 0.5;
                solid::cylinder_z(v.truncate(), -0.5, v.z + 1.0, r, r)
            })
            .collect(),
    );
    let pins_printed = if pins.is_empty() { Manifold::empty() } else { pins.difference(&model) };

    let mut parts = Vec::new();
    let ex = (0.4 * top).max(15.0);
    if p.b("detachable") {
        let lip = 2.0;
        let clear = p.f("clearance");
        let fw = p.f("flange_width");
        let ft = p.f("flange_thickness").min(top * 0.5);
        let tol = p.f("hole_tolerance");
        let style = p.s("clamp_style").to_string();
        let flange = outer.inset(-fw);
        let ring = subtract_all(
            union_all(vec![
                cuboid(outer.min.extend(-lip), outer.max.extend(top)),
                cuboid(flange.min.extend(-lip), flange.max.extend(-lip + ft)),
            ]),
            vec![cut.clone()],
        );
        let plug = rect0.inset(clear);
        let mut base = union_all(vec![
            cuboid(flange.min.extend(-lip - floor), flange.max.extend(-lip)),
            cuboid(plug.min.extend(-lip - 0.01), plug.max.extend(0.0)),
        ]);
        if !pins_printed.is_empty() {
            base = base.union(&pins_printed);
        }
        let fasteners = if style == "none" {
            Vec::new()
        } else {
            features::fastener_positions(outer, fw, p.i("clamps_per_side").max(1) as usize)
        };
        let cuts = features::fastener_cuts(&style, &fasteners, -lip - floor - 1.0, -lip + ft + 1.0, tol);
        let ring = subtract_all(ring, cuts.clone());
        let base = subtract_all(base, cuts);
        parts.push(
            Part::from_solid("Tray walls", PartRole::Printed, &ring)
                .exploded(DVec3::Z * ex)
                .with_note("Fits over the base plate's plug. Seal the seam before pouring."),
        );
        parts.push(
            Part::from_solid("Tray base", PartRole::Printed, &base)
                .exploded(DVec3::NEG_Z * ex * 0.5)
                .with_note("Glue the model's flat back onto the raised plug."),
        );
        if style == "clips" {
            for (i, (pos, side)) in fasteners.iter().enumerate() {
                let (c, q) = features::clip(outer, *pos, *side, fw, -lip - floor, -lip + ft);
                parts.push(
                    Part::from_solid(format!("Clamp {}", i + 1), PartRole::Printed, &c)
                        .print_rotated(q)
                        .with_note("Slide over the flange and the base plate."),
                );
            }
        }
    } else {
        let mut tray = subtract_all(cuboid(outer.min.extend(-floor), outer.max.extend(top)), vec![cut]);
        if !pins_printed.is_empty() {
            tray = tray.union(&pins_printed);
        }
        parts.push(
            Part::from_solid("Tray", PartRole::Printed, &tray)
                .with_note("Glue the model's flat back to the floor, then fill with silicone to the brim."),
        );
    }
    input.check_cancel()?;

    let silicone = subtract_all(cavity.clone(), vec![model.clone(), pins.clone()]);
    let channels = if pins.is_empty() {
        0.0
    } else {
        pins.intersection(&cavity).difference(&model).volume()
    };
    for part in &parts {
        if part.mesh.is_empty() {
            bail!("Could not build the {}. Try a different plane or margin.", part.name);
        }
    }
    parts.push(Part::from_solid("Silicone (pour 1)", PartRole::Silicone(1), &silicone).exploded(DVec3::Z * ex * 0.5));
    let (density, price) = features::density_and_price(p);
    let report = VolumeReport::new(
        cavity.volume(),
        model.volume(),
        channels,
        &[("Pour 1: whole mold", silicone.volume())],
        density,
        price,
    );
    Ok(MoldOutput {
        parts,
        report,
        warnings,
        markers,
    })
}

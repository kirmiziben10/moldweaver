//! Two-part split box with a printed parting board.
//!
//! Layout in the mold frame (parting plane at z = 0):
//!
//! ```text
//!   sprue bars ===============        z = top + bar
//!   +-------------------------+       z = top        (open: pour here)
//!   |  box half A   (ring)    |
//!   |      silicone A         |
//!   +==flange==keys====flange=+       z = 0          (parting plane)
//!   |  box half B   (ring)    |
//!   |  parting board (pour 1) |
//!   |  silicone B    (pour 2) |
//!   +-------------------------+       z = bottom
//!   [========base plate=======]
//! ```
//!
//! Pour 1: base plate + ring B + parting board + model + ring A, pour
//! silicone A from the top. Flip, remove base plate and board, pour
//! silicone B. The rings' inner walls taper outwards (draft) so the
//! board and the silicone halves slide out of the open ends.

use anyhow::{bail, Result};
use glam::{DQuat, DVec2, DVec3};
use manifold_rust::manifold::Manifold;
use manifold_rust::types::{BooleanEngine, Error as ManifoldError, WindingRule};

use super::features::{self, Rect, Section};
use super::params::ParamSpec;
use super::{MoldInput, MoldOutput, MoldType, Part, PartRole, VolumeReport};
use crate::analysis::PlaneUsage;
use crate::mesh::TriMesh;
use crate::solid::{self, cuboid, rect_frustum, subtract_all, union_all};

pub const ID: &str = "two_part_box";
const G_BOX: &str = "Box";

pub struct TwoPartBox;

impl MoldType for TwoPartBox {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Two-part split box"
    }

    fn description(&self) -> &'static str {
        "A printed box split at the parting plane, plus a printed parting board that fills the lower half and holds the model at the parting line. Pour the first silicone half, flip, remove the board and pour the second half. No clay needed."
    }

    fn plane_usage(&self) -> PlaneUsage {
        PlaneUsage::Split
    }

    fn param_specs(&self) -> Vec<ParamSpec> {
        let mut v = vec![
            ParamSpec::float(G_BOX, "margin", "Silicone margin", 10.0, (3.0, 60.0, 0.5), "mm",
                "Thickness of silicone between the model and the box walls (and above/below the model). Thicker silicone is sturdier but uses more material; 8-15 mm suits most models."),
            ParamSpec::float(G_BOX, "wall", "Wall thickness", 3.0, (1.2, 10.0, 0.1), "mm",
                "Thickness of the printed box walls. 2.5-4 mm is stiff enough for most boxes."),
            ParamSpec::float(G_BOX, "floor", "Floor thickness", 3.0, (1.2, 10.0, 0.1), "mm",
                "Thickness of the base plate that closes the bottom of the box."),
            ParamSpec::float(G_BOX, "clearance", "Part clearance", 0.3, (0.0, 1.5, 0.05), "mm",
                "Gap between printed parts that slide into each other (the parting board and the base plate in the box). Too small and parts will not fit; too big and silicone can seep through."),
            ParamSpec::float(G_BOX, "draft", "Draft angle", 1.0, (0.0, 10.0, 0.5), "°",
                "Tapers the inside of the box walls so the silicone and the board slide out easily. 1-2 degrees is plenty."),
            ParamSpec::boolean(G_BOX, "board_safe", "Undercut-safe board", true,
                "Shapes the parting board so it can always be pulled off the model, even where the lower half of the model has overhangs. Turn off to make the board follow the model exactly."),
        ];
        v.extend(features::spout_specs());
        v.extend(features::key_specs());
        v.extend(features::clamp_specs());
        v.extend(features::silicone_specs());
        v
    }

    fn generate(&self, input: &MoldInput) -> Result<MoldOutput> {
        generate(input)
    }
}

fn generate(input: &MoldInput) -> Result<MoldOutput> {
    let p = input.params;
    let model = input.model;
    let mb = model.bounding_box();
    let (zmin, zmax) = (mb.min.z, mb.max.z);
    if !(zmin < -0.2 && zmax > 0.2) {
        bail!("The parting plane must cut through the model. Move it between the bottom and the top of the model.");
    }
    let mut warnings = Vec::new();
    let margin = p.f("margin");
    let wall = p.f("wall");
    let floor = p.f("floor");
    let clear = p.f("clearance");
    let tan = p.f("draft").to_radians().tan();
    let tol = p.f("hole_tolerance");

    // Cavity: model bounds + margin, tapering outwards away from the plane.
    let rect0 = Rect::new(DVec2::new(mb.min.x, mb.min.y) - margin, DVec2::new(mb.max.x, mb.max.y) + margin);
    let (cz0, cz1) = (zmin - margin, zmax + margin);
    let at = |z: f64| rect0.inset(-tan * z.abs());
    let cav_a = rect_frustum(0.0, rect0.min, rect0.max, cz1, at(cz1).min, at(cz1).max);
    let cav_b = rect_frustum(cz0, at(cz0).min, at(cz0).max, 0.0, rect0.min, rect0.max);
    // Slightly longer versions for cutting the rings cleanly.
    let e = 1.0;
    let ext = |z: f64, sign: f64| rect0.inset(-tan * z.abs() * sign);
    let cut_a = rect_frustum(-e, ext(e, -1.0).min, ext(e, -1.0).max, cz1 + e, at(cz1 + e).min, at(cz1 + e).max);
    let cut_b = rect_frustum(cz0 - e, at(cz0 - e).min, at(cz0 - e).max, e, ext(e, -1.0).min, ext(e, -1.0).max);

    let grow = tan * cz1.max(-cz0);
    let outer = rect0.inset(-(grow + wall));
    let fw = p.f("flange_width");
    let ft = p.f("flange_thickness").min(0.6 * cz1.min(-cz0));
    if ft < p.f("flange_thickness") - 1e-9 {
        warnings.push(format!("Flanges thinned to {ft:.1} mm to fit the box height."));
    }
    let flange = outer.inset(-fw);
    input.check_cancel()?;

    // ---- Box rings with flanges -------------------------------------------
    let style = p.s("clamp_style").to_string();
    let per_side = p.i("clamps_per_side").max(1) as usize;
    let fasteners = if style == "none" { Vec::new() } else { features::fastener_positions(outer, fw, per_side) };
    let cuts = features::fastener_cuts(&style, &fasteners, -ft - 1.0, ft + 1.0, tol);

    let dowel_d = (fw - 4.0).clamp(2.5, 5.0);
    let sock_depth = (ft - 1.0).max(1.2);
    let sock_r = solid::hole_radius(dowel_d + tol);
    let corners = features::corner_positions(outer, fw);
    let sockets_a: Vec<Manifold> = corners.iter().map(|c| solid::cylinder_z(*c, -0.01, sock_depth, sock_r, sock_r)).collect();
    let sockets_b: Vec<Manifold> = corners.iter().map(|c| solid::cylinder_z(*c, -sock_depth, 0.01, sock_r, sock_r)).collect();

    let ring_a_body = union_all(vec![
        cuboid(outer.min.extend(0.0), outer.max.extend(cz1)),
        cuboid(flange.min.extend(0.0), flange.max.extend(ft)),
    ]);
    let mut tools = vec![cut_a];
    tools.extend(cuts.iter().cloned());
    tools.extend(sockets_a);
    let ring_a = subtract_all(ring_a_body, tools);

    let ring_b_body = union_all(vec![
        cuboid(outer.min.extend(cz0), outer.max.extend(0.0)),
        cuboid(flange.min.extend(-ft), flange.max.extend(0.0)),
    ]);
    let mut tools = vec![cut_b];
    tools.extend(cuts.iter().cloned());
    tools.extend(sockets_b);
    let ring_b = subtract_all(ring_b_body, tools);
    input.check_cancel()?;

    // ---- Parting board --------------------------------------------------
    let board_blank = {
        let bot = at(cz0).inset(clear);
        let top = rect0.inset(clear);
        rect_frustum(cz0, bot.min, bot.max, 0.0, top.min, top.max)
    };
    let below = solid::trim_z(model, 0.0, false);
    let cutter = if p.b("board_safe") {
        match undercut_safe_cutter(model, &below) {
            Ok(c) => c,
            Err(msg) => {
                warnings.push(msg);
                model.clone()
            }
        }
    } else {
        model.clone()
    };
    input.check_cancel()?;
    let key_pos = if p.b("keys") {
        features::place_keys(rect0, &Section::of(model, 0.0), p, &mut warnings)
    } else {
        Vec::new()
    };
    let keys: Vec<Manifold> = key_pos.iter().map(|k| features::key_solid(*k, 0.0, p, true)).collect();
    let key_union = union_all(keys.clone());
    let mut board = board_blank.difference_with_engine(&cutter, BooleanEngine::Auto);
    if !key_union.is_empty() {
        board = board.union(&key_union);
    }
    input.check_cancel()?;

    // ---- Base plate ------------------------------------------------------
    let lip_w = 2.5;
    let lip_h = 2.0;
    let plate = outer.inset(-(clear + lip_w));
    let base = union_all(vec![
        cuboid(plate.min.extend(cz0 - floor), plate.max.extend(cz0)),
        cuboid(plate.min.extend(cz0 - 0.01), plate.max.extend(cz0 + lip_h))
            .difference(&cuboid(outer.inset(-clear).min.extend(cz0 - 1.0), outer.inset(-clear).max.extend(cz0 + lip_h + 1.0))),
    ]);

    // ---- Spout and vents -------------------------------------------------
    let plan = features::plan_channels(input.model_mesh, p, true, 0.0, p.b("spout"));
    warnings.extend(plan.warnings.iter().cloned());
    let bar_t = 3.0;
    let bars_along_x = outer.size().x <= outer.size().y;
    let bar = |xy: DVec2, w: f64| {
        let h = w * 0.5;
        if bars_along_x {
            cuboid(DVec3::new(outer.min.x, xy.y - h, cz1), DVec3::new(outer.max.x, xy.y + h, cz1 + bar_t))
        } else {
            cuboid(DVec3::new(xy.x - h, outer.min.y, cz1), DVec3::new(xy.x + h, outer.max.y, cz1 + bar_t))
        }
    };
    let mut rods: Vec<Manifold> = Vec::new();
    let mut bars: Vec<Manifold> = Vec::new();
    let mut markers = Vec::new();
    if let Some(sp) = plan.spout {
        let rs = p.f("spout_diameter") * 0.5;
        let rf = (p.f("funnel_diameter") * 0.5).max(rs);
        let xy = sp.truncate();
        let room = cz1 - sp.z;
        let funnel_h = (rf - rs).min(room * 0.6).max(0.0);
        let zf = cz1 - funnel_h;
        rods.push(solid::cylinder_z(xy, sp.z - 1.5, zf + 0.01, rs, rs));
        if funnel_h > 0.1 {
            rods.push(solid::cylinder_z(xy, zf, cz1, rs, rs + funnel_h));
        }
        let rtop = rs + funnel_h;
        rods.push(solid::cylinder_z(xy, cz1 - 0.01, cz1 + bar_t, rtop, rtop));
        bars.push(bar(xy, (rtop * 2.0).max(5.0).min(rtop * 2.0 + 2.0)));
        markers.push(("Pour spout".to_string(), sp));
    }
    for v in &plan.vents {
        let rv = p.f("vent_diameter") * 0.5;
        rods.push(solid::cylinder_z(v.truncate(), v.z - 1.0, cz1 + bar_t, rv, rv));
        bars.push(bar(v.truncate(), (rv * 2.0 + 2.0).max(4.0)));
        markers.push(("Vent".to_string(), *v));
    }
    let rods_u = union_all(rods);
    let sprue = if rods_u.is_empty() {
        Manifold::empty()
    } else {
        union_all(vec![rods_u.clone()]).union(&union_all(bars)).difference(model)
    };
    input.check_cancel()?;

    // ---- Silicone ----------------------------------------------------------
    let cavity = cav_a.union(&cav_b);
    let channels = if rods_u.is_empty() {
        Manifold::empty()
    } else {
        rods_u.intersection(&cavity).difference(model)
    };
    let total = subtract_all(cavity.clone(), vec![model.clone(), rods_u.clone()]);
    let mut sil_a = solid::trim_z(&total, 0.0, true);
    if !key_union.is_empty() {
        sil_a = sil_a.difference(&key_union);
    }
    let mut sil_b = solid::trim_z(&total, 0.0, false);
    if !key_union.is_empty() {
        sil_b = sil_b.union(&key_union.intersection(&cav_a));
    }
    input.check_cancel()?;

    for (name, s) in [("box half A", &ring_a), ("box half B", &ring_b), ("parting board", &board), ("base plate", &base)] {
        if !solid::ok(s) {
            bail!("Could not build the {name} ({:?}). Try a different plane or margin.", s.status());
        }
    }

    // ---- Parts --------------------------------------------------------------
    let ex = (0.35 * (cz1 - cz0)).max(15.0);
    let flip = DQuat::from_rotation_x(std::f64::consts::PI);
    let mut parts = vec![
        Part::from_solid("Box half A (top)", PartRole::Printed, &ring_a)
            .exploded(DVec3::Z * ex)
            .with_note("Top ring. Sits on half B; pour the first silicone into its open top."),
        Part::from_solid("Box half B (bottom)", PartRole::Printed, &ring_b)
            .exploded(DVec3::NEG_Z * ex * 1.2)
            .print_rotated(flip)
            .with_note("Bottom ring. Holds the parting board during the first pour."),
        Part::from_solid("Parting board", PartRole::Printed, &board)
            .exploded(DVec3::NEG_Z * ex * 0.6)
            .with_note("Fills half B up to the parting line. Press the model into it, pour half A, then remove it."),
        Part::from_solid("Base plate", PartRole::Printed, &base)
            .exploded(DVec3::NEG_Z * ex * 2.2)
            .with_note("Closes the bottom of half B. Put it back under half B when casting."),
    ];
    if solid::ok(&sprue) {
        parts.push(
            Part::from_solid("Spout & vent rods", PartRole::Printed, &sprue)
                .exploded(DVec3::Z * ex * 2.2)
                .print_rotated(flip)
                .with_note("Rest the bars on top of half A so the rods touch the model before pouring half A."),
        );
    }
    let dowel_len = 2.0 * sock_depth - 0.6;
    let dowels = union_all(
        corners
            .iter()
            .map(|c| solid::cylinder_z(*c, -dowel_len * 0.5, dowel_len * 0.5, dowel_d * 0.5, dowel_d * 0.5))
            .collect(),
    );
    let pitch = dowel_d + 4.0;
    let loose = union_all(
        (0..corners.len())
            .map(|i| {
                let xy = DVec2::new((i % 2) as f64 * pitch, (i / 2) as f64 * pitch);
                solid::cylinder_z(xy, 0.0, dowel_len, dowel_d * 0.5, dowel_d * 0.5)
            })
            .collect(),
    );
    parts.push(
        Part::from_solid("Alignment dowels", PartRole::Printed, &dowels)
            .with_print_mesh(TriMesh::from_manifold(&loose))
            .with_note("Push into the corner sockets so the two box halves line up."),
    );
    if style == "clips" {
        let mut clips = Vec::new();
        for (pos, side) in &fasteners {
            clips.push(features::clip(outer, *pos, *side, fw, -ft, ft));
        }
        for (i, (c, q)) in clips.into_iter().enumerate() {
            let dir = (side_normal(fasteners[i].1) * ex * 0.5).extend(0.0);
            parts.push(
                Part::from_solid(format!("Clamp {}", i + 1), PartRole::Printed, &c)
                    .exploded(dir)
                    .print_rotated(q)
                    .with_note("Slide over both flanges to hold the halves together."),
            );
        }
    }
    parts.push(
        Part::from_solid("Silicone A (pour 1)", PartRole::Silicone(1), &sil_a)
            .exploded(DVec3::Z * ex * 0.5),
    );
    parts.push(
        Part::from_solid("Silicone B (pour 2)", PartRole::Silicone(2), &sil_b)
            .exploded(DVec3::NEG_Z * ex * 0.5),
    );

    let (density, price) = features::density_and_price(p);
    let report = VolumeReport::new(
        cavity.volume(),
        model.volume(),
        channels.volume(),
        &[("Pour 1: half A (top)", sil_a.volume()), ("Pour 2: half B (bottom)", sil_b.volume())],
        density,
        price,
    );
    for k in &key_pos {
        markers.push(("Key".to_string(), k.extend(0.0)));
    }
    Ok(MoldOutput {
        parts,
        report,
        warnings,
        markers,
    })
}

fn side_normal(s: features::Side) -> DVec2 {
    match s {
        features::Side::MinX => DVec2::NEG_X,
        features::Side::MaxX => DVec2::X,
        features::Side::MinY => DVec2::NEG_Y,
        features::Side::MaxY => DVec2::Y,
    }
}

/// The region the parting board must avoid so it can be pulled straight
/// down off the model: the model plus everything between the model's
/// upward-facing lower surfaces and the parting plane.
fn undercut_safe_cutter(model: &Manifold, below: &Manifold) -> std::result::Result<Manifold, String> {
    let mesh = TriMesh::from_manifold(below);
    let top = 0.01;
    let mut soup = TriMesh::default();
    for i in 0..mesh.triangles.len() {
        let n = mesh.tri_normal(i);
        let tri = mesh.tri(i);
        if n.z <= 0.02 || tri.iter().all(|p| p.z > -1e-4) {
            continue;
        }
        let base = soup.positions.len() as u32;
        for p in tri {
            soup.positions.push(p);
        }
        for p in tri {
            soup.positions.push(DVec3::new(p.x, p.y, top));
        }
        let (a, b, c, a2, b2, c2) = (base, base + 1, base + 2, base + 3, base + 4, base + 5);
        soup.triangles.extend_from_slice(&[
            [a, c, b],
            [a2, b2, c2],
            [a, b, b2],
            [a, b2, a2],
            [b, c, c2],
            [b, c2, b2],
            [c, a, a2],
            [c, a2, c2],
        ]);
    }
    if soup.triangles.is_empty() {
        return Ok(model.clone());
    }
    let fail = || "Could not shape an undercut-safe parting board; it follows the model exactly instead, so check that it can be removed.".to_string();
    let prisms = Manifold::from_mesh_gl64_robust(&soup.to_mesh_gl64());
    if prisms.status() != ManifoldError::NoError || prisms.is_empty() {
        return Err(fail());
    }
    let prisms = prisms.rebuild_solid(WindingRule::Positive);
    if prisms.status() != ManifoldError::NoError {
        return Err(fail());
    }
    let cutter = model.union_with_engine(&prisms, BooleanEngine::Auto);
    if cutter.status() != ManifoldError::NoError || cutter.is_empty() {
        return Err(fail());
    }
    Ok(cutter)
}

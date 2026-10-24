//! Geometry tests against analytic shapes.

use std::f64::consts::PI;
use std::sync::atomic::AtomicBool;

use moldweaver_core::frame::PartingPlane;
use moldweaver_core::glam::{DVec2, DVec3};
use moldweaver_core::manifold_rust::manifold::Manifold;
use moldweaver_core::mesh::primitives::{cuboid, uv_sphere};
use moldweaver_core::mesh::TriMesh;
use moldweaver_core::molds::{self, features, ParamValue, ParamValues, PartRole};
use moldweaver_core::pipeline::{self, Generated};
use moldweaver_core::solid;

fn params(pairs: &[(&str, ParamValue)]) -> ParamValues {
    let mut v = ParamValues::default();
    for (k, val) in pairs {
        v.set(k, val.clone());
    }
    v
}

fn plain() -> Vec<(&'static str, ParamValue)> {
    vec![
        ("draft", ParamValue::Float(0.0)),
        ("spout", ParamValue::Bool(false)),
        ("vents", ParamValue::Bool(false)),
        ("keys", ParamValue::Bool(false)),
    ]
}

fn gen(mesh: &TriMesh, plane: PartingPlane, mold: &str, p: &ParamValues) -> Generated {
    let m = molds::find(mold).unwrap();
    pipeline::generate(mesh, &plane, m.as_ref(), p, &AtomicBool::new(false)).unwrap()
}

fn solid_of(g: &Generated, name: &str) -> Manifold {
    let part = g.output.parts.iter().find(|p| p.name.starts_with(name)).unwrap_or_else(|| panic!("no part {name}"));
    part.mesh.to_manifold().unwrap()
}

fn assert_all_watertight(g: &Generated) {
    for p in &g.output.parts {
        let t = p.mesh.topology();
        assert!(t.is_watertight(), "{} is not watertight: {t:?}", p.name);
        assert_eq!(t.degenerate_triangles, 0, "{} has degenerate triangles", p.name);
        let s = p.mesh.to_manifold().unwrap_or_else(|e| panic!("{} rejected by manifold: {e}", p.name));
        assert!(s.volume() > 0.0, "{} has no volume", p.name);
        assert!((s.volume() - p.volume_mm3).abs() <= 1e-6 * p.volume_mm3.max(1.0) + 1e-6, "{}", p.name);
    }
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs().max(1e-9)
}

#[test]
fn cube_two_part_volumes_are_analytic() {
    let s = 20.0;
    let m = 10.0;
    let cube = cuboid(DVec3::ZERO, DVec3::splat(s));
    let g = gen(&cube, PartingPlane::default(), "two_part_box", &params(&plain()));
    assert_all_watertight(&g);
    let r = &g.output.report;
    let cavity = (s + 2.0 * m).powi(3);
    assert!(close(r.cavity_ml * 1000.0, cavity, 1e-9), "{r:?}");
    assert!(close(r.model_ml * 1000.0, s.powi(3), 1e-9));
    assert!(close(r.silicone_ml * 1000.0, cavity - s.powi(3), 1e-9));
    // Split at the middle: both pours are equal halves.
    assert_eq!(r.pours.len(), 2);
    for p in &r.pours {
        assert!(close(p.silicone_ml * 1000.0, (cavity - s.powi(3)) / 2.0, 1e-9), "{p:?}");
        assert!(close(p.silicone_g, p.silicone_ml * 1.1, 1e-12));
    }
    assert!(r.cost.is_none());
}

#[test]
fn sphere_two_part_volumes_match_formula() {
    let r = 15.0;
    let m = 8.0;
    let sphere = uv_sphere(DVec3::new(5.0, -3.0, 2.0), r, 96, 48);
    let mut p = plain();
    p.push(("margin", ParamValue::Float(m)));
    p.push(("price_per_kg", ParamValue::Float(40.0)));
    let g = gen(&sphere, PartingPlane::default(), "two_part_box", &params(&p));
    assert_all_watertight(&g);
    let rep = &g.output.report;
    let cavity = (2.0 * (r + m)).powi(3);
    let ball = 4.0 / 3.0 * PI * r.powi(3);
    // The tessellated sphere is slightly smaller than the true one.
    assert!(close(rep.cavity_ml * 1000.0, cavity, 1e-6), "{rep:?}");
    assert!(close(rep.model_ml * 1000.0, ball, 0.01), "{} vs {ball}", rep.model_ml * 1000.0);
    assert!(close(rep.silicone_ml * 1000.0, cavity - ball, 0.002));
    let half = (cavity - ball) / 2.0;
    for pour in &rep.pours {
        assert!(close(pour.silicone_ml * 1000.0, half, 0.002), "{pour:?}");
    }
    let cost = rep.cost.unwrap();
    assert!(close(cost, rep.silicone_ml * 1.1 / 1000.0 * 40.0, 1e-9));
}

#[test]
fn draft_widens_the_cavity_away_from_the_plane() {
    let cube = cuboid(DVec3::ZERO, DVec3::splat(20.0));
    let mut p = plain();
    p[0] = ("draft", ParamValue::Float(3.0));
    let g = gen(&cube, PartingPlane::default(), "two_part_box", &params(&p));
    assert_all_watertight(&g);
    // Each half is a frustum: 40x40 at the plane growing by tan(3°)*20 per side.
    let t = 3f64.to_radians().tan();
    let a0 = 40.0f64 * 40.0;
    let a1 = (40.0 + 2.0 * t * 20.0f64).powi(2);
    let frustum = 20.0 / 3.0 * (a0 + a1 + (a0 * a1).sqrt());
    assert!(close(g.output.report.cavity_ml * 1000.0, 2.0 * frustum, 1e-6));
    // The board fits inside half B with clearance at every height.
    let board = solid_of(&g, "Parting board");
    let ring_b = solid_of(&g, "Box half B");
    assert!(board.intersection(&ring_b).volume() < 1e-6);
}

#[test]
fn keys_line_up_across_halves() {
    let sphere = uv_sphere(DVec3::ZERO, 12.0, 64, 32);
    let mut p = plain();
    p[3] = ("keys", ParamValue::Bool(true));
    let pv = params(&p);
    let g = gen(&sphere, PartingPlane::default(), "two_part_box", &pv);
    assert_all_watertight(&g);
    let keys: Vec<DVec3> = g.output.markers.iter().filter(|(n, _)| n == "Key").map(|(_, p)| *p).collect();
    assert_eq!(keys.len(), 4);
    let sil_a = solid_of(&g, "Silicone A");
    let sil_b = solid_of(&g, "Silicone B");
    let board = solid_of(&g, "Parting board");
    let norm = pv.normalized(&molds::find("two_part_box").unwrap().param_specs());
    for k in &keys {
        assert!(k.z.abs() < 1e-9, "keys sit on the parting plane");
        let key = features::key_solid(k.truncate(), 0.0, &norm, true);
        let kv = solid::trim_z(&key, 0.0, true).volume();
        // The board carries the bump, half A gets the matching dimple and
        // half B fills it.
        assert!(close(board.intersection(&key).volume(), key.volume(), 0.01));
        assert!(sil_a.intersection(&key).volume() < 1e-3 * kv);
        assert!(close(sil_b.intersection(&key).volume(), kv, 0.01));
    }
    // The halves do not overlap and together fill the cavity.
    assert!(sil_a.intersection(&sil_b).volume() < 1e-3);
    let total = g.output.report.silicone_ml * 1000.0;
    assert!(close(sil_a.volume() + sil_b.volume(), total, 1e-6));
    // Corner placement is symmetric for a centred model.
    for k in &keys {
        assert!(keys.iter().any(|q| (q.truncate() + k.truncate()).length() < 1e-6), "{k:?} has no opposite");
    }
}

fn is_inside(m: &Manifold, p: DVec3) -> bool {
    let probe = solid::cuboid(p - 0.05, p + 0.05);
    m.intersection(&probe).volume() > 0.5 * probe.volume()
}

#[test]
fn bolt_holes_are_symmetric_and_through_both_flanges() {
    let cube = cuboid(DVec3::new(-5.0, 3.0, 0.0), DVec3::new(35.0, 23.0, 20.0));
    let mut p = plain();
    p.push(("clamps_per_side", ParamValue::Int(2)));
    p.push(("clamp_style", ParamValue::Choice("m4".into())));
    let g = gen(&cube, PartingPlane::default(), "two_part_box", &params(&p));
    assert_all_watertight(&g);
    let a = solid_of(&g, "Box half A");
    let b = solid_of(&g, "Box half B");
    let ba = a.bounding_box();
    // Ring centred on the origin (the mold frame centres the model).
    assert!((ba.min.x + ba.max.x).abs() < 1e-6 && (ba.min.y + ba.max.y).abs() < 1e-6);
    let fw = 10.0;
    let outer = features::Rect::new(DVec2::new(ba.min.x + fw, ba.min.y + fw), DVec2::new(ba.max.x - fw, ba.max.y - fw));
    let holes = features::fastener_positions(outer, fw, 2);
    assert_eq!(holes.len(), 8);
    for (h, _) in &holes {
        assert!(!is_inside(&a, h.extend(2.0)), "hole at {h:?} missing in A");
        assert!(!is_inside(&b, h.extend(-2.0)), "hole at {h:?} missing in B");
        // Next to the hole there is flange material.
        let off = DVec2::new(3.2, 3.2);
        assert!(is_inside(&a, (*h + off).extend(2.0)) || is_inside(&a, (*h - off).extend(2.0)));
        let mirror = DVec2::new(-h.x, -h.y);
        assert!(holes.iter().any(|(q, _)| (*q - mirror).length() < 1e-9));
    }
    // Dowel sockets at the four corners of both flanges.
    for c in features::corner_positions(outer, fw) {
        assert!(!is_inside(&a, c.extend(1.0)) && !is_inside(&b, c.extend(-1.0)));
    }
}

#[test]
fn all_clamp_styles_and_features_stay_watertight() {
    let sphere = uv_sphere(DVec3::ZERO, 15.0, 48, 24);
    for style in ["m3", "m4", "zip", "clips", "none"] {
        let p = params(&[("clamp_style", ParamValue::Choice(style.into()))]);
        let g = gen(&sphere, PartingPlane::default(), "two_part_box", &p);
        assert_all_watertight(&g);
        let clamps = g.output.parts.iter().filter(|p| p.name.starts_with("Clamp")).count();
        assert_eq!(clamps, if style == "clips" { 8 } else { 0 }, "{style}");
        assert!(g.output.parts.iter().any(|p| p.name.starts_with("Spout")));
        // Printed clamps must not intersect the box halves they clip onto.
        if style == "clips" {
            let a = solid_of(&g, "Box half A");
            for c in g.output.parts.iter().filter(|p| p.name.starts_with("Clamp")) {
                assert!(c.mesh.to_manifold().unwrap().intersection(&a).volume() < 1e-6, "{}", c.name);
            }
        }
    }
}

#[test]
fn spout_reaches_the_model_and_the_top() {
    let sphere = uv_sphere(DVec3::ZERO, 15.0, 48, 24);
    let g = gen(&sphere, PartingPlane::default(), "two_part_box", &ParamValues::default());
    let spout = g.output.markers.iter().find(|(n, _)| n == "Pour spout").unwrap().1;
    assert!((spout - DVec3::new(0.0, 0.0, 15.0)).length() < 0.5, "{spout:?}");
    let sil_a = solid_of(&g, "Silicone A");
    // The channel runs from the model to the top of the silicone.
    assert!(!is_inside(&sil_a, DVec3::new(0.0, 0.0, 15.5)));
    assert!(!is_inside(&sil_a, DVec3::new(0.0, 0.0, 24.5)));
    assert!(is_inside(&sil_a, DVec3::new(10.0, 0.0, 20.0)));
    assert!(g.output.report.channels_ml > 0.0);
    // A sphere has no other high points: no vents.
    assert_eq!(g.output.markers.iter().filter(|(n, _)| n == "Vent").count(), 0);
}

#[test]
fn tray_cube_volume_is_analytic() {
    let s = 20.0;
    let cube = cuboid(DVec3::ZERO, DVec3::splat(s));
    let plane = PartingPlane { offset_mm: -s / 2.0, ..Default::default() };
    let p = params(&[("draft", ParamValue::Float(0.0)), ("wall_above", ParamValue::Float(7.0))]);
    let g = gen(&cube, plane, "open_tray", &p);
    assert_all_watertight(&g);
    let cavity = (s + 20.0).powi(2) * (s + 7.0);
    let r = &g.output.report;
    assert!(close(r.cavity_ml * 1000.0, cavity, 1e-9), "{r:?}");
    assert!(close(r.silicone_ml * 1000.0, cavity - s.powi(3), 1e-9));
    assert_eq!(r.pours.len(), 1);
    assert_eq!(g.undercuts.undercut_faces, 0);
    assert!(g.output.warnings.is_empty(), "{:?}", g.output.warnings);
}

#[test]
fn tray_detachable_base_with_bolts() {
    let cube = cuboid(DVec3::ZERO, DVec3::splat(20.0));
    let plane = PartingPlane { offset_mm: -10.0, ..Default::default() };
    let p = params(&[("detachable", ParamValue::Bool(true)), ("vents", ParamValue::Bool(true))]);
    let g = gen(&cube, plane, "open_tray", &p);
    assert_all_watertight(&g);
    let names: Vec<&str> = g.output.parts.iter().filter(|p| p.role == PartRole::Printed).map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["Tray walls", "Tray base"]);
    let walls = solid_of(&g, "Tray walls");
    let base = solid_of(&g, "Tray base");
    assert!(walls.intersection(&base).volume() < 1e-6);
}

#[test]
fn plane_that_misses_the_model_is_an_error() {
    let cube = cuboid(DVec3::ZERO, DVec3::splat(20.0));
    let m = molds::find("two_part_box").unwrap();
    let plane = PartingPlane { offset_mm: 30.0, ..Default::default() };
    let err = pipeline::generate(&cube, &plane, m.as_ref(), &ParamValues::default(), &AtomicBool::new(false)).unwrap_err();
    assert!(format!("{err:#}").contains("parting plane"));
}

#[test]
fn cancelled_job_stops() {
    let cube = cuboid(DVec3::ZERO, DVec3::splat(20.0));
    let m = molds::find("two_part_box").unwrap();
    let r = pipeline::generate(&cube, &PartingPlane::default(), m.as_ref(), &ParamValues::default(), &AtomicBool::new(true));
    assert!(r.unwrap_err().downcast_ref::<molds::Cancelled>().is_some());
}

#[test]
fn tilted_plane_matches_axis_aligned_result() {
    // Rotating the model and the plane together must not change volumes.
    let cube = cuboid(DVec3::ZERO, DVec3::new(30.0, 20.0, 10.0));
    let g0 = gen(&cube, PartingPlane::default(), "two_part_box", &params(&plain()));
    let rot = moldweaver_core::glam::DMat4::from_rotation_x(0.7) * moldweaver_core::glam::DMat4::from_rotation_y(-0.4);
    let tilted = cube.transformed(&rot);
    let n = rot.transform_vector3(DVec3::Z);
    // Spin chosen so the box is axis aligned with the cube again.
    let mut best = None;
    for spin in -180..180 {
        let plane = PartingPlane::from_normal(n, spin as f64, 0.0);
        let b = moldweaver_core::frame::MoldFrame::new(&tilted, &plane).to_mold(&tilted).bounds();
        let area = b.size().x * b.size().y;
        if best.is_none_or(|(a, _)| area < a) {
            best = Some((area, plane));
        }
    }
    let g1 = gen(&tilted, best.unwrap().1, "two_part_box", &params(&plain()));
    assert!(close(g1.output.report.cavity_ml, g0.output.report.cavity_ml, 0.01));
    assert!(close(g1.output.report.silicone_ml, g0.output.report.silicone_ml, 0.01));
}

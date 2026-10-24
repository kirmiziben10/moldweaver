//! Features shared by mold types: pour spout and vents, registration keys,
//! flange fasteners (bolts, zip ties, printed clamps) and the silicone
//! settings. Each feature contributes parameter specs and geometry helpers.

use glam::{DQuat, DVec2, DVec3};
use manifold_rust::manifold::Manifold;

use super::params::{ParamSpec, ParamValue, ParamValues};
use crate::analysis::VerticalRayGrid;
use crate::mesh::TriMesh;
use crate::solid;

pub const G_SPOUT: &str = "Pour spout & vents";
pub const G_KEYS: &str = "Registration keys";
pub const G_CLAMPS: &str = "Clamps & alignment";
pub const G_SILICONE: &str = "Silicone";

// ---------------------------------------------------------------------------
// Parameter specs

pub fn spout_specs() -> Vec<ParamSpec> {
    vec![
        ParamSpec::boolean(G_SPOUT, "spout", "Pour spout", true,
            "Adds a funnel-shaped channel through the top silicone half. When the mold is finished you pour your resin (or other casting material) in through it."),
        ParamSpec::float(G_SPOUT, "spout_diameter", "Spout diameter", 6.0, (2.0, 30.0, 0.5), "mm",
            "Width of the pour channel where it meets the model. Thicker resins need a wider spout.")
            .when("spout", ParamValue::Bool(true)),
        ParamSpec::float(G_SPOUT, "funnel_diameter", "Funnel diameter", 16.0, (4.0, 60.0, 0.5), "mm",
            "Width of the funnel opening at the top of the mold, which makes pouring easier.")
            .when("spout", ParamValue::Bool(true)),
        ParamSpec::boolean(G_SPOUT, "spout_auto", "Place spout automatically", true,
            "Put the spout on the highest point of the model. Untick to choose the position yourself (click the model in the 3D view, or type the coordinates).")
            .when("spout", ParamValue::Bool(true)),
        ParamSpec::float(G_SPOUT, "spout_x", "Spout X", 0.0, (-1000.0, 1000.0, 0.5), "mm",
            "Spout position across the mold (mold coordinates, 0 = centre).")
            .when("spout_auto", ParamValue::Bool(false)),
        ParamSpec::float(G_SPOUT, "spout_y", "Spout Y", 0.0, (-1000.0, 1000.0, 0.5), "mm",
            "Spout position along the mold (mold coordinates, 0 = centre).")
            .when("spout_auto", ParamValue::Bool(false)),
        vents_spec(),
        ParamSpec::float(G_SPOUT, "vent_diameter", "Vent diameter", 2.0, (0.8, 10.0, 0.1), "mm",
            "Width of the air vents. Thin vents leave smaller marks on the cast; very thin vents can clog.")
            .when("vents", ParamValue::Bool(true)),
        ParamSpec::int(G_SPOUT, "vent_count", "Maximum vents", 4, (0, 32),
            "Vents are placed on local high points of the model, where air bubbles would get trapped while casting. This caps how many are made.")
            .when("vents", ParamValue::Bool(true)),
        ParamSpec::float(G_SPOUT, "vent_spacing", "Vent spacing", 12.0, (2.0, 100.0, 0.5), "mm",
            "Minimum distance between two vents (and between a vent and the spout). Larger values give fewer vents.")
            .when("vents", ParamValue::Bool(true)),
    ]
}

pub fn vents_spec() -> ParamSpec {
    ParamSpec::boolean(G_SPOUT, "vents", "Air vents", true,
        "Adds thin channels from the model's local high points (relative to the casting direction) to the outside, so trapped air can escape while you cast.")
}

/// Vent specs only (for mold types without a spout).
pub fn vent_only_specs() -> Vec<ParamSpec> {
    spout_specs().into_iter().filter(|s| s.key.starts_with("vent")).collect()
}

pub fn key_specs() -> Vec<ParamSpec> {
    vec![
        ParamSpec::boolean(G_KEYS, "keys", "Registration keys", true,
            "Bumps on the parting board that become dimples in the first silicone half; the second half fills them, so the two halves lock together in exactly the right position."),
        ParamSpec::choice(G_KEYS, "key_shape", "Key shape", &[("hemisphere", "Hemisphere"), ("cone", "Cone")], "hemisphere",
            "Hemispheres are easy to seat. Cones centre the halves more firmly but are slightly harder to separate.")
            .when("keys", ParamValue::Bool(true)),
        ParamSpec::float(G_KEYS, "key_diameter", "Key diameter", 8.0, (2.0, 40.0, 0.5), "mm",
            "Width of each key. Keep it smaller than the silicone margin so it fits between the model and the wall.")
            .when("keys", ParamValue::Bool(true)),
        ParamSpec::int(G_KEYS, "key_count", "Number of keys", 4, (1, 16),
            "How many keys to place around the model. Four (one per corner) is usually plenty.")
            .when("keys", ParamValue::Bool(true)),
        ParamSpec::choice(G_KEYS, "key_placement", "Placement", &[("corners", "Corners first"), ("even", "Evenly around")], "corners",
            "Corners first: keys go into the box corners (farthest from the model), then the middle of the sides. Evenly around: spaced at equal distances around the model.")
            .when("keys", ParamValue::Bool(true)),
        ParamSpec::float(G_KEYS, "key_inset", "Distance from wall", 0.0, (0.0, 100.0, 0.5), "mm",
            "Distance from the box wall to the key centres. 0 = automatic (half way into the silicone margin).")
            .when("keys", ParamValue::Bool(true)),
    ]
}

pub const CLAMP_STYLES: &[(&str, &str)] = &[
    ("m3", "M3 bolts"),
    ("m4", "M4 bolts"),
    ("zip", "Zip ties"),
    ("clips", "Printed clamps"),
    ("none", "None (tape or rubber bands)"),
];

pub fn clamp_specs() -> Vec<ParamSpec> {
    vec![
        ParamSpec::choice(G_CLAMPS, "clamp_style", "Fastening", CLAMP_STYLES, "m3",
            "How the printed box parts are held together while the silicone cures. Bolts are the most secure; printed clamps need no hardware."),
        ParamSpec::int(G_CLAMPS, "clamps_per_side", "Fasteners per side", 2, (1, 8),
            "How many bolts, zip ties or clamps go along each side of the box."),
        ParamSpec::float(G_CLAMPS, "flange_width", "Flange width", 10.0, (5.0, 30.0, 0.5), "mm",
            "How far the flange (the rim the fasteners go through) sticks out from the box wall."),
        ParamSpec::float(G_CLAMPS, "flange_thickness", "Flange thickness", 4.0, (2.0, 12.0, 0.5), "mm",
            "Thickness of each flange. Thicker is stiffer and keeps the seam tight."),
        ParamSpec::float(G_CLAMPS, "hole_tolerance", "Printing tolerance", 0.3, (0.0, 1.5, 0.05), "mm",
            "Extra size added to holes, slots and sockets because printed holes come out slightly smaller than designed. Increase it if bolts or dowels do not fit."),
    ]
}

pub fn silicone_specs() -> Vec<ParamSpec> {
    vec![
        ParamSpec::float(G_SILICONE, "density", "Silicone density", 1.1, (0.8, 2.0, 0.01), "g/mL",
            "Used to turn the volume into a weight for mixing. Most platinum and tin silicones are 1.05-1.2 g/mL; check your product's data sheet."),
        ParamSpec::float(G_SILICONE, "price_per_kg", "Price per kg", 0.0, (0.0, 1000.0, 0.5), "/kg",
            "Optional: what your silicone costs per kilogram, to estimate the cost of the mold. 0 hides the cost."),
    ]
}

// ---------------------------------------------------------------------------
// Spout and vents

/// Planned pour spout and vents (points on the model surface, mold frame).
#[derive(Clone, Debug, Default)]
pub struct ChannelPlan {
    pub spout: Option<DVec3>,
    pub vents: Vec<DVec3>,
    pub warnings: Vec<String>,
}

/// Plan the spout and vents. While casting, air rises along `+Z`
/// (`up = true`) or `-Z`; vents start at the model's local extremes in that
/// direction and run straight along it. Only points with mold-frame
/// `z > min_z` are used (the silicone side of the parting plane or base).
pub fn plan_channels(model: &TriMesh, params: &ParamValues, up: bool, min_z: f64, want_spout: bool) -> ChannelPlan {
    let mut plan = ChannelPlan::default();
    let s = if up { 1.0 } else { -1.0 };
    // Work on a mirrored copy so "high" always means +Z.
    let mesh = if up { model.clone() } else { model.transformed(&glam::DMat4::from_scale(DVec3::new(1.0, 1.0, -1.0))) };
    let grid = VerticalRayGrid::new(&mesh);
    let b = mesh.bounds();
    // Height in the original (unmirrored) frame.
    let orig_z = |p: &DVec3| s * p.z;
    let spout_r = if want_spout { params.f("spout_diameter") * 0.5 } else { 0.0 };

    if want_spout {
        let spot = if params.b("spout_auto") {
            Some(auto_spout(&mesh, &grid))
        } else {
            let (x, y) = (params.f("spout_x"), params.f("spout_y"));
            match grid.top_z(x, y) {
                Some(z) => Some(DVec3::new(x, y, z)),
                None => {
                    plan.warnings.push(format!(
                        "The pour spout position ({x:.1}, {y:.1}) is not over the model; using the automatic position instead."
                    ));
                    Some(auto_spout(&mesh, &grid))
                }
            }
        };
        if let Some(p) = spot {
            if orig_z(&p) <= min_z {
                plan.warnings
                    .push("The pour spout would start below the parting plane; move the plane or the spout.".into());
            } else {
                plan.spout = Some(p);
            }
        }
    }

    if params.b("vents") && params.i("vent_count") > 0 {
        let spacing = params.f("vent_spacing");
        let mut cands = local_maxima(&mesh, spacing * 0.5);
        cands.retain(|p| orig_z(p) > min_z + 0.5);
        cands.sort_by(|a, b| b.z.total_cmp(&a.z));
        let eps = b.diagonal() * 1e-6;
        let mut chosen: Vec<DVec3> = Vec::new();
        let mut shadowed = 0;
        for p in cands {
            if chosen.len() as i64 >= params.i("vent_count") {
                break;
            }
            let near_spout = plan.spout.is_some_and(|sp| (sp.truncate() - p.truncate()).length() < spacing + spout_r);
            if near_spout || chosen.iter().any(|c| (c.truncate() - p.truncate()).length() < spacing) {
                continue;
            }
            // The vent goes straight up; skip points under an overhang.
            if grid.blocked(p + DVec3::Z * eps * 10.0, true, u32::MAX) {
                shadowed += 1;
                continue;
            }
            chosen.push(p);
        }
        if shadowed > 0 {
            plan.warnings.push(format!(
                "{shadowed} possible air trap(s) sit under an overhang and could not get a straight vent; consider a different orientation or drill a vent by hand."
            ));
        }
        plan.vents = chosen;
    }

    if !up {
        let flip = |p: DVec3| DVec3::new(p.x, p.y, -p.z);
        plan.spout = plan.spout.map(flip);
        plan.vents = plan.vents.into_iter().map(flip).collect();
    }
    plan
}

/// Centre of the topmost region of the model (a flat top gets its spout in
/// the middle rather than on a corner).
fn auto_spout(mesh: &TriMesh, grid: &VerticalRayGrid) -> DVec3 {
    let b = mesh.bounds();
    let tol = (b.size().z * 0.01).max(0.2);
    let top: Vec<DVec3> = mesh.positions.iter().copied().filter(|p| p.z >= b.max.z - tol).collect();
    let highest = mesh.positions.iter().copied().max_by(|a, b| a.z.total_cmp(&b.z)).unwrap_or(DVec3::ZERO);
    if top.len() > 1 {
        let c = top.iter().copied().sum::<DVec3>() / top.len() as f64;
        if let Some(z) = grid.top_z(c.x, c.y) {
            if z >= b.max.z - tol * 2.0 {
                return DVec3::new(c.x, c.y, z);
            }
        }
    }
    highest
}

/// Vertices that are the highest point within `radius` (3D).
pub fn local_maxima(mesh: &TriMesh, radius: f64) -> Vec<DVec3> {
    use std::collections::HashMap;
    let cell = radius.max(1e-3);
    let key = |p: DVec3| ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64);
    let mut hash: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    // Only referenced vertices count.
    let mut used = vec![false; mesh.positions.len()];
    for t in &mesh.triangles {
        for &v in t {
            used[v as usize] = true;
        }
    }
    for (i, p) in mesh.positions.iter().enumerate() {
        if used[i] {
            hash.entry(key(*p)).or_default().push(i);
        }
    }
    let tol = 1e-3;
    let mut out = Vec::new();
    for (i, p) in mesh.positions.iter().enumerate() {
        if !used[i] {
            continue;
        }
        let k = key(*p);
        let mut is_max = true;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in 0..=1 {
                    if let Some(list) = hash.get(&(k.0 + dx, k.1 + dy, k.2 + dz)) {
                        for &j in list {
                            let q = mesh.positions[j];
                            if q.z > p.z + tol && (q - *p).length() <= radius {
                                is_max = false;
                                break 'search;
                            }
                        }
                    }
                }
            }
        }
        if is_max {
            out.push(*p);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Registration keys

/// Axis-aligned rectangle in the parting plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: DVec2,
    pub max: DVec2,
}

impl Rect {
    pub fn new(min: DVec2, max: DVec2) -> Rect {
        Rect { min, max }
    }
    pub fn inset(&self, d: f64) -> Rect {
        Rect { min: self.min + DVec2::splat(d), max: self.max - DVec2::splat(d) }
    }
    pub fn size(&self) -> DVec2 {
        self.max - self.min
    }
    pub fn center(&self) -> DVec2 {
        (self.min + self.max) * 0.5
    }
    /// Point at arc length `s` along the perimeter, counter-clockwise from
    /// the min corner.
    pub fn perimeter_point(&self, s: f64) -> DVec2 {
        let sz = self.size();
        let per = 2.0 * (sz.x + sz.y);
        let mut s = s.rem_euclid(per.max(1e-9));
        if s < sz.x {
            return DVec2::new(self.min.x + s, self.min.y);
        }
        s -= sz.x;
        if s < sz.y {
            return DVec2::new(self.max.x, self.min.y + s);
        }
        s -= sz.y;
        if s < sz.x {
            return DVec2::new(self.max.x - s, self.max.y);
        }
        s -= sz.x;
        DVec2::new(self.min.x, self.max.y - s)
    }
    pub fn perimeter(&self) -> f64 {
        2.0 * (self.size().x + self.size().y)
    }
}

/// Polygons of a cross-section (outer and hole loops), for clearance checks.
pub struct Section {
    pub loops: Vec<Vec<DVec2>>,
}

impl Section {
    pub fn of(model: &Manifold, z: f64) -> Section {
        let cs = model.slice(z);
        Section {
            loops: cs
                .to_polygons()
                .into_iter()
                .map(|l| l.into_iter().map(|p| DVec2::new(p.x, p.y)).collect())
                .collect(),
        }
    }

    /// Enclosed area (even-odd).
    pub fn area(&self) -> f64 {
        self.loops
            .iter()
            .map(|l| {
                let n = l.len();
                (0..n).map(|i| l[i].perp_dot(l[(i + 1) % n])).sum::<f64>() * 0.5
            })
            .sum::<f64>()
            .abs()
    }

    pub fn contains(&self, p: DVec2) -> bool {
        let mut inside = false;
        for l in &self.loops {
            let n = l.len();
            for i in 0..n {
                let (a, b) = (l[i], l[(i + 1) % n]);
                if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
                    inside = !inside;
                }
            }
        }
        inside
    }

    /// Distance to the section outline (0 inside).
    pub fn distance(&self, p: DVec2) -> f64 {
        if self.contains(p) {
            return 0.0;
        }
        let mut d = f64::INFINITY;
        for l in &self.loops {
            let n = l.len();
            for i in 0..n {
                d = d.min(seg_dist(p, l[i], l[(i + 1) % n]));
            }
        }
        d
    }
}

fn seg_dist(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-18)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

/// Key centres in the parting plane. `cavity` is the inside of the box at the
/// plane; keys keep `gap` from the model and from the wall.
pub fn place_keys(cavity: Rect, section: &Section, params: &ParamValues, warnings: &mut Vec<String>) -> Vec<DVec2> {
    let r = params.f("key_diameter") * 0.5;
    let n = params.i("key_count").max(0) as usize;
    let gap = 1.5;
    let min_wall = r + 1.0;
    let mut inset = params.f("key_inset");
    let margin_side = {
        // Smallest distance between the section's bounding box and the wall.
        let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for l in &section.loops {
            for p in l {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        (lo - cavity.min).min_element().min((cavity.max - hi).min_element()).max(0.0)
    };
    if inset <= 0.0 {
        inset = (margin_side * 0.5).max(min_wall);
    }
    inset = inset.max(min_wall);
    let path = cavity.inset(inset);
    if path.size().x <= 0.0 || path.size().y <= 0.0 {
        warnings.push("The box is too small for registration keys at this distance from the wall.".into());
        return Vec::new();
    }
    let per = path.perimeter();
    let sz = path.size();
    // Arc-length positions of the targets.
    let corners = [0.0, sz.x, sz.x + sz.y, 2.0 * sz.x + sz.y];
    let mids = [sz.x * 0.5, sz.x + sz.y * 0.5, 1.5 * sz.x + sz.y, 2.0 * sz.x + 1.5 * sz.y];
    let targets: Vec<f64> = match params.s("key_placement") {
        "even" => (0..n).map(|i| per * i as f64 / n.max(1) as f64 + per / (2.0 * n.max(1) as f64)).collect(),
        _ => {
            let mut t: Vec<f64> = Vec::new();
            // Diagonally opposite corners first so 2 keys still constrain rotation.
            for &i in &[0usize, 2, 1, 3] {
                t.push(corners[i]);
            }
            for &i in &[0usize, 2, 1, 3] {
                t.push(mids[i]);
            }
            let mut extra = 0;
            while t.len() < n {
                extra += 1;
                let k = t.len();
                t.push(per * (k as f64 + 0.5 * extra as f64) / n as f64);
            }
            t.truncate(n);
            t
        }
    };
    let valid = |p: DVec2, chosen: &[DVec2]| {
        section.distance(p) >= r + gap && chosen.iter().all(|c| (*c - p).length() >= 2.0 * r + gap)
    };
    let mut chosen: Vec<DVec2> = Vec::new();
    let step = (r * 0.25).max(0.5);
    for s0 in targets {
        let mut found = None;
        let max_shift = per / (2.0 * n.max(1) as f64);
        let mut d = 0.0;
        while d <= max_shift {
            for s in [s0 + d, s0 - d] {
                let p = path.perimeter_point(s);
                if valid(p, &chosen) {
                    found = Some(p);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
            d += step;
        }
        if let Some(p) = found {
            chosen.push(p);
        }
    }
    if chosen.len() < n {
        warnings.push(format!(
            "Only {} of {} registration keys fit between the model and the wall; increase the silicone margin or use smaller keys.",
            chosen.len(),
            n
        ));
    }
    chosen
}

/// A key bump standing on the plane `z = z0`, pointing to `+Z` (`up`) or `-Z`.
pub fn key_solid(xy: DVec2, z0: f64, params: &ParamValues, up: bool) -> Manifold {
    let r = params.f("key_diameter") * 0.5;
    let s = if up { 1.0 } else { -1.0 };
    let bump = match params.s("key_shape") {
        "cone" => {
            let h = r;
            if up {
                solid::cylinder_z(xy, z0 - 0.01, z0 + h, r, 0.2)
            } else {
                solid::cylinder_z(xy, z0 - h, z0 + 0.01, 0.2, r)
            }
        }
        _ => {
            let sph = solid::sphere(DVec3::new(xy.x, xy.y, z0), r);
            solid::trim_z(&sph, z0 - s * 0.01, up)
        }
    };
    bump
}

// ---------------------------------------------------------------------------
// Flanges, fasteners and clamps

/// Which side of the box a fastener sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    MinX,
    MaxX,
    MinY,
    MaxY,
}

/// Fastener positions along the flange around `outer` (the outside of the
/// box walls): `per_side` evenly spaced on each side, centred in the flange.
pub fn fastener_positions(outer: Rect, flange_w: f64, per_side: usize) -> Vec<(DVec2, Side)> {
    let mut out = Vec::new();
    let off = flange_w * 0.5;
    for i in 0..per_side {
        let t = (i + 1) as f64 / (per_side + 1) as f64;
        let x = outer.min.x + (outer.max.x - outer.min.x) * t;
        let y = outer.min.y + (outer.max.y - outer.min.y) * t;
        out.push((DVec2::new(x, outer.min.y - off), Side::MinY));
        out.push((DVec2::new(x, outer.max.y + off), Side::MaxY));
        out.push((DVec2::new(outer.min.x - off, y), Side::MinX));
        out.push((DVec2::new(outer.max.x + off, y), Side::MaxX));
    }
    out
}

/// Flange corner positions (for alignment dowels).
pub fn corner_positions(outer: Rect, flange_w: f64) -> Vec<DVec2> {
    let o = flange_w * 0.5;
    vec![
        DVec2::new(outer.min.x - o, outer.min.y - o),
        DVec2::new(outer.max.x + o, outer.min.y - o),
        DVec2::new(outer.max.x + o, outer.max.y + o),
        DVec2::new(outer.min.x - o, outer.max.y + o),
    ]
}

/// Nominal bolt diameter for a clamp style.
pub fn bolt_diameter(style: &str) -> Option<f64> {
    match style {
        "m3" => Some(3.0),
        "m4" => Some(4.0),
        _ => None,
    }
}

/// Through-cuts for the fasteners of `style`, spanning `z0..z1`.
pub fn fastener_cuts(style: &str, positions: &[(DVec2, Side)], z0: f64, z1: f64, tol: f64) -> Vec<Manifold> {
    let mut cuts = Vec::new();
    for (p, side) in positions {
        if let Some(d) = bolt_diameter(style) {
            let r = solid::hole_radius(d + tol);
            cuts.push(solid::cylinder_z(*p, z0, z1, r, r));
        } else if style == "zip" {
            // Slot for a 4.8 mm zip tie, long side along the box edge.
            let (lx, ly) = match side {
                Side::MinY | Side::MaxY => (5.0 + tol, 2.0 + tol),
                Side::MinX | Side::MaxX => (2.0 + tol, 5.0 + tol),
            };
            let h = DVec2::new(lx, ly) * 0.5;
            cuts.push(solid::cuboid((*p - h).extend(z0), (*p + h).extend(z1)));
        }
    }
    cuts
}

/// A printed C-clamp that slides over flanges stacked between `z0` and `z1`
/// at `pos` on `side` of `outer`. Returns the clamp in place (mold frame)
/// and its print rotation (U profile flat on the bed).
pub fn clip(outer: Rect, pos: DVec2, side: Side, flange_w: f64, z0: f64, z1: f64) -> (Manifold, DQuat) {
    let t = 3.0; // clamp wall
    let cl = 0.15; // snug sliding fit
    let len = 14.0;
    let reach = 1.0; // stop short of the wall
    let (zlo, zhi) = (z0 - cl, z1 + cl);
    // Build for the +Y side, then rotate about Z into place.
    let body = solid::cuboid(DVec3::new(-len * 0.5, reach, zlo - t), DVec3::new(len * 0.5, flange_w + cl + t, zhi + t));
    let gap = solid::cuboid(DVec3::new(-len, reach - 1.0, zlo), DVec3::new(len, flange_w + cl, zhi));
    let c = body.difference(&gap);
    let (angle, edge) = match side {
        Side::MaxY => (0.0, DVec2::new(pos.x, outer.max.y)),
        Side::MinY => (std::f64::consts::PI, DVec2::new(pos.x, outer.min.y)),
        Side::MaxX => (-std::f64::consts::FRAC_PI_2, DVec2::new(outer.max.x, pos.y)),
        Side::MinX => (std::f64::consts::FRAC_PI_2, DVec2::new(outer.min.x, pos.y)),
    };
    let rz = DQuat::from_rotation_z(angle);
    let placed = solid::rotate(&c, rz).translate(solid::v3(edge.extend(0.0)));
    // Print orientation: undo the placement rotation, then stand the clip on
    // its U profile (local X -> +Z).
    let print = DQuat::from_rotation_y(-std::f64::consts::FRAC_PI_2) * rz.inverse();
    (placed, print)
}

/// Silicone figures from the common parameters.
pub fn density_and_price(params: &ParamValues) -> (f64, f64) {
    (params.f("density"), params.f("price_per_kg"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::molds::params::ParamValues;

    fn key_params(n: i64, placement: &str) -> ParamValues {
        let mut v = ParamValues::defaults(&key_specs());
        v.set("key_count", ParamValue::Int(n));
        v.set("key_placement", ParamValue::Choice(placement.into()));
        v
    }

    #[test]
    fn fasteners_are_symmetric() {
        let outer = Rect::new(DVec2::new(-30.0, -20.0), DVec2::new(30.0, 20.0));
        for n in 1..5 {
            let pos = fastener_positions(outer, 10.0, n);
            assert_eq!(pos.len(), 4 * n);
            for (p, _) in &pos {
                for m in [DVec2::new(-p.x, p.y), DVec2::new(p.x, -p.y)] {
                    assert!(pos.iter().any(|(q, _)| (*q - m).length() < 1e-9), "{p:?} has no mirror");
                }
            }
        }
    }

    #[test]
    fn keys_avoid_model_and_each_other() {
        let cavity = Rect::new(DVec2::splat(-30.0), DVec2::splat(30.0));
        let section = Section {
            loops: vec![vec![
                DVec2::new(-20.0, -20.0),
                DVec2::new(20.0, -20.0),
                DVec2::new(20.0, 20.0),
                DVec2::new(-20.0, 20.0),
            ]],
        };
        for placement in ["corners", "even"] {
            let mut w = Vec::new();
            let keys = place_keys(cavity, &section, &key_params(4, placement), &mut w);
            assert_eq!(keys.len(), 4, "{placement}: {w:?}");
            for k in &keys {
                assert!(section.distance(*k) >= 4.0 + 1.5 - 1e-9);
                assert!(k.x.abs() <= 30.0 - 5.0 + 1e-9 && k.y.abs() <= 30.0 - 5.0 + 1e-9);
            }
        }
    }

    #[test]
    fn keys_warn_when_crowded() {
        let cavity = Rect::new(DVec2::splat(-12.0), DVec2::splat(12.0));
        let section = Section {
            loops: vec![vec![
                DVec2::new(-10.0, -10.0),
                DVec2::new(10.0, -10.0),
                DVec2::new(10.0, 10.0),
                DVec2::new(-10.0, 10.0),
            ]],
        };
        let mut w = Vec::new();
        let keys = place_keys(cavity, &section, &key_params(4, "corners"), &mut w);
        assert!(keys.len() < 4 && !w.is_empty());
    }

    #[test]
    fn rect_perimeter_walk() {
        let r = Rect::new(DVec2::ZERO, DVec2::new(4.0, 2.0));
        assert_eq!(r.perimeter_point(0.0), DVec2::ZERO);
        assert_eq!(r.perimeter_point(5.0), DVec2::new(4.0, 1.0));
        assert_eq!(r.perimeter_point(7.0), DVec2::new(3.0, 2.0));
        assert_eq!(r.perimeter_point(12.0), DVec2::ZERO);
    }
}

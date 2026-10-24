//! Parting analysis: undercut detection and automatic plane suggestions.
//!
//! All functions take a mesh in the mold frame (see [`crate::frame`]), where
//! the upper silicone half is pulled towards `+Z` and the lower half towards
//! `-Z`. A face *locks* (is an undercut) if moving its silicone half along the
//! pull direction would drag silicone through the model: either the face
//! looks against the pull direction, or the model shadows it along the pull
//! direction.

use glam::{DVec2, DVec3};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::frame::{MoldFrame, PartingPlane};
use crate::mesh::TriMesh;

/// Uniform 2D grid over the XY projection of a mesh, for fast vertical rays.
pub struct VerticalRayGrid<'a> {
    mesh: &'a TriMesh,
    origin: DVec2,
    cell: f64,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u32>>,
}

impl<'a> VerticalRayGrid<'a> {
    pub fn new(mesh: &'a TriMesh) -> Self {
        let b = mesh.bounds();
        let n = mesh.triangles.len().max(1);
        let size = b.size();
        let area = (size.x * size.y).max(1e-9);
        // About two triangles per cell on average.
        let cell = (area / n as f64 * 2.0).sqrt().max(size.x.max(size.y) / 1024.0).max(1e-6);
        let nx = ((size.x / cell).ceil() as usize).max(1);
        let ny = ((size.y / cell).ceil() as usize).max(1);
        let origin = DVec2::new(b.min.x, b.min.y);
        let mut cells = vec![Vec::new(); nx * ny];
        for (i, t) in mesh.triangles.iter().enumerate() {
            let ps = t.map(|v| mesh.positions[v as usize]);
            let minx = ps.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
            let maxx = ps.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
            let miny = ps.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
            let maxy = ps.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
            let (x0, x1) = (Self::idx(minx, origin.x, cell, nx), Self::idx(maxx, origin.x, cell, nx));
            let (y0, y1) = (Self::idx(miny, origin.y, cell, ny), Self::idx(maxy, origin.y, cell, ny));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    cells[y * nx + x].push(i as u32);
                }
            }
        }
        VerticalRayGrid {
            mesh,
            origin,
            cell,
            nx,
            ny,
            cells,
        }
    }

    fn idx(v: f64, o: f64, cell: f64, n: usize) -> usize {
        (((v - o) / cell).floor().max(0.0) as usize).min(n - 1)
    }

    /// Heights at which the vertical line through `(x, y)` crosses the mesh,
    /// with the triangle index of each crossing.
    pub fn crossings(&self, x: f64, y: f64, mut f: impl FnMut(f64, u32)) {
        let b = self.mesh.bounds();
        if x < b.min.x || x > b.max.x || y < b.min.y || y > b.max.y {
            return;
        }
        let cx = Self::idx(x, self.origin.x, self.cell, self.nx);
        let cy = Self::idx(y, self.origin.y, self.cell, self.ny);
        for &ti in &self.cells[cy * self.nx + cx] {
            let [a, b, c] = self.mesh.tri(ti as usize);
            if let Some(z) = vertical_hit(a, b, c, x, y) {
                f(z, ti);
            }
        }
    }

    /// Does the ray from `p` along `+Z` (`up = true`) or `-Z` hit the mesh,
    /// ignoring triangle `skip`?
    pub fn blocked(&self, p: DVec3, up: bool, skip: u32) -> bool {
        let mut hit = false;
        self.crossings(p.x, p.y, |z, ti| {
            if ti != skip && ((up && z > p.z) || (!up && z < p.z)) {
                hit = true;
            }
        });
        hit
    }

    /// Highest crossing of the vertical line, if any.
    pub fn top_z(&self, x: f64, y: f64) -> Option<f64> {
        let mut best: Option<f64> = None;
        self.crossings(x, y, |z, _| best = Some(best.map_or(z, |b: f64| b.max(z))));
        best
    }

    /// Lowest crossing of the vertical line, if any.
    pub fn bottom_z(&self, x: f64, y: f64) -> Option<f64> {
        let mut best: Option<f64> = None;
        self.crossings(x, y, |z, _| best = Some(best.map_or(z, |b: f64| b.min(z))));
        best
    }
}

/// Height of triangle `abc` above `(x, y)`, if the point is inside its XY
/// projection.
fn vertical_hit(a: DVec3, b: DVec3, c: DVec3, x: f64, y: f64) -> Option<f64> {
    let d = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
    if d.abs() < 1e-18 {
        return None;
    }
    let w1 = ((x - a.x) * (c.y - a.y) - (c.x - a.x) * (y - a.y)) / d;
    let w2 = ((b.x - a.x) * (y - a.y) - (x - a.x) * (b.y - a.y)) / d;
    let w0 = 1.0 - w1 - w2;
    let eps = -1e-12;
    if w0 >= eps && w1 >= eps && w2 >= eps {
        Some(w0 * a.z + w1 * b.z + w2 * c.z)
    } else {
        None
    }
}

/// How a triangle relates to the parting plane and the pull directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaceClass {
    /// Upper half, releases cleanly when pulled up.
    Upper,
    /// Lower half, releases cleanly when pulled down.
    Lower,
    /// Upper half but locks into the silicone.
    UpperUndercut,
    /// Lower half but locks into the silicone.
    LowerUndercut,
}

impl FaceClass {
    pub fn is_undercut(self) -> bool {
        matches!(self, FaceClass::UpperUndercut | FaceClass::LowerUndercut)
    }
}

/// Which sides of the plane the silicone covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaneUsage {
    /// The plane splits the silicone into an upper and a lower half
    /// (two-part molds).
    Split,
    /// The plane is the base the model sits on; only the upper side is
    /// covered and pulled up (open one-part molds).
    Base,
}

/// Per-triangle release flags in the mold frame (independent of the plane
/// height): can the triangle release upward, and downward?
pub struct ReleaseFlags {
    pub up_ok: Vec<bool>,
    pub down_ok: Vec<bool>,
    pub centroid_z: Vec<f64>,
    pub area: Vec<f64>,
}

/// Normals within this angle of horizontal still count as releasing; silicone
/// stretches enough for vertical walls.
const NORMAL_EPS: f64 = 0.02;

pub fn release_flags(mesh: &TriMesh) -> ReleaseFlags {
    let grid = VerticalRayGrid::new(mesh);
    let eps = mesh.bounds().diagonal() * 1e-6;
    let res: Vec<(bool, bool, f64, f64)> = (0..mesh.triangles.len())
        .into_par_iter()
        .map(|i| {
            let cross = mesh.tri_cross(i);
            let area = cross.length() * 0.5;
            let n = cross.normalize_or_zero();
            let c = mesh.tri_centroid(i);
            let p = c + n * eps;
            let up_ok = n.z >= -NORMAL_EPS && !grid.blocked(p, true, i as u32);
            let down_ok = n.z <= NORMAL_EPS && !grid.blocked(p, false, i as u32);
            (up_ok, down_ok, c.z, area)
        })
        .collect();
    ReleaseFlags {
        up_ok: res.iter().map(|r| r.0).collect(),
        down_ok: res.iter().map(|r| r.1).collect(),
        centroid_z: res.iter().map(|r| r.2).collect(),
        area: res.iter().map(|r| r.3).collect(),
    }
}

/// Classify every triangle of a mold-frame mesh for a plane at `z = 0`.
/// With [`PlaneUsage::Base`], faces below the base (cut off) count as
/// `Lower`, and faces lying on the base are the open back of the mold.
pub fn classify_faces(mesh: &TriMesh, usage: PlaneUsage) -> Vec<FaceClass> {
    let flags = release_flags(mesh);
    let base_tol = (mesh.bounds().size().z * 1e-4).max(1e-3);
    (0..mesh.triangles.len())
        .map(|i| {
            if usage == PlaneUsage::Base {
                let z = flags.centroid_z[i];
                if z < -base_tol {
                    return FaceClass::Lower;
                }
                if z <= base_tol && mesh.tri_normal(i).z < -0.9 {
                    return FaceClass::Upper;
                }
            }
            let upper = usage == PlaneUsage::Base || flags.centroid_z[i] > 0.0;
            match (upper, flags.up_ok[i], flags.down_ok[i]) {
                (true, true, _) => FaceClass::Upper,
                (true, false, _) => FaceClass::UpperUndercut,
                (false, _, true) => FaceClass::Lower,
                (false, _, false) => FaceClass::LowerUndercut,
            }
        })
        .collect()
}

/// Undercut statistics for a classified mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UndercutStats {
    pub undercut_area_mm2: f64,
    pub total_area_mm2: f64,
    pub undercut_faces: usize,
}

impl UndercutStats {
    pub fn fraction(&self) -> f64 {
        if self.total_area_mm2 > 0.0 {
            self.undercut_area_mm2 / self.total_area_mm2
        } else {
            0.0
        }
    }
}

pub fn undercut_stats(mesh: &TriMesh, classes: &[FaceClass]) -> UndercutStats {
    let mut s = UndercutStats::default();
    for (i, c) in classes.iter().enumerate() {
        let a = mesh.tri_area(i);
        s.total_area_mm2 += a;
        if c.is_undercut() {
            s.undercut_area_mm2 += a;
            s.undercut_faces += 1;
        }
    }
    s
}

/// A suggested parting plane with its score.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub plane: PartingPlane,
    /// Fraction of the surface area that would lock (0..1).
    pub undercut_fraction: f64,
    /// Fraction of the surface sitting flat on the base (Base usage only).
    pub flat_base_fraction: f64,
}

/// Points evenly spread over the upper hemisphere (or the full sphere).
fn fibonacci_dirs(n: usize, full_sphere: bool) -> Vec<DVec3> {
    let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
    (0..n)
        .map(|i| {
            let t = (i as f64 + 0.5) / n as f64;
            let z = if full_sphere { 1.0 - 2.0 * t } else { 1.0 - t };
            let r = (1.0 - z * z).max(0.0).sqrt();
            let th = golden * i as f64;
            DVec3::new(r * th.cos(), r * th.sin(), z)
        })
        .collect()
}

/// Large flat regions of the model: (normal, area), biggest first.
fn flat_face_normals(mesh: &TriMesh, max: usize) -> Vec<(DVec3, f64)> {
    use std::collections::HashMap;
    let mut buckets: HashMap<(i32, i32, i32), (DVec3, f64)> = HashMap::new();
    for i in 0..mesh.triangles.len() {
        let c = mesh.tri_cross(i);
        let a = c.length() * 0.5;
        if a <= 0.0 {
            continue;
        }
        let n = c / (2.0 * a);
        let key = ((n.x * 50.0).round() as i32, (n.y * 50.0).round() as i32, (n.z * 50.0).round() as i32);
        let e = buckets.entry(key).or_insert((DVec3::ZERO, 0.0));
        e.0 += n * a;
        e.1 += a;
    }
    let mut v: Vec<(DVec3, f64)> = buckets.into_values().map(|(n, a)| (n.normalize_or_zero(), a)).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.truncate(max);
    v
}

/// Section perimeter of the mesh at height `h` (sum of slice segment lengths).
fn section_perimeter(mesh: &TriMesh, h: f64) -> f64 {
    let mut total = 0.0;
    for i in 0..mesh.triangles.len() {
        let ps = mesh.tri(i);
        let mut pts = [DVec3::ZERO; 2];
        let mut k = 0;
        for e in 0..3 {
            let (a, b) = (ps[e], ps[(e + 1) % 3]);
            if (a.z - h) * (b.z - h) < 0.0 && k < 2 {
                let t = (h - a.z) / (b.z - a.z);
                pts[k] = a + (b - a) * t;
                k += 1;
            }
        }
        if k == 2 {
            total += (pts[1] - pts[0]).length();
        }
    }
    total
}

/// Box spin (about the pull axis) that minimises the XY footprint.
fn best_spin(mesh_mold: &TriMesh) -> f64 {
    // Use the convex hull of the projected vertices for speed.
    let pts: Vec<DVec2> = convex_hull_2d(mesh_mold.positions.iter().map(|p| DVec2::new(p.x, p.y)).collect());
    let mut best = (f64::INFINITY, 0.0);
    for step in 0..90 {
        let a = (step as f64).to_radians();
        let (s, c) = a.sin_cos();
        let (mut x0, mut x1, mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for p in &pts {
            let x = c * p.x - s * p.y;
            let y = s * p.x + c * p.y;
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        // Small bias towards 0 so symmetric models keep their orientation.
        let area = (x1 - x0) * (y1 - y0) * (1.0 + 1e-4 * step as f64);
        if area < best.0 {
            best = (area, step as f64);
        }
    }
    // Rotating the mold frame by +a means rotating the box by -a around the model.
    -best.1
}

pub fn convex_hull_2d(mut pts: Vec<DVec2>) -> Vec<DVec2> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: DVec2, a: DVec2, b: DVec2| (a - o).perp_dot(b - o);
    let mut hull: Vec<DVec2> = Vec::with_capacity(pts.len() * 2);
    for p in pts.iter().chain(pts.iter().rev().skip(1)) {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], *p) <= 0.0 {
            hull.pop();
        }
        hull.push(*p);
    }
    hull.pop();
    hull
}

/// Pick a subset of triangles to cast rays from (all of them for small
/// meshes) so suggestions stay interactive on big scans.
fn sample_triangles(n: usize, max: usize) -> Vec<usize> {
    if n <= max {
        return (0..n).collect();
    }
    let step = n as f64 / max as f64;
    (0..max).map(|i| (i as f64 * step) as usize).collect()
}

/// Score one pull direction: returns (best offset along the direction from
/// the bounding-box centre, undercut fraction, flat-base fraction).
fn score_direction(model: &TriMesh, dir: DVec3, usage: PlaneUsage, samples: &[usize]) -> (f64, f64, f64) {
    let plane = PartingPlane::from_normal(dir, 0.0, 0.0);
    let frame = MoldFrame::new(model, &plane);
    let mesh = frame.to_mold(model);
    let grid = VerticalRayGrid::new(&mesh);
    let b = mesh.bounds();
    let eps = b.diagonal() * 1e-6;
    // Offsets are measured from the model's bounding-box centre along `dir`;
    // the mold frame has z = 0 there.
    let z_center = 0.0;
    let mut rows: Vec<(f64, f64, bool, bool, f64)> = Vec::with_capacity(samples.len());
    let mut total = 0.0;
    for &i in samples {
        let cross = mesh.tri_cross(i);
        let area = cross.length() * 0.5;
        if area <= 0.0 {
            continue;
        }
        let n = cross / (2.0 * area);
        let c = mesh.tri_centroid(i);
        let p = c + n * eps;
        let up_ok = n.z >= -NORMAL_EPS && !grid.blocked(p, true, i as u32);
        let down_ok = n.z <= NORMAL_EPS && !grid.blocked(p, false, i as u32);
        rows.push((c.z, area, up_ok, down_ok, n.z));
        total += area;
    }
    if total <= 0.0 {
        return (0.0, 1.0, 0.0);
    }
    match usage {
        PlaneUsage::Base => {
            // The base sits at height h; everything below is cut off and the
            // cast gets a flat back there. Flat downward faces at the bottom
            // make a natural back. Without one, cut into the model a little,
            // trading lost detail for an open back.
            let zmin = b.min.z;
            let height = b.size().z.max(1e-9);
            let tol = (height * 0.005).max(0.05);
            let flat: f64 = rows
                .iter()
                .filter(|r| r.4 < -0.999 && r.0 - zmin < tol)
                .map(|r| r.1)
                .sum::<f64>()
                / total;
            let mut best = (f64::INFINITY, zmin, 1.0);
            for step in 0..=12 {
                let frac = step as f64 / 24.0; // up to half the height
                let h = zmin + frac * height;
                if step == 0 && flat < 0.01 {
                    continue; // would sit on a point or an edge
                }
                let (mut under, mut kept) = (0.0, 0.0);
                for &(z, a, up_ok, _, nz) in &rows {
                    if z < h || (step == 0 && nz < -0.999 && z - zmin < tol) {
                        continue;
                    }
                    kept += a;
                    if !up_ok {
                        under += a;
                    }
                }
                let uf = if kept > 0.0 { under / kept } else { 1.0 };
                let score = uf + 0.6 * frac;
                if score < best.0 {
                    best = (score, h, uf);
                }
            }
            (best.1 - z_center, best.2, flat)
        }
        PlaneUsage::Split => {
            rows.sort_by(|a, b| a.0.total_cmp(&b.0));
            // Plane between rows k-1 and k: rows < k are lower, rows >= k upper.
            let m = rows.len();
            let mut lower_bad = vec![0.0; m + 1];
            for k in 0..m {
                lower_bad[k + 1] = lower_bad[k] + if rows[k].3 { 0.0 } else { rows[k].1 };
            }
            let mut upper_bad = vec![0.0; m + 1];
            for k in (0..m).rev() {
                upper_bad[k] = upper_bad[k + 1] + if rows[k].2 { 0.0 } else { rows[k].1 };
            }
            // Candidate heights: 64 evenly spaced cuts inside the model.
            let (z0, z1) = (b.min.z, b.max.z);
            let mut cands: Vec<(f64, f64)> = Vec::new();
            let mut k = 0;
            for s in 1..64 {
                let h = z0 + (z1 - z0) * s as f64 / 64.0;
                while k < m && rows[k].0 <= h {
                    k += 1;
                }
                cands.push((h, (lower_bad[k] + upper_bad[k]) / total));
            }
            let best_u = cands.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
            // Among near-best heights prefer the widest section.
            let near: Vec<(f64, f64)> = cands.iter().copied().filter(|c| c.1 <= best_u + 0.005).collect();
            let mut best = (near[0].0, near[0].1, f64::NEG_INFINITY);
            for (h, u) in near {
                let w = section_perimeter(&mesh, h);
                if w > best.2 + 1e-9 {
                    best = (h, u, w);
                }
            }
            (best.0 - z_center, best.1, 0.0)
        }
    }
}

/// Suggest a parting plane (orientation, height and box spin) that
/// minimises undercut area, keeping the parting line on a wide section.
pub fn suggest_plane(model: &TriMesh, usage: PlaneUsage) -> Suggestion {
    let samples = sample_triangles(model.triangles.len(), 20_000);
    let mut dirs = match usage {
        PlaneUsage::Split => fibonacci_dirs(160, false),
        PlaneUsage::Base => fibonacci_dirs(240, true),
    };
    for axis in [DVec3::X, DVec3::Y, DVec3::Z, DVec3::NEG_X, DVec3::NEG_Y, DVec3::NEG_Z] {
        dirs.push(axis);
    }
    for (n, _) in flat_face_normals(model, 8) {
        match usage {
            // Flat faces are good bases (their normal points down onto the base).
            PlaneUsage::Base => dirs.push(-n),
            PlaneUsage::Split => dirs.push(n),
        }
    }
    let scored: Vec<(DVec3, (f64, f64, f64))> = dirs
        .par_iter()
        .map(|&d| (d, score_direction(model, d, usage, &samples)))
        .collect();
    let objective = |s: &(f64, f64, f64), d: DVec3| {
        let flat_bonus = if usage == PlaneUsage::Base { 0.5 * s.2 } else { 0.0 };
        // A small preference for the model's own up axis breaks ties.
        s.1 - flat_bonus + 0.002 * (1.0 - d.z)
    };
    let (dir, best) = scored
        .iter()
        .min_by(|a, b| objective(&a.1, a.0).total_cmp(&objective(&b.1, b.0)))
        .copied()
        .unwrap_or((DVec3::Z, (0.0, 1.0, 0.0)));
    let mut plane = PartingPlane::from_normal(dir, 0.0, best.0);
    let frame = MoldFrame::new(model, &PartingPlane { offset_mm: 0.0, ..plane });
    plane.spin_deg = best_spin(&frame.to_mold(model));
    Suggestion {
        plane,
        undercut_fraction: best.1,
        flat_base_fraction: best.2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::primitives::{cuboid, uv_sphere};

    #[test]
    fn sphere_split_at_equator_has_no_undercuts() {
        let s = uv_sphere(DVec3::ZERO, 10.0, 48, 24);
        let classes = classify_faces(&s, PlaneUsage::Split);
        let st = undercut_stats(&s, &classes);
        assert!(st.fraction() < 1e-9, "{st:?}");
        // Split above the equator: the lower half's upper band locks.
        let shifted = s.translated(DVec3::new(0.0, 0.0, 5.0));
        let st = undercut_stats(&shifted, &classify_faces(&shifted, PlaneUsage::Split));
        assert!(st.fraction() > 0.1, "{st:?}");
    }

    #[test]
    fn suggestion_finds_sphere_equator() {
        let s = uv_sphere(DVec3::new(3.0, 4.0, 5.0), 10.0, 48, 24);
        let sug = suggest_plane(&s, PlaneUsage::Split);
        assert!(sug.undercut_fraction < 0.01, "{sug:?}");
        // Equator: the offset from the centre is ~0 for whatever direction.
        assert!(sug.plane.offset_mm.abs() < 1.0, "{sug:?}");
    }

    #[test]
    fn suggestion_for_tray_picks_flat_face() {
        // A tall box lying on its side: the largest flat face should become
        // the base. Any face of a box is undercut-free, so the flat bonus
        // decides.
        let m = cuboid(DVec3::ZERO, DVec3::new(40.0, 10.0, 20.0));
        let sug = suggest_plane(&m, PlaneUsage::Base);
        let n = sug.plane.normal();
        assert!(n.y.abs() > 0.99, "normal {n:?}");
        assert!(sug.undercut_fraction < 1e-6);
    }

    #[test]
    fn mushroom_has_undercut_unless_split_at_cap() {
        // A wide cap on a thin stem: splitting through the stem leaves the
        // cap's underside in the upper half, facing against the pull.
        let mut m = cuboid(DVec3::new(-20.0, -20.0, 10.0), DVec3::new(20.0, 20.0, 20.0));
        m.append(&cuboid(DVec3::new(-3.0, -3.0, 0.0), DVec3::new(3.0, 3.0, 10.0)));
        let frame_mesh = m.translated(DVec3::new(0.0, 0.0, -7.0));
        let st = undercut_stats(&frame_mesh, &classify_faces(&frame_mesh, PlaneUsage::Split));
        assert!(st.undercut_area_mm2 > 1000.0, "{st:?}");
    }

    #[test]
    fn base_faces_are_the_open_back() {
        let m = cuboid(DVec3::new(-5.0, -5.0, 0.0), DVec3::new(5.0, 5.0, 8.0));
        let st = undercut_stats(&m, &classify_faces(&m, PlaneUsage::Base));
        assert_eq!(st.undercut_faces, 0, "{st:?}");
    }

    #[test]
    fn grid_top_and_bottom() {
        let m = cuboid(DVec3::new(-1.0, -1.0, 2.0), DVec3::new(1.0, 1.0, 5.0));
        let g = VerticalRayGrid::new(&m);
        assert_eq!(g.top_z(0.2, 0.3), Some(5.0));
        assert_eq!(g.bottom_z(0.2, 0.3), Some(2.0));
        assert_eq!(g.top_z(3.0, 0.0), None);
    }
}

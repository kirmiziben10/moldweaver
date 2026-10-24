//! Indexed triangle meshes, topology checks, basic repair and conversion
//! to/from the `manifold-rust` solid representation.

use std::collections::HashMap;

use glam::{DMat4, DVec3};
use manifold_rust::manifold::Manifold;
use manifold_rust::types::{Error as ManifoldError, MeshGL64};

/// Axis-aligned bounding box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: DVec3,
    pub max: DVec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: DVec3::splat(f64::INFINITY), max: DVec3::splat(f64::NEG_INFINITY) };

    pub fn from_points<'a>(points: impl IntoIterator<Item = &'a DVec3>) -> Aabb {
        let mut b = Aabb::EMPTY;
        for p in points {
            b.extend(*p);
        }
        b
    }

    pub fn new(min: DVec3, max: DVec3) -> Aabb {
        Aabb { min, max }
    }

    pub fn extend(&mut self, p: DVec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, other: &Aabb) -> Aabb {
        Aabb { min: self.min.min(other.min), max: self.max.max(other.max) }
    }

    pub fn is_empty(&self) -> bool {
        !(self.min.x <= self.max.x && self.min.y <= self.max.y && self.min.z <= self.max.z)
    }

    pub fn size(&self) -> DVec3 {
        if self.is_empty() {
            DVec3::ZERO
        } else {
            self.max - self.min
        }
    }

    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    pub fn diagonal(&self) -> f64 {
        self.size().length()
    }

    pub fn expanded(&self, by: DVec3) -> Aabb {
        Aabb { min: self.min - by, max: self.max + by }
    }

    pub fn translated(&self, by: DVec3) -> Aabb {
        Aabb { min: self.min + by, max: self.max + by }
    }
}

/// An indexed triangle mesh in millimetres. Triangles are counter-clockwise
/// when seen from outside (right-hand rule gives the outward normal).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriMesh {
    pub positions: Vec<DVec3>,
    pub triangles: Vec<[u32; 3]>,
}

/// Summary of the edge topology of a mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TopologyReport {
    pub vertices: usize,
    pub triangles: usize,
    /// Edges used by exactly one triangle (holes).
    pub boundary_edges: usize,
    /// Edges used by more than two triangles.
    pub non_manifold_edges: usize,
    /// Edges shared by two triangles that run in the same direction
    /// (neighbouring faces with flipped normals).
    pub inconsistent_edges: usize,
    /// Triangles with repeated vertices or (near) zero area.
    pub degenerate_triangles: usize,
    /// Number of edge-connected shells.
    pub shells: usize,
}

impl TopologyReport {
    /// Closed, 2-manifold and consistently oriented.
    pub fn is_watertight(&self) -> bool {
        self.boundary_edges == 0 && self.non_manifold_edges == 0 && self.inconsistent_edges == 0 && self.triangles > 0
    }
}

/// Error converting a mesh into a solid.
#[derive(Debug, thiserror::Error)]
pub enum SolidError {
    #[error("the mesh is empty")]
    Empty,
    #[error("the mesh is not a closed manifold solid ({0:?}); try Repair")]
    NotManifold(ManifoldError),
}

impl TriMesh {
    pub fn new(positions: Vec<DVec3>, triangles: Vec<[u32; 3]>) -> TriMesh {
        TriMesh { positions, triangles }
    }

    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    pub fn bounds(&self) -> Aabb {
        Aabb::from_points(&self.positions)
    }

    pub fn tri(&self, i: usize) -> [DVec3; 3] {
        let t = self.triangles[i];
        [self.positions[t[0] as usize], self.positions[t[1] as usize], self.positions[t[2] as usize]]
    }

    /// Area-weighted (unnormalised, length = 2 * area) normal of triangle `i`.
    pub fn tri_cross(&self, i: usize) -> DVec3 {
        let [a, b, c] = self.tri(i);
        (b - a).cross(c - a)
    }

    pub fn tri_normal(&self, i: usize) -> DVec3 {
        self.tri_cross(i).normalize_or_zero()
    }

    pub fn tri_area(&self, i: usize) -> f64 {
        self.tri_cross(i).length() * 0.5
    }

    pub fn tri_centroid(&self, i: usize) -> DVec3 {
        let [a, b, c] = self.tri(i);
        (a + b + c) / 3.0
    }

    pub fn surface_area(&self) -> f64 {
        (0..self.triangles.len()).map(|i| self.tri_area(i)).sum()
    }

    /// Signed volume via the divergence theorem (positive for outward normals).
    pub fn signed_volume(&self) -> f64 {
        let mut v = 0.0;
        for i in 0..self.triangles.len() {
            let [a, b, c] = self.tri(i);
            v += a.dot(b.cross(c));
        }
        v / 6.0
    }

    pub fn transformed(&self, m: &DMat4) -> TriMesh {
        let flip = m.determinant() < 0.0;
        TriMesh {
            positions: self.positions.iter().map(|p| m.transform_point3(*p)).collect(),
            triangles: if flip {
                self.triangles.iter().map(|t| [t[0], t[2], t[1]]).collect()
            } else {
                self.triangles.clone()
            },
        }
    }

    pub fn translated(&self, by: DVec3) -> TriMesh {
        TriMesh { positions: self.positions.iter().map(|p| *p + by).collect(), triangles: self.triangles.clone() }
    }

    /// Append another mesh (no welding).
    pub fn append(&mut self, other: &TriMesh) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.triangles.extend(other.triangles.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
    }

    /// Smooth per-vertex normals (area weighted).
    pub fn vertex_normals(&self) -> Vec<DVec3> {
        let mut n = vec![DVec3::ZERO; self.positions.len()];
        for (i, t) in self.triangles.iter().enumerate() {
            let c = self.tri_cross(i);
            for &v in t {
                n[v as usize] += c;
            }
        }
        n.into_iter().map(|v| v.normalize_or_zero()).collect()
    }

    /// Edge-topology summary. Assumes vertices are already welded.
    pub fn topology(&self) -> TopologyReport {
        let mut edges: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
        let mut degenerate = 0;
        let diag = self.bounds().diagonal().max(1e-9);
        let area_eps = (diag * 1e-9).powi(2);
        for (i, t) in self.triangles.iter().enumerate() {
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] || self.tri_cross(i).length_squared() <= area_eps {
                degenerate += 1;
            }
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                if a == b {
                    continue;
                }
                let key = (a.min(b), a.max(b));
                let e = edges.entry(key).or_insert((0, 0));
                if a < b {
                    e.0 += 1;
                } else {
                    e.1 += 1;
                }
            }
        }
        let mut report = TopologyReport {
            vertices: self.positions.len(),
            triangles: self.triangles.len(),
            degenerate_triangles: degenerate,
            ..Default::default()
        };
        for (fwd, back) in edges.values() {
            match fwd + back {
                1 => report.boundary_edges += 1,
                2 => {
                    if *fwd != 1 {
                        report.inconsistent_edges += 1
                    }
                }
                _ => report.non_manifold_edges += 1,
            }
        }
        report.shells = self.shell_count();
        report
    }

    fn shell_count(&self) -> usize {
        let mut ds = DisjointSet::new(self.positions.len());
        for t in &self.triangles {
            ds.union(t[0] as usize, t[1] as usize);
            ds.union(t[1] as usize, t[2] as usize);
        }
        let mut roots = std::collections::HashSet::new();
        for t in &self.triangles {
            roots.insert(ds.find(t[0] as usize));
        }
        roots.len()
    }

    /// Merge vertices closer than `tol` and drop unused vertices.
    pub fn welded(&self, tol: f64) -> TriMesh {
        let tol = tol.max(1e-12);
        let inv = 1.0 / tol;
        let key = |p: DVec3| ((p.x * inv).round() as i64, (p.y * inv).round() as i64, (p.z * inv).round() as i64);
        let mut map: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let mut positions: Vec<DVec3> = Vec::new();
        let mut remap = Vec::with_capacity(self.positions.len());
        for p in &self.positions {
            let k = key(*p);
            // Look in the neighbouring cells too so points straddling a cell
            // boundary still merge.
            let mut found = None;
            'outer: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(&idx) = map.get(&(k.0 + dx, k.1 + dy, k.2 + dz)) {
                            if positions[idx as usize] == *p || (positions[idx as usize] - *p).length() <= tol {
                                found = Some(idx);
                                break 'outer;
                            }
                        }
                    }
                }
            }
            let idx = match found {
                Some(i) => i,
                None => {
                    let i = positions.len() as u32;
                    positions.push(*p);
                    map.insert(k, i);
                    i
                }
            };
            remap.push(idx);
        }
        let triangles =
            self.triangles.iter().map(|t| [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]]).collect();
        TriMesh { positions, triangles }.compacted()
    }

    /// Default weld tolerance for imported meshes, relative to model size.
    pub fn default_weld_tolerance(&self) -> f64 {
        (self.bounds().diagonal() * 1e-7).max(1e-7)
    }

    /// Remove vertices not referenced by any triangle.
    pub fn compacted(&self) -> TriMesh {
        let mut remap = vec![u32::MAX; self.positions.len()];
        let mut positions = Vec::new();
        let mut triangles = Vec::with_capacity(self.triangles.len());
        for t in &self.triangles {
            let mut nt = [0u32; 3];
            for k in 0..3 {
                let v = t[k] as usize;
                if remap[v] == u32::MAX {
                    remap[v] = positions.len() as u32;
                    positions.push(self.positions[v]);
                }
                nt[k] = remap[v];
            }
            triangles.push(nt);
        }
        TriMesh { positions, triangles }
    }

    /// Convert to a `Manifold` solid. Fails unless the mesh is a closed
    /// 2-manifold.
    pub fn to_manifold(&self) -> Result<Manifold, SolidError> {
        if self.triangles.is_empty() {
            return Err(SolidError::Empty);
        }
        let m = Manifold::from_mesh_gl64(&self.to_mesh_gl64());
        match m.status() {
            ManifoldError::NoError if !m.is_empty() => Ok(m),
            ManifoldError::NoError => Err(SolidError::Empty),
            e => Err(SolidError::NotManifold(e)),
        }
    }

    pub fn to_mesh_gl64(&self) -> MeshGL64 {
        let mut mesh = MeshGL64 { num_prop: 3, ..Default::default() };
        mesh.vert_properties.reserve(self.positions.len() * 3);
        for p in &self.positions {
            mesh.vert_properties.extend_from_slice(&[p.x, p.y, p.z]);
        }
        mesh.tri_verts.reserve(self.triangles.len() * 3);
        for t in &self.triangles {
            mesh.tri_verts.extend_from_slice(&[t[0] as u64, t[1] as u64, t[2] as u64]);
        }
        mesh
    }

    pub fn from_manifold(m: &Manifold) -> TriMesh {
        let gl = m.get_mesh_gl64(-1);
        let np = (gl.num_prop as usize).max(3);
        let positions = gl.vert_properties.chunks_exact(np).map(|c| DVec3::new(c[0], c[1], c[2])).collect();
        let triangles = gl.tri_verts.chunks_exact(3).map(|c| [c[0] as u32, c[1] as u32, c[2] as u32]).collect();
        TriMesh { positions, triangles }
    }
}

/// What [`repair`] changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepairReport {
    pub merged_vertices: usize,
    pub removed_degenerate: usize,
    pub removed_duplicates: usize,
    pub flipped_triangles: usize,
    pub holes_filled: usize,
    pub inverted: bool,
    /// The exact pipeline still rejected the mesh and the robust winding
    /// number rebuild from `manifold-rust` was used.
    pub used_robust_rebuild: bool,
}

impl RepairReport {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.merged_vertices > 0 {
            parts.push(format!("merged {} duplicate vertices", self.merged_vertices));
        }
        if self.removed_degenerate > 0 {
            parts.push(format!("removed {} degenerate triangles", self.removed_degenerate));
        }
        if self.removed_duplicates > 0 {
            parts.push(format!("removed {} duplicate triangles", self.removed_duplicates));
        }
        if self.flipped_triangles > 0 {
            parts.push(format!("re-oriented {} triangles", self.flipped_triangles));
        }
        if self.holes_filled > 0 {
            parts.push(format!("filled {} holes", self.holes_filled));
        }
        if self.inverted {
            parts.push("turned the mesh right side out".into());
        }
        if self.used_robust_rebuild {
            parts.push("rebuilt the solid from winding numbers".into());
        }
        if parts.is_empty() {
            "nothing to repair".into()
        } else {
            parts.join(", ")
        }
    }
}

/// Basic mesh repair: weld, drop degenerate and duplicate faces, make the
/// orientation consistent, close holes by fan triangulation and turn the
/// result outward. Falls back to a robust winding-number rebuild if the
/// result is still not accepted as a manifold.
pub fn repair(mesh: &TriMesh) -> (TriMesh, RepairReport) {
    let mut report = RepairReport::default();
    let tol = mesh.default_weld_tolerance();
    let welded = mesh.welded(tol);
    report.merged_vertices = mesh.compacted().positions.len().saturating_sub(welded.positions.len());

    // Degenerate and duplicate triangles.
    let diag = welded.bounds().diagonal().max(1e-9);
    let area_eps = (diag * 1e-10).powi(2);
    let mut seen: HashMap<[u32; 3], usize> = HashMap::new();
    let mut keep: Vec<Option<[u32; 3]>> = Vec::with_capacity(welded.triangles.len());
    for (i, t) in welded.triangles.iter().enumerate() {
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] || welded.tri_cross(i).length_squared() <= area_eps {
            report.removed_degenerate += 1;
            keep.push(None);
            continue;
        }
        let mut key = *t;
        key.sort_unstable();
        if let Some(&j) = seen.get(&key) {
            // A duplicate face. Opposite-facing pairs are zero-thickness
            // walls: drop both. Same-facing copies: drop the copy.
            report.removed_duplicates += 1;
            if same_orientation(&keep[j].unwrap_or(*t), t) {
                keep.push(None);
            } else {
                keep[j] = None;
                keep.push(None);
                report.removed_duplicates += 1;
                seen.remove(&key);
            }
            continue;
        }
        seen.insert(key, i);
        keep.push(Some(*t));
    }
    let mut m = TriMesh { positions: welded.positions, triangles: keep.into_iter().flatten().collect() }.compacted();

    report.flipped_triangles = orient_consistently(&mut m);
    report.holes_filled = fill_holes(&mut m);
    if m.signed_volume() < 0.0 {
        for t in &mut m.triangles {
            t.swap(1, 2);
        }
        report.inverted = true;
    }

    if m.to_manifold().is_err() && !m.is_empty() {
        let robust = Manifold::from_mesh_gl64_robust(&m.to_mesh_gl64());
        if robust.status() == ManifoldError::NoError && !robust.is_empty() {
            let rebuilt = robust.rebuild_solid(manifold_rust::types::WindingRule::Nonzero);
            if rebuilt.status() == ManifoldError::NoError && !rebuilt.is_empty() {
                let candidate = TriMesh::from_manifold(&rebuilt);
                if candidate.to_manifold().is_ok() {
                    m = candidate;
                    report.used_robust_rebuild = true;
                }
            }
        }
    }
    (m, report)
}

fn same_orientation(a: &[u32; 3], b: &[u32; 3]) -> bool {
    let rot = |t: &[u32; 3]| {
        let k = (0..3).min_by_key(|&k| t[k]).unwrap();
        [t[k], t[(k + 1) % 3], t[(k + 2) % 3]]
    };
    rot(a) == rot(b)
}

/// Flip triangles so that neighbours across manifold edges agree. Returns the
/// number of flipped triangles. Each shell is oriented so the majority of its
/// faces keeps its original winding.
fn orient_consistently(m: &mut TriMesh) -> usize {
    let n = m.triangles.len();
    let mut edge_tris: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, t) in m.triangles.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            edge_tris.entry((a.min(b), a.max(b))).or_default().push(i);
        }
    }
    let mut flip = vec![false; n];
    let mut visited = vec![false; n];
    let mut total = 0;
    for start in 0..n {
        if visited[start] {
            continue;
        }
        let mut component = vec![start];
        let mut stack = vec![start];
        visited[start] = true;
        while let Some(i) = stack.pop() {
            let t = oriented(&m.triangles[i], flip[i]);
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let tris = &edge_tris[&(a.min(b), a.max(b))];
                if tris.len() != 2 {
                    continue;
                }
                let j = if tris[0] == i { tris[1] } else { tris[0] };
                if visited[j] {
                    continue;
                }
                // Neighbour must traverse the edge as b -> a.
                let u = &m.triangles[j];
                let has_ab = (0..3).any(|q| u[q] == a && u[(q + 1) % 3] == b);
                flip[j] = has_ab;
                visited[j] = true;
                component.push(j);
                stack.push(j);
            }
        }
        let flipped = component.iter().filter(|&&i| flip[i]).count();
        if flipped * 2 > component.len() {
            for &i in &component {
                flip[i] = !flip[i];
            }
        }
        total += component.iter().filter(|&&i| flip[i]).count();
    }
    for (i, t) in m.triangles.iter_mut().enumerate() {
        if flip[i] {
            t.swap(1, 2);
        }
    }
    total
}

fn oriented(t: &[u32; 3], flip: bool) -> [u32; 3] {
    if flip {
        [t[0], t[2], t[1]]
    } else {
        *t
    }
}

/// Close boundary loops by adding a centre vertex and a triangle fan.
/// Returns the number of holes filled.
fn fill_holes(m: &mut TriMesh) -> usize {
    let mut count: HashMap<(u32, u32), i32> = HashMap::new();
    for t in &m.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a, b)).or_default() += 1;
        }
    }
    // A directed edge a->b with no b->a twin lies on a hole. The hole loop
    // runs the other way round (b->a) so the filling faces match.
    let mut next: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut boundary = 0;
    for &(a, b) in count.keys() {
        if !count.contains_key(&(b, a)) {
            next.entry(b).or_default().push(a);
            boundary += 1;
        }
    }
    if boundary == 0 {
        return 0;
    }
    let mut holes = 0;
    let mut new_tris = Vec::new();
    let starts: Vec<u32> = next.keys().copied().collect();
    for s in starts {
        while let Some(first) = next.get_mut(&s).and_then(|l| l.pop()) {
            let mut lp = vec![s];
            let mut cur = first;
            let mut ok = true;
            while cur != s {
                lp.push(cur);
                if lp.len() > m.positions.len() + 1 {
                    ok = false;
                    break;
                }
                match next.get_mut(&cur).and_then(|l| l.pop()) {
                    Some(n) => cur = n,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok || lp.len() < 3 {
                continue;
            }
            holes += 1;
            if lp.len() == 3 {
                new_tris.push([lp[0], lp[1], lp[2]]);
                continue;
            }
            let c = lp.iter().map(|&v| m.positions[v as usize]).sum::<DVec3>() / lp.len() as f64;
            let ci = m.positions.len() as u32;
            m.positions.push(c);
            for k in 0..lp.len() {
                new_tris.push([lp[k], lp[(k + 1) % lp.len()], ci]);
            }
        }
    }
    m.triangles.extend(new_tris);
    holes
}

struct DisjointSet {
    parent: Vec<usize>,
}

impl DisjointSet {
    fn new(n: usize) -> Self {
        DisjointSet { parent: (0..n).collect() }
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

/// Simple primitive builders used by tests and fixtures.
pub mod primitives {
    use super::*;

    /// Axis-aligned box from `min` to `max`.
    pub fn cuboid(min: DVec3, max: DVec3) -> TriMesh {
        let p = |x: bool, y: bool, z: bool| {
            DVec3::new(if x { max.x } else { min.x }, if y { max.y } else { min.y }, if z { max.z } else { min.z })
        };
        let positions = vec![
            p(false, false, false),
            p(true, false, false),
            p(true, true, false),
            p(false, true, false),
            p(false, false, true),
            p(true, false, true),
            p(true, true, true),
            p(false, true, true),
        ];
        let triangles = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        TriMesh { positions, triangles }
    }

    /// UV sphere.
    pub fn uv_sphere(center: DVec3, r: f64, segments: u32, rings: u32) -> TriMesh {
        let mut positions = vec![center + DVec3::Z * r];
        for i in 1..rings {
            let phi = std::f64::consts::PI * i as f64 / rings as f64;
            for j in 0..segments {
                let th = std::f64::consts::TAU * j as f64 / segments as f64;
                positions.push(center + DVec3::new(phi.sin() * th.cos(), phi.sin() * th.sin(), phi.cos()) * r);
            }
        }
        positions.push(center - DVec3::Z * r);
        let s = segments;
        let bottom = positions.len() as u32 - 1;
        let mut triangles = Vec::new();
        for j in 0..s {
            triangles.push([0, 1 + j, 1 + (j + 1) % s]);
        }
        for i in 0..rings - 2 {
            let a = 1 + i * s;
            let b = a + s;
            for j in 0..s {
                let j2 = (j + 1) % s;
                triangles.push([a + j, b + j, b + j2]);
                triangles.push([a + j, b + j2, a + j2]);
            }
        }
        let last = 1 + (rings - 2) * s;
        for j in 0..s {
            triangles.push([bottom, last + (j + 1) % s, last + j]);
        }
        TriMesh { positions, triangles }
    }
}

#[cfg(test)]
mod tests {
    use super::primitives::*;
    use super::*;

    #[test]
    fn cube_volume_and_topology() {
        let m = cuboid(DVec3::ZERO, DVec3::new(10.0, 20.0, 30.0));
        assert!((m.signed_volume() - 6000.0).abs() < 1e-9);
        let t = m.topology();
        assert!(t.is_watertight(), "{t:?}");
        assert_eq!(t.shells, 1);
        let s = m.to_manifold().unwrap();
        assert!((s.volume() - 6000.0).abs() < 1e-6);
    }

    #[test]
    fn sphere_is_watertight() {
        let m = uv_sphere(DVec3::ZERO, 10.0, 32, 16);
        assert!(m.topology().is_watertight());
        assert!(m.to_manifold().is_ok());
    }

    #[test]
    fn repair_fixes_holes_flips_and_soup() {
        let cube = cuboid(DVec3::ZERO, DVec3::splat(10.0));
        // Triangle soup (unwelded) with one face removed and one flipped.
        let mut soup = TriMesh::default();
        for (i, t) in cube.triangles.iter().enumerate() {
            if i == 2 {
                continue;
            }
            let base = soup.positions.len() as u32;
            for &v in t {
                soup.positions.push(cube.positions[v as usize]);
            }
            if i == 5 {
                soup.triangles.push([base, base + 2, base + 1]);
            } else {
                soup.triangles.push([base, base + 1, base + 2]);
            }
        }
        assert!(!soup.topology().is_watertight());
        let (fixed, report) = repair(&soup);
        assert!(fixed.topology().is_watertight(), "{:?}", fixed.topology());
        assert!(report.holes_filled >= 1);
        assert!(report.flipped_triangles >= 1);
        assert!((fixed.signed_volume() - 1000.0).abs() < 1e-6);
        assert!(fixed.to_manifold().is_ok());
    }

    #[test]
    fn repair_turns_inside_out_mesh() {
        let mut cube = cuboid(DVec3::ZERO, DVec3::splat(10.0));
        for t in &mut cube.triangles {
            t.swap(1, 2);
        }
        let (fixed, report) = repair(&cube);
        assert!(report.inverted);
        assert!(fixed.signed_volume() > 0.0);
    }
}

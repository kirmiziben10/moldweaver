//! Plate layout: rotate printed parts into print orientation, pack them on
//! the build plate and write the 3MF.

use anyhow::Result;
use glam::{DMat4, DQuat, DVec2, DVec3};
use serde::Serialize;

use crate::io::threemf::{write_3mf, ExportObject};
use crate::mesh::TriMesh;
use crate::molds::Part;
use crate::printer::PrinterProfile;

/// Distance between parts and from the plate edge, mm.
pub const SPACING: f64 = 5.0;

/// A part ready for printing.
#[derive(Clone, Debug)]
pub struct PlacedPart {
    pub name: String,
    /// Mesh in print orientation with its bounding box minimum at the origin.
    pub mesh: TriMesh,
    /// Where the part sits on the plate.
    pub translation: DVec3,
    pub size: DVec3,
    /// Fits on the plate at all (footprint and height).
    pub fits: bool,
    /// Placed on the first plate (false = parked next to it).
    pub on_plate: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub parts: Vec<PlacedPart>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LayoutSummary {
    pub name: String,
    pub size_mm: [f64; 3],
    pub position_mm: [f64; 3],
    pub fits: bool,
    pub on_plate: bool,
}

impl Layout {
    pub fn summary(&self) -> Vec<LayoutSummary> {
        self.parts
            .iter()
            .map(|p| LayoutSummary {
                name: p.name.clone(),
                size_mm: p.size.to_array(),
                position_mm: p.translation.to_array(),
                fits: p.fits,
                on_plate: p.on_plate,
            })
            .collect()
    }
}

/// Oriented part with footprint, before packing.
struct Item {
    name: String,
    mesh: TriMesh,
    size: DVec3,
    fits: bool,
}

fn orient(part: &Part, printer: &PrinterProfile) -> Item {
    let m = match &part.print_mesh {
        Some(m) => m.clone(),
        None => part.mesh.transformed(&DMat4::from_quat(part.print_rotation)),
    };
    let mut b = m.bounds();
    let mut mesh = m.translated(-b.min);
    let fits_xy = |s: DVec3| s.x <= printer.bed_x - 2.0 * SPACING && s.y <= printer.bed_y - 2.0 * SPACING;
    // Long side along X unless only the other way fits.
    let size = b.size();
    let rotated_size = DVec3::new(size.y, size.x, size.z);
    let want_rotate = (size.y > size.x && fits_xy(rotated_size)) || (!fits_xy(size) && fits_xy(rotated_size));
    if want_rotate {
        let r = DMat4::from_quat(DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2));
        let m2 = mesh.transformed(&r);
        b = m2.bounds();
        mesh = m2.translated(-b.min);
    }
    let size = mesh.bounds().size();
    Item { name: part.name.clone(), mesh, size, fits: fits_xy(size) && size.z <= printer.max_z }
}

/// Per item: position and whether it is rotated, or `None` if it did not fit.
type Packing = Vec<Option<(DVec2, bool)>>;

/// MaxRects packer (best short side fit, 90° rotation allowed) over a
/// `w` x `d` area. Returns, per input size, its position and whether it
/// was rotated, or `None` if it did not fit.
fn maxrects_pack(sizes: &[DVec2], w: f64, d: f64) -> Packing {
    #[derive(Clone, Copy)]
    struct R {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    let mut free = vec![R { x: 0.0, y: 0.0, w: w + SPACING, h: d + SPACING }];
    let mut out = vec![None; sizes.len()];
    for (i, s) in sizes.iter().enumerate() {
        // Pad by the spacing; the extra row/column at the far edge is
        // accounted for by the enlarged free area above.
        let cands = [(s.x + SPACING, s.y + SPACING, false), (s.y + SPACING, s.x + SPACING, true)];
        let mut best: Option<(f64, f64, R, bool)> = None;
        for f in &free {
            for &(cw, ch, rot) in &cands {
                if cw <= f.w + 1e-9 && ch <= f.h + 1e-9 {
                    let short = (f.w - cw).min(f.h - ch);
                    let long = (f.w - cw).max(f.h - ch);
                    if best.is_none_or(|b| (short, long) < (b.0, b.1)) {
                        best = Some((short, long, R { x: f.x, y: f.y, w: cw, h: ch }, rot));
                    }
                }
            }
        }
        let Some((_, _, used, rot)) = best else { continue };
        out[i] = Some((DVec2::new(used.x, used.y), rot));
        // Split every free rectangle that overlaps the used one.
        let mut next = Vec::new();
        for f in &free {
            let overlap = used.x < f.x + f.w && used.x + used.w > f.x && used.y < f.y + f.h && used.y + used.h > f.y;
            if !overlap {
                next.push(*f);
                continue;
            }
            if used.x > f.x {
                next.push(R { x: f.x, y: f.y, w: used.x - f.x, h: f.h });
            }
            if used.x + used.w < f.x + f.w {
                next.push(R { x: used.x + used.w, y: f.y, w: f.x + f.w - used.x - used.w, h: f.h });
            }
            if used.y > f.y {
                next.push(R { x: f.x, y: f.y, w: f.w, h: used.y - f.y });
            }
            if used.y + used.h < f.y + f.h {
                next.push(R { x: f.x, y: used.y + used.h, w: f.w, h: f.y + f.h - used.y - used.h });
            }
        }
        // Drop rectangles contained in others.
        let contained = |a: &R, b: &R| a.x >= b.x && a.y >= b.y && a.x + a.w <= b.x + b.w && a.y + a.h <= b.y + b.h;
        let mut pruned: Vec<R> = Vec::new();
        for (k, a) in next.iter().enumerate() {
            let dup = next.iter().enumerate().any(|(j, b)| j != k && contained(a, b) && (!contained(b, a) || j < k));
            if !dup {
                pruned.push(*a);
            }
        }
        free = pruned;
    }
    out
}

/// Arrange the printed parts on the plate of `printer`.
pub fn arrange(parts: &[&Part], printer: &PrinterProfile) -> Layout {
    let mut items: Vec<Item> = parts.iter().map(|p| orient(p, printer)).collect();
    let usable = DVec2::new(printer.bed_x - 2.0 * SPACING, printer.bed_y - 2.0 * SPACING);
    // Try a few orderings and keep the one that puts the most area on the plate.
    let n = items.len();
    let area = |i: usize| items[i].size.x * items[i].size.y;
    let mut orders: Vec<Vec<usize>> = Vec::new();
    for key in 0..3 {
        let mut o: Vec<usize> = (0..n).collect();
        o.sort_by(|&a, &b| match key {
            0 => area(b).total_cmp(&area(a)),
            1 => items[b].size.x.max(items[b].size.y).total_cmp(&items[a].size.x.max(items[a].size.y)),
            _ => items[b].size.y.total_cmp(&items[a].size.y),
        });
        orders.push(o);
    }
    let mut best: Option<(f64, Vec<usize>, Packing)> = None;
    for order in orders {
        let sizes: Vec<DVec2> = order
            .iter()
            .map(|&i| if items[i].fits { items[i].size.truncate() } else { DVec2::splat(f64::INFINITY) })
            .collect();
        let packed = maxrects_pack(&sizes, usable.x, usable.y);
        let placed_area: f64 = order.iter().zip(&packed).filter(|(_, p)| p.is_some()).map(|(&i, _)| area(i)).sum();
        if best.as_ref().is_none_or(|b| placed_area > b.0 + 1e-9) {
            best = Some((placed_area, order, packed));
        }
    }
    let (_, order, packed) = best.unwrap_or_default();
    // Apply rotations chosen by the packer.
    for (k, &i) in order.iter().enumerate() {
        if let Some((_, true)) = packed[k] {
            let r = DMat4::from_quat(DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2));
            let m = items[i].mesh.transformed(&r);
            let b = m.bounds();
            items[i].mesh = m.translated(-b.min);
            items[i].size = items[i].mesh.bounds().size();
        }
    }
    let sizes: Vec<DVec2> = order.iter().map(|&i| items[i].size.truncate()).collect();
    let packed: Vec<Option<DVec2>> = packed.iter().map(|p| p.map(|(pos, _)| pos)).collect();

    // Centre the packed group on the plate.
    let mut lo = DVec2::splat(f64::INFINITY);
    let mut hi = DVec2::splat(f64::NEG_INFINITY);
    for (k, pos) in packed.iter().enumerate() {
        if let Some(p) = pos {
            lo = lo.min(*p);
            hi = hi.max(*p + sizes[k]);
        }
    }
    let shift =
        if lo.x.is_finite() { DVec2::new(printer.bed_x, printer.bed_y) * 0.5 - (lo + hi) * 0.5 } else { DVec2::ZERO };

    let mut layout = Layout::default();
    let mut placed: Vec<Option<PlacedPart>> = (0..items.len()).map(|_| None).collect();
    // Parts that do not fit go in a row to the right of the plate.
    let mut park_x = printer.bed_x + 20.0;
    for (k, &i) in order.iter().enumerate() {
        let item = std::mem::replace(
            &mut items[i],
            Item { name: String::new(), mesh: TriMesh::default(), size: DVec3::ZERO, fits: false },
        );
        let (translation, on_plate) = match packed[k] {
            Some(p) if item.fits => ((p + shift).extend(0.0), true),
            _ => {
                let t = DVec3::new(park_x, 0.0, 0.0);
                park_x += item.size.x + 20.0;
                (t, false)
            }
        };
        if !item.fits {
            layout.warnings.push(format!(
                "{} ({:.0} x {:.0} x {:.0} mm) does not fit the {} build volume ({:.0} x {:.0} x {:.0} mm).",
                item.name,
                item.size.x,
                item.size.y,
                item.size.z,
                printer.name,
                printer.bed_x,
                printer.bed_y,
                printer.max_z
            ));
        } else if !on_plate {
            layout.warnings.push(format!(
                "{} did not fit on the plate together with the other parts; it is placed next to the plate (move it to a second plate in the slicer).",
                item.name
            ));
        }
        placed[i] = Some(PlacedPart {
            name: item.name,
            size: item.size,
            mesh: item.mesh,
            translation,
            fits: item.fits,
            on_plate,
        });
    }
    layout.parts = placed.into_iter().flatten().collect();
    layout
}

/// Write the arranged parts as a 3MF package.
pub fn export_3mf(layout: &Layout, title: &str) -> Result<Vec<u8>> {
    let objects: Vec<ExportObject> = layout
        .parts
        .iter()
        .map(|p| ExportObject { name: &p.name, mesh: &p.mesh, translation: p.translation })
        .collect();
    write_3mf(&objects, title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::primitives::cuboid;
    use crate::molds::PartRole;

    fn part(name: &str, size: DVec3) -> Part {
        Part {
            name: name.into(),
            role: PartRole::Printed,
            mesh: cuboid(DVec3::splat(-3.0), size - 3.0),
            volume_mm3: size.x * size.y * size.z,
            explode: DVec3::ZERO,
            print_rotation: DQuat::IDENTITY,
            note: String::new(),
            print_mesh: None,
        }
    }

    #[test]
    fn packs_without_overlap_inside_plate() {
        let printer = PrinterProfile { name: "t".into(), bed_x: 220.0, bed_y: 220.0, max_z: 250.0 };
        let parts: Vec<Part> =
            (0..7).map(|i| part(&format!("p{i}"), DVec3::new(60.0 + i as f64 * 3.0, 40.0, 20.0))).collect();
        let refs: Vec<&Part> = parts.iter().collect();
        let l = arrange(&refs, &printer);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        for (i, a) in l.parts.iter().enumerate() {
            let (a0, a1) = (a.translation, a.translation + a.size);
            assert!(a0.x >= 0.0 && a0.y >= 0.0 && a1.x <= 220.0 && a1.y <= 220.0 && a0.z == 0.0);
            for b in &l.parts[i + 1..] {
                let (b0, b1) = (b.translation, b.translation + b.size);
                let overlap = a0.x < b1.x && b0.x < a1.x && a0.y < b1.y && b0.y < a1.y;
                assert!(!overlap, "{} overlaps {}", a.name, b.name);
            }
        }
    }

    #[test]
    fn warns_about_oversized_parts() {
        let printer = PrinterProfile { name: "t".into(), bed_x: 100.0, bed_y: 100.0, max_z: 50.0 };
        let parts = [part("big", DVec3::new(150.0, 20.0, 10.0)), part("tall", DVec3::new(20.0, 20.0, 80.0))];
        let refs: Vec<&Part> = parts.iter().collect();
        let l = arrange(&refs, &printer);
        assert_eq!(l.warnings.len(), 2);
        assert!(l.parts.iter().all(|p| !p.fits && !p.on_plate));
    }
}

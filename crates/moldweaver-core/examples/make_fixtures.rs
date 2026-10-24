//! Regenerates the test fixture models in `tests/fixtures/`:
//! `cargo run -p moldweaver-core --example make_fixtures`

use moldweaver_core::glam::DVec3;
use moldweaver_core::io::stl::write_binary;
use moldweaver_core::manifold_rust::linalg::Vec3;
use moldweaver_core::manifold_rust::manifold::Manifold;
use moldweaver_core::mesh::TriMesh;
use moldweaver_core::solid::{self, union_all};

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    let cube = solid::cuboid(DVec3::ZERO, DVec3::splat(30.0));
    let sphere = Manifold::sphere(20.0, 48);
    for (name, m) in [("cube.stl", cube), ("sphere.stl", sphere), ("lowpoly_dog.stl", dog())] {
        let mesh = TriMesh::from_manifold(&m);
        std::fs::write(dir.join(name), write_binary(&mesh, name)).unwrap();
        println!("{name}: {} triangles, {:.1} mm³", mesh.triangles.len(), m.volume());
    }
}

/// A standing low-poly dog: legs, neck, ears and tail give plenty of
/// undercuts for any parting plane.
fn dog() -> Manifold {
    let seg = 10;
    let ell = |c: [f64; 3], r: [f64; 3]| {
        Manifold::sphere(1.0, seg).scale(Vec3::new(r[0], r[1], r[2])).translate(Vec3::new(c[0], c[1], c[2]))
    };
    let leg = |x: f64, y: f64| Manifold::cylinder(24.0, 3.5, 3.0, 8).translate(Vec3::new(x, y, 0.0));
    let tail = Manifold::cylinder(18.0, 2.5, 1.0, 6).rotate(0.0, -50.0, 0.0).translate(Vec3::new(-22.0, 0.0, 30.0));
    let ear = |y: f64| {
        Manifold::cylinder(8.0, 3.0, 0.5, 6)
            .rotate(if y > 0.0 { -20.0 } else { 20.0 }, 0.0, 0.0)
            .translate(Vec3::new(26.0, y, 44.0))
    };
    union_all(vec![
        ell([0.0, 0.0, 28.0], [24.0, 10.0, 9.0]),
        ell([28.0, 0.0, 41.0], [8.0, 7.0, 7.0]),
        ell([36.0, 0.0, 38.0], [6.0, 3.5, 3.5]),
        Manifold::cylinder(14.0, 5.0, 4.0, 8).rotate(0.0, 40.0, 0.0).translate(Vec3::new(17.0, 0.0, 30.0)),
        leg(15.0, 6.0),
        leg(15.0, -6.0),
        leg(-15.0, 6.0),
        leg(-15.0, -6.0),
        tail,
        ear(4.0),
        ear(-4.0),
    ])
}

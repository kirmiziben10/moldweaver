//! Golden tests: run the real binary in batch mode on the fixture projects
//! and check part count, manifold status, volumes and the 3MF itself.
//!
//! Regenerate the golden files with `UPDATE_GOLDEN=1 cargo test --test cli_golden`.

use std::path::{Path, PathBuf};
use std::process::Command;

use moldweaver_core::io::threemf::validate_3mf;
use serde_json::{json, Value};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn out_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden");
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Run {
    report: Value,
    threemf: Vec<u8>,
}

fn run(name: &str, input: &str, extra: &[&str]) -> Run {
    let out = out_dir().join(format!("{name}.3mf"));
    let report = out_dir().join(format!("{name}.json"));
    let status = Command::new(env!("CARGO_BIN_EXE_moldweaver"))
        .arg("--batch")
        .arg(root().join("tests/fixtures").join(input))
        .arg("-o")
        .arg(&out)
        .arg("--report")
        .arg(&report)
        .args(extra)
        .env("XDG_CONFIG_HOME", out_dir().join("config")) // no user printer profiles
        .output()
        .expect("run moldweaver");
    assert!(
        status.status.success(),
        "moldweaver failed: {}\n{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    Run {
        report: serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap(),
        threemf: std::fs::read(out).unwrap(),
    }
}

/// The stable subset of the report that goes into the golden file.
fn golden_view(r: &Value) -> Value {
    let round = |v: &Value| (v.as_f64().unwrap() * 10.0).round() / 10.0;
    json!({
        "mold_type": r["mold_type"],
        "printer": r["printer"]["name"],
        "parts": r["parts"].as_array().unwrap().iter().map(|p| json!({
            "name": p["name"], "role": p["role"], "volume_ml": round(&p["volume_ml"]),
        })).collect::<Vec<_>>(),
        "silicone_ml": round(&r["volumes"]["silicone_ml"]),
        "pours_ml": r["volumes"]["pours"].as_array().unwrap().iter().map(|p| round(&p["silicone_ml"])).collect::<Vec<_>>(),
        "cavity_ml": round(&r["volumes"]["cavity_ml"]),
        "model_ml": round(&r["volumes"]["model_ml"]),
    })
}

fn compare_golden(name: &str, r: &Value) {
    let path = root().join("tests/golden").join(format!("{name}.json"));
    let actual = golden_view(r);
    if std::env::var_os("UPDATE_GOLDEN").is_some() || !path.exists() {
        std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
        return;
    }
    let expected: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(actual["mold_type"], expected["mold_type"]);
    assert_eq!(actual["printer"], expected["printer"]);
    let (a, e) = (actual["parts"].as_array().unwrap(), expected["parts"].as_array().unwrap());
    let names = |v: &Vec<Value>| v.iter().map(|p| p["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names(a), names(e), "{name}: part list changed");
    let close = |x: &Value, y: &Value, what: &str| {
        let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
        assert!((x - y).abs() <= 0.01 * y.abs() + 0.15, "{name}: {what} is {x}, golden {y}");
    };
    for (pa, pe) in a.iter().zip(e) {
        close(&pa["volume_ml"], &pe["volume_ml"], pe["name"].as_str().unwrap());
    }
    for key in ["silicone_ml", "cavity_ml", "model_ml"] {
        close(&actual[key], &expected[key], key);
    }
    for (x, y) in actual["pours_ml"].as_array().unwrap().iter().zip(expected["pours_ml"].as_array().unwrap()) {
        close(x, y, "pour");
    }
}

/// Every part watertight, the 3MF valid and every object on the plate.
fn check_common(name: &str, run: &Run) {
    let r = &run.report;
    for p in r["parts"].as_array().unwrap() {
        assert!(p["watertight"].as_bool().unwrap(), "{name}: {} is not watertight", p["name"]);
    }
    let printed: Vec<&Value> = r["parts"].as_array().unwrap().iter().filter(|p| p["role"] == "printed").collect();
    let objects = validate_3mf(&run.threemf).expect("3MF validates");
    assert_eq!(objects.len(), printed.len(), "{name}: one 3MF object per printed part");
    let (bx, by, bz) = (
        r["printer"]["bed_x"].as_f64().unwrap(),
        r["printer"]["bed_y"].as_f64().unwrap(),
        r["printer"]["max_z"].as_f64().unwrap(),
    );
    for (o, p) in objects.iter().zip(&printed) {
        assert_eq!(o.name, p["name"].as_str().unwrap());
        let welded = o.mesh.welded(1e-4);
        let topo = welded.topology();
        assert!(topo.is_watertight(), "{name}: 3MF object {} not watertight: {topo:?}", o.name);
        let solid = welded.to_manifold().unwrap_or_else(|e| panic!("{name}: {} not manifold: {e}", o.name));
        let ml = solid.volume() / 1000.0;
        let expected = p["volume_ml"].as_f64().unwrap();
        assert!((ml - expected).abs() < 0.002 * expected + 0.01, "{name}: {} volume {ml} vs {expected}", o.name);
        let b = o.bounds;
        assert!(b.min.z.abs() < 1e-3, "{name}: {} does not sit on the plate", o.name);
        assert!(
            b.min.x >= -1e-3 && b.min.y >= -1e-3 && b.max.x <= bx + 1e-3 && b.max.y <= by + 1e-3 && b.max.z <= bz,
            "{name}: {} is outside the {bx}x{by} plate: {b:?}",
            o.name
        );
    }
    // Objects do not overlap on the plate.
    for (i, a) in objects.iter().enumerate() {
        for b in &objects[i + 1..] {
            let overlap = a.bounds.min.x < b.bounds.max.x
                && b.bounds.min.x < a.bounds.max.x
                && a.bounds.min.y < b.bounds.max.y
                && b.bounds.min.y < a.bounds.max.y;
            assert!(!overlap, "{name}: {} overlaps {}", a.name, b.name);
        }
    }
    assert!(r["layout"].as_array().unwrap().iter().all(|l| l["on_plate"].as_bool().unwrap()));
    compare_golden(name, r);
}

fn count(r: &Value, prefix: &str) -> usize {
    r["parts"].as_array().unwrap().iter().filter(|p| p["name"].as_str().unwrap().starts_with(prefix)).count()
}

#[test]
fn cube_two_part() {
    let run = run("cube", "cube.json", &[]);
    check_common("cube", &run);
    let r = &run.report;
    // Box A, box B, board, base plate, sprue, dowels + 2 silicone previews.
    assert_eq!(r["parts"].as_array().unwrap().len(), 8);
    // 30 mm cube, 10 mm margin: cavity >= 50^3 (draft adds a little).
    let cavity = r["volumes"]["cavity_ml"].as_f64().unwrap();
    assert!(cavity > 125.0 && cavity < 130.0, "{cavity}");
    assert!((r["volumes"]["model_ml"].as_f64().unwrap() - 27.0).abs() < 1e-6);
}

#[test]
fn sphere_two_part_on_ender_with_clamps() {
    let run = run("sphere", "sphere.json", &[]);
    check_common("sphere", &run);
    let r = &run.report;
    assert_eq!(r["printer"]["name"], "Creality Ender 3 V3 SE");
    assert_eq!(count(r, "Clamp"), 8);
    assert!(r["volumes"]["cost"].as_f64().unwrap() > 0.0);
    assert_eq!(r["volumes"]["pours"].as_array().unwrap().len(), 2);
    // Equator split: the two pours are nearly equal.
    let p: Vec<f64> =
        r["volumes"]["pours"].as_array().unwrap().iter().map(|p| p["silicone_ml"].as_f64().unwrap()).collect();
    assert!((p[0] - p[1]).abs() / p[0] < 0.05, "{p:?}");
}

#[test]
fn dog_two_part_with_undercuts() {
    let run = run("dog", "dog.json", &[]);
    check_common("dog", &run);
    let r = &run.report;
    assert_eq!(count(r, "Box half"), 2);
    assert!(r["undercut_fraction"].as_f64().unwrap() > 0.0);
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("undercut")));
}

#[test]
fn dog_suggested_plane() {
    let run = run("dog_suggested", "dog.json", &["--suggest", "--set", "keys=false"]);
    check_common("dog_suggested", &run);
}

#[test]
fn dog_open_tray_detachable() {
    let run = run("dog_tray", "dog_tray.json", &[]);
    check_common("dog_tray", &run);
    let r = &run.report;
    assert_eq!(count(r, "Tray walls"), 1);
    assert_eq!(count(r, "Tray base"), 1);
    assert_eq!(r["volumes"]["pours"].as_array().unwrap().len(), 1);
}

#[test]
fn model_without_project_and_bad_input() {
    let out = out_dir().join("plain.3mf");
    let ok = Command::new(env!("CARGO_BIN_EXE_moldweaver"))
        .args(["--batch"])
        .arg(root().join("tests/fixtures/cube.stl"))
        .arg("-o")
        .arg(&out)
        .args(["--mold", "open_tray", "--suggest"])
        .output()
        .unwrap();
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    assert!(validate_3mf(&std::fs::read(&out).unwrap()).is_ok());

    let bad = Command::new(env!("CARGO_BIN_EXE_moldweaver"))
        .args(["--batch"])
        .arg(root().join("tests/fixtures/cube.json"))
        .arg("-o")
        .arg(&out)
        .args(["--set", "margin=abc"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("margin"));
}

//! Headless batch mode: `moldweaver --batch project.json -o out.3mf`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{anyhow, bail, Context, Result};
use moldweaver_core::analysis::suggest_plane;
use moldweaver_core::molds::{self, ParamKind, ParamValue, PartRole};
use moldweaver_core::pipeline;
use moldweaver_core::printer::{self, PrinterProfile};
use moldweaver_core::project::{ModelSource, Project};
use serde::Serialize;

use crate::Args;

#[derive(Serialize)]
struct PartReport {
    name: String,
    role: String,
    volume_ml: f64,
    triangles: usize,
    watertight: bool,
}

#[derive(Serialize)]
struct BatchReport {
    project: String,
    mold_type: String,
    printer: PrinterProfile,
    plane: moldweaver_core::frame::PartingPlane,
    undercut_fraction: f64,
    parts: Vec<PartReport>,
    volumes: molds::VolumeReport,
    layout: Vec<moldweaver_core::layout::LayoutSummary>,
    warnings: Vec<String>,
}

/// Location of the user's printer profiles.
pub fn user_printers_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("moldweaver").join("printers.json")
}

pub fn find_printer(name: &str) -> Result<PrinterProfile> {
    let user = printer::load_user_profiles(&user_printers_path()).unwrap_or_default();
    let all = printer::all_profiles(&user);
    if name.is_empty() {
        return Ok(all[0].clone());
    }
    all.iter().find(|p| p.name.eq_ignore_ascii_case(name)).cloned().ok_or_else(|| {
        anyhow!(
            "unknown printer '{name}'. Available: {}",
            all.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
        )
    })
}

/// Parse `key=value` into a parameter value matching the mold type's spec.
fn parse_set(mold: &dyn molds::MoldType, kv: &str) -> Result<(String, ParamValue)> {
    let (k, v) = kv.split_once('=').ok_or_else(|| anyhow!("--set expects key=value, got '{kv}'"))?;
    let specs = mold.param_specs();
    let spec = specs
        .iter()
        .find(|s| s.key == k)
        .ok_or_else(|| anyhow!("unknown parameter '{k}' for {}. Use --list-params", mold.id()))?;
    let value = match &spec.kind {
        ParamKind::Float { .. } => ParamValue::Float(v.parse().with_context(|| format!("{k} needs a number"))?),
        ParamKind::Int { .. } => ParamValue::Int(v.parse().with_context(|| format!("{k} needs a whole number"))?),
        ParamKind::Bool => ParamValue::Bool(match v {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => bail!("{k} needs true or false"),
        }),
        ParamKind::Choice(opts) => {
            if !opts.iter().any(|(id, _)| *id == v) {
                bail!("{k} must be one of {}", opts.iter().map(|(id, _)| *id).collect::<Vec<_>>().join(", "));
            }
            ParamValue::Choice(v.to_string())
        }
    };
    Ok((k.to_string(), value))
}

pub fn list_params() {
    for m in molds::registry() {
        println!("{} ({})", m.id(), m.name());
        let mut group = "";
        for s in m.param_specs() {
            if s.group != group {
                group = s.group;
                println!("  [{group}]");
            }
            let range = match &s.kind {
                ParamKind::Float { min, max, unit, .. } => format!("{min}..{max} {unit}"),
                ParamKind::Int { min, max } => format!("{min}..{max}"),
                ParamKind::Bool => "true|false".into(),
                ParamKind::Choice(o) => o.iter().map(|(id, _)| *id).collect::<Vec<_>>().join("|"),
            };
            println!("    {:<18} {:<28} default {:?}", s.key, range, s.default);
        }
    }
}

pub fn run(args: &Args) -> Result<()> {
    let output = args.output.as_ref().ok_or_else(|| anyhow!("--batch needs -o <output.3mf>"))?;
    let (mut project, base_dir, title) = match (&args.batch, &args.model) {
        (Some(path), _) if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) => {
            let p = Project::load(path)?;
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            (p, dir, path.file_stem().unwrap_or_default().to_string_lossy().to_string())
        }
        (Some(path), _) | (None, Some(path)) => {
            // A model file instead of a project: use defaults.
            let p = Project::new(ModelSource {
                path: Some(std::fs::canonicalize(path)?),
                embedded: None,
                repair: args.repair,
            });
            (p, PathBuf::new(), path.file_stem().unwrap_or_default().to_string_lossy().to_string())
        }
        (None, None) => bail!("--batch needs a project file or a model"),
    };
    if let Some(m) = &args.mold {
        project.mold_type = m.clone();
    }
    if let Some(p) = &args.printer {
        project.printer = p.clone();
    }
    if args.repair {
        project.model.repair = true;
    }
    let mold = molds::find(&project.mold_type).ok_or_else(|| {
        anyhow!(
            "unknown mold type '{}'. Available: {}",
            project.mold_type,
            molds::registry().iter().map(|m| m.id()).collect::<Vec<_>>().join(", ")
        )
    })?;
    for kv in &args.set {
        let (k, v) = parse_set(mold.as_ref(), kv)?;
        project.params.set(&k, v);
    }
    let printer = find_printer(&project.printer)?;

    let mesh = project.load_mesh(&base_dir)?;
    let topo = mesh.topology();
    if !topo.is_watertight() && !project.model.repair {
        eprintln!(
            "warning: the model is not watertight ({} open edges, {} non-manifold edges); generation may fail. Use --repair.",
            topo.boundary_edges, topo.non_manifold_edges
        );
    }
    if args.suggest {
        let s = suggest_plane(&mesh, mold.plane_usage());
        eprintln!(
            "suggested plane: tilt X {:.1}°, tilt Y {:.1}°, spin {:.1}°, offset {:.2} mm ({:.1}% undercut)",
            s.plane.tilt_x_deg,
            s.plane.tilt_y_deg,
            s.plane.spin_deg,
            s.plane.offset_mm,
            s.undercut_fraction * 100.0
        );
        project.plane = s.plane;
    }

    let generated = pipeline::generate(&mesh, &project.plane, mold.as_ref(), &project.params, &AtomicBool::new(false))?;
    let (bytes, layout) = pipeline::build_3mf(&generated, &printer, &title)?;
    std::fs::write(output, &bytes).with_context(|| format!("writing {}", output.display()))?;

    if let Some(dir) = &args.export_stl {
        std::fs::create_dir_all(dir)?;
        for p in &layout.parts {
            let file = dir.join(format!("{}.stl", sanitize(&p.name)));
            std::fs::write(&file, moldweaver_core::io::stl::write_binary(&p.mesh, &p.name))?;
        }
    }

    let mut warnings = generated.output.warnings.clone();
    warnings.extend(layout.warnings.iter().cloned());
    let report = BatchReport {
        project: title,
        mold_type: mold.id().into(),
        printer: printer.clone(),
        plane: project.plane,
        undercut_fraction: generated.undercuts.fraction(),
        parts: generated
            .output
            .parts
            .iter()
            .map(|p| PartReport {
                name: p.name.clone(),
                role: match p.role {
                    PartRole::Printed => "printed".into(),
                    PartRole::Silicone(n) => format!("silicone-pour-{n}"),
                },
                volume_ml: p.volume_mm3 / 1000.0,
                triangles: p.mesh.triangles.len(),
                watertight: p.mesh.topology().is_watertight(),
            })
            .collect(),
        volumes: generated.output.report.clone(),
        layout: layout.summary(),
        warnings,
    };
    if let Some(path) = &args.report {
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
    }
    print_summary(&report, output);
    Ok(())
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

fn print_summary(r: &BatchReport, output: &Path) {
    println!("MoldWeaver: {} -> {}", r.mold_type, output.display());
    println!("Printer: {} ({} x {} x {} mm)", r.printer.name, r.printer.bed_x, r.printer.bed_y, r.printer.max_z);
    println!("Parts:");
    for p in &r.parts {
        println!(
            "  {:<26} {:>9.1} mL  {:>7} tris  {}  {}",
            p.name,
            p.volume_ml,
            p.triangles,
            if p.watertight { "watertight" } else { "NOT WATERTIGHT" },
            p.role
        );
    }
    let v = &r.volumes;
    println!(
        "Silicone: {:.1} mL = {:.0} g at {:.2} g/mL{}",
        v.silicone_ml,
        v.silicone_g,
        v.density_g_per_ml,
        v.cost.map(|c| format!(", cost {c:.2}")).unwrap_or_default()
    );
    for pour in &v.pours {
        println!("  {:<28} {:>8.1} mL  {:>7.0} g", pour.name, pour.silicone_ml, pour.silicone_g);
    }
    println!(
        "Cavity {:.1} mL, model {:.1} mL, spout+vents {:.1} mL, casting material per cast {:.1} mL",
        v.cavity_ml, v.model_ml, v.channels_ml, v.cast_ml
    );
    for w in &r.warnings {
        println!("warning: {w}");
    }
}

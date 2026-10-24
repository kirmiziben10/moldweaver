//! MoldWeaver: a free, open-source silicone mold box generator.

mod cli;
mod gui;

use std::path::PathBuf;

use clap::Parser;

/// Turn a 3D model into 3D-printable mold boxes for silicone casting.
#[derive(Parser, Debug, Default)]
#[command(name = "moldweaver", version, about)]
pub struct Args {
    /// Model (STL/OBJ/3MF) or project (.json) to open in the window.
    pub file: Option<PathBuf>,

    /// Generate without opening a window, from a project file (or a model
    /// file with default settings).
    #[arg(long, value_name = "PROJECT")]
    pub batch: Option<PathBuf>,

    /// Output 3MF for --batch.
    #[arg(short, long, value_name = "OUT.3MF")]
    pub output: Option<PathBuf>,

    /// Model file for --batch when no project is given.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<PathBuf>,

    /// Mold type id (overrides the project), e.g. two_part_box, open_tray.
    #[arg(long)]
    pub mold: Option<String>,

    /// Printer profile name (overrides the project).
    #[arg(long)]
    pub printer: Option<String>,

    /// Override a parameter, e.g. --set margin=12 (repeatable).
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,

    /// Use the automatically suggested parting plane.
    #[arg(long)]
    pub suggest: bool,

    /// Repair the model (weld, fill holes, fix normals) before generating.
    #[arg(long)]
    pub repair: bool,

    /// Write a JSON report (parts, volumes, layout, warnings).
    #[arg(long, value_name = "REPORT.JSON")]
    pub report: Option<PathBuf>,

    /// Also write each printed part as an STL into this directory.
    #[arg(long, value_name = "DIR")]
    pub export_stl: Option<PathBuf>,

    /// List mold types and their parameters, then exit.
    #[arg(long)]
    pub list_params: bool,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = Args::parse();
    if args.list_params {
        cli::list_params();
        return;
    }
    let result = if args.batch.is_some() || (args.model.is_some() && args.output.is_some()) {
        cli::run(&args)
    } else {
        gui::run(args.file.clone())
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

//! The MoldWeaver window.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, RichText, Sense, Stroke, Vec2};
use glam::{DMat4, DQuat, DVec3, Mat4, Vec2 as GVec2, Vec3, Vec4};
use moldweaver_core::analysis::{FaceClass, PlaneUsage, UndercutStats};
use moldweaver_core::frame::{MoldFrame, PartingPlane};
use moldweaver_core::io::ModelFormat;
use moldweaver_core::mesh::{Aabb, TopologyReport, TriMesh};
use moldweaver_core::molds::{self, MoldType, ParamValue, ParamValues, PartRole};
use moldweaver_core::pipeline::{self, Generated};
use moldweaver_core::printer::{self, PrinterProfile};
use moldweaver_core::project::{self, EmbeddedMesh, ModelSource, Project};

use super::camera::OrbitCamera;
use super::params_ui::{self, ParamAction};
use super::render::{DrawItem, GpuMesh, Renderer};
use super::scene;
use super::worker::{self, AnalyzeJob, FilePurpose, GenerateJob, Lane, Msg};
use crate::cli::user_printers_path;

/// Debounce before regenerating after the last change.
const GENERATE_DELAY: Duration = Duration::from_millis(300);
const ANALYZE_DELAY: Duration = Duration::from_millis(80);

/// Everything that undo/redo covers and that is saved in project files.
#[derive(Clone, Debug, PartialEq)]
struct Document {
    plane: PartingPlane,
    mold_type: String,
    params: ParamValues,
    printer: String,
}

impl Document {
    /// The part that affects generated geometry.
    fn geometry_key(&self) -> (PartingPlane, String, ParamValues) {
        (self.plane, self.mold_type.clone(), self.params.clone())
    }
}

struct Model {
    mesh: Arc<TriMesh>,
    path: Option<PathBuf>,
    name: String,
    topology: TopologyReport,
    bounds: Aabb,
    volume_ml: f64,
    repaired: bool,
    version: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum GizmoHandle {
    Move,
    TiltU,
    TiltV,
}

struct GpuCache {
    model: Option<(u64, u64, Arc<GpuMesh>)>, // (model version, colouring version, mesh)
    parts: Vec<Arc<GpuMesh>>,
    parts_for: Option<u64>,
    plane: Option<Arc<GpuMesh>>,
    plane_outline: Option<Arc<GpuMesh>>,
    grid: Option<(f32, Arc<GpuMesh>)>,
}

pub struct MoldWeaverApp {
    doc: Document,
    committed: Document,
    undo: Vec<Document>,
    redo: Vec<Document>,
    last_seen: Document,
    last_change: Instant,

    model: Option<Model>,
    model_counter: u64,
    mold_types: Vec<Box<dyn MoldType>>,
    user_printers: Vec<PrinterProfile>,
    printers: Vec<PrinterProfile>,
    project_path: Option<PathBuf>,
    embed_model: bool,

    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    gen_lane: Lane<GenerateJob>,
    analyze_lane: Lane<AnalyzeJob>,
    next_id: u64,
    gen_submitted: Option<(u64, (PartingPlane, String, ParamValues))>,
    generating: bool,
    generated: Option<Arc<Generated>>,
    generated_id: u64,
    gen_error: Option<String>,
    gen_millis: u128,
    layout_for: Option<(u64, String)>,
    layout_warnings: Vec<String>,
    part_fits: HashMap<String, bool>,
    analysis_submitted: Option<(u64, PartingPlane, String, u64)>,
    classes: Option<(u64, Arc<Vec<FaceClass>>, UndercutStats)>,
    classes_version: u64,
    suggesting: bool,
    busy_task: Option<String>,

    renderer: Option<Renderer>,
    render_error: Option<String>,
    camera: OrbitCamera,
    exploded: bool,
    explode_t: f32,
    part_visible: HashMap<String, bool>,
    show_model: bool,
    show_undercuts: bool,
    show_plane: bool,
    show_markers: bool,
    gpu: GpuCache,
    drag: Option<GizmoHandle>,
    hover_handle: Option<GizmoHandle>,
    pick_spout: bool,

    status: String,
    messages: Vec<(String, bool, Instant)>, // (text, is_error, time)
    printer_editor: Option<PrinterEditor>,
    show_guide: bool,
    show_about: bool,
    pending_open: Option<PathBuf>,
}

struct PrinterEditor {
    name: String,
    x: f64,
    y: f64,
    z: f64,
    error: Option<String>,
}

impl MoldWeaverApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = channel();
        let user_printers = printer::load_user_profiles(&user_printers_path()).unwrap_or_default();
        let printers = printer::all_profiles(&user_printers);
        let mold_types = molds::registry();
        let mold_type = molds::DEFAULT_MOLD_TYPE.to_string();
        let params = ParamValues::defaults(&mold_types[0].param_specs());
        let doc = Document {
            plane: PartingPlane::default(),
            mold_type,
            params,
            printer: printers.first().map(|p| p.name.clone()).unwrap_or_default(),
        };
        let renderer = cc.wgpu_render_state.as_ref().map(Renderer::new);
        let render_error = renderer.is_none().then(|| "No wgpu render state; the 3D view is unavailable.".to_string());
        MoldWeaverApp {
            committed: doc.clone(),
            last_seen: doc.clone(),
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
            last_change: Instant::now(),
            model: None,
            model_counter: 0,
            mold_types,
            user_printers,
            printers,
            project_path: None,
            embed_model: false,
            gen_lane: worker::generate_lane(tx.clone(), ctx.clone()),
            analyze_lane: worker::analyze_lane(tx.clone(), ctx.clone()),
            tx,
            rx,
            next_id: 1,
            gen_submitted: None,
            generating: false,
            generated: None,
            generated_id: 0,
            gen_error: None,
            gen_millis: 0,
            layout_for: None,
            layout_warnings: Vec::new(),
            part_fits: HashMap::new(),
            analysis_submitted: None,
            classes: None,
            classes_version: 0,
            suggesting: false,
            busy_task: None,
            renderer,
            render_error,
            camera: OrbitCamera::default(),
            exploded: false,
            explode_t: 0.0,
            part_visible: HashMap::new(),
            show_model: true,
            show_undercuts: true,
            show_plane: true,
            show_markers: true,
            gpu: GpuCache {
                model: None,
                parts: Vec::new(),
                parts_for: None,
                plane: None,
                plane_outline: None,
                grid: None,
            },
            drag: None,
            hover_handle: None,
            pick_spout: false,
            status: "Open a model to begin (File ▸ Open model, or drop a file on the window).".into(),
            messages: Vec::new(),
            printer_editor: None,
            show_guide: false,
            show_about: false,
            pending_open: open,
        }
    }

    fn mold(&self) -> &dyn MoldType {
        self.mold_types
            .iter()
            .find(|m| m.id() == self.doc.mold_type)
            .map(|m| m.as_ref())
            .unwrap_or(self.mold_types[0].as_ref())
    }

    fn printer(&self) -> PrinterProfile {
        self.printers.iter().find(|p| p.name == self.doc.printer).cloned().unwrap_or_else(|| self.printers[0].clone())
    }

    fn notify(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            log::warn!("{text}");
        }
        self.status = text.clone();
        self.messages.push((text, error, Instant::now()));
        if self.messages.len() > 6 {
            self.messages.remove(0);
        }
    }

    // ------------------------------------------------------------------
    // Files

    fn pick_file(&self, ctx: &egui::Context, purpose: FilePurpose) {
        let start_dir = self.project_path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).or_else(|| {
            self.model.as_ref().and_then(|m| m.path.as_ref()).and_then(|p| p.parent().map(Path::to_path_buf))
        });
        let default_name = self.model.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| "mold".into());
        worker::spawn_task(&self.tx, ctx, move || {
            let mut d = rfd::FileDialog::new();
            if let Some(dir) = start_dir {
                d = d.set_directory(dir);
            }
            let path = match purpose {
                FilePurpose::OpenModel => d.add_filter("3D models", ModelFormat::EXTENSIONS).pick_file(),
                FilePurpose::OpenProject => d.add_filter("MoldWeaver project", &["json", "mwproj"]).pick_file(),
                FilePurpose::SaveProject => d
                    .add_filter("MoldWeaver project", &["json"])
                    .set_file_name(format!("{default_name}.moldweaver.json"))
                    .save_file(),
                FilePurpose::Export3mf => {
                    d.add_filter("3MF", &["3mf"]).set_file_name(format!("{default_name}-mold.3mf")).save_file()
                }
            };
            Msg::FilePicked { purpose, path }
        });
    }

    fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        let is_project = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json") || e.eq_ignore_ascii_case("mwproj"));
        self.busy_task = Some(format!("Loading {}…", path.display()));
        if is_project {
            worker::spawn_task(&self.tx, ctx, move || {
                let result = (|| {
                    let p = Project::load(&path)?;
                    let mesh = p.load_mesh(path.parent().unwrap_or(Path::new(".")))?;
                    anyhow::Ok(Box::new((p, mesh)))
                })()
                .map_err(|e| format!("{e:#}"));
                Msg::ProjectLoaded { path, result }
            });
        } else {
            worker::spawn_task(&self.tx, ctx, move || {
                let result = moldweaver_core::io::load_model(&path).map_err(|e| format!("{e:#}"));
                Msg::ModelLoaded { path, result }
            });
        }
    }

    fn set_model(&mut self, mesh: TriMesh, path: Option<PathBuf>, repaired: bool) {
        self.model_counter += 1;
        let topology = mesh.topology();
        let bounds = mesh.bounds();
        let name = path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "model".into());
        self.model = Some(Model {
            volume_ml: mesh.signed_volume().abs() / 1000.0,
            mesh: Arc::new(mesh),
            path,
            name,
            topology,
            bounds,
            repaired,
            version: self.model_counter,
        });
        self.generated = None;
        self.gen_error = None;
        self.gen_submitted = None;
        self.classes = None;
        self.analysis_submitted = None;
        self.part_visible.clear();
        self.gpu.parts_for = None;
        self.gpu.grid = None;
        self.fit_camera();
        self.last_change = Instant::now() - GENERATE_DELAY;
    }

    fn fit_camera(&mut self) {
        if let Some(m) = &self.model {
            let c = m.bounds.center().as_vec3();
            let r = (m.bounds.diagonal() as f32 * 0.5).max(1.0) * 1.6;
            self.camera.fit(c, r);
        }
    }

    fn save_project(&mut self, path: PathBuf) {
        let Some(model) = &self.model else {
            self.notify("Nothing to save: no model loaded.", true);
            return;
        };
        let base = path.parent().unwrap_or(Path::new("."));
        let embed = self.embed_model || model.path.is_none();
        let source = ModelSource {
            path: model.path.as_ref().map(|p| project::relative_to(p, base)),
            embedded: embed.then(|| EmbeddedMesh::encode(&model.mesh)),
            repair: model.repaired && !embed,
        };
        let p = Project {
            plane: self.doc.plane,
            mold_type: self.doc.mold_type.clone(),
            params: self.doc.params.clone(),
            printer: self.doc.printer.clone(),
            ..Project::new(source)
        };
        match p.save(&path) {
            Ok(()) => {
                self.notify(format!("Saved {}", path.display()), false);
                self.project_path = Some(path);
            }
            Err(e) => self.notify(format!("Could not save: {e:#}"), true),
        }
    }

    fn export(&mut self, ctx: &egui::Context, path: PathBuf) {
        let Some(g) = self.generated.clone() else {
            self.notify("Nothing to export yet: the mold has not been generated.", true);
            return;
        };
        let printer = self.printer();
        let title = self.model.as_ref().map(|m| m.name.clone()).unwrap_or_default();
        let path = if path.extension().is_none() { path.with_extension("3mf") } else { path };
        self.busy_task = Some("Exporting 3MF…".into());
        worker::spawn_task(&self.tx, ctx, move || {
            let result = (|| {
                let (bytes, layout) = pipeline::build_3mf(&g, &printer, &title)?;
                std::fs::write(&path, bytes)?;
                anyhow::Ok(layout.warnings)
            })()
            .map_err(|e| format!("{e:#}"));
            Msg::Exported { path, result }
        });
    }

    // ------------------------------------------------------------------
    // Messages from workers

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Generated { id, result, millis } => {
                    if self.gen_submitted.as_ref().map(|s| s.0) != Some(id) {
                        continue; // stale
                    }
                    self.generating = false;
                    match result {
                        Ok(g) => {
                            self.generated = Some(g);
                            self.generated_id = id;
                            self.gen_error = None;
                            self.gen_millis = millis;
                            self.status = format!("Mold generated in {millis} ms.");
                        }
                        Err(e) => {
                            self.gen_error = Some(e);
                        }
                    }
                }
                Msg::Analyzed { id, classes, stats } => {
                    if self.analysis_submitted.as_ref().map(|s| s.0) == Some(id) {
                        self.classes_version += 1;
                        self.classes = Some((id, classes, stats));
                    }
                }
                Msg::Suggested { mold_type, result } => {
                    self.suggesting = false;
                    if mold_type == self.doc.mold_type {
                        self.doc.plane = result.plane;
                        self.notify(
                            format!(
                                "Suggested plane applied: {:.1}% of the surface is undercut.",
                                result.undercut_fraction * 100.0
                            ),
                            false,
                        );
                    }
                }
                Msg::ModelLoaded { path, result } => {
                    self.busy_task = None;
                    match result {
                        Ok(mesh) => {
                            let tray = self.mold().plane_usage() == PlaneUsage::Base;
                            self.doc.plane = PartingPlane::default();
                            if tray {
                                self.doc.plane.offset_mm = PartingPlane::default().offset_range(&mesh).0;
                            }
                            self.project_path = None;
                            let watertight = mesh.topology().is_watertight();
                            self.set_model(mesh, Some(path.clone()), false);
                            self.notify(
                                if watertight {
                                    format!("Loaded {}", path.display())
                                } else {
                                    format!("Loaded {} — the mesh is not watertight; use Repair.", path.display())
                                },
                                !watertight,
                            );
                            self.undo.clear();
                            self.redo.clear();
                            self.committed = self.doc.clone();
                        }
                        Err(e) => self.notify(format!("Could not open {}: {e}", path.display()), true),
                    }
                }
                Msg::ProjectLoaded { path, result } => {
                    self.busy_task = None;
                    match result {
                        Ok(loaded) => {
                            let (p, mesh) = *loaded;
                            let model_path = p.model.path.as_ref().map(|mp| {
                                if mp.is_absolute() {
                                    mp.clone()
                                } else {
                                    path.parent().unwrap_or(Path::new(".")).join(mp)
                                }
                            });
                            self.embed_model = p.model.embedded.is_some();
                            self.set_model(mesh, model_path, p.model.repair);
                            self.doc = Document {
                                plane: p.plane,
                                mold_type: if molds::find(&p.mold_type).is_some() {
                                    p.mold_type.clone()
                                } else {
                                    molds::DEFAULT_MOLD_TYPE.into()
                                },
                                params: p.params.clone(),
                                printer: if self.printers.iter().any(|x| x.name == p.printer) {
                                    p.printer.clone()
                                } else {
                                    self.doc.printer.clone()
                                },
                            };
                            self.doc.params = self.doc.params.normalized(&self.mold().param_specs());
                            self.undo.clear();
                            self.redo.clear();
                            self.committed = self.doc.clone();
                            self.project_path = Some(path.clone());
                            self.notify(format!("Opened project {}", path.display()), false);
                        }
                        Err(e) => self.notify(format!("Could not open project {}: {e}", path.display()), true),
                    }
                }
                Msg::Repaired { mesh, report } => {
                    self.busy_task = None;
                    let path = self.model.as_ref().and_then(|m| m.path.clone());
                    let ok = mesh.topology().is_watertight();
                    self.set_model(mesh, path, true);
                    self.notify(
                        format!(
                            "Repair: {}.{}",
                            report.summary(),
                            if ok { " The mesh is now watertight." } else { " The mesh is still not watertight." }
                        ),
                        !ok,
                    );
                }
                Msg::Exported { path, result } => {
                    self.busy_task = None;
                    match result {
                        Ok(w) if w.is_empty() => self.notify(format!("Exported {}", path.display()), false),
                        Ok(w) => {
                            self.notify(format!("Exported {} with warnings: {}", path.display(), w.join(" ")), true)
                        }
                        Err(e) => self.notify(format!("Export failed: {e}"), true),
                    }
                }
                Msg::FilePicked { purpose, path } => {
                    let Some(path) = path else { continue };
                    match purpose {
                        FilePurpose::OpenModel | FilePurpose::OpenProject => self.open_path(ctx, path),
                        FilePurpose::SaveProject => self.save_project(path),
                        FilePurpose::Export3mf => self.export(ctx, path),
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Scheduling

    fn schedule(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self.doc != self.last_seen {
            self.last_seen = self.doc.clone();
            self.last_change = now;
        }
        let dragging = ctx.input(|i| i.pointer.any_down()) || self.drag.is_some();
        // Undo history: commit once the user lets go.
        if self.doc != self.committed && !dragging {
            self.undo.push(std::mem::replace(&mut self.committed, self.doc.clone()));
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        let Some(model) = &self.model else { return };
        let mesh = model.mesh.clone();
        let version = model.version;

        let key = self.doc.geometry_key();
        let submitted_matches = self.gen_submitted.as_ref().is_some_and(|(_, k)| *k == key);
        if !submitted_matches {
            let wait = GENERATE_DELAY.saturating_sub(now - self.last_change);
            if wait.is_zero() {
                let id = self.next_id;
                self.next_id += 1;
                let params = self.doc.params.normalized(&self.mold().param_specs());
                self.gen_lane.submit(GenerateJob {
                    id,
                    mesh: mesh.clone(),
                    plane: self.doc.plane,
                    mold_type: self.doc.mold_type.clone(),
                    params,
                });
                self.gen_submitted = Some((id, key));
                self.generating = true;
            } else {
                ctx.request_repaint_after(wait);
            }
        }

        if self.show_undercuts {
            let want = (self.doc.plane, self.doc.mold_type.clone(), version);
            let have = self.analysis_submitted.as_ref().map(|(_, p, m, v)| (*p, m.clone(), *v));
            if have.as_ref() != Some(&want) {
                let wait = ANALYZE_DELAY.saturating_sub(now - self.last_change);
                if wait.is_zero() {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.analyze_lane.submit(AnalyzeJob {
                        id,
                        mesh,
                        plane: self.doc.plane,
                        mold_type: self.doc.mold_type.clone(),
                    });
                    self.analysis_submitted = Some((id, want.0, want.1, want.2));
                } else {
                    ctx.request_repaint_after(wait);
                }
            }
        }

        // Plate fit check whenever the result or printer changes.
        if let Some(g) = &self.generated {
            let want = (self.generated_id, self.doc.printer.clone());
            if self.layout_for.as_ref() != Some(&want) {
                let layout = pipeline::plate_layout(g, &self.printer());
                self.part_fits = layout.parts.iter().map(|p| (p.name.clone(), p.fits && p.on_plate)).collect();
                self.layout_warnings = layout.warnings;
                self.layout_for = Some(want);
            }
        }
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.doc, prev));
            self.committed = self.doc.clone();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.doc, next));
            self.committed = self.doc.clone();
        }
    }

    fn suggest(&mut self, ctx: &egui::Context) {
        let Some(m) = &self.model else { return };
        let mesh = m.mesh.clone();
        let usage = self.mold().plane_usage();
        let mold_type = self.doc.mold_type.clone();
        self.suggesting = true;
        worker::spawn_task(&self.tx, ctx, move || Msg::Suggested {
            result: moldweaver_core::analysis::suggest_plane(&mesh, usage),
            mold_type,
        });
    }

    fn repair(&mut self, ctx: &egui::Context) {
        let Some(m) = &self.model else { return };
        let mesh = m.mesh.clone();
        self.busy_task = Some("Repairing mesh…".into());
        worker::spawn_task(&self.tx, ctx, move || {
            let (mesh, report) = moldweaver_core::mesh::repair(&mesh);
            Msg::Repaired { mesh, report }
        });
    }

    // ------------------------------------------------------------------
    // UI

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let sc = |m, k| KeyboardShortcut::new(m, k);
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z))) {
            self.redo();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::COMMAND, Key::Z))) {
            self.undo();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::COMMAND, Key::Y))) {
            self.redo();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::COMMAND, Key::O))) {
            self.pick_file(ctx, FilePurpose::OpenModel);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::COMMAND, Key::S))) {
            match self.project_path.clone() {
                Some(p) => self.save_project(p),
                None => self.pick_file(ctx, FilePurpose::SaveProject),
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::COMMAND, Key::E))) && self.generated.is_some() {
            self.pick_file(ctx, FilePurpose::Export3mf);
        }
        // Dropped files.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        if let Some(p) = dropped.into_iter().next() {
            self.open_path(ctx, p);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open model…   Ctrl+O").clicked() {
                    self.pick_file(&ctx, FilePurpose::OpenModel);
                }
                if ui.button("Open project…").clicked() {
                    self.pick_file(&ctx, FilePurpose::OpenProject);
                }
                ui.separator();
                let has_model = self.model.is_some();
                if ui.add_enabled(has_model, egui::Button::new("Save project   Ctrl+S")).clicked() {
                    match self.project_path.clone() {
                        Some(p) => self.save_project(p),
                        None => self.pick_file(&ctx, FilePurpose::SaveProject),
                    }
                }
                if ui.add_enabled(has_model, egui::Button::new("Save project as…")).clicked() {
                    self.pick_file(&ctx, FilePurpose::SaveProject);
                }
                ui.checkbox(&mut self.embed_model, "Embed model in project file")
                    .on_hover_text("Store the mesh inside the project instead of a link to the model file, so the project works on its own.");
                ui.separator();
                if ui
                    .add_enabled(self.generated.is_some(), egui::Button::new("Export 3MF…   Ctrl+E"))
                    .clicked()
                {
                    self.pick_file(&ctx, FilePurpose::Export3mf);
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo   Ctrl+Z")).clicked() {
                    self.undo();
                }
                if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo   Ctrl+Shift+Z")).clicked() {
                    self.redo();
                }
                ui.separator();
                if ui.button("Reset parameters to defaults").clicked() {
                    self.doc.params = ParamValues::defaults(&self.mold().param_specs());
                }
                if ui.button("Printers…").clicked() {
                    self.printer_editor = Some(PrinterEditor { name: String::new(), x: 220.0, y: 220.0, z: 250.0, error: None });
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.exploded, "Exploded view");
                ui.checkbox(&mut self.show_model, "Show model");
                ui.checkbox(&mut self.show_undercuts, "Colour undercuts");
                ui.checkbox(&mut self.show_plane, "Show parting plane");
                ui.checkbox(&mut self.show_markers, "Show spout, vents and keys");
                if ui.button("Frame model").clicked() {
                    self.fit_camera();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Quick guide").clicked() {
                    self.show_guide = true;
                }
                if ui.button("About MoldWeaver").clicked() {
                    self.show_about = true;
                }
            });
        });
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            // ---- Model
            ui.heading("Model");
            match &self.model {
                None => {
                    if ui.button("Open model…").clicked() {
                        self.pick_file(&ctx, FilePurpose::OpenModel);
                    }
                    ui.label(RichText::new("STL, OBJ or 3MF").weak());
                }
                Some(m) => {
                    let s = m.bounds.size();
                    ui.label(RichText::new(&m.name).strong());
                    ui.label(format!("{:.1} × {:.1} × {:.1} mm", s.x, s.y, s.z));
                    ui.label(format!("{} triangles, {:.1} mL", m.topology.triangles, m.volume_ml));
                    if m.topology.is_watertight() {
                        ui.label(RichText::new("✔ Watertight").color(Color32::from_rgb(110, 200, 120)));
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "⚠ Not watertight: {} open edges, {} non-manifold edges, {} flipped",
                                m.topology.boundary_edges, m.topology.non_manifold_edges, m.topology.inconsistent_edges
                            ))
                            .color(Color32::from_rgb(240, 170, 60)),
                        );
                        if ui
                            .button("Repair mesh")
                            .on_hover_text("Merges duplicate vertices, removes broken faces, fixes flipped faces and fills holes.")
                            .clicked()
                        {
                            self.repair(&ctx);
                        }
                    }
                }
            }
            ui.separator();

            // ---- Mold type
            ui.heading("Mold type");
            let current = self.doc.mold_type.clone();
            egui::ComboBox::from_id_salt("mold_type")
                .selected_text(self.mold().name())
                .width(ui.available_width() - 8.0)
                .show_ui(ui, |ui| {
                    for m in &self.mold_types {
                        ui.selectable_value(&mut self.doc.mold_type, m.id().to_string(), m.name())
                            .on_hover_text(m.description());
                    }
                });
            if self.doc.mold_type != current {
                self.doc.params = self.doc.params.normalized(&self.mold().param_specs());
                // A base plane belongs at the bottom of the model.
                if let Some(m) = &self.model {
                    if self.mold().plane_usage() == PlaneUsage::Base {
                        self.doc.plane.offset_mm = self.doc.plane.offset_range(&m.mesh).0;
                    } else {
                        self.doc.plane.offset_mm = 0.0;
                    }
                }
            }
            ui.label(RichText::new(self.mold().description()).weak().small());
            ui.separator();

            // ---- Plane
            ui.heading(if self.mold().plane_usage() == PlaneUsage::Base { "Base plane" } else { "Parting plane" });
            ui.horizontal(|ui| {
                let enabled = self.model.is_some() && !self.suggesting;
                if ui
                    .add_enabled(enabled, egui::Button::new(if self.suggesting { "Searching…" } else { "Suggest plane" }))
                    .on_hover_text("Search orientations and heights for the plane with the least undercut area, keeping the parting line on the widest part of the model.")
                    .clicked()
                {
                    self.suggest(&ctx);
                }
                if ui.button("Reset").clicked() {
                    let offset = match (&self.model, self.mold().plane_usage()) {
                        (Some(m), PlaneUsage::Base) => PartingPlane::default().offset_range(&m.mesh).0,
                        _ => 0.0,
                    };
                    self.doc.plane = PartingPlane { offset_mm: offset, ..Default::default() };
                }
            });
            let range = self.model.as_ref().map(|m| self.doc.plane.offset_range(&m.mesh)).unwrap_or((-50.0, 50.0));
            egui::Grid::new("plane_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                let p = &mut self.doc.plane;
                ui.label("Tilt X").on_hover_text("Tilts the plane (and the pull direction) about the model's X axis. You can also drag the round handles in the 3D view.");
                ui.add(egui::DragValue::new(&mut p.tilt_x_deg).speed(0.5).range(-180.0..=180.0).suffix("°").fixed_decimals(1));
                ui.end_row();
                ui.label("Tilt Y").on_hover_text("Tilts the plane about the model's Y axis.");
                ui.add(egui::DragValue::new(&mut p.tilt_y_deg).speed(0.5).range(-90.0..=90.0).suffix("°").fixed_decimals(1));
                ui.end_row();
                ui.label("Box rotation").on_hover_text("Turns the mold box around the pull direction. Suggest plane picks the rotation with the smallest box.");
                ui.add(egui::DragValue::new(&mut p.spin_deg).speed(0.5).range(-180.0..=180.0).suffix("°").fixed_decimals(1));
                ui.end_row();
                ui.label("Height").on_hover_text("Moves the plane along the pull direction. Drag the arrow handle in the 3D view for the same effect.");
                ui.add(egui::Slider::new(&mut p.offset_mm, range.0..=range.1).suffix(" mm").fixed_decimals(1));
                ui.end_row();
            });
            ui.checkbox(&mut self.show_undercuts, "Colour undercuts")
                .on_hover_text("Red faces would lock into the silicone when the halves are pulled apart. Blue faces belong to the upper half, orange to the lower half.");
            if let Some((_, _, stats)) = &self.classes {
                let pct = stats.fraction() * 100.0;
                let color = if pct < 1.0 {
                    Color32::from_rgb(110, 200, 120)
                } else if pct < 10.0 {
                    Color32::from_rgb(240, 200, 80)
                } else {
                    Color32::from_rgb(240, 110, 90)
                };
                ui.label(RichText::new(format!("Undercut: {pct:.1}% of the surface ({:.0} mm²)", stats.undercut_area_mm2)).color(color));
            }
            ui.separator();

            // ---- Parameters
            ui.heading("Settings");
            let specs = self.mold().param_specs();
            if let Some(ParamAction::PickSpout) = params_ui::show(ui, &specs, &mut self.doc.params) {
                self.pick_spout = true;
                self.notify("Click on the model to place the pour spout.", false);
            }
            ui.separator();

            // ---- Printer
            ui.heading("Printer");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("printer")
                    .selected_text(&self.doc.printer)
                    .width(ui.available_width() - 70.0)
                    .show_ui(ui, |ui| {
                        for p in &self.printers {
                            ui.selectable_value(&mut self.doc.printer, p.name.clone(), &p.name)
                                .on_hover_text(format!("{} × {} × {} mm", p.bed_x, p.bed_y, p.max_z));
                        }
                    });
                if ui.button("Edit…").clicked() {
                    self.printer_editor = Some(PrinterEditor { name: String::new(), x: 220.0, y: 220.0, z: 250.0, error: None });
                }
            });
            let pr = self.printer();
            ui.label(RichText::new(format!("Build volume {} × {} × {} mm", pr.bed_x, pr.bed_y, pr.max_z)).weak());
            ui.separator();

            // ---- Volumes
            ui.heading("Silicone");
            if let Some(g) = &self.generated {
                let r = &g.output.report;
                egui::Grid::new("volume_grid").num_columns(3).spacing([10.0, 3.0]).show(ui, |ui| {
                    for p in &r.pours {
                        ui.label(&p.name);
                        ui.label(format!("{:.1} mL", p.silicone_ml));
                        ui.label(format!(
                            "{:.0} g{}",
                            p.silicone_g,
                            p.cost.map(|c| format!("  ({c:.2})")).unwrap_or_default()
                        ));
                        ui.end_row();
                    }
                    ui.label(RichText::new("Total").strong());
                    ui.label(RichText::new(format!("{:.1} mL", r.silicone_ml)).strong());
                    ui.label(RichText::new(format!(
                        "{:.0} g{}",
                        r.silicone_g,
                        r.cost.map(|c| format!("  ({c:.2})")).unwrap_or_default()
                    ))
                    .strong());
                    ui.end_row();
                });
                ui.label(
                    RichText::new(format!(
                        "Box inside {:.1} mL − model {:.1} mL − spout/vents {:.1} mL. Casting material per cast ≈ {:.1} mL. Mix a little extra.",
                        r.cavity_ml, r.model_ml, r.channels_ml, r.cast_ml
                    ))
                    .weak()
                    .small(),
                );
            } else {
                ui.label(RichText::new("Generated with the mold.").weak());
            }
            ui.separator();

            // ---- Warnings
            let mut warnings: Vec<String> = Vec::new();
            if let Some(e) = &self.gen_error {
                warnings.push(e.clone());
            }
            if let Some(g) = &self.generated {
                warnings.extend(g.output.warnings.iter().cloned());
            }
            warnings.extend(self.layout_warnings.iter().cloned());
            if !warnings.is_empty() {
                ui.heading("Warnings");
                for w in &warnings {
                    ui.label(RichText::new(format!("⚠ {w}")).color(Color32::from_rgb(240, 170, 60)));
                }
                ui.separator();
            }
            let can_export = self.generated.is_some() && !self.generating;
            if ui
                .add_enabled(can_export, egui::Button::new(RichText::new("Export 3MF…").strong()).min_size(Vec2::new(ui.available_width() - 8.0, 32.0)))
                .on_hover_text("Writes every printed part as a separate object, arranged on the selected printer's plate. Opens in Bambu Studio and OrcaSlicer.")
                .clicked()
            {
                self.pick_file(&ctx, FilePurpose::Export3mf);
            }
            ui.add_space(12.0);
        });
    }

    fn parts_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Parts");
        let Some(g) = self.generated.clone() else {
            ui.label(RichText::new(if self.generating { "Generating…" } else { "No mold yet." }).weak());
            return;
        };
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.exploded, false, "Assembled");
            ui.selectable_value(&mut self.exploded, true, "Exploded");
        });
        ui.checkbox(&mut self.show_model, "Model");
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, p) in g.output.parts.iter().enumerate() {
                let default_visible = p.role == PartRole::Printed;
                let vis = self.part_visible.entry(p.name.clone()).or_insert(default_visible);
                ui.horizontal(|ui| {
                    ui.checkbox(vis, "");
                    let c = part_color(i, p.role);
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().rect_filled(
                        r,
                        2.0,
                        Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8),
                    );
                    let label = ui.label(&p.name);
                    let mut tip = format!("{:.1} mL", p.volume_mm3 / 1000.0);
                    if !p.note.is_empty() {
                        tip = format!("{}\n{tip}", p.note);
                    }
                    if p.role != PartRole::Printed {
                        tip.push_str("\nPreview of the silicone (not printed).");
                    }
                    label.on_hover_text(tip);
                    if p.role == PartRole::Printed && self.part_fits.get(&p.name) == Some(&false) {
                        ui.label(RichText::new("⚠").color(Color32::from_rgb(240, 170, 60)))
                            .on_hover_text("Does not fit on the plate with the other parts (or at all). See Warnings.");
                    }
                });
            }
        });
    }

    fn windows(&mut self, ctx: &egui::Context) {
        if let Some(ed) = &mut self.printer_editor {
            let mut open = true;
            let mut save: Option<Vec<PrinterProfile>> = None;
            egui::Window::new("Printers").open(&mut open).resizable(false).show(ctx, |ui| {
                ui.label("Built-in and your own printer profiles. Your profiles are saved in:");
                ui.label(RichText::new(user_printers_path().display().to_string()).monospace().small());
                ui.separator();
                let mut remove = None;
                for p in &self.printers {
                    ui.horizontal(|ui| {
                        ui.label(format!("{} — {} × {} × {} mm", p.name, p.bed_x, p.bed_y, p.max_z));
                        if self.user_printers.iter().any(|u| u.name == p.name) && ui.small_button("Remove").clicked() {
                            remove = Some(p.name.clone());
                        }
                    });
                }
                if let Some(name) = remove {
                    let mut u = self.user_printers.clone();
                    u.retain(|p| p.name != name);
                    save = Some(u);
                }
                ui.separator();
                ui.label(RichText::new("Add a printer").strong());
                egui::Grid::new("new_printer").num_columns(2).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut ed.name);
                    ui.end_row();
                    ui.label("Plate width (X)");
                    ui.add(egui::DragValue::new(&mut ed.x).range(20.0..=2000.0).suffix(" mm"));
                    ui.end_row();
                    ui.label("Plate depth (Y)");
                    ui.add(egui::DragValue::new(&mut ed.y).range(20.0..=2000.0).suffix(" mm"));
                    ui.end_row();
                    ui.label("Max height (Z)");
                    ui.add(egui::DragValue::new(&mut ed.z).range(20.0..=2000.0).suffix(" mm"));
                    ui.end_row();
                });
                if ui.button("Add printer").clicked() {
                    let p = PrinterProfile { name: ed.name.trim().to_string(), bed_x: ed.x, bed_y: ed.y, max_z: ed.z };
                    match p.validate() {
                        Ok(()) => {
                            let mut u = self.user_printers.clone();
                            u.retain(|x| x.name != p.name);
                            u.push(p);
                            save = Some(u);
                            ed.name.clear();
                            ed.error = None;
                        }
                        Err(e) => ed.error = Some(e.to_string()),
                    }
                }
                if let Some(e) = &ed.error {
                    ui.colored_label(Color32::from_rgb(240, 110, 90), e);
                }
            });
            if let Some(u) = save {
                match printer::save_user_profiles(&user_printers_path(), &u) {
                    Ok(()) => {
                        self.user_printers = u;
                        self.printers = printer::all_profiles(&self.user_printers);
                        if !self.printers.iter().any(|p| p.name == self.doc.printer) {
                            self.doc.printer = self.printers[0].name.clone();
                        }
                    }
                    Err(e) => self.notify(format!("Could not save printers: {e:#}"), true),
                }
            }
            if !open {
                self.printer_editor = None;
            }
        }
        egui::Window::new("Quick guide").open(&mut self.show_guide).default_width(520.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(GUIDE);
            });
        });
        egui::Window::new("About MoldWeaver").open(&mut self.show_about).resizable(false).show(ctx, |ui| {
            ui.heading(format!("MoldWeaver {}", env!("CARGO_PKG_VERSION")));
            ui.label("A free, open-source silicone mold box generator.");
            ui.label("License: GPL-3.0-or-later. Geometry by manifold-rust.");
            if let Some(r) = &self.renderer {
                ui.label(format!("Graphics: {}", r.adapter_info));
            }
        });
    }

    // ------------------------------------------------------------------
    // Viewport

    fn plane_frame(&self) -> Option<MoldFrame> {
        self.model.as_ref().map(|m| MoldFrame::new(&m.mesh, &self.doc.plane))
    }

    fn viewport(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let aspect = rect.width() / rect.height().max(1.0);
        let vp = self.camera.view_proj(aspect);
        let to_screen = |p: DVec3| -> Option<Pos2> {
            let c = vp * p.as_vec3().extend(1.0);
            if c.w <= 1e-6 {
                return None;
            }
            let ndc = c.truncate() / c.w;
            Some(Pos2::new(
                rect.left() + (ndc.x + 1.0) * 0.5 * rect.width(),
                rect.top() + (1.0 - ndc.y) * 0.5 * rect.height(),
            ))
        };

        // Gizmo geometry (model coordinates).
        let frame = self.plane_frame();
        let plane_r = self.model.as_ref().map(|m| m.bounds.diagonal() * 0.6).unwrap_or(50.0);
        let gizmo = frame.map(|f| {
            let c = f.model_from_mold.transform_point3(DVec3::ZERO);
            let u = f.model_from_mold.transform_vector3(DVec3::X).normalize();
            let v = f.model_from_mold.transform_vector3(DVec3::Y).normalize();
            let n = f.model_from_mold.transform_vector3(DVec3::Z).normalize();
            (c, u, v, n)
        });
        let handles: Vec<(GizmoHandle, DVec3)> = match (gizmo, self.show_plane) {
            (Some((c, u, v, n)), true) => vec![
                (GizmoHandle::Move, c + n * plane_r * 0.45),
                (GizmoHandle::TiltU, c + u * plane_r * 0.8),
                (GizmoHandle::TiltV, c + v * plane_r * 0.8),
            ],
            _ => Vec::new(),
        };

        // ---- Input
        let pointer = response.hover_pos();
        self.hover_handle = None;
        if self.drag.is_none() {
            if let Some(pp) = pointer {
                for (h, p) in &handles {
                    if let Some(s) = to_screen(*p) {
                        if s.distance(pp) < 12.0 {
                            self.hover_handle = Some(*h);
                        }
                    }
                }
            }
        }
        if response.drag_started_by(egui::PointerButton::Primary) {
            // Hit-test where the button went down, not where the pointer is
            // once the drag threshold is passed.
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            if let Some(o) = origin {
                self.drag = handles
                    .iter()
                    .filter_map(|(h, p)| to_screen(*p).map(|s| (*h, s.distance(o))))
                    .filter(|(_, d)| *d < 14.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(h, _)| h);
            }
        }
        let delta = response.drag_delta();
        if let (Some(h), Some((c, u, v, n))) = (self.drag, gizmo) {
            if response.dragged() && delta != Vec2::ZERO {
                // Screen direction of the plane normal at the handle.
                let at = match h {
                    GizmoHandle::Move => c,
                    GizmoHandle::TiltU => c + u * plane_r * 0.8,
                    GizmoHandle::TiltV => c + v * plane_r * 0.8,
                };
                if let (Some(a), Some(b)) = (to_screen(at), to_screen(at + n)) {
                    let s = b - a;
                    let along = (delta.x * s.x + delta.y * s.y) as f64 / (s.length_sq() as f64).max(1e-6);
                    match h {
                        GizmoHandle::Move => {
                            let (lo, hi) = self
                                .model
                                .as_ref()
                                .map(|m| self.doc.plane.offset_range(&m.mesh))
                                .unwrap_or((-1e3, 1e3));
                            self.doc.plane.offset_mm = (self.doc.plane.offset_mm + along).clamp(lo, hi);
                        }
                        GizmoHandle::TiltU | GizmoHandle::TiltV => {
                            let (axis, sign) = if h == GizmoHandle::TiltU { (v, -1.0) } else { (u, 1.0) };
                            let angle = sign * (along / (plane_r * 0.8)).atan();
                            let new_n = DQuat::from_axis_angle(axis, angle) * n;
                            let p = PartingPlane::from_normal(new_n, self.doc.plane.spin_deg, self.doc.plane.offset_mm);
                            self.doc.plane = p;
                        }
                    }
                }
            }
            if !response.dragged() {
                self.drag = None;
            }
        } else if response.dragged_by(egui::PointerButton::Primary) && !ui.input(|i| i.modifiers.shift) {
            self.camera.orbit(GVec2::new(delta.x, delta.y));
        } else if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary) && ui.input(|i| i.modifiers.shift))
        {
            self.camera.pan(GVec2::new(delta.x, delta.y), rect.height());
        }
        if response.drag_stopped() {
            self.drag = None;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.camera.zoom(scroll);
            }
        }
        if response.double_clicked() {
            self.fit_camera();
        }
        if response.clicked() && self.pick_spout {
            if let (Some(pp), Some(m)) = (response.interact_pointer_pos(), &self.model) {
                let ndc = GVec2::new(
                    (pp.x - rect.left()) / rect.width() * 2.0 - 1.0,
                    1.0 - (pp.y - rect.top()) / rect.height() * 2.0,
                );
                let (o, d) = self.camera.ray(ndc, aspect);
                if let Some(t) = scene::ray_mesh(&m.mesh, o.as_dvec3(), d.as_dvec3()) {
                    let hit = o.as_dvec3() + d.as_dvec3() * t;
                    let f = MoldFrame::new(&m.mesh, &self.doc.plane);
                    let local = f.mold_from_model.transform_point3(hit);
                    self.doc.params.set("spout_auto", ParamValue::Bool(false));
                    self.doc.params.set("spout_x", ParamValue::Float((local.x * 10.0).round() / 10.0));
                    self.doc.params.set("spout_y", ParamValue::Float((local.y * 10.0).round() / 10.0));
                    self.pick_spout = false;
                    self.notify(format!("Pour spout placed at ({:.1}, {:.1}).", local.x, local.y), false);
                } else {
                    self.notify("Missed the model; click on the model to place the spout.", true);
                }
            }
        }
        if self.pick_spout {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        } else if self.hover_handle.is_some() || self.drag.is_some() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }

        // ---- Explode animation
        let target = if self.exploded { 1.0 } else { 0.0 };
        if (self.explode_t - target).abs() > 1e-3 {
            let dt = ui.input(|i| i.stable_dt).min(0.05);
            self.explode_t += (target - self.explode_t).signum() * dt * 4.0;
            self.explode_t = self.explode_t.clamp(0.0, 1.0);
            ctx.request_repaint();
        } else {
            self.explode_t = target;
        }

        // ---- Draw
        let painter = ui.painter_at(rect);
        let bg = ui.visuals().extreme_bg_color;
        if let Some(err) = &self.render_error {
            painter.rect_filled(rect, 0.0, bg);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                err,
                egui::FontId::proportional(14.0),
                Color32::LIGHT_RED,
            );
            return;
        }
        let items = self.build_draw_items(frame.as_ref(), plane_r);
        let ppp = ctx.pixels_per_point();
        let size = [(rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32];
        let eye = self.camera.eye();
        if let Some(r) = &mut self.renderer {
            let bgf = egui::Rgba::from(bg);
            r.render(size, vp, eye, [bgf.r() as f64, bgf.g() as f64, bgf.b() as f64], &items);
            if let Some(id) = r.texture_id {
                painter.image(id, rect, egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
        }

        // ---- Overlays
        for (h, p) in &handles {
            if let Some(s) = to_screen(*p) {
                let active = self.drag == Some(*h) || self.hover_handle == Some(*h);
                let (col, label) = match h {
                    GizmoHandle::Move => (Color32::from_rgb(255, 210, 70), "move"),
                    GizmoHandle::TiltU => (Color32::from_rgb(240, 100, 100), "tilt"),
                    GizmoHandle::TiltV => (Color32::from_rgb(110, 210, 120), "tilt"),
                };
                if let (GizmoHandle::Move, Some((c, ..))) = (h, gizmo) {
                    if let Some(cs) = to_screen(c) {
                        painter.line_segment([cs, s], Stroke::new(2.0, col));
                    }
                }
                painter.circle(
                    s,
                    if active { 9.0 } else { 7.0 },
                    col.gamma_multiply(if active { 1.0 } else { 0.8 }),
                    Stroke::new(1.5, Color32::BLACK),
                );
                if active {
                    painter.text(
                        s + Vec2::new(12.0, -12.0),
                        egui::Align2::LEFT_BOTTOM,
                        label,
                        egui::FontId::proportional(12.0),
                        col,
                    );
                }
            }
        }
        if self.show_markers {
            if let Some(g) = &self.generated {
                for (name, p) in &g.output.markers {
                    let world = g.frame.model_from_mold.transform_point3(*p);
                    if let Some(s) = to_screen(world) {
                        let col = match name.as_str() {
                            "Pour spout" => Color32::from_rgb(80, 200, 255),
                            "Vent" => Color32::from_rgb(200, 140, 255),
                            _ => Color32::from_rgb(255, 255, 255),
                        };
                        painter.circle_stroke(s, 5.0, Stroke::new(2.0, col));
                    }
                }
            }
        }
        // Axis triad.
        let o = Pos2::new(rect.left() + 40.0, rect.bottom() - 40.0);
        let view = self.camera.view();
        for (axis, col, name) in [
            (Vec3::X, Color32::from_rgb(230, 80, 80), "X"),
            (Vec3::Y, Color32::from_rgb(90, 200, 90), "Y"),
            (Vec3::Z, Color32::from_rgb(90, 140, 240), "Z"),
        ] {
            let d = view.transform_vector3(axis);
            let e = o + Vec2::new(d.x, -d.y) * 26.0;
            painter.line_segment([o, e], Stroke::new(2.0, col));
            painter.text(e, egui::Align2::CENTER_CENTER, name, egui::FontId::proportional(11.0), col);
        }
        let status = if self.model.is_none() {
            "Open a model: File ▸ Open model (Ctrl+O), or drop an STL/OBJ/3MF file here."
        } else if self.pick_spout {
            "Click on the model to place the pour spout (Esc to cancel)."
        } else {
            "Drag: orbit · Shift/right drag: pan · Scroll: zoom · Double-click: frame · Drag the handles to move or tilt the plane"
        };
        painter.text(
            rect.left_top() + Vec2::new(10.0, 8.0),
            egui::Align2::LEFT_TOP,
            status,
            egui::FontId::proportional(12.0),
            Color32::from_gray(170),
        );
        if self.generating || self.suggesting || self.busy_task.is_some() {
            let msg = self.busy_task.clone().unwrap_or_else(|| {
                if self.suggesting {
                    "Searching for a parting plane…".into()
                } else {
                    "Generating mold…".into()
                }
            });
            painter.text(
                rect.right_top() + Vec2::new(-10.0, 8.0),
                egui::Align2::RIGHT_TOP,
                msg,
                egui::FontId::proportional(13.0),
                Color32::from_rgb(255, 210, 70),
            );
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pick_spout = false;
        }
    }

    fn build_draw_items(&mut self, frame: Option<&MoldFrame>, plane_r: f64) -> Vec<DrawItem> {
        let Some(renderer) = &self.renderer else { return Vec::new() };
        let mut items = Vec::new();
        let Some(model) = &self.model else { return items };

        // Grid under the model.
        let b = model.bounds;
        let half = (b.diagonal() as f32).max(20.0) * 1.2;
        if self.gpu.grid.as_ref().is_none_or(|(h, _)| *h != half) {
            let step = nice_step(half / 10.0);
            let mesh =
                renderer.upload(&scene::line_vertices(&scene::grid_segments(half, step), [0.5, 0.5, 0.5, 0.35]), true);
            self.gpu.grid = Some((half, mesh));
        }
        if let Some((_, grid)) = &self.gpu.grid {
            let c = b.center();
            items.push(DrawItem {
                mesh: grid.clone(),
                model: Mat4::from_translation(Vec3::new(c.x as f32, c.y as f32, b.min.z as f32 - 0.5)),
                tint: Vec4::ONE,
                lit: false,
            });
        }

        // Model with undercut colours.
        let colouring = if self.show_undercuts { self.classes_version } else { 0 };
        if self.gpu.model.as_ref().is_none_or(|(v, c, _)| *v != model.version || *c != colouring) {
            let classes = if self.show_undercuts { self.classes.as_ref().map(|c| c.1.clone()) } else { None };
            let verts = scene::mesh_vertices(&model.mesh, |i| match &classes {
                Some(cl) if i < cl.len() => scene::class_color(cl[i], true),
                _ => scene::MODEL_COLOR,
            });
            self.gpu.model = Some((model.version, colouring, renderer.upload(&verts, false)));
        }
        if self.show_model {
            if let Some((_, _, mesh)) = &self.gpu.model {
                items.push(DrawItem { mesh: mesh.clone(), model: Mat4::IDENTITY, tint: Vec4::ONE, lit: true });
            }
        }

        // Generated parts.
        if let Some(g) = &self.generated {
            if self.gpu.parts_for != Some(self.generated_id) {
                self.gpu.parts = g
                    .output
                    .parts
                    .iter()
                    .map(|p| renderer.upload(&scene::mesh_vertices(&p.mesh, |_| [1.0, 1.0, 1.0, 1.0]), false))
                    .collect();
                self.gpu.parts_for = Some(self.generated_id);
            }
            let base = g.frame.model_from_mold;
            for (i, (p, mesh)) in g.output.parts.iter().zip(&self.gpu.parts).enumerate() {
                let visible = *self.part_visible.get(&p.name).unwrap_or(&(p.role == PartRole::Printed));
                if !visible {
                    continue;
                }
                let offset = p.explode * self.explode_t as f64;
                let m = base * DMat4::from_translation(offset);
                let c = part_color(i, p.role);
                let alpha = match p.role {
                    PartRole::Printed if self.explode_t < 0.5 => 0.28,
                    PartRole::Printed => 1.0,
                    PartRole::Silicone(_) => 0.55,
                };
                items.push(DrawItem {
                    mesh: mesh.clone(),
                    model: m.as_mat4(),
                    tint: Vec4::new(c[0], c[1], c[2], alpha),
                    lit: true,
                });
            }
        }

        // Parting plane.
        if self.show_plane {
            if let Some(f) = frame {
                if self.gpu.plane.is_none() {
                    self.gpu.plane = Some(renderer.upload(&scene::quad_vertices(1.0, [1.0, 0.85, 0.3, 1.0]), false));
                    let pts = [
                        Vec3::new(-1.0, -1.0, 0.0),
                        Vec3::new(1.0, -1.0, 0.0),
                        Vec3::new(1.0, 1.0, 0.0),
                        Vec3::new(-1.0, 1.0, 0.0),
                    ];
                    let segs: Vec<(Vec3, Vec3)> = (0..4).map(|i| (pts[i], pts[(i + 1) % 4])).collect();
                    self.gpu.plane_outline =
                        Some(renderer.upload(&scene::line_vertices(&segs, [1.0, 0.85, 0.3, 0.9]), true));
                }
                let m = (f.model_from_mold * DMat4::from_scale(DVec3::new(plane_r, plane_r, 1.0))).as_mat4();
                if let Some(q) = &self.gpu.plane {
                    items.push(DrawItem {
                        mesh: q.clone(),
                        model: m,
                        tint: Vec4::new(1.0, 1.0, 1.0, 0.18),
                        lit: false,
                    });
                }
                if let Some(o) = &self.gpu.plane_outline {
                    items.push(DrawItem { mesh: o.clone(), model: m, tint: Vec4::ONE, lit: false });
                }
            }
        }
        items
    }
}

fn part_color(i: usize, role: PartRole) -> [f32; 3] {
    match role {
        PartRole::Silicone(_) => [0.55, 0.85, 0.80],
        PartRole::Printed => scene::PART_COLORS[i % scene::PART_COLORS.len()],
    }
}

fn nice_step(x: f32) -> f32 {
    let p = 10f32.powf(x.max(1e-3).log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if m * p >= x {
            return m * p;
        }
    }
    10.0 * p
}

impl eframe::App for MoldWeaverApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(p) = self.pending_open.take() {
            self.open_path(&ctx, p);
        }
        self.handle_messages(&ctx);
        self.shortcuts(&ctx);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let recent_error =
                    self.messages.last().is_some_and(|(_, e, t)| *e && t.elapsed() < Duration::from_secs(20));
                let col = if recent_error { Color32::from_rgb(240, 170, 60) } else { ui.visuals().text_color() };
                ui.label(RichText::new(&self.status).color(col));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(r) = &self.renderer {
                        ui.label(RichText::new(&r.adapter_info).weak().small());
                    }
                    if self.generated.is_some() {
                        ui.label(RichText::new(format!("last build {} ms", self.gen_millis)).weak().small());
                    }
                });
            });
        });
        egui::Panel::left("settings")
            .resizable(true)
            .default_size(360.0)
            .min_size(300.0)
            .show(ui, |ui| self.left_panel(ui));
        egui::Panel::right("parts")
            .resizable(true)
            .default_size(220.0)
            .min_size(160.0)
            .show(ui, |ui| self.parts_panel(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| self.viewport(ui));
        self.windows(&ctx);
        self.schedule(&ctx);
    }
}

const GUIDE: &str = "\
Two-part pour with the parting board

1. Open your model (STL, OBJ or 3MF). If it is not watertight, click Repair mesh.
2. Pick 'Two-part split box' and click Suggest plane (or drag the plane handles). Red faces are undercuts: try to keep them small.
3. Adjust the settings. Hover any setting for an explanation. The mold rebuilds automatically.
4. Export 3MF, open it in Bambu Studio or OrcaSlicer and print all parts.

Pour 1 (top half A)
5. Put the base plate on the table and lay the parting board on it (cut-out up). Lower box half B (flange up) over the board into the base plate's rim; the walls taper, so it only fits this way.
6. Press the model into the board's cut-out so it sits exactly at the parting line.
7. Push the alignment dowels into the corner sockets of half B and put half A on top. Fasten with bolts, zip ties or clamps.
8. Rest the spout & vent rods on top of half A so the rods touch the model.
9. Spray mold release, then pour silicone A slowly into one corner until it reaches the top. Let it cure.

Pour 2 (bottom half B)
10. Remove the rods, flip the whole box over and take off the base plate. Lift out the parting board; the model stays in silicone A.
11. Brush or spray mold release onto silicone A (silicone sticks to silicone!). The key dimples will be filled by silicone B.
12. Pour silicone B until it reaches the rim of half B. Let it cure.

Demold and cast
13. Undo the fasteners, separate the box halves, push each silicone half out of its ring and remove the model.
14. To cast: put the silicone halves back into the rings, fasten the box, set it on the base plate (half B down) and pour resin through the funnel.
";

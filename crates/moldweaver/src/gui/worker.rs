//! Background work. Geometry runs off the UI thread so the window stays
//! responsive; only the newest request of each kind is computed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use eframe::egui;
use moldweaver_core::analysis::{FaceClass, Suggestion, UndercutStats};
use moldweaver_core::frame::PartingPlane;
use moldweaver_core::mesh::{RepairReport, TriMesh};
use moldweaver_core::molds::ParamValues;
use moldweaver_core::pipeline::Generated;
use moldweaver_core::project::Project;

/// Results sent back to the UI thread.
pub enum Msg {
    Generated { id: u64, result: Result<Arc<Generated>, String>, millis: u128 },
    Analyzed { id: u64, classes: Arc<Vec<FaceClass>>, stats: UndercutStats },
    Suggested { mold_type: String, result: Suggestion },
    ModelLoaded { path: PathBuf, result: Result<TriMesh, String> },
    ProjectLoaded { path: PathBuf, result: Result<Box<(Project, TriMesh)>, String> },
    Repaired { mesh: TriMesh, report: RepairReport },
    Exported { path: PathBuf, result: Result<Vec<String>, String> },
    FilePicked { purpose: FilePurpose, path: Option<PathBuf> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePurpose {
    OpenModel,
    OpenProject,
    SaveProject,
    Export3mf,
}

/// A generation request.
pub struct GenerateJob {
    pub id: u64,
    pub mesh: Arc<TriMesh>,
    pub plane: PartingPlane,
    pub mold_type: String,
    pub params: ParamValues,
}

/// An undercut-analysis request.
pub struct AnalyzeJob {
    pub id: u64,
    pub mesh: Arc<TriMesh>,
    pub plane: PartingPlane,
    pub mold_type: String,
}

/// The pending job of a lane and its cancel flag.
type Slot<J> = Arc<(Mutex<Option<(J, Arc<AtomicBool>)>>, Condvar)>;

/// A worker thread that only ever runs the latest submitted job.
pub struct Lane<J: Send + 'static> {
    slot: Slot<J>,
    running_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl<J: Send + 'static> Lane<J> {
    pub fn spawn(name: &str, run: impl Fn(J, &AtomicBool) + Send + 'static) -> Lane<J> {
        let slot: Slot<J> = Arc::new((Mutex::new(None), Condvar::new()));
        let running_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>> = Arc::new(Mutex::new(None));
        let (s, rc) = (slot.clone(), running_cancel.clone());
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || loop {
                let (job, cancel) = {
                    let mut guard = s.0.lock().unwrap();
                    while guard.is_none() {
                        guard = s.1.wait(guard).unwrap();
                    }
                    guard.take().unwrap()
                };
                *rc.lock().unwrap() = Some(cancel.clone());
                run(job, &cancel);
                *rc.lock().unwrap() = None;
            })
            .expect("spawn worker");
        Lane { slot, running_cancel }
    }

    /// Queue `job`, replacing any queued one and cancelling the running one.
    pub fn submit(&self, job: J) {
        if let Some(c) = self.running_cancel.lock().unwrap().as_ref() {
            c.store(true, Ordering::Relaxed);
        }
        *self.slot.0.lock().unwrap() = Some((job, Arc::new(AtomicBool::new(false))));
        self.slot.1.notify_one();
    }
}

pub fn generate_lane(tx: Sender<Msg>, ctx: egui::Context) -> Lane<GenerateJob> {
    Lane::spawn("moldweaver-generate", move |job: GenerateJob, cancel| {
        let Some(mold) = moldweaver_core::molds::find(&job.mold_type) else {
            return;
        };
        let t = Instant::now();
        let result = moldweaver_core::pipeline::generate(&job.mesh, &job.plane, mold.as_ref(), &job.params, cancel);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = tx.send(Msg::Generated {
            id: job.id,
            result: result.map(Arc::new).map_err(|e| format!("{e:#}")),
            millis: t.elapsed().as_millis(),
        });
        ctx.request_repaint();
    })
}

pub fn analyze_lane(tx: Sender<Msg>, ctx: egui::Context) -> Lane<AnalyzeJob> {
    Lane::spawn("moldweaver-analyze", move |job: AnalyzeJob, cancel| {
        let Some(mold) = moldweaver_core::molds::find(&job.mold_type) else {
            return;
        };
        let (classes, stats) = moldweaver_core::pipeline::face_classes(&job.mesh, &job.plane, mold.as_ref());
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = tx.send(Msg::Analyzed { id: job.id, classes: Arc::new(classes), stats });
        ctx.request_repaint();
    })
}

/// Run a one-off task on its own thread and post its message.
pub fn spawn_task(tx: &Sender<Msg>, ctx: &egui::Context, f: impl FnOnce() -> Msg + Send + 'static) {
    let (tx, ctx) = (tx.clone(), ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(f());
        ctx.request_repaint();
    });
}

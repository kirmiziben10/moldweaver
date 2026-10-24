//! The desktop window (eframe/egui + wgpu).

mod app;
mod camera;
mod params_ui;
mod render;
mod scene;
mod worker;

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use eframe::egui;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use eframe::wgpu;

fn options(backends: wgpu::Backends) -> eframe::NativeOptions {
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends;
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MoldWeaver")
            .with_app_id("io.github.moldweaver.MoldWeaver")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() },
        ..Default::default()
    }
}

pub fn run(file: Option<PathBuf>) -> Result<()> {
    // Vulkan first, OpenGL (GLES) as fallback. WGPU_BACKEND=gl forces GL.
    let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::GL);
    let start = |backends: wgpu::Backends, file: Option<PathBuf>| {
        eframe::run_native(
            "MoldWeaver",
            options(backends),
            Box::new(move |cc| Ok(Box::new(app::MoldWeaverApp::new(cc, file)))),
        )
    };
    match start(backends, file.clone()) {
        Ok(()) => Ok(()),
        Err(e) if backends != wgpu::Backends::GL => {
            log::warn!("could not start with {backends:?} ({e}); retrying with OpenGL");
            start(wgpu::Backends::GL, file).map_err(|e| anyhow!("could not open the window: {e}"))
        }
        Err(e) => Err(anyhow!("could not open the window: {e}")),
    }
}

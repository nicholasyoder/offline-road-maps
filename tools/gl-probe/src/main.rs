//! A plain eframe window on wgpu with the options the app uses (DX12 then GL on Windows, 4×
//! MSAA, no vsync, a fixed 1280×800 window). It draws a label for 20 s, closes itself and
//! prints "OK <frames> frames". With `panic = "unwind"` a panic prints a backtrace first.
//! `PROBE_MSAA=1` turns multisampling off; `WGPU_BACKEND` works as in the app.

use std::time::{Duration, Instant};

struct Probe {
    start: Instant,
    frames: u64,
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        if self.frames <= 3 {
            eprintln!(
                "[probe +{} ms] frame {}",
                self.start.elapsed().as_millis(),
                self.frames
            );
        }
        ui.heading(format!("gl-probe: frame {}", self.frames));
        ui.label("A stock eframe window. It closes itself after 20 seconds.");
        if self.start.elapsed() > Duration::from_secs(20) {
            ui.ctx()
                .send_viewport_cmd(eframe::egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint();
    }
}

fn main() {
    let start = Instant::now();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!(
            "[probe +{} ms] PANIC on {:?}: {info}\n{}",
            start.elapsed().as_millis(),
            std::thread::current().name(),
            std::backtrace::Backtrace::force_capture()
        );
    }));
    let msaa = std::env::var("PROBE_MSAA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4u16);
    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        multisampling: msaa,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_resizable(false)
            .with_title("gl-probe"),
        ..Default::default()
    };
    options.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    #[cfg(windows)]
    if eframe::wgpu::Backends::from_env().is_none()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(s) = &mut options.wgpu_options.wgpu_setup
    {
        s.instance_descriptor.backends = eframe::wgpu::Backends::DX12 | eframe::wgpu::Backends::GL;
    }
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(s) = &mut options.wgpu_options.wgpu_setup {
        let base = s.device_descriptor.clone();
        s.device_descriptor = std::sync::Arc::new(move |adapter| eframe::wgpu::DeviceDescriptor {
            memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
            ..base(adapter)
        });
    }
    eprintln!("[probe +0 ms] run_native, msaa {msaa}");
    let res = eframe::run_native(
        "gl-probe",
        options,
        Box::new(move |cc| {
            if let Some(rs) = &cc.wgpu_render_state {
                let i = rs.adapter.get_info();
                eprintln!(
                    "[probe +{} ms] adapter: {} ({:?})",
                    start.elapsed().as_millis(),
                    i.name,
                    i.backend
                );
            }
            Ok(Box::new(Probe { start, frames: 0 }))
        }),
    );
    match res {
        Ok(()) => eprintln!("OK {} s", start.elapsed().as_secs()),
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    }
}

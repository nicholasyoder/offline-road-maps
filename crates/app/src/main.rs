//! Offline Road Maps viewer.
//!
//! ```text
//! omp-app [--pack <dir>]
//! omp-app --bench <flight.toml> [--pack <dir>] [--out <report.json>]
//! ```

mod app;
mod bench;
mod cache;
mod camera;
mod config;
mod loader;
mod memory;
mod packs;
mod tiles;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use eframe::egui;

const USAGE: &str = "usage: omp-app [--pack <dir>]
       omp-app --bench <flight.toml> [--pack <dir>] [--out <report.json>]

  --pack <dir>   open the pack in <dir> (or the first chunk file itself); default:
                 maps/ next to the executable, else the pack used last
  --bench <file> fly the path in <file> and write a JSON report, then exit
  --out <file>   where the bench report goes (default: bench-<flight name>.json)";

#[derive(Default)]
struct Args {
    pack: Option<PathBuf>,
    bench: Option<PathBuf>,
    out: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.to_str() {
            Some("--pack") => a.pack = Some(value("--pack")?),
            Some("--bench") => a.bench = Some(value("--bench")?),
            Some("--out") => a.out = Some(value("--out")?),
            Some("-h" | "--help") => return Err(String::new()),
            _ => return Err(format!("unknown argument {}", arg.to_string_lossy())),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let started = Instant::now();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(if e.is_empty() { 0 } else { 2 });
        }
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let config = config::Location::find(&exe_dir);
    let (cfg, _) = config.load();

    // --pack, else maps/ next to the executable, else the pack used last.
    let mut notices = Vec::new();
    let found = match &args.pack {
        Some(p) => {
            let f = packs::find(p);
            if f.is_empty() {
                notices.push(format!("No pack (*.omp.000) in {}", p.display()));
            }
            f
        }
        None => {
            let f = packs::find(&exe_dir.join("maps"));
            match (&cfg.pack, f.is_empty()) {
                (Some(last), true) => packs::find(&config::resolve_path(last, &exe_dir)),
                _ => f,
            }
        }
    };
    let last = cfg.pack.as_ref().map(|p| config::resolve_path(p, &exe_dir));
    let chosen = match found.as_slice() {
        [one] => Some(one.clone()),
        many => many
            .iter()
            .find(|f| last.as_deref() == Some(packs::pack_dir(f).as_path()))
            .cloned(),
    };

    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("Offline Road Maps"),
        ..Default::default()
    };
    let bench = match &args.bench {
        Some(path) => {
            let flight = match bench::Flight::load(path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            if chosen.is_none() {
                eprintln!("bench: no pack to open; pass --pack <dir>");
                for n in &notices {
                    eprintln!("{n}");
                }
                return ExitCode::FAILURE;
            }
            options.viewport = options
                .viewport
                .with_inner_size(flight.window)
                .with_resizable(false);
            // Frame times are only meaningful without vsync. Wayland compositors still pace
            // frames to the display, so use X11 (XWayland) when there is one.
            options.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
            #[cfg(target_os = "linux")]
            if std::env::var_os("DISPLAY").is_some() {
                options.event_loop_builder = Some(Box::new(|b| {
                    use winit::platform::x11::EventLoopBuilderExtX11;
                    b.with_x11();
                }));
            }
            let out = args
                .out
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("bench-{}.json", flight.name)));
            Some(bench::Bench::new(flight, path.clone(), out, started))
        }
        None => None,
    };

    let startup = app::Startup {
        exe_dir,
        config,
        found,
        chosen,
        notices,
        bench,
        started,
    };
    let res = eframe::run_native(
        "Offline Road Maps",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, startup)))),
    );
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

//! Offline Road Maps viewer.
//!
//! ```text
//! omp-app [--pack <dir>] [--style <file>]
//! omp-app --bench <flight.toml> [--pack <dir>] [--out <report.json>]
//! omp-app --render --pack <dir> (--center <lat,lon> --zoom <z> | --route <lat,lon;lat,lon…>)
//!         [--step <n>] [--alternatives] [--size <w>x<h>] [--scale <s>] [--msaa <n>] [--style <file>] <out.png>
//! ```

mod app;
mod bench;
mod cache;
mod camera;
mod config;
mod files;
mod here;
mod labels;
mod layers;
mod loader;
mod logger;
mod memory;
mod mesh;
mod packs;
mod pins;
mod pool;
mod render;
mod routing;
mod scale;
mod search;
mod shapes;
mod side;
mod snapshot;
mod style;
mod text;
mod theme;
mod tiles;
mod trips;
mod verify;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use eframe::egui;

const USAGE: &str = "usage: omp-app [--pack <dir>] [--style <file>]
       omp-app --bench <flight.toml> [--pack <dir>] [--out <report.json>]
       omp-app --render --pack <dir> (--center <lat,lon> --zoom <z> | --route <lat,lon;lat,lon…>)
               [--step <n>] [--alternatives] [--size <w>x<h>] [--scale <s>] [--msaa <n>] [--style <file>] <out.png>

  --pack <dir>   open the pack in <dir> (or the first chunk file itself); default:
                 maps/ next to the executable, else the pack used last
  --bench <file> fly the path in <file> and write a JSON report, then exit
  --out <file>   where the bench report goes (default: bench-<flight name>.json)
  --style <file> the map style; default: style.toml next to the executable, else the
                 built-in style (debug builds reload it when it changes)
  --render       draw one view offscreen to <out.png>, without a window; --size in
                 pixels (default 1200x800), --scale in pixels per point (default 1),
                 --msaa 1 to turn antialiasing off; --route draws the route through
                 the stops, and fits the view to it without --center and --zoom;
                 --step draws step <n> of its directions over it, and fits that;
                 --alternatives also finds each leg's alternatives and draws them
                 grey, as the app does for a leg whose routes are open";

#[derive(Default)]
struct Args {
    pack: Option<PathBuf>,
    bench: Option<PathBuf>,
    out: Option<PathBuf>,
    render: bool,
    center: Option<String>,
    zoom: Option<String>,
    route: Option<String>,
    step: Option<String>,
    alternatives: bool,
    size: Option<String>,
    scale: Option<String>,
    msaa: Option<String>,
    style: Option<PathBuf>,
    /// The PNG for `--render`.
    file: Option<PathBuf>,
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
        let text = |p: PathBuf| p.to_string_lossy().into_owned();
        match arg.to_str() {
            Some("--pack") => a.pack = Some(value("--pack")?),
            Some("--bench") => a.bench = Some(value("--bench")?),
            Some("--out") => a.out = Some(value("--out")?),
            Some("--render") => a.render = true,
            Some("--center") => a.center = Some(text(value("--center")?)),
            Some("--zoom") => a.zoom = Some(text(value("--zoom")?)),
            Some("--route") => a.route = Some(text(value("--route")?)),
            Some("--step") => a.step = Some(text(value("--step")?)),
            Some("--alternatives") => a.alternatives = true,
            Some("--size") => a.size = Some(text(value("--size")?)),
            Some("--scale") => a.scale = Some(text(value("--scale")?)),
            Some("--msaa") => a.msaa = Some(text(value("--msaa")?)),
            Some("--style") => a.style = Some(value("--style")?),
            Some("-h" | "--help") => return Err(String::new()),
            Some(s) if !s.starts_with('-') && a.file.is_none() => a.file = Some(s.into()),
            _ => return Err(format!("unknown argument {}", arg.to_string_lossy())),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let started = Instant::now();
    logger::init();
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
    // --style, else style.toml next to the executable if there is one, else the built-in one.
    let style_path = args
        .style
        .clone()
        .unwrap_or_else(|| exe_dir.join(style::FILE));
    let style = match args.style.is_some() || style_path.exists() {
        true => style::StyleFile::read(&style_path),
        false => Ok(style::StyleFile::builtin()),
    };
    if args.render {
        return match style {
            Ok(style) => render(&args, style),
            Err(e) => {
                eprintln!("render: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let config = config::Location::find(&exe_dir);
    let (cfg, _) = config.load();

    // --pack, else maps/ next to the executable, else the pack used last.
    let mut notices = Vec::new();
    let style = style.unwrap_or_else(|e| {
        notices.push(format!("{e}\nUsing the built-in style."));
        style::StyleFile::builtin()
    });
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

    let msaa = render::samples(cfg.msaa);
    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        multisampling: msaa as u16,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("Offline Road Maps"),
        ..Default::default()
    };
    // Windows: DX12, else GL. Intel's Vulkan driver for UHD 620 crashed while wgpu listed
    // adapters (phase1-plan.md, 1.10), and DX12 covers the same GPUs. WGPU_BACKEND overrides.
    #[cfg(windows)]
    if eframe::wgpu::Backends::from_env().is_none()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(s) = &mut options.wgpu_options.wgpu_setup
    {
        s.instance_descriptor.backends = eframe::wgpu::Backends::DX12 | eframe::wgpu::Backends::GL;
    }
    // Smaller allocator blocks (8–64 MiB rather than 128–256): on integrated GPUs, which
    // share system memory, Windows counts a whole block in the working set.
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(s) = &mut options.wgpu_options.wgpu_setup {
        let base = s.device_descriptor.clone();
        s.device_descriptor = std::sync::Arc::new(move |adapter| eframe::wgpu::DeviceDescriptor {
            memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
            ..base(adapter)
        });
    }
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
            // frames to the display, so use X11 (XWayland) when there is one. OMP_BENCH_VSYNC
            // keeps vsync, to compare (the HD 4000's GL driver leaked per frame without it).
            if std::env::var_os("OMP_BENCH_VSYNC").is_none() {
                options.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
            }
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
            // A first line, so a log that stops here shows the app did start.
            eprintln!(
                "omp-app {}: bench {} on {}",
                env!("CARGO_PKG_VERSION"),
                path.display(),
                chosen
                    .as_ref()
                    .map_or(String::new(), |p| p.display().to_string())
            );
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
        msaa,
        style,
        style_path,
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

fn render(args: &Args, style: style::StyleFile) -> ExitCode {
    let (Some(pack), Some(file)) = (&args.pack, &args.file) else {
        eprintln!("--render needs --pack and an output file\n{USAGE}");
        return ExitCode::from(2);
    };
    let snap = snapshot::Snapshot::parse(
        pack.clone(),
        args.center.as_deref(),
        args.zoom.as_deref(),
        args.route.as_deref(),
        args.size.as_deref(),
        args.scale.as_deref(),
        args.msaa.as_deref(),
        file.clone(),
    );
    let step = match args.step.as_deref().map(str::parse::<usize>) {
        None => None,
        Some(Ok(n)) if n > 0 => Some(n - 1),
        Some(_) => {
            eprintln!("--step wants a step number from 1\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let alternatives = args.alternatives;
    let snap = snap.map(|s| snapshot::Snapshot {
        style,
        step,
        alternatives,
        ..s
    });
    match snap.and_then(|s| s.run()) {
        Ok(msg) => {
            eprintln!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("render: {e}");
            ExitCode::FAILURE
        }
    }
}

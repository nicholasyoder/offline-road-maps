//! Offline Road Maps viewer.
//!
//! ```text
//! OfflineRoadMaps [--pack <dir>] [--style <file>]
//! OfflineRoadMaps --bench <flight.toml> [--pack <dir>] [--out <report.json>]
//! OfflineRoadMaps --render --pack <dir> (--center <lat,lon> --zoom <z> | --route <lat,lon;lat,lon…>)
//!         [--step <n>] [--alternatives] [--size <w>x<h>] [--scale <s>] [--msaa <n>] [--style <file>] <out.png>
//! ```
//!
//! Windows release builds open no console window beside the app: started from a terminal,
//! they write to the terminal's ([`attach_console`]).

// 5.1 D6. Debug builds keep their console, for the warnings.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod allocs;
mod app;
mod bench;
mod cache;
mod camera;
mod config;
mod dialog;
mod files;
mod here;
mod keys;
mod labels;
mod layers;
mod loader;
mod logger;
mod memory;
mod mesh;
mod packs;
mod pages;
mod pdf;
mod pins;
mod pool;
mod print;
mod printer;
mod printui;
mod render;
mod routing;
mod scale;
mod search;
mod shapes;
mod side;
mod snapshot;
mod style;
mod styles;
mod systheme;
mod text;
mod theme;
mod tiles;
mod trace;
mod trips;
mod verify;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use eframe::egui;

/// The window's icon (title bar, taskbar, Alt-Tab): `data/icon.png`, 256 px, compiled in.
fn window_icon() -> egui::IconData {
    let mut reader = png::Decoder::new(std::io::Cursor::new(include_bytes!("../data/icon.png")))
        .read_info()
        .expect("icon.png decodes");
    let mut rgba = vec![0; reader.output_buffer_size().expect("icon.png size")];
    let info = reader.next_frame(&mut rgba).expect("icon.png frame");
    rgba.truncate(info.buffer_size());
    egui::IconData {
        rgba,
        width: info.width,
        height: info.height,
    }
}

const USAGE: &str = "usage: OfflineRoadMaps [--pack <dir>] [--style <file>]
       OfflineRoadMaps --bench <flight.toml> [--pack <dir>] [--out <report.json>]
       OfflineRoadMaps --render --pack <dir> (--center <lat,lon> --zoom <z> | --route <lat,lon;lat,lon…>)
               [--step <n>] [--alternatives] [--size <w>x<h>] [--scale <s>] [--msaa <n>] [--style <file>] <out.png>
       OfflineRoadMaps --render --pack <dir> (--center <lat,lon> --zoom <z> | --route <lat,lon;lat,lon…>)
               --print <out.pdf> [--paper <p>] [--orientation <o>] [--text-size <t>] [--label <text>]
               [--directions] [--dpi <n>] [--units <u>]

  --pack <dir>   open the pack in <dir> (or the first chunk file itself); default:
                 maps/ next to the executable, else the pack used last
  --bench <file> fly the path in <file> and write a JSON report, then exit
  --out <file>   where the bench report goes (default: bench-<flight name>.json)
  --style <file> use this map style file, whatever the Map tab has chosen; the Map tab
                 lists the built-in styles, style.toml and styles/*.toml next to the
                 executable (debug builds reload the file in use when it changes).
                 With --render, the default is style.toml next to the executable if
                 there is one, else the built-in style
  --render       draw one view offscreen to <out.png>, without a window; --size in
                 pixels (default 1200x800), --scale in pixels per point (default 1),
                 --msaa 1 to turn antialiasing off; --route draws the route through
                 the stops, and fits the view to it without --center and --zoom;
                 --step draws step <n> of its directions over it, and fits that;
                 --alternatives also finds each leg's alternatives and draws them
                 grey, as the app does for a leg whose routes are open
  --print <file> print a page instead of a view: the map fitted to the route (or the
                 view --center, --zoom and --size give), or with --directions its
                 directions, to a PDF, or a PNG per page if <file> does not end in .pdf;
                 --paper letter (default), legal, tabloid, a5, a4 or a3; --orientation
                 automatic (default), portrait or landscape; --text-size small, normal
                 or large; --label is the page's header; --dpi 300 by default";

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
    /// `--print`: the PDF, or PNG, to write.
    print: Option<PathBuf>,
    paper: Option<String>,
    orientation: Option<String>,
    text_size: Option<String>,
    label: Option<String>,
    directions: bool,
    dpi: Option<String>,
    units: Option<String>,
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
            Some("--print") => a.print = Some(value("--print")?),
            Some("--paper") => a.paper = Some(text(value("--paper")?)),
            Some("--orientation") => a.orientation = Some(text(value("--orientation")?)),
            Some("--text-size") => a.text_size = Some(text(value("--text-size")?)),
            Some("--label") => a.label = Some(text(value("--label")?)),
            Some("--directions") => a.directions = true,
            Some("--dpi") => a.dpi = Some(text(value("--dpi")?)),
            Some("--units") => a.units = Some(text(value("--units")?)),
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
    attach_console();
    cap_malloc_arenas();
    trace::init(started);
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
    if args.render {
        // --style, else style.toml next to the executable if there is one, else the
        // built-in one.
        let path = args
            .style
            .clone()
            .unwrap_or_else(|| exe_dir.join(style::FILE));
        let style = match args.style.is_some() || path.exists() {
            true => style::StyleFile::read(&path),
            false => Ok(style::StyleFile::builtin()),
        };
        return match style {
            Ok(style) => render(&args, style),
            Err(e) => {
                eprintln!("render: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let config = config::Location::find(&exe_dir);
    let (cfg, cfg_note) = config.load();

    // --pack, else maps/ next to the executable, else the pack used last.
    let mut notices = Vec::new();
    let found = match &args.pack {
        Some(p) => {
            let f = packs::find(p);
            if f.is_empty() {
                notices.push(format!(
                    "There is no map in {} (a map's first file ends in .omp.000).",
                    p.display()
                ));
            }
            f
        }
        None => {
            let f = packs::find(&exe_dir.join("maps"));
            match (&cfg.pack, f.is_empty()) {
                (Some(last), true) if cfg.start.reopen_pack => {
                    packs::find(&config::resolve_path(last, &exe_dir))
                }
                _ => f,
            }
        }
    };
    let last = cfg
        .pack
        .as_ref()
        .filter(|_| cfg.start.reopen_pack)
        .map(|p| config::resolve_path(p, &exe_dir));
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
            .with_title("Offline Road Maps")
            // Wayland app_id and X11 class, so desktops group the window under the product name.
            .with_app_id("OfflineRoadMaps")
            .with_icon(window_icon()),
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
            // OMP_WINDOW=WxH and OMP_RESIZABLE=1 change the window the flight opens, to test
            // what triggers the E6430's resize-time crash (phase5-plan.md 5.14, round 3).
            let size = trace::window_override().unwrap_or(flight.window);
            options.viewport = options
                .viewport
                .with_inner_size(size)
                .with_resizable(std::env::var_os("OMP_RESIZABLE").is_some());
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
                "OfflineRoadMaps {}: bench {} on {}",
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

    if bench.is_none() {
        let w = &cfg.window;
        options.viewport = options.viewport.with_inner_size(w.size);
        if let Some(p) = w.pos {
            options.viewport = options.viewport.with_position(p);
        }
        if w.maximized {
            options.viewport = options.viewport.with_maximized(true);
        }
    }

    surface_overrides(&mut options);
    let startup = app::Startup {
        exe_dir,
        config,
        cfg,
        cfg_note,
        chosen,
        notices,
        bench,
        started,
        msaa,
        style_arg: args.style.clone(),
    };
    trace::mark("run_native");
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

/// Windows release builds have no console of their own: started from a terminal, they write
/// to its console, so the bench's and `--render`'s output, the usage and the warnings show
/// there. Started from Explorer there is none, and this does nothing. The terminal does not
/// wait for the app to exit: `start /wait` (cmd) or `Start-Process -Wait` (PowerShell) do.
#[cfg(all(windows, not(debug_assertions)))]
#[allow(unsafe_code)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    // SAFETY: GetStdHandle and AttachConsole take no pointers; a failure (no parent console)
    // changes nothing.
    unsafe {
        // Output already redirected to a file or pipe (`> log.txt` in a batch file): attaching
        // to the console would replace those handles and the log would stay empty.
        let redirected = |h| !GetStdHandle(h).is_null();
        if redirected(STD_OUTPUT_HANDLE) || redirected(STD_ERROR_HANDLE) {
            return;
        }
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_console() {}

fn render(args: &Args, style: style::StyleFile) -> ExitCode {
    let Some(pack) = &args.pack else {
        eprintln!("--render needs --pack\n{USAGE}");
        return ExitCode::from(2);
    };
    let Some(file) = args.file.as_ref().or(args.print.as_ref()) else {
        eprintln!("--render needs an output file, or --print\n{USAGE}");
        return ExitCode::from(2);
    };
    let job = match print_job(args) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
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
    let done = snap.and_then(|s| match &job {
        Some(job) => s.print(job),
        None => s.run(),
    });
    match done {
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

/// `--print`'s job, from its options.
fn print_job(args: &Args) -> Result<Option<snapshot::PrintJob>, String> {
    let Some(out) = args.print.clone() else {
        return Ok(None);
    };
    fn parse<T>(
        s: &Option<String>,
        what: &str,
        f: fn(&str) -> Option<T>,
    ) -> Result<Option<T>, String> {
        s.as_deref()
            .map(|s| f(s).ok_or_else(|| format!("unknown {what} {s}")))
            .transpose()
    }
    let dpi = match args.dpi.as_deref().map(str::parse::<f64>) {
        None => 300.0,
        Some(Ok(d)) if (72.0..=1200.0).contains(&d) => d,
        Some(_) => return Err("--dpi wants a number from 72 to 1200".into()),
    };
    let units = parse(&args.units, "units", |s| {
        scale::Units::ALL
            .into_iter()
            .find(|u| u.name().eq_ignore_ascii_case(s))
    })?;
    Ok(Some(snapshot::PrintJob {
        out,
        paper: parse(&args.paper, "paper", print::Paper::parse)?.unwrap_or_default(),
        orientation: parse(&args.orientation, "orientation", print::Orientation::parse)?
            .unwrap_or_default(),
        text: parse(&args.text_size, "text size", print::TextSize::parse)?.unwrap_or_default(),
        label: args.label.clone().unwrap_or_default(),
        directions: args.directions,
        dpi,
        units,
    }))
}

/// glibc gives every thread that allocates an arena of its own, and the loaders, the routing
/// and search workers and the print worker each keep what they freed. Two arenas hold the
/// same work in about 30 MiB less on the North America flight, with the same frame times
/// (phase5-plan.md 5.10). Called before any thread is started.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
fn cap_malloc_arenas() {
    // SAFETY: mallopt takes plain integers; a failure leaves glibc's default.
    unsafe { libc::mallopt(libc::M_ARENA_MAX, 2) };
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn cap_malloc_arenas() {}

/// Diagnostics for the memory checks on other machines (phase5-plan.md 5.10): OMP_PRESENT
/// (`fifo`, `mailbox`, `immediate`, `auto`) and OMP_LATENCY (frames the swapchain may queue)
/// replace the window's present mode and frame latency. Without them nothing changes.
fn surface_overrides(options: &mut eframe::NativeOptions) {
    use eframe::wgpu::PresentMode;
    let surface = &mut options.wgpu_options.surface;
    if let Ok(v) = std::env::var("OMP_PRESENT") {
        match v.to_ascii_lowercase().as_str() {
            "fifo" => surface.present_mode = PresentMode::Fifo,
            "mailbox" => surface.present_mode = PresentMode::Mailbox,
            "immediate" => surface.present_mode = PresentMode::Immediate,
            "auto" => surface.present_mode = PresentMode::AutoVsync,
            "autonovsync" => surface.present_mode = PresentMode::AutoNoVsync,
            other => eprintln!("OMP_PRESENT={other}: not one of fifo, mailbox, immediate, auto"),
        }
    }
    if let Some(n) = std::env::var("OMP_LATENCY")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        surface.desired_maximum_frame_latency = Some(n);
    }
}

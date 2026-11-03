//! Opening a pack: its style compiled, its loader threads, search and routing started, and
//! the view restored from the config or fitted to the pack.

use std::path::Path;
use std::sync::Arc;

use eframe::egui::{self, Rect};

use super::bench::set_stops;
use super::{App, CACHE_BUDGET, FrameView, MapState, TileStats};
use crate::cache::Cache;
use crate::camera::{Camera, Motion};
use crate::config;
use crate::here::HereUi;
use crate::labels::Placer;
use crate::layers::Hidden;
use crate::loader::{self, Loader};
use crate::mesh::{self, Prepared};
use crate::packs::{self, Opened};
use crate::pins;
use crate::pool::Pool;
use crate::routing::RouteUi;
use crate::search::SearchUi;
use crate::side::Page;
use crate::style::{Style, StyleFile};

impl App {
    pub(super) fn open(&mut self, ctx: &egui::Context, first_chunk: &Path) {
        // A pack that cannot be opened leaves the one open as it was.
        let opened = match packs::open(first_chunk) {
            Ok(o) => o,
            Err(e) => {
                self.pack_error = Some(e);
                return;
            }
        };
        // Close the old pack before starting the new one's threads, so its loader threads
        // and a check of it stop.
        self.map = None;
        self.verify = None;
        self.tile_error = None;
        let dir = packs::pack_dir(first_chunk);
        let same_pack = self
            .cfg
            .pack
            .as_ref()
            .is_some_and(|p| config::resolve_path(p, &self.exe_dir) == dir);
        let mut camera = Camera::new(opened.bounds);
        let mut fit = true;
        if let (true, true, Some(v)) = (same_pack, self.cfg.start.reopen_view, self.cfg.view) {
            camera.set_lonlat(v.lon, v.lat, v.zoom);
            fit = !camera.contains_center();
        }
        let Some(pool) = self.pool.clone() else {
            self.notices
                .push("No GPU device: the map cannot be drawn.".into());
            return;
        };
        let (style, warned) = compile_style(&self.style, &opened, &self.style_id);
        self.notices.extend(warned);
        let loader = start_loader(&opened, &style, pool, ctx);
        self.cfg.pack = Some(config::store_path(&dir, &self.exe_dir));
        let units = packs::default_units(opened.meta.as_ref().ok());
        let search = SearchUi::start(opened.first_chunk.clone(), &opened.schema, ctx);
        let mut routing = RouteUi::start(opened.first_chunk.clone(), ctx);
        routing.plan.s = self.cfg.schedule.clone();
        routing.plan.time = self.cfg.schedule.depart.clone();
        self.map = Some(MapState {
            opened,
            style,
            loader,
            cache: Cache::new(CACHE_BUDGET),
            camera,
            motion: Motion::default(),
            stats: TileStats::default(),
            fit_on_first_frame: fit,
            view: FrameView::default(),
            placer: Placer::default(),
            hidden: Hidden::default(),
            hidden_for: None,
            units,
            error_suggested: false,
            search,
            here: HereUi::default(),
            routing,
            page: Page::default(),
            last_rect: Rect::NOTHING,
            no_map: std::env::var_os("OMP_BENCH_NO_MAP").is_some(),
            last_frame: None,
        });
        self.first_full = None;
        if let (Some(b), Some(m)) = (&mut self.bench, &mut self.map) {
            // The panel as it first opens, whatever the config says.
            self.cfg.side_panel = config::SidePanel::default();
            let w = b.flight.window;
            m.camera.size = [f64::from(w[0]), f64::from(w[1])];
            m.fit_on_first_frame = false;
            b.start(&mut m.camera, m.opened.reader.bytes_read());
            // Checked when the flight was loaded.
            set_stops(&mut m.routing, &b.flight.route_stops().unwrap_or_default());
            if let Some(n) = b.flight.pins {
                self.pins.replace(vec![pins::bench_set(n, m.opened.bounds)]);
            }
            if let Some((date, time)) = b.flight.depart.as_deref().and_then(|d| d.split_once('T')) {
                m.routing.plan.s = config::Schedule::default();
                m.routing.plan.date = date.into();
                m.routing.plan.time = time.into();
                m.routing.plan.mode = crate::routing::Mode::Depart;
                m.routing.times_open = true;
                m.routing.retime();
            }
        }
    }
}

/// The style compiled for the map, and a notice for the user if it names things the map
/// does not have (the warnings also go to stderr).
pub(super) fn compile_style(
    file: &StyleFile,
    opened: &Opened,
    id: &str,
) -> (Arc<Style>, Option<String>) {
    let (style, warnings) = Style::compile(file, &opened.schema);
    for w in &warnings {
        eprintln!("{w}");
    }
    let notice = warnings_notice(id, &warnings);
    (Arc::new(style), notice)
}

/// What to tell the user about a style's warnings: the first few, and how many more.
pub(super) fn warnings_notice(id: &str, warnings: &[String]) -> Option<String> {
    const SHOWN: usize = 3;
    warnings.first()?;
    let mut s = format!("The style “{id}” does not fit this map:");
    for w in warnings.iter().take(SHOWN) {
        s += &format!("\n\u{2022} {}", w.trim_start_matches("style: "));
    }
    if warnings.len() > SHOWN {
        s += &format!("\n\u{2022} and {} more", warnings.len() - SHOWN);
    }
    s += "\nThe parts it names that the map lacks are left out.";
    Some(s)
}

/// Loader threads that read, tessellate with `style` and upload the pack's tiles.
pub(super) fn start_loader(
    opened: &Opened,
    style: &Arc<Style>,
    pool: Arc<Pool>,
    ctx: &egui::Context,
) -> Loader<Prepared> {
    let ctx = ctx.clone();
    let style = style.clone();
    Loader::new(
        opened.reader.clone(),
        loader::default_threads(),
        move |id, tile| mesh::prepare_tile(id, &tile, &style, &pool),
        move || ctx.request_repaint(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_warnings_become_one_notice_of_a_few_lines() {
        assert_eq!(warnings_notice("mine", &[]), None);
        let w: Vec<String> = (1..=5)
            .map(|i| format!("style: the map has no layer {i}"))
            .collect();
        let n = warnings_notice("mine", &w).unwrap();
        assert!(n.starts_with("The style “mine” does not fit this map:"));
        assert!(n.contains("\u{2022} the map has no layer 3") && !n.contains("layer 4"));
        assert!(n.contains("and 2 more"));
    }
}

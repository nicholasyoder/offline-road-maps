//! The side panel, on the left when open: the search box with the ☰ menu, the tabs, and the
//! page; and what is picked in it, done once it is drawn. Its own parts (pages, tabs, the
//! menu, the layers page) are in [`crate::side`]; the Trips page is [`crate::trips`].

use std::path::Path;

use eframe::egui::{self, Rect, Vec2};

use super::App;
use crate::bench::Bench;
use crate::camera::Camera;
use crate::config::Config;
use crate::here::HereUi;
use crate::keys::{self, Command};
use crate::packs::{self, Opened};
use crate::pins::{PinRef, PinsUi};
use crate::printui::{self, PrintUi};
use crate::routing::{Place, RouteUi, StopAction};
use crate::scale::Units;
use crate::search::{self, SearchUi, Status};
use crate::side::{self, Action, Page};
use crate::styles::Entry;
use crate::theme::{Glyph, glyph_button};
use crate::trips::{self, Browsed, Named, TripFile, TripsUi};

impl App {
    /// The side panel ([`side_panel`]), and what was picked in it.
    pub(super) fn side_panel(&mut self, ui: &mut egui::Ui) {
        let Some(m) = &mut self.map else { return };
        let parts = Parts {
            search: &mut m.search,
            here: &mut m.here,
            routing: &mut m.routing,
            page: &mut m.page,
            camera: &m.camera,
            pack_units: m.units,
            pins: &mut self.pins,
            trips: &mut self.trips,
            print: &mut self.print,
            exe_dir: &self.exe_dir,
            styles: &self.styles,
            style_id: &self.style_id,
        };
        let o = side_panel(ui, &mut self.cfg, parts, &mut self.bench);
        // A result picked: fly there. The bench records it, and jumps there itself.
        if let Some(b) = &mut self.bench {
            for t in &o.out.taken {
                let view = t.first.as_ref().map(|h| search::view_for(h, m.camera.size));
                b.search_answer(t, view);
            }
        } else if let Some(h) = o.out.fly {
            let (c, z) = search::view_for(&h, m.camera.size);
            m.motion.fly_to(&m.camera, c, z);
        }
        if let Some((a, h)) = o.out.route {
            m.routing.apply(a, Place::of_hit(&h));
        }
        if let Some((a, place)) = o.stop {
            m.routing.apply(a, place);
        }
        if let Some((c, z)) = o.fly {
            m.motion.fly_to(&m.camera, c, z);
        }
        // A pushpin added: from the map's menu, "what's here" or a picked result.
        if let Some((at, name)) = m.here.add_pin.take() {
            let name = name.unwrap_or_else(|| self.pins.next_name());
            self.pins.add(&name, omp_core::geo::from_unit(at));
            m.here.panel = None;
        }
        // A pushpin picked in search or on the Trips page: open it and fly there.
        if let Some(r) = o.pin.or(o.trips.show_pin) {
            self.pins.show(r);
            if let Some((c, z)) = self.pins.view_of(r, &m.camera) {
                m.motion.fly_to(&m.camera, c, z);
            }
        }
        if let Some(path) = o.trips.open {
            self.open_trip(&path);
        }
        let Some(m) = &mut self.map else { return };
        if let Some((from, to)) = o.trips.renamed
            && let Some(n) = m.routing.named.as_mut().filter(|n| n.path == from)
        {
            n.name = trips_name(&to);
            n.path = to;
        }
        if let Some(ask) = o.trips.save {
            let file = m.routing.to_file(&pack_label(&m.opened));
            if let Some(n) = self.trips.save(ask, file, m.routing.named.as_ref()) {
                m.routing.named = Some(n);
            }
        }
        let ctx = ui.ctx().clone();
        if let Some(id) = o.style {
            self.set_style(&ctx, &id);
        }
        self.act(&ctx, o.action);
    }

    /// Opens the trip in `path` in the route page.
    fn open_trip(&mut self, path: &Path) {
        let Some(m) = &mut self.map else { return };
        match TripFile::read(path) {
            Ok(file) => {
                let named = Named {
                    name: trips_name(path),
                    path: path.to_path_buf(),
                    file: file.clone(),
                };
                m.routing.open_file(&file, named);
                self.trips.error = None;
            }
            Err(e) => self.trips.error = Some(e),
        }
    }

    /// The save window and the file browser, for trips and pushpin sets.
    pub(super) fn trip_windows(&mut self, ctx: &egui::Context) {
        if let Some(n) = self.trips.save_window(ctx, &self.exe_dir)
            && let Some(m) = &mut self.map
        {
            m.routing.named = Some(n);
        }
        if let Some(gone) = self.trips.delete_window(ctx, &mut self.pins)
            && let Some(m) = &mut self.map
            && m.routing.named.as_ref().is_some_and(|n| n.path == gone)
        {
            m.routing.named = None;
        }
        self.pins.delete_window(ctx);
        match self.trips.browser_window(ctx) {
            Some(Browsed::Saved(n)) => {
                if let Some(m) = &mut self.map {
                    m.routing.named = Some(*n);
                }
            }
            Some(Browsed::OpenTrip(path)) => self.open_trip(&path),
            Some(Browsed::SavePins(set, path)) => self.pins.save_as(set, path),
            Some(Browsed::OpenPins(path)) => self.pins.open_file(path),
            None => {}
        }
    }

    pub(super) fn act(&mut self, ctx: &egui::Context, action: Option<Action>) {
        match action {
            Some(Action::Pick) => self.picker = true,
            Some(Action::Verify) => self.start_verify(ctx),
            Some(Action::About) => self.about = true,
            Some(Action::Shortcuts) => self.shortcuts = true,
            Some(Action::Settings) => self.settings = Some(Default::default()),
            Some(Action::Print) => {
                if let Some(m) = &mut self.map {
                    self.cfg.side_panel.open = true;
                    let mut parts = Parts {
                        search: &mut m.search,
                        here: &mut m.here,
                        routing: &mut m.routing,
                        page: &mut m.page,
                        camera: &m.camera,
                        pack_units: m.units,
                        pins: &mut self.pins,
                        trips: &mut self.trips,
                        print: &mut self.print,
                        exe_dir: &self.exe_dir,
                        styles: &self.styles,
                        style_id: &self.style_id,
                    };
                    open_print(ctx, &mut parts, &self.cfg.print, printui::Kind::Map);
                }
            }
            None => {}
        }
    }
}

/// "Ohio, built 2026-10-28": the pack a trip is saved on, for its file.
fn pack_label(opened: &Opened) -> String {
    let info = &opened.reader.pack().info;
    format!("{}, built {}", info.name, packs::date(info.build_time))
}

fn trips_name(path: &Path) -> String {
    crate::files::stem(path)
}

/// The parts of the open map that the side panel shows, and the app's trips and pushpins.
struct Parts<'a> {
    search: &'a mut SearchUi,
    here: &'a mut HereUi,
    routing: &'a mut RouteUi,
    page: &'a mut Page,
    camera: &'a Camera,
    /// The units the pack suggests.
    pack_units: Units,
    pins: &'a mut PinsUi,
    trips: &'a mut TripsUi,
    print: &'a mut PrintUi,
    exe_dir: &'a Path,
    /// The styles the Map tab lists, and the one in use.
    styles: &'a [Entry],
    style_id: &'a str,
}

/// What was picked in the side panel, done once it is drawn.
struct SideOut {
    out: search::Out,
    /// A route button of "what's here" or the picked result.
    stop: Option<(StopAction, Place)>,
    /// A step of the directions: its view.
    fly: Option<(crate::camera::Vec2, f64)>,
    action: Option<Action>,
    /// The id of a style clicked in the Map tab.
    style: Option<String>,
    /// A pushpin found by search, clicked.
    pin: Option<PinRef>,
    /// What the Trips page asked for.
    trips: trips::PageOut,
    /// Where the tabs and the button that hides the panel were drawn, for the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    tabs: [Rect; 5],
    #[cfg_attr(not(test), allow(dead_code))]
    hide: Rect,
}

/// The side panel, on the left when open: the search box with the ☰ menu, the tabs, and the
/// page. The keys and the answers of its parts are handled whether it is open or not: Ctrl+B
/// opens and closes it, Ctrl+F and "/" open it on the search box, "what's here" opens it on
/// the search page and a stop set on the route page, and Escape steps back in the page once
/// the search box has let go of it.
fn side_panel(
    ui: &mut egui::Ui,
    cfg: &mut Config,
    mut p: Parts<'_>,
    bench: &mut Option<Bench>,
) -> SideOut {
    let ctx = ui.ctx().clone();
    let mut o = SideOut {
        out: search::Out::default(),
        stop: None,
        fly: None,
        action: None,
        style: None,
        pin: None,
        trips: trips::PageOut::default(),
        tabs: [Rect::NOTHING; 5],
        hide: Rect::NOTHING,
    };
    let units = cfg.units.unwrap_or(p.pack_units);
    let recent = &mut cfg.recent_searches;
    let side = &mut cfg.side_panel;
    if keys::take(&ctx, Command::Panel) {
        side.open = !side.open;
    }
    if keys::take(&ctx, Command::Print) {
        side.open = true;
        open_print(&ctx, &mut p, &cfg.print, printui::Kind::Map);
    }
    o.out = p.search.poll(recent);
    if p.search.shortcut(&ctx) {
        side.open = true;
        *p.page = Page::Search;
    }
    if !p.search.box_was_focused() {
        match p.page {
            Page::Search => {
                let _ = p.pins.escape(&ctx) || p.here.escape(&ctx) || p.search.escape(&ctx);
            }
            Page::Route => {
                p.routing.escape(&ctx);
            }
            Page::Trips | Page::Map => {}
            Page::Print => {
                if p.print.escape(&ctx) {
                    *p.page = p.print.back;
                }
            }
        }
    }
    // The bench asks "what's here" at the view's centre, as the right-click menu would, and
    // types into the box as a user would.
    let available = matches!(p.search.status(), Status::Opening | Status::Ready);
    if bench.as_mut().is_some_and(|b| b.here_ask(available)) {
        p.here.ask(p.search, p.camera.center, p.camera);
    }
    // A result picked opens "what's here" at it, with the route buttons. Not in the bench,
    // so its searches are timed as before.
    if bench.is_none() && p.here.follow(p.search, p.camera) {
        ctx.request_repaint();
    }
    for t in p.here.take(p.search) {
        if let Some(b) = bench.as_mut() {
            b.here_answer(&t);
        }
    }
    for (at, address) in p.here.take_stops() {
        p.routing.name_stops_at(at, &address);
    }
    let key = bench.as_mut().and_then(|b| b.search_key(available));
    if std::mem::take(&mut p.here.asked) || key.is_some() {
        side.open = true;
        *p.page = Page::Search;
    }
    if std::mem::take(&mut p.routing.opened) {
        side.open = true;
        *p.page = Page::Route;
    }
    if std::mem::take(&mut p.pins.opened) {
        side.open = true;
        *p.page = Page::Search;
    }
    if let Some(page) = bench.as_mut().and_then(Bench::page_shot) {
        *p.page = page;
        p.search.hide(&ctx);
    }
    p.search.route_menu = p.routing.menu_state();
    if !side.open {
        p.search.hide(&ctx);
        return o;
    }

    let range = side::width_range(ctx.content_rect().width());
    // The config holds the width, not egui: a window made narrow for a while does not keep
    // the panel narrow.
    let id = ui.scope_id().with("side-panel");
    ui.data_mut(|d| d.remove::<egui::containers::panel::PanelState>(id));
    let asked = side.width.clamp(range.min, range.max);
    let frame = egui::Frame::side_top_panel(ui.style()).inner_margin(8);
    let (debug, layers) = (&mut cfg.debug_overlay, &mut cfg.layers);
    let schedule = &mut cfg.schedule;
    let print_cfg = &mut cfg.print;
    let r = egui::Panel::left("side-panel")
        .resizable(true)
        .default_size(asked)
        .size_range(range)
        .frame(frame)
        .show(ui, |ui| {
            let had_focus = p.search.box_was_focused();
            // From the right: hide, the menu, and the box in what is left.
            let row = egui::Layout::right_to_left(egui::Align::Center);
            let size = Vec2::new(ui.available_width(), crate::theme::HEADER_H);
            ui.allocate_ui_with_layout(size, row, |ui| {
                let hide = glyph_button(ui, Glyph::Hide, crate::theme::HEADER_H)
                    .on_hover_text(keys::with_hint("Hide the panel", Command::Panel));
                o.hide = hide.rect;
                if hide.clicked() {
                    side.open = false;
                }
                o.action = side::menu(ui, debug);
                p.search.box_ui(ui, p.camera, recent, key, &mut o.out);
            });
            // A click in the box shows the search page.
            if p.search.focused(&ctx) && !had_focus {
                *p.page = Page::Search;
            }
            // More room above the tabs than below them.
            ui.add_space(5.0);
            let before = *p.page;
            o.tabs = side::tabs(ui, p.page);
            // The Print tab opens the print page as the menu does.
            if *p.page == Page::Print && before != Page::Print {
                *p.page = before;
                open_print(&ctx, &mut p, print_cfg, printui::Kind::Map);
            }
            ui.add(egui::Separator::default().spacing(2.0));
            ui.add_space(3.0);
            match p.page {
                Page::Search => {
                    o.pin = p.pins.found_ui(ui, &p.search.text);
                    if !p
                        .search
                        .suggestions_ui(ui, p.camera, units, recent, &mut o.out)
                    {
                        o.stop = search_page(ui, &mut p, units, &mut o.out);
                    }
                }
                Page::Route => {
                    o.fly = p.routing.page_ui(ui, p.camera, units);
                    // The settings last used are the next trip's.
                    if *schedule != p.routing.plan.s {
                        *schedule = p.routing.plan.s.clone();
                    }
                }
                Page::Trips => {
                    let can_open = p.routing.status().usable();
                    let current = trips::Current {
                        can_save: p.routing.from.is_some() && p.routing.to.is_some(),
                        named: p.routing.named.as_ref(),
                        unsaved: p.routing.unsaved(),
                    };
                    o.trips = p.trips.page_ui(ui, p.pins, current, can_open, p.exe_dir);
                }
                Page::Map => o.style = side::map_ui(ui, layers, p.styles, p.style_id),
                Page::Print => {
                    let around = printui::Around {
                        cam: p.camera,
                        has_route: p.routing.route.is_some(),
                        route_bounds: p.routing.route_bounds(),
                        styles: p.styles,
                        map_style: p.style_id,
                    };
                    p.print.page_ui(ui, print_cfg, &around);
                }
            }
        });
    let w = r.response.rect.width();
    if (w - asked).abs() > 0.5 {
        side.width = w.round();
    }
    o
}

/// Opens the print page on `kind`, to go back to the page it was opened from.
fn open_print(
    ctx: &egui::Context,
    p: &mut Parts<'_>,
    settings: &crate::print::Settings,
    kind: printui::Kind,
) {
    if *p.page != Page::Print {
        p.print.back = *p.page;
    }
    *p.page = Page::Print;
    let route = p.routing.route_bounds();
    p.print.open(ctx, kind, p.camera, route, settings);
}

/// The search page under the suggestions: "what's here" for a right-clicked point or the
/// picked result, then the results of Enter; a few words on what to do when there is
/// neither. Returns a route button clicked, with its stop.
fn search_page(
    ui: &mut egui::Ui,
    p: &mut Parts<'_>,
    units: Units,
    out: &mut search::Out,
) -> Option<(StopAction, Place)> {
    let menu = p.routing.menu_state();
    let pin_stop = p.pins.panel_ui(ui, &menu);
    if p.pins.open.is_some() && p.here.panel.is_some() {
        ui.separator();
    }
    let here = p.here.panel.is_some() || p.pins.open.is_some();
    let stop = p
        .here
        .page_ui(ui, units, p.search.icons(), &menu)
        .or(pin_stop);
    if !p.search.has_results() {
        if !here {
            let weak = |t: &str| egui::RichText::new(t).color(crate::theme::weak());
            ui.label(weak("Type a place, an address, a postcode or a business."));
            ui.add_space(4.0);
            ui.label(weak(
                "Right-click the map for what is there, to route from or to a point, or to add a pushpin.",
            ));
        }
        return stop;
    }
    if here {
        ui.separator();
    }
    p.search.results_ui(ui, p.camera, units, out);
    stop
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;

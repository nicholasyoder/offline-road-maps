//! The side panel opened, closed, resized and switched between pages with keys and clicks,
//! headless, with the map beside it as the window draws it.

use std::time::{Duration, Instant};

use eframe::egui::{Event, Key, Modifiers, PointerButton};
use omp_core::find::{Hit, HitKind};
use omp_core::geo;

use super::*;
use crate::config::SidePanel;

struct Window {
    ctx: egui::Context,
    cfg: Config,
    search: SearchUi,
    here: HereUi,
    routing: RouteUi,
    page: Page,
    cam: Camera,
    size: Vec2,
    /// The map's rectangle this frame and the one before.
    map: Rect,
    tabs: [Rect; 4],
    hide: Rect,
}

impl Window {
    fn new() -> Self {
        let (search, _) = SearchUi::stub();
        Self::with(egui::Context::default(), search, RouteUi::stub())
    }

    fn with(ctx: egui::Context, search: SearchUi, routing: RouteUi) -> Self {
        let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
        cam.zoom = 10.0;
        Self {
            ctx,
            cfg: Config::default(),
            search,
            here: HereUi::default(),
            routing,
            page: Page::default(),
            cam,
            size: Vec2::new(1200.0, 800.0),
            map: Rect::NOTHING,
            tabs: [Rect::NOTHING; 4],
            hide: Rect::NOTHING,
        }
    }

    fn frame(&mut self, events: Vec<Event>) {
        let raw = egui::RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            focused: true,
            ..Default::default()
        };
        let mut full = self.ctx.clone().run_ui(raw, |ui| {
            let parts = Parts {
                search: &mut self.search,
                here: &mut self.here,
                routing: &mut self.routing,
                page: &mut self.page,
                camera: &self.cam,
                pack_units: Units::Kilometres,
            };
            let o = side_panel(ui, &mut self.cfg, parts, &mut None);
            (self.tabs, self.hide) = (o.tabs, o.hide);
            // As the app does with what was picked.
            if let Some((a, h)) = o.out.route {
                self.routing.apply(a, Place::of_hit(&h));
            }
            if let Some((a, place)) = o.stop {
                self.routing.apply(a, place);
            }
            let _ = self.routing.take(&self.cam);
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let rect = ui.max_rect();
                    self.cam.size = [f64::from(rect.width()), f64::from(rect.height())];
                    keep_still(&mut self.cam, self.map, rect);
                    self.map = rect;
                });
        });
        full.textures_delta.clear();
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]);
        self.frame(Vec::new());
    }

    /// Clicks route button `i` of "what's here" or the picked result.
    fn stop_button(&mut self, i: usize) {
        let b = self.here.buttons.expect("route buttons drawn");
        self.click(b[i].center());
    }

    fn click(&mut self, at: Pos2) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![button(at, true)]);
        self.frame(vec![button(at, false)]);
        self.frame(Vec::new());
    }

    /// The world point at `x` points from the window's left, on the map's middle line.
    fn world_at(&self, x: f32) -> [f64; 2] {
        let y = self.cam.size[1] / 2.0;
        self.cam
            .screen_to_world([f64::from(x - self.map.left()), y])
    }
}

fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn same(a: [f64; 2], b: [f64; 2]) -> bool {
    // A fraction of a point at z10.
    (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
}

#[test]
fn open_close_resize_and_pages() {
    let mut w = Window::new();
    w.frame(Vec::new());
    w.frame(Vec::new());
    // Open at 360 points, on the search page; the map starts at its edge.
    assert_eq!(w.cfg.side_panel, SidePanel::default());
    assert!((w.map.left() - 360.0).abs() < 1.0, "{:?}", w.map);
    assert_eq!(w.page, Page::Search);
    let still = w.world_at(800.0);

    // Ctrl+B closes it: the map takes the window and does not move.
    w.key(Key::B, Modifiers::COMMAND);
    assert!(!w.cfg.side_panel.open);
    assert_eq!(w.map.left(), 0.0);
    assert!(same(w.world_at(800.0), still));

    // Ctrl+F opens it on the search page with the box focused.
    w.page = Page::Layers;
    w.key(Key::F, Modifiers::COMMAND);
    assert!(w.cfg.side_panel.open && w.page == Page::Search);
    assert!(w.search.focused(&w.ctx));
    assert!(same(w.world_at(800.0), still));
    // Escape lets go of the box; the panel stays.
    w.key(Key::Escape, Modifiers::NONE);
    assert!(!w.search.focused(&w.ctx) && w.cfg.side_panel.open);

    // The tabs switch pages.
    w.click(w.tabs[3].center());
    assert_eq!(w.page, Page::Layers);
    w.click(w.tabs[2].center());
    assert_eq!(w.page, Page::Trips);

    // A stop set shows the route page, "what's here" the search page.
    let at = w.cam.center;
    w.routing
        .apply(crate::routing::StopAction::From, Place::at(at));
    w.frame(Vec::new());
    assert_eq!(w.page, Page::Route);
    w.here.ask(&w.search, at, &w.cam);
    w.frame(Vec::new());
    assert_eq!(w.page, Page::Search);

    // Drag the panel's edge 60 points right: it is wider, saved, and the map stays still.
    let edge = Pos2::new(w.map.left() - 1.0, 500.0);
    w.frame(vec![Event::PointerMoved(edge)]);
    w.frame(vec![button(edge, true)]);
    for k in 1..=6 {
        w.frame(vec![Event::PointerMoved(
            edge + Vec2::new(10.0 * k as f32, 0.0),
        )]);
    }
    w.frame(vec![button(edge + Vec2::new(60.0, 0.0), false)]);
    w.frame(Vec::new());
    let width = w.cfg.side_panel.width;
    assert!((width - 420.0).abs() < 2.0, "{width}");
    assert!((w.map.left() - width).abs() < 1.0);
    assert!(same(w.world_at(800.0), still));

    // A narrow window: at most 45% of it, and the saved width stays for when it widens.
    w.size = Vec2::new(700.0, 800.0);
    w.frame(Vec::new());
    w.frame(Vec::new());
    assert!((w.map.left() - 315.0).abs() < 1.0, "{:?}", w.map);
    assert_eq!(w.cfg.side_panel.width, width);
    w.size = Vec2::new(1200.0, 800.0);
    w.frame(Vec::new());
    assert!((w.map.left() - width).abs() < 1.0);

    // « hides it.
    w.click(w.hide.center());
    assert!(!w.cfg.side_panel.open);
    assert_eq!(w.map.left(), 0.0);
}

fn hit(name: &str, w: [f64; 2], street: Option<u32>) -> Hit {
    Hit {
        kind: HitKind::Address,
        class: 0,
        name: name.into(),
        context: "Columbus, Ohio".into(),
        p: geo::from_unit(w),
        radius_m: 50.0,
        approx: false,
        number_missing: false,
        postcode: None,
        units: Vec::new(),
        entry: None,
        street,
        score: 1.0,
        exact: true,
    }
}

#[test]
fn picked_result_and_whats_here_set_stops() {
    let mut w = Window::new();
    w.frame(Vec::new());
    let c = w.cam.center;
    // A result picked opens "what's here" at it on the search page, under its name.
    let a = hit("1 Capitol Square", c, Some(7));
    w.search.session.marker = Some(a.clone());
    w.page = Page::Layers;
    w.frame(Vec::new());
    w.frame(Vec::new());
    assert_eq!(w.page, Page::Search);
    assert_eq!(w.here.panel.as_ref().unwrap().hit.as_ref(), Some(&a));

    // "Route from here": the stop is the result, with its own street; the route page shows.
    w.stop_button(0);
    assert_eq!(w.routing.from, Some(Place::of_hit(&a)));
    assert_eq!(w.page, Page::Route);
    // Back on the search page, "Route to here" for another result.
    w.click(w.tabs[0].center());
    let b = hit("2 Broad Street", [c[0] + 1e-4, c[1]], Some(9));
    w.search.session.marker = Some(b.clone());
    w.frame(Vec::new());
    w.frame(Vec::new());
    w.click(w.tabs[0].center());
    w.stop_button(1);
    assert_eq!(w.routing.to, Some(Place::of_hit(&b)));

    // A right-clicked point's "what's here": "Add stop" puts the point between.
    let p = [c[0], c[1] + 1e-4];
    w.here.ask(&w.search, p, &w.cam);
    w.frame(Vec::new());
    assert!(w.here.panel.as_ref().unwrap().hit.is_none());
    w.stop_button(2);
    assert_eq!(w.routing.via, vec![Place::at(p)]);
    assert!(w.routing.waiting.is_some(), "asks for the route");

    // The marker cleared leaves a point's panel; it closes the result's own.
    w.click(w.tabs[0].center());
    w.search.session.marker = None;
    w.frame(Vec::new());
    assert!(w.here.panel.is_some());
    w.search.session.marker = Some(a.clone());
    w.frame(Vec::new());
    assert!(w.here.panel.as_ref().unwrap().hit.is_some());
    w.search.session.marker = None;
    w.frame(Vec::new());
    assert!(w.here.panel.is_none());
    // Escape closes the result's panel and keeps the pin; the panel does not come back.
    w.search.session.marker = Some(b);
    w.frame(Vec::new());
    w.key(Key::Escape, Modifiers::NONE);
    assert!(w.here.panel.is_none() && w.search.session.marker.is_some());
    w.frame(Vec::new());
    assert!(w.here.panel.is_none());
}

/// Step 3.10's check on a real pack (the North America pack in the phase 3 plan):
/// `OMP_TEST_PACK=<first chunk> cargo test -p omp-app --release -- --ignored real_pack`.
/// A search typed and its first suggestion picked, "what's here" for it and for a point,
/// each made a stop with their buttons, and the route between them.
#[test]
#[ignore = "needs OMP_TEST_PACK, a pack with search and routing"]
fn real_pack_result_and_point_route() {
    let pack = std::path::PathBuf::from(std::env::var_os("OMP_TEST_PACK").expect("OMP_TEST_PACK"));
    let opened = crate::packs::open(&pack).unwrap();
    let ctx = egui::Context::default();
    let search = SearchUi::start(opened.first_chunk.clone(), &opened.schema, &ctx);
    let routing = RouteUi::start(opened.first_chunk.clone(), &ctx);
    let mut w = Window::with(ctx, search, routing);
    // Over Ohio, as search ranks by distance from the view.
    w.cam.center = geo::lonlat_to_unit(-82.9988, 39.9612);
    w.cam.zoom = 8.0;
    let until = |w: &mut Window, what: &str, done: &dyn Fn(&Window) -> bool| {
        let start = Instant::now();
        while !done(w) {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "waited for {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
            w.frame(Vec::new());
        }
        eprintln!("{what}: {:.0} ms", start.elapsed().as_secs_f64() * 1e3);
    };
    until(&mut w, "search and routing open", &|w| {
        matches!(w.search.status(), Status::Ready)
            && matches!(w.routing.status(), crate::routing::Status::Ready { .. })
    });

    // Typed, then the first suggestion picked with the arrow keys and Enter.
    w.key(Key::F, Modifiers::COMMAND);
    w.frame(vec![Event::Text("50 West Broad Street, Columbus".into())]);
    until(&mut w, "suggestions", &|w| {
        !w.search.session.hits.is_empty() && !w.search.session.searching()
    });
    let first = w.search.session.hits[0].clone();
    eprintln!(
        "picked: {} — {} (street {:?})",
        first.name, first.context, first.street
    );
    assert!(first.street.is_some(), "an address has its street");
    w.key(Key::ArrowDown, Modifiers::NONE);
    w.key(Key::Enter, Modifiers::NONE);
    assert_eq!(w.search.session.marker.as_ref(), Some(&first));
    until(&mut w, "what's here at the result", &|w| {
        w.here.panel.as_ref().is_some_and(|p| p.answer.is_some())
    });
    let panel = w.here.panel.as_ref().unwrap();
    assert_eq!(panel.hit.as_ref(), Some(&first));
    let shown = panel.answer.clone().unwrap().unwrap();
    eprintln!("its areas: {}", shown.areas.join(", "));
    assert!(shown.areas.iter().any(|a| a == "Ohio"), "{:?}", shown.areas);
    w.stop_button(0);
    assert_eq!(w.routing.from, Some(Place::of_hit(&first)));

    // A point in Cleveland, right-clicked for "what's here", made the destination.
    w.click(w.tabs[0].center());
    w.here
        .ask(&w.search, geo::lonlat_to_unit(-81.6944, 41.4993), &w.cam);
    until(&mut w, "what's here at the point", &|w| {
        w.here.panel.as_ref().is_some_and(|p| p.answer.is_some())
    });
    w.stop_button(1);
    assert!(w.routing.to.as_ref().is_some_and(|p| p.street.is_none()));
    until(&mut w, "the route", &|w| w.routing.route.is_some());
    let trip = &w.routing.route.as_ref().unwrap().trip;
    let km = trip.metres() / 1000.0;
    eprintln!("route: {km:.1} km, {:.0} min", trip.time_s() / 60.0);
    assert!(trip.complete() && (180.0..260.0).contains(&km), "{km} km");
}

//! The side panel opened, closed, resized and switched between pages with keys and clicks,
//! headless, with the map beside it as the window draws it.

use eframe::egui::{Event, Key, Modifiers, PointerButton};

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
        let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
        cam.zoom = 10.0;
        Self {
            ctx: egui::Context::default(),
            cfg: Config::default(),
            search,
            here: HereUi::default(),
            routing: RouteUi::stub(),
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

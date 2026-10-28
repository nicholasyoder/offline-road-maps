//! The route page, clicked and dragged as a user would, on a worker with no thread.

use std::sync::mpsc::Sender;

use eframe::egui::{Event, Key, PointerButton};

use super::*;
use crate::routing::tests::answer;
use crate::routing::worker::{Answer, Worker};
use crate::routing::{Look, Place};

struct Ui {
    ctx: egui::Context,
    r: RouteUi,
    tx: Sender<Answer>,
    cam: Camera,
    rect: Rect,
    fly: Option<(camera::Vec2, f64)>,
}

impl Ui {
    fn new() -> Self {
        let (worker, tx) = Worker::stub();
        let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
        cam.size = [1000.0, 800.0];
        Self {
            ctx: egui::Context::default(),
            r: RouteUi::with_worker(worker),
            tx,
            cam,
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0)),
            fly: None,
        }
    }

    fn frame(&mut self, events: Vec<Event>) {
        let raw = egui::RawInput {
            events,
            screen_rect: Some(self.rect),
            focused: true,
            ..Default::default()
        };
        // In a panel as wide as the side panel's default.
        let mut full = self.ctx.clone().run_ui(raw, |ui| {
            self.r.escape(ui.ctx());
            self.r.take(&self.cam);
            egui::Panel::left("side").exact_size(360.0).show(ui, |ui| {
                self.fly = self.r.page_ui(ui, &self.cam, Units::Kilometres);
            });
        });
        full.textures_delta.clear();
    }

    fn at(&self, t: Target) -> Pos2 {
        let r = self.r.targets.iter().find(|(k, _)| *k == t);
        r.unwrap_or_else(|| panic!("{t:?} not drawn")).1.center()
    }

    fn click(&mut self, t: Target) {
        let pos = self.at(t);
        self.frame(vec![Event::PointerMoved(pos)]);
        self.frame(vec![button(pos, true)]);
        self.frame(vec![button(pos, false)]);
    }

    /// Answers the request waiting, and draws the answer.
    fn answer(&mut self) -> usize {
        let req = self.r.worker.take_request().expect("a request");
        let n = req.stops.len();
        self.tx.send(answer(&req)).unwrap();
        // The panel grows to fit the answer on the next frame.
        self.frame(Vec::new());
        self.frame(Vec::new());
        n
    }

    fn names(&self) -> String {
        self.r
            .stops()
            .iter()
            .map(|(_, p)| p.name.clone().unwrap())
            .collect()
    }
}

fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn named(n: &str, lat: f64) -> Place {
    Place {
        name: Some(n.into()),
        ..Place::at(geo::lonlat_to_unit(-83.0, lat))
    }
}

#[test]
fn stop_list() {
    let mut u = Ui::new();
    // No stops: the options only.
    u.frame(Vec::new());
    assert!(
        u.r.targets
            .iter()
            .all(|t| matches!(t.0, Target::Reverse | Target::Avoid(_)))
    );

    u.r.apply(StopAction::From, named("a", 40.0));
    u.r.apply(StopAction::Add, named("b", 40.2));
    u.r.apply(StopAction::To, named("c", 40.4));
    u.frame(Vec::new());
    assert_eq!(u.answer(), 3);
    // Two legs: six steps listed.
    assert_eq!(
        u.r.targets
            .iter()
            .filter(|t| matches!(t.0, Target::Step(_)))
            .count(),
        6
    );

    // Drag the destination's handle onto the first row: it starts the route.
    let from = u.at(Target::Handle(2));
    let to = u.at(Target::Row(0));
    u.frame(vec![Event::PointerMoved(from)]);
    u.frame(vec![button(from, true)]);
    for k in 1..=6 {
        let p = from + (to - from) * (k as f32 / 6.0);
        u.frame(vec![Event::PointerMoved(p)]);
    }
    u.frame(vec![button(to, false)]);
    assert_eq!(u.names(), "cab");
    assert_eq!(u.answer(), 3);

    // Remove the stop between; reverse; avoid ferries.
    u.click(Target::Remove(1));
    assert_eq!(u.names(), "cb");
    u.answer();
    u.click(Target::Reverse);
    assert_eq!(u.names(), "bc");
    u.answer();
    u.click(Target::Avoid(FERRY));
    assert_eq!(u.r.avoid, FERRY);
    u.answer();

    // A step clicked is drawn over the route, and the view flies to it.
    u.click(Target::Step(1));
    assert_eq!(u.r.picked, Some(1));
    assert!(u.r.overlays.iter().any(|o| o.0 == Look::Picked));
    let (c, _) = u.fly.expect("fly to the step");
    let lat = geo::unit_to_lonlat(c)[1];
    assert!(lat > 40.2 && lat < 40.4, "{lat}");

    // Escape lets go of the step and keeps the route; Clear removes the stops and the route.
    u.frame(vec![Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    assert!(u.r.picked.is_none() && u.r.route.is_some());
    assert!(!u.r.overlays.iter().any(|o| o.0 == Look::Picked));
    u.click(Target::Clear);
    assert!(!u.r.has_stops() && u.r.route.is_none());
}

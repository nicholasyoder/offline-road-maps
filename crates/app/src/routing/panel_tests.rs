//! The route page, clicked and dragged as a user would, on a worker with no thread.

use std::sync::mpsc::Sender;

use eframe::egui::{Event, Key, PointerButton};

use super::*;
use crate::routing::tests::answer;
use crate::routing::worker::{Reply, Request, Worker};
use crate::routing::{Look, Place};

struct Ui {
    ctx: egui::Context,
    r: RouteUi,
    tx: Sender<Reply>,
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
        self.tx.send(answer(&req).into()).unwrap();
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
    assert!(u.r.targets.iter().all(|t| matches!(
        t.0,
        Target::Reverse
            | Target::Avoid(_)
            | Target::Optimize
            | Target::KeepFirst
            | Target::KeepLast
            | Target::RoundTrip
    )));

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

impl Ui {
    /// Answers the optimize request waiting with `order` (or `Err`): the trip routed in
    /// that order, each leg a minute and 500 m shorter than [`answer`]'s.
    fn optimized(&mut self, order: Result<&[usize], &str>) -> Request {
        let mut req = self.r.worker.take_request().expect("a request");
        let ends = req.optimize.expect("an optimize request");
        let mut a = match order {
            Ok(order) => {
                let mut stops: Vec<_> = order.iter().map(|&i| req.stops[i]).collect();
                if ends.round_trip {
                    stops.push(stops[0]);
                }
                let asked = req.stops.clone();
                req.stops = stops;
                let mut a = answer(&req);
                if let Ok(r) = &mut a.routed {
                    for l in r.trip.legs.iter_mut().flatten() {
                        l.time_s -= 60.0;
                        l.metres -= 500.0;
                    }
                }
                a.order = Some(Ok(order.to_vec()));
                req.stops = asked;
                a
            }
            Err(e) => {
                let mut a = answer(&req);
                a.routed = Err(e.into());
                a.order = Some(Err(e.into()));
                a
            }
        };
        a.optimize = std::time::Duration::from_millis(3);
        self.tx.send(a.into()).unwrap();
        self.frame(Vec::new());
        self.frame(Vec::new());
        req
    }

    fn drawn(&self, t: Target) -> bool {
        self.r.targets.iter().any(|(k, _)| *k == t)
    }
}

#[test]
fn optimize_stop_order() {
    use crate::routing::Reordered;
    let mut u = Ui::new();
    u.r.apply(StopAction::From, named("a", 40.0));
    u.r.apply(StopAction::Add, named("b", 40.4));
    u.r.apply(StopAction::Add, named("c", 40.2));
    u.r.apply(StopAction::To, named("d", 40.6));
    u.frame(Vec::new());
    // Not before the route is shown.
    assert!(!u.r.can_optimize());
    u.answer();
    assert!(u.r.can_optimize());

    // Keeping both ends by default: b and c swap, and it says what that saved.
    u.click(Target::Optimize);
    u.frame(Vec::new());
    assert!(u.drawn(Target::CancelOptimize) && !u.drawn(Target::Optimize));
    let req = u.optimized(Ok(&[0, 2, 1, 3]));
    let ends = req.optimize.unwrap();
    assert!(ends.keep_first && ends.keep_last && !ends.round_trip);
    assert_eq!(req.stops.len(), 4);
    assert_eq!(u.names(), "acbd");
    let kinds: Vec<StopAction> = u.r.stops().iter().map(|s| s.0).collect();
    use StopAction::{Add, From, To};
    assert_eq!(kinds, [From, Add, Add, To]);
    match &u.r.reordered {
        Some(Reordered::Saved { time_s, metres, .. }) => {
            assert_eq!((*time_s, *metres), (180.0, 1500.0));
        }
        r => panic!("{r:?}"),
    }
    assert!(u.r.route.is_some() && u.r.waiting.is_none());
    assert!(u.r.overlay_lines()[1].contains("order 3.0 ms"));

    // Undo puts the old order back and routes it.
    u.click(Target::Undo);
    assert_eq!(u.names(), "abcd");
    assert!(u.r.reordered.is_none());
    assert_eq!(u.answer(), 4);

    // Cancelled while it runs: the route shown stays, and the button is back.
    u.click(Target::Optimize);
    u.frame(Vec::new());
    u.click(Target::CancelOptimize);
    u.frame(Vec::new());
    assert!(u.r.waiting.is_none() && u.r.route.is_some());
    assert!(u.drawn(Target::Optimize));
    assert_eq!(u.names(), "abcd");

    // Already the quickest: no Undo. A stop changed lets go of the note.
    u.click(Target::Optimize);
    u.optimized(Ok(&[0, 1, 2, 3]));
    assert!(matches!(u.r.reordered, Some(Reordered::Already)));
    assert!(!u.drawn(Target::Undo));
    assert!(u.drawn(Target::Optimize));
    u.click(Target::Reverse);
    assert!(u.r.reordered.is_none());
    u.answer();
    assert_eq!(u.names(), "dcba");

    // A round trip: from the first, every other stop reordered, and back.
    u.click(Target::RoundTrip);
    assert!(u.r.ends.round_trip);
    u.frame(Vec::new());
    u.click(Target::Optimize);
    let req = u.optimized(Ok(&[0, 3, 1, 2]));
    assert!(req.optimize.unwrap().round_trip);
    assert_eq!(u.names(), "dacbd");
    assert_eq!(u.r.route.as_ref().unwrap().trip.snaps.len(), 5);
    // Back at the start already: optimized again without the return, which comes back.
    u.click(Target::Optimize);
    let req = u.optimized(Ok(&[0, 2, 1, 3]));
    assert_eq!(req.stops.len(), 4);
    assert_eq!(u.names(), "dcabd");

    // A failure says so and keeps the route.
    u.click(Target::Optimize);
    u.optimized(Err("stop 2 has no road within reach"));
    assert!(matches!(&u.r.reordered, Some(Reordered::Failed(e)) if e.contains("stop 2")));
    assert!(u.r.route.is_some());
    assert_eq!(u.names(), "dcabd");
}

#[test]
fn alternatives_from_the_list() {
    use crate::routing::LegAlts;
    use crate::routing::tests::alternatives;
    let mut u = Ui::new();
    u.r.apply(StopAction::From, named("a", 40.0));
    u.r.apply(StopAction::Add, named("b", 40.2));
    u.r.apply(StopAction::To, named("c", 40.4));
    u.frame(Vec::new());
    let req = u.r.worker.take_request().unwrap();
    let a = answer(&req);
    let alts = alternatives(&a, &[]);
    u.tx.send(a.into()).unwrap();
    u.frame(Vec::new());
    u.frame(Vec::new());
    // A row per leg, closed: nothing asked, nothing to pick.
    assert!(u.drawn(Target::Leg(0)) && u.drawn(Target::Leg(1)));
    assert!(!u.drawn(Target::Alternative(1, 0)));
    assert!(u.r.worker.take_alternatives().is_none());
    let steps = |u: &Ui| {
        u.r.targets
            .iter()
            .filter(|t| matches!(t.0, Target::Step(_)))
            .count()
    };
    assert_eq!(steps(&u), 6);

    // Opening the second leg's row looks for its routes, then lists them.
    u.click(Target::Leg(1));
    assert_eq!(u.r.expanded, [false, true]);
    assert_eq!(u.r.worker.take_alternatives(), Some((req.id, vec![1])));
    u.frame(Vec::new());
    assert!(u.r.looking() && !u.drawn(Target::Alternative(1, 0)));
    for x in alts {
        u.tx.send(x).unwrap();
    }
    u.frame(Vec::new());
    u.frame(Vec::new());
    // The first leg's was not asked for: dropped.
    assert!(matches!(u.r.alts[0], LegAlts::NotAsked));
    assert!(u.drawn(Target::Alternative(1, 0)) && u.drawn(Target::Alternative(1, 1)));
    assert!(!u.drawn(Target::Alternative(0, 0)));
    assert_eq!(u.r.chosen, [0, 0]);

    // Picking the second updates the totals and the directions; the pick of a step goes.
    u.click(Target::Step(1));
    assert_eq!(u.r.picked, Some(1));
    u.click(Target::Alternative(1, 1));
    assert_eq!(u.r.chosen, [0, 1]);
    assert!(u.r.picked.is_none());
    let r = u.r.route.as_ref().unwrap();
    assert_eq!((r.trip.time_s(), r.trip.metres()), (600.0, 8000.0));
    assert_eq!(r.steps[4].metres, 2500.0);
    assert!(u.r.overlays.iter().any(|o| o.0 == Look::Alternative));
    assert_eq!(steps(&u), 6);
    // And back, once the page has been laid out again.
    u.frame(Vec::new());
    u.click(Target::Alternative(1, 0));
    assert_eq!(u.r.chosen, [0, 0]);
    assert_eq!(u.r.route.as_ref().unwrap().trip.time_s(), 360.0);
    // Closing the row hides the list and the grey line; the route picked stays.
    u.click(Target::Alternative(1, 1));
    u.frame(Vec::new());
    u.click(Target::Leg(1));
    assert_eq!(u.r.expanded, [false, false]);
    u.frame(Vec::new());
    assert!(!u.drawn(Target::Alternative(1, 0)));
    let grey = u.r.overlays.iter().find(|o| o.0 == Look::Alternative);
    assert!(grey.is_none_or(|g| g.1.is_empty()));
    assert_eq!(u.r.chosen, [0, 1]);

    // Ranked best for its turns: the quickest stays shown, and the list says so.
    let c = match &u.r.alts[1] {
        LegAlts::Found(c) => c.clone(),
        _ => panic!("found"),
    };
    let mut c = c;
    c.best = 1;
    c.routes[0].turns.uturns = 2;
    assert_eq!(
        super::ranked_note(&c).unwrap(),
        "With turn costs, the route via US 33 ranks best: 4 min slower on paper, 2 fewer \
         U-turns."
    );
}

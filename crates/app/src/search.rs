//! The search box (phase2-plan.md 2.7): suggestions as you type, the results list for
//! Enter, flying to a result and the marker left there.
//!
//! Queries run on one worker thread over [`omp_core::find`]. A new keystroke replaces the
//! query waiting there, and answers to anything but the latest query are dropped, as the
//! tile loader drops tiles the view has left. The worker opens the pack's search readers
//! itself when the pack opens, so start-up does not wait for them, and wakes the UI when an
//! answer is ready; nothing polls.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, Event, EventFilter, FontId, Id, Key, Pos2, Rect, Sense, Stroke,
    StrokeKind, Vec2,
};
use omp_core::find::{Hit, HitKind, Request};
use omp_core::geo::P;
use omp_core::pack::Pack;
use omp_core::schema::Schema;
use omp_core::search::Kind;

use crate::camera::{self, Camera, TILE_PX};
use crate::scale::Units;

/// Suggestions shown while typing.
pub const SUGGESTIONS: usize = 8;
/// Results asked for by Enter, shown in the results panel.
pub const RESULTS: usize = 50;
/// Searches kept in `config.toml`.
pub const RECENT: usize = 8;
/// The zoom an address is shown at.
const ADDRESS_ZOOM: f64 = 17.0;
/// The most a place or street is zoomed in to, and the least.
const FIT_ZOOM: [f64; 2] = [4.0, 17.0];
const EARTH_RADIUS_M: f64 = 6_371_008.8;
const CIRCUMFERENCE_M: f64 = 40_075_016.686;
const WORLD: f64 = 4_294_967_296.0;

const BOX_WIDTH: f32 = 320.0;
const PANEL_WIDTH: f32 = 360.0;
const INK: Color32 = Color32::from_gray(40);
const WEAK: Color32 = Color32::from_gray(105);
const APPROX: Color32 = Color32::from_rgb(176, 96, 0);
const PIN: Color32 = Color32::from_rgb(208, 52, 44);

/// Where the worker is.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Reading the search sections; queries asked meanwhile wait.
    Opening,
    Ready,
    /// The pack has no search, and why.
    Missing(String),
    Failed(String),
}

/// A search, as sent to the worker.
#[derive(Clone, Debug)]
pub struct Query {
    pub id: u64,
    pub text: String,
    /// The view's centre (world coordinates) and zoom, for ranking.
    pub near: camera::Vec2,
    pub zoom: f64,
    /// Still being typed: the last word may be a prefix.
    pub prefix: bool,
    pub limit: usize,
    /// Asked by Enter, for the results panel.
    pub submit: bool,
    pub asked: Instant,
}

pub struct Answer {
    pub query: Query,
    pub hits: Result<Vec<Hit>, String>,
    /// Time in `find`.
    pub find: Duration,
}

/// What the readers hold and have read, for the overlay and the bench.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// From the pack opening to the readers being ready.
    pub open: Option<Duration>,
    pub heap_bytes: usize,
    pub bytes_read: u64,
}

struct Slot {
    query: Option<Query>,
    quit: bool,
    status: Status,
    stats: Stats,
}

struct Shared {
    slot: Mutex<Slot>,
    ready: Condvar,
}

pub struct Worker {
    shared: Arc<Shared>,
    rx: Receiver<Answer>,
}

impl Worker {
    /// Opens the pack's search readers on a new thread, then answers queries. `wake` is
    /// called when the readers are open and after each answer.
    pub fn start(first_chunk: PathBuf, wake: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                query: None,
                quit: false,
                status: Status::Opening,
                stats: Stats::default(),
            }),
            ready: Condvar::new(),
        });
        let (tx, rx) = channel();
        let s = shared.clone();
        std::thread::Builder::new()
            .name("search".into())
            .spawn(move || work(&s, &first_chunk, &tx, &wake))
            .expect("spawn search worker");
        Self { shared, rx }
    }

    /// A worker with no thread, ready: the test takes the queries and sends the answers.
    #[cfg(test)]
    fn stub() -> (Self, Sender<Answer>) {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                query: None,
                quit: false,
                status: Status::Ready,
                stats: Stats::default(),
            }),
            ready: Condvar::new(),
        });
        let (tx, rx) = channel();
        (Self { shared, rx }, tx)
    }

    #[cfg(test)]
    fn take_query(&self) -> Option<Query> {
        self.shared.slot.lock().unwrap().query.take()
    }

    /// Replaces the query waiting, if any.
    pub fn ask(&self, q: Query) {
        self.shared.slot.lock().unwrap().query = Some(q);
        self.shared.ready.notify_one();
    }

    pub fn status(&self) -> Status {
        self.shared.slot.lock().unwrap().status.clone()
    }

    pub fn stats(&self) -> Stats {
        self.shared.slot.lock().unwrap().stats
    }

    pub fn poll(&self) -> impl Iterator<Item = Answer> + '_ {
        self.rx.try_iter()
    }
}

impl Drop for Worker {
    /// Stops the worker without waiting for it: a query, or opening a large pack from a
    /// slow stick, may take a while, and the thread holds its own handle on the pack.
    fn drop(&mut self) {
        self.shared.slot.lock().unwrap().quit = true;
        self.shared.ready.notify_all();
    }
}

fn work(shared: &Shared, first_chunk: &std::path::Path, tx: &Sender<Answer>, wake: &dyn Fn()) {
    let t0 = Instant::now();
    let opened = Pack::open(first_chunk).map(Arc::new).and_then(|p| {
        let minor = p.minor;
        omp_core::search::Search::open(p).map(|s| (s, minor))
    });
    let set = |status: Status, stats: Option<Stats>| {
        let mut slot = shared.slot.lock().unwrap();
        slot.status = status;
        if let Some(s) = stats {
            slot.stats = s;
        }
    };
    let search = match opened {
        Ok((Some(s), _)) => {
            let stats = Stats {
                open: Some(t0.elapsed()),
                heap_bytes: s.heap_bytes(),
                bytes_read: s.bytes_read(),
            };
            set(Status::Ready, Some(stats));
            s
        }
        Ok((None, minor)) => {
            let why = if minor < omp_core::search::MIN_MINOR {
                format!(
                    "This map was built before search was added (format 0.{minor}). Build it \
                     again with a newer omp-builder to search it."
                )
            } else {
                "This map was built without search.".into()
            };
            set(Status::Missing(why), None);
            wake();
            return;
        }
        Err(e) => {
            set(Status::Failed(e.to_string()), None);
            wake();
            return;
        }
    };
    wake();
    loop {
        let q = {
            let mut slot = shared.slot.lock().unwrap();
            loop {
                if slot.quit {
                    return;
                }
                if let Some(q) = slot.query.take() {
                    break q;
                }
                slot = shared.ready.wait(slot).unwrap();
            }
        };
        let t = Instant::now();
        let req = Request {
            text: &q.text,
            near: Some(to_p(q.near)),
            zoom: q.zoom,
            limit: q.limit,
            prefix: q.prefix,
        };
        let hits = search.find(&req).map_err(|e| e.to_string());
        let find = t.elapsed();
        {
            let mut slot = shared.slot.lock().unwrap();
            slot.stats.heap_bytes = search.heap_bytes();
            slot.stats.bytes_read = search.bytes_read();
        }
        if tx
            .send(Answer {
                query: q,
                hits,
                find,
            })
            .is_err()
        {
            return;
        }
        wake();
    }
}

/// A pack point in the app's world coordinates (0‥1).
pub fn world(p: P) -> camera::Vec2 {
    [f64::from(p[0]) / WORLD, f64::from(p[1]) / WORLD]
}

fn to_p(w: camera::Vec2) -> P {
    w.map(|v| (v * WORLD).round().clamp(0.0, f64::from(u32::MAX)) as u32)
}

/// The view to fly to for a hit: an address at z17, anything else zoomed so its radius
/// takes 40% of the shorter side of the window.
pub fn view_for(hit: &Hit, size: camera::Vec2) -> (camera::Vec2, f64) {
    let w = world(hit.p);
    if hit.kind == HitKind::Address {
        return (w, ADDRESS_ZOOM);
    }
    let lat = camera::world_to_lonlat(w)[1];
    let radius = hit.radius_m.max(50.0) / (CIRCUMFERENCE_M * lat.to_radians().cos());
    let side = size[0].min(size[1]).max(100.0);
    let zoom = (0.4 * side / (radius * TILE_PX)).log2();
    (w, zoom.clamp(FIT_ZOOM[0], FIT_ZOOM[1]))
}

/// Great-circle distance between two world points, in metres.
pub fn distance_m(a: camera::Vec2, b: camera::Vec2) -> f64 {
    let [lon1, lat1] = camera::world_to_lonlat(a).map(f64::to_radians);
    let [lon2, lat2] = camera::world_to_lonlat(b).map(f64::to_radians);
    let h = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

/// "450 m", "3.2 km", "32 km"; "500 ft", "3.2 mi", "32 mi"; both for [`Units::Both`].
pub fn distance_text(m: f64, units: Units) -> String {
    let metric = || match m {
        m if m < 1000.0 => format!("{:.0} m", (m / 10.0).round() * 10.0),
        m if m < 10_000.0 => format!("{:.1} km", m / 1000.0),
        m => format!("{:.0} km", m / 1000.0),
    };
    let imperial = || match m / 1609.344 {
        mi if mi < 0.1 => format!("{:.0} ft", (m / 0.3048 / 10.0).round() * 10.0),
        mi if mi < 10.0 => format!("{mi:.1} mi"),
        mi => format!("{mi:.0} mi"),
    };
    match units {
        Units::Kilometres => metric(),
        Units::Miles => imperial(),
        Units::Both => format!("{} · {}", imperial(), metric()),
    }
}

/// Puts `text` first in the recent searches, once.
pub fn remember(recent: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    recent.retain(|r| !r.eq_ignore_ascii_case(text));
    recent.insert(0, text.to_string());
    recent.truncate(RECENT);
}

/// The results panel: what Enter found.
pub struct Results {
    pub text: String,
    pub hits: Vec<Hit>,
    pub selected: Option<usize>,
}

/// An answer that was not stale.
#[derive(Clone, Debug)]
pub struct Taken {
    pub submit: bool,
    /// From the query being asked to the answer reaching the UI.
    pub latency: Duration,
    pub find: Duration,
    pub hits: usize,
    pub first: Option<Hit>,
}

/// The queries and answers, without the UI.
#[derive(Default)]
pub struct Session {
    next_id: u64,
    /// The latest query sent; answers to others are dropped.
    latest: Option<Query>,
    answered: bool,
    /// The suggestions and the text they are for.
    pub hits: Vec<Hit>,
    pub hits_for: String,
    pub error: Option<String>,
    pub results: Option<Results>,
    pub marker: Option<Hit>,
    /// The last answer taken, for the overlay: its text and timings.
    pub last: Option<(String, Taken)>,
    /// Answers dropped as stale.
    pub dropped: usize,
}

impl Session {
    /// A query for `text` from the view `near` at `zoom`; it becomes the latest.
    pub fn ask(&mut self, text: &str, submit: bool, near: camera::Vec2, zoom: f64) -> Query {
        self.next_id += 1;
        let q = Query {
            id: self.next_id,
            text: text.to_string(),
            near,
            zoom,
            // Enter on a word still being typed means what its suggestions showed.
            prefix: !text.ends_with(char::is_whitespace),
            limit: if submit { RESULTS } else { SUGGESTIONS },
            submit,
            asked: Instant::now(),
        };
        self.latest = Some(q.clone());
        self.answered = false;
        q
    }

    /// Forgets the latest query, so its answer is dropped.
    pub fn cancel(&mut self) {
        self.latest = None;
        self.hits.clear();
        self.hits_for.clear();
        self.error = None;
    }

    /// Whether the latest query is still out.
    pub fn waiting(&self) -> bool {
        self.latest.is_some() && !self.answered
    }

    /// Takes an answer, unless it is stale. An answer to Enter opens the results panel
    /// when there is more than one result, and its first result is given to fly to.
    pub fn take(&mut self, a: Answer) -> Option<Taken> {
        if self.latest.as_ref().map(|q| q.id) != Some(a.query.id) {
            self.dropped += 1;
            return None;
        }
        self.answered = true;
        let (hits, error) = match a.hits {
            Ok(h) => (h, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let taken = Taken {
            submit: a.query.submit,
            latency: a.query.asked.elapsed(),
            find: a.find,
            hits: hits.len(),
            first: hits.first().cloned(),
        };
        self.error = error;
        if a.query.submit {
            self.results = (hits.len() > 1).then(|| Results {
                text: a.query.text.clone(),
                hits,
                selected: Some(0),
            });
            if let Some(h) = &taken.first {
                self.marker = Some(h.clone());
            }
        } else {
            self.hits = hits;
            self.hits_for = a.query.text.clone();
        }
        self.last = Some((a.query.text, taken.clone()));
        Some(taken)
    }
}

/// A keystroke from the bench, handled as if typed.
#[derive(Clone, Debug, PartialEq)]
pub enum BenchKey {
    /// The whole text of the box after the keystroke.
    Type(String),
    Enter,
}

/// What the search UI did this frame.
#[derive(Default)]
pub struct Out {
    /// A result picked: fly there.
    pub fly: Option<Hit>,
    pub taken: Vec<Taken>,
}

/// The kind icons: glyphs from egui's built-in emoji fonts, by kind and class.
pub struct Icons {
    places: Vec<&'static str>,
    pois: Vec<&'static str>,
    labels: Vec<&'static str>,
}

impl Icons {
    pub fn new(schema: &Schema) -> Self {
        let classes = |layer: &str, f: fn(&str) -> &'static str| {
            schema
                .layer(layer)
                .map(|(_, l)| l.classes.iter().map(|c| f(&c.name)).collect())
                .unwrap_or_default()
        };
        Self {
            places: classes("places", |c| match c {
                "country" | "state" => "🗺",
                "ocean" | "sea" | "bay" => "🌊",
                "city" | "town" | "village" => "🏢",
                _ => "🏡",
            }),
            pois: classes("pois", |c| match c {
                "fuel" => "⛽",
                "charging_station" => "🔌",
                "rest_area" | "parking" => "🅿",
                "hotel" => "🏨",
                "restaurant" | "fast_food" => "🍴",
                "hospital" => "🏥",
                "airport" => "✈",
                "campground" => "⛺",
                _ => "📍",
            }),
            labels: classes("labels", |c| match c {
                "bay" => "🌊",
                "river" | "riverbank" | "lake" => "💧",
                _ => "🌲",
            }),
        }
    }

    pub fn of(&self, hit: &Hit) -> &'static str {
        let class = |v: &[&'static str], or| v.get(usize::from(hit.class)).copied().unwrap_or(or);
        match hit.kind {
            HitKind::Address => "🏠",
            HitKind::Entry(k) => match k {
                Kind::Street | Kind::AddressStreet => "🚗",
                Kind::Admin => "🗺",
                Kind::Postcode => "📮",
                Kind::Place => class(&self.places, "🏡"),
                Kind::Poi => class(&self.pois, "📍"),
                Kind::Feature => class(&self.labels, "🌲"),
            },
        }
    }
}

/// The search box over the map, its suggestions and results, for one open pack.
pub struct SearchUi {
    worker: Worker,
    pub session: Session,
    pub text: String,
    /// The text last asked about, so a query goes out only when it changes.
    sent: String,
    /// Escape closed the suggestions; they stay closed until the text changes.
    dismissed: bool,
    /// The suggestion highlighted with the arrow keys.
    selected: Option<usize>,
    /// Where the suggestions were last drawn, so a click on them keeps the box focused.
    popup_rect: Option<Rect>,
    /// The box had the focus at the end of the last frame.
    was_focused: bool,
    icons: Icons,
}

/// One row of the suggestions or the results.
enum Row {
    Hit(Hit),
    Recent(String),
}

impl SearchUi {
    pub fn start(first_chunk: PathBuf, schema: &Schema, ctx: &egui::Context) -> Self {
        let ctx = ctx.clone();
        Self::with_worker(
            Worker::start(first_chunk, move || ctx.request_repaint()),
            schema,
        )
    }

    fn with_worker(worker: Worker, schema: &Schema) -> Self {
        Self {
            worker,
            session: Session::default(),
            text: String::new(),
            sent: String::new(),
            dismissed: false,
            selected: None,
            popup_rect: None,
            was_focused: false,
            icons: Icons::new(schema),
        }
    }

    pub fn status(&self) -> Status {
        self.worker.status()
    }

    pub fn stats(&self) -> Stats {
        self.worker.stats()
    }

    fn clear(&mut self) {
        self.text.clear();
        self.sent.clear();
        self.selected = None;
        self.session.cancel();
        self.session.results = None;
        self.session.marker = None;
    }

    /// Picks a hit: the marker goes there, the query is remembered, and the box lets go.
    fn pick(&mut self, ctx: &egui::Context, hit: &Hit, recent: &mut Vec<String>) -> Hit {
        self.session.marker = Some(hit.clone());
        remember(recent, &self.text);
        self.dismissed = true;
        ctx.memory_mut(|m| m.surrender_focus(box_id()));
        hit.clone()
    }

    fn submit(&mut self, cam: &Camera) {
        if self.text.trim().is_empty() {
            return;
        }
        self.dismissed = true;
        let q = self.session.ask(&self.text, true, cam.center, cam.zoom);
        self.worker.ask(q);
    }

    /// The overlay's lines: how long opening took, what the readers hold, the last query.
    pub fn overlay_lines(&self) -> Vec<String> {
        let mib = |b: f64| b / (1024.0 * 1024.0);
        let s = self.stats();
        let mut lines = vec![match self.status() {
            Status::Opening => "search: opening…".to_string(),
            Status::Ready => format!(
                "search: opened in {:.0} ms, readers hold {:.1} MiB, {:.1} MiB read, {} stale answers dropped",
                s.open.map_or(0.0, |d| d.as_secs_f64() * 1e3),
                mib(s.heap_bytes as f64),
                mib(s.bytes_read as f64),
                self.session.dropped
            ),
            Status::Missing(_) => "search: none in this pack".into(),
            Status::Failed(e) => format!("search: failed: {e}"),
        }];
        if let Some((text, t)) = &self.session.last {
            lines.push(format!(
                "last search \"{text}\"{}: {} hits, find {:.1} ms, shown after {:.1} ms",
                if t.submit { " (Enter)" } else { "" },
                t.hits,
                t.find.as_secs_f64() * 1e3,
                t.latency.as_secs_f64() * 1e3
            ));
        }
        lines
    }

    /// Takes answers, handles the keys, and draws the box, its suggestions and the results
    /// panel. `recent` is the config's list.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        map_rect: Rect,
        cam: &Camera,
        units: Units,
        recent: &mut Vec<String>,
        bench: Option<BenchKey>,
    ) -> Out {
        let mut out = Out::default();
        let answers: Vec<Answer> = self.worker.poll().collect();
        for a in answers {
            let text = a.query.text.clone();
            if let Some(t) = self.session.take(a) {
                if t.submit {
                    out.fly = t.first.clone();
                    if t.first.is_some() {
                        remember(recent, &text);
                    }
                }
                out.taken.push(t);
            }
        }
        let status = self.status();
        let enabled = matches!(status, Status::Opening | Status::Ready);
        let id = box_id();
        let mut focused = ctx.memory(|m| m.has_focus(id));
        // egui keeps Escape from taking the focus away only once the box has had it for a
        // frame, so Escape right after Ctrl+F reaches here with the focus gone: it closes
        // the suggestions if they were open (the box keeps the focus), and is used up.
        let esc = |i: &egui::InputState| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    Event::Key {
                        key: Key::Escape,
                        pressed: true,
                        ..
                    }
                )
            })
        };
        if !focused && self.was_focused && ctx.input(esc) {
            if self.popup_rect.is_some() {
                ctx.memory_mut(|m| m.request_focus(id));
                focused = true;
            } else {
                ctx.input_mut(|i| consume(i, Key::Escape));
            }
        }
        let elsewhere = ctx.egui_wants_keyboard_input() && !focused;

        // Ctrl+F or "/" (as typed, whatever the keyboard layout) focus the box.
        if enabled && !focused && !elsewhere {
            let go = ctx.input_mut(|i| {
                let slash = i
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::Text(t) if t == "/"));
                if slash {
                    i.events
                        .retain(|e| !matches!(e, Event::Text(t) if t == "/"));
                }
                i.consume_key(egui::Modifiers::COMMAND, Key::F) || slash
            });
            if go {
                ctx.memory_mut(|m| m.request_focus(id));
                self.dismissed = false;
            }
            // Escape on the map closes the results, then removes the marker.
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
                if self.session.results.is_some() {
                    self.session.results = None;
                } else {
                    self.session.marker = None;
                }
            }
        }
        let (mut down, mut up, mut enter, mut escape) = (0, 0, false, false);
        if focused {
            ctx.input_mut(|i| {
                down = consume(i, Key::ArrowDown);
                up = consume(i, Key::ArrowUp);
                enter = consume(i, Key::Enter) > 0;
                escape = consume(i, Key::Escape) > 0;
            });
            // The arrows move through the suggestions, not the focus to another widget.
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        match bench {
            Some(BenchKey::Type(t)) => {
                self.text = t;
                self.dismissed = false;
                self.session.results = None;
                cursor_to_end(ctx, &self.text);
                ctx.memory_mut(|m| m.request_focus(id));
            }
            Some(BenchKey::Enter) => enter = true,
            None => {}
        }

        let panel = egui::Frame::popup(&ctx.global_style())
            .inner_margin(egui::Margin::symmetric(6, 4))
            .fill(Color32::from_white_alpha(235));
        let box_area = egui::Area::new(Id::unique("search-area"))
            .fixed_pos(map_rect.left_top() + Vec2::new(8.0, 8.0))
            .show(ctx, |ui| {
                panel
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // A press on the suggestions must not take the focus away
                            // before the click lands.
                            let keep = self.popup_rect.is_some_and(|r| {
                                ctx.pointer_interact_pos().is_some_and(|p| r.contains(p))
                            });
                            let before = ctx.options(|o| o.input_options.surrender_focus_on);
                            if keep {
                                ctx.options_mut(|o| {
                                    o.input_options.surrender_focus_on =
                                        egui::SurrenderFocusOn::Never;
                                });
                            }
                            let hint = match &status {
                                Status::Missing(_) | Status::Failed(_) => "No search in this map",
                                _ => "Search  (Ctrl+F or /)",
                            };
                            let filter = EventFilter {
                                horizontal_arrows: true,
                                vertical_arrows: true,
                                escape: true,
                                ..Default::default()
                            };
                            let edit = egui::TextEdit::singleline(&mut self.text)
                                .id(id)
                                .hint_text(hint)
                                .desired_width(BOX_WIDTH - 34.0)
                                .interactive(enabled)
                                .return_key(None)
                                .event_filter(filter);
                            let resp = ui.add(edit);
                            // Escape and the arrows stay with the box, from the frame focus
                            // was asked for (the field sets this only from the next one).
                            ctx.memory_mut(|m| m.set_focus_lock_filter(id, filter));
                            ctx.options_mut(|o| o.input_options.surrender_focus_on = before);
                            // A new query: its suggestions replace the last one's results.
                            if resp.changed() {
                                self.dismissed = false;
                                self.selected = None;
                                self.session.results = None;
                            }
                            match &status {
                                Status::Missing(why) => {
                                    resp.on_disabled_hover_text(why.as_str());
                                }
                                Status::Failed(e) => {
                                    resp.on_disabled_hover_text(format!(
                                        "Search could not be opened: {e}"
                                    ));
                                }
                                _ => {}
                            }
                            let any = !self.text.is_empty()
                                || self.session.marker.is_some()
                                || self.session.results.is_some();
                            let clear = ui
                                .add_enabled(
                                    any,
                                    egui::Button::new("×").min_size(Vec2::splat(22.0)),
                                )
                                .on_hover_text("Clear the search and its marker");
                            if clear.clicked() {
                                self.clear();
                            }
                        });
                    })
                    .response
                    .rect
            })
            .inner;

        // A query when the text has changed.
        if enabled && self.text != self.sent {
            self.sent = self.text.clone();
            if self.text.trim().is_empty() {
                self.session.cancel();
            } else {
                let q = self.session.ask(&self.text, false, cam.center, cam.zoom);
                self.worker.ask(q);
            }
        }

        // The suggestions: recent searches for an empty box, else the latest hits.
        let focused = ctx.memory(|m| m.has_focus(id));
        let rows: Vec<Row> = if self.text.trim().is_empty() {
            recent.iter().cloned().map(Row::Recent).collect()
        } else {
            self.session.hits.iter().cloned().map(Row::Hit).collect()
        };
        let message = if !self.text.trim().is_empty() && rows.is_empty() {
            match (&status, &self.session.error) {
                (Status::Opening, _) => Some("Opening search…".to_string()),
                (_, Some(e)) => Some(format!("Search failed: {e}")),
                _ if self.session.waiting() => Some("Searching…".into()),
                _ => Some("Nothing found".into()),
            }
        } else {
            None
        };
        if !rows.is_empty() {
            let n = rows.len();
            if down > 0 || up > 0 {
                let at = self.selected.map_or(n - 1 + usize::from(up > 0), |s| s + n);
                self.selected = Some((at + down - up % n) % n);
            }
            if self.selected.is_some_and(|s| s >= n) {
                self.selected = None;
            }
        }
        let mut chosen: Option<usize> = None;
        if enter {
            match self.selected {
                Some(i) if i < rows.len() => chosen = Some(i),
                _ => self.submit(cam),
            }
        }
        let open = focused && !self.dismissed && (!rows.is_empty() || message.is_some());
        if escape {
            if open {
                self.dismissed = true;
            } else {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
        self.popup_rect = None;
        if open && !escape && !enter {
            let r = egui::Area::new(Id::unique("search-suggestions"))
                .order(egui::Order::Foreground)
                .fixed_pos(box_area.left_bottom() + Vec2::new(0.0, 2.0))
                .show(ctx, |ui| {
                    panel.show(ui, |ui| {
                        ui.set_width(BOX_WIDTH);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if let Some(m) = &message {
                            ui.label(egui::RichText::new(m).color(WEAK));
                        }
                        for (i, row) in rows.iter().enumerate() {
                            let r = self.row(ui, row, cam, units, self.selected == Some(i));
                            if r.clicked() {
                                chosen = Some(i);
                            }
                        }
                    })
                });
            self.popup_rect = Some(r.response.rect);
        }
        if let Some(i) = chosen {
            match rows.get(i) {
                Some(Row::Hit(h)) => out.fly = Some(self.pick(ctx, h, recent)),
                Some(Row::Recent(r)) => {
                    self.text = r.clone();
                    self.sent = self.text.clone();
                    cursor_to_end(ctx, &self.text);
                    self.submit(cam);
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
                None => {}
            }
            self.selected = None;
        }

        if let Some(f) = self.results_panel(ctx, map_rect, box_area, cam, units) {
            self.session.marker = Some(f.clone());
            out.fly = Some(f);
        }
        self.was_focused = ctx.memory(|m| m.has_focus(id));
        out
    }

    /// The results of Enter, under the box; returns a result clicked.
    fn results_panel(
        &mut self,
        ctx: &egui::Context,
        map_rect: Rect,
        box_area: Rect,
        cam: &Camera,
        units: Units,
    ) -> Option<Hit> {
        let res = self.session.results.as_ref()?;
        let mut close = false;
        let mut clicked = None;
        let panel = egui::Frame::popup(&ctx.global_style())
            .inner_margin(egui::Margin::symmetric(6, 6))
            .fill(Color32::from_white_alpha(240));
        let top = box_area.bottom() + 6.0;
        egui::Area::new(Id::unique("search-results"))
            .fixed_pos(Pos2::new(box_area.left(), top))
            .show(ctx, |ui| {
                panel.show(ui, |ui| {
                    ui.set_width(PANEL_WIDTH);
                    ui.horizontal(|ui| {
                        let n = res.hits.len();
                        let more = if n >= RESULTS { " or more" } else { "" };
                        ui.label(
                            egui::RichText::new(format!("{n}{more} results for “{}”", res.text))
                                .strong()
                                .color(INK),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.small_button("×").on_hover_text("Close  (Esc)").clicked();
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height((map_rect.bottom() - top - 90.0).max(120.0))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (i, h) in res.hits.iter().enumerate() {
                                let row = Row::Hit(h.clone());
                                let r = self.row(ui, &row, cam, units, res.selected == Some(i));
                                if r.clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                });
            });
        let res = self.session.results.as_mut()?;
        if close {
            self.session.results = None;
            return None;
        }
        let i = clicked?;
        res.selected = Some(i);
        res.hits.get(i).cloned()
    }

    /// A suggestion or result: icon, name, a tag when approximate, the place it is in and
    /// the distance from the view centre.
    fn row(
        &self,
        ui: &mut egui::Ui,
        row: &Row,
        cam: &Camera,
        units: Units,
        selected: bool,
    ) -> egui::Response {
        let width = ui.available_width();
        let (icon, name, tag, context, right) = match row {
            Row::Recent(r) => ("🕒", r.to_string(), None, String::new(), String::new()),
            Row::Hit(h) => {
                let tag = if h.number_missing {
                    Some("number not found")
                } else if h.approx {
                    Some("approximate")
                } else {
                    None
                };
                let mut context = h.context.clone();
                if let Some(pc) = &h.postcode {
                    context = if context.is_empty() {
                        pc.clone()
                    } else {
                        format!("{context} {pc}")
                    };
                }
                if !h.units.is_empty() {
                    context += &format!(" · units {}", h.units.join(", "));
                }
                let d = distance_text(distance_m(cam.center, world(h.p)), units);
                (self.icons.of(h), h.name.clone(), tag, context, d)
            }
        };
        let height = if context.is_empty() { 26.0 } else { 38.0 };
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
        let p = ui.painter_at(rect);
        if selected || resp.hovered() {
            let fill = if selected {
                Color32::from_rgb(218, 230, 248)
            } else {
                Color32::from_gray(238)
            };
            p.rect_filled(rect, 3.0, fill);
        }
        let x = rect.left() + 28.0;
        p.text(
            Pos2::new(rect.left() + 13.0, rect.center().y),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(15.0),
            WEAK,
        );
        let right_g = p.layout_no_wrap(right, FontId::proportional(11.5), WEAK);
        let right_w = right_g.size().x;
        let top = rect.top() + if context.is_empty() { 5.0 } else { 3.0 };
        p.galley(
            Pos2::new(rect.right() - 4.0 - right_w, top + 2.0),
            right_g,
            WEAK,
        );
        let name_w = (rect.right() - 12.0 - right_w - x).max(40.0);
        let name_g = one_line(&p, name, FontId::proportional(14.0), INK, name_w);
        let after = x + name_g.size().x + 6.0;
        p.galley(Pos2::new(x, top), name_g, INK);
        if let Some(t) = tag {
            let g = p.layout_no_wrap(t.into(), FontId::proportional(11.0), APPROX);
            if after + g.size().x < rect.right() - 12.0 - right_w {
                p.galley(Pos2::new(after, top + 3.0), g, APPROX);
            }
        }
        if !context.is_empty() {
            let g = one_line(
                &p,
                context,
                FontId::proportional(11.5),
                WEAK,
                rect.right() - 4.0 - x,
            );
            p.galley(Pos2::new(x, top + 18.0), g, WEAK);
        }
        resp
    }
}

fn box_id() -> Id {
    Id::unique("search-box")
}

/// Puts the box's cursor after `text`, which was set from outside the box.
fn cursor_to_end(ctx: &egui::Context, text: &str) {
    let mut state = egui::text_edit::TextEditState::load(ctx, box_id()).unwrap_or_default();
    let end = egui::text::CCursor::new(text.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(end)));
    state.store(ctx, box_id());
}

/// Takes the presses of `key` with no modifiers, before the text field sees them.
fn consume(i: &mut egui::InputState, key: Key) -> usize {
    let mut n = 0;
    i.events.retain(|e| {
        let hit = matches!(e, Event::Key { key: k, pressed: true, modifiers, .. }
            if *k == key && modifiers.is_none());
        n += usize::from(hit);
        !hit
    });
    n
}

/// `text` on one line, cut with "…" to `width`.
fn one_line(
    p: &egui::Painter,
    text: String,
    font: FontId,
    color: Color32,
    width: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    p.layout_job(job)
}

/// The marker of a picked result: a pin at its point, with its name, where it is, and
/// "Approximate" for interpolated and blockface addresses.
pub fn draw_marker(painter: &egui::Painter, origin: Pos2, cam: &Camera, hit: &Hit) {
    let s = cam.world_to_screen(world(hit.p));
    let tip = Pos2::new(origin.x + s[0] as f32, origin.y + s[1] as f32);
    if !painter.clip_rect().expand(300.0).contains(tip) {
        return;
    }
    let head = tip - Vec2::new(0.0, 22.0);
    let r = 8.0;
    let outline = Stroke::new(1.5, Color32::from_rgb(120, 20, 16));
    painter.circle_filled(tip, 2.5, Color32::from_black_alpha(90));
    painter.add(egui::Shape::convex_polygon(
        vec![
            tip,
            head + Vec2::new(r * 0.85, 4.0),
            head + Vec2::new(-r * 0.85, 4.0),
        ],
        PIN,
        Stroke::NONE,
    ));
    painter.circle(head, r, PIN, outline);
    painter.circle_filled(head, 3.0, Color32::WHITE);

    let mut lines = vec![(hit.name.clone(), FontId::proportional(13.5), INK)];
    let mut context = hit.context.clone();
    if let Some(pc) = &hit.postcode {
        context = if context.is_empty() {
            pc.clone()
        } else {
            format!("{context} {pc}")
        };
    }
    if !context.is_empty() {
        lines.push((context, FontId::proportional(11.5), WEAK));
    }
    if hit.number_missing {
        lines.push((
            "Number not found on this street".into(),
            FontId::proportional(11.5),
            APPROX,
        ));
    } else if hit.approx {
        lines.push(("Approximate".into(), FontId::proportional(11.5), APPROX));
    }
    let galleys: Vec<_> = lines
        .into_iter()
        .map(|(t, f, c)| (painter.layout_no_wrap(t, f, c), c))
        .collect();
    let w = galleys.iter().map(|(g, _)| g.size().x).fold(0.0, f32::max);
    let h: f32 = galleys.iter().map(|(g, _)| g.size().y).sum();
    let pad = Vec2::new(6.0, 4.0);
    let at = head + Vec2::new(r + 6.0, -h / 2.0 - pad.y);
    let rect = Rect::from_min_size(at, Vec2::new(w, h) + 2.0 * pad);
    painter.rect(
        rect,
        4.0,
        Color32::from_white_alpha(235),
        Stroke::new(1.0, Color32::from_gray(170)),
        StrokeKind::Inside,
    );
    let mut y = rect.top() + pad.y;
    for (g, c) in galleys {
        let gh = g.size().y;
        painter.galley(Pos2::new(rect.left() + pad.x, y), g, c);
        y += gh;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(kind: HitKind, lat: f64, lon: f64, radius_m: f64) -> Hit {
        Hit {
            kind,
            class: 0,
            name: "x".into(),
            context: String::new(),
            p: omp_core::geo::project(lat, lon),
            radius_m,
            approx: false,
            number_missing: false,
            postcode: None,
            units: Vec::new(),
            entry: None,
            street: None,
            score: 1.0,
            exact: true,
        }
    }

    fn answer(q: &Query, hits: Vec<Hit>) -> Answer {
        Answer {
            query: q.clone(),
            hits: Ok(hits),
            find: Duration::from_millis(3),
        }
    }

    #[test]
    fn stale_answers_are_dropped() {
        let mut s = Session::default();
        let a = s.ask("Col", false, [0.3, 0.4], 8.0);
        let b = s.ask("Colu", false, [0.3, 0.4], 8.0);
        assert!(a.prefix && b.limit == SUGGESTIONS);
        assert!(
            s.take(answer(&a, vec![hit(HitKind::Address, 40.0, -83.0, 10.0)]))
                .is_none()
        );
        assert_eq!((s.dropped, s.hits.len()), (1, 0));
        assert!(s.waiting());
        let t = s
            .take(answer(&b, vec![hit(HitKind::Address, 40.0, -83.0, 10.0)]))
            .unwrap();
        assert!(!t.submit && t.hits == 1 && !s.waiting());
        assert_eq!(s.hits_for, "Colu");
        // A space ends the word, so the last word is no longer a prefix.
        assert!(!s.ask("Columbus ", false, [0.3, 0.4], 8.0).prefix);
        s.cancel();
        assert!(!s.waiting() && s.hits.is_empty());
    }

    #[test]
    fn enter_opens_results_and_marks_the_first() {
        let mut s = Session::default();
        let q = s.ask("Springfield", true, [0.3, 0.4], 8.0);
        assert!(q.prefix && q.limit == RESULTS);
        let hits = vec![
            hit(HitKind::Entry(Kind::Place), 39.8, -89.6, 8000.0),
            hit(HitKind::Entry(Kind::Place), 37.2, -93.3, 8000.0),
        ];
        let t = s.take(answer(&q, hits)).unwrap();
        assert!(t.submit && t.first.is_some());
        assert_eq!(s.results.as_ref().unwrap().hits.len(), 2);
        assert!(s.marker.is_some() && s.hits.is_empty());
        // One result: no panel.
        let q = s.ask("45201", true, [0.3, 0.4], 8.0);
        s.take(answer(
            &q,
            vec![hit(HitKind::Entry(Kind::Postcode), 39.1, -84.5, 1000.0)],
        ));
        assert!(s.results.is_none());
    }

    #[test]
    fn views_fit_results() {
        let size = [1280.0, 800.0];
        let (_, z) = view_for(&hit(HitKind::Address, 40.0, -83.0, 10.0), size);
        assert_eq!(z, ADDRESS_ZOOM);
        // A city 8 km across fills 40% of 800 points at about z11.
        let (_, z) = view_for(&hit(HitKind::Entry(Kind::Place), 40.0, -83.0, 8000.0), size);
        assert!((10.5..11.5).contains(&z), "{z}");
        let (_, z) = view_for(&hit(HitKind::Entry(Kind::Admin), 40.0, -83.0, 2e6), size);
        assert_eq!(z, FIT_ZOOM[0]);
        let (w, _) = view_for(&hit(HitKind::Entry(Kind::Street), 40.0, -83.0, 500.0), size);
        let [lon, lat] = camera::world_to_lonlat(w);
        assert!((lat - 40.0).abs() < 1e-6 && (lon + 83.0).abs() < 1e-6);
    }

    #[test]
    fn distances() {
        let a = camera::lonlat_to_world(-83.0, 40.0);
        let b = camera::lonlat_to_world(-82.0, 40.0);
        // A degree of longitude at 40° N: 85.2 km on the great circle.
        assert!((distance_m(a, b) - 85_180.0).abs() < 10.0);
        assert_eq!(distance_text(437.0, Units::Kilometres), "440 m");
        assert_eq!(distance_text(3_240.0, Units::Kilometres), "3.2 km");
        assert_eq!(distance_text(85_180.0, Units::Miles), "53 mi");
        assert_eq!(distance_text(100.0, Units::Miles), "330 ft");
        assert_eq!(distance_text(5_000.0, Units::Both), "3.1 mi · 5.0 km");
    }

    #[test]
    fn keyboard() {
        let (worker, tx) = Worker::stub();
        let schema = Schema {
            whitelist_version: 0,
            layers: Vec::new(),
        };
        let mut s = SearchUi::with_worker(worker, &schema);
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0));
        let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
        cam.size = [1000.0, 700.0];
        let mut recent = Vec::new();
        let frame = |s: &mut SearchUi, recent: &mut Vec<String>, events: Vec<Event>| {
            let raw = egui::RawInput {
                events,
                screen_rect: Some(rect),
                focused: true,
                ..Default::default()
            };
            let mut out = None;
            let mut full = ctx.run_pass(raw, |ctx| {
                out = Some(s.ui(ctx, rect, &cam, Units::Kilometres, recent, None));
            });
            full.textures_delta.clear();
            let focused = ctx.memory(|m| m.has_focus(box_id()));
            (out.unwrap(), focused)
        };
        let key = |key, modifiers| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let none = egui::Modifiers::NONE;

        // "/" focuses the box and is not typed into it.
        frame(&mut s, &mut recent, Vec::new());
        let (_, focused) = frame(&mut s, &mut recent, vec![Event::Text("/".into())]);
        assert!(focused && s.text.is_empty());
        frame(&mut s, &mut recent, vec![Event::Text("Col".into())]);
        assert_eq!(s.text, "Col");
        let q = s.worker.take_query().unwrap();
        assert!(q.prefix && !q.submit);
        let hits = vec![
            hit(HitKind::Entry(Kind::Place), 39.96, -83.0, 8000.0),
            hit(HitKind::Entry(Kind::Street), 41.5, -81.7, 500.0),
        ];
        tx.send(answer(&q, hits.clone())).unwrap();
        frame(&mut s, &mut recent, Vec::new());
        assert_eq!(s.session.hits.len(), 2);

        // Down twice selects the second; Enter picks it.
        frame(
            &mut s,
            &mut recent,
            vec![key(Key::ArrowDown, none), key(Key::ArrowDown, none)],
        );
        assert_eq!(s.selected, Some(1));
        let (out, focused) = frame(&mut s, &mut recent, vec![key(Key::Enter, none)]);
        assert_eq!(out.fly.as_ref(), Some(&hits[1]));
        assert_eq!(s.session.marker.as_ref(), Some(&hits[1]));
        assert!(!focused && recent == ["Col"]);
        // On the map, Escape removes the marker.
        frame(&mut s, &mut recent, vec![key(Key::Escape, none)]);
        assert!(s.session.marker.is_none());

        // Ctrl+F; Escape closes the suggestions, then lets go of the box, from the first
        // frame (egui's focus filter only holds from the second).
        let (_, focused) = frame(
            &mut s,
            &mut recent,
            vec![key(Key::F, egui::Modifiers::COMMAND)],
        );
        assert!(focused && !s.dismissed);
        let (_, focused) = frame(&mut s, &mut recent, vec![key(Key::Escape, none)]);
        assert!(focused && s.dismissed);
        let (_, focused) = frame(&mut s, &mut recent, vec![key(Key::Escape, none)]);
        assert!(!focused);

        // Enter with nothing selected searches for the results panel and flies to the first.
        frame(
            &mut s,
            &mut recent,
            vec![key(Key::F, egui::Modifiers::COMMAND)],
        );
        frame(&mut s, &mut recent, vec![key(Key::Enter, none)]);
        let q = s.worker.take_query().unwrap();
        assert!(q.submit && q.limit == RESULTS && q.prefix);
        tx.send(answer(&q, hits.clone())).unwrap();
        let (out, _) = frame(&mut s, &mut recent, Vec::new());
        assert_eq!(out.fly.as_ref(), Some(&hits[0]));
        assert_eq!(s.session.results.as_ref().map(|r| r.hits.len()), Some(2));
        // Typing again closes the results; the clear button empties everything.
        frame(
            &mut s,
            &mut recent,
            vec![key(Key::F, egui::Modifiers::COMMAND)],
        );
        frame(&mut s, &mut recent, vec![Event::Text("u".into())]);
        assert!(s.session.results.is_none() && s.text == "Colu");
        s.clear();
        assert!(s.text.is_empty() && s.session.marker.is_none());

        // An arrow right after Ctrl+F moves through the recent searches, not the focus.
        frame(&mut s, &mut recent, vec![key(Key::Escape, none)]);
        frame(&mut s, &mut recent, Vec::new());
        frame(
            &mut s,
            &mut recent,
            vec![key(Key::F, egui::Modifiers::COMMAND)],
        );
        let (_, focused) = frame(&mut s, &mut recent, vec![key(Key::ArrowDown, none)]);
        assert!(focused && s.selected == Some(0));
        // Enter on a recent search runs it.
        frame(&mut s, &mut recent, vec![key(Key::Enter, none)]);
        assert_eq!(s.text, "Col");
        assert!(s.worker.take_query().unwrap().submit);
    }

    #[test]
    fn recent_searches_stay_few_and_distinct() {
        let mut r = Vec::new();
        for i in 0..12 {
            remember(&mut r, &format!("query {i}"));
        }
        remember(&mut r, "  QUERY 5 ");
        remember(&mut r, "");
        assert_eq!(r.len(), RECENT);
        assert_eq!(r[0], "QUERY 5");
        assert_eq!(
            r.iter()
                .filter(|q| q.eq_ignore_ascii_case("query 5"))
                .count(),
            1
        );
    }
}

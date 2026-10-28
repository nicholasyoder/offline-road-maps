//! The search box at the top of the side panel, its keys, and the search page: the
//! suggestions while typing, else the results of Enter.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::mpsc::Sender;

use eframe::egui::{self, Event, EventFilter, Id, Key, Rect, Vec2};
use omp_core::find::Hit;
use omp_core::schema::Schema;

use super::draw::{self, Icons, Row};
use super::session::{RESULTS, Session, Taken, remember};
use super::worker::{Answer, Stats, Status, Worker};
use crate::camera::Camera;
use crate::here::{HereAnswer, HereQuery};
use crate::routing::{self, MenuState, StopAction};
use crate::scale::Units;
use crate::theme::{INK, WEAK};

/// Shown while the deeper search levels run.
const SEARCHING_MORE: &str = "Searching smaller towns and more names…";

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
    /// A route item picked in a result's right-click menu.
    pub route: Option<(StopAction, Hit)>,
}

/// The suggestions to show under the box this frame.
#[derive(Default)]
struct List {
    open: bool,
    rows: Vec<Row>,
    message: Option<String>,
    more: bool,
}

/// The search box in the side panel, its suggestions and results, for one open pack.
pub struct SearchUi {
    worker: Worker,
    pub session: Session,
    pub text: String,
    /// The text last asked about, so a query goes out only when it changes.
    sent: String,
    /// Escape closed the suggestions; they stay closed until the text changes.
    dismissed: bool,
    /// The suggestion highlighted with the arrow keys, and the one under the pointer.
    selected: Option<usize>,
    hovered: Option<usize>,
    /// Where the suggestions were last drawn, so a click on them keeps the box focused.
    list_rect: Option<Rect>,
    /// The suggestions as the box left them this frame, for the search page.
    list: List,
    /// The box had the focus at the end of the last frame.
    was_focused: bool,
    icons: Icons,
    /// For the route items of the results' right-click menu; set before each frame.
    pub route_menu: MenuState,
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
            hovered: None,
            list_rect: None,
            list: List::default(),
            was_focused: false,
            icons: Icons::new(schema),
            route_menu: MenuState::default(),
        }
    }

    pub fn status(&self) -> Status {
        self.worker.status()
    }

    pub fn stats(&self) -> Stats {
        self.worker.stats()
    }

    pub fn icons(&self) -> &Icons {
        &self.icons
    }

    /// The box had the focus at the end of the last frame: an Escape now is its own.
    pub fn box_was_focused(&self) -> bool {
        self.was_focused
    }

    pub fn ask_here(&self, q: HereQuery) {
        self.worker.ask_here(q);
    }

    pub fn here_answers(&self) -> Vec<HereAnswer> {
        self.worker.poll_here().collect()
    }

    /// A search UI on a worker with no thread: the test takes the "what's here" requests
    /// and sends the answers.
    #[cfg(test)]
    pub fn stub() -> (Self, Sender<HereAnswer>) {
        let (worker, _, here_tx) = Worker::stub();
        let schema = Schema {
            whitelist_version: 0,
            layers: Vec::new(),
        };
        (Self::with_worker(worker, &schema), here_tx)
    }

    #[cfg(test)]
    pub fn take_here(&self) -> Option<HereQuery> {
        self.worker.take_here()
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
                "last search \"{text}\"{}: level {}{}, {} hits, find {:.1} ms, shown after {:.1} ms; session caches {:.1} MiB (peak {:.1})",
                if t.submit { " (Enter)" } else { "" },
                t.level,
                if t.more { " (deeper running)" } else { "" },
                t.hits,
                t.find.as_secs_f64() * 1e3,
                t.latency.as_secs_f64() * 1e3,
                mib(s.session_bytes as f64),
                mib(s.session_peak as f64)
            ));
        }
        lines
    }

    /// Takes the worker's answers; a search submitted with Enter flies to its first result.
    pub fn poll(&mut self, recent: &mut Vec<String>) -> Out {
        let mut out = Out::default();
        let answers: Vec<Answer> = self.worker.poll().collect();
        for a in answers {
            let text = a.query.text.clone();
            let pinned = self.selected.max(self.hovered);
            if let Some(t) = self.session.take(a, pinned) {
                if t.submit {
                    out.fly = t.first.clone();
                    if t.first.is_some() {
                        remember(recent, &text);
                    }
                }
                out.taken.push(t);
            }
        }
        out
    }

    /// Ctrl+F or "/" (as typed, whatever the keyboard layout) give the box the focus, unless
    /// another field has the keys. Returns whether they did: the side panel is to show the
    /// box and the search page.
    pub fn shortcut(&mut self, ctx: &egui::Context) -> bool {
        let enabled = matches!(self.status(), Status::Opening | Status::Ready);
        if !enabled || ctx.egui_wants_keyboard_input() {
            return false;
        }
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
            ctx.memory_mut(|m| m.request_focus(box_id()));
            self.dismissed = false;
        }
        go
    }

    /// Escape on the map closes the results, then removes the marker. Returns whether it
    /// did.
    pub fn escape(&mut self, ctx: &egui::Context) -> bool {
        if ctx.egui_wants_keyboard_input()
            || (self.session.results.is_none() && self.session.marker.is_none())
        {
            return false;
        }
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if esc {
            if self.session.results.is_some() {
                self.session.results = None;
            } else {
                self.session.marker = None;
            }
        }
        esc
    }

    pub fn has_results(&self) -> bool {
        self.session.results.is_some()
    }

    /// The box has the focus.
    pub fn focused(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.has_focus(box_id()))
    }

    /// The panel is closed: the box lets go of the focus and the suggestions close.
    pub fn hide(&mut self, ctx: &egui::Context) {
        ctx.memory_mut(|m| m.surrender_focus(box_id()));
        self.was_focused = false;
        self.list = List::default();
        self.list_rect = None;
    }

    /// The box and its clear button, across `ui` but for `reserve` points, with its keys: the arrows move through the
    /// suggestions, Enter picks one or searches, Escape closes them and then lets go of the
    /// box. What the suggestions are is kept for [`Self::suggestions_ui`].
    pub fn box_ui(
        &mut self,
        ui: &mut egui::Ui,
        cam: &Camera,
        reserve: f32,
        recent: &mut Vec<String>,
        bench: Option<BenchKey>,
        out: &mut Out,
    ) {
        let ctx = ui.ctx().clone();
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
            if self.list_rect.is_some() {
                ctx.memory_mut(|m| m.request_focus(id));
                focused = true;
            } else {
                ctx.input_mut(|i| consume(i, Key::Escape));
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
                cursor_to_end(&ctx, &self.text);
                ctx.memory_mut(|m| m.request_focus(id));
            }
            Some(BenchKey::Enter) => enter = true,
            None => {}
        }

        ui.horizontal(|ui| {
            // A press on the suggestions must not take the focus away before the click
            // lands.
            let keep = self
                .list_rect
                .is_some_and(|r| ctx.pointer_interact_pos().is_some_and(|p| r.contains(p)));
            let before = ctx.options(|o| o.input_options.surrender_focus_on);
            if keep {
                ctx.options_mut(|o| {
                    o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never;
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
                .desired_width((ui.available_width() - reserve - 42.0).max(80.0))
                .interactive(enabled)
                .return_key(None)
                .event_filter(filter);
            let resp = ui.add(edit);
            // Escape and the arrows stay with the box, from the frame focus was asked for
            // (the field sets this only from the next one).
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
                    resp.on_disabled_hover_text(format!("Search could not be opened: {e}"));
                }
                _ => {}
            }
            let any = !self.text.is_empty()
                || self.session.marker.is_some()
                || self.session.results.is_some();
            let clear = ui
                .add_enabled(any, egui::Button::new("×").min_size(Vec2::splat(22.0)))
                .on_hover_text("Clear the search and its marker");
            if clear.clicked() {
                self.clear();
            }
        });

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
                _ if self.session.searching() => Some(SEARCHING_MORE.into()),
                _ => Some("Nothing found".into()),
            }
        } else {
            None
        };
        // Below the suggestions while deeper levels run.
        let more = !rows.is_empty()
            && matches!(rows[0], Row::Hit(_))
            && self.session.searching()
            && !self.session.waiting();
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
        self.list_rect = None;
        if let Some(i) = chosen {
            self.choose(&ctx, &rows, i, cam, recent, out);
        }
        self.list = List {
            open: open && !escape && !enter,
            rows,
            message,
            more,
        };
        self.was_focused = ctx.memory(|m| m.has_focus(id));
    }

    /// A suggestion picked: a hit flies there; a recent search is run again.
    fn choose(
        &mut self,
        ctx: &egui::Context,
        rows: &[Row],
        i: usize,
        cam: &Camera,
        recent: &mut Vec<String>,
        out: &mut Out,
    ) {
        match rows.get(i) {
            Some(Row::Hit(h)) => out.fly = Some(self.pick(ctx, h, recent)),
            Some(Row::Recent(r)) => {
                self.text = r.clone();
                self.sent = self.text.clone();
                cursor_to_end(ctx, &self.text);
                self.submit(cam);
                ctx.memory_mut(|m| m.surrender_focus(box_id()));
            }
            None => {}
        }
        self.selected = None;
    }

    /// The suggestions, when the box left them open this frame. Returns whether it drew
    /// them.
    pub fn suggestions_ui(
        &mut self,
        ui: &mut egui::Ui,
        cam: &Camera,
        units: Units,
        recent: &mut Vec<String>,
        out: &mut Out,
    ) -> bool {
        if !self.list.open {
            return false;
        }
        let List {
            rows,
            message,
            more,
            ..
        } = std::mem::take(&mut self.list);
        let mut chosen = None;
        let r = ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let searching = |ui: &mut egui::Ui, text: &str| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(WEAK));
                    ui.label(egui::RichText::new(text).color(WEAK));
                });
            };
            match &message {
                Some(m) if m == SEARCHING_MORE => searching(ui, m),
                Some(m) => {
                    ui.label(egui::RichText::new(m).color(WEAK));
                }
                None => {}
            }
            self.hovered = None;
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, row) in rows.iter().enumerate() {
                        let r =
                            draw::row(ui, row, &self.icons, cam, units, self.selected == Some(i));
                        if r.clicked() {
                            chosen = Some(i);
                        }
                        if r.hovered() {
                            self.hovered = Some(i);
                        }
                    }
                    if more {
                        ui.add_space(3.0);
                        searching(ui, SEARCHING_MORE);
                    }
                });
        });
        self.list_rect = Some(r.response.rect);
        if let Some(i) = chosen {
            let ctx = ui.ctx().clone();
            self.choose(&ctx, &rows, i, cam, recent, out);
            self.was_focused = ctx.memory(|m| m.has_focus(box_id()));
        }
        true
    }

    /// The results of Enter, filling the rest of `ui`: a result clicked flies there, and its
    /// right-click menu has the route items.
    pub fn results_ui(&mut self, ui: &mut egui::Ui, cam: &Camera, units: Units, out: &mut Out) {
        let Some(res) = self.session.results.as_ref() else {
            return;
        };
        let mut close = false;
        let mut clicked = None;
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
            .id_salt("search-results")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, h) in res.hits.iter().enumerate() {
                    let row = Row::Hit(h.clone());
                    let r = draw::row(ui, &row, &self.icons, cam, units, res.selected == Some(i));
                    if r.clicked() {
                        clicked = Some(i);
                    }
                    r.context_menu(|ui| {
                        if let Some(a) = routing::menu_items(ui, &self.route_menu) {
                            out.route = Some((a, h.clone()));
                            ui.close();
                        }
                    });
                }
            });
        let Some(res) = self.session.results.as_mut() else {
            return;
        };
        if close {
            self.session.results = None;
            return;
        }
        if let Some(h) = clicked.and_then(|i| {
            res.selected = Some(i);
            res.hits.get(i).cloned()
        }) {
            self.session.marker = Some(h.clone());
            out.fly = Some(h);
        }
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

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;

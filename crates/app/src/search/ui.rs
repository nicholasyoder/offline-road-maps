//! The search box over the map: its keys, the suggestions under it and the results panel.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::mpsc::Sender;

use eframe::egui::{self, Color32, Event, EventFilter, Id, Key, Pos2, Rect, Vec2};
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

const BOX_WIDTH: f32 = 320.0;
const PANEL_WIDTH: f32 = 360.0;

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

/// The search box over the map, its suggestions and results, for one open pack.
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
    popup_rect: Option<Rect>,
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
            popup_rect: None,
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
        self.popup_rect = None;
        if open && !escape && !enter {
            let r = egui::Area::new(Id::unique("search-suggestions"))
                .order(egui::Order::Foreground)
                .fixed_pos(box_area.left_bottom() + Vec2::new(0.0, 2.0))
                .show(ctx, |ui| {
                    panel.show(ui, |ui| {
                        ui.set_width(BOX_WIDTH);
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
                        for (i, row) in rows.iter().enumerate() {
                            let r = draw::row(
                                ui,
                                row,
                                &self.icons,
                                cam,
                                units,
                                self.selected == Some(i),
                            );
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

        let (clicked, route) = self.results_panel(ctx, map_rect, box_area, cam, units);
        if let Some(f) = clicked {
            self.session.marker = Some(f.clone());
            out.fly = Some(f);
        }
        out.route = route;
        self.was_focused = ctx.memory(|m| m.has_focus(id));
        out
    }

    /// The results of Enter, under the box; returns a result clicked, and a route item
    /// picked in a result's right-click menu.
    fn results_panel(
        &mut self,
        ctx: &egui::Context,
        map_rect: Rect,
        box_area: Rect,
        cam: &Camera,
        units: Units,
    ) -> (Option<Hit>, Option<(StopAction, Hit)>) {
        let Some(res) = self.session.results.as_ref() else {
            return (None, None);
        };
        let mut close = false;
        let mut clicked = None;
        let mut route = None;
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
                                let r = draw::row(
                                    ui,
                                    &row,
                                    &self.icons,
                                    cam,
                                    units,
                                    res.selected == Some(i),
                                );
                                if r.clicked() {
                                    clicked = Some(i);
                                }
                                r.context_menu(|ui| {
                                    if let Some(a) = routing::menu_items(ui, &self.route_menu) {
                                        route = Some((a, h.clone()));
                                        ui.close();
                                    }
                                });
                            }
                        });
                });
            });
        let Some(res) = self.session.results.as_mut() else {
            return (None, route);
        };
        if close {
            self.session.results = None;
            return (None, route);
        }
        let Some(i) = clicked else {
            return (None, route);
        };
        res.selected = Some(i);
        (res.hits.get(i).cloned(), route)
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

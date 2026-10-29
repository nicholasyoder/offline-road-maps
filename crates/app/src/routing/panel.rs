//! The route page of the side panel (phase 3 steps 3.7 and 3.8): the stops in order (drag a
//! stop's handle to move it, × to remove it), a row of Clear, Reverse and *Optimize…*, the
//! avoid options under *More options*, the distance and driving time in all and per leg,
//! notices about the route, and the directions. Clicking a step flies to it and draws its
//! stretch over the route; Escape lets go of it. *Optimize…* (phase 4 step 4.1) opens a
//! window beside the panel that reorders the stops, keeping the ends checked, says what it
//! saved and offers Undo. Under the totals, each leg has a row with its distance and time
//! that opens to its routes (step 4.3): they are looked for then, listed with the one shown
//! selected, and drawn grey on the map until the row is closed; clicking another shows it.
//! *Depart at / Arrive by* (step 4.5) has its own section under *More options*: the date and time, the driving day and the breaks. With a time, each
//! stop's row says when it arrives and leaves, with a field for the time spent there, and
//! the directions get a column of clock times and rows for each night ("Day 2"), break and
//! zone crossing; a night along a leg can be made a stop. Times in another zone than the
//! departure's are drawn in an accent colour. The rest of phase 4's trip planning has its
//! places here, disabled.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use omp_core::geo;
use omp_core::route::{FERRY, Kind, Modifier, Step, TOLL};
use omp_core::schedule::Stay;

use super::times::{Mode, Note, NoteKind, parse_time};
use super::worker::Choices;
use super::{
    END, LegAlts, Place, Reordered, RouteUi, START, StopAction, VIA, delta_text, duration_text,
};
use crate::camera::{self, Camera};
use crate::scale::Units;
use crate::search::distance_text;
use crate::side::later;
use crate::theme::{ACCENT, APPROX, Glyph, INK, WEAK, glyph_button};

const ICON: f32 = 20.0;
/// Where a step's text starts, from the row's left, and the line under it.
const TEXT_X: f32 = ICON + 14.0;
const STEP_FONT: FontId = FontId::proportional(13.5);
const SUB_H: f32 = 16.0;
const PICKED_FILL: Color32 = Color32::from_rgb(222, 232, 250);
/// The directions' column of clock times, when a time is set.
const TIME_W: f32 = 64.0;
const SMALL: FontId = FontId::proportional(11.5);
/// The *Make overnight stop* button under a night's row.
const BUTTON_H: f32 = 20.0;
/// Behind the schedule's rows in the directions.
const NOTE_FILL: Color32 = Color32::from_rgb(243, 244, 250);

/// What a click or a drag in the page landed on, for the tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Clear,
    /// A stop's drag handle, and its row (where a dragged stop is dropped).
    Handle(usize),
    Row(usize),
    Remove(usize),
    Reverse,
    /// *Optimize…*, which opens and closes the window.
    OptimizeWindow,
    MoreOptions,
    Avoid(u8),
    Optimize,
    CancelOptimize,
    CloseOptimize,
    KeepFirst,
    KeepLast,
    RoundTrip,
    Undo,
    /// A leg's row, which opens and closes its routes.
    Leg(usize),
    /// A leg's route in its list.
    Alternative(usize, usize),
    Step(usize),
    /// *Depart at / Arrive by*'s row, and its choices.
    Times,
    Mode(Mode),
    /// A stop's time there, which opens its field.
    Stay(usize),
    Overnight(usize),
    /// *Make overnight stop* under a night's row: the schedule's event.
    MakeOvernight(usize),
}

/// A change asked for in the page, made once it is drawn.
enum Edit {
    Clear,
    Move(usize, usize),
    Remove(usize),
    Reverse,
    Avoid(u8, bool),
    Optimize,
    CancelOptimize,
    Undo,
    Expand(usize, bool),
    Choose(usize, usize),
    Pick(usize),
    Mode(Mode),
    Retime,
    Stay(usize, Stay),
    MakeOvernight(usize),
}

impl RouteUi {
    /// Escape lets go of the step picked. Returns whether it did.
    pub fn escape(&mut self, ctx: &egui::Context) -> bool {
        if self.picked.is_none() || ctx.egui_wants_keyboard_input() {
            return false;
        }
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.pick(None);
        }
        esc
    }

    /// Draws the page in `ui`, the directions filling what is left of its height. Returns
    /// the view to fly to when a step is clicked.
    pub fn page_ui(
        &mut self,
        ui: &mut egui::Ui,
        cam: &Camera,
        units: Units,
    ) -> Option<(camera::Vec2, f64)> {
        self.targets.clear();
        self.refresh_times();
        let mut edits = Vec::new();
        if self.has_stops() {
            self.stop_rows(ui, &mut edits);
            ui.add_space(4.0);
        }
        self.options(ui, &mut edits);
        // Beside the panel, level with its buttons.
        let at = Pos2::new(ui.max_rect().right() + 12.0, ui.min_rect().top());
        self.optimize_window(&ui.ctx().clone(), at, units, &mut edits);
        ui.separator();
        self.summary(ui, units, &mut edits);
        if self.route.is_some() {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Print map…")
                    .on_hover_text("The map as a page, with the route  (Ctrl+P)")
                    .clicked()
                {
                    self.print = Some(crate::printui::Kind::Map);
                }
                if ui
                    .button("Print directions…")
                    .on_hover_text("The directions as pages")
                    .clicked()
                {
                    self.print = Some(crate::printui::Kind::Directions);
                }
                later(ui, "Export…", "As a GPX or KML file");
            });
        }
        let left = ui.available_height();
        self.directions(ui, left.max(80.0), units, &mut edits);
        let mut fly = None;
        for e in edits {
            match e {
                Edit::Clear => self.apply(StopAction::Clear, Place::at([0.0, 0.0])),
                Edit::Move(a, b) => self.move_stop(a, b),
                Edit::Remove(i) => self.remove_stop(i),
                Edit::Reverse => self.reverse(),
                Edit::Avoid(flag, on) => self.set_avoid(flag, on),
                Edit::Optimize => self.optimize(),
                Edit::CancelOptimize => self.cancel_optimize(),
                Edit::Undo => self.undo_optimize(),
                Edit::Expand(leg, open) => self.expand(leg, open),
                Edit::Choose(leg, k) => self.choose(leg, k),
                Edit::Pick(i) => {
                    self.pick(Some(i));
                    fly = self.step_view(i, cam.size);
                }
                Edit::Mode(m) => {
                    let stops = self.stops();
                    let at = self.plan.anchor(stops.len());
                    let tz = stops.get(at).map(|(_, p)| self.tz_at(p.p));
                    let tz = tz.unwrap_or(jiff::tz::TimeZone::UTC);
                    self.plan.set_mode(m, &tz);
                    self.retime();
                }
                Edit::Retime => self.retime(),
                Edit::Stay(i, stay) => self.set_stay(i, stay),
                Edit::MakeOvernight(k) => self.make_overnight(k),
            }
        }
        fly
    }

    /// The stops, each with its handle, numbered pin, name and remove button.
    fn stop_rows(&mut self, ui: &mut egui::Ui, edits: &mut Vec<Edit>) {
        let stops: Vec<(StopAction, String)> = self
            .stops()
            .iter()
            .map(|(k, p)| (*k, p.name.clone().unwrap_or_default()))
            .collect();
        let roads = self.route.as_ref().map(|r| r.roads.clone());
        let places: Vec<_> = self.stops().iter().map(|(_, p)| p.p).collect();
        let n = stops.len();
        let dragging = egui::DragAndDrop::payload::<usize>(ui.ctx()).map(|p| *p);
        for (i, (kind, name)) in stops.iter().enumerate() {
            let row = ui.horizontal(|ui| {
                let (handle_rect, handle) =
                    ui.allocate_exact_size(Vec2::new(12.0, 18.0), Sense::drag());
                handle.dnd_set_drag_payload(i);
                let handle = handle.on_hover_text("Drag to change the order");
                self.targets.push((Target::Handle(i), handle_rect));
                if n > 1 {
                    let c = if handle.hovered() || dragging == Some(i) {
                        INK
                    } else {
                        WEAK
                    };
                    for k in 0..3 {
                        let y = handle_rect.center().y + (k as f32 - 1.0) * 4.0;
                        ui.painter().hline(
                            handle_rect.x_range().shrink(1.5),
                            y,
                            Stroke::new(1.5, c),
                        );
                    }
                }
                let fill = match kind {
                    StopAction::From => START,
                    StopAction::To => END,
                    _ => VIA,
                };
                numbered(ui, fill, i + 1);
                // A search result by its name; a point on the map by the road it is on.
                let road = roads
                    .as_ref()
                    .and_then(|r| r.get(i))
                    .filter(|r| !r.is_empty());
                let ll = geo::unit_to_lonlat(geo::to_unit(places[i]));
                let text = match (name.is_empty(), road) {
                    (false, _) => name.clone(),
                    (true, Some(r)) => format!("Near {r}"),
                    (true, None) => format!("{:.5}, {:.5}", ll[1], ll[0]),
                };
                let what = match kind {
                    StopAction::From => "Start",
                    StopAction::To => "Destination",
                    _ => "Stop",
                };
                let rest = ui.available_width() - 26.0;
                ui.allocate_ui(Vec2::new(rest.max(40.0), 18.0), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(text).color(INK)).truncate())
                        .on_hover_text(format!("{what}: {:.5}, {:.5}", ll[1], ll[0]));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = glyph_button(ui, Glyph::Close, 18.0).on_hover_text("Remove this stop");
                    self.targets.push((Target::Remove(i), r.rect));
                    if r.clicked() {
                        edits.push(Edit::Remove(i));
                    }
                });
            });
            let mut row = row.response;
            if let Some(r) = self.stop_times(ui, i, n, edits) {
                row = row.union(r);
            }
            self.targets.push((Target::Row(i), row.rect));
            if let Some(from) = row.dnd_hover_payload::<usize>()
                && *from != i
            {
                // Where the stop would go: above this row when it comes from below.
                let y = match *from > i {
                    true => row.rect.top(),
                    false => row.rect.bottom(),
                };
                ui.painter()
                    .hline(row.rect.x_range(), y, Stroke::new(2.0, START));
            }
            if let Some(from) = row.dnd_release_payload::<usize>() {
                edits.push(Edit::Move(*from, i));
            }
        }
    }

    /// Clear, Reverse and *Optimize…* in a row, a spinner while a route is looked for, and
    /// the avoid options under *More options*.
    fn options(&mut self, ui: &mut egui::Ui, edits: &mut Vec<Edit>) {
        let full = self.from.is_some() && self.to.is_some();
        ui.horizontal(|ui| {
            let r = ui
                .add_enabled(self.has_stops(), egui::Button::new("Clear"))
                .on_hover_text("Remove every stop and the route");
            self.targets.push((Target::Clear, r.rect));
            if r.clicked() {
                edits.push(Edit::Clear);
            }
            let r = ui
                .add_enabled(full, egui::Button::new("Reverse"))
                .on_hover_text("Swap the start and the destination, and reverse the stops between");
            self.targets.push((Target::Reverse, r.rect));
            if r.clicked() {
                edits.push(Edit::Reverse);
            }
            let r = ui
                .add_enabled(
                    full,
                    egui::Button::new("Optimize…").selected(self.optimize_open),
                )
                .on_hover_text("Put the stops in the order that makes the trip quickest");
            self.targets.push((Target::OptimizeWindow, r.rect));
            if r.clicked() {
                self.optimize_open = !self.optimize_open;
            }
            if self.waiting.is_some() {
                ui.add(egui::Spinner::new().size(12.0).color(WEAK));
            }
        });
        let open = self.more_open;
        let hover = match open {
            true => "Hide these options",
            false => "Avoid tolls or ferries",
        };
        let r = fold_row(
            ui,
            "more-options",
            open,
            &more_title(self.avoid),
            Some(hover),
        );
        self.targets.push((Target::MoreOptions, r.rect));
        if r.clicked() {
            self.more_open = !open;
        }
        if open {
            ui.indent("more-options", |ui| {
                for (flag, label) in [(TOLL, "Avoid tolls"), (FERRY, "Avoid ferries")] {
                    let mut on = self.avoid & flag != 0;
                    let r = ui.checkbox(&mut on, label);
                    self.targets.push((Target::Avoid(flag), r.rect));
                    if r.changed() {
                        edits.push(Edit::Avoid(flag, on));
                    }
                }
            });
        }
        self.times_section(ui, edits);
    }

    /// Under stop `i`'s row, with a time set: when it arrives and leaves, and for a stop
    /// between the ends, its time there, which opens a field to change it.
    fn stop_times(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        n: usize,
        edits: &mut Vec<Edit>,
    ) -> Option<egui::Response> {
        let t = self.timed()?;
        let days = t.sched.days() > 1;
        let (arrive, depart) = (
            t.sched.arrive.get(i)?.clone(),
            t.sched.depart.get(i)?.clone(),
        );
        let main = arrive.as_ref().or(depart.as_ref())?;
        let shown = t.show(main, days);
        let text = match (&arrive, &depart) {
            (None, Some(_)) => format!("Leaves {}", shown.text),
            (Some(_), None) => format!("Arrives {}", shown.text),
            (Some(a), Some(d)) if a.timestamp() == d.timestamp() => {
                format!("Arrives {}", shown.text)
            }
            (Some(_), Some(d)) => {
                let same_day = arrive.as_ref().is_some_and(|a| a.date() == d.date());
                let leaves = super::times::clock(d, days && !same_day);
                format!("Arrives {}, leaves {leaves}", shown.text)
            }
            (None, None) => return None,
        };
        let color = if shown.against.is_some() {
            ACCENT
        } else {
            WEAK
        };
        let stay = self.stops().get(i).map(|(_, p)| p.stay).unwrap_or_default();
        let between = i > 0 && i + 1 < n;
        // Under the name, wrapping there: the times, and on a line of their own the
        // difference from the departure's zone and the time spent there.
        let r = ui.horizontal(|ui| {
            ui.add_space(38.0);
            ui.vertical(|ui| {
                let label = ui.label(egui::RichText::new(text).size(11.5).color(color));
                if let Some(h) = &shown.hover {
                    label.on_hover_text(h);
                }
                if shown.against.is_none() && !between {
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    if let Some(a) = &shown.against {
                        ui.label(egui::RichText::new(a).size(11.5).color(ACCENT));
                    }
                    if !between {
                        return;
                    }
                    let chip = match (stay.overnight, stay.s) {
                        (true, _) => "overnight".to_string(),
                        (false, s) if s > 0.0 => format!("{} here", duration_text(s)),
                        _ => "time here…".into(),
                    };
                    let r = ui
                        .add(egui::Button::new(egui::RichText::new(chip).size(11.5)).small())
                        .on_hover_text("Time spent at this stop");
                    self.targets.push((Target::Stay(i), r.rect));
                    if r.clicked() {
                        self.stay_open = match self.stay_open == Some(i) {
                            true => None,
                            false => Some(i),
                        };
                    }
                });
            });
        });
        if between && self.stay_open == Some(i) {
            ui.horizontal(|ui| {
                ui.add_space(38.0);
                let mut s = stay;
                let mut min = (s.s / 60.0).round();
                ui.label(egui::RichText::new("Time here").size(11.5).color(INK));
                let r = ui.add_enabled(
                    !s.overnight,
                    egui::DragValue::new(&mut min)
                        .range(0.0..=24.0 * 60.0)
                        .speed(1.0)
                        .suffix(" min"),
                );
                if r.changed() {
                    s.s = min * 60.0;
                }
                let r = ui
                    .checkbox(&mut s.overnight, "Overnight")
                    .on_hover_text("Stay the night here: the trip goes on at the next day's start");
                self.targets.push((Target::Overnight(i), r.rect));
                if s != stay {
                    edits.push(Edit::Stay(i, s));
                }
            });
        }
        Some(r.response)
    }

    /// *Depart at / Arrive by*: closed by default, and closed or opened like *More
    /// options* whether a time is set or not; closed, its title says what is set. Off, a departure time or an arrival time, in the zone of the stop it is at;
    /// the driving day; the breaks.
    fn times_section(&mut self, ui: &mut egui::Ui, edits: &mut Vec<Edit>) {
        let set = self.plan.mode != Mode::Off;
        let open = self.times_open;
        let title = self.times_title();
        let hover = match open {
            true => "Hide these options",
            false => "Time the trip: when to leave or arrive, and the hours to drive",
        };
        let r = fold_row(ui, "times", open, &title, Some(hover));
        self.targets.push((Target::Times, r.rect));
        if r.clicked() {
            self.times_open = !open;
        }
        if !open {
            return;
        }
        let mut changed = false;
        ui.indent("times", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (m, label) in [
                    (Mode::Off, "No time"),
                    (Mode::Depart, "Depart at"),
                    (Mode::ArriveBy, "Arrive by"),
                ] {
                    let r = ui.radio(self.plan.mode == m, label);
                    self.targets.push((Target::Mode(m), r.rect));
                    if r.clicked() && self.plan.mode != m {
                        edits.push(Edit::Mode(m));
                    }
                }
            });
            if set {
                ui.horizontal_wrapped(|ui| {
                    let bad_date = self.plan.date.trim().parse::<jiff::civil::Date>().is_err();
                    changed |= field(ui, &mut self.plan.date, 96.0, bad_date, "2026-11-01")
                        .on_hover_text("The date, year-month-day")
                        .changed();
                    let bad = parse_time(&self.plan.time).is_none();
                    changed |= field(ui, &mut self.plan.time, 52.0, bad, "08:00")
                        .on_hover_text("The time, 24-hour")
                        .changed();
                    let at = match self.plan.mode {
                        Mode::ArriveBy => "at the destination",
                        _ => "at the start",
                    };
                    let zone = self.timed().and_then(|t| match self.plan.mode {
                        Mode::ArriveBy => t.sched.arrival(),
                        _ => t.sched.departure(),
                    });
                    let text = match zone {
                        Some(z) => format!("{}, {at}", z.strftime("%Z")),
                        None => at.to_string(),
                    };
                    ui.label(egui::RichText::new(text).size(11.5).color(WEAK));
                });
                if self.plan.time.trim() != self.plan.s.depart
                    && parse_time(&self.plan.time).is_some()
                {
                    self.plan.s.depart = self.plan.time.trim().to_owned();
                }
            }
            let s = &mut self.plan.s;
            ui.horizontal_wrapped(|ui| {
                changed |= ui.checkbox(&mut s.window, "Drive from").changed();
                let bad = parse_time(&s.day_start).is_none();
                changed |= field(ui, &mut s.day_start, 52.0, bad, "08:00").changed();
                ui.label("to");
                let bad = parse_time(&s.day_end).is_none();
                changed |= field(ui, &mut s.day_end, 52.0, bad, "18:00").changed();
            });
            ui.horizontal_wrapped(|ui| {
                changed |= ui.checkbox(&mut s.max_drive, "At most").changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut s.max_drive_h)
                            .range(1.0..=24.0)
                            .speed(0.25)
                            .max_decimals(2)
                            .suffix(" h"),
                    )
                    .changed();
                ui.label("of driving a day");
            });
            ui.horizontal_wrapped(|ui| {
                changed |= ui.checkbox(&mut s.breaks, "A break every").changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut s.break_every_h)
                            .range(0.5..=12.0)
                            .speed(0.25)
                            .max_decimals(2)
                            .suffix(" h"),
                    )
                    .changed();
                ui.label("for");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut s.break_min)
                            .range(5.0..=240.0)
                            .speed(1.0)
                            .suffix(" min"),
                    )
                    .changed();
            });
            if let Some(e) = self.times_failed() {
                ui.label(egui::RichText::new(e).size(11.5).color(APPROX));
            }
            if let Some(t) = self.timed().filter(|t| t.sched.waited) {
                let d = t.sched.departure().map(|z| clock_short(z, false));
                let text = format!(
                    "Leaves at {}, the start of the driving day.",
                    d.unwrap_or_default()
                );
                ui.label(egui::RichText::new(text).size(11.5).color(WEAK));
            }
            if set {
                ui.label(
                    egui::RichText::new("Times assume no traffic.")
                        .size(11.5)
                        .color(WEAK),
                );
            }
        });
        if changed {
            edits.push(Edit::Retime);
        }
    }

    /// *Depart at / Arrive by*'s title: the time set, in its stop's zone, "Depart Sat Oct
    /// 10, 08:00 EDT".
    fn times_title(&self) -> String {
        let verb = match self.plan.mode {
            Mode::Off => return "Depart at / Arrive by".into(),
            Mode::Depart => "Depart",
            Mode::ArriveBy => "Arrive by",
        };
        let tz = self.timed().and_then(|t| match self.plan.mode {
            Mode::ArriveBy => t.sched.arrival(),
            _ => t.sched.departure(),
        });
        let date = self.plan.date.trim().parse::<jiff::civil::Date>().ok();
        let asked = match (tz, date, parse_time(&self.plan.time)) {
            (Some(z), Some(d), Some(t)) => z.time_zone().to_zoned(d.to_datetime(t)).ok(),
            _ => None,
        };
        match asked {
            Some(z) => format!("{verb} {}", z.strftime("%a %b %-d, %H:%M %Z")),
            None => verb.into(),
        }
    }

    /// The *Optimize stop order* window: the ends it keeps, Optimize (or Cancel while it
    /// runs), and what it did last, with Undo. Closing it lets a search running finish.
    fn optimize_window(
        &mut self,
        ctx: &egui::Context,
        at: Pos2,
        units: Units,
        edits: &mut Vec<Edit>,
    ) {
        if !self.optimize_open {
            return;
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new("Optimize stop order")
            .open(&mut open)
            .collapsible(false)
            .auto_sized()
            .pivot(Align2::LEFT_TOP)
            .default_pos(at)
            .show(ctx, |ui| {
                ui.set_max_width(280.0);
                let round = self.ends.round_trip;
                let boxes = [
                    (Target::KeepFirst, "Keep first stop", round),
                    (Target::KeepLast, "Keep last stop", round),
                    (Target::RoundTrip, "Round trip", false),
                ];
                for (target, label, disabled) in boxes {
                    let ends = &mut self.ends;
                    let on = match target {
                        Target::KeepFirst => &mut ends.keep_first,
                        Target::KeepLast => &mut ends.keep_last,
                        _ => &mut ends.round_trip,
                    };
                    let r = ui.add_enabled(!disabled, egui::Checkbox::new(on, label));
                    let r = match target {
                        Target::RoundTrip => r.on_hover_text("End back at the first stop"),
                        _ => r.on_disabled_hover_text(
                            "A round trip starts and ends at the first stop",
                        ),
                    };
                    self.targets.push((target, r.rect));
                }
                if let Some(done) = &self.reordered {
                    ui.add_space(4.0);
                    let text = match done {
                        Reordered::Saved { time_s, metres, .. } => {
                            let d = distance_text(metres.abs(), units);
                            let m = match *metres < 0.0 {
                                true => format!("; {d} longer"),
                                false => format!(", {d}"),
                            };
                            format!("Saves {}{m}", duration_text(time_s.max(0.0)))
                        }
                        Reordered::Already => "The stops are already in the quickest order.".into(),
                        Reordered::Failed(e) => format!("Could not optimize: {e}"),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(text).color(INK));
                        if matches!(done, Reordered::Saved { .. }) {
                            let r = ui
                                .small_button("Undo")
                                .on_hover_text("Put the stops back in their order before");
                            self.targets.push((Target::Undo, r.rect));
                            if r.clicked() {
                                edits.push(Edit::Undo);
                            }
                        }
                    });
                }
                let blocked = self.optimize_blocked();
                if let Some(why) = blocked.filter(|_| self.optimizing.is_none()) {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(why).color(WEAK));
                }
                // Close at the window's right edge (which its title may set): a right-to-left
                // layout would stretch the window to its widest.
                let width = ui.min_rect().width().max(ui.available_width());
                ui.separator();
                ui.horizontal(|ui| {
                    if self.optimizing.is_some() {
                        let r = ui
                            .button("Cancel")
                            .on_hover_text("Stop looking for a quicker order");
                        self.targets.push((Target::CancelOptimize, r.rect));
                        if r.clicked() {
                            edits.push(Edit::CancelOptimize);
                        }
                        ui.add(egui::Spinner::new().size(12.0).color(WEAK));
                    } else {
                        let r = ui
                            .add_enabled(blocked.is_none(), egui::Button::new("Optimize"))
                            .on_hover_text("Reorder the stops, keeping the ends checked");
                        self.targets.push((Target::Optimize, r.rect));
                        if r.clicked() {
                            edits.push(Edit::Optimize);
                        }
                    }
                    let close_w = egui::WidgetText::from("Close")
                        .into_galley(
                            ui,
                            Some(egui::TextWrapMode::Extend),
                            f32::INFINITY,
                            egui::TextStyle::Button,
                        )
                        .size()
                        .x
                        + 2.0 * ui.spacing().button_padding.x;
                    let used = ui.min_rect().width() + ui.spacing().item_spacing.x;
                    ui.add_space((width - used - close_w).max(0.0));
                    let r = ui.button("Close");
                    self.targets.push((Target::CloseOptimize, r.rect));
                    close = r.clicked();
                });
            });
        self.optimize_open = open && !close;
    }

    /// The totals, each leg's row, and the notices.
    fn summary(&mut self, ui: &mut egui::Ui, units: Units, edits: &mut Vec<Edit>) {
        let Some(r) = &self.route else {
            let text = match (&self.error, self.from.is_some() && self.to.is_some()) {
                (None, _) if !self.has_stops() => {
                    "Pick a search result or right-click the map: “Route from here”, “Route to here”."
                        .into()
                }
                (Some(e), _) => format!("Routing failed: {e}"),
                (None, true) => "Routing…".into(),
                (None, false) if self.from.is_none() => {
                    "Pick a search result or right-click the map: “Route from here”.".into()
                }
                (None, false) => "Pick a search result or right-click the map: “Route to here”.".into(),
            };
            ui.label(egui::RichText::new(text).color(WEAK));
            return;
        };
        let trip = &r.trip;
        ui.label(
            egui::RichText::new(format!(
                "{} · {}",
                distance_text(trip.metres(), units),
                duration_text(trip.time_s())
            ))
            .size(16.0)
            .strong()
            .color(INK),
        );
        self.leg_rows(ui, units, edits);
        for n in self.notices(units) {
            ui.label(egui::RichText::new(format!("⚠ {n}")).color(APPROX));
        }
    }

    /// Each leg's row, "1 to 2: 285 mi · 4 h 10 min", with a triangle: clicked, it opens
    /// the leg's routes or closes them.
    fn leg_rows(&mut self, ui: &mut egui::Ui, units: Units, edits: &mut Vec<Edit>) {
        let Some(r) = &self.route else { return };
        let legs: Vec<Option<(f64, f64)>> = r
            .trip
            .legs
            .iter()
            .map(|l| l.as_ref().map(|l| (l.metres, l.time_s)))
            .collect();
        for (li, leg) in legs.into_iter().enumerate() {
            let open = self.expanded.get(li).copied().unwrap_or(false);
            let via = match self.alts.get(li) {
                Some(LegAlts::Found(c)) if c.routes.len() > 1 => {
                    let road = &c.routes[self.chosen[li]].road;
                    match road.is_empty() {
                        true => String::new(),
                        false => format!(" · via {road}"),
                    }
                }
                _ => String::new(),
            };
            let text = match leg {
                Some((m, t)) => format!(
                    "{} to {}: {} · {}{via}",
                    li + 1,
                    li + 2,
                    distance_text(m, units),
                    duration_text(t)
                ),
                None => format!("{} to {}: no route", li + 1, li + 2),
            };
            let hover = match open {
                true => "Hide the other routes",
                false => "Show other routes for this leg",
            };
            let resp = fold_row(ui, ("leg", li), open, &text, leg.map(|_| hover));
            self.targets.push((Target::Leg(li), resp.rect));
            if resp.clicked() {
                edits.push(Edit::Expand(li, !open));
            }
            if open {
                ui.indent(("routes", li), |ui| self.leg_routes(ui, li, units, edits));
            }
        }
    }

    /// An open leg's routes, the one shown selected: time, distance, against the quickest,
    /// and the road it takes that the others do not.
    fn leg_routes(&mut self, ui: &mut egui::Ui, li: usize, units: Units, edits: &mut Vec<Edit>) {
        let c = match &self.alts[li] {
            LegAlts::NotAsked => return,
            LegAlts::Looking(_) => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(WEAK));
                    ui.label(egui::RichText::new("Checking for other routes…").color(WEAK));
                });
                return;
            }
            LegAlts::Failed(e) => {
                let text = format!("Other routes could not be found: {e}");
                ui.label(egui::RichText::new(text).color(APPROX));
                return;
            }
            LegAlts::Found(c) => c,
        };
        if c.routes.len() < 2 {
            let text = "No other route within a quarter of the quickest's time.";
            ui.label(egui::RichText::new(text).size(12.0).color(WEAK));
            return;
        }
        let quickest = c.routes[0].leg.time_s;
        let mut targets = Vec::new();
        for (k, x) in c.routes.iter().enumerate() {
            let against = match k {
                0 => "quickest".to_string(),
                _ => delta_text(x.leg.time_s - quickest),
            };
            let via = match x.road.is_empty() {
                true => String::new(),
                false => format!(" · via {}", x.road),
            };
            let text = format!(
                "{} · {} · {against}{via}",
                duration_text(x.leg.time_s),
                distance_text(x.leg.metres, units)
            );
            let shown = self.chosen.get(li) == Some(&k);
            let r = ui
                .selectable_label(shown, egui::RichText::new(text).color(INK))
                .on_hover_text(format!(
                    "{} turns, {} U-turns",
                    x.turns.turns, x.turns.uturns
                ));
            targets.push((Target::Alternative(li, k), r.rect));
            if r.clicked() && !shown {
                edits.push(Edit::Choose(li, k));
            }
        }
        if let Some(why) = ranked_note(c) {
            ui.label(egui::RichText::new(why).size(12.0).color(WEAK));
        }
        self.targets.extend(targets);
    }

    /// The steps, only those in view laid out and drawn: a long route has hundreds.
    fn directions(
        &mut self,
        ui: &mut egui::Ui,
        max_height: f32,
        units: Units,
        edits: &mut Vec<Edit>,
    ) {
        let Some(r) = &self.route else { return };
        if r.steps.is_empty() {
            return;
        }
        ui.separator();
        let width = ui.available_width();
        if self
            .rows
            .as_ref()
            .is_none_or(|c| c.width != width || c.units != units)
        {
            let timed = self.timed().is_some();
            let rows = Rows::new(ui, &r.steps, &self.notes, timed, width, units);
            self.rows = Some(rows);
        }
        let timed = self.timed();
        let rows = self.rows.as_ref().unwrap();
        let total = rows.heights.iter().sum::<f32>();
        let mut targets = Vec::new();
        // Asked for its height, so it fills the rest of the page rather than shrinking to
        // last frame's size.
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .min_scrolled_height(total.min(max_height))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let (all, _) = ui.allocate_exact_size(Vec2::new(width, total), Sense::hover());
                let clip = ui.clip_rect();
                let mut y = all.top();
                for (row, &h) in rows.rows.iter().zip(&rows.heights) {
                    let rect = Rect::from_min_size(Pos2::new(all.left(), y), Vec2::new(width, h));
                    y += h;
                    if rect.bottom() < clip.top() || rect.top() > clip.bottom() {
                        continue;
                    }
                    match *row {
                        Row::Step(i) => {
                            let s = &r.steps[i];
                            let time = timed.and_then(|t| Some(t.show(t.steps.get(i)?, false)));
                            let picked = self.picked == Some(i);
                            let resp = step_row(ui, rect, i, s, &rows.subs[i], picked, time);
                            targets.push((Target::Step(i), resp.rect));
                            if resp.clicked() {
                                edits.push(Edit::Pick(i));
                            }
                        }
                        Row::Note(k) => {
                            let note = &self.notes[k];
                            let time = match (timed, &note.time) {
                                (Some(t), Some(z)) => Some(t.show(z, false)),
                                _ => None,
                            };
                            if let Some((event, button)) = note_row(ui, rect, k, note, time) {
                                targets.push((Target::MakeOvernight(event), button.rect));
                                if button.clicked() {
                                    edits.push(Edit::MakeOvernight(event));
                                }
                            }
                        }
                    }
                }
            });
        self.targets.extend(targets);
    }
}

/// Which route turn costs rank best when it is not the quickest (decisions 3 and 23):
/// "With turn costs, the route via US 33 ranks best: 1 min slower on paper, 2 fewer
/// U-turns."
/// *More options*' title, which says what is avoided, so that closed it still says why a
/// route leaves out tolls.
fn more_title(avoid: u8) -> String {
    let avoiding: Vec<&str> = [(TOLL, "tolls"), (FERRY, "ferries")]
        .into_iter()
        .filter(|(f, _)| avoid & f != 0)
        .map(|(_, what)| what)
        .collect();
    match avoiding.is_empty() {
        true => "More options".into(),
        false => format!("More options: avoiding {}", avoiding.join(" and ")),
    }
}

fn ranked_note(c: &Choices) -> Option<String> {
    if c.best == 0 {
        return None;
    }
    let (b, q) = (&c.routes[c.best], &c.routes[0]);
    let fewer = |n: u32, m: u32, what: &str| {
        let d = n.saturating_sub(m);
        (d > 0).then(|| format!("{d} fewer {what}{}", if d == 1 { "" } else { "s" }))
    };
    let why = fewer(q.turns.uturns, b.turns.uturns, "U-turn")
        .or_else(|| fewer(q.turns.turns, b.turns.turns, "turn"))
        .unwrap_or_else(|| "gentler turns".into());
    let slower = b.leg.time_s - q.leg.time_s;
    let paper = match (slower / 60.0).round() {
        0.0 => "the same time on paper".to_string(),
        _ => format!("{} slower on paper", duration_text(slower)),
    };
    let which = match b.road.is_empty() {
        true => format!("route {}", c.best + 1),
        false => format!("the route via {}", b.road),
    };
    Some(format!(
        "With turn costs, {which} ranks best: {paper}, {why}."
    ))
}

/// A small triangle at `c`: pointing right when closed, down when open.
/// A row that opens and closes what is under it, with a triangle that says which: a leg's
/// routes and *More options*. Without `hover` it is only a label, with no triangle.
fn fold_row(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    open: bool,
    text: &str,
    hover: Option<&str>,
) -> egui::Response {
    let row = ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 16.0), Sense::hover());
        if hover.is_some() {
            triangle(ui.painter(), rect.center(), open, WEAK);
        }
        ui.add(egui::Label::new(egui::RichText::new(text).color(INK)).truncate());
    });
    let rect = row.response.rect;
    let Some(hover) = hover else {
        return row.response;
    };
    let resp = ui
        .interact(rect, ui.scope_id().with(id), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hover);
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, Color32::from_black_alpha(8));
    }
    resp
}

fn triangle(p: &egui::Painter, c: Pos2, open: bool, color: Color32) {
    let pts = match open {
        true => vec![
            c + Vec2::new(-4.0, -2.0),
            c + Vec2::new(4.0, -2.0),
            c + Vec2::new(0.0, 3.0),
        ],
        false => vec![
            c + Vec2::new(-2.0, -4.0),
            c + Vec2::new(3.0, 0.0),
            c + Vec2::new(-2.0, 4.0),
        ],
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

impl RouteUi {
    /// The directions as a printout holds them, with `label` for its header.
    pub fn directions_doc(&self, label: &str, units: Units) -> Option<DirectionsDoc> {
        let r = self.route.as_ref().filter(|r| !r.steps.is_empty())?;
        let timed = self.timed();
        let stops = self
            .stops()
            .iter()
            .enumerate()
            .map(|(i, (kind, place))| {
                let fill = match kind {
                    StopAction::From => START,
                    StopAction::To => END,
                    _ => VIA,
                };
                let name = place
                    .name
                    .clone()
                    .or_else(|| r.roads.get(i).filter(|r| !r.is_empty()).cloned())
                    .unwrap_or_else(|| {
                        let [lon, lat] = geo::unit_to_lonlat(geo::to_unit(place.p));
                        format!("{lat:.4}, {lon:.4}")
                    });
                (fill, name)
            })
            .collect();
        let step_times = (0..r.steps.len())
            .map(|i| timed.and_then(|t| Some(t.show(t.steps.get(i)?, false))))
            .collect();
        let note_times = self
            .notes
            .iter()
            .map(|n| match (timed, &n.time) {
                (Some(t), Some(z)) => Some(t.show(z, false)),
                _ => None,
            })
            .collect();
        Some(DirectionsDoc {
            label: label.to_owned(),
            stops,
            totals: format!(
                "{} · {}",
                distance_text(r.trip.metres(), units),
                duration_text(r.trip.time_s())
            ),
            steps: r.steps.clone(),
            notes: self.notes.clone(),
            step_times,
            note_times,
            units,
        })
    }
}

/// What the directions' printout holds (phase 4 step 4.9), all of it plain data so a page can
/// be drawn on another thread: the stops, the totals, and the rows as the route page has them.
#[derive(Clone)]
pub struct DirectionsDoc {
    pub label: String,
    /// Each stop's pin colour and name.
    pub stops: Vec<(Color32, String)>,
    pub totals: String,
    pub steps: Vec<Step>,
    pub notes: Vec<Note>,
    /// The clock of each step and each note, when a time is set.
    pub step_times: Vec<Option<super::times::Shown>>,
    pub note_times: Vec<Option<super::times::Shown>>,
    pub units: Units,
}

/// The directions' rows laid out for a printed page `width` points wide.
pub struct PrintRows {
    rows: Rows,
}

impl PrintRows {
    pub fn new(ui: &egui::Ui, doc: &DirectionsDoc, width: f32) -> Self {
        let timed = doc.step_times.iter().any(Option::is_some);
        Self {
            rows: Rows::new(ui, &doc.steps, &doc.notes, timed, width, doc.units),
        }
    }

    /// Each row's height.
    pub fn heights(&self) -> &[f32] {
        &self.rows.heights
    }

    /// Draws row `i` in `rect`, as the route page does but without a click or a hover.
    pub fn paint(&self, ui: &mut egui::Ui, rect: Rect, doc: &DirectionsDoc, i: usize) {
        match self.rows.rows[i] {
            Row::Step(s) => {
                let time = doc.step_times.get(s).cloned().flatten();
                step_row(ui, rect, s, &doc.steps[s], &self.rows.subs[s], false, time);
            }
            Row::Note(k) => {
                let mut note = doc.notes[k].clone();
                note.movable = false;
                let time = doc.note_times.get(k).cloned().flatten();
                note_row(ui, rect, k, &note, time);
            }
        }
    }
}

/// A row of the directions: a step, or a row the schedule adds (a night, a break, a zone
/// crossing).
#[derive(Clone, Copy, Debug)]
enum Row {
    Step(usize),
    Note(usize),
}

/// The directions' rows as laid out for a width: their order and heights, and the distance
/// and time under each instruction.
pub(super) struct Rows {
    width: f32,
    units: Units,
    rows: Vec<Row>,
    heights: Vec<f32>,
    subs: Vec<Option<String>>,
}

impl Rows {
    fn new(
        ui: &egui::Ui,
        steps: &[Step],
        notes: &[Note],
        timed: bool,
        width: f32,
        units: Units,
    ) -> Self {
        let subs: Vec<Option<String>> = steps
            .iter()
            .map(|s| {
                (s.metres > 0.0).then(|| {
                    format!(
                        "{} · {}",
                        distance_text(s.metres, units),
                        duration_text(s.time_s)
                    )
                })
            })
            .collect();
        let text_w = text_width(width, timed);
        let mut rows = Vec::with_capacity(steps.len() + notes.len());
        let mut next = 0;
        for i in 0..steps.len() {
            rows.push(Row::Step(i));
            while notes.get(next).is_some_and(|n| n.after <= i) {
                rows.push(Row::Note(next));
                next += 1;
            }
        }
        rows.extend((next..notes.len()).map(Row::Note));
        let p = ui.painter();
        let heights = rows
            .iter()
            .map(|row| {
                let h = match *row {
                    Row::Step(i) => {
                        let g = p.layout(steps[i].text(), STEP_FONT, INK, text_w);
                        g.size().y + if subs[i].is_some() { SUB_H } else { 0.0 } + 8.0
                    }
                    Row::Note(k) => {
                        let n = &notes[k];
                        let g = p.layout(n.text.clone(), STEP_FONT, INK, text_w);
                        let sub = n.sub.as_ref().map_or(0.0, |t| {
                            p.layout(t.clone(), SMALL, WEAK, text_w).size().y + 1.0
                        });
                        let button = if n.movable { BUTTON_H + 4.0 } else { 0.0 };
                        g.size().y + sub + button + 8.0
                    }
                };
                h.max(ICON + 8.0)
            })
            .collect();
        Self {
            width,
            units,
            rows,
            heights,
            subs,
        }
    }
}

/// The width of a row's text: less the clock column when a time is set.
fn text_width(width: f32, timed: bool) -> f32 {
    width - TEXT_X - if timed { TIME_W } else { 0.0 }
}

/// "14:35", or with `day` "Sun 08:00": a time beside one that gave its zone.
fn clock_short(z: &jiff::Zoned, day: bool) -> String {
    match day {
        true => z.strftime("%a %H:%M").to_string(),
        false => z.strftime("%H:%M").to_string(),
    }
}

/// A short text field, outlined in the warning colour when `bad`.
fn field(
    ui: &mut egui::Ui,
    text: &mut String,
    width: f32,
    bad: bool,
    hint: &str,
) -> egui::Response {
    let mut edit = egui::TextEdit::singleline(text)
        .desired_width(width)
        .hint_text(hint);
    if bad {
        edit = edit.background_color(Color32::from_rgb(253, 236, 220));
    }
    ui.add(edit)
}

/// A stop's number in a disc of its pin's colour.
fn numbered(ui: &mut egui::Ui, fill: Color32, n: usize) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 8.5, fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        n.to_string(),
        FontId::proportional(11.0),
        Color32::WHITE,
    );
}

/// A step in `rect`: its icon, instruction, and the distance and time to the next.
fn step_row(
    ui: &mut egui::Ui,
    rect: Rect,
    i: usize,
    s: &Step,
    sub: &Option<String>,
    picked: bool,
    time: Option<super::times::Shown>,
) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(("step", i)), Sense::click());
    let p = ui.painter();
    if picked {
        p.rect_filled(rect, 3.0, PICKED_FILL);
    } else if resp.hovered() {
        p.rect_filled(rect, 3.0, Color32::from_black_alpha(10));
    }
    let icon = Rect::from_min_size(rect.min + Vec2::new(2.0, 4.0), Vec2::splat(ICON));
    draw_icon(p, icon, s);
    let text_w = text_width(rect.width(), time.is_some());
    let galley = p.layout(s.text(), STEP_FONT, INK, text_w);
    let x = rect.left() + TEXT_X - 4.0;
    let text_h = galley.size().y;
    p.galley(Pos2::new(x, rect.top() + 4.0), galley, INK);
    if let Some(sub) = sub {
        p.text(
            Pos2::new(x, rect.top() + 5.0 + text_h),
            Align2::LEFT_TOP,
            sub,
            FontId::proportional(11.5),
            WEAK,
        );
    }
    p.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(0.5, Color32::from_black_alpha(25)),
    );
    if let Some(t) = time {
        time_cell(ui, rect, ("time", i), &t);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row's clock, at its top right: in the accent colour, with its tip, in another zone
/// than the departure's.
fn time_cell(
    ui: &mut egui::Ui,
    rect: Rect,
    id: impl std::hash::Hash + std::fmt::Debug,
    t: &super::times::Shown,
) {
    let color = if t.against.is_some() { ACCENT } else { WEAK };
    let at = Pos2::new(rect.right() - 4.0, rect.top() + 6.0);
    let r = ui
        .painter()
        .text(at, Align2::RIGHT_TOP, &t.text, SMALL, color);
    if let Some(h) = &t.hover {
        let tip = match &t.against {
            Some(a) => format!("{}, {a}\n{h}", t.text),
            None => h.clone(),
        };
        ui.interact(r, ui.scope_id().with(id), Sense::hover())
            .on_hover_text(tip);
    }
}

/// A row the schedule adds to the directions: its icon, text and clock, and under a night
/// along a leg, *Make overnight stop*. Returns that button and the night's event.
fn note_row(
    ui: &mut egui::Ui,
    rect: Rect,
    k: usize,
    n: &Note,
    time: Option<super::times::Shown>,
) -> Option<(usize, egui::Response)> {
    let p = ui.painter().clone();
    p.rect_filled(rect.shrink2(Vec2::new(0.0, 1.0)), 3.0, NOTE_FILL);
    let c = rect.min + Vec2::new(2.0 + ICON / 2.0, 4.0 + ICON / 2.0);
    match n.kind {
        NoteKind::Night(_) => {
            p.circle_filled(c, 8.5, super::NIGHT);
            let day = n.text.split(' ').nth(1).unwrap_or_default().to_string();
            p.text(
                c,
                Align2::CENTER_CENTER,
                day,
                FontId::proportional(10.5),
                Color32::WHITE,
            );
        }
        NoteKind::Break => {
            for dx in [-2.5, 2.5] {
                p.line_segment(
                    [c + Vec2::new(dx, -5.0), c + Vec2::new(dx, 5.0)],
                    Stroke::new(2.2, INK),
                );
            }
        }
        NoteKind::Zone => {
            p.circle_stroke(c, 7.0, Stroke::new(1.6, INK));
            p.line_segment([c, c + Vec2::new(0.0, -4.5)], Stroke::new(1.6, INK));
            p.line_segment([c, c + Vec2::new(3.5, 0.0)], Stroke::new(1.6, INK));
        }
    }
    let text_w = text_width(rect.width(), time.is_some());
    let x = rect.left() + TEXT_X - 4.0;
    let mut y = rect.top() + 4.0;
    let g = p.layout(n.text.clone(), STEP_FONT, INK, text_w);
    y += g.size().y;
    p.galley(Pos2::new(x, rect.top() + 4.0), g, INK);
    if let Some(sub) = &n.sub {
        let g = p.layout(sub.clone(), SMALL, WEAK, text_w);
        let h = g.size().y;
        p.galley(Pos2::new(x, y + 1.0), g, WEAK);
        y += h + 1.0;
    }
    if let Some(t) = time {
        time_cell(ui, rect, ("note-time", k), &t);
    }
    let NoteKind::Night(event) = n.kind else {
        return None;
    };
    if !n.movable {
        return None;
    }
    let label = "Make overnight stop";
    let g = p.layout_no_wrap(label.into(), SMALL, INK);
    let button = Rect::from_min_size(
        Pos2::new(x, y + 3.0),
        Vec2::new(g.size().x + 14.0, BUTTON_H),
    );
    let resp = ui
        .interact(button, ui.scope_id().with(("overnight", k)), Sense::click())
        .on_hover_text("Add a stop here that stays the night");
    let fill = match resp.hovered() {
        true => Color32::from_gray(232),
        false => Color32::from_gray(244),
    };
    p.rect(
        button,
        3.0,
        fill,
        Stroke::new(1.0, Color32::from_gray(190)),
        egui::StrokeKind::Inside,
    );
    p.galley(button.center() - g.size() / 2.0, g, INK);
    Some((event, resp.on_hover_cursor(egui::CursorIcon::PointingHand)))
}

/// A step's icon: an arrow bent by the turn, a ring for roundabouts, a boat for ferries,
/// and the stops' discs.
fn draw_icon(p: &egui::Painter, r: Rect, s: &Step) {
    let c = r.center();
    let ink = Stroke::new(2.2, INK);
    let disc = |fill: Color32, label: String| {
        p.circle_filled(c, 8.5, fill);
        p.text(
            c,
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(10.5),
            Color32::WHITE,
        );
    };
    match s.kind {
        Kind::Depart if s.leg == 0 => disc(START, "1".into()),
        Kind::Depart => disc(VIA, (s.leg + 1).to_string()),
        Kind::Stop => disc(VIA, s.stop.to_string()),
        Kind::Arrive => disc(END, s.stop.to_string()),
        Kind::Ferry | Kind::OffFerry => {
            let hull = vec![
                c + Vec2::new(-8.0, 0.0),
                c + Vec2::new(8.0, 0.0),
                c + Vec2::new(5.0, 5.0),
                c + Vec2::new(-5.0, 5.0),
            ];
            p.add(egui::Shape::convex_polygon(hull, INK, Stroke::NONE));
            p.rect_filled(
                Rect::from_min_size(c + Vec2::new(-4.0, -6.0), Vec2::new(7.0, 5.0)),
                1.0,
                INK,
            );
            let wave: Vec<Pos2> = (0..=16)
                .map(|k| {
                    let x = k as f32 - 8.0;
                    c + Vec2::new(x, 8.5 + (x * 0.8).sin() * 1.2)
                })
                .collect();
            p.add(egui::Shape::line(wave, Stroke::new(1.2, INK)));
        }
        Kind::Roundabout => {
            let ring = c + Vec2::new(0.0, 1.0);
            p.circle_stroke(ring, 4.5, ink);
            p.line_segment(
                [Pos2::new(c.x, r.bottom() - 1.0), ring + Vec2::new(0.0, 4.5)],
                ink,
            );
            arrow(p, ring, s.angle, 4.5, ink);
        }
        Kind::UTurn => {
            let right = s.angle > 0.0;
            let sx = if right { 1.0 } else { -1.0 };
            let x0 = c.x - sx * 3.5;
            let mut pts = vec![Pos2::new(x0, r.bottom() - 1.0), Pos2::new(x0, c.y - 1.0)];
            for k in 1..=8 {
                let a = std::f32::consts::PI * k as f32 / 8.0;
                pts.push(Pos2::new(
                    c.x - sx * 3.5 * a.cos(),
                    c.y - 1.0 - 3.5 * a.sin(),
                ));
            }
            let tip = Pos2::new(c.x + sx * 3.5, c.y + 5.0);
            pts.push(tip);
            p.add(egui::Shape::line(pts, ink));
            head(p, tip, Vec2::new(0.0, 1.0), ink);
        }
        _ => {
            let angle = match (s.kind, s.modifier) {
                (Kind::Continue, Modifier::Straight) => 0.0,
                _ => s.angle,
            };
            p.line_segment(
                [Pos2::new(c.x, r.bottom() - 1.0), c + Vec2::new(0.0, 2.0)],
                ink,
            );
            arrow(p, c + Vec2::new(0.0, 2.0), angle, 0.0, ink);
        }
    }
}

/// An arrow from `from` (or `skip` along it) at `angle` degrees from straight ahead.
fn arrow(p: &egui::Painter, from: Pos2, angle: f64, skip: f32, ink: Stroke) {
    let a = (angle.clamp(-150.0, 150.0) as f32).to_radians();
    let dir = Vec2::new(a.sin(), -a.cos());
    let start = from + dir * skip;
    let tip = from + dir * (skip + 8.0);
    p.line_segment([start, tip], ink);
    head(p, tip, dir, ink);
}

fn head(p: &egui::Painter, tip: Pos2, dir: Vec2, ink: Stroke) {
    let n = Vec2::new(-dir.y, dir.x);
    let back = tip - dir * 4.5;
    p.add(egui::Shape::convex_polygon(
        vec![tip + dir * 1.0, back + n * 4.0, back - n * 4.0],
        ink.color,
        Stroke::NONE,
    ));
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;

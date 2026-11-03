//! The dialogs apart from the map's window: the pack picker, About this map, Verify pack, the errors
//! (a pack that could not be opened, a tile that could not be read) and the notices.

use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, Vec2};

use super::App;
use crate::config;
use crate::dialog::{self, Size};
use crate::keys::{self, Command};
use crate::memory::mib;
use crate::packs;
use crate::verify::{self, Outcome, Verify};

impl App {
    /// What the window shows with no map open: what a map is, where the app looked and a
    /// button to choose one.
    pub(super) fn no_map(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let looked = self.exe_dir.join("maps");
        let last = self
            .cfg
            .pack
            .as_ref()
            .filter(|_| self.cfg.start.reopen_pack)
            .map(|p| config::resolve_path(p, &self.exe_dir));
        let width = 440.0_f32.min(rect.width() - 32.0).max(120.0);
        let place = egui::Rect::from_center_size(
            rect.center_top() + Vec2::new(0.0, 60.0 + 100.0),
            Vec2::new(width, 200.0),
        );
        let ink = Color32::from_gray(40);
        ui.scope_builder(egui::UiBuilder::new().max_rect(place), |ui| {
            ui.set_width(width);
            let t = |s: String| egui::RichText::new(s).color(ink);
            ui.label(
                egui::RichText::new("No map is open")
                    .heading()
                    .size(20.0)
                    .color(ink),
            );
            ui.add_space(6.0);
            ui.label(t(
                "A map is a set of files ending in .omp.000, .omp.001 and so on, made by \
                 this app's builder and kept in one folder. Without one there is nothing to \
                 show."
                    .into(),
            ));
            ui.add_space(6.0);
            ui.label(t("The app looked in:".into()));
            ui.label(t(format!("\u{2022} {}", looked.display())));
            match &last {
                Some(p) => ui.label(t(format!(
                    "\u{2022} the last map, {}{}",
                    p.display(),
                    if p.exists() { "" } else { " (no longer there)" }
                ))),
                None => ui.label(t(
                    "\u{2022} the last map you opened (there is none yet)".into()
                )),
            };
            ui.add_space(10.0);
            let open = ui.button(keys::with_hint("Choose a map\u{2026}", Command::OpenMap));
            if open.clicked() {
                self.picker = true;
            }
        });
    }

    pub(super) fn picker_window(&mut self, ctx: &egui::Context) {
        if !self.picker {
            return;
        }
        let mut chosen = None;
        let closed = dialog::show(ctx, "picker", "Open a map", Size::Fit(576.0), |ui| {
            if self.found.is_empty() {
                ui.label(format!(
                    "No maps found in {}.",
                    self.exe_dir.join("maps").display()
                ));
            }
            for p in &self.found {
                let dir = packs::pack_dir(p);
                let shown = config::store_path(&dir, &self.exe_dir);
                let current = self
                    .map
                    .as_ref()
                    .is_some_and(|m| &m.opened.first_chunk == p);
                if ui
                    .selectable_label(current, shown.display().to_string())
                    .clicked()
                {
                    chosen = Some(p.clone());
                }
            }
            ui.separator();
            ui.label("Or open the map in a folder:");
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.path_input)
                        .hint_text("folder or .omp.000 file")
                        .desired_width(320.0),
                );
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Open").clicked() || enter) && !self.path_input.trim().is_empty() {
                    let dir = PathBuf::from(self.path_input.trim());
                    let found = packs::find(&dir);
                    match found.as_slice() {
                        [] => {
                            self.pack_error = Some(format!(
                                "There is no map in {} (a map's first file ends in .omp.000).",
                                dir.display()
                            ));
                        }
                        [one] => chosen = Some(one.clone()),
                        _ => self.found = found,
                    }
                }
            });
        });
        if let Some(p) = chosen {
            self.open(ctx, &p);
            self.picker = false;
        } else {
            self.picker = !closed;
        }
    }

    pub(super) fn start_verify(&mut self, ctx: &egui::Context) {
        let Some(m) = &self.map else { return };
        let ctx = ctx.clone();
        self.verify = Some(Verify::start(
            m.opened.reader.clone(),
            omp_core::tiles::TileReader::pack,
            move || ctx.request_repaint(),
        ));
    }

    /// Keyboard shortcuts (F1): every key in [`keys::TABLE`].
    pub(super) fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.shortcuts {
            return;
        }
        let closed = dialog::show(
            ctx,
            "shortcuts",
            "Keyboard shortcuts",
            Size::Fit(440.0),
            |ui| {
                egui::Grid::new("shortcuts-grid")
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        for e in keys::TABLE {
                            ui.label(egui::RichText::new(keys::keys_text(e.command)).strong());
                            ui.label(e.what);
                            ui.end_row();
                        }
                        for (k, what) in keys::LIST_KEYS {
                            ui.label(egui::RichText::new(*k).strong());
                            ui.label(*what);
                            ui.end_row();
                        }
                    });
            },
        );
        if closed {
            self.shortcuts = false;
        }
    }

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about {
            return;
        }
        let closed = dialog::show(ctx, "about", "About this map", Size::Fit(520.0), |ui| {
            let Some(m) = &self.map else {
                ui.label("No map is open.");
                return;
            };
            let pack = m.opened.reader.pack();
            let info = &pack.info;
            ui.label(egui::RichText::new(&info.name).heading().size(18.0));
            egui::Grid::new("about-grid")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    let mut row = |k: &str, v: String| {
                        ui.label(egui::RichText::new(k).strong());
                        ui.label(v);
                        ui.end_row();
                    };
                    row("Built", packs::date(info.build_time));
                    row("Data as of", info.snapshot.clone());
                    if let Ok(meta) = &m.opened.meta {
                        for (k, v) in packs::source_rows(&meta.sources) {
                            row(&k, v);
                        }
                        row(
                            "Builder",
                            format!("{}, format {}", meta.builder_version, meta.format_version),
                        );
                    }
                    let dir = packs::pack_dir(&m.opened.first_chunk);
                    let chunks = match pack.chunks() {
                        1 => "1 file".to_string(),
                        n => format!("{n} files"),
                    };
                    row("Files", format!("{} ({chunks})", dir.display()));
                });
            ui.separator();
            match &m.opened.meta {
                Ok(meta) => {
                    // A pack with addresses credits hundreds of sources.
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            ui.label(&meta.attribution);
                            // Sources' own attributions that the pack's does not already
                            // carry.
                            let mut shown: Vec<&str> = vec![&meta.attribution];
                            for src in meta.sources.iter().filter_map(|s| s.attribution.as_deref())
                            {
                                if !shown.iter().any(|s| s.contains(src)) {
                                    ui.label(src);
                                    shown.push(src);
                                }
                            }
                        });
                }
                Err(e) => {
                    ui.label(format!("© OpenStreetMap contributors. ({e})"));
                }
            }
            ui.separator();
            ui.label(format!("Offline Road Maps {}", env!("CARGO_PKG_VERSION")));
        });
        self.about = !closed;
    }

    pub(super) fn verify_window(&mut self, ctx: &egui::Context) {
        let Some(v) = &mut self.verify else { return };
        let outcome = v.outcome();
        let (done, total) = v.progress();
        let secs = v.elapsed().as_secs_f64();
        let mut close = false;
        let closed = dialog::show(ctx, "verify", "Verify map", Size::Fit(380.0), |ui| {
            ui.set_width(380.0);
            match &outcome {
                None => {
                    ui.label("Checking every part of the map against its checksum…");
                    let f = if total > 0 {
                        done as f32 / total as f32
                    } else {
                        0.0
                    };
                    ui.add(egui::ProgressBar::new(f).show_percentage());
                    ui.label(format!(
                        "{:.0} of {:.0} MiB, {}",
                        mib(done),
                        mib(total),
                        seconds(secs)
                    ));
                    close = ui.button("Cancel").clicked();
                }
                Some(o) => {
                    match o {
                        Outcome::Intact { sections, bytes } => ui.label(format!(
                            "The map is intact: all {sections} parts ({:.0} MiB) \
                                 match their checksums. Checked in {}.",
                            mib(*bytes),
                            seconds(secs)
                        )),
                        Outcome::Damaged(ids) => ui.colored_label(
                            crate::theme::error(),
                            format!(
                                "The map is damaged: {} did not pass the check. Copy \
                                     the map again from where it came from, or build it \
                                     again.",
                                verify::damaged_parts(ids),
                            ),
                        ),
                        Outcome::Failed(e) => ui.colored_label(
                            crate::theme::error(),
                            format!(
                                "The map could not be read: {e}. If it is on a stick, check \
                                 that the stick is still plugged in."
                            ),
                        ),
                        Outcome::Cancelled => ui.label("Cancelled."),
                    };
                    close = ui.button("Close").clicked();
                }
            }
        });
        if closed || close {
            // Dropping it stops the check.
            self.verify = None;
        }
    }

    pub(super) fn error_windows(&mut self, ctx: &egui::Context) {
        if let Some(e) = &self.pack_error {
            let mut close = false;
            let mut pick = false;
            egui::Window::new("Could not open the map")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_max_width(480.0);
                    ui.colored_label(crate::theme::error(), e);
                    ui.label(
                        "The map may be incomplete (one of its files missing or not copied \
                         completely), damaged, or made for a newer version of the app. Copy it \
                         again, or choose another map.",
                    );
                    ui.horizontal(|ui| {
                        pick = ui.button("Choose a map…").clicked();
                        close = ui.button("OK").clicked();
                    });
                });
            if pick {
                self.picker = true;
            }
            if pick || close {
                self.pack_error = None;
            }
        }
        if let Some(e) = &self.tile_error {
            let mut close = false;
            let mut verify = false;
            egui::Window::new("Part of the map could not be read")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_max_width(480.0);
                    ui.colored_label(crate::theme::error(), e);
                    ui.label(
                        "The map may be damaged. Verify it to find out; it takes a minute or \
                         more for a large map.",
                    );
                    ui.horizontal(|ui| {
                        verify = ui.button("Verify map").clicked();
                        close = ui.button("Not now").clicked();
                    });
                });
            if verify {
                self.start_verify(ctx);
            }
            if verify || close {
                self.tile_error = None;
            }
        }
    }

    pub(super) fn notices_window(&mut self, ctx: &egui::Context) {
        if self.notices.is_empty() {
            return;
        }
        let mut dismiss = false;
        egui::Window::new("Notice")
            .collapsible(false)
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -24.0))
            .show(ctx, |ui| {
                for n in &self.notices {
                    ui.label(n);
                }
                dismiss = ui.button("OK").clicked();
            });
        if dismiss {
            self.notices.clear();
        }
    }
}

fn seconds(s: f64) -> String {
    if s < 10.0 {
        format!("{s:.1} s")
    } else {
        format!("{s:.0} s")
    }
}

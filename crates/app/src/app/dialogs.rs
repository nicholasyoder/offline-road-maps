//! The dialogs over the window: the pack picker, About this map, Verify pack, the errors
//! (a pack that could not be opened, a tile that could not be read) and the notices.

use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, Vec2};

use super::App;
use crate::config;
use crate::memory::mib;
use crate::packs;
use crate::verify::{Outcome, Verify};

impl App {
    pub(super) fn picker_window(&mut self, ctx: &egui::Context) {
        if !self.picker {
            return;
        }
        let mut open = true;
        let mut chosen = None;
        egui::Window::new("Open a pack")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                if self.found.is_empty() {
                    ui.label(format!(
                        "No packs found in {}.",
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
                ui.label("Or open the pack in a folder:");
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .hint_text("folder or .omp.000 file")
                            .desired_width(320.0),
                    );
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button("Open").clicked() || enter) && !self.path_input.trim().is_empty()
                    {
                        let dir = PathBuf::from(self.path_input.trim());
                        let found = packs::find(&dir);
                        match found.as_slice() {
                            [] => {
                                self.pack_error =
                                    Some(format!("No pack (*.omp.000) in {}", dir.display()));
                            }
                            [one] => chosen = Some(one.clone()),
                            _ => self.found = found,
                        }
                    }
                });
            });
        if let Some(p) = chosen {
            self.open(ctx, &p);
            open = false;
        }
        self.picker = open;
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

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about {
            return;
        }
        let mut open = true;
        egui::Window::new("About this map")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                let Some(m) = &self.map else {
                    ui.label("No pack is open.");
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
                        row("Pack", format!("{} ({chunks})", dir.display()));
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
                                for src in
                                    meta.sources.iter().filter_map(|s| s.attribution.as_deref())
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
        self.about = open;
    }

    pub(super) fn verify_window(&mut self, ctx: &egui::Context) {
        let Some(v) = &mut self.verify else { return };
        let outcome = v.outcome();
        let (done, total) = v.progress();
        let secs = v.elapsed().as_secs_f64();
        let mut open = true;
        let mut close = false;
        egui::Window::new("Verify pack")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(380.0);
                match &outcome {
                    None => {
                        ui.label("Checking every part of the pack against its checksum…");
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
                                "The pack is intact: all {sections} sections ({:.0} MiB) \
                                 match their checksums. Checked in {}.",
                                mib(*bytes),
                                seconds(secs)
                            )),
                            Outcome::Damaged(ids) => ui.colored_label(
                                ERROR,
                                format!(
                                    "The pack is damaged: {} {} not match {} checksum. \
                                     Copy the pack again from where it came from, or \
                                     build it again.",
                                    ids.join(", "),
                                    if ids.len() == 1 { "does" } else { "do" },
                                    if ids.len() == 1 { "its" } else { "their" },
                                ),
                            ),
                            Outcome::Failed(e) => {
                                ui.colored_label(ERROR, format!("The pack could not be read: {e}"))
                            }
                            Outcome::Cancelled => ui.label("Cancelled."),
                        };
                        close = ui.button("Close").clicked();
                    }
                }
            });
        if !open || close {
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
                    ui.colored_label(ERROR, e);
                    ui.label(
                        "The pack may be incomplete (a chunk file missing), damaged, or made \
                         for a newer version of the app.",
                    );
                    ui.horizontal(|ui| {
                        pick = ui.button("Choose a pack…").clicked();
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
                    ui.colored_label(ERROR, e);
                    ui.label(
                        "The pack may be damaged. Verify it to find out; it takes a minute or \
                         more for a large pack.",
                    );
                    ui.horizontal(|ui| {
                        verify = ui.button("Verify pack").clicked();
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

const ERROR: Color32 = Color32::from_rgb(180, 30, 30);

fn seconds(s: f64) -> String {
    if s < 10.0 {
        format!("{s:.1} s")
    } else {
        format!("{s:.0} s")
    }
}

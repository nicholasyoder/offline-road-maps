//! Dialogs in windows of their own, apart from the map's. Where the platform cannot make
//! another window, egui draws the dialog inside the map's window instead (as it does in
//! the tests), and the code is the same.

use eframe::egui::{self, Color32, LayerId, Order, Pos2, Rect, UiBuilder, Vec2, ViewportClass};

use crate::keys::{self, Command};

/// The margin round a dialog's content.
const MARGIN: i8 = 12;

/// The least a dialog's window is asked to be. A fixed size below it would put the window's
/// minimum over its maximum, which Wayland answers by closing the app.
const MIN_SIZE: Vec2 = Vec2::new(200.0, 80.0);

/// How a dialog's window is sized.
#[derive(Clone, Copy)]
pub enum Size {
    /// To its content, which the window follows as it changes: the content at most this
    /// wide (and as wide as it sets itself, when that is less), the margins round it added.
    Fit(f32),
    /// To this size, which the user may change.
    Resizable(Vec2),
}

/// What is kept between frames of a dialog.
#[derive(Clone, Copy)]
struct Seen {
    /// The last size the window was asked to be.
    size: Vec2,
    /// The pass this was last shown in; a gap means the dialog was closed and opened again.
    pass: u64,
}

/// Shows a dialog in a window titled `title` (and the app's name) for as long as it is
/// called. `id` tells the dialog from the others. Returns whether the user asked to close
/// it, by the window's close button or Escape; the caller then stops showing it.
///
/// A window that fits its content opens at that size: `add` is first run once unseen
/// (invisible, only for its size) before the window exists. It is called again for the
/// window itself, so it must not do anything the second time that it should not.
pub fn show(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    size: Size,
    mut add: impl FnMut(&mut egui::Ui),
) -> bool {
    let mut add = |ui: &mut egui::Ui| {
        if let Size::Fit(width) = size {
            ui.set_max_width(width);
        }
        add(ui);
    };
    let viewport = egui::ViewportId::from_hash_of(id);
    let key = viewport.0.with("dialog-size");
    let pass = ctx.cumulative_pass_nr();
    let kept = ctx.data(|d| d.get_temp::<Seen>(key));
    let seen = match kept {
        Some(seen) if seen.pass + 1 >= pass => Seen { pass, ..seen },
        _ => Seen {
            size: match size {
                // Embedded, egui's window sizes itself.
                Size::Fit(_) if ctx.embed_viewports() => Vec2::ZERO,
                Size::Fit(width) => measure(ctx, viewport, width, &mut add).max(MIN_SIZE),
                Size::Resizable(size) => size,
            },
            pass,
        },
    };
    ctx.data_mut(|d| d.insert_temp(key, seen));
    let fit = matches!(size, Size::Fit(_));
    let builder = egui::ViewportBuilder::default()
        .with_title(format!("{title} – Offline Road Maps"))
        .with_inner_size(seen.size)
        .with_min_inner_size(MIN_SIZE)
        .with_resizable(!fit);
    ctx.show_viewport_immediate(viewport, builder, |ui, class| {
        if class == ViewportClass::EmbeddedWindow {
            // egui's own window already fits its content and has a close button.
            add(ui);
            return ui.input(|i| i.viewport().close_requested());
        }
        let panel = egui::Frame::central_panel(ui.style()).inner_margin(MARGIN);
        egui::CentralPanel::default().frame(panel).show(ui, |ui| {
            let used = ui.scope(&mut add).response.rect.size();
            if fit {
                follow(ui.ctx(), key, (used + margins()).max(MIN_SIZE));
            }
        });
        ui.input(|i| i.viewport().close_requested()) || keys::take(ui.ctx(), Command::Close)
    })
}

/// A question in a window of its own, with `yes` and a *Cancel* button beside it, centred. Returns
/// `Some(true)` when `yes` was clicked (or Enter pressed) and `Some(false)` when it was
/// cancelled (or closed, or Escape pressed); the caller then stops asking. `buttons` gets
/// where *Cancel* and `yes` were drawn, for the tests.
pub fn confirm(
    ctx: &egui::Context,
    title: &str,
    text: &str,
    yes: &str,
    buttons: &mut [Rect; 2],
) -> Option<bool> {
    let mut answer = None;
    let closed = show(ctx, "confirm", title, Size::Fit(340.0), |ui| {
        // Room round the text, so a window made taller than its content keeps it centred.
        ui.add_space(10.0);
        ui.vertical_centered(|ui| ui.label(text));
        ui.add_space(14.0);
        // The buttons side by side in the middle: width is the text's and the padding.
        let width = |ui: &egui::Ui, label: &str| {
            let font = egui::TextStyle::Button.resolve(ui.style());
            let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font, Color32::WHITE));
            galley.size().x + 2.0 * ui.spacing().button_padding.x
        };
        let both = width(ui, yes) + width(ui, "Cancel") + ui.spacing().item_spacing.x;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - both) / 2.0).max(0.0));
            let ok = ui.button(yes);
            let cancel = ui.button("Cancel");
            *buttons = [cancel.rect, ok.rect];
            if cancel.clicked() {
                answer = Some(false);
            } else if ok.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                answer = Some(true);
            }
        });
    });
    answer.or(closed.then_some(false))
}

/// The margins round the content, both sides, as a size.
fn margins() -> Vec2 {
    Vec2::splat(2.0 * f32::from(MARGIN))
}

/// The size a dialog's content takes, margins included, found by laying it out unseen.
fn measure(
    ctx: &egui::Context,
    viewport: egui::ViewportId,
    width: f32,
    add: &mut impl FnMut(&mut egui::Ui),
) -> Vec2 {
    // Taller than any dialog: those that could grow past the screen (Settings, Page
    // preview) keep themselves within the monitor's height.
    let room = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 2000.0));
    let layer = LayerId::new(Order::Background, viewport.0);
    let builder = UiBuilder::new()
        .layer_id(layer)
        .max_rect(room)
        .invisible()
        .sizing_pass();
    let mut ui = egui::Ui::new(ctx.clone(), viewport.0, builder);
    ui.scope(add).response.rect.size() + margins()
}

/// Keeps `want` as the window's size when it is not (to a pixel) the size already: the
/// next frame's builder asks for it, and egui resizes the window to match.
fn follow(ctx: &egui::Context, key: egui::Id, want: Vec2) {
    let Some(mut seen) = ctx.data(|d| d.get_temp::<Seen>(key)) else {
        return;
    };
    if (want - seen.size).abs().max_elem() >= 1.0 {
        seen.size = want;
        ctx.data_mut(|d| d.insert_temp(key, seen));
        // The builder is made in the parent's frame, which draws this window's too.
        ctx.request_repaint_of(ctx.parent_viewport_id());
    }
}

//! Dialogs in windows of their own, apart from the map's. Where the platform cannot make
//! another window, egui draws the dialog inside the map's window instead (as it does in
//! the tests), and the code is the same.

use eframe::egui::{self, LayerId, Order, Pos2, Rect, UiBuilder, Vec2, ViewportClass};

/// The margin round a dialog's content.
const MARGIN: i8 = 12;

/// How a dialog's window is sized.
#[derive(Clone, Copy)]
pub enum Size {
    /// To its content, which the window follows as it changes: at most this wide (and as
    /// wide as the content sets itself, when that is less).
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
                Size::Fit(width) => measure(ctx, viewport, width, &mut add),
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
        .with_min_inner_size([200.0, 80.0])
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
                follow(ui.ctx(), key, used + margins());
            }
        });
        ui.input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::Escape))
    })
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
    let room = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 2000.0) - margins());
    let layer = LayerId::new(Order::Background, viewport.0);
    let builder = UiBuilder::new()
        .layer_id(layer)
        .max_rect(room)
        .invisible()
        .sizing_pass();
    let mut ui = egui::Ui::new(ctx.clone(), viewport.0, builder);
    ui.scope(add).response.rect.size() + margins()
}

/// Asks for the window to be `want` big when it is not (to a pixel) already.
fn follow(ctx: &egui::Context, key: egui::Id, want: Vec2) {
    let Some(mut seen) = ctx.data(|d| d.get_temp::<Seen>(key)) else {
        return;
    };
    if (want - seen.size).abs().max_elem() >= 1.0 {
        seen.size = want;
        ctx.data_mut(|d| d.insert_temp(key, seen));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
        ctx.request_repaint();
    }
}

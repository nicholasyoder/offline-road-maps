//! The search box's keys, on a worker with no thread.

use super::*;
use crate::search::testing::{answer, hit};
use eframe::egui::{Pos2, Vec2};
use omp_core::find::HitKind;
use omp_core::search::Kind;

#[test]
fn keyboard() {
    let (worker, tx, _) = Worker::stub();
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
        // As the side panel calls it, on the search page.
        let mut full = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            let mut o = s.poll(recent);
            s.shortcut(&ctx);
            if !s.box_was_focused() {
                s.escape(&ctx);
            }
            s.box_ui(ui, &cam, 0.0, recent, None, &mut o);
            if !s.suggestions_ui(ui, &cam, Units::Kilometres, recent, &mut o) {
                s.results_ui(ui, &cam, Units::Kilometres, &mut o);
            }
            out = Some(o);
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
    assert!(!q.submit);
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
    assert!(q.submit && q.limit == RESULTS);
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

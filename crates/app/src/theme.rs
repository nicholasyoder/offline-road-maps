//! Colours of the app's own UI (search, "what's here", the scale bar), apart from the map's
//! style.

use eframe::egui::Color32;

/// Main text, and the scale bar and credits over the map.
pub const INK: Color32 = Color32::from_gray(40);
/// Secondary text: kinds, places, distances.
pub const WEAK: Color32 = Color32::from_gray(105);
/// Marks a result or position that is only approximate.
pub const APPROX: Color32 = Color32::from_rgb(176, 96, 0);

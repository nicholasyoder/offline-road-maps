//! Which tiles a view needs, and what to draw while some of them are still loading.

use std::collections::HashSet;

use omp_core::tile::MAX_ZOOM;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub const fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    pub fn parent(self) -> Option<Self> {
        (self.z > 0).then(|| Self::new(self.z - 1, self.x / 2, self.y / 2))
    }

    pub fn children(self) -> [Self; 4] {
        let (z, x, y) = (self.z + 1, self.x * 2, self.y * 2);
        [
            Self::new(z, x, y),
            Self::new(z, x + 1, y),
            Self::new(z, x, y + 1),
            Self::new(z, x + 1, y + 1),
        ]
    }

    /// The tile's square in world coordinates, `[x0, y0, x1, y1]`.
    pub fn world_rect(self) -> [f64; 4] {
        let n = f64::from(1u32 << self.z);
        let (x, y) = (f64::from(self.x), f64::from(self.y));
        [x / n, y / n, (x + 1.0) / n, (y + 1.0) / n]
    }
}

impl std::fmt::Display for TileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.z, self.x, self.y)
    }
}

/// The tile zoom drawn at a fractional view zoom. Above z14 the z14 tiles are overzoomed.
/// The rounding point is a first guess (open decision, tuned in 1.5).
pub fn tile_zoom(zoom: f64) -> u8 {
    (zoom + 0.5).floor().clamp(0.0, f64::from(MAX_ZOOM)) as u8
}

/// The tiles a view needs at zoom `z`.
#[derive(Debug, Default, PartialEq)]
pub struct Selection {
    /// Tiles on screen, nearest the centre first.
    pub visible: Vec<TileId>,
    /// A one-tile ring around them, loaded next so short pans find them ready.
    pub margin: Vec<TileId>,
}

/// The tiles at zoom `z` that cover `view` (world `[x0, y0, x1, y1]`) and lie in `bounds`.
pub fn select(z: u8, view: [f64; 4], bounds: [f64; 4]) -> Selection {
    let n = 1u32 << z;
    let range = |a: f64, b: f64| {
        let lo = (a * f64::from(n)).floor().max(0.0) as i64;
        let hi = ((b * f64::from(n)).ceil() as i64 - 1).min(i64::from(n) - 1);
        (lo, hi)
    };
    let (vx0, vx1) = range(view[0], view[2]);
    let (vy0, vy1) = range(view[1], view[3]);
    let (bx0, bx1) = range(bounds[0], bounds[2]);
    let (by0, by1) = range(bounds[1], bounds[3]);
    let cx = (view[0] + view[2]) / 2.0 * f64::from(n);
    let cy = (view[1] + view[3]) / 2.0 * f64::from(n);
    let mut sel = Selection::default();
    for y in (vy0 - 1).max(by0)..=(vy1 + 1).min(by1) {
        for x in (vx0 - 1).max(bx0)..=(vx1 + 1).min(bx1) {
            let id = TileId::new(z, x as u32, y as u32);
            if (vx0..=vx1).contains(&x) && (vy0..=vy1).contains(&y) {
                sel.visible.push(id);
            } else {
                sel.margin.push(id);
            }
        }
    }
    let dist = |t: &TileId| {
        let (dx, dy) = (f64::from(t.x) + 0.5 - cx, f64::from(t.y) + 0.5 - cy);
        dx * dx + dy * dy
    };
    sel.visible.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
    sel.margin.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
    sel
}

/// How a tile in [`plan`] stands in for the view's tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cover {
    /// The wanted tile itself.
    Exact,
    /// An ancestor drawn for wanted tiles that have not loaded yet.
    Parent,
    /// A child, one of four that together cover a wanted tile that has not loaded yet.
    Child,
    /// Wanted, not loaded, and nothing loaded covers it.
    Missing,
}

/// Ancestors tried as fallbacks; z14 data drawn at z18 is already four levels of overzoom.
const MAX_PARENT_LEVELS: u8 = 8;

/// What to draw for the `visible` tiles, given which tiles are loaded (`has`). Sorted by
/// zoom, so fallback parents come first and are drawn underneath.
pub fn plan(visible: &[TileId], has: impl Fn(TileId) -> bool) -> Vec<(TileId, Cover)> {
    let mut out = Vec::with_capacity(visible.len());
    let mut parents = HashSet::new();
    for &t in visible {
        if has(t) {
            out.push((t, Cover::Exact));
            continue;
        }
        let mut p = t.parent();
        let mut found = false;
        for _ in 0..MAX_PARENT_LEVELS {
            let Some(a) = p else { break };
            if has(a) {
                if parents.insert(a) {
                    out.push((a, Cover::Parent));
                }
                found = true;
                break;
            }
            p = a.parent();
        }
        if found {
            continue;
        }
        let kids = t.children();
        if t.z < MAX_ZOOM && kids.iter().all(|&c| has(c)) {
            out.extend(kids.map(|c| (c, Cover::Child)));
        } else {
            out.push((t, Cover::Missing));
        }
    }
    out.sort_by_key(|(t, _)| t.z);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_zoom_rounds_and_clamps() {
        assert_eq!(tile_zoom(0.0), 0);
        assert_eq!(tile_zoom(3.49), 3);
        assert_eq!(tile_zoom(3.5), 4);
        assert_eq!(tile_zoom(17.0), 14);
    }

    #[test]
    fn selection_covers_view_with_margin() {
        // A view over tiles x 2..=3, y 1..=2 at z3.
        let view = [2.1 / 8.0, 1.2 / 8.0, 3.9 / 8.0, 2.8 / 8.0];
        let sel = select(3, view, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(sel.visible.len(), 4);
        assert_eq!(sel.margin.len(), 16 - 4);
        assert!(sel.visible.contains(&TileId::new(3, 3, 2)));
        // Clipped to the bounds: only column 3 and row 2 remain, plus margin row 3.
        let b = [3.0 / 8.0, 2.0 / 8.0, 4.0 / 8.0, 4.0 / 8.0];
        let sel = select(3, view, b);
        assert_eq!(sel.visible, vec![TileId::new(3, 3, 2)]);
        assert_eq!(sel.margin, vec![TileId::new(3, 3, 3)]);
    }

    #[test]
    fn selection_at_world_edge() {
        let sel = select(1, [-0.5, -0.5, 1.5, 1.5], [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(sel.visible.len(), 4);
        assert!(sel.margin.is_empty());
    }

    #[test]
    fn plan_uses_parents_then_children() {
        let want = [
            TileId::new(5, 10, 10),
            TileId::new(5, 11, 10),
            TileId::new(5, 12, 10),
        ];
        let loaded: HashSet<_> = [
            TileId::new(5, 10, 10),
            TileId::new(3, 2, 2), // ancestor of 11/10
        ]
        .into_iter()
        .chain(TileId::new(5, 12, 10).children())
        .collect();
        let p = plan(&want, |t| loaded.contains(&t));
        assert_eq!(p[0], (TileId::new(3, 2, 2), Cover::Parent));
        assert!(p.contains(&(TileId::new(5, 10, 10), Cover::Exact)));
        assert_eq!(p.iter().filter(|(_, c)| *c == Cover::Child).count(), 4);
        let none = plan(&want[..1], |_| false);
        assert_eq!(none, vec![(want[0], Cover::Missing)]);
    }
}

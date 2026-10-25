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

/// What the cache knows about a tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Known {
    /// Loaded and ready to draw.
    Loaded,
    /// Not in the pack: it is drawn from its nearest stored ancestor (format.md).
    Absent,
}

/// What stands for tile `t` once its absent ancestors are skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// `t` itself or its nearest stored ancestor, loaded.
    Stored(TileId),
    /// Neither `t` nor any ancestor is stored: bare land.
    Empty,
    /// This tile (`t` or an ancestor) has not been read yet.
    Unknown(TileId),
}

/// Walks up from `t` past the tiles known to be absent.
pub fn resolve(t: TileId, known: impl Fn(TileId) -> Option<Known>) -> Resolved {
    let mut at = t;
    loop {
        match known(at) {
            Some(Known::Loaded) => return Resolved::Stored(at),
            None => return Resolved::Unknown(at),
            Some(Known::Absent) => match at.parent() {
                Some(p) => at = p,
                None => return Resolved::Empty,
            },
        }
    }
}

/// How a tile in [`plan`] stands in for the view's tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cover {
    /// The wanted tile itself.
    Exact,
    /// The nearest stored ancestor of wanted tiles the pack leaves out.
    Ancestor,
    /// A wanted tile with nothing stored at or above it, drawn as bare land.
    Empty,
    /// An ancestor drawn for wanted tiles that have not loaded yet.
    Parent,
    /// A child, one of four that together cover a wanted tile that has not loaded yet.
    Child,
    /// Wanted, not loaded, and nothing loaded covers it.
    Missing,
}

/// Ancestors tried as fallbacks; z14 data drawn at z18 is already four levels of overzoom.
const MAX_PARENT_LEVELS: u8 = 8;

/// What to draw for a view.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Sorted by zoom, so ancestors come first and are drawn underneath.
    pub draws: Vec<(TileId, Cover)>,
    /// For drawn tiles above a cap zoom: `(tile, cap, source)`, where `source` is the stored
    /// tile at or above the cap zoom whose capped layers are drawn in `tile`'s square, once
    /// it is loaded (format 0.3, layer `maxzoom`).
    pub capped: Vec<(TileId, u8, TileId)>,
    /// Where each visible tile's labels come from: `(tile, square, cap)` draws the labels of
    /// loaded `tile` whose anchor is in `square`'s square, so an ancestor standing in for some
    /// tiles adds no labels where others are drawn from their own data. With `cap`, only those
    /// of the layers capped at that zoom.
    pub labels: Vec<(TileId, TileId, Option<u8>)>,
    /// Tiles still to read before the view is fully detailed: wanted tiles, the next
    /// ancestor up from a left-out one, or a capped layer's source. In `visible` order.
    pub need: Vec<TileId>,
}

impl Plan {
    /// Whether every visible tile is drawn from its own data or its stored ancestor's.
    pub fn detailed(&self) -> bool {
        self.need.is_empty()
    }
}

/// What to draw for the `visible` tiles, given what the cache knows (`known`). `caps` are
/// the zooms that capped layers are stored up to; a drawn tile above one also needs its
/// ancestor at that zoom (or that ancestor's nearest stored ancestor).
pub fn plan(visible: &[TileId], caps: &[u8], known: impl Fn(TileId) -> Option<Known>) -> Plan {
    let mut out = Plan::default();
    let mut drawn = HashSet::new();
    let loaded = |t: TileId| known(t) == Some(Known::Loaded);
    for &t in visible {
        let unknown = match resolve(t, &known) {
            Resolved::Stored(a) => {
                let cover = if a == t {
                    Cover::Exact
                } else {
                    Cover::Ancestor
                };
                if drawn.insert(a) {
                    out.draws.push((a, cover));
                }
                out.labels.push((a, t, None));
                continue;
            }
            Resolved::Empty => {
                out.draws.push((t, Cover::Empty));
                continue;
            }
            Resolved::Unknown(u) => u,
        };
        if !out.need.contains(&unknown) {
            out.need.push(unknown);
        }
        // Until it is known, draw any loaded ancestor of the unknown tile, else its children.
        let mut p = unknown.parent();
        let mut found = false;
        for _ in 0..MAX_PARENT_LEVELS {
            let Some(a) = p else { break };
            if loaded(a) {
                if drawn.insert(a) {
                    out.draws.push((a, Cover::Parent));
                }
                out.labels.push((a, t, None));
                found = true;
                break;
            }
            p = a.parent();
        }
        if found {
            continue;
        }
        let kids = t.children();
        if unknown == t && t.z < MAX_ZOOM && kids.iter().all(|&c| loaded(c)) {
            out.draws.extend(kids.map(|c| (c, Cover::Child)));
            out.labels.extend(kids.map(|c| (c, c, None)));
        } else {
            out.draws.push((t, Cover::Missing));
        }
    }
    out.draws.sort_by_key(|(t, _)| t.z);
    for &(t, cover) in &out.draws {
        if matches!(cover, Cover::Missing | Cover::Empty) {
            continue;
        }
        for &m in caps.iter().filter(|&&m| t.z > m) {
            let d = t.z - m;
            match resolve(TileId::new(m, t.x >> d, t.y >> d), &known) {
                Resolved::Stored(s) => out.capped.push((t, m, s)),
                Resolved::Unknown(u) if !out.need.contains(&u) => out.need.push(u),
                _ => {}
            }
        }
    }
    // A capped layer's labels come from the tile its geometry is drawn from.
    let capped: Vec<_> = out
        .labels
        .iter()
        .flat_map(|&(t, square, _)| {
            out.capped
                .iter()
                .filter(move |c| c.0 == t)
                .map(move |&(_, cap, s)| (s, square, Some(cap)))
        })
        .collect();
    out.labels.extend(capped);
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
        let p = plan(&want, &[], |t| loaded.contains(&t).then_some(Known::Loaded));
        assert_eq!(p.draws[0], (TileId::new(3, 2, 2), Cover::Parent));
        assert!(p.draws.contains(&(TileId::new(5, 10, 10), Cover::Exact)));
        assert_eq!(
            p.draws.iter().filter(|(_, c)| *c == Cover::Child).count(),
            4
        );
        assert_eq!(p.need, want[1..]);
        // The parent's labels only where it stands in; each child's in its own square.
        assert!(p.labels.contains(&(TileId::new(3, 2, 2), want[1], None)));
        assert!(p.labels.contains(&(want[0], want[0], None)));
        assert_eq!(p.labels.len(), 1 + 1 + 4);
        let none = plan(&want[..1], &[], |_| None);
        assert_eq!(none.draws, vec![(want[0], Cover::Missing)]);
    }

    #[test]
    fn left_out_tiles_are_drawn_from_their_stored_ancestor() {
        // 14/100/100 and 13/50/50 are left out, 12/25/25 is stored; 14/101/100 is stored.
        let want = [TileId::new(14, 100, 100), TileId::new(14, 101, 100)];
        let (t14, t13, t12) = (want[0], TileId::new(13, 50, 50), TileId::new(12, 25, 25));
        let mut known = std::collections::HashMap::from([
            (t14, Known::Absent),
            (want[1], Known::Loaded),
            (t12, Known::Loaded),
        ]);
        // 13/50/50 not read yet: it is needed next, and 12/25/25 stands in meanwhile.
        let p = plan(&want, &[], |t| known.get(&t).copied());
        assert_eq!(p.need, vec![t13]);
        assert_eq!(p.draws[0], (t12, Cover::Parent));
        known.insert(t13, Known::Absent);
        let p = plan(&want, &[], |t| known.get(&t).copied());
        assert!(p.detailed());
        assert_eq!(
            p.draws,
            vec![(t12, Cover::Ancestor), (want[1], Cover::Exact)]
        );
        assert_eq!(p.labels, vec![(t12, t14, None), (want[1], want[1], None)]);
        // Nothing stored all the way up: bare land.
        let p = plan(&want[..1], &[], |_| Some(Known::Absent));
        assert_eq!(p.draws, vec![(t14, Cover::Empty)]);
        assert!(p.detailed());
    }

    #[test]
    fn capped_layers_come_from_the_tile_at_the_cap() {
        // Layers capped at z12. 14/100/100 is loaded; its z12 ancestor 12/25/25 is not read.
        let (t14, t13, t12) = (
            TileId::new(14, 100, 100),
            TileId::new(13, 50, 50),
            TileId::new(12, 25, 25),
        );
        let mut known = std::collections::HashMap::from([(t14, Known::Loaded)]);
        let p = plan(&[t14], &[12], |t| known.get(&t).copied());
        assert_eq!(p.draws, vec![(t14, Cover::Exact)]);
        assert!(p.capped.is_empty());
        assert_eq!(p.need, vec![t12]);
        // Left out: its own stored ancestor stands in.
        known.insert(t12, Known::Absent);
        known.insert(TileId::new(11, 12, 12), Known::Loaded);
        let p = plan(&[t14], &[12], |t| known.get(&t).copied());
        assert!(p.detailed());
        assert_eq!(p.capped, vec![(t14, 12, TileId::new(11, 12, 12))]);
        assert_eq!(
            p.labels,
            vec![(t14, t14, None), (TileId::new(11, 12, 12), t14, Some(12))]
        );
        // A tile at or below the cap draws its own.
        known.insert(t13, Known::Loaded);
        let p = plan(&[t13], &[13], |t| known.get(&t).copied());
        assert!(p.capped.is_empty() && p.detailed());
    }
}

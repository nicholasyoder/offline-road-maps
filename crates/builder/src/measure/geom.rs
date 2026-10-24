//! Geometry in Web Mercator world coordinates: projection, clipping, simplification and
//! multipolygon ring assembly.

use std::collections::HashMap;

/// A point in world coordinates: Web Mercator scaled to `0..2^32` on both axes, y down.
/// One unit is under 1 cm at the equator.
pub type P = [u32; 2];

pub const WORLD: f64 = 4_294_967_296.0;
const MAX_LAT: f64 = 85.051_128_779_806_59;
const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;

pub fn project(lat: f64, lon: f64) -> P {
    let lat = lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    let x = (lon + 180.0) / 360.0;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0;
    [to_world(x), to_world(y)]
}

pub fn project_e7(lat: i32, lon: i32) -> P {
    project(f64::from(lat) * 1e-7, f64::from(lon) * 1e-7)
}

fn to_world(v: f64) -> u32 {
    (v * WORLD).round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// `(lat, lon)` in degrees.
pub fn unproject(p: P) -> (f64, f64) {
    let x = f64::from(p[0]) / WORLD;
    let y = f64::from(p[1]) / WORLD;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y))
        .sinh()
        .atan()
        .to_degrees();
    (lat, x * 360.0 - 180.0)
}

/// Metres per world unit at the latitude of `p`.
pub fn metres_per_unit(p: P) -> f64 {
    EARTH_CIRCUMFERENCE_M / WORLD * unproject(p).0.to_radians().cos()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BBox {
    pub min: P,
    pub max: P,
}

impl BBox {
    pub fn of(points: impl IntoIterator<Item = P>) -> Self {
        let mut b = BBox {
            min: [u32::MAX; 2],
            max: [0; 2],
        };
        for p in points {
            b.min = [b.min[0].min(p[0]), b.min[1].min(p[1])];
            b.max = [b.max[0].max(p[0]), b.max[1].max(p[1])];
        }
        b
    }
}

#[derive(Clone, Debug)]
pub enum Geometry {
    Point(P),
    Lines(Vec<Vec<P>>),
    /// Rings with outer rings clockwise on screen (positive [`ring_area`]) and holes
    /// counter-clockwise, as in MVT.
    Polygon(Vec<Vec<P>>),
}

impl Geometry {
    pub fn bbox(&self) -> BBox {
        match self {
            Geometry::Point(p) => BBox { min: *p, max: *p },
            Geometry::Lines(parts) | Geometry::Polygon(parts) => {
                BBox::of(parts.iter().flatten().copied())
            }
        }
    }

    pub fn vertices(&self) -> usize {
        match self {
            Geometry::Point(_) => 1,
            Geometry::Lines(parts) | Geometry::Polygon(parts) => parts.iter().map(Vec::len).sum(),
        }
    }
}

/// A clip rectangle in world units, `[min_x, min_y, max_x, max_y]`, may extend past the world.
pub type Rect = [i64; 4];

pub fn bbox_inside(b: &BBox, r: &Rect) -> bool {
    i64::from(b.min[0]) >= r[0]
        && i64::from(b.min[1]) >= r[1]
        && i64::from(b.max[0]) <= r[2]
        && i64::from(b.max[1]) <= r[3]
}

pub fn bbox_overlaps(b: &BBox, r: &Rect) -> bool {
    i64::from(b.max[0]) >= r[0]
        && i64::from(b.max[1]) >= r[1]
        && i64::from(b.min[0]) <= r[2]
        && i64::from(b.min[1]) <= r[3]
}

fn clamp_p(x: f64, y: f64) -> P {
    let c = |v: f64| v.round().clamp(0.0, f64::from(u32::MAX)) as u32;
    [c(x), c(y)]
}

/// Clips `g` to `r`. Returns `None` when nothing is left.
pub fn clip(g: &Geometry, r: &Rect) -> Option<Geometry> {
    match g {
        Geometry::Point(p) => {
            bbox_inside(&BBox { min: *p, max: *p }, r).then_some(Geometry::Point(*p))
        }
        Geometry::Lines(parts) => {
            let out: Vec<Vec<P>> = parts.iter().flat_map(|l| clip_line(l, r)).collect();
            (!out.is_empty()).then_some(Geometry::Lines(out))
        }
        Geometry::Polygon(rings) => {
            let out: Vec<Vec<P>> = rings
                .iter()
                .map(|ring| clip_ring(ring, r))
                .filter(|ring| ring.len() >= 3)
                .collect();
            // A polygon that lost all its outer rings is only holes; drop it.
            let has_outer = out.iter().any(|ring| ring_area(ring) > 0.0);
            has_outer.then_some(Geometry::Polygon(out))
        }
    }
}

/// Liang–Barsky per segment; a line leaving and re-entering the rectangle becomes several parts.
fn clip_line(line: &[P], r: &Rect) -> Vec<Vec<P>> {
    let (x0, y0, x1, y1) = (r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64);
    let mut parts = Vec::new();
    let mut cur: Vec<P> = Vec::new();
    for w in line.windows(2) {
        let (ax, ay) = (f64::from(w[0][0]), f64::from(w[0][1]));
        let (bx, by) = (f64::from(w[1][0]), f64::from(w[1][1]));
        let (dx, dy) = (bx - ax, by - ay);
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let mut visible = true;
        for (p, q) in [(-dx, ax - x0), (dx, x1 - ax), (-dy, ay - y0), (dy, y1 - ay)] {
            if p == 0.0 {
                if q < 0.0 {
                    visible = false;
                    break;
                }
            } else {
                let t = q / p;
                if p < 0.0 {
                    t0 = t0.max(t);
                } else {
                    t1 = t1.min(t);
                }
                if t0 > t1 {
                    visible = false;
                    break;
                }
            }
        }
        if !visible {
            if cur.len() >= 2 {
                parts.push(std::mem::take(&mut cur));
            }
            cur.clear();
            continue;
        }
        let a = clamp_p(ax + t0 * dx, ay + t0 * dy);
        let b = clamp_p(ax + t1 * dx, ay + t1 * dy);
        if cur.last() != Some(&a) {
            if cur.len() >= 2 {
                parts.push(std::mem::take(&mut cur));
            }
            cur.clear();
            cur.push(a);
        }
        cur.push(b);
        if t1 < 1.0 {
            parts.push(std::mem::take(&mut cur));
        }
    }
    if cur.len() >= 2 {
        parts.push(cur);
    }
    parts
}

/// Sutherland–Hodgman. Rings are implicitly closed (the last point is not repeated).
fn clip_ring(ring: &[P], r: &Rect) -> Vec<P> {
    let mut pts: Vec<[f64; 2]> = ring
        .iter()
        .map(|p| [f64::from(p[0]), f64::from(p[1])])
        .collect();
    let edges: [(usize, f64, bool); 4] = [
        (0, r[0] as f64, true),
        (0, r[2] as f64, false),
        (1, r[1] as f64, true),
        (1, r[3] as f64, false),
    ];
    for (axis, v, is_min) in edges {
        if pts.is_empty() {
            break;
        }
        let inside = |p: &[f64; 2]| if is_min { p[axis] >= v } else { p[axis] <= v };
        let mut out = Vec::with_capacity(pts.len() + 4);
        let mut prev = *pts.last().unwrap();
        for &p in &pts {
            match (inside(&prev), inside(&p)) {
                (true, true) => out.push(p),
                (true, false) => out.push(intersect(prev, p, axis, v)),
                (false, true) => {
                    out.push(intersect(prev, p, axis, v));
                    out.push(p);
                }
                (false, false) => {}
            }
            prev = p;
        }
        pts = out;
    }
    let mut res: Vec<P> = Vec::with_capacity(pts.len());
    for p in pts {
        let q = clamp_p(p[0], p[1]);
        if res.last() != Some(&q) {
            res.push(q);
        }
    }
    if res.len() > 1 && res.first() == res.last() {
        res.pop();
    }
    res
}

fn intersect(a: [f64; 2], b: [f64; 2], axis: usize, v: f64) -> [f64; 2] {
    let t = (v - a[axis]) / (b[axis] - a[axis]);
    let mut p = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
    p[axis] = v;
    p
}

/// Shoelace area; positive for clockwise on screen (y down), as MVT defines exterior rings.
pub fn ring_area<T: Copy + Into<f64>>(ring: &[[T; 2]]) -> f64 {
    let n = ring.len();
    let mut s = 0.0;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        s += a[0].into() * b[1].into() - b[0].into() * a[1].into();
    }
    s / 2.0
}

/// Douglas–Peucker on tile-local integer points. Keeps the endpoints.
pub fn simplify(pts: &[[i32; 2]], tolerance: f64) -> Vec<[i32; 2]> {
    if pts.len() <= 2 || tolerance <= 0.0 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0, pts.len() - 1)];
    let tol2 = tolerance * tolerance;
    while let Some((a, b)) = stack.pop() {
        let (mut best, mut best_d) = (0, 0.0);
        for i in a + 1..b {
            let d = seg_dist2(pts[i], pts[a], pts[b]);
            if d > best_d {
                best = i;
                best_d = d;
            }
        }
        if best_d > tol2 {
            keep[best] = true;
            stack.push((a, best));
            stack.push((best, b));
        }
    }
    pts.iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

fn seg_dist2(p: [i32; 2], a: [i32; 2], b: [i32; 2]) -> f64 {
    let (px, py) = (f64::from(p[0]), f64::from(p[1]));
    let (ax, ay) = (f64::from(a[0]), f64::from(a[1]));
    let (dx, dy) = (f64::from(b[0]) - ax, f64::from(b[1]) - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    let (ex, ey) = (ax + t * dx - px, ay + t * dy - py);
    ex * ex + ey * ey
}

/// Area centroid of the largest outer ring, or the bbox centre for degenerate rings.
pub fn centroid(rings: &[Vec<P>]) -> P {
    let Some(ring) = rings
        .iter()
        .filter(|r| ring_area(r) > 0.0)
        .max_by(|a, b| ring_area(a).total_cmp(&ring_area(b)))
    else {
        return centroid_or_middle(BBox::of(rings.iter().flatten().copied()));
    };
    // Relative to the first point, so f64 keeps its precision.
    let o = [f64::from(ring[0][0]), f64::from(ring[0][1])];
    let (mut cx, mut cy, mut a2) = (0.0, 0.0, 0.0);
    for i in 0..ring.len() {
        let p = ring[i];
        let q = ring[(i + 1) % ring.len()];
        let (px, py) = (f64::from(p[0]) - o[0], f64::from(p[1]) - o[1]);
        let (qx, qy) = (f64::from(q[0]) - o[0], f64::from(q[1]) - o[1]);
        let c = px * qy - qx * py;
        a2 += c;
        cx += (px + qx) * c;
        cy += (py + qy) * c;
    }
    clamp_p(o[0] + cx / (3.0 * a2), o[1] + cy / (3.0 * a2))
}

/// The pole of inaccessibility (polylabel): a point inside the polygon, as far from its edges
/// as it gets to within `precision` world units, or after `max_cells` cells, whichever comes
/// first. Unlike the centroid it stays inside long lakes and crescents. Holes count, and so
/// do all outer rings: the point lands in the part with the most room.
pub fn polylabel(rings: &[Vec<P>], precision: f64, max_cells: usize) -> P {
    use std::collections::BinaryHeap;

    let b = BBox::of(rings.iter().flatten().copied());
    let o = [f64::from(b.min[0]), f64::from(b.min[1])];
    let rings: Vec<Vec<[f64; 2]>> = rings
        .iter()
        .map(|r| {
            r.iter()
                .map(|p| [f64::from(p[0]) - o[0], f64::from(p[1]) - o[1]])
                .collect()
        })
        .collect();
    let (w, h) = (
        f64::from(b.max[0] - b.min[0]),
        f64::from(b.max[1] - b.min[1]),
    );
    let size = w.min(h);
    if size <= 0.0 {
        return centroid_or_middle(b);
    }

    struct Cell {
        c: [f64; 2],
        half: f64,
        /// Signed distance from the centre to the polygon's edges, positive inside.
        d: f64,
        /// The best distance any point in the cell could have.
        max: f64,
    }
    impl PartialEq for Cell {
        fn eq(&self, o: &Self) -> bool {
            self.max == o.max
        }
    }
    impl Eq for Cell {}
    impl PartialOrd for Cell {
        fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Cell {
        fn cmp(&self, o: &Self) -> std::cmp::Ordering {
            self.max.total_cmp(&o.max)
        }
    }
    let cell = |c: [f64; 2], half: f64| {
        let d = signed_distance(c, &rings);
        Cell {
            c,
            half,
            d,
            max: d + half * std::f64::consts::SQRT_2,
        }
    };

    let mut queue = BinaryHeap::new();
    let half = size / 2.0;
    let mut y = 0.0;
    while y < h {
        let mut x = 0.0;
        while x < w {
            queue.push(cell([x + half, y + half], half));
            x += size;
        }
        y += size;
    }
    // Start from the area centroid, which is often good already, or the bbox centre.
    let ring_centroid = |r: &[[f64; 2]]| {
        let (mut cx, mut cy, mut a2) = (0.0, 0.0, 0.0);
        for i in 0..r.len() {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            let c = p[0] * q[1] - q[0] * p[1];
            a2 += c;
            cx += (p[0] + q[0]) * c;
            cy += (p[1] + q[1]) * c;
        }
        (a2 != 0.0).then(|| [cx / (3.0 * a2), cy / (3.0 * a2)])
    };
    let mut best = cell([w / 2.0, h / 2.0], 0.0);
    if let Some(c) = rings
        .iter()
        .filter(|r| ring_area(r) > 0.0)
        .max_by(|a, b| ring_area(a).total_cmp(&ring_area(b)))
        .and_then(|r| ring_centroid(r))
    {
        let c = cell(c, 0.0);
        if c.d > best.d {
            best = c;
        }
    }
    let mut seen = queue.len();
    while let Some(c) = queue.pop() {
        if c.d > best.d {
            best = Cell { ..c };
        }
        if c.max - best.d <= precision || seen >= max_cells {
            continue;
        }
        let h = c.half / 2.0;
        for (dx, dy) in [(-h, -h), (h, -h), (-h, h), (h, h)] {
            queue.push(cell([c.c[0] + dx, c.c[1] + dy], h));
        }
        seen += 4;
    }
    clamp_p(best.c[0] + o[0], best.c[1] + o[1])
}

fn centroid_or_middle(b: BBox) -> P {
    [
        ((u64::from(b.min[0]) + u64::from(b.max[0])) / 2) as u32,
        ((u64::from(b.min[1]) + u64::from(b.max[1])) / 2) as u32,
    ]
}

/// Distance from `p` to the nearest ring edge, positive inside (even-odd over all rings).
fn signed_distance(p: [f64; 2], rings: &[Vec<[f64; 2]>]) -> f64 {
    let mut inside = false;
    let mut best = f64::INFINITY;
    for r in rings {
        let mut j = r.len() - 1;
        for i in 0..r.len() {
            let (a, b) = (r[i], r[j]);
            if (a[1] > p[1]) != (b[1] > p[1])
                && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
            {
                inside = !inside;
            }
            best = best.min(seg_dist2_f(p, a, b));
            j = i;
        }
    }
    let d = best.sqrt();
    if inside { d } else { -d }
}

fn seg_dist2_f(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    };
    let (ex, ey) = (a[0] + t * dx - p[0], a[1] + t * dy - p[1]);
    ex * ex + ey * ey
}

/// Area of a polygon in square metres (approximate: uses the scale at its first point).
pub fn area_m2(rings: &[Vec<P>]) -> f64 {
    let Some(first) = rings.first().and_then(|r| r.first()) else {
        return 0.0;
    };
    let m = metres_per_unit(*first);
    rings.iter().map(|r| ring_area(r)).sum::<f64>() * m * m
}

/// Turns a closed way (first node repeated at the end) into a ring oriented as `outer`.
pub fn closed_way_ring(coords: &[P], outer: bool) -> Option<Vec<P>> {
    if coords.len() < 4 || coords.first() != coords.last() {
        return None;
    }
    let mut ring = coords[..coords.len() - 1].to_vec();
    orient(&mut ring, outer);
    Some(ring)
}

fn orient(ring: &mut [P], outer: bool) {
    if (ring_area(ring) > 0.0) != outer {
        ring.reverse();
    }
}

pub enum Assembly {
    Ok(Vec<Vec<P>>),
    /// Some member ways do not join into closed rings. Holds whatever rings did close.
    Broken(Vec<Vec<P>>),
}

/// Joins member ways (`(coords, is_inner)`) end to end into closed rings.
pub fn assemble(members: &[(&[P], bool)]) -> Assembly {
    let mut rings = Vec::new();
    let mut broken = false;
    for inner in [false, true] {
        let ways: Vec<&[P]> = members
            .iter()
            .filter(|(c, i)| *i == inner && c.len() >= 2)
            .map(|(c, _)| *c)
            .collect();
        let mut by_end: HashMap<P, Vec<usize>> = HashMap::new();
        for (i, w) in ways.iter().enumerate() {
            by_end.entry(w[0]).or_default().push(i);
            by_end.entry(*w.last().unwrap()).or_default().push(i);
        }
        let mut used = vec![false; ways.len()];
        for start in 0..ways.len() {
            if used[start] {
                continue;
            }
            used[start] = true;
            let mut ring: Vec<P> = ways[start].to_vec();
            while ring.first() != ring.last() {
                let end = *ring.last().unwrap();
                let next = by_end
                    .get(&end)
                    .and_then(|c| c.iter().copied().find(|&i| !used[i]));
                let Some(i) = next else { break };
                used[i] = true;
                if ways[i][0] == end {
                    ring.extend_from_slice(&ways[i][1..]);
                } else {
                    ring.extend(ways[i].iter().rev().skip(1));
                }
            }
            match closed_way_ring(&ring, !inner) {
                Some(r) => rings.push(r),
                None => broken = true,
            }
        }
    }
    if broken {
        Assembly::Broken(rings)
    } else {
        Assembly::Ok(rings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_round_trips() {
        let p = project(39.9612, -82.9988);
        let (lat, lon) = unproject(p);
        assert!((lat - 39.9612).abs() < 1e-7 && (lon + 82.9988).abs() < 1e-7);
    }

    #[test]
    fn line_clip_splits_parts() {
        // Goes in, out the right side, and back in.
        let line = vec![[0, 5], [5, 5], [15, 5], [15, 8], [5, 8]];
        let parts = clip_line(&line, &[0, 0, 10, 10]);
        assert_eq!(
            parts,
            vec![vec![[0, 5], [5, 5], [10, 5]], vec![[10, 8], [5, 8]]]
        );
    }

    #[test]
    fn ring_clip_and_orientation() {
        let mut sq = vec![[0, 0], [20, 0], [20, 20], [0, 20]];
        orient(&mut sq, true);
        assert!(ring_area(&sq) > 0.0);
        let c = clip_ring(&sq, &[10, 10, 30, 30]);
        assert_eq!(c.len(), 4);
        assert_eq!(ring_area(&c), 100.0);
    }

    #[test]
    fn assembles_split_rings() {
        let a: Vec<P> = vec![[0, 0], [10, 0], [10, 10]];
        let b: Vec<P> = vec![[0, 0], [0, 10], [10, 10]];
        let Assembly::Ok(rings) = assemble(&[(&a, false), (&b, false)]) else {
            panic!("should close");
        };
        assert_eq!(rings.len(), 1);
        assert!(ring_area(&rings[0]) > 0.0);
        let open: Vec<P> = vec![[0, 0], [5, 5]];
        assert!(matches!(assemble(&[(&open, false)]), Assembly::Broken(_)));
    }

    #[test]
    fn polylabel_stays_inside() {
        // A U shape: the centroid falls in the gap between the arms, polylabel does not.
        let u: Vec<P> = vec![
            [0, 0],
            [300, 0],
            [300, 1000],
            [700, 1000],
            [700, 0],
            [1000, 0],
            [1000, 1200],
            [0, 1200],
        ];
        let rings = vec![u];
        let c = centroid(&rings);
        assert!(c[0] > 300 && c[0] < 700 && c[1] < 1000, "centroid {c:?}");
        let p = polylabel(&rings, 1.0, 10_000);
        assert!(
            signed_distance(
                [f64::from(p[0]), f64::from(p[1])],
                &[rings[0]
                    .iter()
                    .map(|q| [f64::from(q[0]), f64::from(q[1])])
                    .collect()]
            ) > 90.0,
            "{p:?}"
        );
        // A square with a hole in the middle: the point goes around the hole.
        let sq: Vec<P> = vec![[0, 0], [1000, 0], [1000, 1000], [0, 1000]];
        let hole: Vec<P> = vec![[200, 200], [200, 800], [800, 800], [800, 200]];
        let p = polylabel(&[sq, hole], 1.0, 10_000);
        let inside_hole = (200..=800).contains(&p[0]) && (200..=800).contains(&p[1]);
        assert!(!inside_hole, "{p:?}");
    }

    #[test]
    fn simplify_drops_collinear() {
        let pts = [[0, 0], [5, 1], [10, 0], [10, 10]];
        assert_eq!(simplify(&pts, 2.0), vec![[0, 0], [10, 0], [10, 10]]);
    }
}

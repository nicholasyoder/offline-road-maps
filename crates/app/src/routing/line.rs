//! The route's line on the map (phase 3 step 3.6): simplified per zoom band from the full
//! geometry and drawn as line segments, as the roads are (`map.wgsl`), so it keeps its
//! width at every zoom and needs no tessellation.
//!
//! The Douglas–Peucker tolerance at which each point would be dropped is found once, when
//! the route arrives (on the worker); a band keeps the points above its tolerance, which
//! is the same line Douglas–Peucker would give at that tolerance. A band's segments are cut
//! into pieces, each with its own origin, so their positions stay small enough for `f32`
//! at any zoom, and pieces off screen are not drawn.
//!
//! Step 3.7 draws more lines the same way, each a [`Look`] with its own paint: the parts on
//! restricted roads and the step picked in the directions over the route, and dashed lines
//! from where the route meets each stop's road to the stop.

use std::ops::Range;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use omp_core::geo::{self, P};

use crate::camera::{self, Camera, TILE_PX};
use crate::pool::{Alloc, Kind, Pool};
use crate::style::ROUTE_PAINTS;

/// Bands are whole zooms up to this one; deeper zooms draw its line. Its tolerance (a
/// quarter point, about 0.3 m) is under the 2 m the pack's geometry is simplified to.
pub const MAX_BAND: u8 = 16;
/// The most a point may be off the band's line, in points at the band's zoom: up to twice
/// that at the next band's.
const TOLERANCE_PT: f64 = 0.25;
/// Segments in a piece, and the most a piece spans in points at its band's zoom.
const PIECE_SEGMENTS: usize = 256;
const PIECE_PT: f64 = 16_384.0;

/// A segment of the route, in points at the band's zoom from its piece's origin.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct RouteSegment {
    pub a: [f32; 2],
    pub b: [f32; 2],
}

/// The route's points, in world coordinates, in runs: one per leg, so a leg with no route
/// leaves a gap.
pub struct Line {
    parts: Vec<Vec<camera::Vec2>>,
    /// Per point of each part, the tolerance in world units below which it is kept; the
    /// ends are always kept.
    keep: Vec<Vec<f64>>,
    /// `[x0, y0, x1, y1]`.
    pub bounds: [f64; 4],
    pub points: usize,
}

impl Line {
    pub fn new<'a>(parts: impl IntoIterator<Item = &'a [P]>) -> Self {
        let mut bounds = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        let mut points = 0;
        let parts: Vec<Vec<camera::Vec2>> = parts
            .into_iter()
            .filter(|p| p.len() >= 2)
            .map(|p| {
                points += p.len();
                p.iter()
                    .map(|&q| {
                        let w = geo::to_unit(q);
                        bounds = [
                            bounds[0].min(w[0]),
                            bounds[1].min(w[1]),
                            bounds[2].max(w[0]),
                            bounds[3].max(w[1]),
                        ];
                        w
                    })
                    .collect()
            })
            .collect();
        let keep = parts.iter().map(|p| importance(p)).collect();
        Line {
            parts,
            keep,
            bounds,
            points,
        }
    }

    /// The route's line: one part per routed leg.
    pub fn of_trip(trip: &omp_core::route::Trip) -> Self {
        Self::new(trip.legs.iter().flatten().map(|l| l.points.as_slice()))
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// The band's segments in pieces.
    pub fn band(&self, band: u8) -> Band {
        let scale = TILE_PX * f64::from(band).exp2();
        let tol = TOLERANCE_PT / scale;
        let mut b = Band {
            band,
            segments: Vec::new(),
            pieces: Vec::new(),
        };
        for (pts, keep) in self.parts.iter().zip(&self.keep) {
            let kept: Vec<camera::Vec2> = pts
                .iter()
                .zip(keep)
                .filter(|&(_, &k)| k > tol)
                .map(|(&p, _)| p)
                .collect();
            let mut i = 0;
            while i + 1 < kept.len() {
                let origin = kept[i];
                let start = b.segments.len() as u32;
                let mut bounds = [origin[0], origin[1], origin[0], origin[1]];
                let local = |w: camera::Vec2| {
                    [
                        ((w[0] - origin[0]) * scale) as f32,
                        ((w[1] - origin[1]) * scale) as f32,
                    ]
                };
                let mut n = 0;
                while i + 1 < kept.len() && n < PIECE_SEGMENTS {
                    let q = kept[i + 1];
                    let far = (q[0] - origin[0]).abs().max((q[1] - origin[1]).abs()) * scale;
                    if n > 0 && far > PIECE_PT {
                        break;
                    }
                    b.segments.push(RouteSegment {
                        a: local(kept[i]),
                        b: local(q),
                    });
                    bounds = [
                        bounds[0].min(q[0]),
                        bounds[1].min(q[1]),
                        bounds[2].max(q[0]),
                        bounds[3].max(q[1]),
                    ];
                    n += 1;
                    i += 1;
                }
                b.pieces.push(Piece {
                    segments: start..b.segments.len() as u32,
                    origin,
                    bounds,
                });
            }
        }
        b
    }
}

/// Douglas–Peucker over the whole line, keeping for each point the distance at which it
/// splits its span, but never more than its parent's: then the points above a tolerance
/// are exactly those Douglas–Peucker keeps at it.
fn importance(pts: &[camera::Vec2]) -> Vec<f64> {
    let n = pts.len();
    let mut keep = vec![0.0; n];
    keep[0] = f64::INFINITY;
    keep[n - 1] = f64::INFINITY;
    let mut stack = vec![(0, n - 1, f64::INFINITY)];
    while let Some((a, b, cap)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (pa, pb) = (pts[a], pts[b]);
        let d = [pb[0] - pa[0], pb[1] - pa[1]];
        let len2 = d[0] * d[0] + d[1] * d[1];
        let (mut far, mut at) = (-1.0, a + 1);
        for (k, p) in pts.iter().enumerate().take(b).skip(a + 1) {
            let v = [p[0] - pa[0], p[1] - pa[1]];
            let dist = match len2 > 0.0 {
                true => {
                    let t = ((v[0] * d[0] + v[1] * d[1]) / len2).clamp(0.0, 1.0);
                    (v[0] - t * d[0]).hypot(v[1] - t * d[1])
                }
                false => v[0].hypot(v[1]),
            };
            if dist > far {
                (far, at) = (dist, k);
            }
        }
        let k = far.min(cap);
        keep[at] = k;
        stack.push((a, at, k));
        stack.push((at, b, k));
    }
    keep
}

/// A run of a band's segments with its own origin.
pub struct Piece {
    pub segments: Range<u32>,
    /// World coordinates of the segments' (0, 0).
    pub origin: camera::Vec2,
    /// `[x0, y0, x1, y1]`, world.
    pub bounds: [f64; 4],
}

/// The line simplified for one band.
pub struct Band {
    pub band: u8,
    pub segments: Vec<RouteSegment>,
    pub pieces: Vec<Piece>,
}

/// A band's segments on the GPU.
pub struct GpuBand {
    pub band: u8,
    pub segments: Alloc,
    pub pieces: Vec<Piece>,
}

impl GpuBand {
    pub fn upload(pool: &Arc<Pool>, b: Band) -> Option<Self> {
        if b.segments.is_empty() {
            return None;
        }
        Some(Self {
            band: b.band,
            segments: pool.upload(Kind::Vertex, bytemuck::cast_slice(&b.segments)),
            pieces: b.pieces,
        })
    }
}

/// The band to draw at `zoom`.
pub fn band_for(zoom: f64) -> u8 {
    (zoom.floor().max(0.0) as u8).min(MAX_BAND)
}

/// How a line of the route overlay is drawn, in the order drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// Dashed, from where the route meets a stop's road to the stop.
    Connector,
    Route,
    /// Over the route where it is on a private or destination-only road.
    Restricted,
    /// Over the route: the step picked in the directions.
    Picked,
}

impl Look {
    pub const ALL: [Look; 4] = [Look::Connector, Look::Route, Look::Restricted, Look::Picked];

    /// Its entry in the paint table (render.rs).
    pub fn paint(self) -> usize {
        ROUTE_PAINTS + self as usize
    }

    /// Drawn with a halo first. The lines over the route have none, so their ends do not
    /// cut into it.
    pub fn halo(self) -> bool {
        !matches!(self, Look::Restricted)
    }
}

/// One line of the overlay as a frame draws it: its band's segments, and the pieces on
/// screen with their origins in viewport pixels and pixels per unit.
pub struct LineDraw {
    pub look: Look,
    pub band: Arc<GpuBand>,
    pub pieces: Vec<(Range<u32>, [f32; 2], f32)>,
}

/// The route overlay's lines, in the order drawn.
#[derive(Default)]
pub struct RouteDraw {
    pub lines: Vec<LineDraw>,
}

impl RouteDraw {
    /// The pieces on screen of every line.
    pub fn pieces(&self) -> usize {
        self.lines.iter().map(|l| l.pieces.len()).sum()
    }
}

impl LineDraw {
    /// The pieces of `band` on `cam`'s view, `margin` points around it included.
    pub fn layout(
        look: Look,
        band: Arc<GpuBand>,
        cam: &Camera,
        pixels_per_point: f32,
        margin: f64,
    ) -> Self {
        let ppp = f64::from(pixels_per_point);
        let [x0, y0, x1, y1] = cam.visible();
        let m = margin / cam.scale();
        let scale = (cam.zoom - f64::from(band.band)).exp2() * ppp;
        let pieces = band
            .pieces
            .iter()
            .filter(|p| {
                let b = p.bounds;
                b[0] <= x1 + m && b[2] >= x0 - m && b[1] <= y1 + m && b[3] >= y0 - m
            })
            .map(|p| {
                let o = cam.world_to_screen(p.origin);
                (
                    p.segments.clone(),
                    [(o[0] * ppp) as f32, (o[1] * ppp) as f32],
                    scale as f32,
                )
            })
            .collect();
        Self { look, band, pieces }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn importance_is_douglas_peucker() {
        // A zigzag: the middle bump is 10 units off the base line, the small ones 1.
        let pts = [
            [0.0, 0.0],
            [1.0, 1.0],
            [2.0, 0.0],
            [5.0, 10.0],
            [8.0, 0.0],
            [9.0, 1.0],
            [10.0, 0.0],
        ];
        let k = importance(&pts);
        assert_eq!(k[0], f64::INFINITY);
        assert_eq!(k[6], f64::INFINITY);
        assert_eq!(k[3], 10.0);
        // Never more than the point that split their span.
        assert!(k.iter().all(|&v| v > 0.0));
        assert!(k[1] <= k[3] && k[5] <= k[3]);
        // Above 2 only the ends and the bump are left; the bump's feet are 1.79 off the
        // lines from the ends to it.
        let kept = |tol: f64| (0..7).filter(|&i| k[i] > tol).collect::<Vec<usize>>();
        assert_eq!(kept(2.0), [0, 3, 6]);
        assert_eq!(kept(1.5), [0, 2, 3, 4, 6]);
    }

    #[test]
    fn bands_simplify_and_cut_into_pieces() {
        // A straight line of 1,001 points with a spike at its middle.
        let mut pts: Vec<P> = (0..=1000u32)
            .map(|i| [1_000_000 + i * 4_000, 2_000_000_000])
            .collect();
        pts[500][1] += 400_000;
        let line = Line::new([pts.as_slice(), &pts[..1]]);
        // The one-point part is dropped.
        assert_eq!(line.points, 1001);
        let low = line.band(0);
        // At z0 the spike is under a quarter point high: one segment.
        assert_eq!(low.segments.len(), 1);
        let high = line.band(MAX_BAND);
        // Collinear points go even at the deepest band; the spike's three corners stay.
        assert_eq!(high.segments.len(), 4);
        // A zigzag 1.5 points high at z16 (a point is 128 units there) keeps every point,
        // in pieces of at most 256 segments, each starting where the last ended.
        let zigzag: Vec<P> = (0..=1000u32)
            .map(|i| [1_000_000 + i * 4_000, 2_000_000_000 + (i % 2) * 200])
            .collect();
        let fine = Line::new([zigzag.as_slice()]);
        let b = fine.band(MAX_BAND);
        assert_eq!(b.segments.len(), 1000);
        assert_eq!(b.pieces.len(), 4);
        for w in b.pieces.windows(2) {
            assert_eq!(w[0].segments.end, w[1].segments.start);
            let s = &b.segments[w[0].segments.end as usize - 1];
            let scale = TILE_PX * f64::from(MAX_BAND).exp2();
            let end = [
                w[0].origin[0] + f64::from(s.b[0]) / scale,
                w[0].origin[1] + f64::from(s.b[1]) / scale,
            ];
            assert!((end[0] - w[1].origin[0]).abs() * scale < 0.01);
        }
        assert!(b.segments.iter().all(|s| {
            s.a.iter()
                .chain(&s.b)
                .all(|v| v.abs() <= PIECE_PT as f32 + 1.0)
        }));
    }
}

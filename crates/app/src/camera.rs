//! Web Mercator camera, north up and without rotation (decision 7), and the motion that
//! moves it: inertia after a drag, animated zoom around a point, and flights to a search
//! result.
//!
//! World coordinates are Web Mercator scaled to 0‥1 in both axes, y down, as in tile
//! numbering. Screen coordinates are egui points from the map's top-left corner.

/// Points per tile at an integer zoom; the tiles' `area` prop assumes 512 (format.md).
pub const TILE_PX: f64 = 512.0;
pub const MIN_ZOOM: f64 = 0.0;
/// z14 is the deepest tile zoom; z15–z18 overzoom it.
pub const MAX_ZOOM: f64 = 18.0;
const MAX_LAT: f64 = 85.051_128_779_806_59;

pub type Vec2 = [f64; 2];

pub fn lonlat_to_world(lon: f64, lat: f64) -> Vec2 {
    let lat = lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    let x = (lon + 180.0) / 360.0;
    let y = 0.5 - (lat.tan() + 1.0 / lat.cos()).ln() / (2.0 * std::f64::consts::PI);
    [x, y]
}

/// `[lon, lat]` of a world point.
pub fn world_to_lonlat(p: Vec2) -> Vec2 {
    let lon = p[0] * 360.0 - 180.0;
    let n = std::f64::consts::PI * (1.0 - 2.0 * p[1]);
    let lat = n.sinh().atan().to_degrees();
    [lon, lat]
}

#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    /// World point at the middle of the viewport.
    pub center: Vec2,
    pub zoom: f64,
    /// Viewport size in points.
    pub size: Vec2,
    /// World rectangle `[x0, y0, x1, y1]` the centre is kept near.
    pub bounds: [f64; 4],
}

impl Camera {
    pub fn new(bounds: [f64; 4]) -> Self {
        let mut c = Self {
            center: [0.5, 0.5],
            zoom: 2.0,
            size: [800.0, 600.0],
            bounds,
        };
        c.fit_bounds();
        c
    }

    /// Points per world unit.
    pub fn scale(&self) -> f64 {
        TILE_PX * self.zoom.exp2()
    }

    pub fn screen_to_world(&self, p: Vec2) -> Vec2 {
        let s = self.scale();
        [
            self.center[0] + (p[0] - self.size[0] / 2.0) / s,
            self.center[1] + (p[1] - self.size[1] / 2.0) / s,
        ]
    }

    pub fn world_to_screen(&self, w: Vec2) -> Vec2 {
        let s = self.scale();
        [
            (w[0] - self.center[0]) * s + self.size[0] / 2.0,
            (w[1] - self.center[1]) * s + self.size[1] / 2.0,
        ]
    }

    /// The world rectangle on screen, `[x0, y0, x1, y1]`.
    pub fn visible(&self) -> [f64; 4] {
        let [x0, y0] = self.screen_to_world([0.0, 0.0]);
        let [x1, y1] = self.screen_to_world(self.size);
        [x0, y0, x1, y1]
    }

    /// Moves the map content by `d` points, as a drag does.
    pub fn pan(&mut self, d: Vec2) {
        let s = self.scale();
        self.center[0] -= d[0] / s;
        self.center[1] -= d[1] / s;
        self.clamp();
    }

    /// Changes the zoom by `dz`, keeping the world point under `anchor` (screen) still.
    pub fn zoom_around(&mut self, dz: f64, anchor: Vec2) {
        let before = self.screen_to_world(anchor);
        self.zoom = (self.zoom + dz).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.screen_to_world(anchor);
        self.center[0] += before[0] - after[0];
        self.center[1] += before[1] - after[1];
        self.clamp();
    }

    pub fn set_lonlat(&mut self, lon: f64, lat: f64, zoom: f64) {
        self.center = lonlat_to_world(lon, lat);
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.clamp();
    }

    pub fn lonlat(&self) -> Vec2 {
        world_to_lonlat(self.center)
    }

    /// Centres the bounds and zooms so they fill the viewport.
    pub fn fit_bounds(&mut self) {
        let [x0, y0, x1, y1] = self.bounds;
        self.center = [(x0 + x1) / 2.0, (y0 + y1) / 2.0];
        let fit = |extent: f64, px: f64| (px / (extent.max(1e-9) * TILE_PX)).log2();
        self.zoom = fit(x1 - x0, self.size[0])
            .min(fit(y1 - y0, self.size[1]))
            .clamp(MIN_ZOOM, MAX_ZOOM);
    }

    pub fn contains_center(&self) -> bool {
        let [x0, y0, x1, y1] = self.bounds;
        (x0..=x1).contains(&self.center[0]) && (y0..=y1).contains(&self.center[1])
    }

    /// Keeps the zoom in range and the centre inside the bounds plus a quarter viewport,
    /// so some of the region always stays on screen.
    pub fn clamp(&mut self) {
        self.zoom = self.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let s = self.scale();
        let [x0, y0, x1, y1] = self.bounds;
        let (mx, my) = (self.size[0] / 4.0 / s, self.size[1] / 4.0 / s);
        self.center[0] = self.center[0].clamp((x0 - mx).max(0.0), (x1 + mx).min(1.0));
        self.center[1] = self.center[1].clamp((y0 - my).max(0.0), (y1 + my).min(1.0));
    }
}

/// Inertia and zoom animation between input events.
#[derive(Clone, Debug, Default)]
pub struct Motion {
    /// Pan velocity in points per second, after a drag ends.
    pub velocity: Vec2,
    /// Zoom still to apply, and the screen point it is anchored at.
    zoom_left: f64,
    anchor: Vec2,
    fly: Option<Fly>,
}

/// A flight to a view: out, across and in, with the centre moving at a steady speed on
/// screen.
#[derive(Clone, Debug)]
struct Fly {
    from: (Vec2, f64),
    to: (Vec2, f64),
    /// How far below the straight line from one zoom to the other the middle dips, so both
    /// ends fit on screen there.
    dip: f64,
    seconds: f64,
    t: f64,
    /// The share of the way across at each of `FLY_SAMPLES` + 1 even steps of the path.
    across: Vec<f64>,
}

const FLY_SAMPLES: usize = 64;

impl Fly {
    fn new(cam: &Camera, to: Vec2, zoom: f64) -> Self {
        let from = (cam.center, cam.zoom);
        let d = (to[0] - from.0[0]).hypot(to[1] - from.0[1]);
        let side = cam.size[0].min(cam.size[1]).max(1.0);
        let fit = (side / (d.max(1e-12) * TILE_PX)).log2() - 0.5;
        let dip = ((from.1 + zoom) / 2.0 - fit).max(0.0);
        let mut f = Self {
            from,
            to: (to, zoom),
            dip,
            seconds: (0.8 + 0.1 * (2.0 * dip + (zoom - from.1).abs())).min(2.5),
            t: 0.0,
            across: Vec::with_capacity(FLY_SAMPLES + 1),
        };
        // The centre moves 2^-zoom world units per step of the path for a steady speed
        // on screen.
        let mut sum = 0.0;
        f.across.push(0.0);
        for i in 0..FLY_SAMPLES {
            let s = (i as f64 + 0.5) / FLY_SAMPLES as f64;
            sum += (-f.zoom_at(s)).exp2();
            f.across.push(sum);
        }
        for a in &mut f.across {
            *a /= sum;
        }
        f
    }

    fn zoom_at(&self, s: f64) -> f64 {
        self.from.1 + (self.to.1 - self.from.1) * s - self.dip * 4.0 * s * (1.0 - s)
    }

    /// The view at path position `s` (0‥1).
    fn at(&self, s: f64) -> (Vec2, f64) {
        let x = s * FLY_SAMPLES as f64;
        let i = (x as usize).min(FLY_SAMPLES - 1);
        let u = self.across[i] + (self.across[i + 1] - self.across[i]) * (x - i as f64);
        let (a, b) = (self.from.0, self.to.0);
        (
            [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u],
            self.zoom_at(s),
        )
    }
}

/// Inertia slows by this factor per second.
const FRICTION: f64 = 0.004;
/// The animated zoom covers this share of what is left per second (exponential approach).
const ZOOM_RATE: f64 = 14.0;

impl Motion {
    /// Adds `dz` to the animated zoom around `anchor`.
    pub fn zoom(&mut self, dz: f64, anchor: Vec2) {
        self.fly = None;
        self.zoom_left += dz;
        self.anchor = anchor;
    }

    /// Flies the camera to `center` at `zoom`, replacing any other motion.
    pub fn fly_to(&mut self, cam: &Camera, center: Vec2, zoom: f64) {
        *self = Self {
            fly: Some(Fly::new(cam, center, zoom.clamp(MIN_ZOOM, MAX_ZOOM))),
            ..Self::default()
        };
    }

    /// Ends a flight where it is, as a drag or a key does.
    pub fn stop_flying(&mut self) {
        self.fly = None;
    }

    pub fn moving(&self) -> bool {
        self.velocity != [0.0, 0.0] || self.zoom_left != 0.0 || self.fly.is_some()
    }

    /// Advances by `dt` seconds.
    pub fn step(&mut self, cam: &mut Camera, dt: f64) {
        if let Some(f) = &mut self.fly {
            f.t += dt;
            let x = (f.t / f.seconds).min(1.0);
            let (c, z) = f.at(x * x * (3.0 - 2.0 * x));
            cam.center = c;
            cam.zoom = z;
            cam.clamp();
            if x >= 1.0 {
                self.fly = None;
            }
        }
        if self.zoom_left != 0.0 {
            let dz = if self.zoom_left.abs() < 0.002 {
                self.zoom_left
            } else {
                self.zoom_left * (1.0 - (-ZOOM_RATE * dt).exp())
            };
            let before = cam.zoom;
            cam.zoom_around(dz, self.anchor);
            // At a zoom limit the rest can never be applied.
            self.zoom_left = if cam.zoom == before {
                0.0
            } else {
                self.zoom_left - dz
            };
        }
        if self.velocity != [0.0, 0.0] {
            let before = cam.center;
            cam.pan([self.velocity[0] * dt, self.velocity[1] * dt]);
            let k = FRICTION.powf(dt);
            self.velocity = [self.velocity[0] * k, self.velocity[1] * k];
            if self.velocity[0].hypot(self.velocity[1]) < 5.0 || cam.center == before {
                self.velocity = [0.0, 0.0];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn mercator_round_trip() {
        for (lon, lat) in [(0.0, 0.0), (-81.7, 41.5), (-123.1, 49.3), (179.9, -60.0)] {
            let [lon2, lat2] = world_to_lonlat(lonlat_to_world(lon, lat));
            assert!(close(lon, lon2) && close(lat, lat2), "{lon},{lat}");
        }
        assert!(close(lonlat_to_world(0.0, 0.0)[1], 0.5));
        assert!(lonlat_to_world(0.0, 89.0)[1] >= 0.0);
    }

    #[test]
    fn zoom_keeps_anchor_still() {
        let mut c = Camera::new([0.0, 0.0, 1.0, 1.0]);
        c.size = [1000.0, 800.0];
        c.zoom = 6.0;
        c.center = [0.3, 0.4];
        let anchor = [100.0, 700.0];
        let w = c.screen_to_world(anchor);
        c.zoom_around(1.5, anchor);
        let s = c.world_to_screen(w);
        assert!(close(s[0], anchor[0]) && close(s[1], anchor[1]));
        assert!(close(c.zoom, 7.5));
    }

    #[test]
    fn clamped_to_bounds_and_zoom() {
        let mut c = Camera::new([0.2, 0.3, 0.3, 0.4]);
        c.size = [800.0, 800.0];
        c.zoom = 10.0;
        c.pan([1e9, 1e9]);
        let margin = 200.0 / c.scale();
        assert!(close(c.center[0], 0.2 - margin) && close(c.center[1], 0.3 - margin));
        c.zoom_around(100.0, [0.0, 0.0]);
        assert_eq!(c.zoom, MAX_ZOOM);
    }

    #[test]
    fn fit_shows_bounds() {
        let mut c = Camera::new([0.1, 0.2, 0.3, 0.3]);
        c.size = [1024.0, 512.0];
        c.fit_bounds();
        let [x0, y0, x1, y1] = c.visible();
        assert!(x0 <= 0.1 + 1e-9 && x1 >= 0.3 - 1e-9 && y0 <= 0.2 + 1e-9 && y1 >= 0.3 - 1e-9);
    }

    #[test]
    fn flight_ends_at_target_and_dips_on_long_ones() {
        let mut c = Camera::new([0.0, 0.0, 1.0, 1.0]);
        c.size = [1000.0, 800.0];
        c.center = [0.2, 0.3];
        c.zoom = 12.0;
        let mut m = Motion::default();
        m.fly_to(&c, [0.25, 0.35], 15.0);
        let mut lowest = c.zoom;
        for _ in 0..600 {
            m.step(&mut c, 1.0 / 60.0);
            lowest = lowest.min(c.zoom);
        }
        assert!(!m.moving());
        assert!(close(c.center[0], 0.25) && close(c.center[1], 0.35) && close(c.zoom, 15.0));
        // Both ends 0.07 apart fit in 800 points around z3.5.
        assert!(lowest < 5.0, "{lowest}");
        // A drag ends it.
        m.fly_to(&c, [0.2, 0.3], 12.0);
        m.step(&mut c, 0.1);
        m.stop_flying();
        assert!(!m.moving());
    }

    #[test]
    fn motion_settles() {
        let mut c = Camera::new([0.0, 0.0, 1.0, 1.0]);
        c.zoom = 5.0;
        let mut m = Motion {
            velocity: [1000.0, 0.0],
            ..Default::default()
        };
        m.zoom(1.0, [400.0, 300.0]);
        for _ in 0..600 {
            m.step(&mut c, 1.0 / 60.0);
        }
        assert!(!m.moving());
        assert!(close(c.zoom, 6.0));
    }
}

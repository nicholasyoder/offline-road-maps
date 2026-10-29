//! Print layout (phase 4 step 4.9): the paper, where the map goes on it, and how the
//! rectangle chosen on screen becomes the page's view of the ground.
//!
//! A page is laid out in points (1/72 in), top-left origin. The map is drawn as the app
//! would draw a window the size of the page's map area, at the printer's resolution; the
//! rectangle's ground width over the map's width on paper sets the zoom, so the page covers
//! exactly the rectangle. *Text size* scales that window: a larger text size is a smaller
//! window of the same paper, so labels print larger and the map's zoom is lower.

use omp_core::geo;
use serde::{Deserialize, Serialize};

use crate::camera::{self, Camera};
use crate::scale::{self, Units};

pub const MM_PT: f64 = 72.0 / 25.4;
/// Margins on every side, 0.25 in: the command-line CUPS tools do not report the
/// printable area (step 4.10).
pub const MARGIN_PT: f64 = 18.0;
/// The one-line header and the credit line under the map, at normal text size.
const HEADER_PT: f64 = 22.0;
const FOOTER_PT: f64 = 14.0;

pub const CREDIT: &str = "© OpenStreetMap contributors";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Paper {
    #[default]
    Letter,
    Legal,
    Tabloid,
    A5,
    A4,
    A3,
}

impl Paper {
    pub const ALL: [Paper; 6] = [
        Paper::Letter,
        Paper::Legal,
        Paper::Tabloid,
        Paper::A5,
        Paper::A4,
        Paper::A3,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Paper::Letter => "Letter",
            Paper::Legal => "Legal",
            Paper::Tabloid => "Tabloid",
            Paper::A5 => "A5",
            Paper::A4 => "A4",
            Paper::A3 => "A3",
        }
    }

    /// Width and height in points, portrait.
    pub fn size_pt(self) -> [f64; 2] {
        match self {
            Paper::Letter => [612.0, 792.0],
            Paper::Legal => [612.0, 1008.0],
            Paper::Tabloid => [792.0, 1224.0],
            Paper::A5 => [148.0 * MM_PT, 210.0 * MM_PT],
            Paper::A4 => [210.0 * MM_PT, 297.0 * MM_PT],
            Paper::A3 => [297.0 * MM_PT, 420.0 * MM_PT],
        }
    }

    /// Its name as it is given on the command line.
    pub fn parse(s: &str) -> Option<Paper> {
        Paper::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(s.trim()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    /// From the shape of the area to print, when the print page opens.
    #[default]
    Auto,
    Portrait,
    Landscape,
}

impl Orientation {
    pub const ALL: [Orientation; 3] = [
        Orientation::Auto,
        Orientation::Portrait,
        Orientation::Landscape,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Orientation::Auto => "Automatic",
            Orientation::Portrait => "Portrait",
            Orientation::Landscape => "Landscape",
        }
    }

    pub fn parse(s: &str) -> Option<Orientation> {
        Orientation::ALL
            .into_iter()
            .find(|o| o.name().eq_ignore_ascii_case(s.trim()))
            .or_else(|| {
                s.trim()
                    .eq_ignore_ascii_case("auto")
                    .then_some(Orientation::Auto)
            })
    }

    /// Whether the page is landscape, given whether the area to print is wider than tall.
    pub fn landscape(self, wide: bool) -> bool {
        match self {
            Orientation::Auto => wide,
            Orientation::Portrait => false,
            Orientation::Landscape => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSize {
    Small,
    #[default]
    Normal,
    Large,
}

impl TextSize {
    pub const ALL: [TextSize; 3] = [TextSize::Small, TextSize::Normal, TextSize::Large];

    pub fn name(self) -> &'static str {
        match self {
            TextSize::Small => "Small",
            TextSize::Normal => "Normal",
            TextSize::Large => "Large",
        }
    }

    /// Printed size of the text and symbols against the screen's: the window the map is
    /// drawn for is the paper's divided by this.
    pub fn factor(self) -> f64 {
        match self {
            TextSize::Small => 0.8,
            TextSize::Normal => 1.0,
            TextSize::Large => 1.3,
        }
    }

    pub fn parse(s: &str) -> Option<TextSize> {
        TextSize::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(s.trim()))
    }
}

/// The print settings kept in `config.toml`; the label is typed for each printout.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub paper: Paper,
    pub orientation: Orientation,
    pub text: TextSize,
    pub copies: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            paper: Paper::default(),
            orientation: Orientation::default(),
            text: TextSize::default(),
            copies: 1,
        }
    }
}

/// Where things go on one page, in points from its top-left corner. Rectangles are
/// `[x0, y0, x1, y1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub page: [f64; 2],
    /// The one-line header, when the page has a label.
    pub header: Option<[f64; 4]>,
    pub map: [f64; 4],
    /// The credit line under the map.
    pub footer: [f64; 4],
    pub text: TextSize,
}

impl Layout {
    pub fn new(paper: Paper, landscape: bool, label: bool, text: TextSize) -> Self {
        let [w, h] = paper.size_pt();
        let page = if landscape { [h, w] } else { [w, h] };
        let s = text.factor();
        let m = MARGIN_PT;
        let top = m + if label { HEADER_PT * s } else { 0.0 };
        let foot = FOOTER_PT * s;
        Self {
            page,
            header: label.then_some([m, m, page[0] - m, top]),
            map: [m, top, page[0] - m, page[1] - m - foot],
            footer: [m, page[1] - m - foot, page[0] - m, page[1] - m],
            text,
        }
    }

    pub fn map_size(&self) -> [f64; 2] {
        [self.map[2] - self.map[0], self.map[3] - self.map[1]]
    }

    /// The map area's height over its width.
    pub fn aspect(&self) -> f64 {
        let [w, h] = self.map_size();
        h / w
    }

    /// The size in pixels of a page at `dpi`.
    pub fn pixels(&self, dpi: f64) -> [u32; 2] {
        self.page.map(|v| (v * dpi / 72.0).round() as u32)
    }
}

/// The ground a page's map shows: a rectangle in world units, as the screen's camera
/// uses them, with the page map's aspect ratio. Kept as its centre and width; the height
/// follows from the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub center: [f64; 2],
    pub width: f64,
}

impl Area {
    /// The `[x0, y0, x1, y1]` for a map area whose height over width is `aspect`.
    pub fn rect(&self, aspect: f64) -> [f64; 4] {
        let h = self.width * aspect;
        [
            self.center[0] - self.width / 2.0,
            self.center[1] - h / 2.0,
            self.center[0] + self.width / 2.0,
            self.center[1] + h / 2.0,
        ]
    }

    /// The smallest area of this aspect that holds `bounds` with `margin` (a share of the
    /// area) on every side.
    pub fn fit(bounds: [f64; 4], aspect: f64, margin: f64) -> Self {
        let [x0, y0, x1, y1] = bounds;
        let (w, h) = ((x1 - x0).max(1e-9), (y1 - y0).max(1e-9));
        let width = w.max(h / aspect) / (1.0 - 2.0 * margin);
        Self {
            center: [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
            width,
        }
    }

    /// What the screen's map shows now, as an area for a page of this aspect: the whole
    /// view if it fits, else the view's largest rectangle of that shape.
    pub fn of_view(cam: &Camera, aspect: f64) -> Self {
        let [x0, y0, x1, y1] = cam.visible();
        let (w, h) = (x1 - x0, y1 - y0);
        Self {
            center: cam.center,
            width: w.min(h / aspect),
        }
    }

    /// The widths a page can show: the deepest zoom, and the whole world.
    pub fn width_range(layout: &Layout) -> [f64; 2] {
        let win = window_width(layout);
        [
            win / (camera::TILE_PX * camera::MAX_ZOOM.exp2()),
            win / (camera::TILE_PX * camera::MIN_ZOOM.exp2()),
        ]
    }

    pub fn clamped(mut self, layout: &Layout) -> Self {
        let [lo, hi] = Self::width_range(layout);
        self.width = self.width.clamp(lo, hi);
        self
    }

    /// The map zoom that draws the page: fractional, within the camera's range.
    pub fn zoom(&self, layout: &Layout) -> f64 {
        (window_width(layout) / (self.width * camera::TILE_PX))
            .log2()
            .clamp(camera::MIN_ZOOM, camera::MAX_ZOOM)
    }

    /// The camera that draws the page's map: a window the size of the map area in
    /// points, divided by the text size, over this area.
    pub fn camera(&self, layout: &Layout, bounds: [f64; 4]) -> Camera {
        let mut cam = Camera::new(bounds);
        cam.size = [window_width(layout), window_width(layout) * layout.aspect()];
        cam.center = self.center;
        cam.zoom = self.zoom(layout);
        cam
    }

    /// Ground metres per point of paper along the map's width, at the area's centre.
    pub fn metres_per_page_point(&self, layout: &Layout) -> f64 {
        let lat = geo::unit_to_lonlat(self.center)[1];
        let per_world = layout.map_size()[0] / self.width;
        scale::metres_per_point(lat, per_world)
    }

    /// "1:250,000, 1 in ≈ 4 mi" (or "1 cm ≈ 2.5 km", with kilometres).
    pub fn scale_text(&self, layout: &Layout, units: Units) -> String {
        let per_pt = self.metres_per_page_point(layout);
        let ratio = per_pt * 72.0 / 0.0254;
        let ratio = round_sig(ratio, 2);
        let mut text = format!("1:{}", thousands(ratio));
        let inch = per_pt * 72.0;
        text += &match units {
            Units::Kilometres => {
                format!(", 1 cm ≈ {}", length(per_pt * 72.0 / 2.54, false))
            }
            _ => format!(", 1 in ≈ {}", length(inch, true)),
        };
        text
    }
}

/// The map window's width in points: the map area's on paper over the text size.
fn window_width(layout: &Layout) -> f64 {
    layout.map_size()[0] / layout.text.factor()
}

/// A length in metres as "4 mi" or "2.5 km", two significant digits.
fn length(m: f64, imperial: bool) -> String {
    let (v, unit) = if imperial {
        let miles = m / 1609.344;
        if miles >= 0.1 {
            (miles, "mi")
        } else {
            (m / 0.3048, "ft")
        }
    } else if m >= 1000.0 {
        (m / 1000.0, "km")
    } else {
        (m, "m")
    };
    let v = round_sig(v, 2);
    let digits = if v >= 10.0 { 0 } else { 1 };
    let text = format!("{v:.digits$}");
    format!("{} {unit}", text.trim_end_matches(".0"))
}

fn round_sig(v: f64, digits: i32) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return 0.0;
    }
    let p = 10f64.powi(v.log10().floor() as i32 - digits + 1);
    (v / p).round() * p
}

fn thousands(v: f64) -> String {
    let s = format!("{v:.0}");
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORLD: [f64; 4] = [0.0, 0.0, 1.0, 1.0];

    fn layout() -> Layout {
        Layout::new(Paper::Letter, true, true, TextSize::Normal)
    }

    #[test]
    fn letter_pages() {
        let l = Layout::new(Paper::Letter, false, false, TextSize::Normal);
        assert_eq!(l.page, [612.0, 792.0]);
        assert_eq!(l.header, None);
        assert_eq!(l.map, [18.0, 18.0, 594.0, 760.0]);
        // 300 dpi: 2550 x 3300.
        assert_eq!(l.pixels(300.0), [2550, 3300]);
        let l = Layout::new(Paper::Letter, true, true, TextSize::Normal);
        assert_eq!(l.page, [792.0, 612.0]);
        assert_eq!(l.header, Some([18.0, 18.0, 774.0, 40.0]));
        assert_eq!(l.map[1], 40.0);
    }

    #[test]
    fn the_map_never_leaves_the_margins() {
        for paper in Paper::ALL {
            for text in TextSize::ALL {
                for (land, label) in [(false, false), (true, true)] {
                    let l = Layout::new(paper, land, label, text);
                    assert!(l.map[0] >= MARGIN_PT && l.map[1] >= MARGIN_PT);
                    assert!(l.map[2] <= l.page[0] - MARGIN_PT);
                    assert!(l.footer[3] <= l.page[1] - MARGIN_PT + 1e-9);
                    assert!(l.map[3] <= l.footer[1] + 1e-9);
                }
            }
        }
    }

    #[test]
    fn the_camera_shows_exactly_the_area() {
        // Headless check of the page's ground extent (criterion 7: edges within 1 mm).
        let l = layout();
        let area = Area {
            center: [0.28, 0.37],
            width: 0.01,
        };
        let cam = area.camera(&l, WORLD);
        let want = area.rect(l.aspect());
        let got = cam.visible();
        for (w, g) in want.iter().zip(got) {
            assert!((w - g).abs() < 1e-12, "{want:?} vs {got:?}");
        }
        // 1 mm on paper, in world units.
        let mm = area.width / l.map_size()[0] * MM_PT;
        assert!((want[0] - got[0]).abs() < mm / 1000.0);
        // Pixels at 300 dpi: the area's width is the map's width in pixels.
        let ppp = 300.0 / 72.0;
        let w_px = cam.size[0] * ppp * l.text.factor();
        assert!((w_px - l.map_size()[0] * ppp).abs() < 1e-6);
    }

    #[test]
    fn text_size_changes_the_zoom_not_the_ground() {
        let area = Area {
            center: [0.28, 0.37],
            width: 0.01,
        };
        let mut zooms = Vec::new();
        for text in TextSize::ALL {
            let l = Layout::new(Paper::Letter, true, true, text);
            let cam = area.camera(&l, WORLD);
            let want = area.rect(l.aspect());
            for (w, v) in want.iter().zip(cam.visible()) {
                assert!((w - v).abs() < 1e-12);
            }
            zooms.push(cam.zoom);
        }
        // Larger text, lower zoom: a smaller window of the same ground.
        assert!(zooms[0] > zooms[1] && zooms[1] > zooms[2], "{zooms:?}");
    }

    #[test]
    fn widths_stay_within_the_zooms() {
        let l = layout();
        let [lo, hi] = Area::width_range(&l);
        let tiny = Area {
            center: [0.5, 0.5],
            width: lo / 10.0,
        }
        .clamped(&l);
        assert_eq!(tiny.width, lo);
        assert!((tiny.zoom(&l) - camera::MAX_ZOOM).abs() < 1e-9);
        let huge = Area {
            center: [0.5, 0.5],
            width: hi * 10.0,
        }
        .clamped(&l);
        assert!((huge.zoom(&l) - camera::MIN_ZOOM).abs() < 1e-9);
    }

    #[test]
    fn fitting_holds_the_bounds_with_a_margin() {
        let l = layout();
        let b = [0.30, 0.40, 0.32, 0.41];
        let area = Area::fit(b, l.aspect(), 0.05);
        let r = area.rect(l.aspect());
        assert!(r[0] < b[0] && r[1] < b[1] && r[2] > b[2] && r[3] > b[3]);
        // The tight direction has the margin, 5% of the area each side.
        let slack = (b[0] - r[0]) / area.width;
        assert!(slack >= 0.05 - 1e-12, "{slack}");
    }

    #[test]
    fn the_scale_is_true_to_the_ground() {
        // A Letter landscape page covering 100 km at the equator: 756 pt for 100 km.
        let l = layout();
        let world = geo::EARTH_CIRCUMFERENCE_M;
        let area = Area {
            center: [0.5, 0.5],
            width: 100_000.0 / world,
        };
        let per_pt = area.metres_per_page_point(&l);
        assert!((per_pt * l.map_size()[0] - 100_000.0).abs() < 1e-6);
        // The printed bar is within 1% of the ground (criterion 7).
        let (len, text) = scale::bar(per_pt, 120.0, false);
        assert_eq!(text, "10 km");
        assert!((len * per_pt - 10_000.0).abs() < 1e-6);
    }

    #[test]
    fn scale_text_reads_well() {
        let l = layout();
        let world = geo::EARTH_CIRCUMFERENCE_M;
        // 1:250,000 on the page: 756 pt = 756/72*0.0254 m of paper = 0.2667 m -> 66.7 km.
        let paper_m = l.map_size()[0] / 72.0 * 0.0254;
        let area = Area {
            center: [0.5, 0.5],
            width: paper_m * 250_000.0 / world,
        };
        assert_eq!(
            area.scale_text(&l, Units::Miles),
            "1:250,000, 1 in ≈ 3.9 mi"
        );
        assert_eq!(
            area.scale_text(&l, Units::Kilometres),
            "1:250,000, 1 cm ≈ 2.5 km"
        );
    }

    #[test]
    fn view_becomes_an_area_of_the_pages_shape() {
        let l = layout();
        let mut cam = Camera::new(WORLD);
        cam.size = [1000.0, 800.0];
        cam.zoom = 10.0;
        cam.center = [0.3, 0.4];
        let area = Area::of_view(&cam, l.aspect());
        let r = area.rect(l.aspect());
        let v = cam.visible();
        assert!(r[0] >= v[0] - 1e-12 && r[2] <= v[2] + 1e-12);
        assert!(r[1] >= v[1] - 1e-12 && r[3] <= v[3] + 1e-12);
    }

    #[test]
    fn settings_round_trip_through_toml() {
        let s = Settings {
            paper: Paper::A4,
            orientation: Orientation::Landscape,
            text: TextSize::Large,
            copies: 2,
        };
        let text = toml::to_string(&s).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), s);
        assert_eq!(toml::from_str::<Settings>("").unwrap(), Settings::default());
    }

    #[test]
    fn names_parse() {
        assert_eq!(Paper::parse("a4"), Some(Paper::A4));
        assert_eq!(Orientation::parse("auto"), Some(Orientation::Auto));
        assert_eq!(
            Orientation::parse("Landscape"),
            Some(Orientation::Landscape)
        );
        assert_eq!(TextSize::parse("LARGE"), Some(TextSize::Large));
        assert_eq!(Paper::parse("foolscap"), None);
    }
}

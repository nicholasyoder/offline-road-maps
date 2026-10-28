//! `omp-app --render`: draws one view offscreen to a PNG, without a window. For reviewing
//! renderer and style changes, and for the reference screenshots in `docs/screenshots/`.
//! With `--route`, it routes through the stops and draws the route as the app does (with
//! its restricted parts and the lines to stops off the road), fitting the view to it unless
//! `--center` and `--zoom` say otherwise. With `--alternatives`, each leg's alternatives
//! (phase 4 step 4.3) are drawn grey under it, as the app draws an open leg's.

use std::collections::HashMap;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::camera::Camera;
use crate::labels::{self, Placer, TileLabels};
use crate::layers::Hidden;
use crate::pool::Pool;
use crate::render::{self, GpuMesh, Renderer};
use crate::routing::{self, GpuBand, Line, LineDraw, Look, RouteDraw};
use crate::style::{Style, StyleFile};
use crate::tiles::{self, Known, TileId};
use crate::{loader, mesh, packs};

/// A loaded tile: its mesh and labels.
type Loaded = Option<(Arc<GpuMesh>, Arc<TileLabels>)>;

pub struct Snapshot {
    pub pack: PathBuf,
    /// `None` with a route: fitted to it.
    pub center: Option<(f64, f64)>,
    pub zoom: Option<f64>,
    /// Stops to route through, `(lat, lon)`.
    pub route: Vec<(f64, f64)>,
    /// A step of the route's directions to draw over it, from 0; the view fits it.
    pub step: Option<usize>,
    /// Find each leg's alternatives, show the best and draw the others.
    pub alternatives: bool,
    /// In pixels.
    pub size: [u32; 2],
    pub pixels_per_point: f32,
    pub msaa: u32,
    pub style: StyleFile,
    pub out: PathBuf,
}

impl Snapshot {
    /// From `--center <lat,lon>`, `--zoom <z>`, `--route <lat,lon;lat,lon…>`, `--size
    /// <w>x<h>`, `--scale <ppp>` and `--msaa <samples>`.
    #[allow(clippy::too_many_arguments)]
    pub fn parse(
        pack: PathBuf,
        center: Option<&str>,
        zoom: Option<&str>,
        route: Option<&str>,
        size: Option<&str>,
        scale: Option<&str>,
        msaa: Option<&str>,
        out: PathBuf,
    ) -> Result<Self, String> {
        let num = |s: &str, what: &str| {
            s.trim()
                .parse::<f64>()
                .map_err(|_| format!("bad {what}: {s}"))
        };
        let lat_lon = |s: &str, what: &str| -> Result<(f64, f64), String> {
            let (lat, lon) = s
                .split_once(',')
                .ok_or_else(|| format!("{what} wants <lat,lon>, got {s}"))?;
            Ok((num(lat, "latitude")?, num(lon, "longitude")?))
        };
        let route = match route {
            Some(r) => r
                .split(';')
                .map(|s| lat_lon(s, "a --route stop"))
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        if route.len() == 1 {
            return Err("--route needs two stops or more".into());
        }
        if route.is_empty() && (center.is_none() || zoom.is_none()) {
            return Err("--render needs --center and --zoom, or --route".into());
        }
        let size = match size {
            Some(s) => {
                let (w, h) = s
                    .split_once('x')
                    .ok_or_else(|| format!("--size wants <w>x<h>, got {s}"))?;
                [num(w, "width")?, num(h, "height")?].map(|v| v.clamp(16.0, 8192.0) as u32)
            }
            None => [1200, 800],
        };
        Ok(Self {
            pack,
            center: center.map(|c| lat_lon(c, "--center")).transpose()?,
            zoom: zoom.map(|z| num(z, "zoom")).transpose()?,
            route,
            step: None,
            alternatives: false,
            size,
            pixels_per_point: scale.map_or(Ok(1.0), |s| num(s, "scale"))?.clamp(0.25, 8.0) as f32,
            msaa: render::samples(msaa.map(|s| num(s, "msaa")).transpose()?.map(|v| v as u32)),
            style: StyleFile::builtin(),
            out,
        })
    }

    /// Renders and writes the PNG; returns a line about what was drawn.
    pub fn run(&self) -> Result<String, String> {
        let t0 = Instant::now();
        let first = packs::find(&self.pack)
            .into_iter()
            .next()
            .ok_or_else(|| format!("no pack (*.omp.000) in {}", self.pack.display()))?;
        let opened = packs::open(&first)?;
        let (style, warnings) = Style::compile(&self.style, &opened.schema);
        for w in warnings {
            eprintln!("{w}");
        }
        let ppp = f64::from(self.pixels_per_point);
        let mut cam = Camera::new(opened.bounds);
        cam.size = self.size.map(|v| f64::from(v) / ppp);
        let (route, route_msg) = self.route(&first)?;
        let routed = route
            .iter()
            .any(|(look, l)| *look == Look::Route && !l.is_empty());
        let fit = |look: Option<Look>| {
            let lines = route.iter().filter(|(l, _)| look.is_none_or(|k| k == *l));
            routing::view_for(routing::fit_bounds(lines.map(|(_, l)| l)), cam.size)
        };
        let picked = route.iter().any(|(look, _)| *look == Look::Picked);
        let fitted = routed.then(|| fit(picked.then_some(Look::Picked)));
        let center = match (self.center, fitted) {
            (Some((lat, lon)), _) => omp_core::geo::lonlat_to_unit(lon, lat),
            (None, Some((c, _))) => c,
            (None, None) => cam.center,
        };
        cam.center = center;
        cam.zoom = self.zoom.or(fitted.map(|f| f.1)).unwrap_or(cam.zoom);
        cam.clamp();
        let z = tiles::tile_zoom(cam.zoom);
        let visible = tiles::select(z, cam.visible(), opened.bounds).visible;

        let (adapter, device, queue) = render::headless_device()?;
        let pool = Pool::new(device.clone(), queue.clone());
        let failed = AtomicUsize::new(0);
        // `None`: not in the pack. Read in rounds: the visible tiles, then the ancestors of
        // those left out, until each has a stored ancestor or none.
        let mut known: HashMap<TileId, Loaded> = HashMap::new();
        let mut errors = Vec::new();
        let state = |known: &HashMap<TileId, Loaded>, t| {
            known.get(&t).map(|m| match m {
                Some(_) => Known::Loaded,
                None => Known::Absent,
            })
        };
        let caps = style.caps_below(z);
        let mut plan = tiles::plan(&visible, &caps, |t| state(&known, t));
        while !plan.need.is_empty() {
            let wanted = &plan.need;
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| {
                let workers: Vec<_> = (0..loader::default_threads())
                    .map(|_| {
                        s.spawn(|| {
                            let mut out = Vec::new();
                            while let Some(&id) = wanted.get(next.fetch_add(1, Ordering::Relaxed)) {
                                let r = opened.reader.tile(id.z, id.x, id.y).map(|t| {
                                    t.map(|t| {
                                        let m = mesh::build(&t, &style);
                                        failed.fetch_add(m.failed, Ordering::Relaxed);
                                        let l = labels::extract(id, &t, &style);
                                        (Arc::new(GpuMesh::upload(&pool, &m)), Arc::new(l))
                                    })
                                });
                                out.push((id, r));
                            }
                            out
                        })
                    })
                    .collect();
                for w in workers {
                    for (id, r) in w.join().expect("tile worker") {
                        match r {
                            Ok(m) => known.insert(id, m),
                            // Reported, and drawn from its ancestor.
                            Err(e) => {
                                errors.push(format!("{id}: {e}"));
                                known.insert(id, None)
                            }
                        };
                    }
                }
            });
            plan = tiles::plan(&visible, &caps, |t| state(&known, t));
        }
        let loaded: Vec<(TileId, Option<Arc<GpuMesh>>)> = plan
            .draws
            .iter()
            .map(|&(t, _)| (t, known.get(&t).cloned().flatten().map(|m| m.0)))
            .collect();
        let loaded_in = t0.elapsed();
        let mesh_bytes: usize = loaded
            .iter()
            .filter_map(|(_, m)| m.as_ref())
            .map(|m| m.bytes)
            .sum();
        let tiles = loaded.len();

        // wgpu's GL backend reads back a black image from a multisampled offscreen target (the
        // window is fine), so `--render` draws without MSAA there.
        let gl = adapter.get_info().backend == eframe::wgpu::Backend::Gl;
        let samples = if gl { 1 } else { self.msaa };
        let mut renderer = Renderer::new(&device, render::OFFSCREEN_FORMAT, samples);
        let paints = render::paints(
            &style,
            &Hidden::default(),
            cam.zoom,
            self.pixels_per_point,
            renderer.srgb,
        );
        let capped = plan.capped.iter().filter_map(|&(t, cap, s)| {
            let m = known.get(&s).cloned().flatten()?;
            Some((t, cap, s, m.0))
        });
        let mut frame = render::layout(
            &cam,
            self.pixels_per_point,
            paints,
            loaded,
            capped,
            &style.pass_cap,
        );
        let mut route_msg = route_msg;
        if !route.is_empty() {
            let t = Instant::now();
            let z = routing::band_for(cam.zoom);
            let mut draw = RouteDraw::default();
            let mut n = 0;
            for (look, line) in &route {
                let band = line.band(z);
                n += band.segments.len();
                if let Some(b) = GpuBand::upload(&pool, band) {
                    let ppp = self.pixels_per_point;
                    let m = routing::DRAW_MARGIN;
                    draw.lines
                        .push(LineDraw::layout(*look, Arc::new(b), &cam, ppp, m));
                }
            }
            route_msg += &format!(
                "; lines at z{z}: {n} segments, {} pieces drawn, in {:.2} ms",
                draw.pieces(),
                t.elapsed().as_secs_f64() * 1e3
            );
            frame.route = Some(draw);
        }
        let t1 = Instant::now();
        let sources: Vec<labels::Source> = plan
            .labels
            .iter()
            .filter_map(|&(tile, square, cap)| {
                let (_, l) = known.get(&tile)?.as_ref()?;
                Some(labels::Source {
                    labels: l,
                    tile,
                    square,
                    cap,
                })
            })
            .collect();
        let mut placer = Placer::default();
        let placed = placer.place(
            &cam,
            self.pixels_per_point,
            &style,
            &Hidden::default(),
            &sources,
            renderer.srgb,
            None,
        );
        let label_ms = t1.elapsed().as_secs_f64() * 1e3;
        let (n_labels, n_shields) = (placed.labels, placed.shields);
        frame.glyphs = placed.glyphs;
        frame.atlas = placer.atlas.take_uploads();
        let atlas_fill = placer.atlas.fill();
        let no_map = crate::app::NO_MAP.to_array();
        let pixels = render::draw_offscreen(&device, &queue, &mut renderer, &frame, no_map)?;
        write_png(&self.out, self.size, &pixels)
            .map_err(|e| format!("{}: {e}", self.out.display()))?;
        let info = adapter.get_info();
        let mut msg = format!(
            "{}: z{:.2} (tiles z{z}), {tiles} tiles, {:.1} MiB mesh, loaded in {:.0} ms, {n_labels} labels ({n_shields} shields) in {label_ms:.1} ms, atlas {:.0}% full, {:.0} ms in all ({} {:?}, MSAA {samples})",
            self.out.display(),
            cam.zoom,
            mesh_bytes as f64 / (1024.0 * 1024.0),
            loaded_in.as_secs_f64() * 1e3,
            atlas_fill * 100.0,
            t0.elapsed().as_secs_f64() * 1e3,
            info.name,
            info.backend,
        );
        msg += &route_msg;
        let failed = failed.into_inner();
        if failed > 0 {
            msg += &format!("\n  {failed} polygons could not be tessellated");
        }
        for e in errors {
            msg += &format!("\n  error: {e}");
        }
        Ok(msg)
    }
}

impl Snapshot {
    /// The lines of the route through `--route`'s stops, in the order drawn, and a line
    /// about it.
    fn route(&self, first: &Path) -> Result<(Vec<(Look, Line)>, String), String> {
        if self.route.is_empty() {
            return Ok((Vec::new(), String::new()));
        }
        let t = Instant::now();
        let pack = Arc::new(omp_core::pack::Pack::open(first).map_err(|e| e.to_string())?);
        let streets = omp_core::street::Streets::open(pack.clone()).map_err(|e| e.to_string())?;
        let r = omp_core::route::Routing::open(pack)
            .map_err(|e| e.to_string())?
            .ok_or("the pack has no routing")?;
        let stops: Vec<omp_core::route::Stop> = self
            .route
            .iter()
            .map(|&(lat, lon)| {
                let p = omp_core::geo::from_unit(omp_core::geo::lonlat_to_unit(lon, lat));
                omp_core::route::Stop::at(p)
            })
            .collect();
        let trip = r.trip(&stops, 0).map_err(|e| e.to_string())?;
        let line = Line::of_trip(&trip);
        let names = streets.as_ref().map(|s| s as &dyn omp_core::route::Names);
        let (routed, roads) =
            routing::Routed::with_roads(&r, streets.as_ref(), &stops, trip, line, &|| false)
                .map_err(|e| e.to_string())?
                .expect("never stopped");
        let mut grey = None;
        let mut alt_msg = String::new();
        if self.alternatives {
            let t = Instant::now();
            let mut times = [std::time::Duration::ZERO; 2];
            let quickest = routing::Quickest {
                trip: &routed.trip,
                steps: &routed.steps,
                restricted: &routed.restricted,
                roads: &roads,
            };
            let legs = routing::choices(&r, names, &stops, &quickest, &|| false, &mut times)
                .map_err(|e| e.to_string())?
                .expect("never stopped");
            for (li, c) in legs.iter().enumerate() {
                let Some(c) = c else { continue };
                for (k, x) in c.routes.iter().enumerate() {
                    alt_msg += &format!(
                        "\n  leg {} route {}{}: {:.1} km, {:.0} min, {} turns, {} U-turns, via {}",
                        li + 1,
                        k + 1,
                        match (k, k == c.best) {
                            (0, _) => " (shown)",
                            (_, true) => " (ranked best with turn costs)",
                            _ => "",
                        },
                        x.leg.metres / 1000.0,
                        x.leg.time_s / 60.0,
                        x.turns.turns,
                        x.turns.uturns,
                        if x.road.is_empty() { "-" } else { &x.road }
                    );
                }
            }
            alt_msg += &format!(
                "\n  alternatives in {:.0} ms (search {:.0} ms, directions and lines {:.0} ms)",
                t.elapsed().as_secs_f64() * 1e3,
                times[0].as_secs_f64() * 1e3,
                times[1].as_secs_f64() * 1e3
            );
            // The quickest shown, as in the app (decision 23).
            let legs: Vec<_> = legs.iter().map(Option::as_ref).collect();
            grey = Some(routing::grey_line(&legs, &vec![0; legs.len()]));
        }
        let trip = &routed.trip;
        let restricted: f64 = trip
            .legs
            .iter()
            .zip(&routed.restricted)
            .filter_map(|(l, runs)| Some((l.as_ref()?, runs)))
            .flat_map(|(l, runs)| runs.iter().flat_map(|r| &l.pieces[r.clone()]))
            .fold(0.0, |m, p| m + p.metres);
        let mut msg = format!(
            "\n  route: {:.1} km, {:.0} min, {} points, {} steps, {:.2} km restricted, in {:.0} ms",
            trip.metres() / 1000.0,
            trip.time_s() / 60.0,
            routed.line.points,
            routed.steps.len(),
            restricted / 1000.0,
            t.elapsed().as_secs_f64() * 1e3
        );
        if !trip.complete() {
            msg += " (some stops not reached)";
        }
        msg += &alt_msg;
        let points: Vec<_> = stops.iter().map(|s| s.p).collect();
        if let Some(s) = self.step.and_then(|i| routed.steps.get(i)) {
            msg += &format!("\n  step {}: {}", self.step.unwrap() + 1, s.text());
        }
        let mut lines = routing::overlay_lines(&routed, &points, self.step);
        lines.push((Look::Route, routed.line));
        lines.extend(grey.map(|g| (Look::Alternative, g)));
        lines.sort_by_key(|(look, _)| *look as usize);
        Ok((lines, msg))
    }
}

pub fn write_png(path: &Path, [w, h]: [u32; 2], rgba: &[u8]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    // The map is opaque.
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    let rgb: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let mut wr = enc.write_header().map_err(std::io::Error::other)?;
    wr.write_image_data(&rgb).map_err(std::io::Error::other)?;
    wr.finish().map_err(std::io::Error::other)
}

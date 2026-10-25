//! `omp-app --render`: draws one view offscreen to a PNG, without a window. For reviewing
//! renderer and style changes, and for the reference screenshots in `docs/screenshots/`.

use std::collections::HashMap;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::camera::Camera;
use crate::render::{self, GpuMesh, Renderer};
use crate::style::Style;
use crate::tiles::{self, Known, TileId};
use crate::{cap, loader, mesh, packs};

pub struct Snapshot {
    pub pack: PathBuf,
    pub lat: f64,
    pub lon: f64,
    pub zoom: f64,
    /// In pixels.
    pub size: [u32; 2],
    pub pixels_per_point: f32,
    pub msaa: u32,
    /// `--cap`: layers drawn from their ancestor above a zoom, as in experiment 1.5c B.
    pub cap: Option<String>,
    pub out: PathBuf,
}

impl Snapshot {
    /// From `--center <lat,lon>`, `--zoom <z>`, `--size <w>x<h>`, `--scale <ppp>` and
    /// `--msaa <samples>`.
    pub fn parse(
        pack: PathBuf,
        center: &str,
        zoom: &str,
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
        let (lat, lon) = center
            .split_once(',')
            .ok_or_else(|| format!("--center wants <lat,lon>, got {center}"))?;
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
            lat: num(lat, "latitude")?,
            lon: num(lon, "longitude")?,
            zoom: num(zoom, "zoom")?,
            size,
            pixels_per_point: scale.map_or(Ok(1.0), |s| num(s, "scale"))?.clamp(0.25, 8.0) as f32,
            msaa: render::samples(msaa.map(|s| num(s, "msaa")).transpose()?.map(|v| v as u32)),
            cap: None,
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
        let style = Style::compile(&opened.schema);
        let caps = match &self.cap {
            Some(spec) => cap::parse(spec, &opened.schema)?,
            None => Vec::new(),
        };
        let ppp = f64::from(self.pixels_per_point);
        let mut cam = Camera::new(opened.bounds);
        cam.size = self.size.map(|v| f64::from(v) / ppp);
        cam.set_lonlat(self.lon, self.lat, self.zoom);
        let z = tiles::tile_zoom(cam.zoom);
        let visible = tiles::select(z, cam.visible(), opened.bounds).visible;

        let (adapter, device, queue) = render::headless_device()?;
        let failed = AtomicUsize::new(0);
        // `None`: not in the pack. Read in rounds: the visible tiles, then the ancestors of
        // those left out, until each has a stored ancestor or none.
        let mut known: HashMap<TileId, Option<Arc<GpuMesh>>> = HashMap::new();
        let mut errors = Vec::new();
        let state = |known: &HashMap<TileId, Option<Arc<GpuMesh>>>, t| {
            known.get(&t).map(|m| match m {
                Some(_) => Known::Loaded,
                None => Known::Absent,
            })
        };
        let mut plan = tiles::plan(&visible, |t| state(&known, t));
        while !plan.need.is_empty() {
            let wanted = &plan.need;
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| {
                let workers: Vec<_> = (0..loader::default_threads())
                    .map(|_| {
                        s.spawn(|| {
                            let mut out = Vec::new();
                            while let Some(&id) = wanted.get(next.fetch_add(1, Ordering::Relaxed)) {
                                let r = opened.reader.tile(id.z, id.x, id.y).and_then(|t| {
                                    t.map(|t| cap::apply(&opened.reader, id, t, &caps))
                                        .transpose()
                                });
                                let r = r.map(|t| {
                                    t.map(|t| {
                                        let m = mesh::build(&t, &style);
                                        failed.fetch_add(m.failed, Ordering::Relaxed);
                                        Arc::new(GpuMesh::upload(&device, &m))
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
            plan = tiles::plan(&visible, |t| state(&known, t));
        }
        let loaded: Vec<(TileId, Option<Arc<GpuMesh>>)> = plan
            .draws
            .iter()
            .map(|&(t, _)| (t, known.get(&t).cloned().flatten()))
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
        let paints = render::paints(&style, cam.zoom, self.pixels_per_point, renderer.srgb);
        let frame = render::layout(&cam, self.pixels_per_point, paints, loaded);
        let no_map = crate::app::NO_MAP.to_array();
        let pixels = render::draw_offscreen(&device, &queue, &mut renderer, &frame, no_map)?;
        write_png(&self.out, self.size, &pixels)
            .map_err(|e| format!("{}: {e}", self.out.display()))?;
        let info = adapter.get_info();
        let mut msg = format!(
            "{}: z{:.2} (tiles z{z}), {tiles} tiles, {:.1} MiB mesh, loaded in {:.0} ms, {:.0} ms in all ({} {:?}, MSAA {samples})",
            self.out.display(),
            cam.zoom,
            mesh_bytes as f64 / (1024.0 * 1024.0),
            loaded_in.as_secs_f64() * 1e3,
            t0.elapsed().as_secs_f64() * 1e3,
            info.name,
            info.backend,
        );
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

fn write_png(path: &Path, [w, h]: [u32; 2], rgba: &[u8]) -> std::io::Result<()> {
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

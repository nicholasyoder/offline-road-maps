//! `omp-builder size-experiments`: phase 1 step 1.5c. Reads a pack and works out, without
//! rebuilding it, what its tiles and `TIDX` would weigh with other frame layouts (A) and
//! with water or land cover capped at a lower zoom (B). See `docs/phase1-plan.md`.
//!
//! - F1 and F2 drop the 4-byte magic number and the 4-byte checksum of every frame, which
//!   changes nothing else in the frame, so their sizes are exact.
//! - F3 stores a payload raw when it is at most a threshold, with a flag bit in its `TIDX`
//!   length.
//! - F4 compresses the whole tile as one frame, with one dictionary per zoom band trained on
//!   whole tiles.
//! - Caps drop land cover above z13 or z12, or the water areas above z13 (water lines stay),
//!   and leave out tiles that are left with nothing.
//!
//! `TIDX` is rebuilt for every variant with contiguous offsets, the baseline too, so the
//! variants compare exactly (the real index also starts a block after chunk padding).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use clap::Args;
use omp_core::pack::Pack;
use omp_core::schema::{self, Schema};
use omp_core::tile::{
    self, Dictionaries, Enc, GeomType, IndexBuilder, Layer, MAX_ZOOM, TileLoc, zoom_band,
};
use omp_core::tiles::{FrameCompressor, LEVEL, TDIC, TileReader};
use omp_core::varint;
use rayon::prelude::*;
use zstd::zstd_safe::{DCtx, DDict, DParameter, FrameFormat};

use super::first_chunk;

#[derive(Args)]
pub struct ExperimentArgs {
    /// Pack directory or its `.omp.000` file.
    pack: PathBuf,

    /// Time the decode of every n-th tile under F1+F2 and F3 against the pack's frames
    /// (0: no timing).
    #[arg(long, default_value_t = 64)]
    time_every: usize,

    /// Skip F4, which recompresses every tile.
    #[arg(long)]
    no_f4: bool,
}

const MAGIC: [u8; 4] = 0xFD2F_B528u32.to_le_bytes();
const F3_THRESHOLDS: [u32; 6] = [8, 16, 24, 32, 48, 64];
/// Tiles per parallel batch.
const BATCH: usize = 16_384;
/// Whole-tile training input per band for F4.
const TRAIN_BYTES: usize = 16 << 20;

/// One layer frame as stored.
struct Frame {
    layer: u8,
    /// Compressed (stored) and decompressed length.
    c: u32,
    r: u32,
}

/// One tile's layers under each variant: `None` when the variant leaves it out.
struct TileOut {
    key: u64,
    variants: Vec<Option<Vec<(u8, u32)>>>,
}

struct Variant {
    name: String,
    index: IndexBuilder,
    offset: u64,
    tdat: u64,
    tiles: u64,
    frames: u64,
}

impl Variant {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            index: IndexBuilder::new(),
            offset: 0,
            tdat: 0,
            tiles: 0,
            frames: 0,
        }
    }
}

/// Which layers a cap variant drops above which zoom.
#[derive(Clone, Copy)]
struct Cap {
    landcover: Option<u8>,
    water_areas: Option<u8>,
}

const CAPS: [(&str, Cap); 4] = [
    (
        "B landcover to z13",
        Cap {
            landcover: Some(13),
            water_areas: None,
        },
    ),
    (
        "B landcover to z12",
        Cap {
            landcover: Some(12),
            water_areas: None,
        },
    ),
    (
        "B water areas to z13",
        Cap {
            landcover: None,
            water_areas: Some(13),
        },
    ),
    (
        "B landcover + water areas to z13",
        Cap {
            landcover: Some(13),
            water_areas: Some(13),
        },
    ),
];

/// Variant names, in the order of [`TileOut::variants`].
fn variant_names(f4: bool) -> Vec<String> {
    let mut v: Vec<String> = [
        "baseline (1.5b)",
        "F1 magicless",
        "F2 no checksum",
        "F1+F2",
        "F1+F2, one checksum per tile",
    ]
    .map(String::from)
    .to_vec();
    v.extend(F3_THRESHOLDS.map(|t| format!("F1+F2+F3 raw <= {t} B")));
    v.push("F1+F2+F3 raw when smaller".into());
    if f4 {
        v.push("F4 one frame per tile".into());
        v.push("F4+F1+F2".into());
    }
    v.extend(CAPS.map(|(n, _)| n.to_owned()));
    v
}

/// Header facts of a stored frame: whether it carries a checksum.
fn has_checksum(frame: &[u8]) -> Result<bool> {
    ensure!(frame.len() > 5 && frame[..4] == MAGIC, "not a zstd frame");
    Ok(frame[4] & 0x04 != 0)
}

/// The same frame without its magic number and checksum (F1+F2).
fn strip(frame: &[u8]) -> Vec<u8> {
    let mut f = frame[4..frame.len() - 4].to_vec();
    f[0] &= !0x04;
    f
}

/// `TIDX` lengths with F3's raw flag in the low bit.
fn flagged(frames: &[Frame], t: u32) -> Vec<(u8, u32)> {
    frames
        .iter()
        .map(|f| match f.r <= t {
            true => (f.layer, f.r << 1 | 1),
            false => (f.layer, (f.c - 8) << 1),
        })
        .collect()
}

struct Ctx<'a> {
    pack: &'a Pack,
    dicts: HashMap<(u8, u8), DDict<'static>>,
    raw_dicts: &'a Dictionaries,
    tile_dicts: Option<Dictionaries>,
    landcover: Option<u8>,
    water: Option<u8>,
    f4: bool,
}

impl Ctx<'_> {
    fn decompress(&self, dctx: &mut DCtx, layer: u8, z: u8, frame: &[u8]) -> Result<Vec<u8>> {
        let size = zstd::zstd_safe::get_frame_content_size(frame)
            .ok()
            .flatten()
            .context("frame without a content size")?;
        let mut out = Vec::with_capacity(size as usize);
        let res = match self.dicts.get(&(layer, zoom_band(z))) {
            Some(d) => dctx.decompress_using_ddict(&mut out, frame, d),
            None => dctx.decompress(&mut out, frame),
        };
        res.map_err(zstd_err)?;
        Ok(out)
    }
}

/// The water layer with only its lines, re-encoded; `None` if it has none.
fn water_lines(raw: Vec<u8>) -> Result<Option<Vec<u8>>> {
    let layer = Layer::decode(raw)?;
    let mut props = Vec::new();
    let mut geoms = Vec::new();
    for f in layer.features.iter().filter(|f| f.kind == GeomType::Lines) {
        let p: Vec<(u8, Box<str>)> = layer
            .props(f)
            .iter()
            .map(|&(k, _)| (k, layer.prop(f, k).unwrap_or_default().into()))
            .collect();
        props.push((f.class, p));
        geoms.push(layer.geom(f));
    }
    if geoms.is_empty() {
        return Ok(None);
    }
    let enc: Vec<Enc> = props
        .iter()
        .zip(geoms)
        .map(|((class, p), geom)| Enc {
            class: *class,
            props: p,
            geom,
        })
        .collect();
    Ok(Some(tile::encode_layer(&enc)))
}

/// F4's payload: the layer count, each layer's ID and length, then the payloads.
fn whole_tile(raws: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    varint::put(&mut out, raws.len() as u64);
    for (l, r) in raws {
        out.push(*l);
        varint::put(&mut out, r.len() as u64);
    }
    for (_, r) in raws {
        out.extend_from_slice(r);
    }
    out
}

struct Scratch {
    dctx: DCtx<'static>,
    fc: FrameCompressor,
    tile_fc: Option<FrameCompressor>,
}

fn tile_out(
    cx: &Ctx,
    s: &mut Scratch,
    z: u8,
    key: u64,
    loc: &TileLoc,
    nvariants: usize,
) -> Result<TileOut> {
    let bytes = cx.pack.read(loc.offset, loc.len() as usize)?;
    let mut frames = Vec::with_capacity(loc.layers.len());
    let mut at = 0;
    let mut slices = Vec::with_capacity(loc.layers.len());
    for &(layer, n) in &loc.layers {
        let f = &bytes[at..at + n as usize];
        at += n as usize;
        ensure!(has_checksum(f)?, "tile {z}/{key}: frame without a checksum");
        let r = zstd::zstd_safe::get_frame_content_size(f)
            .ok()
            .flatten()
            .context("frame without a content size")? as u32;
        frames.push(Frame { layer, c: n, r });
        slices.push(f);
    }
    let plain = |d: u32| -> Vec<(u8, u32)> { frames.iter().map(|f| (f.layer, f.c - d)).collect() };
    let mut v: Vec<Option<Vec<(u8, u32)>>> = Vec::with_capacity(nvariants);
    v.push(Some(plain(0)));
    v.push(Some(plain(4)));
    v.push(Some(plain(4)));
    v.push(Some(plain(8)));
    // The tile checksum is added to the record in `fold`.
    v.push(Some(plain(8)));
    for t in F3_THRESHOLDS {
        v.push(Some(flagged(&frames, t)));
    }
    v.push(Some(
        frames
            .iter()
            .map(|f| match f.r <= f.c - 8 {
                true => (f.layer, f.r << 1 | 1),
                false => (f.layer, (f.c - 8) << 1),
            })
            .collect(),
    ));

    // Decompress only what F4 and the water cap need.
    let need_all = cx.f4;
    let mut raws: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut water_raw = None;
    for (f, slice) in frames.iter().zip(&slices) {
        let want_water = Some(f.layer) == cx.water && z > 13;
        if need_all || want_water {
            let raw = cx.decompress(&mut s.dctx, f.layer, z, slice)?;
            if want_water {
                water_raw = Some(raw.clone());
            }
            raws.push((f.layer, raw));
        }
    }
    if cx.f4 {
        let payload = whole_tile(&raws);
        let tc = s.tile_fc.as_mut().unwrap();
        let n = tc.compress(0, z, &payload)?.len() as u32;
        let first = frames[0].layer;
        v.push(Some(vec![(first, n)]));
        v.push(Some(vec![(first, n - 8)]));
    }

    // The water layer re-encoded with only its lines, for the water cap at z14.
    let water_lines_len = match water_raw {
        Some(raw) => match water_lines(raw)? {
            Some(p) => Some(s.fc.compress(cx.water.unwrap(), z, &p)?.len() as u32),
            None => None,
        },
        None => None,
    };
    for (_, cap) in CAPS {
        let mut out = Vec::new();
        for f in &frames {
            if Some(f.layer) == cx.landcover && cap.landcover.is_some_and(|m| z > m) {
                continue;
            }
            if Some(f.layer) == cx.water && cap.water_areas.is_some_and(|m| z > m) {
                if let Some(n) = water_lines_len {
                    out.push((f.layer, n));
                }
                continue;
            }
            out.push((f.layer, f.c));
        }
        v.push((!out.is_empty()).then_some(out));
    }
    debug_assert_eq!(v.len(), nvariants);
    Ok(TileOut { key, variants: v })
}

/// Trains F4's whole-tile dictionaries, one per zoom band, from tiles sampled evenly.
fn train_tile_dicts(cx: &Ctx, reader: &TileReader) -> Result<Dictionaries> {
    let mut by_band: HashMap<u8, Vec<(u8, u64, TileLoc)>> = HashMap::new();
    let mut bytes: HashMap<u8, u64> = HashMap::new();
    for z in 0..=MAX_ZOOM {
        reader.for_each_tile(z, |key, loc| {
            by_band
                .entry(zoom_band(z))
                .or_default()
                .push((z, key, loc.clone()));
        })?;
    }
    // Stored bytes are about a quarter of raw; sample on them.
    for (b, tiles) in &by_band {
        bytes.insert(*b, tiles.iter().map(|t| t.2.len()).sum::<u64>() * 4);
    }
    let mut out = Dictionaries::default();
    for (b, tiles) in by_band {
        let step = (bytes[&b] as usize / TRAIN_BYTES).max(1);
        let picked: Vec<&(u8, u64, TileLoc)> = tiles.iter().step_by(step).collect();
        let samples: Vec<Vec<u8>> = picked
            .par_iter()
            .map_init(DCtx::create, |dctx, (z, _, loc)| -> Result<Vec<u8>> {
                let bytes = cx.pack.read(loc.offset, loc.len() as usize)?;
                let mut at = 0;
                let mut raws = Vec::new();
                for &(l, n) in &loc.layers {
                    let f = &bytes[at..at + n as usize];
                    at += n as usize;
                    raws.push((l, cx.decompress(dctx, l, *z, f)?));
                }
                Ok(whole_tile(&raws))
            })
            .collect::<Result<_>>()?;
        let total: usize = samples.iter().map(Vec::len).sum();
        let dict = if total >= 128 << 10 && samples.len() >= 64 {
            zstd::dict::from_samples(&samples, 64 << 10).unwrap_or_default()
        } else {
            Vec::new()
        };
        eprintln!(
            "  F4 dictionary band {b}: {} samples, {:.1} MiB, dict {} B",
            samples.len(),
            mib(total as u64),
            dict.len()
        );
        out.0.insert((0, b), dict);
    }
    Ok(out)
}

fn zstd_err(code: usize) -> anyhow::Error {
    anyhow::anyhow!("zstd: {}", zstd::zstd_safe::get_error_name(code))
}

fn mib(b: u64) -> f64 {
    b as f64 / (1u64 << 20) as f64
}

pub fn run(args: ExperimentArgs) -> Result<()> {
    let start = Instant::now();
    let first = first_chunk(&args.pack)?;
    let pack = Pack::open(&first).with_context(|| format!("open {}", first.display()))?;
    let schema = Schema::decode(&pack.read_section(pack.section(schema::ID).context("no SCHM")?)?)?;
    let raw_dicts =
        Dictionaries::decode(&pack.read_section(pack.section(TDIC).context("no TDIC")?)?)?;
    let reader = TileReader::open(Pack::open(&first)?)?;
    let mut dicts = HashMap::new();
    for (k, d) in &raw_dicts.0 {
        if !d.is_empty() {
            dicts.insert(*k, DDict::try_create(d).context("bad dictionary")?);
        }
    }
    let mut cx = Ctx {
        pack: &pack,
        dicts,
        raw_dicts: &raw_dicts,
        tile_dicts: None,
        landcover: schema.layer("landcover").map(|l| l.0),
        water: schema.layer("water").map(|l| l.0),
        f4: !args.no_f4,
    };
    if cx.f4 {
        cx.tile_dicts = Some(train_tile_dicts(&cx, &reader)?);
        eprintln!("  trained in {:.0} s", start.elapsed().as_secs_f64());
    }
    let names = variant_names(cx.f4);
    let mut vars: Vec<Variant> = names.iter().map(Variant::new).collect();
    let tile_checksum = names
        .iter()
        .position(|n| n == "F1+F2, one checksum per tile")
        .unwrap();
    // Frames by stored size, for the size histogram.
    let mut hist = [0u64; 8];
    let mut sample = Vec::new();
    let pool: std::sync::Mutex<Vec<Scratch>> = std::sync::Mutex::new(Vec::new());
    let new_scratch = || -> Result<Scratch> {
        Ok(Scratch {
            dctx: DCtx::create(),
            fc: FrameCompressor::new(cx.raw_dicts, LEVEL)?,
            tile_fc: match &cx.tile_dicts {
                Some(d) => Some(FrameCompressor::new(d, LEVEL)?),
                None => None,
            },
        })
    };
    for z in 0..=MAX_ZOOM {
        let mut tiles: Vec<(u64, TileLoc)> = Vec::new();
        reader.for_each_tile(z, |key, loc| tiles.push((key, loc.clone())))?;
        for (i, (_, loc)) in tiles.iter().enumerate() {
            for &(_, n) in &loc.layers {
                let b = match n {
                    0..16 => 0,
                    16..24 => 1,
                    24..32 => 2,
                    32..40 => 3,
                    40..64 => 4,
                    64..256 => 5,
                    256..4096 => 6,
                    _ => 7,
                };
                hist[b] += 1;
            }
            if args.time_every > 0 && i % args.time_every == 0 {
                sample.push((z, loc.clone()));
            }
        }
        for batch in tiles.chunks(BATCH) {
            let outs: Vec<TileOut> = batch
                .par_iter()
                .map(|(key, loc)| -> Result<TileOut> {
                    let mut s = match pool.lock().unwrap().pop() {
                        Some(s) => s,
                        None => new_scratch()?,
                    };
                    let out = tile_out(&cx, &mut s, z, *key, loc, names.len());
                    pool.lock().unwrap().push(s);
                    out
                })
                .collect::<Result<_>>()?;
            for out in outs {
                for (i, (v, lens)) in vars.iter_mut().zip(&out.variants).enumerate() {
                    let Some(lens) = lens else { continue };
                    // F3's flag bit is not part of the stored length; the index sums
                    // the flagged values, so the synthetic offsets follow those.
                    let flag = names[i].contains("F3");
                    let index_len: u64 = lens.iter().map(|&(_, n)| u64::from(n)).sum();
                    let mut len: u64 = lens
                        .iter()
                        .map(|&(_, n)| u64::from(if flag { n >> 1 } else { n }))
                        .sum();
                    if i == tile_checksum {
                        len += 4;
                    }
                    v.index.push(z, out.key, v.offset, lens)?;
                    v.offset += index_len;
                    v.tdat += len;
                    v.tiles += 1;
                    v.frames += lens.len() as u64;
                }
            }
        }
        eprintln!(
            "  z{z}: {} tiles, {:.0} s",
            tiles.len(),
            start.elapsed().as_secs_f64()
        );
    }

    println!("\nframes by stored size");
    let labels = [
        "< 16", "16-23", "24-31", "32-39", "40-63", "64-255", "256-4095", ">= 4096",
    ];
    let all: u64 = hist.iter().sum();
    for (l, n) in labels.iter().zip(hist) {
        println!("  {l:>9} B {n:>11} {:>6.1}%", 100.0 * n as f64 / all as f64);
    }

    println!(
        "\n{:<36} {:>11} {:>11} {:>10} {:>11} {:>10} {:>9}",
        "variant", "tiles", "frames", "TDAT MiB", "TIDX MiB", "sum MiB", "saved"
    );
    let mut base_sum = 0;
    for (i, v) in vars.into_iter().enumerate() {
        let tidx = v.index.finish()?.len() as u64;
        let sum = v.tdat + tidx;
        if i == 0 {
            base_sum = sum;
        }
        println!(
            "{:<36} {:>11} {:>11} {:>10.1} {:>11.2} {:>10.1} {:>9.1}",
            v.name,
            v.tiles,
            v.frames,
            mib(v.tdat),
            mib(tidx),
            mib(sum),
            mib(base_sum) - mib(sum)
        );
    }

    if !sample.is_empty() {
        time_decode(&cx, &sample)?;
    }
    eprintln!("done in {:.0} s", start.elapsed().as_secs_f64());
    Ok(())
}

/// Decode time per tile (decompress and decode every layer), single-threaded, best of 3:
/// the pack's frames, F1+F2, and F1+F2 with F3 at 32 bytes.
fn time_decode(cx: &Ctx, sample: &[(u8, TileLoc)]) -> Result<()> {
    let mut plain = DCtx::create();
    let mut magicless = DCtx::create();
    magicless
        .set_parameter(DParameter::Format(FrameFormat::Magicless))
        .map_err(zstd_err)?;
    let mut times: [Vec<f64>; 3] = Default::default();
    for (z, loc) in sample {
        let bytes = cx.pack.read(loc.offset, loc.len() as usize)?;
        let mut frames = Vec::new();
        let mut at = 0;
        for &(l, n) in &loc.layers {
            let f = &bytes[at..at + n as usize];
            at += n as usize;
            let r = zstd::zstd_safe::get_frame_content_size(f)
                .ok()
                .flatten()
                .unwrap_or(0) as usize;
            frames.push((l, f.to_vec(), strip(f), r));
        }
        // Raw payloads for F3, made outside the timing.
        let raws: Vec<Option<Vec<u8>>> = frames
            .iter()
            .map(|(l, f, _, r)| -> Result<_> {
                (*r <= 32)
                    .then(|| cx.decompress(&mut plain, *l, *z, f))
                    .transpose()
            })
            .collect::<Result<_>>()?;
        for (v, t) in times.iter_mut().enumerate() {
            let mut best = f64::MAX;
            for _ in 0..3 {
                let t0 = Instant::now();
                for ((l, f, stripped, r), raw) in frames.iter().zip(&raws) {
                    let dict = cx.dicts.get(&(*l, zoom_band(*z)));
                    let payload = match (v, raw) {
                        (0, _) => cx.decompress(&mut plain, *l, *z, f)?,
                        (2, Some(raw)) => raw.clone(),
                        _ => {
                            let mut out = Vec::with_capacity(*r);
                            let res = match dict {
                                Some(d) => magicless.decompress_using_ddict(&mut out, stripped, d),
                                None => magicless.decompress(&mut out, stripped),
                            };
                            res.map_err(zstd_err)?;
                            out
                        }
                    };
                    std::hint::black_box(Layer::decode(payload)?);
                }
                best = best.min(t0.elapsed().as_secs_f64() * 1e3);
            }
            t.push(best);
        }
    }
    println!(
        "\ndecode per tile, {} tiles sampled, ms: p50 / p95 / p99 / mean",
        sample.len()
    );
    for (name, t) in ["baseline", "F1+F2", "F1+F2+F3 <= 32 B"]
        .iter()
        .zip(&mut times)
    {
        t.sort_by(f64::total_cmp);
        let q = |p: f64| t[((t.len() - 1) as f64 * p) as usize];
        let mean = t.iter().sum::<f64>() / t.len() as f64;
        println!(
            "  {name:<18} {:.4} / {:.4} / {:.4} / {:.4}",
            q(0.5),
            q(0.95),
            q(0.99),
            mean
        );
    }
    Ok(())
}

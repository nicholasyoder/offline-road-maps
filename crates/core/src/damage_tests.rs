//! Damaged sections give errors, never panics (phase 5 audit, R2). Each section of a small
//! built pack is damaged, then every reader the app uses is run over it.
//!
//! Two kinds of damage: bytes flipped where the section lies in the file, as a bad stick
//! does; and bytes flipped inside a decompressed block that is compressed again and put
//! back, which zstd's checksum would otherwise stop before any decoder saw it (a builder
//! bug, or damage the checksum misses). Unoptimized, so a sum that overflows panics here,
//! where release builds would wrap.
//!
//! `OMP_TEST_PACK=<first chunk> cargo test -p omp-core -- --ignored --nocapture damaged`,
//! with a pack built with addresses (`--addresses US-DE` for Delaware: 16 MiB, about 15
//! minutes). `OMP_DAMAGE_TRIALS` sets the trials per section and kind (default 10), and
//! `OMP_DAMAGE_SECTIONS` limits them to some sections (`RCHE,SENT`).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use xxhash_rust::xxh3::xxh3_64;
use zstd::zstd_safe::DCtx;

use crate::geo::P;
use crate::pack::{self, PAGE, Pack};
use crate::route::{AVOID, AltParams, Ends, Routing, Stop};
use crate::schedule::{self, Breaks, Day, Local, Settings, Stay, When};
use crate::search::Search;
use crate::tiles::TileReader;
use crate::tz::Zones;
use crate::zframe;
use crate::{meta, schema};

/// xorshift64*, so a failing trial can be run again from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// What the readers are asked, chosen on the intact pack.
#[derive(Default)]
struct Plan {
    tiles: Vec<(u8, u32, u32)>,
    points: Vec<P>,
    queries: Vec<String>,
    stops: Vec<P>,
}

fn plan(first: &Path) -> Plan {
    let mut plan = Plan::default();
    let tiles = TileReader::open(Pack::open(first).unwrap()).unwrap();
    for z in 0..=crate::tile::MAX_ZOOM {
        tiles
            .for_each_tile(z, |key, _| {
                let (x, y) = crate::tile::hilbert_xy(z, key);
                plan.tiles.push((z, x, y));
            })
            .unwrap();
    }
    let pack = Arc::new(Pack::open(first).unwrap());
    let s = Search::open(pack.clone())
        .unwrap()
        .expect("a pack with search");
    let n = s.streets.count();
    s.streets
        .for_each(|id, st, g| {
            if id % (n / 40).max(1) == 7 {
                plan.points.push(g.point_at(g.length_m() / 2.0));
                plan.queries.push(st.name.clone());
            }
        })
        .unwrap();
    if let Some(a) = &s.addresses {
        let mut i = 0;
        a.for_each(|a| {
            if i % 20_000 == 3 {
                let name = s.streets.street(a.street).unwrap().name;
                plan.queries.push(format!("{} {name}", a.number));
                plan.points.push(s.address_point(a).unwrap());
            }
            i += 1;
        })
        .unwrap();
    }
    for id in (0..s.index.count()).step_by((s.index.count() as usize / 30).max(1)) {
        plan.queries.push(s.index.entry(id).unwrap().name);
    }
    plan.queries.retain(|q| !q.is_empty());
    let r = Routing::open(pack).unwrap().expect("a pack with routing");
    let n = r.header.segments;
    for k in 0..5 {
        let p = r.segment(n / 5 * k + n / 11).unwrap().points[0];
        plan.stops.push(p);
    }
    plan
}

/// Which readers read a section.
#[derive(Clone, Copy)]
struct Groups {
    tiles: bool,
    search: bool,
    routing: bool,
}

fn groups(id: [u8; 4]) -> Groups {
    let tiles = matches!(&id, b"TDIC" | b"TDAT" | b"TIDX" | b"SCHM");
    // Directions name streets, and the towns along a route come from the search index.
    let routing = id[0] == b'R' || matches!(&id, b"SSTR" | b"SENT" | b"SCHM" | b"TZON");
    let search = id[0] == b'S' || id[0] == b'A';
    // `META` alone is read by every trial.
    Groups {
        tiles,
        search,
        routing,
    }
}

/// Runs the readers of `g` over the pack; errors are what damage should give.
fn exercise(first: &Path, plan: &Plan, g: Groups) -> usize {
    let mut errors = 0;
    let mut count = |r: io::Result<()>| errors += usize::from(r.is_err());
    count(read_meta(first));
    if g.tiles {
        count(read_tiles(first, plan));
    }
    if g.search {
        count(read_search(first, plan));
    }
    if g.routing {
        count(read_routing(first, plan));
    }
    errors
}

fn read_meta(first: &Path) -> io::Result<()> {
    let pack = Pack::open(first)?;
    if let Some(s) = pack.section(meta::ID) {
        meta::Meta::decode(&pack.read_section(s)?)?;
    }
    if let Some(s) = pack.section(schema::ID) {
        schema::Schema::decode(&pack.read_section(s)?)?;
    }
    Ok(())
}

fn read_tiles(first: &Path, plan: &Plan) -> io::Result<()> {
    let tiles = TileReader::open(Pack::open(first)?)?;
    for z in 0..=crate::tile::MAX_ZOOM {
        tiles.tile_count(z)?;
        tiles.for_each_tile(z, |_, _| {})?;
    }
    let mut last = Ok(());
    for &(z, x, y) in &plan.tiles {
        let t = match tiles.tile(z, x, y) {
            Ok(Some(t)) => t,
            Ok(None) => continue,
            Err(e) => {
                last = Err(e);
                continue;
            }
        };
        for (_, l) in &t.layers {
            for f in &l.features {
                for (k, _) in l.props(f) {
                    l.prop(f, *k);
                }
                for part in l.parts(f) {
                    std::hint::black_box(part.len());
                }
                l.geom(f);
            }
        }
    }
    last
}

fn read_search(first: &Path, plan: &Plan) -> io::Result<()> {
    let pack = Arc::new(Pack::open(first)?);
    let Some(s) = Search::open(pack.clone())? else {
        return Ok(());
    };
    // Whole sections first, then the lookups; each part goes on after another's error.
    let mut last = Ok(());
    let mut keep = |r: io::Result<()>| {
        if r.is_err() {
            last = r;
        }
    };
    keep(s.streets.for_each(|_, _, g| {
        g.length_m();
    }));
    keep(s.grid.for_each(|_, _| {}));
    if let Some(a) = &s.addresses {
        keep(a.for_each(|_| {}));
        keep(a.for_each_loose(|_| {}));
        keep(a.for_each_range(|_| {}));
        for i in 0..a.counts()[3] {
            keep(a.postcode(i).map(drop));
        }
    }
    for id in 0..s.index.count() {
        keep(s.index.entry(id).map(drop));
    }
    {
        use fst::Streamer;
        let mut tokens = s.index.tokens().stream();
        while let Some((k, _)) = tokens.next() {
            keep(s.index.postings(&String::from_utf8_lossy(k)).map(drop));
        }
    }
    for a in 0..s.admin.areas().len() as u32 {
        s.admin.parents(a);
        keep(s.admin.polygon(a).map(drop));
    }
    let places: Vec<String> = ["city", "town", "village"].map(String::from).into();
    for &p in &plan.points {
        keep(s.whats_here(p, 100.0, 300.0).map(drop));
        keep(s.admin.chain(p).map(drop));
        keep(s.index.town_near(p, &places).map(drop));
        keep(s.streets_near(p, 200.0).map(drop));
    }
    for q in &plan.queries {
        keep(s.find(&crate::find::Request::new(q)).map(drop));
        let mut typed = crate::find::Request::new(q);
        typed.prefix = true;
        typed.near = plan.points.first().copied();
        typed.zoom = 12.0;
        keep(s.find(&typed).map(drop));
    }
    last
}

fn read_routing(first: &Path, plan: &Plan) -> io::Result<()> {
    let pack = Arc::new(Pack::open(first)?);
    let Some(r) = Routing::open(pack.clone())? else {
        return Ok(());
    };
    let search = Search::open(pack.clone()).ok().flatten();
    let zones = Zones::open(&pack).ok().flatten();
    let mut last = Ok(());
    let mut keep = |res: io::Result<()>| {
        if res.is_err() {
            last = res;
        }
    };
    keep(r.warm_top().map(drop));
    keep(r.for_each_segment(|_, _| {}));
    keep(r.for_each_cell(|_, _| {}));
    keep(r.for_each_copy(|_, _| {}));
    for l in 0..r.header.labels {
        keep(r.label(l).map(drop));
    }
    for v in 0..r.header.nodes {
        keep(r.node(v).map(drop));
        keep(r.junction(v).map(drop));
        keep(r.copies(v).map(drop));
    }
    let stops: Vec<Stop> = plan.stops.iter().map(|&p| Stop::at(p)).collect();
    let names = search
        .as_ref()
        .map(|s| &s.streets as &dyn crate::route::Names);
    for avoid in [0, AVOID] {
        let trip = match r.trip(&stops, avoid) {
            Ok(t) => t,
            Err(e) => {
                keep(Err(e));
                continue;
            }
        };
        trip.time_s();
        trip.metres();
        for leg in trip.legs.iter().flatten() {
            leg.turns();
            keep(r.roads(leg, names).map(drop));
        }
        let steps = r.directions(&trip, &stops, names);
        if let Ok(steps) = &steps {
            for s in steps {
                s.text();
            }
        }
        keep(steps.map(drop));
        let settings = Settings {
            when: When::Depart(jiff::Timestamp::from_second(1_800_000_000).unwrap()),
            stays: vec![Stay::default(); stops.len()],
            day: Some(Day {
                start: jiff::civil::time(8, 0, 0, 0),
                end: Some(jiff::civil::time(18, 0, 0, 0)),
                max_drive_s: Some(8.0 * 3600.0),
            }),
            breaks: Some(Breaks {
                every_s: 2.0 * 3600.0,
                for_s: 900.0,
            }),
        };
        let local = Local {
            zones: zones.as_ref(),
            fallback: jiff::tz::TimeZone::UTC,
        };
        schedule::schedule(&trip, &plan.stops, &settings, &local);
    }
    for w in stops.windows(2).take(2) {
        let (Ok(Some(a)), Ok(Some(b))) = (r.snap(&w[0]), r.snap(&w[1])) else {
            continue;
        };
        match r.leg_avoiding(&a, &b, 0) {
            Ok(Some(leg)) => keep(
                r.alternatives_until(&a, &b, &leg, 0, &AltParams::default(), &|| false)
                    .map(drop),
            ),
            res => keep(res.map(drop)),
        }
    }
    keep(
        r.optimize_until(&stops[..4], 0, Ends::default(), &|| false)
            .map(drop),
    );
    if let Some(z) = &zones {
        for &p in &plan.points {
            z.at(p);
            z.tz_at(p);
        }
    }
    last
}

/// The pack's file, put back as it was after each trial.
struct Subject {
    file: File,
    bytes: Vec<u8>,
    table: (u64, usize),
}

impl Subject {
    fn new(first: &Path) -> Self {
        let bytes = std::fs::read(first).unwrap();
        let p = Pack::open(first).unwrap();
        assert_eq!(p.chunks(), 1, "the test takes a pack of one chunk");
        let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let table = (PAGE + u64_at(48), u64_at(56) as usize);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(first)
            .unwrap();
        Self { file, bytes, table }
    }

    fn section(&self, s: &pack::Section) -> &[u8] {
        let at = (PAGE + s.offset) as usize;
        &self.bytes[at..at + s.len as usize]
    }

    fn write(&self, at: u64, b: &[u8]) {
        pack::write_at(&self.file, b, at).unwrap();
    }

    fn restore(&self, at: u64, len: usize) {
        self.write(at, &self.bytes[at as usize..at as usize + len]);
    }

    /// Puts `data` after the end of the file as section `id`, in the table and header.
    fn relocate(&self, id: [u8; 4], data: &[u8]) {
        let v = (self.bytes.len() as u64 - PAGE).div_ceil(PAGE) * PAGE;
        self.write(PAGE + v, data);
        let (t_at, t_len) = self.table;
        let mut table = self.bytes[t_at as usize..t_at as usize + t_len].to_vec();
        let e = table
            .chunks_mut(32)
            .find(|e| e[0..4] == id)
            .expect("the section");
        e[8..16].copy_from_slice(&v.to_le_bytes());
        e[16..24].copy_from_slice(&(data.len() as u64).to_le_bytes());
        e[24..32].copy_from_slice(&xxh3_64(data).to_le_bytes());
        self.write(t_at, &table);
        let mut head = self.bytes[..PAGE as usize].to_vec();
        head[64..72].copy_from_slice(&xxh3_64(&table).to_le_bytes());
        let len = u32::from_le_bytes(head[12..16].try_into().unwrap()) as usize;
        let sum = xxh3_64(&head[..len - 8]);
        head[len - 8..len].copy_from_slice(&sum.to_le_bytes());
        self.write(0, &head);
    }

    fn unrelocate(&self) {
        self.restore(0, PAGE as usize);
        self.restore(self.table.0, self.table.1);
        self.file.set_len(self.bytes.len() as u64).unwrap();
    }
}

/// How a section's bytes can be decompressed, to damage what the decoders see.
enum Sealed {
    /// One zstd frame (`META`, `RHDR`, `SADM`, …).
    Frame(Vec<u8>),
    /// The blocked layout: `count`, and each block's `first` and decompressed bytes.
    Blocks(u32, Vec<(u32, Vec<u8>)>),
}

fn unseal(b: &[u8]) -> Option<Sealed> {
    let u32_at = |i: usize| -> Option<u32> {
        Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().unwrap()))
    };
    if let Ok(raw) = zframe::decompress(&mut DCtx::create(), b, None) {
        return Some(Sealed::Frame(raw));
    }
    let (count, n) = (u32_at(0)?, u32_at(4)? as usize);
    if n == 0 || 8 + 12 * n > b.len() {
        return None;
    }
    let mut blocks = Vec::with_capacity(n);
    for i in 0..n {
        let (first, at, len) = (
            u32_at(8 + 12 * i)?,
            u32_at(12 + 12 * i)? as usize,
            u32_at(16 + 12 * i)? as usize,
        );
        let frame = b.get(at..at + len)?;
        blocks.push((
            first,
            zframe::decompress(&mut DCtx::create(), frame, None).ok()?,
        ));
    }
    Some(Sealed::Blocks(count, blocks))
}

fn seal(s: &Sealed) -> Vec<u8> {
    let compress = |raw: &[u8]| zframe::compressor(3, &[]).unwrap().compress(raw).unwrap();
    match s {
        Sealed::Frame(raw) => compress(raw),
        Sealed::Blocks(count, blocks) => {
            let frames: Vec<Vec<u8>> = blocks.iter().map(|(_, b)| compress(b)).collect();
            let mut out = Vec::new();
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
            let mut at = 8 + 12 * blocks.len();
            for ((first, _), f) in blocks.iter().zip(&frames) {
                for v in [*first, at as u32, f.len() as u32] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                at += f.len();
            }
            for f in &frames {
                out.extend_from_slice(f);
            }
            out
        }
    }
}

/// Damages `b` in place: one to four bytes, half the time within its first 64 (where
/// counts and headers are), flipped, zeroed or set to 0xff. Returns the positions.
fn damage(rng: &mut Rng, b: &mut [u8]) -> Vec<usize> {
    let mut at = Vec::new();
    for _ in 0..1 + rng.below(4) {
        let i = match rng.below(2) {
            0 => rng.below(b.len().min(64)),
            _ => rng.below(b.len()),
        };
        b[i] = match rng.below(4) {
            0 => 0,
            1 => 0xff,
            _ => b[i] ^ (1 + rng.below(255)) as u8,
        };
        at.push(i);
    }
    at
}

#[derive(Default)]
struct Tally {
    trials: usize,
    with_errors: usize,
    failures: Vec<String>,
}

/// The message and place of the last panic on each thread, from the hook.
static PANICS: Mutex<Vec<(std::thread::ThreadId, String)>> = Mutex::new(Vec::new());

/// Runs `exercise` on another thread: a panic is caught and a hang is given up on.
fn trial(first: &Path, plan: &Arc<Plan>, g: Groups) -> Result<usize, String> {
    let (tx, rx) = mpsc::channel();
    let (first, plan) = (first.to_owned(), plan.clone());
    let h = std::thread::spawn(move || {
        let r = panic::catch_unwind(AssertUnwindSafe(|| exercise(&first, &plan, g)));
        let _ = tx.send(r.is_ok());
        r.ok()
    });
    let id = h.thread().id();
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(true) => Ok(h.join().unwrap().unwrap()),
        Ok(false) => {
            let _ = h.join();
            let p = PANICS.lock().unwrap();
            let msg = p.iter().rev().find(|(t, _)| *t == id);
            Err(msg.map_or("a panic".into(), |(_, m)| format!("panic: {m}")))
        }
        Err(_) => Err("no answer in 120 s (a hang, or very slow)".into()),
    }
}

#[test]
#[ignore = "needs OMP_TEST_PACK, a small pack of one chunk with every section"]
fn damaged_sections_are_errors_not_panics() {
    let src = PathBuf::from(std::env::var_os("OMP_TEST_PACK").expect("OMP_TEST_PACK"));
    let trials: usize =
        std::env::var("OMP_DAMAGE_TRIALS").map_or(10, |v| v.parse().expect("OMP_DAMAGE_TRIALS"));
    let only: Option<Vec<String>> = std::env::var("OMP_DAMAGE_SECTIONS")
        .ok()
        .map(|v| v.split(',').map(str::to_owned).collect());
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("t.omp.000");
    std::fs::copy(&src, &first).unwrap();
    let subject = Subject::new(&first);
    let sections = Pack::open(&first).unwrap().sections;

    let t = Instant::now();
    let plan = Arc::new(plan(&first));
    println!(
        "plan: {} tiles, {} points, {} queries, {} stops, in {:.1} s",
        plan.tiles.len(),
        plan.points.len(),
        plan.queries.len(),
        plan.stops.len(),
        t.elapsed().as_secs_f64()
    );
    let t = Instant::now();
    let all = Groups {
        tiles: true,
        search: true,
        routing: true,
    };
    let base = trial(&first, &plan, all).expect("the intact pack reads");
    assert_eq!(base, 0, "the intact pack reads without errors");
    println!("intact: read in {:.1} s", t.elapsed().as_secs_f64());

    panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let at = info.location().map(|l| l.to_string()).unwrap_or_default();
        let id = std::thread::current().id();
        PANICS.lock().unwrap().push((id, format!("{msg} at {at}")));
    }));

    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    for (k, s) in sections.iter().enumerate() {
        let name = String::from_utf8_lossy(&s.id).into_owned();
        if only.as_ref().is_some_and(|o| !o.contains(&name)) {
            continue;
        }
        let g = groups(s.id);
        let original = subject.section(s).to_vec();
        let sealed = unseal(&original);
        let t = Instant::now();
        for i in 0..trials {
            // On disk.
            let seed = (k as u64 + 1) << 32 | i as u64;
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let mut b = original.clone();
            let at = damage(&mut rng, &mut b);
            for &j in &at {
                subject.write(PAGE + s.offset + j as u64, &b[j..j + 1]);
            }
            let r = trial(&first, &plan, g);
            for &j in &at {
                subject.restore(PAGE + s.offset + j as u64, 1);
            }
            let tally = tallies.entry(format!("{name} on disk")).or_default();
            tally.trials += 1;
            match r {
                Ok(e) => tally.with_errors += usize::from(e > 0),
                Err(e) => tally
                    .failures
                    .push(format!("seed {seed:#x} at {at:?}: {e}")),
            }

            // Decompressed, damaged and compressed again.
            let Some(sealed) = &sealed else { continue };
            let damaged = match sealed {
                Sealed::Frame(raw) => {
                    let mut raw = raw.clone();
                    let at = damage(&mut rng, &mut raw);
                    (Sealed::Frame(raw), format!("{at:?}"))
                }
                Sealed::Blocks(count, blocks) => {
                    let mut blocks = blocks.clone();
                    let bi = rng.below(blocks.len());
                    let at = damage(&mut rng, &mut blocks[bi].1);
                    (Sealed::Blocks(*count, blocks), format!("block {bi} {at:?}"))
                }
            };
            subject.relocate(s.id, &seal(&damaged.0));
            let r = trial(&first, &plan, g);
            subject.unrelocate();
            let tally = tallies.entry(format!("{name} sealed")).or_default();
            tally.trials += 1;
            match r {
                Ok(e) => tally.with_errors += usize::from(e > 0),
                Err(e) => tally
                    .failures
                    .push(format!("seed {seed:#x} at {}: {e}", damaged.1)),
            }
        }
        println!(
            "{name}: {} bytes, {}, {:.1} s",
            s.len,
            match &sealed {
                Some(Sealed::Frame(_)) => "one frame".to_string(),
                Some(Sealed::Blocks(_, b)) => format!("{} blocks", b.len()),
                None => "raw".into(),
            },
            t.elapsed().as_secs_f64()
        );
    }
    let _ = panic::take_hook();

    let mut failed = 0;
    for (what, t) in &tallies {
        println!(
            "{what:<16} {:>4} trials, {:>4} with errors, {:>3} failures",
            t.trials,
            t.with_errors,
            t.failures.len()
        );
        for f in &t.failures {
            println!("    {f}");
        }
        failed += t.failures.len();
    }
    assert_eq!(failed, 0, "damaged sections panicked or hung");
}

//! `omp-builder fetch`: downloads a region's inputs and records them in `manifest.toml`.

mod http;
pub(crate) mod manifest;
pub(crate) mod regions;
mod sources;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, ValueEnum};
use ureq::Agent;

use manifest::{FileEntry, License, Manifest};

/// Geofabrik snapshot pinned for all Phase 0 measurements (a first-of-month extract).
const OSM_SNAPSHOT: &str = "261001";
const TIGER_YEAR: u16 = 2026;
/// NAR release, `YYYYMM`.
const NAR_RELEASE: &str = "202606";

#[derive(Args)]
pub struct FetchArgs {
    /// Geofabrik region id, e.g. us/ohio, ontario, quebec, north-america.
    #[arg(long)]
    region: String,

    /// Sources to fetch. Ones that do not apply to the region (tiger in Canada) are skipped.
    /// `water` is global and not date-pinned; the manifest records the hash of the copy fetched.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "osm,water,poly,openaddresses,tiger,oda,nar"
    )]
    sources: Vec<Source>,

    /// Output directory [default: <work dir>/inputs]
    #[arg(long)]
    out: Option<PathBuf>,

    /// Geofabrik extract date, YYMMDD.
    #[arg(long, default_value = OSM_SNAPSHOT)]
    osm_snapshot: String,

    #[arg(long, default_value_t = TIGER_YEAR)]
    tiger_year: u16,

    /// National Address Register release, YYYYMM.
    #[arg(long, default_value = NAR_RELEASE)]
    nar_release: String,

    /// Parallel downloads.
    #[arg(long, default_value_t = 4)]
    jobs: usize,

    /// Re-download files that are already in the manifest. Without this, a file that is
    /// present with its recorded size is kept even if upstream has a newer version, so
    /// the input set stays fixed once fetched.
    #[arg(long)]
    refresh: bool,

    /// List what would be downloaded, without downloading.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Source {
    Osm,
    /// osmdata.openstreetmap.de ocean polygons (global, one file).
    Water,
    /// Geofabrik boundary polygons of each state's and province's extract.
    Poly,
    #[value(name = "openaddresses")]
    OpenAddresses,
    Tiger,
    /// StatCan Open Database of Addresses (2021).
    Oda,
    /// StatCan National Address Register.
    Nar,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::Osm => "osm",
            Self::Water => "water",
            Self::Poly => "poly",
            Self::OpenAddresses => "openaddresses",
            Self::Tiger => "tiger",
            Self::Oda => "oda",
            Self::Nar => "nar",
        }
    }
}

struct Job {
    /// Relative to the output directory, `/`-separated.
    path: String,
    source: Source,
    source_id: Option<String>,
    url: String,
    expected_size: Option<u64>,
    expected_md5: Option<String>,
    license: LicenseSpec,
}

enum LicenseSpec {
    Known(License),
    /// Read from the job's source definition when the file is downloaded.
    OpenAddresses {
        job: u64,
        layer: String,
    },
}

pub fn run(args: FetchArgs, work_dir: &Path) -> Result<()> {
    ensure!(
        args.osm_snapshot.len() == 6 && args.osm_snapshot.bytes().all(|b| b.is_ascii_digit()),
        "--osm-snapshot must be YYMMDD"
    );
    let out = args.out.clone().unwrap_or_else(|| work_dir.join("inputs"));
    let agent = http::agent();

    let index = http::get_string(&agent, regions::GEOFABRIK_INDEX)?;
    let region = regions::resolve(&index, &args.region)?;
    let subs: Vec<String> = region.subdivisions.iter().map(|s| s.oa_prefix()).collect();
    println!(
        "region {}: {} subdivisions ({})",
        region.id,
        subs.len(),
        subs.join(" ")
    );

    let mut jobs = Vec::new();
    for &source in &args.sources {
        let planned = match source {
            Source::Osm => sources::osm(&agent, &region, &args.osm_snapshot)?,
            Source::Water => sources::water(),
            Source::Poly => sources::polys(&index, &region)?,
            Source::OpenAddresses => sources::openaddresses(&agent, &region)?,
            Source::Tiger => sources::tiger(&agent, &region, args.tiger_year)?,
            Source::Oda => sources::oda(&region),
            Source::Nar => sources::nar(&region, &args.nar_release),
        };
        println!("{}: {} files", source.as_str(), planned.len());
        jobs.extend(planned);
    }

    let manifest_path = out.join("manifest.toml");
    let manifest = Manifest::load_or_default(&manifest_path)?;
    let (done, todo): (Vec<Job>, Vec<Job>) = jobs
        .into_iter()
        .partition(|j| !args.refresh && is_present(&manifest, &out, j));
    let known_bytes: u64 = todo.iter().filter_map(|j| j.expected_size).sum();
    println!(
        "{} files to download ({} already present); {} of them with known size total {:.1} MiB",
        todo.len(),
        done.len(),
        todo.iter().filter(|j| j.expected_size.is_some()).count(),
        known_bytes as f64 / (1 << 20) as f64
    );
    if args.dry_run {
        for j in &todo {
            println!("  {}  <-  {}", j.path, j.url);
        }
        return Ok(());
    }

    std::fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;
    let start = Instant::now();
    let (entries, failures) = run_jobs(&agent, &out, todo, args.jobs.max(1), &manifest_path);
    let fetched = entries.len();
    save_merged(&manifest_path, &entries)?;
    println!(
        "fetched {fetched} files in {:.0?}; manifest: {}",
        start.elapsed(),
        manifest_path.display()
    );
    if !failures.is_empty() {
        for (path, err) in &failures {
            eprintln!("FAILED {path}: {err:#}");
        }
        bail!("{} downloads failed; rerun to retry them", failures.len());
    }
    Ok(())
}

fn is_present(manifest: &Manifest, out: &Path, job: &Job) -> bool {
    manifest
        .get(&job.path)
        .is_some_and(|e| std::fs::metadata(out.join(&job.path)).is_ok_and(|m| m.len() == e.size))
}

/// Adds `entries` to the manifest as it is on disk now, so another `fetch` writing the same
/// manifest keeps its entries.
fn save_merged(path: &Path, entries: &[FileEntry]) -> Result<()> {
    let mut manifest = Manifest::load_or_default(path)?;
    manifest.upsert(entries.iter().cloned());
    manifest.save(path)
}

/// How often the manifest is saved during a long fetch, so an interrupted one keeps its files.
const SAVE_EVERY: Duration = Duration::from_secs(30);

/// Downloads `jobs` on `workers` threads. Returns the new manifest entries and the failures.
fn run_jobs(
    agent: &Agent,
    out: &Path,
    jobs: Vec<Job>,
    workers: usize,
    manifest_path: &Path,
) -> (Vec<FileEntry>, Vec<(String, anyhow::Error)>) {
    let next = AtomicUsize::new(0);
    let results = Mutex::new((Vec::new(), Vec::new()));
    let last_save = Mutex::new(Instant::now());
    std::thread::scope(|s| {
        for _ in 0..workers.min(jobs.len()) {
            s.spawn(|| {
                while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let r = fetch_with_retries(agent, out, job);
                    let mut results = results.lock().unwrap();
                    match r {
                        Ok(e) => results.0.push(e),
                        Err(e) => results.1.push((job.path.clone(), e)),
                    }
                    let mut last = last_save.lock().unwrap();
                    if last.elapsed() >= SAVE_EVERY {
                        if let Err(e) = save_merged(manifest_path, &results.0) {
                            eprintln!("  could not save the manifest: {e:#}");
                        }
                        *last = Instant::now();
                    }
                }
            });
        }
    });
    results.into_inner().unwrap()
}

fn fetch_with_retries(agent: &Agent, out: &Path, job: &Job) -> Result<FileEntry> {
    const ATTEMPTS: u32 = 3;
    let mut attempt = 1;
    loop {
        match fetch_one(agent, out, job) {
            Ok(e) => return Ok(e),
            Err(e) if attempt < ATTEMPTS => {
                eprintln!("  retrying {} after: {e:#}", job.path);
                std::thread::sleep(Duration::from_secs(5 << attempt));
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

fn fetch_one(agent: &Agent, out: &Path, job: &Job) -> Result<FileEntry> {
    let (license, metadata_url) = match &job.license {
        LicenseSpec::Known(l) => (l.clone(), None),
        LicenseSpec::OpenAddresses { job, layer } => {
            let (l, url) = sources::openaddresses_license(agent, *job, layer)?;
            (l, Some(url))
        }
    };

    let dest = out.join(&job.path);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name().unwrap_or_default().to_string_lossy()
    ));
    let started = Instant::now();
    let d = http::download(agent, &job.url, &part, &job.path)?;
    let check = || -> Result<()> {
        if let Some(size) = job.expected_size {
            ensure!(
                d.size == size,
                "size {} but the source lists {size}",
                d.size
            );
        }
        if let Some(md5) = &job.expected_md5 {
            ensure!(&d.md5 == md5, "md5 {} but the publisher's is {md5}", d.md5);
        }
        Ok(())
    };
    if let Err(e) = check() {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &dest).with_context(|| format!("rename to {}", dest.display()))?;
    println!(
        "  {} ({:.1} MiB, {:.0?})",
        job.path,
        d.size as f64 / (1 << 20) as f64,
        started.elapsed()
    );

    Ok(FileEntry {
        path: job.path.clone(),
        source: job.source.as_str().into(),
        source_id: job.source_id.clone(),
        url: job.url.clone(),
        metadata_url,
        downloaded: jiff::Timestamp::now()
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
        size: d.size,
        sha256: d.sha256,
        md5: job.expected_md5.as_ref().map(|_| d.md5),
        license,
    })
}

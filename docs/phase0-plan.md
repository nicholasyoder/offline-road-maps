# Phase 0 Plan: Measure and Prototype

*Draft 2, 2026-10-23. Expands Phase 0 of [roadmap.md](roadmap.md#phase-0-measure-and-prototype).*

## Purpose

Phase 0 answers one question before any real app code is written: **can a North America pack with full addresses fit the roughly 6–10 GB budget in roadmap §4, and in what format?** Everything built here is either kept (workspace, CI, PBF reading, fetch) or thrown away on purpose (encoding experiments). Spike code may be rough; measurements must be reproducible.

### Exit criteria (expanded)

1. **Confirmed size table.** Measured, not estimated, bytes per layer for the US test state, both Canadian test provinces and all of North America, with the chosen encoding. It replaces the estimates in roadmap §4, with a planet extrapolation and the CoMaps comparison next to it.
2. **`docs/format.md`.** Describes the container (header, section table, chunking, versioning) in full, and each section's encoding at least at the level the measurements support. Sections not yet prototyped (routing, search) are marked *provisional*.
3. **Address decision.** A written answer, with numbers, on whether the OSM + OpenAddresses + TIGER merge gives "full address support" and what it costs per address.
4. **Reproducible.** One documented command sequence regenerates every number in the table from fetched inputs.

If the measured North America total is over **15 GB**, stop and revisit scope (zoom levels, POI set, service roads, address precision) before Phase 1.

## Status

| Step | Status | Notes |
|---|---|---|
| 0.1 Workspace and CI | **Done** 2026-10-23 | CI green on `master`. Two manual checks open: launch the Windows `.exe`, and the old-laptop test. |
| 0.2 Test regions and `fetch` | **Done** 2026-10-23 | Ohio, Ontario and Quebec inputs fetched (5.0 GB). NAR chosen over ODA. 79 of 92 Ohio OpenAddresses sources have no license in their metadata. |
| 0.3 Builder spike | Not started | |
| 0.4 Address spike | Not started | |
| 0.5 North America run | Not started | |
| 0.6 Size table and `format.md` | Not started | |

## Environment notes

Current dev machine: 16 cores, 62 GB RAM, Rust 1.98. Only about 31 GB is free on `/`, so all builder data goes on the data disk. GNU `time` is not installed (`/usr/bin/time` is missing); install the `time` package before 0.3 needs peak RSS.

**Work directory:** `$OMP_WORK=/media/nicholas/Data/maps-testing` on `sda1` (about 586 GB free). Stay inside that folder; the rest of the disk holds unrelated data.
- The disk is mounted by udisks, not `/etc/fstab`. If it is missing, run `udisksctl mount -b /dev/sda1`.
- **It is exFAT.** That has three consequences:
  - **No sparse files.** A dense node-location index (0.3, strategy 1) is about 8 bytes × max node ID. Node IDs are global, so that is roughly 100 GB of real disk even for Ohio, and it must be zero-filled. Run the dense variant for Ohio on ext4 (`/`), where holes stay free. For North America, either accept about 100 GB of physical writes on exFAT (record the time) or skip the variant if the needed-nodes-only strategy has already won.
  - **No Unix permissions or symlinks.** Keep the Cargo `target/` directory and anything executable on `/`. Only inputs, temp files and outputs go on `$OMP_WORK`.
  - Single files up to 16 EB are fine, so large temp files are not a problem.

**Repo:** public on GitHub (`nicholasyoder/offline-road-maps`). The default branch is `master`, and Actions is enabled with all actions allowed. Public repos get standard runners free, so CI minutes are not a constraint.

---

## 0.1 Workspace and CI

> **Done 2026-10-23** (commit `787cd68`, [first green CI run](https://github.com/nicholasyoder/offline-road-maps/actions/runs/40039450835)).
> Stripped sizes: `omp-app` 9.7 MB on Linux (needs glibc 2.35) and 8.6 MB on Windows; `omp-builder` 0.65 MB on both.
> Note: eframe is 0.37, where `App` implements `ui(&mut Ui, …)`, not `update(&Context, …)`.
> Still to do by hand: launch the Windows `.exe` on a real Windows PC, and launch on an old laptop with integrated graphics (we may need a GL fallback). The Linux build has opened a window on the dev machine (Vulkan via radv).

### Layout

```
Cargo.toml              # [workspace], shared lints and profile settings
rust-toolchain.toml     # pin 1.98, components rustfmt + clippy
crates/
  core/                 # omp-core: format types, reader/writer, tag whitelist, normalization
  builder/              # omp-builder: CLI (fetch, measure, addr-spike, ...)
  app/                  # omp-app: eframe stub window (size and cross-build canary only)
docs/
data/                   # .gitignored; default work dir if $OMP_WORK is unset
```

- Edition 2024. Workspace-level `[lints]` with `clippy::all` as warnings and `unsafe_code = "deny"` except in `core` (mmap needs it).
- Release profile: `lto = "fat"`, `codegen-units = 1`, `strip = true`, `panic = "abort"` for the app. A separate `release-builder` profile without `panic = "abort"`, so builder crashes keep backtraces.
- Builder CLI uses `clap` (derive). Spikes are builder subcommands, not separate crates, so code that survives stays in place.
- License: `MIT OR Apache-2.0` (decided). Add `LICENSE-MIT` and `LICENSE-APACHE`, set `license = "MIT OR Apache-2.0"` in `[workspace.package]`, and update roadmap §8.

### App stub

`omp-app` opens an eframe window with a `wgpu` backend and draws one colored rectangle. It exists only so CI proves early that:
- the wgpu + egui stack cross-builds for Windows and Linux;
- the stripped binary is well under the 30 MB budget (record the baseline in the size table);
- it starts on an old laptop with integrated graphics. Test once by hand, and note whether the GL fallback is needed.

### CI (GitHub Actions)

Triggered on push to `master` and on pull requests. Jobs:

| Job | Runner | Steps |
|---|---|---|
| `check` | `ubuntu-latest` | `cargo fmt --check`, `cargo clippy --workspace -D warnings`, `cargo test --workspace` |
| `build-linux` | `ubuntu-22.04` | Release build of `omp-app` and `omp-builder` for `x86_64-unknown-linux-gnu`. Building on 22.04 keeps the glibc requirement at 2.35. Consider `cargo-zigbuild` with a lower glibc target later if older distros matter. |
| `build-windows` | `windows-latest` | Release build for `x86_64-pc-windows-msvc`, with a static CRT (`-C target-feature=+crt-static`) so no VC++ redistributable is needed on the target PC. |

- Native runners instead of cross-compiling from Linux: simpler, and the MSVC target is what Windows users should get.
- Cache with `Swatinem/rust-cache`. Upload binaries as artifacts. Print stripped binary sizes in the job summary so size regressions are visible.
- Optional local check: `cargo build --target x86_64-pc-windows-gnu` with mingw, for quick Windows compile checks without pushing.

**Done when:** a push to `master` produces Linux and Windows `omp-app` binaries that open a window, and their sizes are recorded.

---

## 0.2 Test regions and `fetch`

> **Done 2026-10-23** (the commit that adds `omp-builder fetch`).
> **Pinned inputs:** Geofabrik snapshot `261001` (2026-10-01, a first-of-month extract that Geofabrik keeps for several months; daily ones last about a week), TIGER 2026, NAR release `202606`. OpenAddresses is pinned by the job IDs in the manifest, since `fetch` keeps files already in the manifest unless `--refresh` is given.
> **Reproduce:**
> ```
> cargo build --profile release-builder -p omp-builder
> B=target/release-builder/omp-builder
> $B fetch --region us/ohio     # 181 files, 589 MiB, 2 min
> $B fetch --region ontario     #  72 files, 2.7 GB with the 1.6 GB national NAR zip, 6.5 min
> $B fetch --region quebec      #  12 files, 1.4 GB (reuses the NAR zip), 4.5 min
> ```
> Rerunning is a no-op. Region ids are Geofabrik's (`us/delaware`, `ontario`, `north-america`, ...); `--dry-run` lists the files without downloading them.
> **Inputs (raw records):**
> - Ohio: OSM 310 MiB; 88 TIGER counties; 92 OpenAddresses sources with 6.9M records (2 more listed with no output).
> - Ontario: OSM 984 MiB; 69 OA sources with 7.1M records (18 with no output).
> - Quebec: OSM 1,108 MiB; 10 OA sources with 6.7M records, of which the province-wide Adresses Québec source (CC BY 4.0) is 5.3M. **Quebec address coverage is not thin**, so it serves as a full address test region.
>
> Raw OA counts overlap: city and county sources cover the same addresses.
> **OpenAddresses download:** no account or token is needed. `batch.openaddresses.io/api/data?source=<prefix>&layer=addresses` lists the jobs, and `v2.openaddresses.io/batch-prod/job/<id>/source.geojson.gz` redirects to a public, presigned download. The `/api/job/<id>/output/...` path does need a token. Each file's license comes from the source definition at the commit the job ran from, which is recorded as `metadata_url`.
> **Licenses:** the manifest records what each source states. **79 of the 92 Ohio OA sources state no license** (mostly Ohio LBRS county files) and are recorded as `unspecified`. A further 10 say attribution is required but give no license name. Before Phase 5, the attribution file and license filter (roadmap §7) must decide how to treat `unspecified`.
> **ODA vs NAR:** results below; NAR is the default Canadian source.
> Still open: `fetch` does not yet download the osmdata.openstreetmap.de land and water polygons; add them with 0.3. Not yet run for `north-america`; that run belongs to 0.5.

### Regions

| Role | Region | Why |
|---|---|---|
| Inner dev loop | A small US state, e.g. Delaware or Rhode Island | Runs in seconds; used for every code change. |
| US test state | **Ohio** | Mid-size, a mix of urban and rural, good TIGER coverage, statewide OpenAddresses sources. |
| Canadian test province 1 | **Ontario** | Most likely the best Canadian address coverage. It is the main check that the Canadian address merge works. |
| Canadian test province 2 | **Quebec** | Exercises French street names and normalization. ODA coverage is thin (1.25M points), but NAR (3.2M civic addresses) and the OpenAddresses province-wide source (5.3M records) cover it well. |
| Full scale | **North America** (Geofabrik `north-america-latest.osm.pbf`) | The confirmed numbers. Run last, once per encoding candidate, on the large disk. |

Both provinces are run because they test different things and together they cost little compared with the North America run. Pin one Geofabrik snapshot date for all Phase 0 measurements so numbers stay comparable.

### `omp-builder fetch`

A minimal but real subcommand, since Phase 5 needs it anyway:

```
omp-builder fetch --region us/ohio --sources osm,openaddresses,tiger,oda,nar --out $OMP_WORK/inputs/
```

- **OSM:** Geofabrik extract, plus checksum verification against the published `.md5`.
- **OpenAddresses:** per-source files for the region. Check the current download mechanism first; bulk downloads from batch.openaddresses.io may need an account or token. If so, document the manual step and have `fetch` pick up files placed in the inputs folder.
- **TIGER `ADDRFEAT`:** one zip per county (`tl_<year>_<county FIPS>_addrfeat.zip`; Ohio is state FIPS 39 with 88 counties). Build the county list from the TIGER directory listing, not a hard-coded table.
- **StatCan addresses:** fetch both the ODA per-province CSV (2021) and the newer **National Address Register** (NAR, Statistics Canada Open Licence, regularly updated). Compare them for Ontario and Quebec: record count, whether points have coordinates (or only a link to a building or dissemination block), and licence. Use NAR as the default source if it has usable coordinates, and update roadmap §3 with the result.
**ODA vs NAR result (2026-10-23).** NAR `202606` is one national zip (1.6 GB). It has per-province address CSVs plus location CSVs, joined on `LOC_GUID`. Coordinates are on the location: either a building point (`BG_LATITUDE/LONGITUDE`) or a blockface representative point (`BF_REPPOINT_*`). One civic address counts as one location; apartment units are extra address rows.

| | Ontario | Quebec |
|---|---|---|
| ODA v1 (2021) rows / distinct number+street+CSD | 4,033,020 / 3,352,397 | 1,254,237 / 1,229,031 |
| ODA rows with coordinates | 100% | 100% |
| NAR civic addresses (locations) | 4,195,809 | 3,237,345 |
| NAR building coordinate | 3,448,811 (82.2%) | 3,229,937 (99.8%) |
| NAR blockface point only | 720,479 (17.2%) | 5,660 (0.2%) |
| NAR no coordinate | 26,519 (0.6%) | 1,748 (0.05%) |
| NAR address rows (including units) | 6,246,543 | 4,568,811 |
| Licence | ODA: OGL – Canada. NAR: Statistics Canada Open Licence (also contains OGL – Yukon data). Both are attribution-only. | |

**Decision:** NAR is the default Canadian source. It covers more in both provinces (2.6× ODA in Quebec), it is current, and it has building coordinates for almost all addresses. `fetch` still downloads ODA by default so 0.4 can measure what it adds. For example, ODA may have building points for some of Ontario's 720k blockface-only NAR addresses. Blockface points are approximate, about the middle of one side of a block. 0.4 should treat them like interpolated addresses, not exact points.

- Write `inputs/manifest.toml`: URL, download date, size, checksum and **license plus attribution text** for every file. This is the seed of the Phase 5 attribution file and of the license filter (roadmap §7).

**Done when:** `fetch` reproduces the Ohio input set from scratch and the manifest lists every source's license.

---

## 0.3 Builder spike: PBF, whitelist, per-layer sizes

### Reading the PBF

- Use the `osmpbf` crate for parallel blob decoding in the spike. Replace it with our own reader only if profiling shows a need.
- **Node locations:** compare two strategies, because this decides builder RAM for North America and the planet:
  1. **Dense mmap index.** One `(i32 lat, i32 lon)` slot per node ID in a sparse file. Simple; the file is about 8 bytes × max node ID (around 100 GB virtual for the planet, mostly holes).
  2. **Needed-nodes-only.** Pass 1 reads ways and relations and sets a bitset of node IDs referenced by whitelisted features. Pass 2 stores only those locations in a sorted array. Uses far less disk and RAM, at the cost of an extra pass.
  
  Record wall time, peak RSS (`/usr/bin/time -v`) and temp disk use for both on Ohio and North America.
- **Multipolygons:** a basic ring assembler (join way segments into closed rings, classify outer and inner) for water, land cover and admin boundaries. Count assembly failures instead of fixing every edge case. Coastlines: use the preprocessed OSM land/water polygons from osmdata.openstreetmap.de (ODbL, record in the manifest) rather than assembling coastline ways.

### Tag whitelist

Lives in `core` as a versioned data file (e.g. `crates/core/data/whitelist.toml`) so the builder and the app style share one definition. Starting set, mapped to layers:

| Layer | Tags (first draft) | Open question to measure |
|---|---|---|
| Roads | `highway=motorway…residential`, `*_link`, `unclassified`, `living_street`, `service` | `service` roads (driveways, parking aisles) are large. Measure with all of them, with only `service` without a `service=*` subtag, and with none. |
| Ferries | `route=ferry` | — |
| Rail | `railway=rail` (not `abandoned`, `disused`, `tram`, `subway`) | — |
| Water | `natural=water`, `waterway=river,canal` (lines), `riverbank`, coastline/ocean polygons | Smaller streams only at high zoom, or not at all? |
| Land cover | `landuse=forest,residential,industrial,commercial,farmland`, `natural=wood`, `leisure=park`, protected areas | Biggest polygon volume. Measure each class. |
| Places | `place=*` nodes | — |
| Boundaries | `boundary=administrative`, `admin_level` 2, 4, 6, 8 | Levels 9 and 10 needed for search? |
| POIs | Fuel, EV charging, rest areas and services, hotels and motels, restaurants and fast food, hospitals, airports, campgrounds, tourist attractions, parking (large lots only?) | POI count is the main size lever; measure per category. |
| Addresses | `addr:housenumber` on nodes and building ways (centroid only, geometry dropped), `addr:interpolation` ways | Feeds 0.4. |
| Routing extras | `type=restriction` relations, `oneway`, `maxspeed`, `toll`, `access` | Counted here, used in Phase 3. |

Output a per-tag-value count and byte report, so whitelist decisions are made from data.

### Encoding variants

Run each variant over the same filtered feature set, for a z0–z14 tile pyramid. Report bytes per layer and per zoom.

| Variant | Description |
|---|---|
| V0 | Raw baseline: `f64` coordinates, tags as strings. Only to show what each step saves. |
| V1 | Tile-local integer coordinates (4096 grid per tile), delta + zigzag varint, tag keys and values as small integer enums, names in a per-tile string table. |
| V2 | V1 + Douglas–Peucker simplification per zoom, dropping features below a minimum size per zoom. |
| V3 | V2 + zstd per tile, no dictionary. |
| V4 | V2 + zstd with dictionaries trained per layer group and zoom band. |
| V5 *(optional)* | V4 with tile-group compression (e.g. 4×4 tiles per zstd frame) to trade random-access cost for ratio. |
| Reference | Standard MVT + gzip for the same features (own MVT writer, or Planetiler/tilemaker if easier), to sanity-check against common tooling. |

Also measure:
- **Tile index** overhead with Hilbert ordering: offset table size and how many tiles are empty.
- **Random-read cost** of V3/V4/V5: decode time per tile and bytes read per typical viewport, on a USB 2 flash drive if one is available. Size alone shouldn't pick the winner.

### Routing and search estimates

These are layers in the size table too, so get real numbers cheaply:
- **Routing:** build the car road graph (intersections as nodes, segments as edges, with one-way and restrictions counted) and run an off-the-shelf contraction hierarchy (the `fast_paths` crate) on Ohio. Record node, edge and shortcut counts and the serialized size. Extrapolate North America from the edge-count ratio, then confirm on the full run if memory allows.
- **Search:** build an `fst` over normalized tokens of places, street names and POI names, plus a naive postings layout. Record its size. It only needs to be in the right ballpark.

### Interface

```
omp-builder measure <input.pbf> --whitelist <file> --variants v1,v3,v4 --json out/ohio.json
omp-builder report out/*.json > docs/size-table.md
```

**Done when:** `measure` runs on Ohio, Ontario and Quebec, and the JSON holds feature counts, vertex counts and bytes for every layer × zoom × variant, plus build time and peak RAM.

---

## 0.4 Address spike (Ohio first, then Ontario and Quebec)

### Pipeline

1. **Extract** address records from each source into one shared form: `{source, house number (raw), street (raw), unit, city, postcode, lat, lon}`.
   - OSM: `addr:*` nodes, building-way centroids, `addr:interpolation` ways (expand or keep as ranges).
   - OpenAddresses: the Ohio sources from the manifest. Record each source's license, and add a `--exclude-license` filter now.
   - TIGER `ADDRFEAT`: left and right from/to ranges per street segment; these are ranges, not points.
2. **Normalize** in `core` (shared with Phase 2 search):
   - Street suffixes and directionals per USPS Publication 28 ("St" → "Street", "N" → "North"), case and accents, ordinals ("1st" vs "First").
   - House numbers: plain, letter suffix ("123A"), fractional ("123 1/2"), ranges ("123-125"). Count anything else as an exception.
   - Canada: Canada Post abbreviations, French generic-before-name order ("Rue Saint-Denis", "Boul." → "Boulevard"), accents kept for display and folded for matching.
3. **Attach to streets.** Every address point must link to an OSM street way, because storage is grouped by street (roadmap §4). Match by normalized name to the nearest way within a threshold. Try 100, 200 and 500 m. Record the match rate per source and keep unmatched samples to inspect.
4. **Deduplicate.** Same street + same normalized number within *d* metres means one address; OSM wins (roadmap §3). Try *d* = 25, 50, 100 m. Records with the same key but farther apart are **conflicts**: count them and dump a sample with both positions.
5. **TIGER fallback.** For street segments with no point addresses, keep the TIGER range for interpolation, and count how many segments rely on it.

### Metrics

| Metric | Why |
|---|---|
| Unique addresses: OSM only, +OpenAddresses, +ODA/NAR | Coverage gained by each source |
| Share of TIGER ranges with at least one point, before and after the merge | Proxy for "full" coverage, since no single ground truth exists |
| Street-attach match rate per source | Unmatched addresses need a fallback store and cost more bytes |
| Duplicate and conflict rate per threshold | Merge quality (roadmap §7 risk) |
| Normalization exception count | Size of the long tail |
| **Bytes per address** for each encoding variant below | The budget number |

Add a manual spot check: 50 random merged addresses and 25 conflicts, looked up against aerial imagery or local knowledge. Write the error categories into the results.

### Address encoding variants

Per street: a sorted run of entries, delta-encoded.

| Variant | Entry contents |
|---|---|
| A1 | `(number delta varint, offset along street in metres varint, side bit, source bit)` |
| A2 | A1 with offset quantized to 5 m |
| A3 | A2 + zstd over blocks of streets |
| A4 | A3 with offset stored as perpendicular-projected position, plus a small lateral distance (for long rural driveways that geocode far from the road) |

Exceptions (letter suffixes, units, unmatched points with full coordinates) go in a side table; measure it separately. TIGER ranges are stored per segment: `(from, to, parity, side)`.

**Done when:** Ohio, Ontario and Quebec each have the metrics table filled in and a recommended variant, and the conflict sample has been reviewed.

---

## 0.5 Full North America run

Run after 0.3 and 0.4 have picked their candidate variants (at most two per layer).

- Check that `$OMP_WORK` is mounted. Fetch North America OSM, OpenAddresses for the US and Canada, all TIGER `ADDRFEAT` counties (about 3,200 files) and NAR (the Canadian source chosen in 0.2) for every province.
- Run `measure` and the address pipeline on all of it. Record wall time, peak RSS and peak temp disk: these are the first numbers for the Phase 5 `BUILDING.md` hardware section.
- If the run does not fit in 62 GB RAM, that is itself a finding. Switch to the needed-nodes-only strategy or region tiling and note it.

**Done when:** every row of the size table has a North America measurement.

---

## 0.6 Size table and `docs/format.md`

### Size table

Write `docs/size-table.md`, generated by `omp-builder report` with a short hand-written commentary, and replace the estimates in roadmap §4 with a link to it plus the headline numbers.

| Layer | Ohio | Ontario | Quebec | North America | Planet (extrapolated) | CoMaps equivalent |
|---|---|---|---|---|---|---|

For the CoMaps column, sum the CoMaps `.mwm` files covering the same regions. CoMaps splits US states into several files, so add them up. This shows how far ahead or behind we are, keeping in mind CoMaps includes buildings and paths.

### `docs/format.md`

Fully specified in Phase 0:
- **Container:** magic bytes, format version and compatibility rule (major version must match, minor additions ignored), little-endian, region bounding box and name, build date and input snapshot, section table (`id, offset, length, checksum`), section offsets page-aligned (4 KiB) for mmap.
- **Chunking:** a pack is a directory of `region.omp.000`, `.001`, …, each under 4 GB for FAT32, with no section crossing a chunk boundary (or a defined rule if one must). The reader presents one virtual address space.
- **Checksums:** xxHash3 per section, checked by a "verify pack" command rather than on every load, so start-up stays fast.
- **Metadata and attribution section:** the source manifest, embedded in every pack.
- **Tile section:** tile index (Hilbert order, offsets), zstd dictionaries, and the chosen tile encoding from 0.3.
- **Address section:** the chosen encoding from 0.4, plus TIGER ranges and the exception table.

Marked *provisional*, to be finalized in their phases:
- Routing section (Phase 3), search section (Phase 2), style and label data (Phase 1).

Optionally, write the container reader and writer in `omp-core` now (header, section table, chunking, round-trip tests). It is small, it checks that the spec is implementable, and Phase 1 needs it immediately.

---

## Order of work

```
0.1 workspace + CI ─┬─► 0.2 fetch (Ohio, dev state) ─┬─► 0.3 builder spike (Ohio) ─┐
                    │                                 └─► 0.4 address spike (Ohio) ─┤
                    │                                                               ├─► Ontario + Quebec (0.3 + 0.4)
                    └─► app stub binary-size baseline                               │
                                                                                    └─► 0.5 North America ─► 0.6 size table + format.md
```

0.3 and 0.4 can proceed in parallel once Ohio is fetched; they share the PBF reader and normalization code in `core`. Draft `format.md` incrementally as decisions land, not all at the end.

## Decisions

Settled 2026-10-23:
1. **License:** `MIT OR Apache-2.0`.
2. **CI host:** GitHub Actions on the public GitHub repo.
3. **Work directory:** `/media/nicholas/Data/maps-testing` on the exFAT data disk (see Environment notes).
4. **Canadian provinces:** both Ontario and Quebec.

Settled by Phase 0 data:
5. **Canadian address source:** NAR, with ODA kept only if 0.4 shows it adds coverage (2026-10-23, see 0.2).

Still open, to be settled by Phase 0 data: the encoding variants, the `service` road and POI cut-offs, and the dedup threshold.

## Out of scope for Phase 0

- Any rendering beyond the stub window.
- Real contraction hierarchy implementation (an off-the-shelf crate is used only for size numbers).
- Search ranking, the admin hierarchy, and query parsing.
- Planet build (extrapolation only).

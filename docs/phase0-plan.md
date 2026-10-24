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
| 0.3 Builder spike | **Done** 2026-10-23 | `measure` and `report` run on Ohio, Ontario and Quebec. V4 is 134 / 580 / 912 MiB. Lakes and streams are 60–72% of the Canadian tiles. Needed-nodes-only beats the dense index on Ohio; North America is still to confirm. |
| 0.4 Address spike | **Done** 2026-10-23 | `addr-spike` run on Ohio, Ontario and Quebec. Merged 4.95M / 5.58M / 4.24M addresses, at 1.9–2.0 B each with A3. OSM alone has 12–40% of them. 89 of 92 Ohio OA sources have no license name; without them Ohio drops to 1.13M. Dedup at 250 m. ODA dropped. |
| 0.5 North America run | **Done** 2026-10-24 | Tiles V4 9.2 GiB, CH about 3.2 GiB (estimated), search 0.25 GiB or less, addresses 0.36 GiB: about 13 GiB in all, under the 15 GB stop line but over the 6–10 GB budget. Lakes and streams are 40% of z14 bytes. One-pass `measure` did not fit in 56 GB, so it ran in 3 bands (peak 43 GB). 151.5M addresses, or 86.8M without unlicensed sources. |
| 0.6 Size table and `format.md` | Not started | |

## Environment notes

Current dev machine: 16 cores, 62 GB RAM, Rust 1.98. Only about 25 GB is free on `/`, so all builder data goes on the data disk. GNU `time` is installed (`/usr/bin/time -v`); `measure` also records its own peak RSS from `/proc/self/status`.

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
> Still open: not yet run for `north-america`; that run belongs to 0.5. (The osmdata.openstreetmap.de water polygons were added to `fetch` in 0.3.)

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

> **Done 2026-10-23** (the commit that adds `omp-builder measure` and `report`).
> **Reproduce** (inputs from 0.2, plus `fetch --sources water` for the 865 MiB global `osmdata/water-polygons-split-4326.zip`, which is not date-pinned; its hash is in the manifest):
> ```
> M=$OMP_WORK/measure; I=$OMP_WORK/inputs/osm
> $B measure $I/ohio-261001.osm.pbf    --json $M/ohio-sparse.json
> $B measure $I/ohio-261001.osm.pbf    --json $M/ohio-dense.json --nodes dense --node-index /var/tmp/omp-nodes.idx
> $B measure $I/ontario-261001.osm.pbf --json $M/ontario-sparse.json
> $B measure $I/quebec-261001.osm.pbf  --json $M/quebec-sparse.json
> $B report $M/*-sparse.json $M/ohio-dense.json
> ```
> Output is deterministic: features, roads and tile sections are sorted, so reruns give identical JSON.
>
> | | Ohio | Ontario | Quebec |
> |---|---:|---:|---:|
> | Input PBF (MiB) | 310 | 984 | 1,108 |
> | Wall time, 16 threads (s) | 48 | 63 | 73 |
> | Peak RSS (MiB) | 3,443 | 7,937 | 8,916 |
> | V0 raw (f64, string tags) | 2,275 | 13,444 | 19,811 |
> | V1 grid + varint | 302 | 1,429 | 2,176 |
> | V2 + simplify/drop | 173 | 673 | 1,060 |
> | V3 + zstd 19 | 143 | 614 | 968 |
> | **V4 + dictionaries** (dicts 1.1–1.3 MiB, included) | **134** | **580** | **912** |
> | V5 4×4 groups | 128 | 553 | 875 |
> | MVT + gzip reference (V2 geometry) | 170 | 704 | 1,112 |
> | V4 roads / landcover / water | 55 / 61 / 14 | 44 / 185 / 346 | 34 / 168 / 701 |
> | CH graph nodes / edges | 2.2M / 5.3M | 1.3M / 3.0M | 0.76M / 1.8M |
> | CH, compact layout (MiB) | 115 | 66 | 40 |
> | Search: fst + postings + entries (MiB) | 6.6 | 5.6 | 4.3 |
>
> **Node locations** (Ohio, identical output both ways):
> - Needed-nodes-only: read stage 4.4 s and 3.0 GB peak, no temp disk. The bitset is a fixed ~1.7 GB because its pages fill across the whole node-ID range even for one state, so it does not shrink for small extracts.
> - Dense index: 5.2 s and 4.8 GB peak RSS (mapped pages count), plus **3.3 GB of real disk for one state**.
> - Needed-nodes-only is the working default. Roadmap §5 says "dense on-disk index"; change it after 0.5 confirms on North America.
>
> **Encodings:**
> - V2 thinning saves 43–53% over V1. zstd (V3) adds 9–17%, dictionaries (V4) 5–6% more, and V5 groups 4–5% more.
> - V5 reads 2.4–2.6× the bytes per viewport (z14: Ohio 84 KB vs 33 KB) and decodes 10–20% slower.
> - MVT+gzip of the same geometry is 21–27% larger than V4.
> - **Candidate for 0.5: V4.** V5 is second only if the USB 2 test shows its bigger reads don't matter.
>
> **Random reads, V4** (100 viewports of 5×4 tiles, in RAM):
> - z14: 12–33 KB and 0.09–0.29 ms per viewport;
> - z10: 181–519 KB and 1.3–4.7 ms.
>
> **Tile index:** a sparse Hilbert-ordered index (delta varint, layer mask, lengths) is 0.3 / 2.6 / 3.7 MB at z14. Dense offset arrays would be 108 MB for Quebec, where 95% of the z14 bbox is empty, so the index is sparse.
>
> **Whitelist findings** (per-class rows and all tag values are in the JSON):
> - **Water is the biggest lever in Canada.** Lakes are 39% / 53% and streams 21% / 19% of z14 V1 bytes in Ontario / Quebec (3.0M lakes and 2.2M streams in Quebec, mostly CanVec imports). In Ohio they are 5% and 3%. 0.5 needs a size-based lake minzoom and a stream decision (z14 only, or none).
> - **`service` roads** (Ohio ablations, `tile = false`): dropping only subtagged ones (driveways, parking aisles, …) saves 9 MiB, 7% of V4. Dropping all `service` roads saves 21 MiB (16%). In Canada service roads are only 1–2% of z14. Driveways alone are 11.6% of Ohio z14.
> - **POIs are not a size lever:** under 1.3 MiB per region with all categories, parking included (51k–93k lots). *Corrected in 0.5:* the area buckets first counted only parking relations, not closed ways. With ways counted, 29–39% of lots are under 500 m², 38–40% are 500–2,000 m², 20–27% are 2,000–10,000 m² and 3–5% are larger.
> - **Boundaries:** admin 9 and 10 exist (Ontario 559 level-10 relations) but stay `tile = false`. Their need for search is left to Phase 2.
> - **Addresses** (not tiled; feeds 0.4): 0.60M / 2.5M / 1.5M OSM address points. Ontario and Quebec also have 649k / 442k `addr:interpolation` ways.
>
> **Multipolygons:** over 99.9% assemble. Admin boundaries miss members in about 10% of relations, all cut at the extract border.
> **Routing extras:** 16–21k turn restrictions per region. In Ohio, `access` is on 285k road ways and `maxspeed` on 115k.
>
> Still open:
> - USB 2 flash-drive read timing (no drive available; the numbers above are decode-only from RAM).
> - Dense vs needed-nodes on North America (0.5).
> - Lake/stream thresholds, and the `service` decision.
> - The ocean is clipped to the extract's bbox, not its polygon, which slightly overcounts it for Quebec (218 pieces, 1% of z14).

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
omp-builder measure <input.pbf> [--whitelist <file>] [--nodes sparse|dense --node-index <file>] --json out/ohio.json
omp-builder report out/*.json > docs/size-table.md
```

Every variant is computed on every run, since they share the tiling, so there is no `--variants` flag. `--routing ch|graph|none` (added in 0.5) builds the contraction hierarchy, only counts the road graph, or skips routing.

**Done when:** `measure` runs on Ohio, Ontario and Quebec, and the JSON holds feature counts, vertex counts and bytes for every layer × zoom × variant, plus build time and peak RAM.

---

## 0.4 Address spike (Ohio first, then Ontario and Quebec)

> **Done 2026-10-23** (the commit that adds `omp-builder addr-spike`).
> **Reproduce** (inputs from 0.2; the `report` output is `$M/addr-table-0.4.md`, and sample TSVs go in `$M/<region>-samples/`):
> ```
> M=$OMP_WORK/measure/addr; I=$OMP_WORK/inputs/osm
> $B addr-spike $I/ohio-261001.osm.pbf    --subdivision US-OH --json $M/ohio.json      # 17 s, 2.7 GB
> $B addr-spike $I/ontario-261001.osm.pbf --subdivision CA-ON --json $M/ontario.json   # 40 s, 5.8 GB
> $B addr-spike $I/quebec-261001.osm.pbf  --subdivision CA-QC --json $M/quebec.json    # 37 s, 4.2 GB
> $B addr-spike $I/ohio-261001.osm.pbf    --subdivision US-OH --exclude-license unspecified --region ohio-licensed --json $M/ohio-licensed.json
> $B report $M/ohio.json $M/ontario.json $M/quebec.json
> ```
> Defaults: attach within 200 m, dedup within 250 m, zstd 19. The 100/200/500 m attach rates and the 25/50/100/250/1000 m dedup results are always computed. Output is deterministic.
>
> | | Ohio | Ontario | Quebec |
> |---|---:|---:|---:|
> | Unique addresses, OSM only | 576k | 2,261k | 1,437k |
> | + OpenAddresses | 4,947k | 4,909k | 4,126k |
> | + NAR building points | – | 5,245k | 4,233k |
> | + ODA | – | 5,304k (+59k, 1.1%) | 4,242k (+9k, 0.2%) |
> | + NAR blockface only (approximate) | – | 5,581k | 4,244k |
> | Units (separate table) | 0.70M | 2.78M | 1.80M |
> | Attached to an OSM street (200 m) | 94.2% | 93.7% | 96.1% |
> | TIGER ranges with a point: OSM only → merged | 8.6% → 69.6% | – | – |
> | Conflicts at 100 / 250 m | 24k / 8.5k | 303k / 111k | 154k / 67k |
> | **A3 bytes per address** (side tables included) | **1.95** | **1.91** | **1.89** |
> | A1 / A2 / A4 bytes per address | 3.78 / 3.67 / 2.35 | 5.06 / 4.94 / 2.30 | 4.36 / 4.24 / 2.31 |
> | A3 total (MiB) | 9.2 | 10.2 | 7.6 |
> | TIGER fallback ranges (MiB) | 1.9 (420k ranges, 5.3 B each) | – | – |
>
> **Address decision: yes, the merge gives full address support** in all three regions, at about 2 bytes per address, plus about 0.4 B per address for US TIGER fallback ranges.
> - OSM alone has 12% of Ohio's merged addresses, 40% of Ontario's and 34% of Quebec's.
> - Merged totals are close to dwelling counts: Ohio 4.95M civic addresses plus 0.70M units (about 5.2M housing units); Quebec 4.24M.
> - In Ohio, 70% of TIGER ranges now have a point. The other 30% (420k ranges) stay as approximate interpolation.
> - **Licensing caveat:** 89 of the 92 Ohio OpenAddresses sources state no license name (79 state nothing; 10 require attribution without naming one). Excluding `unspecified` cuts Ohio to 1.13M addresses (OSM 576k plus 557k from Cuyahoga, Clermont and Medina) and TIGER range coverage to 13%. "Full" US address support depends on how Phase 5 treats those sources.
>
> **Encoding: A3.**
> - A3 is A2 (5 m offsets) plus zstd over blocks of streets.
> - Street runs are 6.9 / 6.8 / 5.4 MiB. Each address costs one varint (number delta, then side, OSM and approximate bits) plus one zigzag varint for the offset delta.
> - Side tables: units 0.4 / 1.1 / 0.9 MiB, unattached points with names and coordinates 1.9 / 2.1 / 1.1 MiB, and suffixes 0.2 MiB or less.
> - zstd halves the size. A4's lateral distance adds 20%. Keep it in reserve in case long rural driveways geocode badly.
> - Block size barely matters: 4 KiB blocks are 3.6% larger than 32 KiB, and 8 KiB blocks 1.8% larger. So Phase 2 can use small blocks for random access.
> - Street names are not counted here. They are shared with search, and the address store refers to street records (connected same-name ways, offsets measured along them).
>
> **Normalization** (in `omp-core`, shared with Phase 2):
> - USPS/Canada Post abbreviations, French types and directions, and ordinals ("21st", "Twenty-First" and "21e" give "21").
> - Route designators: `ST RTE 180` = `State Route 180` = `SR 180` = OSM `ref`.
> - A second, loose key drops types, directions, articles and designators, strips plural "s", and sorts and joins the words. It catches `VALLEYVISTA DR`, `Jackie's Bend`/`JACKIES`, `Line 14`/`14th Line` and `PETANQUE, CROIS DE LA`.
> - Exact-key attach at 200 m is 87–97% depending on source. The loose key adds 1–7 points.
> - Unparseable house numbers are 0.5% or less per source. Empty numbers are separate: OpenAddresses Ohio 3.3%, nearly all of it **Butler County, whose OA output has no house numbers at all** (153,823 records).
> - What still fails to attach (samples in `unattached.tsv`): streets missing from OSM, spelling differences (Dixon/Dixion, Bloxam/Bloxham), and prefixes like "Oro-Medonte Line 4". Fuzzy matching is left to Phase 2.
>
> **Dedup threshold: 250 m on the same street, same direction set and same number.** NAR blockface points merge with any exact point on the street within 1 km.
> - Within 100 m, almost every pair is clearly one address.
> - Most pairs left at 100 m are 100–1,000 m apart *along* the street, and 72–88% are on the same side. Same-number duplicates that close on one street don't occur in US or Canadian civic numbering, so one source placed the address wrongly.
> - Conflicts concentrate in a few sources: OA Carroll County, Ohio (22% of its records), NAR in Ontario (201k), and the Quebec province-wide OA source vs OSM (85k). OSM disagrees with itself in 42k Ontario and 42k Quebec cases.
> - Conflicts left at 250 m stay as separate addresses: 0.2% in Ohio, 2.0% in Ontario, 1.6% in Quebec.
>
> **Spot check** (50 merged addresses per region and 100 conflicts per region in the samples; 51 merged and 27 conflicts reviewed):
> - All 51 merged addresses attach to the right street, 1–126 m away. One matched through `ref` (`148 ROUTE` → `Rue Principale`, ref 148).
> - Conflict categories:
>   1. 5 of 27 are 20–160 km apart on long routes (SR 4, US 42, Route 105): different towns, correctly kept apart.
>   2. About 18 are 250 m–1.1 km apart on rural roads: one address, one source off.
>   3. About 4 are 1.5–3 km apart and ambiguous.
> - This review used the data only, not aerial imagery.
>
> **Other findings:**
> - **OSM interpolation ways are ranges, not points.** Ontario's 561k ranges would expand to 20.5M numbers, against 5.6M real addresses. In Canada 99.97% of them already have points, so they add nothing there.
> - **Drop ODA:** it adds 1.1% in Ontario and 0.2% in Quebec, and it is 2021 data. The reader stays in the builder.
> - NAR blockface-only points add 277k (5%) in Ontario, stored as approximate.
>
> Still open:
> - An aerial-imagery check of the conflict sample (`conflicts.tsv` has OSM links).
> - Memory: about 0.3 KB per input record (Ontario: 5.8 GB for 20.5M points and ranges; Ohio: 2.7 GB for 8.9M). All of North America in one process would not fit in 62 GB, so 0.5 should run `addr-spike` once per state and province and sum the results. A `--subdivision all` or region option would help.
> - Fuzzy name matching, and a decision on `unspecified` licenses (Phase 5).

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

> **Done 2026-10-24** (the commits that add `--band`, `--each`, `--routing graph` and `--clip-poly`).
> **Inputs:** Geofabrik `north-america-261001` (18.1 GiB), plus everything for the 66 US and Canadian subdivisions:
> - OpenAddresses: 2,193 sources, 9.6 GB.
> - TIGER: 3,223 of 3,225 counties, 3.5 GB.
> - NAR, ocean polygons and `.poly` boundaries.
>
> About 35 GB of inputs in all. The North America PBF took a day at Geofabrik's throttled 150–300 KB/s one evening and 30 minutes the next morning.
>
> **Missing:** 2 TIGER counties, Pearl River, MS (28115) and Sheboygan, WI (55121). The Census server answers them with an HTML "Request Rejected" page and status 200. `fetch` now checks zip and gzip magic bytes, so it fails on such pages instead of saving them. A third county saved that way the first time was fetched again.
>
> **Reproduce** (with `B`, `I=$OMP_WORK/inputs`, `M=$OMP_WORK/measure/na`, `NA=$I/osm/north-america-261001.osm.pbf`, and `cap` meaning `systemd-run --user --scope -p MemoryMax=56G -p MemorySwapMax=0`):
> ```
> $B fetch --region north-america --sources osm,poly,openaddresses,tiger,nar
> $B measure $I/osm/ohio-261001.osm.pbf    --json $M/ohio.json
> $B measure $I/osm/ontario-261001.osm.pbf --json $M/ontario.json --clip-poly $I/poly/ontario.poly
> $B measure $I/osm/quebec-261001.osm.pbf  --json $M/quebec.json  --clip-poly $I/poly/quebec.poly
> for k in 0 1 2; do   # band 0 also counts the routing graph
>   cap $B measure $NA --region north-america --band $k/3 --routing $([ $k = 0 ] && echo graph || echo none) \
>     --clip-poly $I/poly/north-america.poly --json $M/north-america-$k.json
> done
> cap $B addr-spike $NA --subdivision all --each --sources osm,openaddresses,nar,tiger --json $M/addr
> cap $B addr-spike $NA --subdivision all --each --sources osm,openaddresses,nar,tiger --exclude-license unspecified --json $M/addr-licensed
> $B report $M/ohio.json $M/ontario.json $M/quebec.json $M/north-america-*.json > $M/size-table-0.5.md
> $B report --sum US --sum-json $M/addr/sum-us.json $M/addr/US-*.json   # likewise Canada (CA-*) and North America (both)
> $B report $M/addr/US-OH.json $M/addr/CA-ON.json $M/addr/CA-QC.json $M/addr/sum-*.json > $M/addr-table-0.5.md
> ```
>
> **Size table, North America** (V4 tiles z0–z14; the tile, routing and search rows include Mexico and Greenland, which are in Geofabrik's extract; addresses cover the US and Canada):
>
> | Layer | Ohio | Ontario | Quebec | North America |
> |---|---:|---:|---:|---:|
> | Tiles V4 (MiB) | 134 | 579 | 906 | **9,456** |
> | — water / landcover / roads | 14 / 61 / 55 | 344 / 185 / 44 | 695 / 168 / 34 | 4,739 / 2,449 / 2,070 |
> | — boundaries, rail, POIs, places, ferries | 4 | 6 | 10 | 198 |
> | Tiles V5 / MVT + gzip (MiB) | 128 / 170 | 551 / 701 | 869 / 1,105 | 8,743 / 13,288 |
> | Routing graph nodes / edges | 2.2M / 5.3M | 1.3M / 3.0M | 0.76M / 1.8M | 62.5M / 149.6M |
> | CH, compact layout (MiB) | 115 | 66 | 40 | **about 3,276** (estimated, 1.50 CH edges per edge) |
> | Search: fst + postings + entries (MiB) | 6.6 | 5.6 | 4.3 | **253 or less** |
> | Addresses A3 + TIGER fallback ranges (MiB) | 9.2 + 1.9 | 10.0 | 7.6 | **315 + 50** |
> | **Total (MiB)** | 267 | 661 | 958 | **about 13,350 (13.0 GiB, 14.0 GB)** |
>
> - **Over budget, under the stop line.** The total is above roadmap §4's 6–10 GB and below the 15 GB at which Phase 0 says to revisit scope. Tiles are 2–3× the 3–5 GB estimate, and the CH is about 2–3× its 1–1.5 GB.
> - **Water is the lever.** At z14 (V1 bytes, all bands), lakes are 21.7% (14.1M features) and streams 18.2% (17.8M). Protected areas are 9.8% from only 32k polygons. `service` roads are 5.4% and driveways 3.5%. A lake minimum size, streams at z14 only or not at all, and simpler protected-area outlines are the first things to settle (the cut-offs 0.3 left open).
> - The ocean clip kept 2,145 pieces and dropped 14,820 that are in the bbox but outside `north-america.poly`.
> - Tile bytes add exactly across bands (checked on Quebec: V1 identical). V4/V5 differ by 0.2% or less because each band trains its own dictionaries; the dictionaries row shows band 0's.
> - Search is an upper bound: each feature is counted in one band, but the bands' `fst`s share tokens.
>
> **Addresses** (`addr-spike --each`, summed; full tables in `$M/addr-table-0.5.md` and `$M/addr-licensed-table-0.5.md`):
>
> | | US | Canada | North America |
> |---|---:|---:|---:|
> | Unique addresses, OSM only | 34.2M (25%) | 5.9M (37%) | 40.1M (26%) |
> | + OpenAddresses | 135.6M | 13.9M | 149.5M |
> | + NAR building points, then blockface-only points | – | 15.4M, 15.9M | **151.5M** |
> | Without sources whose license is `unspecified` | 71.4M | 15.4M | **86.8M** (57%) |
> | Units (separate table) | 24.9M | 7.1M | 32.0M |
> | Attached to an OSM street (200 m) | 88.6% | 92.2% | 88.9% |
> | Conflicts left at 250 m | 512k (0.4%) | 228k (1.4%) | 740k (0.5%) |
> | TIGER ranges with a point: OSM only → merged | 19.1% → 66.0% | – | – |
> | A3 total (MiB, bytes per address) | 284 (2.20) | 30.6 (2.02) | **315 (2.18)** |
> | TIGER fallback ranges (MiB) | 50 (12.6M ranges, 5.0 B each) | – | 50 |
>
> - The merged total matches roadmap §3's estimate of 150–200M. Ohio from the North America split (4,946,070) matches the 0.4 run on Ohio's own extract (4,946,534).
> - A3 is 2.18 B per address, above the test regions' 1.9–2.0. The US attaches worse (88.6% vs 94% in Ohio), and unattached points are the costliest side table (103 MiB, a third of A3). Fuzzy name matching in Phase 2 would lower both.
> - **Licensing still decides US coverage.** Without the `unspecified` sources the US keeps 53% of its addresses, and the share of TIGER ranges with a point falls from 66% to 35%.
> - 1.12M OSM address points lie outside every US and Canadian `.poly` (mostly Mexico) and are not counted.
>
> **Builder resources** (16 threads, 62 GB RAM; inputs and temp files on the exFAT HDD):
> - **One-pass `measure` does not fit.** It was killed by the 56 GB cap 5.5 minutes in, before its read finished.
> - **In 3 bands** (`--band k/3`: z6 columns with `x % 3 == k`, z0–z5 in band 0) it peaks at 42.7 / 40.6 / 40.4 GB and takes 15.0 + 9.7 + 9.7 = 34 minutes. Each band reads the whole PBF (about 340 s), and tiles go to a temp store of about 3.8 GiB per band (11.5 GiB in all, deleted afterwards).
>   - 1.62 billion needed nodes take 13.9 GiB of locations.
>   - The way-marking pass alone peaks at 14.7 GB, because `pass()` keeps every block's tag counters until the pass ends. That is the next memory to cut.
> - **`addr-spike --each`** over all 66 subdivisions takes 18.7 minutes and peaks at 30.8 GB, of which the shared OSM read is 2.2 minutes and 27.7 GB.
> - The roadmap's 16–32 GB desktop target is not met yet. Phase 5 needs features spilled to disk by z6 tile, or more bands. Bands trade time for memory: each re-reads the PBF, and none can go below the roughly 16 GB the node locations need.
> - **Node locations: needed-nodes-only.** The dense index is not run on North America: it would need about 108 GB of zero-filled writes on exFAT and only 24 GB is free on `/`.
>
> **The work to get here** (all checked against the 0.3 and 0.4 JSON; identical except where noted):
> - **Node locations:** the needed-nodes bitset has a rank index, and each location takes one 8-byte slot at its rank. Way refs are read again in a fourth pass instead of being held. Read-stage peak: Ohio 3.0 → 2.1 GB, Ontario 7.9 → 4.6 GB, Quebec 8.9 → 4.8 GB. `addr-spike` uses the same store.
> - **Tiling:** z0–z5 come from only the features shown there. From z6 down, each z6 tile is cut on its own. Every cell is byte-identical; Quebec's peak drops 7.4 → 5.8 GB.
> - **Bands:** `measure --band k/n` keeps only the features the band's tiles need, and `report` merges band runs of a region. Per-feature totals count each feature in the band its bbox starts in.
> - **Routing:** `measure --routing graph` counts graph nodes and edges in two extra passes over the ways, with no roads held. It matches the CH build exactly on Ohio (2,211,466 nodes, 5,281,643 edges). `report` estimates the CH from the regional runs' CH-edges-per-edge ratio (1.48–1.52).
> - **Ocean:** `measure --clip-poly` keeps only the ocean pieces that meet the extract's `.poly`. For Quebec that is 218 → 107 pieces (906 MiB V4, about 0.7% less).
> - **Addresses:** `addr-spike --subdivision all --each` reads OSM once. Each point and interpolation range goes to the first `.poly` that contains it, and each street to every subdivision within about 5 km. `report --sum` adds up the runs.
> - **Fetch:**
>   - `--sources poly`.
>   - Interrupted downloads resume, and the manifest is saved every 30 s, merged with the copy on disk.
>   - Magic-byte checks for zip and gzip files.
>   - Deleted OpenAddresses source definitions are recorded as `unspecified (source definition deleted upstream)`.
> - **Bug fixed:** area buckets of POIs made from closed ways were dropped when blocks were merged (0.3's parking note is corrected above). Another bug was fixed in `report`'s summing: row keys such as `zoom` and `block_kib` were added up.
>
> Still open:
> - The CH is estimated, not built. `fast_paths` does not fit for a continent, and Phase 3 builds its own.
> - Lake, stream, protected-area and `service` cut-offs (next: 0.6 or Phase 1).
> - The two blocked TIGER counties.
> - The `unspecified` license decision (before Phase 5).
> - Builder memory for a 16–32 GB machine (Phase 5).

Run after 0.3 and 0.4 have picked their candidate variants (at most two per layer).

- Check that `$OMP_WORK` is mounted. Fetch North America OSM, OpenAddresses for the US and Canada, all TIGER `ADDRFEAT` counties (about 3,200 files) and NAR (the Canadian source chosen in 0.2) for every province.
- Run `measure` and the address pipeline on all of it. Record wall time, peak RSS and peak temp disk: these are the first numbers for the Phase 5 `BUILDING.md` hardware section.
- `addr-spike` needs about 0.3 KB of RAM per input record, so run it per state and province (the extract can stay `north-america`) and sum the results.
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
5. **Canadian address source:** NAR. ODA is dropped: it adds 1.1% in Ontario and 0.2% in Quebec, and it is 2021 data (2026-10-23, see 0.2 and 0.4).
6. **Tile encoding candidate:** V4 (zstd 19 with per-layer, per-zoom-band dictionaries) for 0.5. V5 is kept in reserve only (2026-10-23, see 0.3).
7. **Tile index:** sparse and Hilbert-ordered, not dense offset arrays (2026-10-23, see 0.3).
8. **POIs:** keep every category in the whitelist; they are under 1% of tile bytes (2026-10-23, see 0.3).
9. **Address encoding:** A3 (per-street runs of number and 5 m offset deltas, zstd over blocks of streets, plus side tables for units, suffixes and unattached points). 1.9–2.0 bytes per address (2026-10-23, see 0.4).
10. **Address dedup:** same street, same direction set and same number within 250 m. NAR blockface points merge within 1 km. Attach to OSM streets within 200 m (2026-10-23, see 0.4).
11. **OSM `addr:interpolation`:** kept as ranges like TIGER, never expanded into points (2026-10-23, see 0.4).
12. **Node locations:** needed-nodes-only, as a rank-indexed bitset with one 8-byte slot per needed node (13.9 GiB for North America's 1.62 billion). The dense index is dropped (2026-10-24, see 0.5).

Still open, to be settled by Phase 0 data: the lake, stream, protected-area and `service` road cut-offs, which decide whether North America gets from about 13 GiB toward the 6–10 GB budget. Settle before Phase 5: how to treat OpenAddresses sources with no license name (most US address coverage depends on them).

## Out of scope for Phase 0

- Any rendering beyond the stub window.
- Real contraction hierarchy implementation (an off-the-shelf crate is used only for size numbers).
- Search ranking, the admin hierarchy, and query parsing.
- Planet build (extrapolation only).

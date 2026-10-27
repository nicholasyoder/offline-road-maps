# Refactor Plan: Code Review Fixes

*Draft 1, 2026-10-26. From a review of the whole workspace after phase 2. Done between phases 2 and 3, before routing adds to `build`. Each step below is sized for one session.*

## Purpose

Phase 2 left the code working and well tested (163 tests), but with three structural problems:

- **The production builder runs on Phase 0 spike modules.** `build` uses `measure::{osm, tiles, compress, water, poly, geom}`, and the address merge in [addr/merge.rs](../crates/builder/src/addr/merge.rs) imports its stages from [addr/mod.rs](../crates/builder/src/addr/mod.rs), which is mostly the `addr-spike` command and its reports. So a change to a spike report can break `build`, and it is hard to tell which code ships.
- **Shared logic is copied around.** Web Mercator math appears in five places, and the PBF pass helper in three. Douglas–Peucker, `put_str`, `read_at`, `invalid`, `mib` and finding a pack's first chunk each appear two to five times.
- **Builds are slow.** A North America `build --addresses all` takes 51 min ([phase 2 step 2.4](phase2-plan.md#24-address-store-in-build)), and dev loops on Ohio and Quebec take 40–90 s. The PBF is read about 18 times (see R4).

There are also two real bugs (R1) and two files too long to work in comfortably: [app/src/search.rs](../crates/app/src/search.rs) (1,732 lines) and [core/src/find.rs](../crates/core/src/find.rs) (1,786 lines, with an `impl Run` of about 900).

None of this changes the pack format or what the app shows.

### Exit criteria

1. **Same output.** A `--deterministic` Ohio build (and Quebec, for the steps that touch addresses or search) gives the same SHA-256 before and after every step, except R1 (which changes only failure behaviour). Phase 1 showed `--deterministic` builds are byte-identical across runs and band counts ([phase 1 step 1.3](phase1-plan.md#13-label-and-shield-data-in-the-builder)), so any difference is a change in behaviour.
2. **Same search.** `omp-builder search --check` gives the same numbers on Ohio and Quebec after R5 and R6, and warm p95 does not get worse.
3. **No copies.** Each piece of logic in the review list lives in one place (R2, R3).
4. **Faster builds.** North America `build --addresses all` takes at least 30% less wall time than the 51 min baseline (measured again at the start of R4), and its peak stays under the 56 GB cap. Ohio dev builds get faster too.
5. CI checks pass after every step (`fmt`, `clippy -D warnings`, `test`).

## Status

| Step | Status | Notes |
|---|---|---|
| R1 Bug fixes | **Done** 2026-10-26 | `build` writes into `<out>/.building` and swaps the chunks in on success; `--addresses` keeps each subdivision once; resumes send `If-Range` from `<file>.part.meta`; `Pack::open` checks section bounds without wrapping; `OMP_BENCH_NO_MAP` read once; `wanted` deduped with a `HashSet`. Delaware `--deterministic` pack byte-identical to `master`'s. |
| R2 Shared geometry and small helpers | **Done** 2026-10-26 | One Web Mercator in `omp_core::geo` (with the app's `0..1` form and `distance_m`); one Douglas–Peucker (`geom::simplify_kept`); `put_str`, `invalid`, `read_at`/`write_at`, `mib`, first-chunk lookup and the UI colours each in one place. Ohio and Quebec `--deterministic` packs byte-identical; renders pixel-identical; `search --check` unchanged. |
| R3 Builder structure: production code out of the spikes | **Done** 2026-10-26 | `addr-spike` retired, `measure` kept (decision 1). New `pbf/` (one `pass`), `geom.rs`, `poly.rs`, `tiles/`, `addr/pipeline.rs`; `build::run` is 91 lines of stages, with `build/meta.rs` and `build/report.rs`; one hidden `--measure-blocks`. Ohio and Quebec `--deterministic` packs byte-identical; `measure` JSON on Delaware unchanged. |
| R4 Build time | Not started | |
| R5 Split the app's search UI | **Done** 2026-10-26 | `search/` = `mod.rs` (view and distance helpers, re-exports), `worker.rs`, `session.rs`, `ui.rs`, `draw.rs` (icons, rows, marker); largest file 591 lines. The worker loop is generic over a small `Engine` trait, so four headless tests run it on a stand-in search. Ohio bench keystroke latency the same within noise (p95 49.8/49.6 ms before, 54.0/48.3 after). |
| R6 Split the search engine | Not started | |

## Order of work

```
R1 bug fixes ─► R2 shared geo + helpers ─► R3 builder structure ─► R4 build time

R5 app search split    (independent; any time)
R6 core find split     (independent; any time, best after R2)
```

- R2 comes before R3 because R3 moves `measure/geom.rs`, and that is far easier once its copies of core's projection are gone.
- R3 comes before R4: R4 rewrites how the PBF readers are called, and that is easier once the readers live in one production module.
- R5 and R6 don't touch the builder. Either can go between any two builder steps.

## How to check a step

Before changing anything, build the baseline packs from `master` with `--deterministic`:

```
omp-builder build $OMP_WORK/inputs/osm/ohio-*.osm.pbf --out $OMP_WORK/refactor/base/ohio \
    --addresses US-OH --deterministic
omp-builder build $OMP_WORK/inputs/osm/quebec-*.osm.pbf --out $OMP_WORK/refactor/base/quebec \
    --addresses CA-QC --deterministic
sha256sum $OMP_WORK/refactor/base/*/*.omp.*
```

(Use the input paths the phase 2 packs were built from.) After the change, build again into `$OMP_WORK/refactor/new/` and compare the hashes. If they differ, `omp-builder inspect` on both shows which sections changed. App-only steps (R5) are checked with the tests, the bench and `--render` snapshots instead.

---

## R1 Bug fixes

> **Done 2026-10-26** (the commit "Refactor R1: bug fixes"). What was done, and how it was checked:
> - **Build swap:** the chunks are written into `<out>/.building` (inside `--out`, so always the same filesystem, and two levels below an app packs folder, which the app does not scan). After `Writer::finish`, `replace_pack` renames them over the old chunks, then deletes the old pack's extra chunks. The next build clears a failed build's `.building`. Unit test `a_failed_build_keeps_the_old_pack`; live check on Delaware: a rebuild killed at 3 s left the old pack byte-identical and openable by `inspect`.
> - **Subdivisions:** `Subdivision::parse_list`, shared by `build --addresses` and `addr-spike` (which had the same bug), keeps each subdivision once, where it first appears. So `all` alone gives `Subdivision::all()`'s order, and any list without repeats gives the same order as before. Unit test `subdivision_lists_keep_each_once`.
> - **Resumed downloads:** the first response's strong `ETag` (else `Last-Modified`) is kept in `<file>.part.meta` and sent as `If-Range`; a part with no `.meta` (from an older builder, or a server with no validator) is not resumed. The `.meta` is deleted when the download completes. Unit test `resumes_only_the_same_version` against a local HTTP server: same version resumes (one `Range` request), changed version starts over, no validator starts over.
> - **`Pack::open`:** `chunks × capacity`, `offset + len` and `offset % capacity + len` are checked; `Pack::read` checks `local + len` too. Unit test `refuses_section_bounds_that_overflow` crafts a TIDX length of `u64::MAX` with valid checksums.
> - **App:** `OMP_BENCH_NO_MAP` is read once into `MapState::no_map` when a pack opens (not into `Bench`, since it works without the bench too); `wanted` uses a `HashSet` beside the `Vec`, order kept.
> - Output unchanged: Delaware `--deterministic` gives the same SHA-256 (`ec08d9be…`) with `master`'s builder and this one.

Small and independent. One session, one commit per fix or one for all.

1. **`build` deletes the old pack before it starts** ([build/mod.rs `remove_old_chunks`](../crates/builder/src/build/mod.rs)). A North America build that fails after 50 minutes leaves no pack, or partial chunks. **Fix:** write the chunks into a temp directory next to `--out` (same filesystem, so a rename works), and only after `Writer::finish` succeeds remove the old chunks and rename the new ones in. A failed build leaves the old pack as it was, and the next build clears the temp directory.
2. **Repeated subdivisions in `--addresses`.** `subs.dedup()` only removes neighbouring duplicates, so `--addresses US-OH --addresses all` merges Ohio twice. **Fix:** sort and dedup, or collect into a `BTreeSet`; keep the order `Subdivision::all()` uses so the output is unchanged.
3. **A resumed download can join two versions of a file** ([fetch/http.rs](../crates/builder/src/fetch/http.rs)). The `Range` request has no `If-Range`. Geofabrik files are safe, since their md5 is checked, but OpenAddresses and TIGER files have no hash. **Fix:** keep the `ETag` (or `Last-Modified`) of the first response in a small `<file>.part.meta`, and send it as `If-Range` when resuming. A server that answers 200 starts the file over, which the code already handles.
4. **Small issues worth fixing while here:**
   - `Pack::open` adds `s.offset + s.len` without a checked add ([pack.rs](../crates/core/src/pack.rs)). A crafted file wraps in release builds. Use `checked_add`, and reject the file on overflow.
   - `draw` reads the `OMP_BENCH_NO_MAP` environment variable every frame ([app.rs](../crates/app/src/app.rs)). Read it once, into `App` or `Bench`.
   - `draw` builds `wanted` with `Vec::contains` in a loop (O(n²) in the tile count). Use a `HashSet` beside the `Vec`, keeping the order.

   Left alone on purpose: once over budget, `Cache::evict` sorts the whole cache on every insert. At today's tile counts (a few thousand at most) it doesn't show up in the bench. Revisit only if it does.

**Done when:** a `build` killed partway through leaves the old pack openable; `--addresses US-OH --addresses all` merges Ohio once; a resumed download whose server copy changed starts over; `Pack::open` rejects a section table whose offset plus length overflows (unit test); CI passes.

## R2 Shared geometry and small helpers

> **Done 2026-10-26** (the commit "Refactor R2: shared geometry and small helpers"). What was done, and how it was checked:
> - **Projection:** `omp_core::geo` now has `WORLD`, `EARTH_CIRCUMFERENCE_M`, `EARTH_RADIUS_M` (pub), `project_e7` (moved from the builder), the app's form as `Unit = [f64; 2]` with `lonlat_to_unit`/`unit_to_lonlat` and `to_unit`/`from_unit`, and the haversine `distance_m`. `project` and `unproject` are written on top of the unit form, with the same operations as before, so the `u32` output is unchanged. [measure/geom.rs](../crates/builder/src/measure/geom.rs) re-exports core's `P`, `project`, `project_e7`, `unproject` and `metres_per_unit`, so its callers did not change. The app's `camera::lonlat_to_world`/`world_to_lonlat` and `search::world`/`to_p`/`distance_m` are gone; callers use `geo::*`. The app's y used to be `0.5 − ln(…)/2π` and is now core's `(1 − ln(…)/π)/2`: equal up to the last bit, and the renders below are pixel-identical. The camera's round-trip test moved to core's `projection_round_trips`. `scale.rs` and `find::view_scale_m` use `EARTH_CIRCUMFERENCE_M`; the latter had `40_075_016.0`, and `search --check` gives the same results with `.686`.
> - **Douglas–Peucker:** `geom::simplify_kept` (generic over `u32`/`i32` points, returns kept indices) is the one implementation, using `geom`'s single `seg_dist2` (the integer copy is gone too); `geom::simplify` and the street table call it. Street sections stay byte-identical.
> - **Helpers:** `dist` from core in `street/`; `omp_core::varint::put_str` for the street, admin and address encoders and core's search tests (`pack.rs` keeps its own header `put_str`, a different format: a `u8` length, at most 255 bytes); `omp_core::pack::{read_at, write_at}` are `pub` and `measure/compress.rs` uses them; `omp_core::varint::invalid` is `pub` and the only copy (`pack.rs` and the builder's encoders use it); one builder `mib` in `main.rs` (`report.rs`'s, which formats text, is renamed `mib_text`); `omp_core::pack::{is_first_chunk, first_chunks}`, used by `build::first_chunk` (still errors on several packs) and `app::packs::find` (still looks one level down).
> - **Theme:** [app/src/theme.rs](../crates/app/src/theme.rs) holds `INK`, `WEAK` and `APPROX` (`app.rs` had a third `INK`, now also from there).
> - **Checks:** Ohio (`12c70434…`) and Quebec (`df133861…`) `--deterministic` packs have the same SHA-256 with `master`'s builder and this one. `omp-app --render` of Ohio at Columbus, z5, z10 and z15: decoded pixels identical (the PNG files differ only in their compressed bytes). `search --check --level 9` with all the query sets: the same pass counts on both packs. CI checks pass.

Mechanical: no behaviour change, and the deterministic packs stay byte-identical. One session.

1. **One Web Mercator in `omp-core`.** [core/geo.rs](../crates/core/src/geo.rs) becomes the only home of the projection and the distances:
   - Delete the copies of `WORLD`, `MAX_LAT`, `EARTH_CIRCUMFERENCE_M`, `project`, `unproject` and `metres_per_unit` from [builder measure/geom.rs](../crates/builder/src/measure/geom.rs), and re-export or call core's. Keep `project_e7` if it is used, moved to core.
   - Add the app's normalised f64 form to core (`to_unit`/`from_unit`, or `lonlat_to_world`/`world_to_lonlat` on `[f64; 2]`), with converters to and from `P`. Use them from [app camera.rs](../crates/app/src/camera.rs) and [app search.rs](../crates/app/src/search.rs) (`world`, `to_p`).
   - Move the haversine `distance_m` from app `search.rs` to core. Make [app scale.rs](../crates/app/src/scale.rs) and the literal `40_075_016.0` in [core find.rs](../crates/core/src/find.rs) use core's constant. Check that the f32/f64 rounding is the same where the app's maths moves to core's.
2. **One Douglas–Peucker.** [measure/geom.rs `simplify`](../crates/builder/src/measure/geom.rs) returns points, and [street/mod.rs `simplify`](../crates/builder/src/street/mod.rs) returns kept indices. Keep the indices form as the core of it, and have the points version call it. The two give the same result today, since both use the same segment distance and tie-breaking; the street sections must stay identical to prove it.
3. **Small helpers:**
   - `dist`: delete the copy in [street/mod.rs](../crates/builder/src/street/mod.rs) and use `omp_core::geo::dist`.
   - `put_str`: one copy for the three builder encoders (street, admin, addr), e.g. in a builder `encode_util` module, or next to `omp_core::varint::put` if core's tests want it too.
   - `read_at`: make core's `pack::read_at`/`write_at` `pub` (or put them in a small `omp_core::io` module), and delete the copy in [measure/compress.rs](../crates/builder/src/measure/compress.rs).
   - `invalid`: one `pub(crate)` copy in core (`varint::invalid` already exists, so `pack.rs` uses it). The builder's in [street/encode.rs](../crates/builder/src/street/encode.rs) moves to core or stays as the builder's one copy.
   - `mib`: one builder copy (four today). The app keeps its own, since the crates share no utility module and one tiny function isn't worth one.
   - First chunk of a pack: `build::first_chunk` and `app::packs::find` both look for `*.omp.000`. Move the shared part (is this a first chunk; the first chunks in a directory) into `omp_core::pack`, and keep each caller's policy (the builder errors on several packs; the app also looks one level down).
4. **App UI colours.** `INK`, `WEAK` and `APPROX` are defined in both [here.rs](../crates/app/src/here.rs) and [search.rs](../crates/app/src/search.rs). Put them in one `app/src/theme.rs`, which phase 5's map styles and settings will grow.

**Done when:** `grep` finds each of these once; Ohio and Quebec `--deterministic` packs are byte-identical to the baseline; `--render` snapshots of Ohio at z5, z10 and z15 match the baseline pixel for pixel; CI passes.

## R3 Builder structure: production code out of the spikes

> **Done 2026-10-26** (the commit "Refactor R3: builder structure"). What was done, and how it was checked:
> - **Decision:** keep `measure`, retire `addr-spike` ([decision 1](#decisions)).
> - **Modules:** `pbf/` holds `pass`, `header_bounds`, `nodes.rs` (was `measure/nodes.rs`) and `features.rs` (was `measure/osm.rs`, the whitelisted feature reader); `geom.rs` and `poly.rs` are top-level (`poly.rs` went there, not into `tiles/`, since the address merge uses it too); `tiles/` holds the pyramid cut and store (`mod.rs`), the layer encodings (`encode.rs`), dictionary training and the compression variants (`compress.rs`) and `water.rs`. `measure/` keeps only the command, `routing.rs` and `search.rs`. `peak_rss_mib` moved to `main.rs` next to `mib`. The moves are `git mv`s, so history follows the files.
> - **Addresses:** `addr/pipeline.rs` holds the merge stages (`select`, `extract`, `normalize`, `attach_records`, `dedup`, `clusters`, `fallback_ranges`) and their types; `addr/mod.rs` is only the module list. With the spike gone, its measuring went too: the encodings A1–A4 (`addr/encode.rs`), the town and street-table checks (`addr/towns.rs`, `addr/street_check.rs`), the postcode report, the samples, the per-source counters in the readers (`OsmStats`, NAR, ODA, OpenAddresses and TIGER stats), the dedup statistics at five distances, and `addr/osm.rs`'s unindexed `split`. Nothing that decides which record is kept, which cluster it joins or which street it attaches to changed.
> - **One `pass`:** `pbf::pass(path, init, f)`, with `f` returning `Result`. The copies in `addr/osm.rs`, `admin/read.rs` and `measure/osm.rs` are gone, and search's `read_alts` uses it too. `features.rs` keeps a one-line `blocks` that only picks its block type. `count_road_graph` (for `measure --routing graph`) moved to `measure/routing.rs` as `count_graph`, built on a new `features::road_ways`, so `pbf` imports nothing from `measure`.
> - **`build::run`:** 91 lines: `Setup::new`, admin, `read_bands`, `addresses_and_streets`, `search_index`, `meta::meta`, `write_pack`, then the summary. Each stage frees what it no longer needs, as before; the admin sections are now encoded before the tiles are written, so `Admin` is freed earlier. `build/meta.rs` holds the provenance, attribution and settings; `build/report.rs` the stage timer, `ChainStats`, `ZoomTotals` and the `print_*` functions. `build` imports nothing from `measure`.
> - **Measurement options:** `--address-block-kib`, `--search-block-kib` and `--grid-zoom` are replaced by one hidden `--measure-blocks`, which also prints the address and `SENT` sizes at 4 and 32 KiB blocks and the grid at z13 and z15 (the alternatives phase 2 compared). The pack always uses the constants (16 KiB, z14). Checked on Delaware.
> - **`report`:** keeps the `measure` tables; `--sum`, `--sum-json` and `--size-table --addr` are gone, and it refuses an `addr-spike` JSON with a message.
> - **Checks:** Ohio (`12c70434…`, 37 s, 6.0 GiB) and Quebec (`df133861…`, 88 s, 8.3 GiB) `--deterministic` packs have the same SHA-256 as R2's. `measure` on Delaware, default and `--routing graph`: the same JSON as the old builder's except timings, and `report` gives the same tables. CI checks pass (164 tests).
> - Docs: no current doc (CLAUDE.md, format.md, README) named the moved files; the phase plans keep their paths as history.

Moves and splits, with no behaviour change. One session; probably the largest diff of the plan, but mostly moved code.

1. **Decide first, with the owner: do the spike commands stay?** `measure` and `addr-spike` produced the Phase 0 size tables and the address coverage numbers, and `report` turns their JSON into Markdown. Options:
   - **Keep them** as thin commands over the production modules. Their reports stay useful when a format change needs measuring again (phase 3 will want routing size estimates, which `measure` has).
   - **Retire them**, and delete `measure/{routing,search,encode variants}`, the `addr-spike` report rows and `report`.

   *Recommended:* keep `measure` for its routing estimate until phase 3 has a real graph, and retire `addr-spike` and its report code, since `build --addresses` prints the same coverage numbers. Record the decision in this plan.
2. **New production modules** in `omp-builder` (names to settle during the session):
   - `pbf/`: the shared parallel pass (R3.3), `header_bounds`, the needed-node sets (`measure/nodes.rs`) and the whitelisted feature reader (`measure/osm.rs`).
   - `geom.rs`: what is left of `measure/geom.rs` after R2 (clipping, ring assembly, centroids, simplification).
   - `tiles/`: `measure/tiles.rs` (pyramid cut, store, bands), `measure/compress.rs` (dictionary training) and `measure/water.rs`, `measure/poly.rs`.
   - `addr/`: split [addr/mod.rs](../crates/builder/src/addr/mod.rs) into `addr/pipeline.rs` (`select`, `extract`, `normalize`, `attach_records`, `dedup`, `clusters`, `fallback_ranges`, `Interner`, `Raw`, `Src`, the row types the merge returns) and, if kept, `addr/spike.rs` (the `addr-spike` command, its reports and samples).
   - `measure/` keeps only the spike command, its variants and its report types, if it stays.
3. **One PBF pass helper.** [measure/osm.rs](../crates/builder/src/measure/osm.rs), [addr/osm.rs](../crates/builder/src/addr/osm.rs) and [admin/read.rs](../crates/builder/src/admin/read.rs) each define `pass`; the last two are identical, and [search/mod.rs](../crates/builder/src/search/mod.rs) borrows admin's. Replace them all with `pbf::pass(path, init, f)`, which returns `Result` from `f` (measure's version needs that). R4 builds on this one function.
4. **Split `build::run`** (about 400 lines in [build/mod.rs](../crates/builder/src/build/mod.rs)):
   - stage functions: `read_bands`, `addresses_and_streets`, `search_index`, `write_pack`, each taking what it needs and returning what the next needs, so the order of `drop`s that keeps the peak under the cap stays visible in `run`;
   - a `build/report.rs` for the `print_*` functions, `ChainStats`, `ZoomTotals` and the stage timer;
   - a `build/meta.rs` for `META`'s settings, sources, snapshot and attribution.

   The hidden measurement-only options (`--address-block-kib`, `--search-block-kib`, `--grid-zoom` with more than one value) re-encode whole sections just to print sizes. Move them behind one `--measure-blocks` flag, or into `measure` if it stays, so `run` holds only the pipeline.
5. Update the module docs and the paths in the phase plans' code notes that point at moved files. The plans' history stays as written, but CLAUDE.md and format.md should name the right files.

**Done when:** `build` imports nothing from `measure` or from the `addr-spike` command; there is one `pass`; `build::run` reads as a list of stages (under about 100 lines); Ohio and Quebec `--deterministic` packs are byte-identical to the baseline; the spike commands that stay still run on Delaware; CI passes.

## R4 Build time

Measure, then cut PBF passes, then make dev loops cheaper. One session, maybe two if R4.4 grows. Builds on R3's single `pbf::pass`.

### Where the time goes today

A `build --addresses all --bands 3` reads the whole PBF about **18 times**, decompressing every blob each time:

| Reader | Passes | Notes |
|---|---|---|
| `admin::load` | 3 | relations; their member ways; nodes |
| `osm::read`, sparse strategy, per band | 4 × 3 bands = 12 | relations; ways (mark needed nodes); nodes; ways again (resolve) |
| `addr::osm::read` | 2 | ways; nodes |
| `search` `read_alts` | 1 | alternative names of the features kept |

The phase 2 stage timings are a starting point (bands up to 44.5 GiB, OSM address read 2.6 min, merge 14.0 min, street table 2.3 min). Two things are not known yet: how the band time splits between decompression and work, and how long each pass takes.

### Steps

1. **Measure.** Time every pass (not only every stage) on Ohio and North America, with the blob decode time apart from the per-block work. Record the table here. Get the North America baseline wall time and peak with today's code (the 51 min is from before the step 2.5–2.7 rebuilds).
2. **Read only the blobs a pass needs.** Planet and Geofabrik PBFs are sorted: node blocks, then ways, then relations. Index the file once (each blob's offset and the element type it holds), which costs one decode of each blob, or less where the blob header says enough. Then a relations pass reads only the relation blobs (a few percent of the file), and a ways pass only the way blobs. `osmpbf`'s `IndexedReader` does this; use it or copy its idea into `pbf::pass` behind a `Kinds` filter. This should cut the 18 passes' decode cost by more than half without changing any reader's logic.
3. **Share passes across readers and bands:**
   - Relations are read by `admin::load` and by each band's `osm::read`, and the relation index doesn't depend on the band (check `relations_block`). Read the relations once and hand the index to every band.
   - Band 0's node pass, or one pass of its own, can also collect the admin nodes. That would fold `admin::load`'s node pass into band 0, if the memory stays under the cap.
   - Fold `read_alts` into the band reads: the features are known there, so their alternative names can be kept as they are read, rather than in another full pass at the end.
   - `addr::osm::read` walks the same ways and nodes as the bands. Folding it in is possible, but only if the peak stays under 56 GB; the bands are already at 44.5 GiB. Try it last, and measure.
4. **Faster dev loops.** Most dev changes touch one stage (tiles, the street table, search or addresses), yet every build redoes all of them. Add a stage cache under `<temp>/cache/`, keyed by the input file's size, mtime and hash, and by the builder version and options that stage depends on:
   - the admin areas (`Admin`, already serialisable for `SADM`);
   - the merged addresses (`merge` output), the largest saving: 14 min on North America;
   - the band tile store (`*.v2`), when the whitelist and the tile options are unchanged.

   Use the cache only when asked (`--reuse-stages`), so release builds always run everything. This is for dev loops and does not count towards the 30% target.
5. If time is left: profile the merge (14 min) for easy wins, such as allocation in `normalize` and the dedup clustering.

**Done when:** the pass and stage timings are recorded here before and after; North America `build --addresses all` wall time is at least 30% below the R4.1 baseline, with peak under 56 GB; Ohio and Quebec `--deterministic` packs are byte-identical to the baseline; a second Ohio build with `--reuse-stages`, after a change that touches only search, skips admin, addresses and tiles; the roadmap's build-cost notes are updated.

## R5 Split the app's search UI

[app/src/search.rs](../crates/app/src/search.rs) holds the worker thread, the session and result merging, the geometry helpers and the egui widgets in one 1,732-line file. One session.

Split into a `search/` module:
- `search/worker.rs`: `Query`, `Answer`, `Shared`, `Slot`, `Worker`, `work`, `stopper`. These touch no egui, so they can be tested headless: a query answered, a stale answer dropped, a level stopped by a new keystroke, "what's here" served between levels.
- `search/session.rs`: `Session`, `Results`, `Taken`, `merge`, `remember`.
- `search/ui.rs`: `SearchUi`, `Icons`, `draw_marker` and the input helpers (`consume`, `cursor_to_end`, `one_line`, `box_id`).
- `search/mod.rs`: re-exports what `app.rs`, `here.rs` and `bench.rs` use, so their imports barely change.

The geometry helpers (`world`, `to_p`, `distance_m`) will have moved to core in R2; `view_for` and `distance_text` stay in the app.

**Done when:** no file in `search/` is over about 600 lines; the worker has headless tests for the four cases above; the bench's search phase gives the same keystroke latencies (within noise) on Ohio; CI passes.

> **Done 2026-10-26** (the commit "Refactor R5: split the app's search UI"). What was done, and how it was checked:
> - **Modules:** as planned, except that the painting went to its own `search/draw.rs` (`Icons`, `Row`, `row`, `one_line`, `draw_marker`, the pin colour): with it, `ui.rs` was about 800 lines. `row` is now a free function taking the icons. `view_for` and `distance_text` (with their tests and the zoom constants) are in `search/mod.rs`, which re-exports `Icons`, `draw_marker`, `Taken`, `BenchKey`, `SearchUi` and `Status`, so `app.rs`, `here.rs` and `bench.rs` did not change. Each constant moved to the file that uses it (`PAUSE` to the worker; `SUGGESTIONS`, `RESULTS`, `RECENT` to the session). The tests of `worker.rs` and `ui.rs` are in `worker_tests.rs` and `ui_tests.rs` (child modules through `#[path]`, like core's `search_tests.rs`); shared test hits and answers are in `mod.rs`'s `testing`. Sizes: `ui.rs` 591, `worker.rs` 496, `session.rs` 291, `draw.rs` 251, `worker_tests.rs` 219, `mod.rs` 137, `ui_tests.rs` 127 lines. `search.rs` became `ui.rs` with `git mv`.
> - **Headless worker:** `work` opens the pack and hands the `Search` to `serve`, the loop, which is generic over a private `Engine` trait (`start`, `whats_here`, `heap_bytes`, `bytes_read`) and its `Levels` (`find::Session`'s `stop_when`, `set_limit`, `hits`, `level`, `heap_bytes`), implemented for `Search` and `find::Session` by calling their own methods. There was no way to build a searchable pack in the app's tests, and a stand-in is what makes a level that runs until stopped possible. `Worker::spawn` starts the thread for both. The four tests run the real loop on a stand-in whose level `d` of "text" finds "text d", and one chosen level runs until its stop check fires: a query answered level by level after the pause, then Enter going on with the same session; a stale answer dropped (the old query's answers all refused by `Session::take`); a level stopped by a new keystroke sending nothing; "what's here" answered while a level runs, which then runs again. Passed 30 runs in a row.
> - **Checks:** Ohio bench (`ohio.toml`, release, warm, alternating master and the split twice each): keystroke latency over 153 keystrokes mean 18.1/18.1 ms → 18.3/17.7, p50 10.3/9.8 → 9.6/9.8, p95 49.8/49.6 → 54.0/48.3, first keystroke 22.7/21.1 → 21.3/20.9; every search step's top results the same. JSON in `$OMP_WORK/refactor/r5/`. `search --check` was not rerun: R5 touches only the app. CI checks pass (168 tests).

## R6 Split the search engine

[core/src/find.rs](../crates/core/src/find.rs) is 1,786 lines, and its `impl Run` is about 900 of them. [Phase 2 decided](phase2-plan.md#27b-progressive-search) not to *rewrite* `find` to avoid repeated work across levels; that decision stands. This step only reorganises the code so it can be read and changed. One session.

Split `find.rs` into a `find/` module along the stages its module doc already describes:
- `find/mod.rs`: `Request`, `Hit`, `HitKind`, `Search::find`/`start`, `Session` and its levels.
- `find/readings.rs`: the readings of a query (state, locality and name splits, at a comma if there is one), and `qtoks`, `join`.
- `find/candidates.rs`: postings, intersection, `union`, `within`, the region intervals and the bounds per level.
- `find/score.rs`: `quality`, `match_name`, `kind_bonus`, `place_radius_m`, distance and importance scoring.
- `find/numbers.rs`: house numbers resolved to addresses and ranges, and `number_text`.
- `find/text.rs` (or into `score.rs`): `titlecase`, `shown_name`, dedup by `same`.

Split `Run` itself by stage too: its methods move into the files above as `impl Run` blocks, or as free functions taking the `Run`'s caches, so each file has one stage of the pipeline. If, while splitting, a pass turns out simpler to express differently, record it as a follow-up rather than mixing it into the move.

[search_tests.rs](../crates/core/src/search_tests.rs) (1,101 lines) can move to `find/tests.rs`, split the same way if that helps.

**Done when:** no file in `find/` is over about 500 lines; `cargo test` and `omp-builder search --check` on Ohio and Quebec give the same numbers as before; warm p95 is unchanged within noise; CI passes.

---

## Decisions

1. **Spike commands (R3.1, 2026-10-26).** `measure` stays, as a thin command over the production modules, for its routing estimate until phase 3 has a real graph, and `report` stays for its JSON. `addr-spike` is retired, with its reports, its samples, and the address half of `report` (`--sum`, `--sum-json`, `--size-table --addr`), since `build --addresses` prints the same coverage numbers. Phase 0 and 2 runs of `addr-spike` stay in the plans as history; rerunning them needs a builder from before R3.

## Out of scope

- The search ranking items carried from phase 2 ([phase 2, carried](phase2-plan.md#carried-to-later-phases)): they change results, so they belong in their own step.
- Format changes of any kind.
- Routing (phase 3), though R3's structure and R4's shared passes are meant to make its graph build fit in easily.

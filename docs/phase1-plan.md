# Phase 1 Plan: Map Viewer MVP

*Draft 1, 2026-10-24. Expands Phase 1 of [roadmap.md](roadmap.md#phase-1-map-viewer-mvp). Builds on the format in [format.md](format.md) and the measurements in [phase0-plan.md](phase0-plan.md).*

## Purpose

Phase 1 turns the phase 0 spike into a real pipeline: the builder writes a **real pack** with map tiles, and the app **draws it**. When it is done, someone can copy a North America pack and the app to a USB stick, plug it into an ordinary laptop, and browse the whole continent: roads, water, land cover, places, POIs, boundaries, labels and highway shields. Search, addresses and routing are phases 2 and 3; nothing here should get in their way.

### Exit criteria (expanded)

The roadmap's exit criterion is "browse all of North America from a USB stick at a smooth frame rate with less than 300 MB RAM." In numbers, measured with the scripted flight from step 1.4 (`omp-app --bench`) over the North America pack:

1. **Smooth.** On the dev machine, p95 frame time under 16.7 ms (60 fps) while panning and zooming at every zoom. On the 2012-or-newer laptop, p95 under 33 ms (30 fps). Frames never wait on disk: tiles that are not loaded yet are drawn from a parent tile until they arrive.
2. **From a USB stick, cold.** With the pack on a USB stick and the OS cache dropped:
   - start-up to first full frame under 3 s;
   - after a jump to a new place, the viewport is fully detailed within 2 s on USB 2 (target 1 s on USB 3);
   - the bench prints the bytes read and the read latency, so a slow result can be pinned on the drive or on the app.
3. **Memory.** Peak RSS under 300 MB after the whole flight, including the GPU driver's share as far as the OS reports it. The tile cache has a byte budget, and pack data is read with positioned reads, not memory-mapped, so the page cache does the caching and RSS stays honest (decision 1).
4. **Idle.** Close to 0% CPU when nothing moves: no repaint without input or newly arrived tiles.
5. **Size.** The app stays under 30 MB stripped on Linux and Windows, fonts and icons included.
6. **Platforms.** The same checks pass on the Windows PC, and the app starts and is usable on the 2012-or-newer laptop. The pre-2012 laptop is tried once; if it needs more than the wgpu GL backend, it is recorded and skipped for now (decision 2).
7. **Looks like a road map.** Labels don't overlap or flicker while panning, roads have casings and draw in the right order, and Interstate, US, state and provincial routes show shields. Judged by screenshots at fixed spots (step 1.5's `--render`), kept in the repo for comparison.

## Status

| Step | Status | Notes |
|---|---|---|
| 1.1 Shared tile codec and pack reader | **Done** 2026-10-24 | `omp-core::{tile, tiles, schema, meta, varint}`; `measure` output on Ohio unchanged. |
| 1.2 `omp-builder build`: write a tile pack | **Done** 2026-10-24 | Delaware, Ohio and North America packs; North America is 8.1 GiB in 3 chunks, built in 24 min at 43 GiB peak. |
| 1.3 Label and shield data in the builder | **Done** 2026-10-24 | Whitelist v2: state and water labels, a `labels` layer of polylabel points, `routes` for shields, merged lines, one tile per point. North America tiles −297 MiB (−3.6%); pack 8,001 MiB in 2 chunks. |
| 1.4 App skeleton: pack, camera, tile loading, bench | **Done** 2026-10-24 | Tile outlines, loader pool, 96 MiB cache, F3 overlay, `--bench`. North America flight: 0.45 ms p95 frame (outlines only), RSS peak 212 MiB, worst cold jump 132 ms on SATA SSD. |
| 1.5 Geometry renderer | **Done** 2026-10-24 | wgpu fills (lyon) and screen-width lines with casings, per-tile scissor, MSAA 4×, `--render`. Ohio flight 0.69 ms p95 frame; North America RSS peak 162 MiB. Ocean pieces were being dropped by the builder: fixed, North America pack now 9,200 MiB. |
| 1.5b Leave out full-ocean tiles | **Done** 2026-10-24 | Format 0.2: a missing tile is drawn from its nearest stored ancestor. Tiles that one polygon covers are left out (40.1M ocean, 5.6M protected area). North America 8,026 MiB (was 9,200), 17.9M tiles (was 64.0M); renders unchanged. |
| 1.5c Tile size experiments: frame overhead, per-layer max zoom | In progress | Both measured on Quebec and North America (`size-experiments`, `--render --cap`). North America: F1+F2 −253 MiB, no decode cost; F3 and F4 not worth it; caps −743 MiB (land cover to z13) to −1,654 MiB (land cover and water areas to z13). **Waiting for the owner's decision**; then format, builder, app, rebuild. |
| 1.6 Style file and default style | Not started | |
| 1.7 Text and label placement | Not started | |
| 1.8 Shields and POI icons | Not started | |
| 1.9 Viewer UI | Not started | |
| 1.10 Performance, USB and hardware checks | Not started | Needs the USB stick, the Windows PC and both laptops. |

## Environment notes

As in [phase 0](phase0-plan.md#environment-notes): builder data in `$OMP_WORK` on the exFAT disk, `target/` on `/`. Packs are written to `$OMP_WORK/packs/`. For the app, put a pack under `maps/` next to the executable, or pass `--pack <dir>`.

**Bench runs:** `omp-app --bench crates/app/bench/<flight>.toml --pack $OMP_WORK/packs/<region> --out $OMP_WORK/measure/bench/<name>.json`, from a release build. For a cold run without `sudo`, remount the work disk first (`udisksctl unmount -b /dev/sda1 && udisksctl mount -b /dev/sda1`), which drops its page cache. The bench opens its own window; on Linux it uses X11 so frames are not paced by the Wayland compositor.

**Hardware for 1.10** (owner, 2026-10-24): a Windows PC, a laptop from 2012 or later, and one from before 2012 (low priority). The **USB stick** is needed only in 1.10, plus an optional early check after 1.5. It should be at least 16 GB, formatted FAT32 so the pack's chunks are tested for real. A USB 2 stick, or a USB 3 stick in a USB 2 port, gives the worst-case numbers.

**Visual checks without a screen:** step 1.5 adds `omp-app --render`, which draws a view to a PNG offscreen. That is how renderer and style changes are reviewed in a session, and how the reference screenshots are made.

---

## 1.1 Shared tile codec and pack reader

The V2 payload encoder and decoder, the Hilbert curve and the tile index are in `omp-builder` (`measure::encode`, `measure::compress`). The app needs the same code, and roadmap §5 puts format code in `omp-core` so the two cannot disagree.

- Move to `omp-core::tile`: the V2 layer payload (encode and decode, as in [format.md](format.md#layer-payload-encoding-v2)), `hilbert(z, x, y)`, the zoom bands, and the `TIDX` block encoding.
- `omp-core::schema`: read and write `SCHM` (layers, classes, props). `omp-core::meta`: read and write `META`.
- `omp-core::tiles::TileReader` on top of `pack::Reader`: load `TDIC` at open, look up `(z, x, y)` in `TIDX` (binary search of the zoom directory, then one 256-tile block, cached), read one tile record from `TDAT` with a positioned read, and decompress each layer frame with its dictionary. It returns decoded features in tile-local coordinates.
- Reads go through `pread`-style calls (`FileExt::read_at` / `seek_read`), not `mmap`, so pack pages count as page cache, not as the app's RSS (decision 1). `TIDX` is about 110 MiB for North America, so it is read block by block, not held whole.
- The decoder works on borrowed bytes and does not allocate per point.

**Done when:** the builder's `measure` uses the moved code with byte-identical output on Ohio, and `TileReader` round-trips tiles written by a test writer (all geometry types, props, empty layers, missing tiles, a spanning `TDAT` across a chunk boundary with a small `chunk_capacity`).

> **Done 2026-10-24** (the commit that adds `omp-core::tiles`).
> **What moved where:** `omp-core::varint` (LEB128, zigzag, a bounds-checked reader; the address and search spikes use it too), `omp-core::tile` (V2 payload `encode_layer` / `Layer::decode`, `hilbert`, `zoom_band`, `TDIC` as `Dictionaries`, and `TIDX` as `IndexBuilder`, `DirEntry` and `Block`), `omp-core::schema` (`Schema::from_whitelist`, JSON in a zstd frame) and `omp-core::meta`. `omp-core::tiles` has `FrameCompressor` (one per thread, picks the (layer, band) dictionary), `TileWriter` (writes `TDIC`, begins the spanning `TDAT`, adds one record per tile, writes `TIDX` on finish) and `TileReader`. The writer is the one 1.2 will use, not only a test writer. V0, MVT, quantizing and simplifying stay in the builder.
> **`TileReader`:** owns the `Pack`, reads `TDIC` and the 120-byte `TIDX` header at open, loads a zoom's directory on first use (`OnceLock`), and keeps 512 decoded blocks in an LRU. A tile is one positioned read of its record, then one zstd frame per layer. It is `Send + Sync`: prepared dictionaries (`DDict`) are shared, and zstd contexts come from a small pool, so the loader threads of 1.4 can share one reader. It counts bytes read for the debug overlay and bench.
> **Decoder:** `Layer::decode` takes the payload and fills flat arrays (features, props, part ends, points), so a layer is a handful of allocations and none per point; strings are checked as UTF-8 once and handed out as `&str`. Counts are checked against the bytes left, so a corrupt count cannot allocate gigabytes. It is also faster than phase 0's decoder: in `measure`'s random-read bench on Ohio, V4 decode went from 302 to 174 µs per tile at z10 and 65 to 41 µs at z12.
> **Byte-identical on Ohio:** `measure ohio-261001.osm.pbf --routing none` before and after gives the same report apart from timings: every layer × zoom × variant byte count, and the same 165,539,632-byte V2 tile store. (`measure` keeps its own zstd settings, without content checksums, so its V3–V5 numbers stay comparable with phase 0.)
> **Tests:** payload round trip and garbage rejection, `TDIC` round trip, `TIDX` blocks split at 256 tiles and at padding, `SCHM` of the default whitelist, `META` with unknown fields, and a pack round trip: 603 tiles at z0, z1, z6 and z14 with lines, polygons with holes, points, string and integer props, an empty layer, layers left out, a raw-content dictionary, and a 16 KiB `chunk_capacity`, so `TDAT` spans 4+ chunks and padding splits `TIDX` blocks. Missing tiles (absent, z15, x out of range) return `None`; a damaged frame returns an error.
> **format.md** now states the `TIDX` byte layout (each zoom's directory followed by its blocks, which is how the reader finds where a block ends), that frames carry their content size, and the `META` field types.

## 1.2 `omp-builder build`: write a tile pack

A new subcommand that writes a real pack. It reuses `measure`'s pipeline (needed-nodes store, whitelist, ring assembly, z6 split tiles, bands, temp tile store) but keeps only V4 and writes it out.

```
omp-builder build <input.pbf> --out $OMP_WORK/packs/ohio [--name "Ohio"] [--clip-poly ohio.poly] [--bands 3]
omp-builder inspect <pack> [--tile z/x/y] [--stats]
omp-builder verify <pack>
```

- **Bands in one process.** `--bands n` runs the bands one after another in one run, freeing feature memory in between (North America: 3, as in phase 0, about 42 GiB peak). Each band writes uncompressed V2 payloads to the temp store.
- **One dictionary set per pack.** Phase 0 trained dictionaries per band. `build` trains them once from samples drawn across all bands' temp stores, then compresses.
- **Order.** Tiles from all bands are merged into zoom, then Hilbert order, and written to `TDAT` with the padding rule for chunk boundaries; `TIDX` is built alongside.
- **Sections:** `META` (from the fetch manifest, the whitelist version and the build settings), `SCHM`, `TDIC`, `TIDX`, `TDAT`. The address, search and routing sections are added in phases 2 and 3; the format already allows packs without them.
- `inspect` prints the header, the section table, per-zoom tile counts and bytes, and optionally one decoded tile. `verify` checks every section's XXH3. Both are thin wrappers over `omp-core`, so the app's "Verify pack" command (1.9) shares the code.
- Deterministic: the same inputs and options give a byte-identical pack, apart from the build time and pack ID (fixable with `--deterministic` for tests).
- Record wall time, peak RSS and temp disk per region, as in phase 0.

**Done when:** `build` writes Delaware, Ohio and North America packs; `verify` passes; tile bytes match `measure`'s V4 for the same region within 1% (the difference is the dictionaries); and a North America pack splits into 3 chunk files under 4 GiB each.

> **Done 2026-10-24** (the commit that adds `omp-builder build`).
> **Code:** `crates/builder/src/build/` holds `build`, `inspect` and `verify`. `build` reuses `measure`'s reader, water and tile cutter; the cutter now takes a `measure` flag, so `build` encodes only V2 and skips the V0, V1 and MVT measurements. Every band appends to one temp store. `compress::train` (split out of `measure`) trains one dictionary set from all of it. Tiles are then sorted into zoom and Hilbert order, compressed in parallel batches of 16k tiles with pooled `FrameCompressor`s, and written in order through `omp-core`'s `TileWriter`. `META` gets its sources from the fetch manifest (the PBF and the water polygons, with URL, hash and license), the attribution lines, and the settings (whitelist version, encoding, zstd level, input file names). The header snapshot reads, e.g., `osm 2026-10-01; water 2026-10-24`. `--out <dir>` names the chunks after the directory (`packs/ohio/ohio.omp.000`) and deletes old chunks of that name first.
> **omp-core additions:** `Pack::verify_with` (progress callback, for the app's "Verify pack" in 1.9), `Pack::chunks`, `TileReader::tile_count` and `for_each_tile` (walks `TIDX` without filling the block cache), `Block::iter`, and `tile::hilbert_xy` (the inverse of `hilbert`).
> **Results** (`$OMP_WORK/measure/build/*.log`), from `omp-builder build $OMP_WORK/inputs/osm/<region>-261001.osm.pbf --out $OMP_WORK/packs/<region>`, plus `--clip-poly inputs/poly/us/delaware.poly`, or `--clip-poly inputs/poly/north-america.poly --bands 3`; Ohio has no poly, as in phase 0):
>
> | Region | Tiles | Frames | Pack | Chunks | Wall | Peak RSS | Temp store |
> |---|---:|---:|---:|---:|---:|---:|---:|
> | Delaware | 3,110 | 10,815 | 8.7 MiB | 1 | 6 s | 0.6 GiB | 11 MiB |
> | Ohio | 55,649 | 170,255 | 122.8 MiB | 1 | 16 s | 2.2 GiB | 158 MiB |
> | North America (`--bands 3`) | 23,658,210 | 32,940,398 | 8,302 MiB | 3 (4,094 + 4,094 + 112 MiB) | 24 min | 43.0 GiB | 10.0 GiB |
>
> North America: each band's read takes about 350 s, and its cut about 55 s; training takes 23 s, compressing 154 s and writing 10 s. `TIDX` is 112 MiB and `TDIC` holds 29 dictionaries. `verify` passes on all three packs. On North America it took 1.3 s because the pack was still in the page cache, so this is not a cold-read number. `inspect --stats` reads the whole index in 1.2 s.
> **Against `measure`'s V4** (rerun the same day with the same whitelist, and with `--band k/3` for North America): Delaware and Ohio frames are **byte-identical** to V4 (`TDAT` − 4 bytes × frames = V4 exactly), because a single-band build trains on the same samples. The packs are 0.5% larger because each frame carries zstd's 4-byte content checksum (format.md). North America's frames are 0.15% larger than the sum of the three per-band V4 runs (one dictionary set instead of three). With the checksums, `TDAT` is 8,581 MB against V4's 8,437 MB (+1.7%, of which checksums are 126 MiB). That is over the 1% in **Done when**, but only because of the checksums, which format.md already requires; without them the difference is 0.15%.
> **Deterministic:** two `--deterministic` Delaware builds, one with `--bands 1` and one with `--bands 3`, give the same SHA-256. So banding does not change the tiles, and neither does parallel order.
> **Open:** packs have no address, search or routing sections yet (phases 2 and 3). The tiles are those of whitelist v1 (no labels layer or `routes` yet; 1.3 adds them and bumps the version). The North America pack must be rebuilt after 1.3.

## 1.3 Label and shield data in the builder

The tiles from phase 0 have the geometry but not enough to label a map well. This step adds what the renderer needs, and measures the cost of each addition on Ohio, Quebec and North America. The roadmap's "compute label points" is part of it.

**Places and boundaries:**
- Add `place=country`, `state` and `province` nodes to the places layer (state and country labels at low zoom), and `place=sea`/`ocean`/`bay` nodes as water labels.
- Add a `capital` prop and keep `population`, so the app can rank places.
- Decide whether to keep `name:en` and `name:fr`. Default: the plain `name` only, which is what is signed locally. *Decided: `name` only.*

**Label points for areas** (decision 4):
- For each named lake, large river area, park, protected area and wood, compute one label point with *polylabel* (the pole of inaccessibility, which stays inside odd shapes such as long lakes).
- Emit it as a point feature in a new synthetic `labels` layer, with the source layer and class, the name, and the area as a log₂ class. It goes only in the tile that contains the point, at every zoom where the area is at least a few hundred pixels on screen. That avoids duplicate labels from the tile buffer, and lets a lake be labelled at a zoom where its outline is already drawn.
- Point features in the 64-unit buffer would also be duplicated in a neighbour tile: check how `measure` handles places and POIs, and keep each point in one tile only.

**Line merging** (decision 5): before encoding a tile, join road, rail and waterway pieces that share a class, all props, and an end point. Fewer, longer lines make road labels easier to place and should make tiles smaller. Measure the size change; keep it if tiles do not grow.

**Route relations for shields** (owner's choice: real shields in phase 1):
- Read `type=route` + `route=road` relations with `network` and `ref` (e.g. `US:I` 75, `US:US` 23, `US:OH` 4, `CA:ON` 401, `CA:transcanada`), and attach them to their member ways as a `routes` prop: a compact string such as `US:I=75;US:US=23`, sorted by a fixed network priority.
- Fallback for ways in no relation: the way's own `ref` (e.g. `I 75;US 23`), parsed with the route designator rules from phase 0's normalization.
- Count ways with relations, refs only and neither, per country, so the shield coverage is known.
- Network names beyond the US, Canada and Mexico's federal roads fall back to a generic shield.

**Format:** these change the whitelist (bump its `version`) and `SCHM`, not the container. Update [format.md](format.md) for the `labels` layer and the `routes` prop.

**Done when:** an Ohio and a Quebec pack have state labels, label points for named lakes and parks, and `routes` on the Interstate, US, state and provincial highways; the North America size table is updated with the cost of each change; and format.md describes it.

> **Done 2026-10-24** (the commit that adds `omp-core::routes`).
> **Whitelist v2** (`crates/core/data/whitelist.toml`; [format.md](format.md#labels-and-shields) has the details):
> - `places` gains `country` (z1), `state` (`place=state|province`, z3), and the water labels `ocean` (z0), `sea` (z3) and `bay` (`natural=bay` nodes, z9), and a `capital` prop. **`name` only**, no `name:en`/`name:fr` (the open question is settled: the plain name is what is signed).
> - A new **`labels` layer**: one point per named lake, river area, riverbank, bay, protected area, park, forest and wood, at its pole of inaccessibility (`geom::polylabel`, rings first simplified to a quarter of the precision so big lakes stay cheap). Props `name` and `area` (⌊log₂ px²⌋ at z14). It appears from the zoom where the area covers 1024 px², but not before the class's minzoom; areas under that even at z16 get none. `Feature` gained a per-feature `minzoom` for this.
> - **Points go in one tile only:** the cutter drops points that fall in a tile's buffer, for every point layer (the duplicate-label question in the bullets above: `measure` did copy places and POIs into neighbours' buffers).
> - **`routes`** replaces `ref` on roads: `network=ref` entries from `type=route` + `route=road` relations, sorted by `omp_core::routes::priority`, falling back to the way's `ref` parsed by designator (`I 75` → `US:I=75`, `QC-138` → `CA:QC=138`, `CR 12` → `CR=12`). Not done: guessing the state of a bare `SR 4` from the way's position; with these coverage numbers it can wait for 1.8.
> - **Line merging** (`merge_lines` per layer): roads, ferries and rail "reversible" (except oneway roads), the water layer's lines "directed". A tile's pieces with the same class and props become one feature whose parts are the chains joined where exactly two piece ends meet. Kept: it makes tiles smaller (decision 5).
>
> **Cost of each change** (tile bytes, MiB, from `inspect --stats`; Ohio and Quebec built once per change with `$OMP_WORK/whitelists/v2-*.toml`, each adding one change to the one before; v1 is the 1.2 binary; logs in `$OMP_WORK/measure/labels/`):
>
> | Change | Ohio | Quebec | North America |
> |---|---:|---:|---:|
> | v1 tiles (1.2) | 121.21 | 745.87 | 8,183.7 |
> | Points in one tile (places, POIs) | −0.08 | −0.01 | about −1.8 (POIs −1.8; places mixed with the next row) |
> | New place classes, `capital` | +0.01 | +0.02 | +1.4 (places, both rows) |
> | `labels` layer | +0.29 | +2.78 | +23.5 |
> | `routes` instead of `ref` | +0.18 | +0.09 | +5.7 |
> | Line merging | −6.89 (roads −6.26, water −0.43, rail −0.19) | −12.71 (water −8.15, roads −4.45, rail −0.10) | −325.8 (roads −227.8, water −91.3, rail −6.7) |
> | **v2 tiles** | **114.72 (−5.4%)** | **736.05 (−1.3%)** | **7,886.7 (−3.6%)** |
>
> North America was built twice, with v2 and with v2 minus merging; the other rows are read off its per-layer bytes. Merging cuts roads by 11–13% in all three; water's share is small because most water bytes are polygons.
>
> **Shield coverage** (motorway to tertiary ways, not links, by the way's middle point in `$OMP_WORK/inputs/poly/XX-YY.poly`; `build` prints it per country, class and state or province):
>
> | North America | ways | in a route relation | `ref` only | neither |
> |---|---:|---:|---:|---:|
> | US motorway / trunk / primary | 288k / 309k / 835k | 96% / 96% / 69% | 1% / 1% / 1% | 3% / 4% / 30% |
> | US secondary / tertiary | 1,076k / 1,164k | 30% / 12% | 2% / 5% | 69% / 84% |
> | Canada motorway / trunk / primary | 27k / 24k / 77k | 95% / 94% / 77% | 1% / 1% / 2% | 5% / 5% / 20% |
> | Canada secondary / tertiary | 174k / 197k | 43% / 2% | 2% / 2% | 56% / 96% |
> | Elsewhere (Mexico, Greenland, …) | 329k | 16% | 13% | 72% |
>
> So Interstates, US routes and the main state and provincial highways come almost entirely from relations, and the `ref` fallback adds little. Most "neither" ways are secondary and tertiary roads that carry no route at all. 71,757 road route relations in 1,483 networks; the commonest by ways are `US:US`, `US:I`, `US:GA`, `US:FL`, `US:CA`, `US:VA:Secondary`, `US:NY`, `US:PA`, `US:OH`, `US:TX:FM`, `MX:MX`; Canada's are `CA:QC:R`, `CA:ON:primary`, `CA:BC` (input for 1.8's shield table). Mexico's federal network is mostly `MX:MX`, which the priority now counts as federal.
> **Checked in the packs** with the new `inspect --features <layer>/<z>`: Ohio has `state Ohio` from z3, `lake Lake Erie` and the state forests as label points at z8, and `US:I=71`, `US:US=23`, `US:OH=4` routes; Quebec has `state Québec` at z3, `Lac Saint-Jean`, `Lac Mistassini` and `Parc national du Mont-Tremblant` at z7, 1,441 bays at z9, and `CA:QC:R=138`-style routes.
> **Build cost:** Ohio 16 s (unchanged), Quebec 43 s (unchanged once relation label points are computed in parallel; sequential polylabel had taken it to 61 s), North America 24.9 min at 43.0 GiB peak in 3 bands (1.2: 24 min, 43.0 GiB); the no-merge build took 25.5 min.
> **Packs rebuilt** with v2: Delaware (8.1 MiB, was 8.7), Ohio (116.4, was 122.8), Quebec (742.5) and North America in `$OMP_WORK/packs/`. North America is now **8,001 MiB in 2 chunks** (4,095 + 3,906 MiB): with 297 MiB less tile data it fits in two. `verify` passes.
> **Open:** the 1024 px² threshold and the per-class label minzooms are first guesses, to be tuned when 1.7 draws them. Bare refs without a known designator (`SR 4`, `=401`) get a generic shield.

## 1.4 App skeleton: pack, camera, tile loading, bench

Replace the stub window with the real structure, drawing only tile outlines and debug text. No styling yet.

- **Portable mode** (roadmap §5): `config.toml` next to the executable, holding the last view, the chosen pack and settings. If that folder is not writable, fall back to the OS config folder and say so. No network code anywhere; check that no dependency pulls one in (`cargo tree`).
- **Pack discovery:** open the pack in `maps/` next to the executable, or `--pack <dir>`. If there are several, open the last one used and offer a picker. One pack at a time (decision 6).
- **Camera:** Web Mercator, north up, no rotation (decision 7). Continuous zoom from z0 to z18; above z14 the app overzooms z14 tiles. Drag to pan with inertia, wheel and pinch to zoom around the cursor, keyboard arrows and +/−, double-click to zoom in. The view is clamped to the pack's bounding box with a margin.
- **Tile selection:** the integer tile zoom for a fractional zoom (start with `floor(zoom + 0.5)`, tune in 1.5), the visible tiles plus a one-tile margin, and parent or child fallbacks while tiles load.
- **Loader:** a small pool of worker threads reads, decompresses and decodes tiles (and later tessellates them, 1.5) off the UI thread, newest request first; stale requests are dropped when the view moves on. Finished tiles trigger `request_repaint`.
- **Cache:** an LRU of decoded and tessellated tiles with a byte budget (start at 96 MB CPU-side plus the GPU buffers), counted from the actual buffer sizes.
- **Debug overlay** (F3): fps and frame time, tiles visible, loading and cached, cache bytes, RSS, bytes read, and read latency.
- **Bench:** `omp-app --bench <flight.toml>` flies a fixed path (jumps, pans and zooms across the continent, from z3 to z17), then writes frame times, tile latencies, peak RSS and bytes read as JSON. Flights live in the repo (`crates/app/bench/`). This is the measuring tool for the exit criteria.

**Done when:** the app opens the Ohio and North America packs, pans and zooms smoothly over tile outlines, keeps its view across restarts, and `--bench` produces a JSON report.

> **Done 2026-10-24** (the commit that adds `crates/app/src/bench.rs`).
> **Code** (`crates/app/src/`): `camera` (Web Mercator in 0‥1 world units, 512-point tiles as format.md's `area` assumes, z0–z18, clamped so the centre stays within the pack's bbox plus a quarter viewport; `Motion` does inertia and the animated zoom), `tiles` (`TileId`, `tile_zoom` = `floor(zoom + 0.5)` capped at z14, `select` = visible tiles nearest the centre first plus a one-tile margin, clipped to the pack bbox; `plan` draws a loaded ancestor up to 8 levels up, else four loaded children, else a placeholder), `loader` (`available_parallelism − 1` workers, 2–6; the UI's wanted list replaces the queue every frame, so stale requests are dropped unread; each result wakes the UI), `cache` (LRU by bytes, 96 MiB, never evicts what the current frame uses), `config`, `packs`, `memory` (VmRSS/VmHWM on Linux, `GetProcessMemoryInfo` on Windows) and `app` (input, outlines, F3 overlay, Ctrl+O pack picker, notices). `omp-core` gained `TileReader::tile_timed` (read and decode time per tile) and `Layer`/`Tile::heap_bytes` for the cache budget. The cache holds the decoded `Tile` itself, so the budget shows up in RSS; 1.5 adds the tessellated form.
> **Input:** drag with inertia, wheel (half a zoom per notch, animated) and pinch around the cursor, double-click zooms in, arrows pan, +/− zoom. Repaints only on input, motion or an arrived tile.
> **Portable config:** `config.toml` next to the executable (`pack`, `[view]`, `debug_overlay`), written through a temp file 5 s after the view settles and on exit; if the folder is not writable it falls back to `$XDG_CONFIG_HOME`/`%APPDATA%`/`~/Library/Application Support` and shows a notice. The pack path is stored relative to the executable when inside its folder, so a stick at another drive letter still works. Pack choice: `--pack <dir>` (a folder, a parent of pack folders, or the `.omp.000` file), else `maps/` next to the executable (one level of subfolders), else the pack used last; with several, the last one used, else the picker. Checked: a second start without `--pack` reopened the Ohio pack at the saved view.
> **No network:** `cargo tree` for Linux, Windows and macOS has no HTTP, TLS, socket or async-runtime crate; CI now fails if one appears.
> **Bench:** `omp-app --bench <flight.toml> [--pack <dir>] [--out <file>]`. Flights in `crates/app/bench/` (`ohio.toml`, `north-america.toml`: 23 steps, z3–z17, ~150 s) are lists of jumps (which wait until the view is fully detailed, timeout 20 s) and linear flights. The JSON has start-up to first full frame, frame interval and CPU time (mean, p50, p95, p99, max), per-step frames, incomplete frames, time to full detail and bytes read, tile latency (first request to decoded) split into read and decode, bytes read, RSS peak and end, cache peak, and the wgpu adapter. Vsync is off during the bench, and on Linux it uses X11 (XWayland) when `DISPLAY` is set, because Wayland compositors pace frames to the display even then (16.7 ms intervals on Wayland, 0.4 ms on X11).
> **Results** (dev machine, Radeon GFX1200 via Vulkan/RADV, pack on the SATA SSD `$OMP_WORK`; reports in `$OMP_WORK/measure/bench/`):
>
> | Flight | Frames | Interval p50 / p95 / max | Start-up | Tile latency p50 / p95 / max | Read p95 | Decode p95 | Read | RSS peak |
> |---|---:|---:|---:|---:|---:|---:|---:|---:|
> | Ohio, warm | 74,753 | 0.39 / 0.45 / 14.7 ms | 94 ms | 0.48 / 1.55 / 2.4 ms | 0.07 ms | 0.73 ms | 12.3 MiB | 182 MiB |
> | North America, warm | 133,188 | 0.38 / 0.45 / 14.8 ms | 97 ms | 0.49 / 2.5 / 4.5 ms | 0.07 ms | 0.69 ms | 42.4 MiB | 212 MiB |
> | North America, cold (disk remounted) | 133,158 | 0.38 / 0.45 / 17.3 ms | 110 ms | 1.9 / 132 / 610 ms | 6.4 ms | 0.74 ms | 41.8 MiB | 211 MiB |
>
> Frames are cheap because only outlines are drawn; they say nothing yet about criterion 1. Cold, every jump was fully detailed within 132 ms (Chicago z13; Montreal z11 95 ms, the rest under 10 ms). The slow ones are the first visit to a zoom, which reads that zoom's whole `TIDX` directory (read max 450 ms). The cache filled its 96 MiB on both flights. RSS is about 90 MiB of wgpu, driver and egui plus the cache, so the 300 MB budget leaves about 110 MB for the GPU buffers and labels of 1.5–1.8. The decoded `Layer` keeps its whole decompressed payload for its strings next to the decoded points, which can be trimmed if memory gets tight. The release binary is 11.0 MB stripped.
> **Open:** the tile zoom rounding (`floor(zoom + 0.5)`) stays a first guess until 1.5 draws real geometry. Not checked here: Windows (CI builds it; the RSS code is Windows-only and is compiled only there), USB, and a cold first-zoom-directory read on USB 2, which is the likely worst case for criterion 2 (1.10). Screenshots wait for `--render` in 1.5.

## 1.5 Geometry renderer

Draw polygons and lines with wgpu, inside egui. Roadmap §5 chose `wgpu` + `lyon`.

- **Integration:** an `egui_wgpu` paint callback draws the map into the central panel with its own pipelines, so egui widgets draw on top. Enable wgpu's GL backend next to Vulkan, DX12 and Metal, for older hardware (decision 2).
- **Polygons:** tessellate with `lyon` (fill, even-odd) once per tile on the loader threads. Vertices stay in tile-local coordinates (i16 fits −64…4159) and a per-tile transform places them, so a tile is tessellated once for every zoom it is drawn at.
- **Lines:** expanded to quads in the vertex shader, with the width in screen pixels, and round joins and caps. Zooming then never re-tessellates, and widths change smoothly with fractional zoom. Roads get a casing pass.
- **Draw order across tiles:** draw layer by layer across all visible tiles (all land cover, then all water, then every road casing, then every road fill, by class), not tile by tile. Then the 64-unit tile buffers overlap harmlessly, and a road's casing never covers the neighbouring tile's fill. Clip each tile's polygons to its own square (scissor or a stencil) so semi-transparent fills don't darken the seams.
- **Bridges and tunnels:** tunnels dashed or faded, bridges with a casing above the roads below, using the existing `bridge` and `tunnel` props.
- **Antialiasing:** MSAA 4× where supported, checked on the old laptops (it may need to be a setting).
- **Headless render:** `omp-app --render --pack <dir> --center <lat,lon> --zoom <z> --size 1200x800 out.png` draws offscreen without a window. Use it for the reference screenshots (`docs/screenshots/`) and in sessions to check changes.
- Temporary hard-coded colors until 1.6.

**Done when:** Ohio, Quebec and North America render all geometry layers at every zoom with no gaps or seams at tile edges, overzoom works to z18, `--render` produces PNGs, and the bench shows the dev machine at 60 fps p95 on the Ohio pack.

> **Done 2026-10-24** (the commit that adds `crates/app/src/render.rs`).
> **Code** (`crates/app/src/`): `style` (the temporary hard-coded style: rules by layer and class name, resolved through the pack's `SCHM`; widths as zoom stops in points; passes land cover, water, water lines, boundaries, tunnels, rail, ferries, roads, bridges), `mesh` (runs on the loader threads: polygons tessellated per feature with `lyon`, even-odd; lines become one instance per segment with its distance along the line, for dashes; everything in `i16` tile units, sorted by pass and class so each pass is one range per tile), `render` and `map.wgsl` (fill, line and casing pipelines; a per-tile transform at a dynamic uniform offset; a 128-entry paint table recomputed each frame for the fractional zoom; `--render`'s offscreen target) and `snapshot` (`--render`). The loader is now generic: its workers decode, tessellate and upload each tile, so the UI thread only draws. The cache holds the GPU buffers and counts their bytes against the 96 MiB budget; the decoded tile is dropped (1.7 will extract its label data on the loader thread).
> **Drawing:** an `egui_wgpu` paint callback draws into egui's pass, so egui draws on top (the F3 overlay now also draws tile outlines). Tiles are drawn by zoom, lowest first (fallback parents), then pass by pass across the tiles of that zoom, casings under fills. Each tile is scissored to its square, with edges rounded the same way for neighbours, and starts with a land square that reaches into the buffer (stopping it at the tile edge left a faint antialiased seam). That square hides the parent drawn underneath. Lines are quads widened in the vertex shader, with round ends from the distance to the segment in the fragment shader and a 1-pixel antialiased edge; lines under a pixel are drawn one pixel wide and opaque, since fading them darkened every vertex where segment ends overlap. Roads, tunnels and bridges have casings; tunnels are faded with a dashed casing, bridges get a dark casing in a pass above the roads. Dashes restart at tile edges.
> **Antialiasing:** MSAA 4× (eframe `multisampling`). `msaa = 1` in `config.toml` (or `--msaa 1` for `--render`) turns it off, for GPUs that cannot do it.
> **Backends:** egui-wgpu already enables Vulkan, DX12, Metal and GL (`PRIMARY | GL`), so nothing was added. On the dev machine the window draws correctly with `WGPU_BACKEND=gl` and MSAA (checked with a temporary screenshot). Headless `--render` on GL reads back a black image with MSAA, so `--render` uses 1 sample on GL.
> **Headless render:** `omp-app --render --pack <dir> --center <lat,lon> --zoom <z> [--size <w>x<h>] [--scale <s>] [--msaa <n>] out.png`, 0.1–0.3 s per image. `docs/screenshots/render.sh` renders 11 spots from the North America pack (continent, Arctic, Rockies, rural Ohio, Chicago, the Bay Area, Mexico City, Montreal, the Ontario lakes, Manhattan at z14 and z17); they are in `docs/screenshots/`, with the temporary colours.
> **Ocean fix (builder):** the first renders showed open ocean missing at every zoom. The `water-polygons-split` shapefile winds its rings the opposite way from the shapefile convention that `measure/water.rs` assumed, so every ocean piece came out with its outer ring counter-clockwise; pieces with islands survived (the islands looked like outer rings), and pieces without were dropped as all holes. Pieces are now oriented by their largest ring. **Phase 0's and 1.2–1.3's water sizes were measured with the bug**, so they undercount the ocean. Rebuilt: Delaware (8.1 MiB, Delaware Bay now filled), Quebec (751.0 MiB, was 742.5) and North America: **9,200 MiB in 3 chunks** (4,095 + 4,095 + 1,010), was 8,001 in 2. Water tiles 4,711 MiB (was 3,675), `TIDX` 275 MiB (was 112), 64.0M tiles (was 23.7M): almost all of the new tiles are open ocean at z13–z14. Build 27.2 min at 43.6 GiB peak. `verify` passes on both. See the open question under Decisions.
> **Tile zoom:** `floor(zoom + 0.5)` stays (open decision settled). A tile drawn at up to 1.41× its size still shows its simplification well under a pixel, and the frame times leave plenty of room for the extra tiles of rounding up.
> **Results** (release build, dev machine, Radeon GFX1200 via Vulkan; reports in `$OMP_WORK/measure/bench/*-1.5*.json`):
>
> | Flight | Frames | Interval p50 / p95 / max | Start-up | Tile latency p50 / p95 / max | Read p95 / max | Decode p95 | Tessellate + upload p50 / p95 | RSS peak |
> |---|---:|---:|---:|---:|---:|---:|---:|---:|
> | Ohio, warm | 47,142 | 0.60 / 0.69 / 19.0 ms | 130 ms | 4.4 / 20.3 / 47 ms | 1.2 / 1.9 ms | 0.83 ms | 1.4 / 11.1 ms | 115 MiB |
> | North America, warm | 84,035 | 0.60 / 0.69 / 17.8 ms | 96 ms | 6.2 / 24.6 / 60 ms | 1.5 / 9.3 ms | 0.91 ms | 1.5 / 9.5 ms | 160 MiB |
> | North America, cold (disk remounted) | 84,324 | 0.60 / 0.68 / 18.1 ms | 112 ms | 6.6 / 32.2 / 62 ms | 1.5 / 35 ms | 0.89 ms | 1.4 / 9.4 ms | 162 MiB |
>
> The frame target (16.7 ms p95) is met with a wide margin: drawing a full view costs about 0.2 ms more than 1.4's outlines. Tile latency is now mostly tessellation, which runs on the loader threads, so frames never wait for it; jumps were fully detailed within 28 ms warm or cold (Toronto z10). The cold numbers are better than 1.4's (worst read 35 ms, was 450 ms); the cause is not known, and 1.10 measures cold reads properly on USB. RSS fell by about 50 MiB because the cache no longer keeps decoded tiles; on this discrete GPU the vertex buffers are in video memory, which RSS does not count. On an integrated GPU they may count, which 1.10 measures. The cache still fills its 96 MiB on North America. The release binary is 11.3 MB stripped (was 11.0).
> **Seams:** none seen in the screenshots or at tile edges checked pixel by pixel (Delaware z9, a row across a tile edge). Overzoom to z18 works (Columbus z18, Manhattan z17).
> **Open:** the full-ocean tiles (Decisions); old hardware and Windows (1.10); `--render` on GL without MSAA; dashes restart at tile edges; translucent fills are fine, but a translucent line would still darken where segments overlap (no line uses one).

## 1.5b Leave out full-ocean tiles

Added after 1.5 (owner, 2026-10-24). With the ocean fixed, North America stores about 40M tiles at z13–z14 that hold nothing but one ocean square covering the tile, which costs about 1.2 GiB (1,036 MiB of tiles and 163 MiB of `TIDX`; see [size-table.md](size-table.md#after-phase-1-step-15-the-ocean)). Those tiles add nothing their parent does not already show.

Checked on the 1.5 pack (2026-10-24, design review): a full-ocean tile is about 23 bytes of `TDAT` plus about 4 of `TIDX`, and about 13 of the 23 are the zstd frame's own overhead (see [1.5c](#15c-tile-size-experiments-frame-overhead-and-per-layer-max-zoom)). The count is large because Geofabrik's North America `.poly` takes in a wide stretch of open ocean: z14 tile 14/1802/6461, about 2,000 km off California, is stored.

- **Format:** **every tile that is not stored is drawn from its nearest stored ancestor**, scaled up, as the app already does for overzoom past z14 (owner, 2026-10-24). No flag in `TIDX`: an empty land tile and a left-out ocean tile are handled the same way. This is safe because a tile holds everything its ancestor holds in its square (features are only filtered by `minzoom`), so the ancestor has nothing there either. Write the rule in format.md, with the argument below for why simplification cannot leave slivers.
- **Simplification cannot leave slivers.** The worry was that the ancestor's simplified edges, up to its tolerance off, could reach into a left-out square. They cannot: a left-out child is full ocean out to its 64-unit buffer, which is 32 units in its parent's grid, so the real coastline is at least 32 parent units from the child's square. Every tolerance in the whitelist is at most 16 units (water and land cover 6 / 8 / 16 at z12 / z13 / z14, 4 elsewhere), so the parent's simplified edge stays outside it. The same holds level by level up to the stored ancestor. So no flag on the ancestor's `TIDX` entry is needed. Add a builder assertion that every tolerance stays under half the buffer, so a later whitelist change cannot break this unnoticed.
- **Builder:** after cutting, leave out a tile whose only content is ocean that **together** covers the whole tile plus its buffer, when its parent is the same (so the parent can stand in). Anything else in the tile (a maritime boundary, a ferry, an island) keeps it.
  - "Together" matters: `water-polygons-split` is cut along whole-degree lines, so tiles on those lines hold 2–4 ocean pieces, not one (tile 14/5097/6202, at 40°N 68°W, has 4 ocean squares at every zoom from z10 to z14). A rule that looked for one covering polygon would keep all of them.
  - Better still, **dissolve the ocean pieces of one tile into one polygon** before encoding, so the split lines vanish everywhere. Then the full-ocean test is "one ocean polygon covering tile and buffer" again. That may also shrink water at low zooms, where a tile holds many pieces with shared edges along the split lines: measure the water bytes per zoom against 1.5.
  - Check whether the same rule is worth applying to full-tile lakes and land cover (Hudson Bay, the Great Lakes, large forests).
- **App:** `tiles::plan` draws the stored ancestor for a left-out tile, and the loader stops asking for tiles below it. The bench's "fully detailed" counts such a tile as loaded once its ancestor is. The style's line widths already work at any scale.
  - Finding the ancestor can take several `TIDX` lookups (z14 missing, z13 missing, …), and a cold first read of a zoom's directory was already 1.4's slowest read. Cache "not stored" answers next to the decoded `TIDX` blocks, so panning over open ocean does not repeat the walk.
  - Label data drawn from an ancestor must be clipped to the left-out tile's square, as its geometry already is by the scissor (see [1.7](#17-text-and-label-placement)).
- **Measure** on Quebec and North America: tiles stored, tile bytes, `TIDX` bytes and pack size, against the 1.5 packs (North America 9,200 MiB, 64.0M tiles). The target is most of the 1.2 GiB back, so the pack is near 8.0 GiB again.
- Rebuild Delaware, Quebec and North America; rerun `verify`, the North America bench (warm and cold) and `docs/screenshots/render.sh`, and check that open ocean is still drawn at every zoom with no seams.

**Done when:** format.md describes left-out tiles, the North America pack no longer stores full-ocean tiles whose parent is full ocean, the ocean renders the same as in the 1.5 screenshots, and the size table and roadmap §4 have the new numbers.

> **Done 2026-10-24** (the commit that adds `geom::rects_cover`). Format **0.2** ([format.md, Left-out tiles](format.md#left-out-tiles)): a missing tile means "draw the nearest stored ancestor". The minor version is bumped because a 0.1 reader would draw left-out tiles as land.
> **Builder** (`measure/tiles.rs`): a tile at z1–z14 is left out when its whole V2 content is one polygon covering the tile and its buffer, and that polygon's `minzoom` is below the tile's zoom, so the parent shows it. The test runs on the encoded output, so anything else, even only in the buffer, keeps the tile. It is not limited to the ocean: big parks, lakes and forests qualify by the same argument. The `minzoom` condition was added after the first full build dropped 16 z6 tiles inside Greenland's national park: protected areas start at z6, and the z5 ancestor does not have them. A startup assertion checks that every tolerance is under half the buffer. `build` prints what it left out, per zoom and per class.
> **Split lines:** `water-polygons-split` pieces **overlap** along the whole-degree lines, by about 0.0013° (238 grid units at z14) on 75° W. So a dissolve that cancels shared edges does not work (the first try left a land-coloured strip along 75° W, which even-odd filling made out of the overlap). A tile's ocean pieces are now merged into one square only when they are all rectangles that together cover the tile (`geom::rects_cover`), which is the full-ocean case. A general union would only help the tiles that keep several ocean pieces: in Quebec, 2,048 tiles with about 5 MiB raw, so at most 0.7% of the pack. It is not done.
> **Lakes and land cover:** checked with the same counter. In Quebec they are 14.6k tiles (under 1 MiB). In North America, protected areas are 5.6M tiles (317 MiB raw), so they are left out too: 137 MiB of the pack.
> **App:** `tiles::resolve` walks up past tiles known to be absent; `tiles::plan` draws the stored ancestor (`Cover::Ancestor`) or, when nothing is stored up to z0, bare land (`Cover::Empty`), and returns the next tiles to read (`need`): a wanted tile, or the next ancestor up from an absent one. The view is "fully detailed" when `need` is empty, which the bench uses. Absent answers stay in the tile cache (64 bytes each) and are kept in use while the view needs them, so panning over open ocean does not walk the index again; the decoded `TIDX` blocks are cached as before. `--render` reads in rounds until every visible tile resolves. The F3 overlay outlines stand-in ancestors in blue.
> **Packs** (`$OMP_WORK/measure/build/*-1.5b.log`; the 1.5 packs are kept in `$OMP_WORK/packs/1.5/`):
>
> | Pack | Size (was) | Tiles stored (was) | Left out |
> |---|---:|---:|---:|
> | Delaware | 8.1 MiB (8.1) | 3,254 (3,660) | 406 ocean |
> | Ohio | 116.1 MiB (116.4) | 47,003 (55,649) | 8,645 lake, 1 farmland |
> | Quebec | 743.5 MiB (751.0) | 878,657 (1,114,780) | 221k ocean, 8k protected area, 4k lake, 2k wood |
> | North America | **8,026 MiB** (9,200), 2 chunks (4,095 + 3,931) | **17.9M** (64.0M) | 40.1M ocean, 5.6M protected area, 0.24M lake, 0.16M wood and forest |
>
> North America: water tiles 3,828 MiB (was 4,711), land cover 2,010 (2,122), all tiles 7,929 (8,923), `TIDX` 95 MiB (275), 33.1M frames (79.2M). The target was most of the 1.2 GiB back: 1.15 GiB came back. Build 24.2 min at 43.0 GiB peak. `verify` passes on all four.
> **Renders:** the 1.5 app on the 1.5 packs against this one on the new packs, pixel by pixel: 91 North America views (the 11 reference spots, and open Pacific, Atlantic and Gulf, the California coast, Wood Buffalo, Gates of the Arctic, two Greenland spots, Hudson Bay and Lake Superior, each at z5–z16), 48 Quebec views and 30 Delaware views are **identical**. `render.sh` gives pixel-identical screenshots, so they are not recommitted.
> **Bench** (dev machine, as in 1.5; `$OMP_WORK/measure/bench/*-1.5b*.json`):
>
> | Flight | Frames | Interval p50 / p95 / max | Start-up | Tile latency p50 / p95 / max | Read p95 / max | Tiles loaded + absent | Read | RSS peak |
> |---|---:|---:|---:|---:|---:|---:|---:|---:|
> | Ohio, warm | 47,116 | 0.60 / 0.69 / 17.7 ms | 117 ms | 3.4 / 19.6 / 41 ms | 0.1 / 1 ms | 529 + 48 | 12.4 MiB | 117 MiB |
> | North America, warm | 83,586 | 0.60 / 0.69 / 17.9 ms | 123 ms | 4.7 / 21.1 / 64 ms | 0.1 / 2 ms | 1,182 + 55 | 37.4 MiB | 121 MiB |
> | North America, cold (disk remounted) | 83,602 | 0.60 / 0.69 / 117 ms | 230 ms | 6.2 / 24.2 / 61 ms | 1.5 / 6 ms | 1,187 + 56 | 37.5 MiB | 124 MiB |
>
> Every jump was fully detailed within 39 ms. The flight reads 37 MiB instead of 62, and RSS peaks about 40 MiB lower. A one-off pause of 117–192 ms in the first step (start-up), with under 10 ms of CPU in any frame, showed up on the first runs; reruns of Ohio and warm North America did not repeat it (max 18 ms, as in 1.5), so it is outside the frame work, probably window start-up.
> **Open:** label data drawn from an ancestor must be clipped to the left-out tile's square (1.7). A cold first walk up over open ocean reads up to one `TIDX` block per zoom; on USB this belongs to 1.10's cold-read checks.

## 1.5c Tile size experiments: frame overhead and per-layer max zoom

> **Baseline from 1.5b** (North America): 33.1M frames (was 79.2M), so the magic number and checksum come to about 250 MiB. z14 is now water 1,554 MiB and land cover 734 MiB; all water 3,828, all land cover 2,010.

Added after the design review of 2026-10-24 (owner). Two experiments that could save space without losing features. Each one is measured, and kept only if it pays. Run them after 1.5b, on its packs, so the numbers are not inflated by the ocean tiles. Both change the format; whatever is kept goes into format.md and bumps the minor version.

### A. zstd frame overhead

Each layer of each tile is compressed as its own zstd **frame**. Every frame carries a fixed wrapper, whatever is inside it: a 4-byte magic number that says "this is zstd", a frame header (about 2 bytes here), a 3-byte block header, and a 4-byte checksum, so about 13 bytes in all. For a 2 KiB tile that is noise, but most frames are tiny. The 1.5 North America pack has **79.2M frames, and 60.7M of them are under 40 bytes**. A full-ocean tile is 23 bytes, of which about 10 are the actual square. The magic number and checksum alone come to about 600 MiB over the pack (79.2M × 8 bytes), and about 300 MiB once 1.5b has left out the ocean tiles. Neither is needed per frame:
- the reader knows from `TIDX` exactly where each frame starts and how long it is, so the magic number tells it nothing;
- `verify` already covers corruption, and the section checksums catch it.

Variants, measured against the 1.5b pack:
- **F1: magicless frames.** zstd's `format` parameter (`ZSTD_f_zstd1_magicless`, in the experimental API; check what the `zstd` crate exposes). Saves 4 bytes per frame and changes nothing else.
- **F2: no per-frame checksum.** Either none at all, or one checksum per tile instead of per frame. A corrupt tile must then fail cleanly in the payload decoder rather than in zstd: check that `omp-core::tile` returns an error, without panicking, on truncated or garbled payloads (a fuzz test). That also matters for `verify`'s "suggested when a tile fails to decode" (1.9).
- **F3: tiny payloads stored raw**, without zstd, below a size threshold, flagged in `TIDX` (for example `layer_len << 1 | raw`). This saves the whole wrapper on frames where compression gains nothing. Try a few thresholds.
- **F4: one frame per tile** instead of one per layer, as a comparison only. It saves the most wrappers, but loses the per-layer dictionaries, so it is likely worse.

Measure tile bytes, `TIDX` bytes and pack size on Quebec and North America, plus decode time p95 from the North America bench. Keep the variants that save meaningful space (say over 100 MiB on North America) without a measurable decode-time cost.

### B. Per-layer max zoom

Store a polygon layer only up to some zoom *M*, and draw it from the tile at *M*, scaled up, at higher zooms, while the other layers still come from the current tile. Water (4.7 GiB) and land cover (2.1 GiB) are 6.8 of the 8.9 GiB of tiles in the 1.5 pack. Their z14 bytes are the most this can save per layer: land cover 811 MiB, water 2,210 MiB (part of which is ocean that 1.5b removes anyway).

- **Cost in quality:** a layer drawn from z13 has z13's simplification (8 grid units, about one z13 pixel), so edges sit up to about 2 px off at z14 and 8 px at z16. Road-heavy places will show it most: shorelines next to roads, bridges over rivers, ferries meeting their docks, and islands in lakes.
- **Variants:** land cover to z13; land cover to z12; water areas to z13 (water lines such as rivers and canals stay to z14, since they sit under road bridges). If the water layer mixes lines and areas, splitting it into two layers may be needed first.
- **Classes:** a class whose `minzoom` is above *M* would disappear. List such classes in the affected layers, and either lower their `minzoom` to *M* or keep the layer to z14.
- **Format:** a `maxzoom` per layer in `SCHM` (a minor-version addition), so the app knows the layer is not missing from higher tiles.
- **Builder:** stop emitting the layer above its `maxzoom`. A tile left with nothing else is left out, as in 1.5b.
- **App:** draw a capped layer from the tile at its `maxzoom`, which the loader then also needs at higher zooms. Rendering already goes pass by pass across tiles, so the ancestor's land cover pass, scissored to the current tile, is drawn where the current tile's would be. This reuses 1.5b's ancestor drawing.
- **Quality check:** render `docs/screenshots/render.sh`'s spots at z14, z15 and z17 with each variant next to the 1.5b pack, and add a few spots for the cases above (a lakeshore road in Ontario, a river bridge, a coastal town). The owner judges the screenshots.

**Done when:** both experiments are measured on Quebec and North America and the results are written under this step; the owner has decided what to keep; any kept variant is in format.md, the builder and the app; the packs are rebuilt and verified; the bench and screenshots are rerun; and the size table and roadmap §4 have the new numbers.

> **Measured 2026-10-24; waiting for the owner's decision** (the commit that adds `build/experiment.rs`).
> **Tools:** `omp-builder size-experiments <pack>` reads a pack and works out every variant without rebuilding: F1 and F2 change only the frame wrapper, so their sizes are exact; F3 uses each frame's content size; F4 and the water cap decompress and recompress (level 19, with dictionaries). It rebuilds `TIDX` for each variant, the baseline too, so the index sizes compare exactly, and times the decode of every 64th tile (single thread, best of 3). `omp-app --render --cap landcover=13,water-areas=13` previews a capped pack: above the cap a tile's own features of that layer (or only its polygons, with `-areas`) are replaced by its ancestor's at the cap zoom, scaled into the tile. Results in `$OMP_WORK/measure/1.5c/`.
> **Frames by stored size** (North America, 33.1M): 28% are 16–23 bytes, 43% under 40, 80% under 256.
>
> | Variant | Quebec MiB saved | North America: tiles + `TIDX` MiB | saved |
> |---|---:|---:|---:|
> | baseline (1.5b) | 0 (742.0) | 7,928.7 + 95.0 = 8,023.7 | 0 |
> | F1 magicless | 5.7 | 7,897.2 | 126.5 |
> | F2 no checksum | 5.7 | 7,897.2 | 126.5 |
> | **F1+F2** | **11.5** | **7,770.6** | **253.1** |
> | F1+F2, one checksum per tile | 8.1 | 7,839.0 | 184.7 |
> | F1+F2+F3, raw ≤ 16 B | 11.4 | 7,771.0 | 252.7 |
> | F1+F2+F3, raw ≤ 32 B | 11.4 | 7,787.8 | 235.9 |
> | F1+F2+F3, raw ≤ 64 B | 10.6 | 7,987.4 | 36.3 |
> | F1+F2+F3, raw when smaller than the frame | 11.9 | 7,756.3 | 267.4 |
> | F4 one frame per tile | 6.4 | 7,943.0 | 80.6 |
> | F4+F1+F2 | 13.1 | 7,806.1 | 217.5 |
> | B land cover to z13 | 46.3 | 7,280.3 | 743.3 |
> | B land cover to z12 | 81.0 | 6,787.6 | 1,236.1 |
> | B water areas to z13 | 147.6 | 7,124.8 | 898.9 |
> | **B land cover and water areas to z13** | **194.0** | **6,369.4** | **1,654.3** |
>
> **A, frame overhead.** F1+F2 saves exactly 8 bytes per frame: 253 MiB on North America (3.2%), 11.5 MiB on Quebec (1.5%). Decode per tile is no slower (North America p50 / p95 / p99: 0.8 / 8.1 / 24.7 µs baseline, 0.7 / 7.8 / 23.4 µs F1+F2). It needs zstd's experimental API (`zstd` feature `experimental`, `FrameFormat::Magicless`), and corruption is then caught by the section checksums (`verify`) and the payload decoder. That decoder still needs the fuzz test of F2 before F2 is kept. A checksum per tile gives back 68 of the 253 MiB. **F3** does not pay: zstd with a dictionary compresses even 10-byte payloads below their raw size, so raw storage only helps where it is chosen frame by frame ("when smaller", 14 MiB more than F1+F2), and the flag bit makes `TIDX` 3.8 MiB larger. **F4** saves less than F1+F2 on North America (F4+F1+F2 saves 36 MiB less) and costs the per-layer dictionaries; its z14 dictionary trained to only 7 KiB, so z14 may be underrated, but it is a comparison only. *Proposal: keep F1+F2 (format 0.3), drop F3 and F4.*
> **B, per-layer max zoom.** Every class of land cover and every water area class starts at z10 or lower, so no class disappears under any cap; streams (z13) are lines and stay. The water layer did not need splitting: the cap keeps its lines at z14 and drops only its polygons, which the format could express as a `maxzoom` per layer *and geometry* in `SCHM`, or by moving water lines to their own layer. With both z13 caps, 5.3M of 17.9M tiles hold nothing else and are left out.
> - **Quality.** The water and land cover tolerances are 8 units at z13 and 16 at z14, the same distance on the ground, so a z13 cap does not move edges further than they already are: the shapes differ because Douglas–Peucker keeps other vertices in the other tile. What z13 drops that z14 keeps are rings under 256 grid units (about 2×2 z13 pixels, 14 m across at 45° N) and lines under 8 units. At z12 the tolerance is 1.5 times coarser and rings under about 27 m across are dropped.
> - **Screenshots** with `--cap` against the 1.5b pack at 15 spots (the 1.5 reference spots and a lakeshore road in Muskoka, Port Carling, the Roebling bridge in Cincinnati, the Thousand Islands, Bar Harbor, the Woods Hole ferry, Gloucester), at z14, z15 and z17: with both z13 caps at most 1.5% of the pixels change (10,240 of 700,000, Woods Hole z17), and they are edge shifts of 1–16 px along shores and land cover outlines (`$OMP_WORK/measure/1.5c/caps-diff.txt`). Land cover to z12 changes up to 3.4% and visibly drops small patches (Mexico City z15). Eight comparisons are in [`docs/screenshots/1.5c/`](screenshots/1.5c/): baseline, both z13 caps and land cover to z12, with changed pixels in red.
> - **App cost** if kept: a view above the cap also needs the tile at the cap zoom (one z13 tile covers 4 z14 tiles, so 1–2 more reads per view), and the ancestor's capped layers are drawn scissored to each tile, as for 1.5b's left-out tiles.
> - *Proposal: cap land cover and water areas at z13 (−1,654 MiB, tiles and `TIDX` 6,369 MiB); not land cover at z12, which loses features.*
> **Combined** (not measured together): F1+F2 on the z13-capped pack would save about 8 bytes × 20.3M frames, 155 MiB more, so about 6,210 MiB in all, from 8,024.
> **Left for after the decision:** format.md (0.3), builder, app (`--cap` is only a preview and goes when the real one lands), the F2 fuzz test, rebuilds, `verify`, bench, `render.sh`, the size table and roadmap §4.

## 1.6 Style file and default style

- **Format:** TOML, like the whitelist. Rules per layer and class: draw order, fill and stroke colors, line width and casing width as functions of zoom (stops with linear interpolation), dashes, minimum and maximum zoom, and label rules (font, size, color, halo, priority, the prop to use, placement: point, line or area label point) plus the icon or shield for POIs and routes.
- Layers and classes are referred to by **name** and resolved through the pack's `SCHM` when it opens, so an unknown class in a pack is skipped with a warning instead of failing.
- The style is compiled into the app, and a `style.toml` next to the executable overrides it. In debug builds it reloads when the file changes, for quick tuning.
- **No `STYL` section in packs** (decision 8). The app owns the style; `SCHM` names are the contract. Remove `STYL` from format.md's provisional list.
- **Default look** (owner: no strong preference): a road atlas look, leaning towards Streets & Trips: pale land, soft greens for parks and woods, blue water, and a strong road hierarchy (Interstates and motorways red or orange, primaries yellow or amber, local streets white with grey casings), so main routes stand out for trip planning. Tune it from `--render` screenshots at about ten fixed spots: Manhattan, Chicago, rural Ohio, the Ontario lakes, Montreal, the Rockies, Mexico City, and the Canadian Arctic at low zoom.
- Layer toggles (1.9) work by switching style rules on and off.

**Done when:** the default style draws every class in the whitelist, the screenshots at the fixed spots are in `docs/screenshots/`, and changing `style.toml` changes the map without rebuilding the app.

## 1.7 Text and label placement

The roadmap's main renderer risk (§7). Latin script only, which covers English, French and Spanish, plus Greenlandic.

- **Font:** bundle Noto Sans Regular, Italic (water labels) and Bold, subset to Latin and Latin Extended (about 1–2 MB in all).
- **Glyphs:** a glyph atlas of signed distance fields, drawn in the map's own pass, so halos are cheap and any glyph can be rotated. No complex shaping is needed for Latin script, so start without `rustybuzz`: use kerning from the font. Rasterize with the font crate egui already pulls in, so the binary does not grow (decision 9). Revisit if a script needs shaping.
- **Placement**, from the visible tiles every time the view settles, and incrementally while it moves:
  - point labels for places, POIs and area label points, with candidate positions around the point;
  - line labels for roads and rivers, glyph by glyph along the line, only where the line is straight enough, spaced out along long roads, never upside down;
  - one label per name in the viewport for roads, deduplicated across tile pieces;
  - a tile drawn from a stored ancestor (1.5b's left-out tiles, overzoom, and 1.5c's capped layers) contributes only the ancestor's labels inside that tile's own square, or neighbours' labels appear twice.
- **Collision:** greedy by priority (place class and population, capital, road class, area size), with a screen-space grid index. Labels placed in the previous frame keep their place while they still fit (hysteresis), so panning does not make them jump or flicker. Labels fade in.
- The cost of placement is in the bench report; budget about 3 ms a frame on the dev machine.

**Done when:** places, roads, water and POI names are labelled at every zoom without overlaps, labels stay still while panning, the screenshots show it, and the bench still meets 60 fps p95 on the dev machine.

## 1.8 Shields and POI icons

- **Shield shapes**, drawn as vector shapes tessellated with `lyon` and cached per network and text width, so no image library is needed:
  - US: Interstate, US Highway, and a state shape (circle, oval, rectangle or a few state-specific outlines; start with a per-state table that maps to a few generic shapes).
  - Canada: Trans-Canada, and provincial shapes (Ontario's crowned shield, Quebec's, and generic ones for the rest).
  - Mexico: federal highways. Anything else: a generic rectangle.
- **Placement:** along the road line, using the label collision index of 1.7, repeated at a spacing, with the highest-priority route first. Concurrent routes (I-75 / US-23) show side by side up to two or three shields.
- **POI icons:** one simple icon per POI class (fuel, EV charging, rest area, hotel, restaurant, fast food, hospital, airport, campground, attraction, parking), from an open icon set with a compatible license (e.g. Maki, CC0), stored as vector paths or a small atlas.

**Done when:** Ohio, Ontario and Quebec show the right shields on Interstate, US, state, Trans-Canada and provincial routes, POIs show icons, and each per-country count from 1.3 shows what share of highways gets a real shield.

## 1.9 Viewer UI

Kept small: the map is the app. Owner's choices: scale bar and zoom buttons, and layer toggles.

- **Scale bar** with miles and kilometres (the default unit follows the pack's country, switchable), **zoom buttons**, and the **coordinates** under the cursor.
- **Layer toggles:** land cover, boundaries, POIs (by group: fuel and charging, lodging, food, other), rail, and labels.
- **About this map:** the attribution text from `META`, the build date and input snapshot, and the app version. ODbL attribution is always visible as a small credit line on the map.
- **Verify pack:** runs the `verify` check (1.2) on a background thread with a progress bar. Suggested when a tile fails to decode.
- **Remembered state** in `config.toml`: view, pack, toggles and units.
- Errors are shown, never panics: a missing or damaged pack opens a dialog that says what is wrong.

**Done when:** all of the above works on Linux and Windows, and the config round-trips.

## 1.10 Performance, USB and hardware checks

The exit criteria are checked here. Small checks can happen earlier, for example after 1.5, once the USB stick is available.

- Copy the app and the North America pack to the FAT32 USB stick. Run the flight with a cold cache: on Linux, unmount and remount the stick; on Windows, unplug it.
- Run `--bench` on the dev machine (USB 2 and USB 3 ports), the Windows PC and the 2012-or-newer laptop. Record frame times, tile latency, start-up time, peak RSS, idle CPU and binary size in a table under this step.
- Try the pre-2012 laptop once. Record which wgpu backend it picks and whether it runs.
- Do the two manual checks left open since 0.1 (the Windows `.exe` and the old laptop).
- If a number misses: the levers are the read pattern (prefetch neighbours, bigger reads; V5 tile groups are the format-level lever measured in phase 0), the cache budget, the tessellation and vertex format, label placement cost, and MSAA.

**Done when:** every exit criterion has a measured number, on every machine above, written here and in roadmap §6.

---

## Order of work

```
1.1 tile codec + reader ─► 1.2 build (Delaware, Ohio) ─┬─► 1.3 label + shield data ──────────────┐
                        └─► 1.4 app skeleton ───────────┴─► 1.5 geometry ─► 1.5b ocean tiles ─► 1.5c size experiments ─► 1.6 style ─► 1.7 labels ─► 1.8 shields ─► 1.9 UI ─► 1.10 checks
                                                             (USB check optional here)
```

- 1.4 can start once 1.1 exists, using a Delaware or Ohio pack from 1.2.
- Build the North America pack at the end of 1.2, so the renderer meets continental data early, and again after 1.3.
- 1.3 must be done before 1.7 and 1.8 need its data, but it can run alongside 1.5 and 1.6.
- Keep binary size in the CI summary at every step, so a heavy dependency shows up when it is added.

## Decisions

Settled while planning (2026-10-24):
1. **Positioned reads, not mmap, for pack data.** Pack bytes then live in the OS page cache, which is not the app's RSS, and the 300 MB budget measures what the app holds. `TIDX` blocks are read and cached on demand. The format stays mmap-friendly for later.
2. **Rendering backends:** wgpu with Vulkan, DX12, Metal and its GL backend. Pre-2012 hardware is not a priority: if it does not run with wgpu's GL backend, it is recorded and left for later, with no separate glow renderer in phase 1 (owner, 2026-10-24).
3. **Real route shields in phase 1** (owner, 2026-10-24): from route relations, falling back to `ref`.
4. **Area labels as label points** in a synthetic `labels` layer, computed with polylabel in the builder and stored in one tile only.
5. **Line merging** in the builder before encoding, if it does not make tiles bigger (measured in 1.3: kept, roads 11–13% smaller).
6. **One pack open at a time**, with a picker if `maps/` holds several. Several packs at once is left for later (owner did not choose it for phase 1).
7. **North up, no map rotation** in phase 1, like Streets & Trips. It keeps tile clipping to axis-aligned squares.
8. **The style lives in the app, not the pack:** a compiled-in TOML style with a `style.toml` override, and no `STYL` section.
9. **Text:** SDF glyph atlas in the map pass, Latin script, kerning but no complex shaping; no `cosmic-text`/`rustybuzz` for now. Update roadmap §5 when 1.7 confirms it.
10. **Viewer extras in phase 1:** scale bar, zoom buttons, cursor coordinates and layer toggles (owner, 2026-10-24). No night style yet.
11. **`routes` replaces `ref`** on roads (1.3): it carries the same refs, sorted, with their networks.
12. **`name` only** in tiles, no `name:en`/`name:fr` (1.3).
13. **Tile zoom for a fractional zoom:** `floor(zoom + 0.5)`, overzooming z14 up to z18 (1.4, kept in 1.5).
14. **MSAA 4×** in the window and in `--render`, with `msaa = 1` in `config.toml` to turn it off (1.5).
15. **Left-out tiles** (1.5b, owner 2026-10-24): every missing tile is drawn from its nearest stored ancestor, with no flag. Slivers cannot occur while every tolerance is under half the buffer, which the builder asserts. Tiles that one polygon covers are left out, ocean or not, when the parent shows that polygon.

Open, to settle in their steps:
- **Frame overhead and per-layer max zoom** (design review, 2026-10-24): measured in [step 1.5c](#15c-tile-size-experiments-frame-overhead-and-per-layer-max-zoom); the owner decides what to keep from the numbers and screenshots.
- The state-by-state shield table: how many distinct shapes are worth drawing (1.8).
- The default units rule for the scale bar in a pack that covers both countries (1.9).

## Out of scope for phase 1

- Search, addresses and "what's here" (phase 2); routing (phase 3).
- Builder memory for a 16–32 GB machine (phase 5); North America keeps building in 3 bands on the dev machine.
- Night style, map rotation, 3D, terrain or hillshading.
- Several packs at once.
- Scripts other than Latin.
- A separate OpenGL (glow) renderer for pre-2012 hardware.

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
| 1.3 Label and shield data in the builder | Not started | |
| 1.4 App skeleton: pack, camera, tile loading, bench | Not started | |
| 1.5 Geometry renderer | Not started | |
| 1.6 Style file and default style | Not started | |
| 1.7 Text and label placement | Not started | |
| 1.8 Shields and POI icons | Not started | |
| 1.9 Viewer UI | Not started | |
| 1.10 Performance, USB and hardware checks | Not started | Needs the USB stick, the Windows PC and both laptops. |

## Environment notes

As in [phase 0](phase0-plan.md#environment-notes): builder data in `$OMP_WORK` on the exFAT disk, `target/` on `/`. Packs are written to `$OMP_WORK/packs/`. For the app, put a pack under `maps/` next to the executable, or pass `--pack <dir>`.

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
- Decide whether to keep `name:en` and `name:fr`. Default: the plain `name` only, which is what is signed locally.

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
  - one label per name in the viewport for roads, deduplicated across tile pieces.
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
                        └─► 1.4 app skeleton ───────────┴─► 1.5 geometry ─► 1.6 style ─► 1.7 labels ─► 1.8 shields ─► 1.9 UI ─► 1.10 checks
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
5. **Line merging** in the builder before encoding, if it does not make tiles bigger (measured in 1.3).
6. **One pack open at a time**, with a picker if `maps/` holds several. Several packs at once is left for later (owner did not choose it for phase 1).
7. **North up, no map rotation** in phase 1, like Streets & Trips. It keeps tile clipping to axis-aligned squares.
8. **The style lives in the app, not the pack:** a compiled-in TOML style with a `style.toml` override, and no `STYL` section.
9. **Text:** SDF glyph atlas in the map pass, Latin script, kerning but no complex shaping; no `cosmic-text`/`rustybuzz` for now. Update roadmap §5 when 1.7 confirms it.
10. **Viewer extras in phase 1:** scale bar, zoom buttons, cursor coordinates and layer toggles (owner, 2026-10-24). No night style yet.

Open, to settle in their steps:
- Tile zoom choice for a fractional zoom, and the overzoom limit (1.4, 1.5).
- `name` only, or also `name:en`/`name:fr` (1.3).
- The state-by-state shield table: how many distinct shapes are worth drawing (1.8).
- The default units rule for the scale bar in a pack that covers both countries (1.9).

## Out of scope for phase 1

- Search, addresses and "what's here" (phase 2); routing (phase 3).
- Builder memory for a 16–32 GB machine (phase 5); North America keeps building in 3 bands on the dev machine.
- Night style, map rotation, 3D, terrain or hillshading.
- Several packs at once.
- Scripts other than Latin.
- A separate OpenGL (glow) renderer for pre-2012 hardware.

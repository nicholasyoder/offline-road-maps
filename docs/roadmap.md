# Offline Road Maps: Roadmap

*Draft 2. Updated 2026-10-23 with the project owner's answers to the scoping questions; phase 5 (settings, map styles and polish) added 2026-10-26.*

## 1. Vision

This is a portable desktop **car trip planner** in the spirit of Microsoft Streets & Trips, built on current OpenStreetMap data. It is for people who have no internet access.

- **Audience and distribution.** This is a personal project. The maintainer runs the builder and hands out the app and the map data together on physical media, such as a flash drive or an external disk. The repo also documents how to run the builder, so anyone can make their own region pack.
- **One self-contained app.** There is no server, no install and no network access at runtime. Copy the folder to a drive and run it.
- **Coverage.** North America is the primary target. Any region can be built, from one state up to a continent. A whole-planet pack under 100 GB is a stretch goal.
- **Small and light.** The app binary should be about 30 MB or less. It should use less than about 300 MB of RAM and close to 0% CPU when idle, and stay usable on old laptops with integrated graphics.
- **Platforms.** Linux and Windows first, then macOS. Mobile is out of scope for now.
- **Features.** Car only:
  - a map display of roads, places, water, land cover, rail and POIs that matter for car trips;
  - search with **full house-number address support**;
  - multi-stop route planning with driving directions.

**Not included:**
- building outlines;
- hiking and footpaths, and cycling;
- walking and bike routing;
- live GPS and turn-by-turn navigation, at least for the proof of concept.

```
              ┌──────────────────────── shared Rust core (format + algorithms) ────────────────────────┐
OSM .pbf  ─┐
OpenAddr.  ├─► BUILDER (CLI, maintainer's machine) ──► region pack ──► physical media ──► APP (desktop)
TIGER      │   fetch → filter → merge addresses          *.omp files                       render · search · route
StatCan    ┘   → encode tiles → routing graph            (mmap-able, FAT32-safe chunks)
               → search index
```

## 2. Does this already exist?

Not quite. **CoMaps**, the community fork of Organic Maps, is the closest.

- It is a C++/Qt app with offline search and car, bike and foot routing.
- A Linux build has been on Flathub since January 2026, and there is a macOS build.
- Windows support is unclear: a build shows up on winget, but the project's own pages don't list Windows.
- It is mobile-first, its own listing says the desktop build lacks feature parity, and it has no trip-planner features.

Summing CoMaps' `countries.txt` (data version 261004) gives these sizes, which are **the benchmark to beat**:

| Region | CoMaps size |
|---|---|
| World | about 99 GB |
| USA | about 15.8 GB |
| Canada | about 6.6 GB |

Its data includes buildings, paths and walking/bike data that this project leaves out.

Everything else is unmaintained, server-based (Dawarich Atlas), Windows-only (MapFactor), or has no real desktop app (OsmAnd; OffRoad has been dead since 2021). See draft 1 in the git history for the full comparison.

**Decision:** build from scratch in Rust, for full control over storage, and borrow ideas from CoMaps and Protomaps as needed.

## 3. Data sources

| Source | Used for | License | Notes |
|---|---|---|---|
| OSM planet or Geofabrik extracts (`.osm.pbf`) | Roads, places, POIs, land and water, OSM addresses | ODbL (attribution and share-alike apply to the derived database) | The planet file is about 87 GB (July 2026). Geofabrik offers continent, country and US-state extracts. |
| **OpenAddresses** | Fills house-number gaps in the US and Canada | Varies per source. Some sources need attribution and some are share-alike. Licenses are listed in each source's metadata, but many sources name none (89 of 92 in Ohio). | **Essential for "full address support"**, because OSM house-number coverage in North America is patchy. OSM alone has 12% of Ohio's merged addresses and 34–40% in Ontario and Quebec. Sources that name no license are used when publicly available (decided 2026-10-24). Without them North America would drop from 151.5M to 86.8M addresses. The builder must keep a list of the sources used and their attribution. |
| **US Census TIGER/Line `ADDRFEAT`** | House-number ranges along each US street block, used to estimate a location when no exact address point exists | Public domain | Distributed per county, for example `tl_2025_<county>_addrfeat.zip`. These are *potential* ranges, so the app labels estimated locations "approximate". |
| **Statistics Canada National Address Register (NAR)** | Canadian address points | Statistics Canada Open Licence (attribution) | Updated regularly (release `202606` used in Phase 0); one national zip of about 1.6 GB. Building coordinates for 99.8% of Quebec and 82% of Ontario civic addresses; most of the rest have only a blockface point. Chosen over the 2021 **Open Database of Addresses** (ODA, OGL – Canada), which has fewer addresses (Quebec 1.2M vs 3.2M) and adds only 1.1% in Ontario and 0.2% in Quebec on top of NAR, so it is not used. See phase 0 steps [0.2](phase0-plan.md#02-test-regions-and-fetch) and [0.4](phase0-plan.md#04-address-spike-ohio-first-then-ontario-and-quebec). |

The builder **deduplicates** across sources, matching on normalized street, house number and a distance threshold (250 m on the same street, from phase 0). OSM wins when sources conflict. Each address keeps a 1-bit "source" flag so the app can show where it came from. OSM `addr:interpolation` ways are kept as ranges, like TIGER.

## 4. Size budget (measured in Phase 0)

Leaving out buildings, paths and walking/bike data removes most of the bulk. House numbers add some back. The full table, with Ohio, Ontario, Quebec and the CoMaps comparison, is in [size-table.md](size-table.md).

| Layer | Estimate before Phase 0 | **North America, measured** | Planet (extrapolated) |
|---|---|---|---|
| Rendering tiles: no buildings or paths, zoom 14 max, simplified, custom encoding plus zstd dictionaries | 3–5 GB | **6.1 GiB** with the index (V4). Roads 1.8 GiB, water 1.7, land cover 1.2, waterways 1.1. Tiles that one polygon covers (open ocean, big parks) are left out and drawn from their parent ([phase 1 step 1.5b](phase1-plan.md#15b-leave-out-full-ocean-tiles)), and land cover and water areas stop at z13, drawn from there at z14 and above, with tile frames 8 bytes lighter ([step 1.5c](phase1-plan.md#15c-tile-size-experiments-frame-overhead-and-per-layer-max-zoom)). Together they took the pack from 9.0 GiB, after phase 1 step 1.5 fixed the open ocean that phase 0's reader dropped, to 6.1. Water and land cover are simplified to about 6.5 m, with every lake, wood and park kept. Driveways are included (about 0.14 GiB). | about 30 GiB (scaled from 38 by the North America change) |
| Car routing graph (contraction hierarchy) | 1–1.5 GB | **2.64 GiB** (2,699 MiB) as built in [phase 3](phase3-plan.md#39-performance-accuracy-and-hardware-checks), with private roads kept as restricted (decision 14): the CH (68.4M nodes) in 1,335 MiB and the road segments with their geometry and names in 1,350 MiB; phase 0 estimated 3.2 GiB for the CH alone | about 16 GiB |
| Search: places, streets, POIs | about 0.5 GB | **0.24 GiB** measured in phase 2 (14.5M entries with postcodes, including streets known only from addresses; admin areas 14 MiB, the street table 332 MiB and the spatial grid 8 MiB on top). Search and addresses together add 997 MiB to the pack ([phase 2 step 2.9](phase2-plan.md#29-performance-accuracy-and-hardware-checks)). | about 1.2 GiB |
| **House-number addresses** | 1.5–3 GB | **0.38 GiB** for 151.2M merged addresses as `build` writes them (phase 2 step 2.4: 2.34 B each with postcodes, plus 55 MiB of US TIGER ranges; street names are counted under search; phase 0 measured 0.36 GiB). Without OpenAddresses sources that name no license: 86.8M. | depends on open data elsewhere |
| **Total** | 6–10 GB targeted | **9.8 GiB (10.5 GB)** as built in phase 3, routing included (9,996 MiB in 3 chunks; phase 2's pack without routing was 7,210 MiB). About 0.45× CoMaps' maps of the same area | **about 42–50 GiB**, well under the 100 GB stretch target |

The total, routing included, is just over the original 6–10 GB target (10.5 GB) and well under Phase 0's 15 GB stop line, so scope stays as it is. The levers left, if size matters later, are the smallest ponds and streams (waterways are 1.1 GiB, 0.6 of it at z14, where streams start), V5 tile groups (8% smaller tiles, at about 2.5× the bytes read per viewport), and capping land cover at z12 (about 0.5 GiB more, but it visibly drops small patches; [step 1.5c](phase1-plan.md#15c-tile-size-experiments-frame-overhead-and-per-layer-max-zoom)).

Addresses are stored grouped by street rather than as standalone points. Each street keeps a sorted run of `(house number, offset along the street)` pairs, delta-encoded. This should come to only a few bytes per address instead of a full coordinate and name each time. That grouping is the main reason the address budget is feasible. Measured in phase 0 (A3, with zstd over blocks of streets): 1.9–2.0 bytes per address in the test regions and 2.18 for North America, units and unattached points included. The layout is in [format.md](format.md#addresses).

## 5. Key technical decisions

| Area | Decision | Notes |
|---|---|---|
| Language | **Rust**, as a Cargo workspace with `core`, `builder` and `app` crates. | `core` holds the format reader and writer and the shared algorithms, so the builder and the app can never disagree about the format. |
| Rendering | A custom 2D vector renderer: `wgpu` with `lyon` to turn shapes into triangles, plus an atlas of signed distance field glyphs, which also holds the route shields and POI icons (phase 1 step 1.8). Text is shaped with `harfrust` and drawn from `skrifa` outlines, the crates egui already uses, in bundled Noto Sans subset to Latin (phase 1 step 1.7). | Fallback if this stalls: convert to standard map tiles (MVT) and use MapLibre Native. Scripts other than Latin need more font coverage, not a new text stack. |
| UI | egui/eframe. | It is tiny and lets the map be drawn inside the UI. Could move to Slint later for polish. |
| Container | One region pack is a header plus memory-mappable sections: tiles, routing, search, addresses and metadata. It is split into **chunks under 4 GB** for FAT32. Specified in [format.md](format.md). | Tiles are ordered along a Hilbert curve, which keeps nearby tiles together on disk so reads stay fast on slow USB drives. The OS page cache does the caching. |
| Routing | **Contraction hierarchies, car profile only**, built in the builder. | Covers one-ways, turn restrictions, speed by road class and `maxspeed`, and avoiding tolls and ferries (these flags are built into the graph; no "avoid highways" or turn costs, [phase 3 decisions 2 and 10](phase3-plan.md#decisions)). User-drawn "avoid this area" may later need Multi-Level Dijkstra or customizable routing instead. |
| Search | An `fst` index over normalized tokens, an admin-boundary hierarchy for "123 Main St, Springfield, IL", and a spatial grid for reverse lookup. | Normalizes US and Canadian street suffixes and directions ("St" = "Street", "N" = "North"). Handles French for Quebec. |
| Builder | Reads OSM data as a stream and keeps the locations of needed nodes only, in a rank-indexed bitset with 8 bytes per node (13.9 GiB for North America). Includes a `fetch` subcommand for Geofabrik, OpenAddresses, TIGER and StatCan. | The target is building North America on a 16–32 GB RAM desktop, and the planet may need 64 GB. Phase 0 peaks at 43 GB for North America, in 3 bands (one pass needs over 56 GB). Reaching the target needs features spilled to disk by tile. A full North America `build --addresses all --bands 3` takes 36.7 min at a 47 GiB peak ([refactor R4](refactor-plan.md#r4-build-time): each pass reads only the blobs of the element types it needs); 13.6 min of that is the address merge. `--reuse-stages` reuses the admin areas, tiles and addresses for dev loops. |
| Portable app | Config, pins and saved trips live next to the executable. | The app makes no network calls at all. |

## 6. Phased roadmap

### Phase 0: Measure and prototype
Detailed plan: [phase0-plan.md](phase0-plan.md).

- [x] Set up the Cargo workspace, plus CI cross-builds for Windows and Linux.
- [x] Choose test regions: one US state (for example Ohio), one Canadian province, then all of North America.
- [x] Builder spike: read the PBF, apply a car-relevant tag whitelist, and measure size per layer with a few encoding variants.
- [x] Address spike: merge OSM, OpenAddresses and TIGER for the test state. Measure coverage gained and bytes per address.
- [x] **Exit criterion:** a confirmed size table and `docs/format.md` describing the file format. Done 2026-10-24: [size-table.md](size-table.md), [format.md](format.md).

### Phase 1: Map viewer MVP
Detailed plan: [phase1-plan.md](phase1-plan.md).

- [x] Builder: simplify geometry per zoom level, compute label points, read route relations for highway shields, and write the pack. Done 2026-10-24: [phase 1 steps 1.2 and 1.3](phase1-plan.md#13-label-and-shield-data-in-the-builder).
- [x] App: pan and zoom; draw polygons, lines and labels with collision avoidance; highway shields and POI icons; a style file; a scale bar, zoom buttons and layer toggles; portable mode. Done 2026-10-25: [phase 1 steps 1.4–1.9](phase1-plan.md#19-viewer-ui); checked on Windows in 1.10.
- [x] **Exit criterion:** browse all of North America from a USB stick at a smooth frame rate with less than 300 MB RAM. Accepted 2026-10-25 by the owner, with the HD 4000's Windows GL leak left as a known upstream issue, to revisit if it shows up on a machine the app is really used on.
  Measured 2026-10-25 ([phase 1 step 1.10](phase1-plan.md#110-performance-usb-and-hardware-checks)): from a FAT32 USB 2 stick with a cold cache, the dev machine draws at 1.17 ms p95, starts in 1.46 s, has any jump fully detailed within 0.63 s, and peaks at 144 MiB RSS. Two Windows laptops pass the 30 fps target (Latitude 7490 on DX12 12–14 ms; Latitude E6430 on GL 20 ms). Not met everywhere: the 7490 on Windows peaks at 373 MiB (flat), and the E6430's Windows GL driver leaks about 0.35 MiB per frame presented, outside the app. A pre-2012 laptop gets only a software renderer (~3 fps).

### Phase 2: Search and addresses
Detailed plan: [phase2-plan.md](phase2-plan.md).

- [x] Builder: admin hierarchy, places, streets, POIs, the merged address store and TIGER ranges. Done 2026-10-26: [phase 2 steps 2.1–2.5](phase2-plan.md#25-spatial-grid-and-core-readers).
- [x] App: search suggestions as you type, ranking by importance and distance from the map view, address lookup (with "approximate" marking for interpolated addresses), and a "what's here" lookup on click. Done 2026-10-26: [phase 2 steps 2.6–2.8](phase2-plan.md#28-whats-here).
  Measured 2026-10-26 ([phase 2 step 2.9](phase2-plan.md#29-performance-accuracy-and-hardware-checks)), North America: random addresses 96.5% first as typed and 97.6% after a pause, hand-written queries 94.5% first and 98.7% in the top 5, "what's here" 99.5%. Search and addresses add 997 MiB. Keystroke p95 warm 99 ms on the dev machine and 241 ms on the Latitude E6430; "what's here" within 335 ms cold everywhere. Not met: the first address search from a cold USB 2 stick takes 1.1–1.3 s (2.4–2.6 s on the 7490, whose CPU runs slow), the 7490 on Windows peaks at 473 MiB, and the E6430's GL leak remains. Accepted 2026-10-26 by the owner as small issues ([decision 9](phase2-plan.md#decisions)); the 7490's slow CPU was its low battery.

### Between phases 2 and 3: code review fixes
Detailed plan: [refactor-plan.md](refactor-plan.md). Bug fixes, shared code, the builder's production modules out of the Phase 0 spikes, faster builds, and splitting the app's search UI and the search engine. No format change.

- [x] **Done 2026-10-27:** all six steps ([status](refactor-plan.md#status)). Two bugs fixed; shared geometry and helpers in one place each; the builder's production code out of the spikes; North America `build --addresses all` 52.6 → 36.7 min (−30.4%) at a 47 GiB peak; the app's search UI and the search engine split into modules. Packs byte-identical and search results unchanged throughout. Left open: profiling the address merge (R4.5), now the largest build stage.

### Phase 3: Car routing
Detailed plan: [phase3-plan.md](phase3-plan.md).

- [x] Builder: road graph, car profile, turn restrictions and contraction hierarchies. Done 2026-10-27: [phase 3 steps 3.1–3.3](phase3-plan.md#33-routing-format-and-core-reader).
- [x] App: A to B and multi-stop routing, a directions list, distance and time, a route overlay, and avoid options. Done 2026-10-28: [phase 3 steps 3.4–3.8 and 3.10](phase3-plan.md#310-search-and-route-polish-from-the-hand-checks).
  Measured 2026-10-28 ([phase 3 step 3.9](phase3-plan.md#39-performance-accuracy-and-hardware-checks)), North America: exact against Dijkstra, the route set 72 of 72, 99.7% of address pairs routed, random pairs 98.6–99.4% within 15% of OSRM; routing adds 2,699.5 MiB; warm short routes 8–69 ms on every machine. Not met, accepted by the owner ([decisions 15 and 16](phase3-plan.md#decisions)): the first route from a cold USB 2 stick (3.2–8.0 s against 3 s), warm coast to coast on the laptops (1.9–2.5 s against 1.25 s), RSS (dev 322 MiB against 300 MB), and the hand-written routes against OSRM (76–77%).

### Phase 4: Trip planner features
Detailed plan: [phase4-plan.md](phase4-plan.md) (draft 2, 2026-10-28).

- [ ] Stop optimization (a travelling-salesman heuristic), with first and last stops fixed or open.
- [ ] Planned departure and arrival times, in local time (marked when in another zone than the departure's), and overnight stops. Fuel or rest-stop suggestions along the route only if they are cheap in disk, RAM and CPU (owner, 2026-10-28): the plan uses only the POIs already in the pack. **Times, stops and overnights done 2026-10-28 ([phase 4 step 4.5](phase4-plan.md#45-schedule-times-stops-and-overnights)):** format 0.6 adds the time zones (0.39 MiB for North America); suggestions are step 4.6.
- [ ] Saved trips and pushpins. GPX, CSV and KML import and export are best effort: each format is dropped if it gets complicated (owner, 2026-10-28).
- [ ] Printing **on a real printer** (Windows and Linux; owner, 2026-10-28): the map on one page, framed by a rectangle over the map with a print preview, with the route line, a scale bar and a label the user types; the directions from their own button. The same pages can be saved as PDF.
- [x] Alternative routes (added 2026-10-28, from trying phase 3's routes): after the quickest route, a few that take clearly different roads (via-node alternatives over the same hierarchy), each scored again with what the hierarchy leaves out, mainly turn and U-turn costs (phase 3 decisions 2 and 13), so an avoidable U-turn or a run of left turns can lose to a route a little slower on paper. Shown in grey beside the chosen one, to pick from. It cannot make a route faster than the hierarchy's: that one is already the quickest under the profile. **Found and ranked in `omp-core` 2026-10-28 ([phase 4 step 4.2](phase4-plan.md#42-alternative-routes-omp-core)); shown and picked in the app the same day ([4.3](phase4-plan.md#43-alternatives-in-the-app)).**
- [x] U-turn penalty, measured first (added 2026-10-28, [phase 3 decisions 13 and 16](phase3-plan.md#decisions)): a hidden penalty of a few minutes on U-turns at junctions, counted in what the route minimizes but not in the time shown, as private roads' entry penalty is. Unlike that penalty, which sits on an edge, a U-turn is a pair of edges at a node, which the node-based graph cannot price: each junction's node must be split per incoming road, as turn restrictions are, so the U-turn's arc can carry the cost. A penalty rather than a ban keeps dead ends and cul-de-sacs routable. Measure on Ohio and Quebec first: nodes and routing bytes added (decision 13 guessed about twice the nodes at intersections), U-turns left against OSRM's 1.9% and 6.8%, and times against OSRM; then decide whether it is worth the size on North America, against the alternatives' rescoring above. **Measured 2026-10-28 ([phase 4 step 4.4](phase4-plan.md#44-u-turn-penalty-measured-first)): not taken.** It doubles the routing sections and removes only a third of the U-turns; the rest cross medians, which only a cost on the turn's angle sees. Step 4.2 measured that the rescoring above removes few of them (4.7% → 4.6% of Ohio's routes), since an alternative differs by more than a block.

### Phase 5: Settings, map styles and polish
Added 2026-10-26. The settings already in `config.toml` (units, MSAA, layers, the last view) can only be changed in the file or in scattered places in the UI. This phase gives them one home and smooths the rough edges found in real use.

- [ ] A Settings window from the menu, saved to `config.toml` next to the executable: units, zoom speed for the wheel (now a quarter zoom per notch, set 2026-10-26) and for the keys, pan inertia, UI and label size, anti-aliasing, and whether to reopen the last view. UI size multiplies the scale the system reports (egui's zoom factor): on X11 that comes from `Xft.dpi`, else the screen's physical size, so the Latitude 7490's 14" 1080p panel on Debian gets 1.67 px/pt where its Windows uses 1.25, and everything looks too large (phase 3 step 3.9). Until then, `WINIT_X11_SCALE_FACTOR=1.25` or `Xft.dpi: 120` in `~/.Xresources` sets it there.
- [ ] A few built-in map styles chosen in Settings and switched without a restart: the default, a dark or night style, and a light, high-contrast style for printing (phase 4's printing). A `style.toml` the user adds next to the executable is listed with them. The UI follows a dark style.
- [ ] UI polish: a keyboard shortcut list, tooltips, the window size and position remembered, a clear first-run screen when no map is found, and plain error messages.
- [ ] Map tuning from use: road widths and colours by zoom (widths reworked 2026-10-26: each class is the minor road width plus a small step per rank), label density, and POI and shield priorities.
- [ ] A performance pass over the whole project (added 2026-10-28, after [phase 3 step 3.9](phase3-plan.md#39-performance-accuracy-and-hardware-checks)): profile pack size, RAM and time end to end, builder and app, and take what is worth it. Targets that phases 1–3 missed and accepted, to get back toward: the first search and the first route from a cold USB 2 stick (1.1–1.3 s and 3.2–8.0 s against 1 and 3 s), long routes and directions on older laptops (E6430 coast to coast 2.5 s against 1.25 s; two thirds of it the directions), RSS (dev 322 MiB against 300 MB; Windows 555 MiB on DX12), and the pack (10.5 GB against the 6–10 GB aimed for). Leads already noted: directions decoding each junction's cell (phase 3 step 3.5), showing the route line before its directions, routing caches of 8 + 8 MiB (−21 MiB), search's 41 MiB of readers, background reads of hot search and routing data after start-up, and the size levers in §4.
- [ ] **Exit criterion:** every setting is reachable from the UI and survives a restart; styles switch at runtime; the performance pass's findings are measured and written up, with the ones taken in.

### Phase 6: Distribution
- [ ] `docs/BUILDING.md`: hardware needs, a step-by-step region build, and a custom region from a polygon.
- [ ] Release layout for physical media: `app/` holding the Windows `.exe`, a Linux AppImage or binary and a macOS build, plus `maps/` and `README.txt` Linux mounts FAT32 without exec, so the Linux app needs a launcher run with `sh` (phase 1 step 1.10).
- [ ] Attribution screen and file covering ODbL and each address source.
- [ ] Planet build trial (stretch goal).

### Later or optional
- A larger POI set (shops, schools, offices and other named places beyond the 11 car-trip classes), added to the map and search together. To consider once the app works end to end and the final pack sizes are known (decided 2026-10-25, phase 2 planning).
- GPS position display (NMEA, gpsd, Windows Location API).
- macOS polish.
- Mobile.

## 7. Risks
- **Text labels** are the hardest part of the renderer. Start with Latin-script place labels. (Phase 1 step 1.7: Latin labels for places, roads, water and POIs are placed in about 0.3 ms a frame; names in other scripts are left out.)
- **Address merging quality.** Duplicates and conflicts across sources can produce bad results. Phase 0 must measure this on real data.
- **Address licensing.** Some OpenAddresses sources are share-alike or non-commercial, and many name no license. Sources with no license are used when publicly available (decided 2026-10-24). This is fine for private distribution, but the builder keeps `--exclude-license` so sources can be filtered if packs are ever shared more widely.
- **Builder resources.** North America needs about 35 GB of input (19 GB OSM, 10 GB OpenAddresses, 3.5 GB TIGER, 2 GB StatCan, 1 GB ocean polygons), plus about 12 GB of temporary tiles. Document the hardware requirements.

## 8. Licensing
- The app code is dual-licensed `MIT OR Apache-2.0`. CoMaps is Apache-2.0, so its ideas and code can be borrowed.
- The app bundles Noto Sans, under the SIL Open Font License 1.1 (`crates/app/data/fonts/OFL.txt`).
- The POI icons are from Maki, under CC0 1.0 (`crates/app/data/icons/LICENSE.txt`).
- Each pack embeds an attribution file listing ODbL and every address source used.

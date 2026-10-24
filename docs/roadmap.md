# Offline Maps: Roadmap

*Draft 2. Updated 2026-10-23 with the project owner's answers to the scoping questions.*

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
| **OpenAddresses** | Fills house-number gaps in the US and Canada | Varies per source. Some sources need attribution and some are share-alike. Licenses are listed in each source's metadata, but many sources name none (89 of 92 in Ohio). | **Essential for "full address support"**, because OSM house-number coverage in North America is patchy. OSM alone has 12% of Ohio's merged addresses and 34–40% in Ontario and Quebec. Without the sources that name no license, Ohio drops from 4.95M to 1.13M addresses. The builder must keep a list of the sources used and their attribution. |
| **US Census TIGER/Line `ADDRFEAT`** | House-number ranges along each US street block, used to estimate a location when no exact address point exists | Public domain | Distributed per county, for example `tl_2025_<county>_addrfeat.zip`. These are *potential* ranges, so the app labels estimated locations "approximate". |
| **Statistics Canada National Address Register (NAR)** | Canadian address points | Statistics Canada Open Licence (attribution) | Updated regularly (release `202606` used in Phase 0); one national zip of about 1.6 GB. Building coordinates for 99.8% of Quebec and 82% of Ontario civic addresses; most of the rest have only a blockface point. Chosen over the 2021 **Open Database of Addresses** (ODA, OGL – Canada), which has fewer addresses (Quebec 1.2M vs 3.2M) and adds only 1.1% in Ontario and 0.2% in Quebec on top of NAR, so it is not used. See phase 0 steps [0.2](phase0-plan.md#02-test-regions-and-fetch) and [0.4](phase0-plan.md#04-address-spike-ohio-first-then-ontario-and-quebec). |

The builder **deduplicates** across sources, matching on normalized street, house number and a distance threshold (250 m on the same street, from phase 0). OSM wins when sources conflict. Each address keeps a 1-bit "source" flag so the app can show where it came from. OSM `addr:interpolation` ways are kept as ranges, like TIGER.

## 4. Size budget (estimates, and Phase 0 measurements)

Leaving out buildings, paths and walking/bike data removes most of the bulk. House numbers add some back.

| Layer | North America | Planet (stretch) |
|---|---|---|
| Rendering tiles: no buildings or paths, zoom 14 max, simplified, custom encoding plus zstd dictionaries | 3–5 GB estimated; **9.2 GiB measured** (phase 0.5, V4). Water is half of it: lakes and streams are 40% of z14 bytes. | 15–25 GB |
| Car routing graph (contraction hierarchy) | 1–1.5 GB estimated; **about 3.2 GiB** from the measured graph (62.5M nodes, 150M edges) at the regional CH-to-graph ratio | 3–6 GB |
| Search: places, streets, POIs | about 0.5 GB estimated; **0.25 GiB or less** measured (no admin hierarchy yet) | 2–4 GB |
| **House-number addresses** (about 150–200M points in North America after merging) | 1.5–3 GB estimated; **0.36 GiB measured** for 151.5M merged addresses (2.18 B each, plus 50 MiB of US TIGER ranges; street names are counted under search). Without OpenAddresses sources that name no license: 86.8M. | 6–15 GB (depends on open data elsewhere) |
| **Total** | **about 6–10 GB** targeted; **about 13 GiB measured** before any cut-offs for lakes, streams, protected areas and `service` roads | **about 30–50 GB** — well under the 100 GB stretch target |

Addresses are stored grouped by street rather than as standalone points. Each street keeps a sorted run of `(house number, offset along the street)` pairs, delta-encoded. This should come to only a few bytes per address instead of a full coordinate and name each time. That grouping is the main reason the address budget is feasible. Measured in phase 0 (A3, with zstd over blocks of streets): 1.9–2.0 bytes per address, units and unattached points included.

## 5. Key technical decisions

| Area | Decision | Notes |
|---|---|---|
| Language | **Rust**, as a Cargo workspace with `core`, `builder` and `app` crates. | `core` holds the format reader and writer and the shared algorithms, so the builder and the app can never disagree about the format. |
| Rendering | A custom 2D vector renderer: `wgpu` with `lyon` to turn shapes into triangles, plus a glyph atlas. Text uses `cosmic-text`/`rustybuzz`. | Fallback if this stalls: convert to standard map tiles (MVT) and use MapLibre Native. |
| UI | egui/eframe. | It is tiny and lets the map be drawn inside the UI. Could move to Slint later for polish. |
| Container | One region pack is a header plus memory-mappable sections: tiles, routing, search, addresses and metadata. It is split into **chunks under 4 GB** for FAT32. | Tiles are ordered along a Hilbert curve, which keeps nearby tiles together on disk so reads stay fast on slow USB drives. The OS page cache does the caching. |
| Routing | **Contraction hierarchies, car profile only**, built in the builder. | Covers one-ways, turn restrictions, speed by road class and `maxspeed`, and avoiding tolls, ferries and highways (these flags are built into the graph). User-drawn "avoid this area" may later need Multi-Level Dijkstra or customizable routing instead. |
| Search | An `fst` index over normalized tokens, an admin-boundary hierarchy for "123 Main St, Springfield, IL", and a spatial grid for reverse lookup. | Normalizes US and Canadian street suffixes and directions ("St" = "Street", "N" = "North"). Handles French for Quebec. |
| Builder | Reads OSM data as a stream and keeps the locations of needed nodes only, in a rank-indexed bitset with 8 bytes per node (13.9 GiB for North America). Includes a `fetch` subcommand for Geofabrik, OpenAddresses, TIGER and StatCan. | The target is building North America on a 16–32 GB RAM desktop, and the planet may need 64 GB. Phase 0 peaks at 43 GB for North America, in 3 bands (one pass needs over 56 GB). Reaching the target needs features spilled to disk by tile. |
| Portable app | Config, pins and saved trips live next to the executable. | The app makes no network calls at all. |

## 6. Phased roadmap

### Phase 0: Measure and prototype
Detailed plan: [phase0-plan.md](phase0-plan.md).

- [ ] Set up the Cargo workspace, plus CI cross-builds for Windows and Linux.
- [ ] Choose test regions: one US state (for example Ohio), one Canadian province, then all of North America.
- [ ] Builder spike: read the PBF, apply a car-relevant tag whitelist, and measure size per layer with a few encoding variants.
- [ ] Address spike: merge OSM, OpenAddresses and TIGER for the test state. Measure coverage gained and bytes per address.
- [ ] **Exit criterion:** a confirmed size table and `docs/format.md` describing the file format.

### Phase 1: Map viewer MVP
- [ ] Builder: simplify geometry per zoom level, compute label points, and write the pack.
- [ ] App: pan and zoom; draw polygons, lines and labels with collision avoidance; a style file; portable mode.
- [ ] **Exit criterion:** browse all of North America from a USB stick at a smooth frame rate with less than 300 MB RAM.

### Phase 2: Search and addresses
- [ ] Builder: admin hierarchy, places, streets, POIs, the merged address store and TIGER ranges.
- [ ] App: search suggestions as you type, ranking by importance and distance from the map view, address lookup (with "approximate" marking for interpolated addresses), and a "what's here" lookup on click.

### Phase 3: Car routing
- [ ] Builder: road graph, car profile, turn restrictions and contraction hierarchies.
- [ ] App: A to B and multi-stop routing, a directions list, distance and time, a route overlay, and avoid options.

### Phase 4: Trip planner features
- [ ] Stop optimization (a travelling-salesman heuristic), with first and last stops fixed or open.
- [ ] Planned departure and arrival times, overnight stops, and fuel or rest-stop suggestions along the route.
- [ ] Saved trips and pushpins, with GPX, CSV and KML import and export.
- [ ] Print or export directions and the map to PDF.

### Phase 5: Distribution
- [ ] `docs/BUILDING.md`: hardware needs, a step-by-step region build, and a custom region from a polygon.
- [ ] Release layout for physical media: `app/` holding the Windows `.exe`, a Linux AppImage or binary and a macOS build, plus `maps/` and `README.txt`.
- [ ] Attribution screen and file covering ODbL and each address source.
- [ ] Planet build trial (stretch goal).

### Later or optional
- GPS position display (NMEA, gpsd, Windows Location API).
- macOS polish.
- Mobile.

## 7. Risks
- **Text labels** are the hardest part of the renderer. Start with Latin-script place labels.
- **Address merging quality.** Duplicates and conflicts across sources can produce bad results. Phase 0 must measure this on real data.
- **Address licensing.** Some OpenAddresses sources are share-alike or non-commercial. Fine for private distribution, but the builder should be able to filter sources by license.
- **Builder resources.** North America needs about 35 GB of input (19 GB OSM, 10 GB OpenAddresses, 3.5 GB TIGER, 2 GB StatCan, 1 GB ocean polygons), plus about 12 GB of temporary tiles. Document the hardware requirements.

## 8. Licensing
- The app code is dual-licensed `MIT OR Apache-2.0`. CoMaps is Apache-2.0, so its ideas and code can be borrowed.
- Each pack embeds an attribution file listing ODbL and every address source used.

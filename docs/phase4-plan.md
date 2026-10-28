# Phase 4 Plan: Trip Planner Features

*Draft 2, 2026-10-28, with the owner's answers to the planning questions (decisions 14–19) and decision 12 reworded; decisions 1–13 confirmed by the owner the same day. Expands Phase 4 of [roadmap.md](roadmap.md#phase-4-trip-planner-features). Builds on phase 3's routing ([phase3-plan.md](phase3-plan.md)): the contraction hierarchy and its queries in `omp_core::route`, the route page and side panel (steps 3.7 and 3.8, which already hold this phase's placeholders), and `omp-app --render`'s offscreen renderer.*

## Purpose

Phase 4 makes the app a trip planner rather than only a router. When it is done, someone can enter a dozen places to visit, press "Optimize stop order", see two alternative roads for a leg and pick one, set "Depart Saturday 8:00, drive until 18:00" and get arrival times in each stop's local time, with day breaks and a suggested overnight town. Times that are in another zone than the departure's are marked. They can save the trip and a set of pushpins next to the app, swap them with other tools as GPX, KML or CSV, print the area they framed on screen as a one-page map with the route and a label of their own on a real printer, and print the directions separately.

The owner's guidance for this phase (2026-10-28):
- **Printing to an actual printer is required**, not just PDF export. It needs a print preview and an easy way to choose the exact area printed, such as a rectangle over the map. PDF export is fine if it adds no complexity (on Windows, "Microsoft Print to PDF" already covers it).
- **Fuel and rest-stop suggestions only if they are easy** and cost little disk, RAM and CPU.
- **GPX, CSV and KML import and export are not a hard requirement** if they turn out to be complicated.
- **The map prints on a single page** (owner's answers, decision 17): the map with the route line if there is one, a scale bar, and only a short header with a label the user types when printing ("Route to X for summer trip"). **Directions have their own print button.** No detail maps of stops.

What phase 3 left in place for this:
- The route page has disabled *Optimize stop order*, *Depart at / Arrive by*, *Alternatives*, *Save trip* and *Print or export directions* entries, and the Trips page an empty list (step 3.8).
- `omp_core::route` answers one leg at a time from many start and end nodes, with avoid flags (step 3.4), and makes directions with each turn's angle (step 3.5). There is no one-to-many or many-to-many query, and no alternative routes.
- The pack already has the POIs a planner suggests: `fuel` and `charging_station` (from z12), `rest_area` (`highway=rest_area|services`, from z10), `hotel`, `campground` and the places, both in the tiles and as `SENT` entries, which form a spatial index per importance bucket ([format.md](format.md#sent-entries-contiguous)).
- `omp-app --render` draws any view to a PNG at any size and pixels-per-point (`snapshot.rs`), which is most of what a printed page needs.
- A stop is a point, a name and a pack-specific street ID (`routing::Place`). Saving a trip needs something that survives a pack rebuild.

### Exit criteria (expanded)

The roadmap's items are stop optimization, departure and arrival times with overnight stops (and fuel or rest-stop suggestions if cheap), saved trips and pushpins with GPX, CSV and KML, printing the directions and the map, alternative routes, and a measured decision on the U-turn penalty. In numbers, on the North America pack unless noted:

1. **Stop order.** With up to 12 stops to reorder, the order is optimal (equal to brute force on random matrices in tests). With more, the heuristic comes within 2% on average of the best order found from 100 random restarts (checked up to 500 stops). There is no limit on the number of stops (decision 15): larger trips take longer and can be cancelled. First and last stop each fixed or open, or a round trip. 25 stops are reordered within **0.5 s** warm on the dev machine and **2 s** on the E6430, not counting the route itself; the times for 100 and 500 stops are recorded. The optimized trip's leg times add up exactly to the matrix's prediction.
2. **Alternatives.** For random pairs over 50 km on Ohio, Quebec and North America, at least one alternative is found for at least **70%** of pairs (measured on Ohio first; the via-node methods over a CH find one for about 60–90% of queries, depending on how far the searches are relaxed). Each alternative shares at most 80% of its length with the chosen route, is at most 25% slower, and is locally optimal (defined in 4.2). The quickest route appears as fast as it does today, and the alternatives follow within the route's own time again (coast to coast on the dev machine: under 1 s warm, all with directions).
3. **U-turns.** The penalty's cost (nodes, edges, routing MiB, build time and peak) and its effect (U-turn rate and times against OSRM) are measured on Ohio and Quebec, and the owner decides whether to build it for North America.
4. **Schedule.** Departure or arrive-by, time at each stop, and a daily driving window give each stop's arrival and departure and each step's clock time. A test set of trips gets them right: across Arizona in summer, Indiana's split counties, Saskatchewan, Newfoundland's half hour, and a DST change during the trip. Every time in another zone than the departure's is marked as such, with the difference, and so is a crossing between zones in the directions (decision 14).
5. **Saved trips and pushpins.** Saved and opened unchanged, in files next to the executable. A trip saved on one build of the pack opens on a newer build with each stop on the same street. 10,000 pushpins cost under 0.1 ms of frame p95 on the Ohio bench.
6. **Import and export** (best effort, owner's guidance). GPX 1.1 and KML 2.2 exports validate against their schemas (`xmllint` on the dev machine) and open in GPSBabel. Our own exports read back unchanged. CSV import of addresses with no coordinates geocodes at least 95% of a 1,000-address sample to within 100 m.
7. **Printing.** From the app to a real printer on Windows and on Linux (the owner's Epson ET-2400, Letter, decision 16): the one-page map and, separately, the directions. The printed map covers the rectangle chosen on screen (edges within 1 mm), and the printed scale bar matches the ground within 1%. The preview shows the page as it prints. A Letter page at 300 dpi renders within **3 s** on the E6430. Printing raises RSS by at most 100 MiB, which is freed afterwards. Saved PDFs open in Firefox, Evince and Edge, and pass `qpdf --check`.
8. **Footprint.** The pack grows only by what decisions 5 and 4 add (the time zone section, aim ≤ 5 MiB; the U-turn penalty if taken). The app stays under 30 MB. `cargo tree` has no network crate. Frame p95 and idle RSS are unchanged when none of this phase's features are in use.

## Status

| Step | Status | Notes |
|---|---|---|
| 4.1 Stop optimization | Not started | |
| 4.2 Alternative routes (`omp-core`) | Not started | |
| 4.3 Alternatives in the app | Not started | |
| 4.4 U-turn penalty, measured first | Not started | |
| 4.5 Schedule: times, stops and overnights | Not started | |
| 4.6 Fuel, rest and overnight suggestions (if cheap) | Not started | |
| 4.7 Pushpins and saved trips | Not started | |
| 4.8 GPX, KML and CSV (best effort) | Not started | |
| 4.9 Print layout and preview | Not started | |
| 4.10 Printing and PDF | Not started | |
| 4.11 Performance, accuracy and hardware checks | Not started | |

## Environment notes

As in [phase 3](phase3-plan.md#environment-notes): builder data in `$OMP_WORK`, `target/` on `/`, packs in `$OMP_WORK/packs/`, OSRM 6.0.0 in Podman on Ohio and Quebec as the reference (it also serves alternatives, `alternatives=true`).

**Test regions:** Delaware for fast loops; Ohio and Quebec for routing changes (4.2, 4.4) and North America for their checks. Time zones need packs that cross them: North America, plus a small test pack (Indiana or Kentucky) for 4.5's builder loop.

**Without a window:** `omp-builder route` gains `--optimize [--keep-first] [--keep-last | --round-trip]` (4.1) and `--alternatives` (4.2; `--geojson` writes every line). `omp-app --render` gains `--print <out.pdf>` with `--paper`, `--orientation`, `--label` and `--directions` (4.9), so pages are checked without a printer.

**Printer:** the owner's **Epson ET-2400** (colour inkjet, Letter), on both Linux (CUPS, Epson's `escpr` driver) and Windows (Epson's driver) (decision 16). For automated checks: on Linux, `printer-driver-cups-pdf` as a CUPS queue that writes PDFs; on Windows, "Microsoft Print to PDF" as a GDI target. Neither replaces the hand check on paper.

---

## 4.1 Stop optimization

Reorder the stops to make the trip quickest (decision 1).

- **Matrix** (`omp_core::route`): travel times between every pair of snapped stops by **bucket many-to-many** over the existing CH (Knopp et al. 2007). Run a backward upward search from each stop, leaving a bucket entry `(stop, distance)` at every node it settles. Then run a forward upward search from each stop, scanning the buckets at each node it settles. That is 2n searches and no unpacking: no geometry and no directions. Avoid options apply as they do to routes. Times are in the route's own metric, restricted-road penalty included, so the matrix agrees with the legs exactly. The matrix is asymmetric because of one-ways.
- **Order** (`omp_core::route::order`, pure, over the matrix): first stop fixed or open, last stop fixed or open, or a round trip. An open end is a dummy stop with zero-cost arcs. Up to 12 stops to reorder: exact Held–Karp (2^12 × 12² ≈ 0.6M steps). Above that: cheapest insertion, then Or-opt and 2-opt moves adapted to asymmetric costs (a reversed run is costed again in full). Repeat from a few starts with a fixed seed, so the same stops always give the same order. **No stop limit** (decision 15): the matrix is 2n searches and n² entries (1,000 stops: 4 MB), and the improvement passes, which grow fastest, run on a time budget (a few seconds, scaled by the stop count) and keep the best order found. Neighbour lists (each stop's nearest few) keep a pass near n² rather than n³ for large trips. The stop list in the route page lays out only the rows in view, as the directions do.
- **Checks:** unit tests against brute force on random asymmetric matrices up to 9 stops, and against Held–Karp up to 12. The heuristic against 100 random restarts for 13–500 stops (criterion 1), and its time for 100, 500 and 1,000. On Ohio and North America, the optimized trip routed leg by leg must add up to the matrix's total exactly.
- **CLI:** `route <pack> <stops…> --optimize [--keep-first] [--keep-last | --round-trip]` prints the order, the times before and after, and the matrix and order times.
- **App:** *Optimize stop order* on the route page, with "Keep first stop" and "Keep last stop" checked by default, and "Round trip". It runs on the routing worker and can be cancelled like a trip. It shows "Saves 1 h 12 min, 84 km" with **Undo**, and stops keep their names and durations (4.5).

**Done when:** criterion 1 is met in tests and on North America, `--optimize` works in the CLI, and the app's button reorders, undoes and is covered by a headless panel test.

## 4.2 Alternative routes (`omp-core`)

Find a few routes that take clearly different roads, and score them again with what the CH leaves out (roadmap; phase 3 decisions 2, 13 and 16).

- **Via-node alternatives over the CH** (Abraham, Delling, Goldberg and Werneck, *Alternative routes in road networks*, 2013, the CH variant). Run the forward and backward searches past the usual stopping point, until the smallest key passes (1 + ε) × the quickest time (ε = 0.25). Every node settled in both is a candidate via node *v*, with d(s, v) + d(v, t) ≤ (1 + ε) × opt. Take candidates in order of a score (length, then sharing). Unpack s → v → t and keep it if it is admissible:
  - **limited sharing:** at most 80% of its length is on the quickest route or an alternative already taken;
  - **locally optimal** (the T-test): the stretch of the path around *v*, α = 25% of the quickest time long, is itself a quickest path, checked with one more CH query;
  - **bounded stretch:** at most 25% slower than the quickest.
  Keep up to two alternatives.
- **Stall-on-demand** prunes nodes whose labels are not exact. Measure the share found with and without it, and with relaxed searches (the paper's way to find more candidates in a CH), on Ohio first. If fewer than criterion 2's 70% of pairs get one, write up the options for the owner before going on.
- **Rescoring** (decision 3): each route, the quickest one included, gets turn costs from its geometry at every junction where it changes segment. This is OSRM's turn function: a sigmoid up to 7.5 s by angle, plus 20 s for a U-turn, using the bearings `guide` already takes over 15 m of each piece. Routes are ranked by profile time plus these costs, and the best becomes the one shown. Times shown stay the profile's. The route page says why one was preferred ("1 min slower on paper, 2 fewer U-turns").
- **Multi-stop:** alternatives are found per leg.
- **Measure** on 1,000 random pairs each on Ohio, Quebec and North America, by length band: the share with one and with two alternatives, their stretch and sharing, and time per query warm and cold. On Ohio and Quebec against OSRM's alternatives: how often our pick is OSRM's first route (by overlap, as in 3.5), and the U-turn rate of the routes picked against 3.5's 4.7% and 11.2% (OSRM: 1.9% and 6.8%). In-town trips may have no admissible alternative, and there rescoring cannot remove a U-turn: that is 4.4's job.
- **CLI:** `route --alternatives` prints each with its time, rescored time, sharing and turn counts. `--geojson` writes them all.

**Done when:** alternatives work for single and multi-stop trips in the CLI, criterion 2's share and time are measured on the three packs (or the shortfall written up), and the U-turn rate of the picked routes is compared with OSRM.

## 4.3 Alternatives in the app

- **Worker:** the quickest route and its directions come first, as now. The alternatives are found after it, on the same worker, and fill in. A newer request cancels them. If rescoring prefers an alternative, the line shown changes once, and the panel says so.
- **Map:** alternatives drawn in grey under the chosen route (two more of the overlay's lines), each with a small tag at a point where it runs apart from the others ("+4 min"). Clicking a grey line picks it.
- **Route page:** an *Alternatives* list per leg: time, distance, the difference from the quickest, and the main road it takes that the others do not ("via US 33"). Picking one updates the totals and the directions. The choice holds until the stops or avoid options change. When a trip is saved, it is saved as a hidden via point at its via node (4.7).
- **Bench:** the Ohio route flight with alternatives shown, frame p95 against 3.8's.

**Done when:** alternatives can be seen and picked on the map and in the list on the North America pack, with a headless test of picking from the list, and the bench's frame p95 is recorded.

## 4.4 U-turn penalty, measured first

From the roadmap (phase 3 decisions 13 and 16): a hidden penalty of a few minutes on U-turns at junctions. It counts in what the route minimizes but not in the time shown, like the restricted roads' entry penalty.

- **In the graph** (builder, behind `--uturn-penalty <s>`): a U-turn is arriving at node *v* from *a* and leaving back to *a*. The node-based graph cannot price a pair of edges, so a junction's node is split per incoming road, as turn restrictions split via nodes (3.1). Each copy leaves by every road, and leaving back the way it came carries the penalty. This reuses 3.1's automaton: a U-turn is the node path *a, v, a*, which today's automaton forbids for restrictions. Here it is kept, with a cost on that transition instead of a ban.
- **Where:** junctions with three or more roads (decision 13's option (b)). Nodes with two roads are not junctions, and dead ends keep their free U-turn, so cul-de-sacs stay routable. Restriction copies and U-turn copies must combine. The 3.1 test that checks every pair on a hand-built grid against a search over paths covers it.
- **Cost:** a junction with *k* roads becomes *k* copies with *k* arcs each, so arcs grow faster than nodes (decision 13 guessed about twice the nodes at junctions; arcs could be several times more). Measure on Ohio and Quebec: nodes, edges, CH edges, routing MiB, `RCPY` (U-turn copies are copies a destination must be snapped to), the build stages' time and peak, and query latency. Extrapolate to North America.
- **Effect:** penalties of 60, 180 and 300 s on 3.5's 1,000 random pairs each and the route set. Measure the U-turn steps against OSRM's 1.9% and 6.8%, times against OSRM (3.5's tool), and the route set, which must still pass. Compare with what 4.2's rescoring alone achieved.
- **Decision for the owner** with these numbers: build it for North America or not. If yes, rebuild, rerun `route --check` (exact against Dijkstra on the split graph) and the route set, and update the size table and format.md (no new section is expected; `RCPY` grows).

**Done when:** the measurements are in this plan and the size table, and the owner has decided. If the answer is yes, North America is built with it and checked.

## 4.5 Schedule: times, stops and overnights

- **Trip settings** on the route page under *Depart at / Arrive by*: a date and time to depart at, or to arrive by; time spent at each stop (per stop, default 0); and a daily driving window (start at 8:00, stop by 18:00, or at most N hours of driving a day). Breaks: optional, every N hours for M minutes (also used by 4.6). Defaults live in `config.toml`, and each trip's own values in its file (4.7).
- **Output:** each stop's arrival and departure, and each step's clock time in the directions (a column, shown when a time is set). Where a day's driving ends, a "Day 2" row appears with the place it ends near (the nearest town from search's places), and on the map a small marker. **Make overnight stop** turns it into a stop with a night's duration, which 4.6 can fill with a hotel or campground. Arrive-by works backwards day by day.
- **Local times** (decision 5): each time is in the zone where it happens, shown as "14:05 CDT". The builder adds a small `TZON` section from timezone-boundary-builder's polygons (ODbL, made from OSM; added to `fetch` and to the attribution list), clipped to the region and simplified to about 100 m. The app holds it in RAM and finds a point's zone with a point-in-polygon test. The zones' rules come from `jiff` with its bundled tz database (`jiff` is already a builder dependency), so Windows and Linux give the same answers from a stick. Format 0.6. Measure the section's size on North America (aim ≤ 5 MiB) and the binary's growth.
- **Showing zones** (decision 14): the *Depart at* and *Arrive by* fields are in the zone of that stop and say so. Every time is followed by its zone ("14:05 CDT"). One in another zone than the departure's is drawn in an accent colour with the difference ("CDT, 1 h behind departure"); its tooltip gives it in the departure's zone and in the computer's own zone (from `jiff`'s system zone lookup) when that differs. The directions get a row where the route crosses into another zone ("Enter Central Time: clocks go back 1 h"), found by testing the route's points against `TZON` at each step. A trip that stays in one zone shows no marks at all.
- **Speeds are the profile's** (phase 3 decision 9): no traffic or time of day. The panel says "Times assume no traffic".
- **Tests:** the cross-zone trips of criterion 4, a trip that crosses a DST change at night, arrive-by over several days, and a window shorter than the first leg (the stop then falls on day 2).

**Done when:** criterion 4's test set passes, the schedule shows in the panel and directions on the North America pack, and `TZON`'s size is in the size table and format.md.

## 4.6 Fuel, rest and overnight suggestions (if cheap)

The owner asked for this only if it is easy and cheap. The plan expects it to be. **No new data:** fuel, charging stations, rest areas and services, hotels, campgrounds and towns are already in `SENT` (kind 3 POIs, kind 1 places). It costs **no standing RAM** beyond search's caches. **CPU is spent only at a few points:** where a trip needs something, not along its whole length.

- **Settings**, with the schedule's: fuel every N km or miles (default 400 km); rest every N hours (from 4.5's breaks); overnight from 4.5's day ends. Each can be off, and all are off by default (decision 7).
- **Where to look:** for each target point on the route (where the tank range is three quarters used, each break, each day end), look at a window of the route around it (for fuel, the last 25% of the range before it). In that window, find POIs of the classes wanted within 2 km of the route line, through `SENT`'s Morton runs in the class's importance bucket, or through the z12 tiles' `pois` layer if that reads less (measure both).
- **Detour:** for the best few candidates by distance off the route, two CH queries (route to it, it to the next stop) give the real detour, with no unpacking. That also drops a motorway rest area on the far carriageway, whose detour is large. Suggest the three with the smallest detour, with their name and brand, the detour ("+3 min") and the distance along the route.
- **App:** small icons along the route and a list in the route page under each target ("Fuel near 610 km: …"). **Add as stop** adds it with a default duration (fuel 10 min, rest 15, overnight to the next day's start).
- **Measure** on the coast-to-coast routes on the dev machine and the E6430: time to the suggestions (aim: under 300 ms warm on the E6430), bytes read cold, and RSS. **If it is over budget, or the suggestions are poor** (wrong side of the road, closed POIs that OSM still has), write it up and drop or shrink it as the owner prefers. Charging stations come along for free, but range planning for electric cars is out of scope.

**Done when:** suggestions work on the North America pack within the budget, or the measurements are written up and the owner has decided.

## 4.7 Pushpins and saved trips

- **Files** (decision 8), next to the executable like `config.toml`: `trips/<name>.toml`, one file per trip; and `pins/<set>.toml`, one file per pushpin set. They are plain TOML, readable and copied with the app. If the folder is read-only (a locked stick), saving says so and offers the file browser (4.8).
- **A saved trip** holds the stops (name, latitude and longitude, the street's name, duration, and kind: stop, overnight or hidden via point from 4.3), avoid options, the schedule settings, the optimizer's options, a format version and the pack it was made on (for information only). **No pack IDs:** on open, a stop with a street name is matched to a street of that name near its point (the `SGRD` lookup "what's here" uses) and snapped to it first. Otherwise it snaps as a point. A stop that no longer snaps well gets the far-snap notice.
- **Pushpins:** a point, a name, a note, and a symbol from a small set (shape × colour). Add one from the map's right-click menu, and from the "what's here" and search-result buttons ("Add pushpin"). They are drawn as instanced symbols over the map with their names from a zoom on, culled by view, under the route's pins. A click opens it on the Search page with its note and the route buttons. Pins are found by search (by name, in memory, listed above the pack's results).
- **Trips page:** saved trips (open, rename, duplicate, delete) and pin sets (show or hide, rename, delete, a list of a set's pins). The route page's *Save trip* saves, or saves as, and shows when the trip has unsaved changes.
- **Tests:** round trips of both files, matching a stop's street after a pack rebuild (a test pack with the street IDs shuffled), and the Trips page headless. Bench: 10,000 pins on the Ohio flight (criterion 5).

**Done when:** criterion 5 is met and trips and pins can be made, saved, opened and deleted from the panel on the North America pack.

## 4.8 GPX, KML and CSV (best effort)

Not a hard requirement (owner's guidance): each format is kept only if it stays simple. Writers are a page of code each. Readers use `quick-xml` (already in the lock file through Wayland's build; measure the binary's growth).

| | Export | Import |
|---|---|---|
| **GPX 1.1** | A trip as `<rte>` of its stops plus a `<trk>` of the route's line, so GPS units and other apps show the roads taken. Pins as `<wpt>`. | `<wpt>` → pins. `<rte>` → a trip, its points as stops; with over 100 points (likely another app's shaping points), it asks whether to take them as stops or as pins. `<trk>` is not read (out of scope). |
| **KML 2.2** | Stops and pins as Placemarks, a set as a Folder, and the route as a LineString. | Point Placemarks → pins, a Folder → a set. Lines are not read. KMZ is not read (it needs a zip reader). |
| **CSV** | Stops with their times, pins (name, latitude, longitude, note, symbol), and the directions. | Header detection (name, lat or latitude, lon, lng or longitude, note, address), with commas, semicolons or tabs. Rows with no coordinates but an address are **geocoded by search**: the top hit is taken, and weak matches are flagged in a list of rows to check. |

- **Files** (decision 9): an in-app file browser in egui (folders, a file name box, the formats), starting next to the executable. No native dialog crate: `rfd` would pull in GTK or a D-Bus portal on Linux. The same browser serves 4.10's "Save as PDF".
- **Checks:** `xmllint --schema` for GPX 1.1 and KML 2.2, and GPSBabel reading our files on the dev machine. Our exports read back unchanged. Sample files from OsmAnd, Garmin BaseCamp and Google My Maps exports, kept under `crates/app/tests/` (owner to provide if wanted). Criterion 6's address CSV is made from search's address accuracy set (phase 2 step 2.9).

**Done when:** each format kept passes its checks, and any dropped format is written up here.

## 4.9 Print layout and preview

Two printouts (decision 17): **the map**, one page of exactly the area chosen on screen, and **the directions**, from their own button. This step lays them out and previews them; 4.10 sends them to a printer.

- **Print map**, from the ☰ menu, the route page or Ctrl+P, opens the print page in the side panel:
  - a **label** the user types for this printout ("Route to X for summer trip"), empty by default, printed as the page's one-line header (left out when empty);
  - **printer**, from 4.10's list, then its **paper sizes** (Letter by default), orientation (automatic, portrait or landscape) and copies;
  - **text size** (small, normal or large).
  The page holds the label, the map, a **scale bar** in the config's units in a corner of the map, and the credit line "© OpenStreetMap contributors" (decision 12), and nothing else. The map shows the route line, its stop pins and the pushpins shown on screen, as on screen. Settings other than the label are kept in `config.toml`.
- **The rectangle:** while the print page is open, the map shows the page's map area as a rectangle at the paper's aspect ratio (the paper minus margins and the header), tied to the ground, with the rest of the map dimmed. Drag inside it to move it and drag a corner to resize it, keeping the aspect ratio. The map still pans and zooms around it. It starts fitted to the route with a margin, or to the visible map when there is no route, so Ctrl+P with no trip prints what is on screen. The panel shows the scale ("1:250,000, 1 in ≈ 4 mi").
- **How detail follows the rectangle:** the page's map is drawn as the app would draw a window the size of the paper's map area in points (1 pt = 1/72 in), at the printer's resolution (pixels per point = dpi / 72). Labels, roads and icons therefore print at about the size they show on screen. The rectangle's ground width over the paper's width sets the zoom (fractional, capped at z18). *Text size* scales the points per inch: more map per label, or larger text. The labels are placed for the paper's window rather than the screen's, so the preview is what shows the true page.
- **Print directions**, a button on the route page, uses the same print page without the rectangle: the label as its header, the stops with their times and the totals, then the steps as the route page draws them (icons, distances, clock times when a schedule is set), over as many pages as needed, with page numbers and the credit line in the footer.
- **Rendering** (decision 10): pages are laid out in points by egui and rendered offscreen at the printer's resolution. The map is the map renderer's output (`snapshot.rs`'s path, waiting for every tile at that zoom rather than drawing progressively). The directions reuse the route page's own drawing code, so they match the panel. **First, a spike:** check egui's font atlas and tessellation at 4.17 pixels per point (300 dpi), and the page's texture size against the adapter's limit (8192 px covers Letter at 600 dpi).
- **Preview:** a thumbnail of the page in the print page, re-rendered (debounced) as the rectangle moves or the settings change. Clicking it opens a large preview over the map (with previous and next page buttons for directions). Both are the same page at a lower resolution: the page as printed, only less sharp.
- **Headless:** `omp-app --render --print <out.pdf> --route … --paper letter --label <text> [--directions]` writes the map page, or the directions pages, with 4.10's PDF writer, for checks and screenshots in `docs/screenshots/`.

**Done when:** both printouts' print page and preview work on the North America pack, the rectangle can be moved and resized, a headless test checks the map page's ground extent from the rectangle, and `--print` writes the map page and the directions for a coast-to-coast route.

## 4.10 Printing and PDF

- **Windows:** GDI, through `windows-sys` (already a dependency). Printers come from `EnumPrintersW` and the default from `GetDefaultPrinterW`. Paper sizes come from `DeviceCapabilitiesW`. The printable area and resolution come from a DC's `GetDeviceCaps` (`PHYSICALWIDTH`, `PHYSICALOFFSETX`, `LOGPIXELSX`), so the preview uses the real margins. **Printer properties…** opens the driver's own dialog (`DocumentPropertiesW`) for quality, colour and duplex, and its `DEVMODE` is kept. To print: `CreateDCW` with that `DEVMODE`, `StartDocW` (in windows-sys's `Win32_Storage_Xps`), then per page `StartPage`, `StretchDIBits` in bands of rows, `EndPage`, and finally `EndDoc`. This runs on a worker thread, since spooling blocks. Resolution: the printer's, capped at 600 dpi (measure RSS); for the ET-2400, check whether 300 dpi looks as good as what its driver asks for (360 or 720), which saves memory and time.
- **Linux:** CUPS through its command-line tools (decision 11). Printers come from `lpstat -e`, the default from `lpstat -d`, and paper sizes from `lpoptions -p <printer> -l` (`PageSize`). The job is sent as a PDF on stdin: `lp -d <printer> -n <copies> -o media=<size> -o print-scaling=none -t <title>`. `print-scaling=none` stops CUPS fitting the page, which would break the scale. Margins default to 0.25 in, since the command-line tools do not report the printable area; if that proves wrong on the owner's printer, read it from libcups through `libloading` (`cupsCopyDestInfo`). With no `lp` on the path, *Print* is disabled with the reason, and *Save as PDF* still works.
- **PDF writer** (needed for Linux anyway, so *Save as PDF* on both platforms costs nothing more): one image per page, at the page's size, Flate-compressed with PNG predictors (`miniz_oxide`, already in the tree through `png`). It is written by hand: catalog, pages, one image XObject per page. Text in the PDF is part of the image, so it cannot be selected (decision 18). Measure the file size per page.
- **Memory:** a 300 dpi Letter page is 8.4 MP, so 34 MB as RGBA. Render, send and free one page at a time. At 600 dpi, render the page in bands with an offset viewport if one full page goes over criterion 7's 100 MiB.
- **macOS** printing (`NSPrintOperation`) is left for the roadmap's macOS polish.
- **Checks:** the PDFs with `qpdf --check` and in Firefox and Evince. Printing to the CUPS-PDF queue on Linux and to "Microsoft Print to PDF" on Windows, compared with `--print`'s pages. Then **on paper on the ET-2400**, from Windows and from Linux: measure the scale bar and a known distance with a ruler, check the map's edges against the rectangle, check legibility, and print once in black and white to see that the route and roads still read. If they do not, the print style gets a darker, wider route line, ahead of phase 5's print style.

**Done when:** criterion 7 is met on a real printer on Windows and on Linux, and *Save as PDF* writes files that pass the checks.

## 4.11 Performance, accuracy and hardware checks

As phase 3 step 3.9, with this phase's features:
- Rebuild North America with `TZON` (and the U-turn penalty if taken); update the size table and roadmap §4.
- Rerun phase 3's checks that a rebuild touches: `route --check`, the route set, and the address pairs.
- Bench on the dev machine (warm, and cold from the FAT32 USB 2 stick), the Latitude 7490 and the E6430: optimizing 25 stops, alternatives for a short and a coast-to-coast trip, the schedule and suggestions for coast to coast, 10,000 pins, a printed page's render time and RSS, frame times throughout, and idle RSS with nothing of this phase in use.
- Check the binary's size and that `cargo tree` still has no network crate.
- Hand checks on each machine: the route page's new parts, the rectangle, printing on paper (Windows and Linux), and opening an exported GPX in another app.

**Done when:** every exit criterion is measured on every machine, with any that miss written up for the owner.

---

## Order of work

```
4.1 optimize ──────────────────────────────────────┐
4.2 alternatives (core) ─► 4.3 app ─┐               │
4.4 U-turn measurement ─► (owner) ──┤               ├─► 4.11 checks
4.5 schedule + TZON ─► 4.6 suggestions ─┐           │
4.7 trips + pins ─► 4.8 GPX/KML/CSV ────┼─► 4.9 print layout ─► 4.10 printing + PDF
                                        │
```

- 4.1, 4.2, 4.4, 4.5 and 4.7 are independent and can run side by side. 4.4's builds are long, so start them early and decide while other steps go on.
- 4.4's decision waits for 4.2's U-turn numbers, so the owner sees both fixes side by side.
- 4.5's `TZON` is the only format change planned (0.6). Build it with 4.4's experiment if that is taken, so North America is rebuilt once.
- Printing comes after trips, schedule and suggestions, so the printout has everything it prints: times, stops, pins.
- 4.9's spike (egui at 300 dpi) comes first in that step: it decides how pages are drawn.

## Decisions

Proposed while planning and confirmed by the owner (2026-10-28):
1. **Optimize for driving time**, the route's own metric, with first and last stops fixed or open, or a round trip. Exact up to 12 stops to reorder, a deterministic heuristic beyond (no stop limit: decision 15).
2. **Alternatives by via nodes over the existing CH**: no second metric, no format change, up to two per leg. Penalty and plateau methods need a customizable metric or full Dijkstra trees, which the pack does not support at continent size.
3. **Alternatives are ranked by time plus turn costs** (OSRM's turn function, from the geometry), and the best is the one shown. The times shown stay the profile's, as with restricted roads' penalty, so legs, steps and totals keep adding up.
4. **The U-turn penalty is measured on Ohio and Quebec before anything is built for North America** (roadmap). The owner decides with the numbers.
5. **Times are local**, from a small `TZON` section (timezone-boundary-builder, ODbL) and `jiff`'s bundled rules (confirmed: decision 14).
6. **Schedules use the profile's speeds**: no traffic, opening hours or ferry timetables. Stops, breaks and overnights are durations at stops.
7. **Suggestions use only the POIs already in the pack**, found only near the trip's target points, off by default, and dropped if 4.6's measurements say they cost too much.
8. **Trips and pins are TOML files next to the executable**, storing coordinates and names, never pack IDs, and snapped again when opened.
9. **Own GPX, KML and CSV writers, `quick-xml` for reading, and an in-app file browser** rather than native file dialogs, which would add GTK or a D-Bus portal on Linux.
10. **Printed pages are raster images at the printer's resolution** (300–600 dpi), drawn by the map renderer and egui. The same pages go to every printer, the PDF and the preview, so all three agree.
11. **The printer is chosen in the app's print page**, not a native print dialog, so the preview always shows the paper being used. Windows prints through GDI, with the driver's properties dialog for its own settings. Linux prints through CUPS's `lp` with a PDF. *Save as PDF* comes from the same writer.
12. **Printouts carry one credit line, "© OpenStreetMap contributors"**, the same as on screen, as ODbL asks of produced works. The printed map draws no address data (house numbers and address points are not on the map), so the address sources' credits stay in About this map, as for the screen. Reworded 2026-10-28 after the owner asked: draft 1 also had a line naming the address sources.
13. **Printing uses the current map style.** A print style is phase 5's (roadmap), unless 4.10's black-and-white test shows the route needs help sooner.

Settled by the owner (2026-10-28), from draft 1's questions:

14. **Local times with a `TZON` section** (question a). The UI must show when a time is in another zone than the departure's or the computer's: each time with its zone, times outside the departure's zone marked with the difference, and zone crossings in the directions (4.5).
15. **No limit on stops** (question b): 100 was thought enough, but the method needs no cap, so larger trips just take longer, on a time budget, and can be cancelled (4.1).
16. **The test printer is the owner's Epson ET-2400** (colour inkjet, Letter), on Linux and Windows (question c).
17. **The map prints on one page** (question d): the map with the route line if there is one, a scale bar, the credit line, and a one-line header with a label the user types when printing. **Directions print from their own button.** No detail maps of stops, and no page combining map and directions (4.9).
18. **PDF text as part of the image is fine** (question e); no selectable text.
19. **Fuel and rest suggestions go ahead as 4.6 plans** (question f), to be dropped if its measurements miss the budget.

Open: none.

## Out of scope for phase 4

- Route strip maps, detail maps of stops, a page combining map and directions, and posters printed across several pages (decision 17).
- GPX tracks and KML lines shown on the map, and KMZ.
- Traffic, opening hours, ferry timetables, fuel prices, and range planning for electric cars.
- Dragging the route to add a via point, and "avoid this area" (roadmap §5).
- A "shortest" metric, "avoid highways" and full turn costs (phase 3 decisions 2, 4 and 10). Turn costs are only used here to rescore alternatives.
- Printing on macOS (roadmap: macOS polish).
- Sharing trips online, or anything with a network.

## Carried from earlier phases

- **Directions run after the trip and cannot be stopped by a newer request** (3.7). 4.3's alternatives come after the directions, so they must be cancellable from the start. Phase 5's "show the route's line before its directions" also helps them.
- **Free U-turns** (phase 3 decisions 13 and 16): 4.2's rescoring and 4.4's penalty.
- **How the route page's buttons wrap in a narrow panel** (3.10): the page gains many controls here, so check it at 300 points.
- **Phase 5's print style and UI size setting:** the print page should take a style and a text scale as parameters, so phase 5 only adds choices.

# Phase 4 Plan: Trip Planner Features

*Draft 2, 2026-10-28, with the owner's answers to the planning questions (decisions 14–19) and decision 12 reworded; decisions 1–13 confirmed by the owner the same day, decision 20 from step 4.4, decisions 21 and 22 from step 4.2, and decisions 23 and 24 from the hand checks of step 4.3. Expands Phase 4 of [roadmap.md](roadmap.md#phase-4-trip-planner-features). Builds on phase 3's routing ([phase3-plan.md](phase3-plan.md)): the contraction hierarchy and its queries in `omp_core::route`, the route page and side panel (steps 3.7 and 3.8, which already hold this phase's placeholders), and `omp-app --render`'s offscreen renderer.*

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
2. **Alternatives.** For random pairs over 50 km on Ohio, Quebec and North America, at least one alternative is found for at least **70%** of pairs, or at least OSRM's share on the same pairs where that is lower (decision 21: Quebec) (measured on Ohio first; the via-node methods over a CH find one for about 60–90% of queries, depending on how far the searches are relaxed). Each alternative shares at most 80% of its length with the chosen route, is at most 25% slower, and is locally optimal (defined in 4.2). The quickest route appears as fast as it does today, and the alternatives follow within the route's own time again (coast to coast on the dev machine: under 1 s warm, all with directions).
3. **U-turns.** The penalty's cost (nodes, edges, routing MiB, build time and peak) and its effect (U-turn rate and times against OSRM) are measured on Ohio and Quebec, and the owner decides whether to build it for North America.
4. **Schedule.** Departure or arrive-by, time at each stop, and a daily driving window give each stop's arrival and departure and each step's clock time. A test set of trips gets them right: across Arizona in summer, Indiana's split counties, Saskatchewan, Newfoundland's half hour, and a DST change during the trip. Every time in another zone than the departure's is marked as such, with the difference, and so is a crossing between zones in the directions (decision 14).
5. **Saved trips and pushpins.** Saved and opened unchanged, in files next to the executable. A trip saved on one build of the pack opens on a newer build with each stop on the same street. 10,000 pushpins cost under 0.1 ms of frame p95 on the Ohio bench.
6. **Import and export** (best effort, owner's guidance). GPX 1.1 and KML 2.2 exports validate against their schemas (`xmllint` on the dev machine) and open in GPSBabel. Our own exports read back unchanged. CSV import of addresses with no coordinates geocodes at least 95% of a 1,000-address sample to within 100 m.
7. **Printing.** From the app to a real printer on Windows and on Linux (the owner's Epson ET-2400, Letter, decision 16): the one-page map and, separately, the directions. The printed map covers the rectangle chosen on screen (edges within 1 mm), and the printed scale bar matches the ground within 1%. The preview shows the page as it prints. A Letter page at 300 dpi renders within **3 s** on the E6430. Printing raises RSS by at most 100 MiB, which is freed afterwards. Saved PDFs open in Firefox, Evince and Edge, and pass `qpdf --check`.
8. **Footprint.** The pack grows only by what decisions 5 and 4 add (the time zone section, aim ≤ 5 MiB; the U-turn penalty if taken). The app stays under 30 MB. `cargo tree` has no network crate. Frame p95 and idle RSS are unchanged when none of this phase's features are in use.

## Status

| Step | Status | Notes |
|---|---|---|
| 4.1 Stop optimization | **Done** 2026-10-28 | Bucket matrix + Held–Karp ≤ 12 / heuristic above. North America warm: 25 stops 39 ms, 100 in 0.22 s, 500 in 1.4 s, 1,000 in 3.3 s; 0.996 of 100 random restarts on average (worst 1.0085); legs add up to the matrix in every run. E6430 time left to 4.11 |
| 4.2 Alternative routes (`omp-core`) | **Done** 2026-10-28 | Via nodes over the CH, T-test, turn-cost ranking. Over 50 km, one alternative for 85.5% of Ohio's pairs, 75.6% of North America's, 68.9% of Quebec's (OSRM: 52.2%; criterion restated for Quebec, decision 21). Coast to coast 0.47 s with directions. Turn costs pick another route on 1% of legs: U-turns 4.7% → 4.6% (Ohio), 11.2% → 10.7% (Quebec) |
| 4.3 Alternatives in the app | **Done** 2026-10-28 | Each leg's row in the route page opens to its routes, looked for then (decision 23) and drawn grey with a "+10 min" tag while open; picked by a click on the map or in the list. Ohio flight with every leg open: frame p95 1.07–1.08 ms against 1.07 before. New York–Los Angeles: alternatives with directions 0.48 s warm. Directions now stop on a newer request |
| 4.4 U-turn penalty, measured first | **Done** 2026-10-28 | Built behind `--uturn-penalty` and measured on Ohio and Quebec: routing ×2.2–2.5, contraction ×8–24, U-turn routes 4.7% → 3.3% (Ohio) and 11.2% → 9.5% (Quebec) at any penalty. **Not built for North America** (owner, decision 20); the flag stays, off by default |
| 4.5 Schedule: times, stops and overnights | **Done** 2026-10-28 | Format 0.6, `TZON` from timezone-boundary-builder 2026e: North America 74 zones, **0.39 MiB**, 2.2 MiB in RAM; app +0.79 MiB. Criterion 4's trips pass (12 tests); New York–Los Angeles on North America: 6 days, 4 crossings, 0.3 ms to work out. Frame p95 unchanged with no time set (1.02–1.03 ms), 1.38 ms with a coast-to-coast schedule shown |
| 4.6 Fuel, rest and overnight suggestions (if cheap) | Not started | |
| 4.7 Pushpins and saved trips | **Done** 2026-10-29 | TOML files in `trips/` and `pins/` next to the app, with an in-app file browser. Stops kept by name and coordinates: a trip saved on the Ohio pack opens on North America on the same street (IDs 131009 → 7204075), same 230.0 km route. 10,000 pins on the Ohio flight: frame p95 0.81–0.82 → 0.84 ms (budget +0.1). App +0.26 MB |
| 4.8 GPX, KML and CSV (best effort) | Not started | |
| 4.9 Print layout and preview | **Done** 2026-10-29 | Pages drawn by the map renderer plus an egui layer at the printer's dpi (the spike passed: no atlas or texture limit at 300 dpi). Print page with rectangle, label, paper, orientation, text size, preview; directions paginated; `--print` writes PDF/PNG. New York–Los Angeles map page at 300 dpi 5 s in a debug build. Hand check of dragging the rectangle and of 4.10's buttons still to do |
| 4.10 Printing and PDF | **Done** 2026-10-29 | Printing works on paper from Linux (CUPS) and Windows (GDI); Save as PDF passes `qpdf --check` and opens in several viewers. The printer's own dpi is used (360 on the ET-2400 under Windows), which gets its first print in 4.11's Windows run. RSS increase in the running app and the E6430's render time are for 4.11; black and white waits for phase 5's print style |
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

> **Done 2026-10-28** (commit "Step 4.1 done: stop optimization"). Logs in `$OMP_WORK/measure/opt41/`, on phase 3's final packs (`measure/private/`).
>
> - **Built:** `omp_core::route::matrix` (`Routing::matrix_until`, `optimize_until`) and `omp_core::route::order` (pure). The matrix's searches start from a leg's own seeds, U-turn and restricted penalties included, so each entry is `leg_avoiding`'s weight. Pairs on one segment, and pairs with no route avoiding all that was asked, are routed as legs. The order is a cycle through the stops plus one dummy stop (none for a round trip). Up to 12 stops to reorder it is exact (Held–Karp over at most 14 nodes). Above that come 8 starts of random-order cheapest-place insertion, then Or-opt (runs of up to 3) and 2-opt moves over 10-nearest neighbour lists, driven by a queue of the nodes a move touched (don't-look bits). Then 20 rounds a stop of local double-bridge kicks (runs of up to 30), each kept if it improves. The time budget is 10 ms a stop, between 2 and 10 s; no measured trip reached it, so orders are the same on every run.
> - **CLI:** `route … --optimize [--keep-first] [--keep-last | --round-trip]`, and `--optimize-random <n>` for criterion 1. `--optimize-random` replaces random stops that have no route to or from the others (islands): 1 of 100, 2 of 500 and 2 of 1,000 on North America. Its "before/after" are matrix weights, restricted-road penalty included.
> - **North America** (dev machine, keep both ends; matrix warm + order):
>
>   | Stops | Matrix | Order | Against 100 restarts |
>   |---|---|---|---|
>   | 25 | 36 ms (59 ms cold) | 3 ms | 1.0000 |
>   | 100 | 189 ms | 29 ms | 1.0000 |
>   | 500 | 979 ms | 393 ms | 0.9887 |
>   | 1,000 | 1,979 ms | 1,324 ms | 0.9766 |
>
>   The sweep over 13, 25, 50, 100, 200 and 500 stops × 3 seeds × 4 end modes (72 runs) averages **0.9962** of the best of 100 random restarts, worst 1.0085. Order time averages 3 ms at 25 stops, 30 ms at 100 and 372 ms at 500. In all 72 runs the legs routed in the new order add up exactly to the matrix's total. The matrix costs about 2 ms a stop (two upward searches of about 350 nodes each) and holds peak RSS at 64–101 MiB for 25–1,000 stops. Ohio: 25 stops in 15 ms, 100 in 0.28 s.
> - **Tests:** exact against brute force up to 9 stops in every end mode; the heuristic within 0.5% of Held–Karp at 10–13 stops; within 2% of 100 restarts at 13–60 stops in tests (the CLI covers more); the same order on every run; the matrix against `leg_avoiding` for every pair and avoid option on the builder's test network, and the optimized legs adding up to it.
> - **App:** *Optimize stop order* on the route page, with *Keep first stop* and *Keep last stop* checked by default and *Round trip*. It is enabled once a complete route is shown and the order could change. It runs on the routing worker, then routes the trip in the new order. While it runs, a *Cancel* button replaces it, and any other change to the stops supersedes it. The result says "Saves 1 h 12 min, 84 km" (or "…; 3 km longer", or "already in the quickest order") from the routes before and after, with **Undo** until the stops change. Stops keep their names. A round trip adds the start again as the destination; if the destination is already at the start, it is left out of the order and added back. Covered by the headless panel test `optimize_stop_order`.
> - **Left open:** the E6430's time for 25 stops (criterion: 2 s; 39 ms here) is measured with 4.11's benches. The stop list is not yet laid out only in view for very long trips (4.5 adds durations to its rows, so it is done there).

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

> **Done 2026-10-28** (commit "Step 4.2 done: alternative routes"), with Quebec's share short of criterion 2's 70% and written up for the owner, who restated the criterion (decision 21). Logs in `$OMP_WORK/measure/alt42/` (`run.sh` reruns them), on phase 3's final packs (`measure/private/`).
>
> - **Built:** `omp_core::route::alt` (`Routing::alternatives_until`). Given a leg's quickest route, as the trip routed it, it searches again from the leg's own seeds with `Routing::explore`, which carries both sides on until their next node is 1.25 times the quickest weight away. A node both sides settled is a via node, and its route (up the hierarchy to it, down from it) is a candidate. Candidates are scored by twice their weight plus their weight on the hierarchy edges of routes already taken. Identical ones are dropped, and so are those that share over 80% by hierarchy edges, which is a lower bound of what they share once unpacked. Up to 16 are unpacked in score order. Each is kept if it drives no segment twice, has at most 80% of its weight on segments of the routes taken, and passes the T-test. The T-test runs one CH query between the nodes 12.5% of the quickest weight before and after the via node, so the stretch is the plan's α = 25% long, and it must find nothing quicker than the route's own stretch. The search's limit bounds the stretch. Up to two alternatives per leg. They avoid what the quickest leg avoided (a flag it had to break is allowed). A leg along one segment gets none.
> - **Rescoring** (`Leg::turns`, in `guide.rs`; decision 3): OSRM's `car.lua` turn function at every node where the leg changes segment, from the bearings over 15 m that directions use. It gives up to 7.5 s by angle, cheaper to the right (bias 1.075): 2.1 s for a right turn, 5.4 s for a left turn, 7.5 s for turning back. Each U-turn adds 20 s, counted as directions count them: back at one junction (over 165°), or two turns within 30 m that add up to over 165° (across a median). OSRM charges only junctions of three roads or more; this charges every node, which costs 0.007 s going straight. It reads nothing from the pack, only the leg's line. Each route is ranked by its weight plus that cost, and the best is `picked`. The times shown stay the profile's.
> - **Stall-on-demand** is off in these searches by default (`AltParams::stall`). Without it, more pairs get an alternative on every pack (table), for 20–40% more search time. `--stall` measures with it.
> - **CLI:** `route … --alternatives` prints each leg's routes: distance, time, the time ranked with turn costs, turns and U-turns, the difference and stretch, the share it has with the routes before it, and the road it takes that the others do not ("via US 395"; `Routing::via_road`, for 4.3). `*` marks the one picked, with "picked 2: 3 min slower on paper, 1 fewer U-turns". `--geojson` writes them all. `--alternatives-random <n>` measures, on a quarter each of pairs 1–10, 10–50 and 50–250 km apart and random points anywhere (`--out` writes the pairs). `--osrm … --alternatives` compares the picked routes with OSRM's. `tools/osrm/alternatives.py` asks OSRM for its own alternatives on the same pairs.
> - **Share found** (1,000 random pairs per pack, by the quickest route's length; OSRM 6.0.0 MLD with `alternatives=2` on the same pairs):
>
>   | | Ohio | Quebec | North America |
>   |---|---:|---:|---:|
>   | **Over 50 km, at least one** (criterion 2: 70%) | **85.5%** | **68.9%** | **75.6%** |
>   | — with stall-on-demand | 83.0% | 62.3% | 70.8% |
>   | — OSRM | 87.9% | 52.2% | |
>   | Over 50 km, two: ours / with stall / OSRM | 75.5 / 67.6 / 57.6% | 40.9 / 33.2 / 21.4% | 61.0 / 55.7% / – |
>   | At least one, < 10 / 10–50 / 50–200 / 200–1,000 / > 1,000 km | 39 / 73 / 85 / 87 / – % | 31 / 50 / 70 / 68 / (3 of 5) | 38 / 55 / 74 / 76 / 77% |
>   | — OSRM, same bands | 42 / 72 / 88 / 88 / – % | 28 / 48 / 63 / 35 / (2 of 5) | |
>   | Stretch p50 / max; sharing p50 / max | 1.107 / 1.250; 0.39 / 0.80 | 1.112 / 1.250; 0.47 / 0.80 | 1.094 / 1.249; 0.39 / 0.80 |
>   | Alternatives' search, ms p50 / p95 / max | 2.3 / 15.5 / 34.2 | 1.0 / 8.7 / 21.6 | 2.2 / 158 / 309 |
>
>   Testing 64 candidates instead of 16 takes Quebec only from 68.9% to 69.3%. Peak RSS over the 1,000 North America pairs: 81 MiB.
> - **Time** (dev machine; each CLI run is a new process, so the trip comes first from cold caches and the alternatives follow it, as they will in the app). The quickest route is untouched: the app's trip and its directions run as before. On North America over 1,000 km the quickest leg takes 56 ms (p50), and its alternatives 104 ms (p50, 309 max) plus 132 ms for their directions. New York to Los Angeles: the trip takes 97 ms and its directions 125 ms. The alternatives (I-40, 10 min slower, sharing 5%; I-70, 58 min, sharing 70%) take 201 ms and their directions 264 ms: **0.47 s**, under criterion 2's 1 s but about twice the route's own 0.22 s, mostly decoding the alternatives' lines and naming their steps. New York, Los Angeles and Seattle: 149 + 192 ms for the trip, 270 + 328 ms for its alternatives. Cold from the disk is left to 4.11's benches.
> - **Effect against OSRM** (`route --osrm … --alternatives` on 3.5's pairs and the route set):
>
>   | | Ohio | Quebec |
>   |---|---:|---:|
>   | Legs where turn costs pick another route | 9 of 1,000 | 12 of 993 |
>   | Routes with a U-turn step: quickest (3.5) → picked; OSRM | 4.7% → **4.6%**; 1.9% | 11.2% → **10.7%**; 6.8% |
>   | Ours is OSRM's first route (90% overlap): quickest → picked | 92.8% → 93.3% | 92.1% → 92.4% |
>   | Route set with a U-turn: quickest → picked; OSRM | 28% → 24%; 4% | 23% → 23%; 18% |
>
>   **Rescoring removes few U-turns.** It only chooses among admissible alternatives. Those differ from the quickest by more than 20% of their weight, while a route that avoids a U-turn differs by a block. In town, where most U-turns are, under 40% of pairs have an alternative at all. 4.4's penalty alone took the rates to 3.3% and 9.5%; it is not built (decision 20). The route set passes as before (Ohio 26/26, Quebec 22/22); it routes the quickest.
> - **Tests:** OSRM's turn costs at known angles, and turns, U-turns and median U-turns from hand-made lines (`guide.rs`). On a hand-built ladder of a street and a faster road, with and without stall-on-demand, the alternative is found on the other road and its line, pieces and totals add up. Stopping returns `None`, and a leg along one segment has no alternative. Between every pair of points on the builder's test network, with every avoid option, every alternative is within the stretch, shares at most 80%, drives no segment twice and uses nothing avoided, and `picked` is the lowest score.
> - **Decided by the owner, 2026-10-28:** Quebec's criterion is at least OSRM's share on the same pairs (decision 21; 68.9% against 52.2%, met), and the U-turns are left as they are (decision 22).
> - **Left open:** Alternatives are listed in the order they were taken, not by time; 4.3 sorts them for display. Directions for alternatives are 4.3's to make cancellable.

## 4.3 Alternatives in the app

- **Worker:** the quickest route and its directions come first, as now. The alternatives are found after it, on the same worker, and fill in. A newer request cancels them. If rescoring prefers an alternative, the line shown changes once, and the panel says so.
- **Map:** alternatives drawn in grey under the chosen route (two more of the overlay's lines), each with a small tag at a point where it runs apart from the others ("+4 min"). Clicking a grey line picks it.
- **Route page:** an *Alternatives* list per leg: time, distance, the difference from the quickest, and the main road it takes that the others do not ("via US 33"). Picking one updates the totals and the directions. The choice holds until the stops or avoid options change. When a trip is saved, it is saved as a hidden via point at its via node (4.7).
- **Bench:** the Ohio route flight with alternatives shown, frame p95 against 3.8's.

**Done when:** alternatives can be seen and picked on the map and in the list on the North America pack, with a headless test of picking from the list, and the bench's frame p95 is recorded.

> **Done 2026-10-28** (commit "Step 4.3 done: alternatives in the app"). Logs in `$OMP_WORK/measure/alt43/` (`run.sh` reruns them), on phase 3's final packs (`measure/private/`).
>
> - **Revised the same day after the owner's hand check** (decision 23). The first version looked for every leg's alternatives after each route, showed the turn-cost best, and listed them under a separate *Routes* heading below the per-leg totals. Now each leg has one row that opens to its routes, they are looked for only then, and the grey lines show only while the row is open. What follows describes the revised version.
> - **Worker:** the route and its directions are sent first, as before. The worker keeps the last route it sent. When a leg's row is opened, the UI asks for that leg's alternatives (`Job::Alternatives`, with every leg still waiting). The worker finds them (`AltParams::default()`: up to two, at most 25% slower) and makes their directions, restricted runs, lines and tag points, then sends each leg's as it is done. A newer request stops all of it within milliseconds: `Routing::directions_until` is new and checks every 64 pieces and junctions. The route's own directions use it too, which closes phase 3.7's carried item (see below). A leg opened while a route is being asked for waits for that route, and a leg left open is looked at again for each new route.
> - **Map:** the routes not shown, of the open legs only, are one more overlay line (`Look::Alternative`), grey and drawn under the rest. Each has a tag giving its time against the route shown ("+10 min", "−4 min"). The tag sits at the middle of the longest stretch it shares with none of the leg's other routes. A click within 8 points of a grey line, or on its tag, shows that route. Closing the row removes its grey lines and tags and keeps the route picked, so the map (and 4.9's printout) shows only that. The tags are egui shapes, so `--render` does not draw them.
> - **Route page:** under the totals, one row per leg, for a single leg too: a triangle and "1 to 2: 2,793 mi · 49 h 48 min", with "· via I-80" once its routes are known. This replaces the per-leg totals and the separate list of the first version. Clicked, the row opens: "Checking for other routes…" while the worker looks, then the leg's routes, each as time · distance · "quickest" or the difference from it · "via I-40". The quickest comes first, then the alternatives by time (4.2 left them in the order taken). The route shown is selected, and clicking another shows it. The leg's line, its restricted runs and its steps are swapped for that route's, and the directions are renumbered, so the totals, the notices and the directions follow. The quickest stays shown until another is picked. If turn costs rank another best, the list says so: "With turn costs, the route via US 33 ranks best: 1 min slower on paper, 2 fewer U-turns." The choice holds until the stops or avoid options change, which ask for the route again. The via node is kept with each route (`Route::via`) for 4.7's saved trips.
> - **Via road** ("via I-40"): the road most of the route takes that the leg's other routes do not, by metres. It is `Routing::via_road`'s result, computed from the roads the route's own directions already looked up (`Routing::directions_roads_until`, `route::via_road_of`), and shows the main ref, else the name. `Road::short` gives it.
> - **Cost of reading:** a coast-to-coast leg's geometry is larger than the 16 MiB segment cache, so every pass over a leg decodes its `RGEO` blocks again. A first version took 1.0 s for New York–Los Angeles's alternatives: it made a separate pass each for the restricted runs and for the via road, and those passes took 85 ms and 115 ms per route. Two changes fixed it. `Piece` now carries its direction's flags, so `Leg::restricted` reads nothing (this replaces `Routing::restricted`, and saves the same pass on the route itself). The via road now comes from the directions' own roads. Result: 0.48 s, as `route --alternatives` takes with directions. A test with a 256 MiB segment cache took it to 0.19 s, but the cache stays at 16 MiB, since RSS is budgeted. A via road taken from the steps' roads would need no read at all, but it was wrong on 3 of 11 Ohio routes, because a step's road does not cover the name changes within it.
> - **Measured** (the Ohio flight with the first version, where every leg's alternatives were found and drawn, as now with every leg open; with the rows closed, as by default, nothing more is drawn or looked for):
>
>   | | Before (1851ed8) | With alternatives |
>   |---|---:|---:|
>   | Ohio route flight (`ohio-route.toml`, 2 runs each): frame interval p50 / p95 | 0.80 / 1.07 ms | 0.80–0.81 / 1.07–1.08 ms |
>   | — max | 22–28 ms | 26–33 ms |
>   | — RSS peak | 223 MiB | 227–229 MiB |
>   | — route shown after (trip; directions) | 111–117 ms (41; 56–58 ms) | 89 ms (41–42; 36–39 ms) |
>   | — its 6 alternatives shown after | – | 161–164 ms |
>
>   **North America, `--render --alternatives`** (one process per run, readers' caches cold, system cache warm). New York–Los Angeles: route 0.71 s including opening the pack. Alternatives 0.48 s: the search 0.21 s, then directions and lines 0.28 s. The routes are I-80 (shown), I-40 (+10 min) and I-70 (+58 min). New York–Los Angeles–Seattle: 0.62 s, and leg 2's alternative goes via US 395 (+2 h 37 min). Ohio's five stops: 68 ms for 6 alternatives. **In the app** (`na-alternatives.toml`, asked as the pack opens, from the disk; the flight opens every leg once the route is shown, `alternatives = true`): the route was shown after 438 ms, and its 3 alternatives 769 ms after the legs were opened; frame p95 1.28 ms. [`docs/screenshots/route-alternatives.png`](screenshots/route-alternatives.png) is its route page.
> - **Tests:** `alternatives_are_shown_and_picked` (worker stub): nothing is asked for until a leg is opened, and opening two legs asks for both in one job. The quickest stays shown when another ranks best. A route picked is swapped in with its steps renumbered. Clicks near a grey line and on a tag pick it, while one far away does nothing. A closed leg's grey line goes and cannot be clicked, and opening it again asks nothing more. Answers for another route are dropped, and a change of avoid option lets go of the routes and looks again for the legs left open. `alternatives_from_the_list` (headless panel): a row per leg, and opening one asks for its routes and then lists them. Picking from the list changes the totals and directions, and back. Closing the row hides the list and the grey line and keeps the route picked. The turn-cost note is right. `tags_go_where_routes_part`, `restricted_runs_from_the_pieces_flags`. On the builder's ladder network, `directions_roads_until` gives the same steps as `directions`, the same roads as `roads` and the same via road as `via_road`, and returns `None` when stopped.
> - **Left open:** the owner checked the first version by hand on North America; the revised rows are covered by the headless tests and the bench screenshot. The list is not checked at a 300-point panel: a leg's row is cut short with "…", and the routes' rows wrap.

## 4.4 U-turn penalty, measured first

From the roadmap (phase 3 decisions 13 and 16): a hidden penalty of a few minutes on U-turns at junctions. It counts in what the route minimizes but not in the time shown, like the restricted roads' entry penalty.

- **In the graph** (builder, behind `--uturn-penalty <s>`): a U-turn is arriving at node *v* from *a* and leaving back to *a*. The node-based graph cannot price a pair of edges, so a junction's node is split per incoming road, as turn restrictions split via nodes (3.1). Each copy leaves by every road, and leaving back the way it came carries the penalty. This reuses 3.1's automaton: a U-turn is the node path *a, v, a*, which today's automaton forbids for restrictions. Here it is kept, with a cost on that transition instead of a ban.
- **Where:** junctions with three or more roads (decision 13's option (b)). Nodes with two roads are not junctions, and dead ends keep their free U-turn, so cul-de-sacs stay routable. Restriction copies and U-turn copies must combine. The 3.1 test that checks every pair on a hand-built grid against a search over paths covers it.
- **Cost:** a junction with *k* roads becomes *k* copies with *k* arcs each, so arcs grow faster than nodes (decision 13 guessed about twice the nodes at junctions; arcs could be several times more). Measure on Ohio and Quebec: nodes, edges, CH edges, routing MiB, `RCPY` (U-turn copies are copies a destination must be snapped to), the build stages' time and peak, and query latency. Extrapolate to North America.
- **Effect:** penalties of 60, 180 and 300 s on 3.5's 1,000 random pairs each and the route set. Measure the U-turn steps against OSRM's 1.9% and 6.8%, times against OSRM (3.5's tool), and the route set, which must still pass. Compare with what 4.2's rescoring alone achieved.
- **Decision for the owner** with these numbers: build it for North America or not. If yes, rebuild, rerun `route --check` (exact against Dijkstra on the split graph) and the route set, and update the size table and format.md (no new section is expected; `RCPY` grows).

**Done when:** the measurements are in this plan and the size table, and the owner has decided. If the answer is yes, North America is built with it and checked.

> **Done 2026-10-28** (871bdc7): measured, and **not taken for North America** (owner, decision 20). Built as planned, behind `build`, `graph` and `ch --uturn-penalty <s>` (0, the default, gives phase 3's graph and sections: the same counts and sizes). The restriction automaton ([`restrict.rs`](../crates/builder/src/graph/restrict.rs)) takes each two-way road `[u, v]` into a junction (three or more roads) as an approach that bans nothing, so the copy knows u and its edge back to u carries the penalty (`Edge::uturn_ds`, in the weight, not the time). Where every road into v is two-way, one of them keeps the plain node instead of a copy, which saves a node per junction. Restriction copies and U-turn copies are states of one automaton, so they combine. In the pack: `RHDR` gains the setting `uturn_penalty_s`; `RCHE` is unchanged; an `RGEO` direction flag (16) and a bit on each `RCPY` leaf mark the way back, so a route that ends just after a U-turn pays it too ([format.md](format.md#rhdr-routing-header-contiguous)). Tests: 3.1's grid test again with restrictions and penalties of 1 and 60 s, against a search over node paths that charges `[u, v, u]` at junctions (and checks that the penalty changes some routes), and a reader test that every leg between two segments' middles weighs what a search over the graph's states finds. Logs and packs in `$OMP_WORK/measure/uturn44/` (`run.sh` reruns them all).
>
> **Cost** (same binary; 0 is the baseline, which matches phase 3's packs exactly):
>
> | | Ohio 0 | Ohio 60 / 180 / 300 s | Quebec 0 | Quebec 60 / 180 / 300 s |
> |---|---:|---:|---:|---:|
> | Junctions / approach copies | | 1.41M / 2.82M | | 0.48M / 0.91M |
> | Graph nodes / edges | 2.35M / 5.65M | 5.17M / 14.44M (×2.20 / ×2.55) | 0.84M / 1.95M | 1.75M / 4.77M (×2.09 / ×2.45) |
> | CH edges (shortcuts) | 9.05M (3.57M) | 29.3M (15.7M) / 29.7M / 29.7M (×3.2–3.3) | 3.26M (1.36M) | 9.66M (5.12M) (×3.0) |
> | `RCHE` / `RGEO` / `RCPY`, MiB | 47.8 / 43.9 / 0.09 | 159.5–165.6 / 53.2 / 13.6–13.8 | 15.5 / 17.9 / 0.07 | 48.5–51.3 / 21.9 / 4.9 |
> | **Routing sections, MiB** | **92.1** | **226.8 / 229.1 / 232.6** (×2.46–2.53) | **33.7** | **75.6 / 77.2 / 78.4** (×2.24–2.33) |
> | Graph stage / contraction | 5.2 s / 9.2 s | 8.3 s / 129, 200, 221 s | 4.6 s / 4.2 s | 5.5 s / 34, 54, 60 s |
> | Build wall time / peak RSS | 37 s / 4.9 GiB | 2:39, 3:52, 4:12 / 5.4 GiB | 51 s / 6.7 GiB | 1:25, 1:44, 1:50 / 6.8 GiB |
> | Nodes settled per query (`route --check`) | 292 | 489–497 | 197 | 262–265 |
> | Warm trips with geometry, p50: short / long | 0.51 / 3.17 ms | 0.55 / 3.19 ms | 0.42 / 1.00 ms | 0.43–0.44 / 1.02 ms |
>
> Decision 13 guessed twice the nodes at junctions; it is twice the nodes of the whole graph, because 60% of Ohio's nodes are junctions of three roads or more, and 2.5 times the edges. A higher penalty does not change the copies, but contraction slows with it (heavier edges need longer witnesses). Warm trips barely change: their time is decoding geometry (3.4), not the search, which settles 30–70% more nodes. **Still exact:** `route --check 10000 --pbf` on all eight packs, 10,000 pairs × 4 avoid combinations against Dijkstra on the split graph: 0 differ, 0 bad paths, 0 forbidden paths.
>
> **North America, extrapolated** (not built): routing 2,699.5 MiB × 2.2–2.5 ≈ **6.0–6.7 GiB, over criterion 7's 4.5 GiB** of phase 3 (the pack would grow from 9.8 to about 13.5 GiB). Contraction took 354 s in phase 3's build; at Ohio's and Quebec's ratios it would take about 50 min to 2 h, and its memory (19.6 GiB alone in 3.2) grows with the edges to perhaps 45–50 GiB, which does not fit beside what the build holds at that point (peak 49.7 GiB of the 62 GB machine) without spilling to disk.
>
> **Effect** (`route --osrm` on 3.5's 1,000 pairs and the route set, against OSRM 6.0.0's answers saved in 3.5):
>
> | | Ohio 0 | Ohio 60 / 180 / 300 s | Quebec 0 | Quebec 60 / 180 / 300 s | OSRM (Ohio / Quebec) |
> |---|---:|---:|---:|---:|---:|
> | Routes with a U-turn step | 4.7% | 3.3% / 3.3% / 3.3% | 11.2% | 9.6% / 9.4% / 9.5% | 1.9% / 6.8% |
> | — in town (< 10 km) / regional / highway | 3.0 / 4.8 / 5.7% | 1.7 / 3.9 / 3.6% | 9.3 / 11.0 / 12.6% | 7.4 / 9.5 / 11.1% (60 s) | 2.2 / 1.6 / 2.1%; 6.4 / 6.2 / 8.0% |
> | Time ours/OSRM, median; within 15%; same roads | 0.994; 99.4%; 92.8% | unchanged (92.7–92.8%) | 0.993; 98.6%; 92.1% | 0.993; 98.6%; 92.2% | |
> | Route set: pass; within 15% of OSRM; with a U-turn | 26/26; 76%; 28% | 25/26; 80%; 24% | 22/22; 77%; 23% | 21/22; 77%; 18% | 4% / 18% |
>
> Only about a third of the U-turn routes go, and the same third at 60 s as at 300 s: those are the U-turns the node split can see. The rest are U-turns **across a median**: on a divided road each carriageway is its own one-way way, so turning back crosses from one node to another and never revisits one (Pearl Road, Carnegie Avenue and Tylersville Road in Ohio's sample, 11 of 400 pairs at 60 s). Only a cost on the turn's angle catches those, which is 4.2's rescoring (OSRM's turn function from the geometry). The two set lines that fail are expectations written around the free U-turn, not wrong routes: `exit-171` now leaves I-90 at exit 172A for the same 6.5 km and 7 min, instead of exit 171 and a U-turn on East 9th Street; `jeanne-mance` no longer makes the U-turn on Rue Jeanne-Mance but goes round through a 40 m lane (1 min instead of 25 s), and the step "Turn left onto Rue Jeanne-Mance" out of that lane matches the line's text for the banned left from René-Lévesque, which the route does not take. Neither line was changed.
>
> **Compared with 4.2's rescoring:** not yet possible; 4.2 has not been started. Its rescoring needs no rebuild and costs nothing in the pack, and the measurement above says the median-crossing U-turns, the larger share, are within its reach and not this step's. *Measured in 4.2 (2026-10-28): rescoring reaches far fewer than expected, 4.7% → 4.6% (Ohio) and 11.2% → 10.7% (Quebec), because it only chooses among alternatives that differ by more than a block.*
>
> **Recommendation for the owner: do not build the penalty for North America.** It would take routing over its 4.5 GiB budget and the build likely over the machine, for a third of the U-turns. Keep `--uturn-penalty` in the builder (off by default) so it can be measured again after 4.2.
>
> **Decided by the owner, 2026-10-28: not built** (decision 20). North America is rebuilt only for 4.5's `TZON`. Left for 4.2: its U-turn rates are compared with this table's (the comparison this step could not make), and if rescoring leaves too many, `--uturn-penalty` can be measured again on top of it.

## 4.5 Schedule: times, stops and overnights

- **Trip settings** on the route page in a *Depart at / Arrive by* section under *More options* (decision 24): a date and time to depart at, or to arrive by; time spent at each stop (per stop, default 0); and a daily driving window (start at 8:00, stop by 18:00, or at most N hours of driving a day). Breaks: optional, every N hours for M minutes (also used by 4.6). Defaults live in `config.toml`, and each trip's own values in its file (4.7).
- **Output:** each stop's arrival and departure, and each step's clock time in the directions (a column, shown when a time is set). Where a day's driving ends, a "Day 2" row appears with the place it ends near (the nearest town from search's places), and on the map a small marker. **Make overnight stop** turns it into a stop with a night's duration, which 4.6 can fill with a hotel or campground. Arrive-by works backwards day by day.
- **Local times** (decision 5): each time is in the zone where it happens, shown as "14:05 CDT". The builder adds a small `TZON` section from timezone-boundary-builder's polygons (ODbL, made from OSM; added to `fetch` and to the attribution list), clipped to the region and simplified to about 100 m. The app holds it in RAM and finds a point's zone with a point-in-polygon test. The zones' rules come from `jiff` with its bundled tz database (`jiff` is already a builder dependency), so Windows and Linux give the same answers from a stick. Format 0.6. Measure the section's size on North America (aim ≤ 5 MiB) and the binary's growth.
- **Showing zones** (decision 14): the *Depart at* and *Arrive by* fields are in the zone of that stop and say so. Every time is followed by its zone ("14:05 CDT"). One in another zone than the departure's is drawn in an accent colour with the difference ("CDT, 1 h behind departure"); its tooltip gives it in the departure's zone and in the computer's own zone (from `jiff`'s system zone lookup) when that differs. The directions get a row where the route crosses into another zone ("Enter Central Time: clocks go back 1 h"), found by testing the route's points against `TZON` at each step. A trip that stays in one zone shows no marks at all.
- **Speeds are the profile's** (phase 3 decision 9): no traffic or time of day. The panel says "Times assume no traffic".
- **Tests:** the cross-zone trips of criterion 4, a trip that crosses a DST change at night, arrive-by over several days, and a window shorter than the first leg (the stop then falls on day 2).

**Done when:** criterion 4's test set passes, the schedule shows in the panel and directions on the North America pack, and `TZON`'s size is in the size table and format.md.

> **Done 2026-10-28** (commit "Step 4.5 done: schedule, local times and overnights"). Logs, screenshots and bench reports in `$OMP_WORK/measure/tz45/`; the North America pack is `measure/tz45/na-pack`.
>
> - **Time zones (`TZON`, format 0.6):** `fetch --sources tz` downloads timezone-boundary-builder's `timezones.geojson.zip`, pinned to release 2026e, the tz database version `jiff`'s bundled copy has (`fetch --tz-release`). It is in the manifest (ODbL, "Time zones: timezone-boundary-builder, © OpenStreetMap contributors"), so `META`'s sources and the About screen list it, and the snapshot says `tz 2026e`. `build` reads it first, while memory is free (1.3 s, 150 MiB), and keeps the zones whose polygons meet the region. A zone is kept if a vertex of either polygon is inside the other, using `--clip-poly` if given, else the bounding box. North America's header box spans all longitudes, so the box alone would keep most of the world. Rings are simplified to 100 m and clipped to the box. The encoding is in [format.md](format.md#tzon-time-zones-contiguous). North America: **74 zones, 108,281 vertices, 0.39 MiB** (aim ≤ 5 MiB). Indiana: 12 zones, 0.01 MiB.
> - **Reader (`omp_core::tz`):** the section is decoded whole when the routing worker opens the pack, and held in RAM: 2.2 MiB on North America, with each zone's edges bucketed in horizontal bands of 2²⁰ world units, so a point tests only the edges of its band. Rules come from `jiff`'s bundled database (`TimeZoneDatabase::bundled()`, features `tzdb-bundle-always` and `tz-system`), so Windows and Linux give the same answers. A point in no zone, in a sliver between borders simplified apart, takes the nearest zone within 1 km. The app binary grows from 14.66 to 15.47 MB (+0.79 MiB, `jiff` and its 200 KiB database).
> - **Schedule (`omp_core::schedule`):** from a departure or an arrive-by time, each stop's time there, an optional driving day and optional breaks, it gives each stop's arrival and departure, the nights and breaks, and the clock at any driving second (`Schedule::clock`). The driving day is a start and an end (8:00 to 18:00), an end past midnight, or at most N hours a day. Choices made here:
>   - The day's end is read in the zone where the driver is when it comes: in the start's zone, then in the zone where the stretch would end, until the two agree. A day that starts at 8:00 CDT and crosses into Mountain time ends at 18:00 MDT.
>   - A departure outside the driving day waits for its start, and the panel says "Leaves at 08:00, the start of the driving day." A stop whose time there runs past the day's end stays the night. *Overnight* at a stop goes on at the next day's start, with or without a driving day (8:00 by default).
>   - A break of M minutes comes after every N hours of driving. A night resets the count, and so does a stop at least as long as a break.
>   - Arrive-by finds the latest departure, to the minute, that arrives in time, by bisection: a later departure never arrives earlier. This gives what working back day by day would, and the forward pass then places the breaks and nights.
> - **Zone crossings** (`schedule::crossings`): each step's start is tested, and where the next step starts in another zone, the step's line is tested point by point. So a crossing is placed where the road crosses the border ("near Wolcott" on I-65, at the Jasper and White county line), not at the next turn. A zone left within 90 s of driving is a sliver and never counts as entered. Without this, I-69 near Princeton flickered Chicago, Petersburg, Chicago within 500 m. A crossing is shown only where the clock reads differently in offset or abbreviation, so America/Indiana/Indianapolis to America/New_York shows nothing, and Phoenix to Los Angeles in summer shows "Enter Pacific Time: clocks stay the same".
> - **Route page** (decision 24): *Depart at / Arrive by* has its own fold row under *More options*. It is closed by default and folds like *More options* whether a time is set or not (the owner, 2026-10-28: at first it stayed open while a time was set, with no triangle), and its title says what is set ("Depart Sat Jul 11, 08:00 EDT"). Its controls:
>   - No time, Depart at or Arrive by.
>   - A date and a time field, outlined when they don't parse, with the zone of the stop they are at ("EDT, at the start").
>   - Drive from … to …, At most … h of driving a day, and A break every … h for … min.
>   - "Times assume no traffic."
>
>   With a time set:
>   - Each stop's row says "Arrives Sun 11:50 CDT, leaves Sun 12:20 CDT". Its difference from the departure's zone is on a line of its own ("1 h behind departure"), in the accent colour, with a tip giving the time in the departure's zone and in the computer's when that differs. A stop between the ends has a "time here…" button that opens its field: minutes, and *Overnight*.
>   - The directions get a column of clock times, each in its step's zone. Rows for the schedule are added: "Day 2 · Sunday Jul 12" with "Day 1 ends near Fremont at 18:00 EDT; on at 08:00 EDT" and *Make overnight stop*; "Break, 15 min"; and "Enter Central Time: clocks go back 1 h".
>   - The map draws a navy disc with the day's number where each day ends along a leg.
>   - *Make overnight stop* inserts a stop there named after the town and marked *Overnight*, and routes again.
>   - A trip in one zone shows no marks.
>
>   The settings last used are saved in `config.toml` (`[schedule]`), for the next trip (4.7 saves each trip's own). The schedule is worked out on the UI thread when the route, its settings or a stop's time change: 0.3 ms for New York–Los Angeles with its crossings.
> - **Towns** for the nights: `search::Index::town_near` gives the nearest city or town within 25 km, else the nearest village within 10 km, reading only the importance buckets places are in. The app asks a thread of its own, which opens the search index the first time a night needs a town (about 6 MiB), so a trip without times costs nothing.
> - **Without a window:** `omp-builder route … --depart 2026-07-11T08:00` (or `--arrive-by`, with `--window 8:00-18:00|off`, `--max-drive`, `--break-every`, `--break-for`, `--stay`) prints each stop's times, nights and breaks with their towns, and each crossing with its point and zone. Bench flights take `depart = "…"`: `crates/app/bench/na-schedule.toml` is New York–Chicago–Denver–Phoenix–Los Angeles from Saturday 8:00.
> - **Tests** (criterion 4, `schedule_tests.rs`): straight legs through boxes named after the real zones, timed with the real rules:
>   - Across Arizona in summer: Los Angeles 08:00 PDT, Phoenix 13:00 MST (same offset), Albuquerque 21:00 MDT.
>   - Indiana's split counties: Gary, Indianapolis and Evansville, with an Eastern pocket between Central corners.
>   - Saskatchewan: in summer, Winnipeg to Regina goes back 1 h and Regina to Calgary is the same; in winter, Winnipeg to Regina shows no crossing.
>   - Newfoundland's half hour: Halifax 06:00 ADT to St. John's 18:30 NDT, with the crossing placed at 59.5° W.
>   - A DST change during the night: 18:00 CDT on Oct 31 to 08:00 CST, 15 h later.
>   - Arrive-by over four days (Sep 16 12:00, so leave Sep 13 17:00).
>   - A window shorter than the first leg (the stop falls on day 2), at most 8 h a day, a departure before the window, and an overnight stop.
>   - Breaks every 2 h, the day ending by the clock in the zone it ends in (twice), slivers, and times as typed.
>
>   `tz` tests cover holes, unknown zones, the nearest zone and long edges. The builder reads a test zip with a polygon covering the box. The headless panel test `schedule` covers turning on *Depart at*, a stop's time and *Overnight*, the night at a stop, and *Make overnight stop* along a leg.
> - **On North America** (`na-schedule.log`): New York–Los Angeles via Chicago, Denver and Phoenix, Saturday 08:00 EDT, 8:00 to 18:00. It takes 6 days and arrives Thu 09:52 PDT. The nights fall near Fremont OH, Johnston IA, Lochbuie CO, Mexican Water AZ (Navajo Nation, MDT) and Desert Hot Springs CA. The crossings are Central near New Carlisle IN, Mountain near Paxton NE, MST entering Arizona's own zone from the Navajo Nation, and Pacific (clocks stay the same) near Blythe. Screenshot: [`docs/screenshots/route-schedule.png`](screenshots/route-schedule.png). Indiana (`measure/tz45/indiana`, the builder loop's test pack): Gary 09:00 CDT, Indianapolis 12:54 EDT, Evansville 15:06 CDT.
> - **Measured:**
>
>   | | Before (9eecca2) | With 4.5 |
>   |---|---:|---:|
>   | Ohio route flight, no time set (2 runs each): frame interval p50 / p95 | 0.79–0.80 / 1.03 ms | 0.79–0.80 / 1.02–1.03 ms |
>   | — RSS peak | 219 MiB | 217–219 MiB |
>   | North America, `na-schedule.toml` with the schedule shown: frame p95, RSS | – | 1.38 ms, 231 MiB |
>   | App binary (release) | 14.66 MB | 15.47 MB |
> - **The North America pack** was rebuilt for `TZON` (50.9 min, peak 49.5 GiB, the same peak as before): 9,910.4 MiB in 3 chunks. Every section but the tiles has the same size as phase 3's pack. The tiles are 84 MiB smaller (`TDAT` 6,229.9 → 6,145.8, `TIDX` 67.2 → 65.7), because this build used `--clip-poly north-america.poly`, as phase 1 documented, and the phase 3 build did not: the ocean pieces that do not meet the region are left out (9.9M ocean tiles against 26.0M). This is the pack to use from now on.
> - **Left open:**
>   - The owner's hand check in the app.
>   - The E6430 (4.11).
>   - A step that leaves a zone and comes back within itself is not seen, since only steps whose start is in another zone are tested point by point.
>   - Driving into a zone whose day is already over (east at 17:30 CDT, which is 18:30 EDT) stops by the start zone's 18:00, not at the border.
>   - The page is not checked at a 300-point panel.

## 4.6 Fuel, rest and overnight suggestions (if cheap)

The owner asked for this only if it is easy and cheap. The plan expects it to be. **No new data:** fuel, charging stations, rest areas and services, hotels, campgrounds and towns are already in `SENT` (kind 3 POIs, kind 1 places). It costs **no standing RAM** beyond search's caches. **CPU is spent only at a few points:** where a trip needs something, not along its whole length.

- **Settings**, with the schedule's: fuel every N km or miles (default 400 km); rest every N hours (from 4.5's breaks); overnight from 4.5's day ends. Each can be off, and all are off by default (decision 7).
- **Where to look:** for each target point on the route (where the tank range is three quarters used, each break, each day end), look at a window of the route around it (for fuel, the last 25% of the range before it). In that window, find POIs of the classes wanted within 2 km of the route line, through `SENT`'s Morton runs in the class's importance bucket, or through the z12 tiles' `pois` layer if that reads less (measure both).
- **Detour:** for the best few candidates by distance off the route, two CH queries (route to it, it to the next stop) give the real detour, with no unpacking. That also drops a motorway rest area on the far carriageway, whose detour is large. Suggest the three with the smallest detour, with their name and brand, the detour ("+3 min") and the distance along the route.
- **App:** small icons along the route, drawn by the map renderer rather than as egui shapes so 4.9's printed map shows them (4.9 comes first, decision 25), and a list in the route page under each target ("Fuel near 610 km: …"). **Add as stop** adds it with a default duration (fuel 10 min, rest 15, overnight to the next day's start).
- **Measure** on the coast-to-coast routes on the dev machine and the E6430: time to the suggestions (aim: under 300 ms warm on the E6430), bytes read cold, and RSS. **If it is over budget, or the suggestions are poor** (wrong side of the road, closed POIs that OSM still has), write it up and drop or shrink it as the owner prefers. Charging stations come along for free, but range planning for electric cars is out of scope.

**Done when:** suggestions work on the North America pack within the budget, or the measurements are written up and the owner has decided.

## 4.7 Pushpins and saved trips

- **Files** (decision 8), next to the executable like `config.toml`: `trips/<name>.toml`, one file per trip; and `pins/<set>.toml`, one file per pushpin set. They are plain TOML, readable and copied with the app. If the folder is read-only (a locked stick), saving says so and offers the file browser, which is built in this step (decision 25) and reused by 4.8 and 4.10.
- **A saved trip** holds the stops (name, latitude and longitude, the street's name, duration, and kind: stop, overnight or hidden via point from 4.3), avoid options, the schedule settings, the optimizer's options, a format version and the pack it was made on (for information only). **No pack IDs:** on open, a stop with a street name is matched to a street of that name near its point (the `SGRD` lookup "what's here" uses) and snapped to it first. Otherwise it snaps as a point. A stop that no longer snaps well gets the far-snap notice.
- **Pushpins:** a point, a name, a note, and a symbol from a small set (shape × colour). Add one from the map's right-click menu, and from the "what's here" and search-result buttons ("Add pushpin"). They are drawn as instanced symbols over the map with their names from a zoom on, culled by view, under the route's pins. A click opens it on the Search page with its note and the route buttons. Pins are found by search (by name, in memory, listed above the pack's results).
- **Trips page:** saved trips (open, rename, duplicate, delete) and pin sets (show or hide, rename, delete, a list of a set's pins). The route page's *Save trip* saves, or saves as, and shows when the trip has unsaved changes.
- **Tests:** round trips of both files, matching a stop's street after a pack rebuild (a test pack with the street IDs shuffled), and the Trips page headless. Bench: 10,000 pins on the Ohio flight (criterion 5).

**Done when:** criterion 5 is met and trips and pins can be made, saved, opened and deleted from the panel on the North America pack.

> **Done 2026-10-29** (commit "Step 4.7 done: saved trips, pushpins and the file browser"). Bench logs in `$OMP_WORK/measure/pins47/`.
>
> - **Files** (`files.rs`): `trips/<name>.toml` and `pins/<set>.toml` next to the executable, written through a temporary file. A folder that cannot be written makes the save window say so, with *Save elsewhere…*. That opens the **in-app file browser**: Up, a folder field, subfolders and `.toml` files, a name box, and Replace for an existing file. *Open file…* on the Trips page uses it for trips and pin sets from anywhere. Steps 4.8 and 4.10 add their formats to it.
> - **A trip file** (`trips.rs`, version 1) holds `pack` (for information only), `avoid_tolls`, `avoid_ferries`, `[optimize]`, `[schedule]` (mode, date, time and the driving settings) and `[[stop]]` entries: name, lat, lon, `street`, `minutes` and `kind` (`stop`, `overnight` or `via`). Points are written to full precision, so the app's points come back to the world unit, and a trip saved and opened is unchanged (tested exactly).
> - **Streets by name, not ID:** the routing worker names each stop's street (`SSTR`). When a trip is opened, a stop with a street name gets the nearest street of that name within 1 km before routing (`Streets::named_near`, through `SGRD`, shared with "what's here"). A stop with no match snaps as a point, with the far-snap notice when it applies.
>   - Core test: the test pack built again with its street IDs reversed; every stop finds its street.
>   - Real packs: 50 West Broad Street, Columbus, saved from the panel on the Ohio pack (street 131009), opened on the North America pack, lands on West Broad Street there (street 7204075), on its street. The route to Cleveland is the same 230.0 km on both.
> - **A leg's alternative picked** is saved as a `via` entry where it passes its via node. **Changed from the plan:** the point is not routed through as a hidden stop, which would have split the leg and its row. Opening the trip looks for that leg's alternatives at once, without opening its row, and picks the one passing within 100 m. If none does on a newer pack, the quickest is shown, with a notice saying so.
> - **Saving** is on the Trips page, not the Route page (owner, 2026-10-29: the Route page is only for the route being worked on). *Save trip* asks for a name the first time (suggested "First to Last"), then saves in place. *Save as…* asks again. A line under them says whether the route shown is saved, as which trip, and whether it has unsaved changes; that trip's name is in bold in the list. The stops' street names, which routing fills in, and coordinates within the world unit are not counted as changes. Clear forgets the file.
> - **Pushpins** (`pins.rs`): name, note, lat, lon, shape (circle, square, diamond, triangle) and colour (6), in sets. *Add pushpin* is in the map's right-click menu and under "what's here" and a picked result, named after the result, the address found, or "Pushpin N". A new pin opens at the top of the Search page with its name selected. There you edit its note, symbol and set, use the route buttons, or delete it. A click on a pin on the map opens it there too.
>   - The map renderer draws pins as instanced quads over the labels, each shape from its distance in the shader (`vs_pin`/`fs_pin`). Their buffer is made only when pins change, so they cost one draw call a frame. Names are drawn by egui from z12, for at most 200 pins in view, under the route's markers.
>   - Search lists the shown sets' pins whose name or note matches, above the pack's results, worked out again only when the text or the pins change.
>   - A set's file is written when a pin is added, moved, deleted or given another symbol, and when its name or note field is left.
> - **Trips page**, in three sections split by lines like the Route page's: saved trips (*Save*, *Save as…* and *Open…* in the heading's row, then each trip's name, left aligned, which opens it, with drawn pencil, copy and bin buttons to rename, duplicate and delete it; the bin asks "Delete?" before deleting), pushpin sets (shown or hidden, a click on the name lists its pins, a click on a pin flies to it, pencil and bin, New set, *Save elsewhere…* for a set that could not be written), and import and export (their 4.8 placeholders). Changed after the owner's first look, 2026-10-29.
> - **Tests:** file and set round trips, names and copies, saving with and without a name, the route page's trip to a file and back (street found by name, the alternative picked again, and a lost one with its notice), the Trips page headless (open, rename, copy, delete, a pin from the menu, a set listed, hidden and deleted), and the real-pack test above (`OMP_TEST_PACK`, and `OMP_TEST_PACK2` for the second build), run on Ohio and on North America.
> - **Measured** (Ohio flight, 2 runs each, `ohio-pins.toml` = `ohio.toml` with `pins = 10000` spread over the pack):
>
>   | | Before (1017b39) `ohio.toml` | With 4.7 `ohio.toml` | With 4.7 `ohio-pins.toml` |
>   |---|---:|---:|---:|
>   | Frame interval p50 / p95 | 0.64–0.65 / 0.82–0.83 ms | 0.64–0.65 / 0.81–0.82 ms | 0.66 / 0.84 ms |
>   | CPU p95 | 0.20 ms | 0.20 ms | 0.21 ms |
>   | RSS peak | 188–189 MiB | 186 MiB | 187–189 MiB |
>   | App binary (release) | 15.47 MB | 15.74 MB | |
>
>   Criterion 5's 10,000 pins cost +0.02–0.03 ms of frame p95 (budget 0.1 ms). Frame times with no pins are unchanged.
> - **Left open:**
>   - The owner's hand check in the app.
>   - Opening a trip replaces the one shown without asking about its unsaved changes.
>   - A pin set opened from elsewhere is not remembered at the next start.
>   - A result's street name comes with its route, so a trip saved before the route arrives keeps that stop as a point.
>   - Pins' names are drawn by egui, so 4.9's printed map needs them drawn for the page; the symbols are the renderer's already.
>   - The page is not checked at a 300-point panel.

## 4.8 GPX, KML and CSV (best effort)

Not a hard requirement (owner's guidance): each format is kept only if it stays simple. Writers are a page of code each. Readers use `quick-xml` (already in the lock file through Wayland's build; measure the binary's growth).

| | Export | Import |
|---|---|---|
| **GPX 1.1** | A trip as `<rte>` of its stops plus a `<trk>` of the route's line, so GPS units and other apps show the roads taken. Pins as `<wpt>`. | `<wpt>` → pins. `<rte>` → a trip, its points as stops; with over 100 points (likely another app's shaping points), it asks whether to take them as stops or as pins. `<trk>` is not read (out of scope). |
| **KML 2.2** | Stops and pins as Placemarks, a set as a Folder, and the route as a LineString. | Point Placemarks → pins, a Folder → a set. Lines are not read. KMZ is not read (it needs a zip reader). |
| **CSV** | Stops with their times, pins (name, latitude, longitude, note, symbol), and the directions. | Header detection (name, lat or latitude, lon, lng or longitude, note, address), with commas, semicolons or tabs. Rows with no coordinates but an address are **geocoded by search**: the top hit is taken, and weak matches are flagged in a list of rows to check. |

- **Files** (decision 9): an in-app file browser in egui (folders, a file name box, the formats), starting next to the executable. No native dialog crate: `rfd` would pull in GTK or a D-Bus portal on Linux. The browser is built in 4.7 (decision 25), and this step adds the formats to it. The same browser serves 4.10's "Save as PDF".
- **Checks:** `xmllint --schema` for GPX 1.1 and KML 2.2, and GPSBabel reading our files on the dev machine. Our exports read back unchanged. Sample files from OsmAnd, Garmin BaseCamp and Google My Maps exports, kept under `crates/app/tests/` (owner to provide if wanted). Criterion 6's address CSV is made from search's address accuracy set (phase 2 step 2.9).

**Done when:** each format kept passes its checks, and any dropped format is written up here.

## 4.9 Print layout and preview

Two printouts (decision 17): **the map**, one page of exactly the area chosen on screen, and **the directions**, from their own button. This step lays them out and previews them; 4.10 sends them to a printer.

> **Done 2026-10-29**
> - **Pages** (`pages.rs`, `print.rs`, `pdf.rs`): the map is `snapshot::draw_view` at `dpi/72 × text size` pixels per point, for a camera that shows exactly the rectangle (tested to 1e-12 world units); egui draws the label, border, stop pins, pushpin names, scale bar and credit on a transparent layer of the same size, composited on the CPU. Directions reuse the route page's own row drawing (`PrintRows`) and are paginated with page numbers. The PDF writer (one Flate image per page, PNG predictors) is the one 4.10 needs; poppler opens it.
> - **App** (`printui.rs`): *Print map…* / *Print directions…* on the route page, ☰ menu, Ctrl+P. A `Print` side page (not a tab) with the settings kept in `config.toml` (`[print]`), the rectangle drawn over the map (drag inside to move, a corner to resize, aspect kept; tested with synthetic pointer events), and a thumbnail that opens larger, drawn by a worker thread with its own GPU device after a 250 ms pause. Seen working in the real app on North America.
> - **Headless:** `omp-app --render --pack … --route … --print out.pdf [--directions --paper --orientation --text-size --label --dpi --units]`.
> - **Tweaks after the hand check (owner):** Print is a fifth tab, opening the print page as the menu does (no heading or Close button; Escape or another tab leaves it); the tabs sit closer; the preview shrinks to the room left above the buttons; the corner cursors follow each diagonal; no "Letter on paper" caption.
> - **Open:** *Print…* and *Save as PDF…* are disabled placeholders for 4.10. Margins are a fixed 0.25 in. The night discs along a leg are not drawn on the printed map. Render time at 300 dpi and RSS on the E6430 are for 4.11.

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

> **Done 2026-10-29** (commits `d28c623` to `508c8db`)
> - **Code** (`printer.rs`, `printer/cups.rs`, `printer/gdi.rs`, `printui.rs`): the print page has a *Printer* box (kept in `config.toml`; the default is first), *Properties…* on Windows (the driver's dialog; its `DEVMODE` is kept for the session), paper sizes filtered to what the printer lists, *Print* and *Save as PDF…* (the file browser with a `pdf` extension). Both go to a second worker that draws the pages at the printer's resolution, up to 300 dpi (`OMP_PRINT_DPI` overrides, for the dpi comparison), one page at a time, and streams them to the PDF file, to `lp`'s stdin, or to the GDI device context. A unit test runs the CUPS path against a fake `lp`; the GDI code is type-checked against `windows-sys` for the MSVC target on Linux and built by CI.
> - **Margins:** the layout keeps its fixed 0.25 in. On Windows the printer's real unprintable margin is read; if it is larger the page says the edge is trimmed (the image covers the whole sheet, so the scale is right). Variable margins in the layout are not done.
> - **Hand checks (owner, 2026-10-29):** printing works on Linux and Windows (ET-2400, Letter), *Save as PDF* works on both, the printed edges match the rectangle, directions print well over several pages, and *Properties…* opens the driver's dialog. Fixes found on Windows: the preview worker's GPU device chose Intel's Vulkan driver and crashed (it now uses DX12 or GL like the window), and the printer queries moved off the UI thread, with a "Loading printers…" indicator.
> - **Resolution:** pages are drawn at the printer's own dpi (Windows reports 360 for the ET-2400; CUPS reports none, so 300), at most 600; `OMP_PRINT_DPI` overrides for comparisons. *Save as PDF* uses 300.
> - **Scale:** no ruler check. The scale bar is drawn into the page image by the same path as the map, so the printed bar matches the printed scale text as long as the driver prints at 100%, which `print-scaling=none` and the one-pixel-per-dot GDI draw give.
> - **`qpdf --check`** passes on the New York–Los Angeles map at 300, 360 and 600 dpi (0.9, 1.1 and 1.9 MiB) and on the 3-page directions.
> - **Headless render and peak RSS** (dev machine, total process, North America pack): 300 dpi 0.7 s, 227 MB; 360 dpi 0.8 s, 269 MB; 600 dpi 1.7 s, 500 MB; directions at 360 dpi 1.5 s, 321 MB. These include the GPU driver and the pack, so they are not the increase over the running app, which 4.11 measures against criterion 7's 100 MiB; 600 dpi will probably need the banded render.
> - **PDF viewers (owner):** saved PDFs look right in several viewers.
> - **Open:** the 360 dpi print on Windows (4.11's run); black and white (left for phase 5's high-contrast print style, owner's call); the increase in RSS in the running app on the E6430 and its render time (4.11).

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

Reordered by the owner on 2026-10-29 (decision 25) so that saved trips and printing come before suggestions and exchange formats:

```
Done:  4.1 optimize · 4.2 alternatives (core) ─► 4.3 app · 4.4 U-turns (not taken) · 4.5 schedule + TZON
Next:  4.7 trips + pins + file browser ─► 4.9 print layout ─► 4.10 printing + PDF
Then:  4.6 suggestions ─► 4.8 GPX/KML/CSV ─► 4.11 checks
```

- Printing needs what it prints: the schedule's times (4.5, done) and the pushpins (4.7). It does not need 4.6 or 4.8. A suggestion added as a stop prints as any stop does. 4.6's icons along the route must be drawn by the map renderer so they print (noted in 4.6).
- The in-app file browser (decision 9) moves from 4.8 to 4.7: saved trips need it when their folder is read-only, and 4.10's *Save as PDF* needs it. 4.8 adds only the formats. Printing to CUPS on Linux does not need it, since the PDF goes to `lp` on stdin.
- 4.9's test of egui at 300 dpi comes first in that step: it decides how pages are drawn.
- 4.10 is done only after printing on paper from Windows and Linux. If time is short before a demo, Linux CUPS printing and *Save as PDF* can be shown before Windows GDI printing.
- 4.4 was decided before 4.2 (decision 20: not taken), from its own numbers; 4.2 compares its rescoring with them.
- 4.5's `TZON` is the only format change planned (0.6), and the only North America rebuild this phase needs now that 4.4's penalty is not taken.

## Decisions

Proposed while planning and confirmed by the owner (2026-10-28):
1. **Optimize for driving time**, the route's own metric, with first and last stops fixed or open, or a round trip. Exact up to 12 stops to reorder, a deterministic heuristic beyond (no stop limit: decision 15).
2. **Alternatives by via nodes over the existing CH**: no second metric, no format change, up to two per leg. Penalty and plateau methods need a customizable metric or full Dijkstra trees, which the pack does not support at continent size.
3. **Alternatives are ranked by time plus turn costs** (OSRM's turn function, from the geometry), and the best is the one shown. The times shown stay the profile's, as with restricted roads' penalty, so legs, steps and totals keep adding up. *Changed by decision 23: the quickest is shown, and the ranking is a note in the leg's list.*
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

Settled by the owner (2026-10-28), from step 4.4's measurements:

20. **No U-turn penalty in the North America pack.** It doubles the routing sections (over phase 3's 4.5 GiB budget) and makes contraction 8–24 times slower, for a third of the U-turns: the rest cross medians on divided roads, which only 4.2's angle-based rescoring sees. `--uturn-penalty` stays in the builder, off by default.

Settled by the owner (2026-10-28), from step 4.2's measurements:

21. **Criterion 2 for Quebec is at least OSRM's share on the same pairs.** Over 50 km, 68.9% of Quebec's pairs get an alternative, against 70%; OSRM finds one for only 52.2% of the same pairs, so the shortfall comes from Quebec's roads more than from the method. Ohio (85.5%) and North America (75.6%) keep the 70%. Admissibility is unchanged (sharing 80%, stretch 25%).
22. **U-turns stay as they are for now.** Ranking with turn costs removes few (4.7% → 4.6% in Ohio, 11.2% → 10.7% in Quebec; OSRM 1.9% and 6.8%), because a route that avoids one differs from the quickest by a block, not by 20%. Not pursued in this phase: no penalty (decision 20), and no search for hidden variants without the U-turn (perhaps a day or two of work), which can be taken up later, for example with phase 5's routing work.

Settled by the owner (2026-10-28), from the hand check of step 4.3:

23. **Alternatives are looked for when the user opens a leg, and shown only while it is open.** Routing finds the quickest route only. Each leg has one row in the route page, with its distance and time. Opening it looks for the leg's alternatives ("Checking for other routes…"), lists them and draws them grey on the map. Closing it leaves only the route picked, so the map shows, and prints, just that. The quickest stays shown until the user picks another. Turn costs (decision 3) no longer switch the route by themselves; the list says which route they rank best.
24. **The route page's controls, top to bottom:** the stops; a row of Clear, Reverse and *Optimize…*; then *More options*, closed by default and drawn like a leg's row (same triangle, indent and hover), with Avoid tolls and Avoid ferries one under the other (closed, its title says what is avoided: "More options: avoiding tolls"). *Optimize…* opens a window beside the panel with the ends to keep, Optimize (Cancel while it runs), Close, what the last run saved, with Undo, and why Optimize is greyed out when it is ("Already optimal: with these ends kept, the stops have only one order."); closing it lets a run finish. The "Route" heading is gone (the tab says it). For 4.5: *Depart at / Arrive by* gets its own section under *More options*, which folds like the others; its title says the time set, so closed it still shows it (changed by the owner after trying 4.5: it was to stay open whenever a time is set); the driving window and breaks go in that section, a stop's time in its row (a field that opens from it), and 4.6's fuel and rest settings with the schedule's. 4.7's *Save trip* stays with *Print or export* under the totals.

Settled by the owner (2026-10-29), for a demo:

25. **Saved trips and printing come before suggestions and exchange formats:** 4.7, 4.9 and 4.10, then 4.6 and 4.8. The in-app file browser is built in 4.7 rather than 4.8. All of phase 4 is still planned; this changes only the order.

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

- **Directions run after the trip and cannot be stopped by a newer request** (3.7). 4.3's alternatives come after the directions, so they must be cancellable from the start. Phase 5's "show the route's line before its directions" also helps them. **Done in 4.3:** directions and alternatives both stop on a newer request (`Routing::directions_until`).
- **Free U-turns** (phase 3 decisions 13 and 16): 4.2's rescoring and 4.4's penalty.
- **How the route page's buttons wrap in a narrow panel** (3.10): the page gains many controls here, so check it at 300 points.
- **Phase 5's print style and UI size setting:** the print page should take a style and a text scale as parameters, so phase 5 only adds choices.

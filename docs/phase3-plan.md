# Phase 3 Plan: Car Routing

*Draft 2, 2026-10-27, with the owner's answers to the planning questions (decisions 1–12). Expands Phase 3 of [roadmap.md](roadmap.md#phase-3-car-routing). Builds on the routing estimate from phase 0 ([step 0.3](phase0-plan.md#routing-and-search-estimates), [0.5](phase0-plan.md#05-full-north-america-run)), the provisional routing note in [format.md](format.md#routing--provisional), the street table and spatial grid from [phase 2](phase2-plan.md), and the builder as restructured by the [refactor plan](refactor-plan.md).*

## Purpose

Phase 3 makes the app plan drives. When it is done, someone can right-click a spot or pick a search result, choose "Route from here" and "Route to here", add stops in between, tick "avoid tolls" or "avoid ferries", and get the route drawn on the map with its distance, driving time and a list of directions ("Turn left onto Main St", "Take exit 12 toward Columbus", "Take the ferry"), all on a laptop with no network, from a USB stick.

Phase 0 measured only the size of an off-the-shelf contraction hierarchy (CH) from `fast_paths` on a bare graph:

| | Ohio | Ontario | Quebec | North America |
|---|---:|---:|---:|---:|
| Graph nodes / directed edges | 2.21M / 5.28M | 1.27M / 3.04M | 0.76M / 1.78M | 62.5M / 149.6M |
| CH edges (up + down) | 7.83M (1.48 per edge) | 4.51M | 2.71M | about 225M (estimated) |
| `fast_paths` contraction, one thread | 20.5 s | 10.9 s | 5.9 s | not run |
| Turn restriction relations | 20,859 | 16,344 | 16,843 | 567,305 |
| Ways with `toll` | 1,594 | 1,663 | 190 | 100,147 |

The size table reserves **about 3.2 GiB** for the North America CH (12 bytes per node and per CH edge). What phase 0 did not build:
- **A car profile.** Speeds come from a short table by road class and `maxspeed`. There is no handling of `access=destination`, barriers, `junction=roundabout`, `maxspeed` in mph, ferries or tolls.
- **Turn restrictions.** They were counted, not applied.
- **Avoid options.** A plain CH has one fixed metric; leaving out tolls or ferries at query time breaks it (decision 3).
- **Anything the app can read.** No route geometry, no road names on the graph, no way to snap a point to a road, no reader that works from positioned reads on a USB stick, and no layout for cold reads.
- **Directions.** Nothing turns a path into turns and road names.
- **Contraction at continent size.** Single-threaded `fast_paths` on 62.5M nodes was never run, and `build` already peaks at 47 GiB of the 56 GB cap.

### Exit criteria (expanded)

The roadmap's items are "road graph, car profile, turn restrictions and contraction hierarchies" in the builder, and "A to B and multi-stop routing, a directions list, distance and time, a route overlay, and avoid options" in the app. In numbers, over the North America pack:

1. **Coverage.** Every road a car may use is in the graph: `highway=motorway` to `living_street`, their links, `service` roads and car ferries, with `access`, `motor_vehicle`, `motorcar` and barriers applied. Between 1,000 random address points in the 48 contiguous states and southern Canada, paired at random, at least **99.5%** get a route, and each end snaps to within 50 m of its point or to the address's own street.
2. **Exact.** The CH gives exactly the same travel time as a plain Dijkstra on the same graph for 10,000 random pairs on Ohio and Quebec and 1,000 on North America, with no avoid options and with each combination of them. No route in those sets breaks a turn restriction.
3. **Right routes.** A hand-written route set (step 3.5, kept in the repo; about 60 routes in Ohio, Quebec, Ontario and across the continent) meets its expectations in at least **95%** of cases: the expected highway is used, the toll road or ferry is avoided when asked, a known restriction is honoured, and the directions name the expected turns.
4. **Plausible times and routes.** Driving times are within **15%** of OSRM's car profile on the same extract (decision 12) for at least 90% of the hand-written routes and of 1,000 random pairs each on Ohio and Quebec. Where the two engines choose different roads, the reason is checked on a sample and written up.
5. **Fast.** On the dev machine, warm: a route under 100 km within **50 ms** p95, and coast to coast (Miami to Seattle, Halifax to Los Angeles) within **500 ms**, including the geometry and directions. On the Latitude E6430, warm: within 2.5× those. The first route after a cold start from the USB 2 stick within **3 s**. A 10-stop route within **2 s** warm on the E6430. Routing runs off the UI thread and never drops frames.
6. **Memory.** Peak RSS stays under 300 MB on the dev machine after the bench flight with its routes (the Windows exceptions of phase 1 decisions 21 and 22 stay accepted). What routing keeps in RAM is budgeted and counted in the F3 overlay.
7. **Size.** Routing adds at most **4.5 GiB** to the North America pack (accepted by the owner, 2026-10-27; the CH estimate of 3.2 GiB, plus the route geometry, names and snapping grid that phase 0 did not count). The app stays under 30 MB.
8. **Builder.** `omp-builder build` writes tiles, search, addresses and routing in one run for North America under the 56 GB cap, with wall time and peak RSS recorded per stage. Aim: the routing stages under 45 min.

## Status

| Step | Status | Notes |
|---|---|---|
| 3.1 Car profile and road graph | Not started | |
| 3.2 Contraction hierarchy | Not started | |
| 3.3 Routing format and core reader | Not started | |
| 3.4 Queries, snapping and multi-stop (`omp-core`) | Not started | |
| 3.5 Directions and the route set | Not started | |
| 3.6 Routing in the app: stops, worker and overlay | Not started | |
| 3.7 Route panel and directions list | Not started | |
| 3.8 Performance, accuracy and hardware checks | Not started | |

## Environment notes

As in [phase 2](phase2-plan.md#environment-notes): builder data in `$OMP_WORK`, `target/` on `/`, packs in `$OMP_WORK/packs/`. The phase 2 inputs (Geofabrik `261001` extracts, address sources) stay current unless a step needs fresher OSM data.

**Test regions:** Delaware for fast loops; **Ohio** (the Ohio Turnpike's tolls, the Lake Erie island ferries, dense interstate junctions, 20,859 restrictions) and **Quebec** (ferries across the St. Lawrence, the toll A-25 and A-30 bridges, French road names in directions, many roundabouts) for every builder change; then North America. Ontario is checked once in 3.5 for the toll 407 ETR.

**Memory:** run North America builds under `systemd-run --user --scope -p MemoryMax=56G -p MemorySwapMax=0`, as before.

**OSRM as the reference** (decision 12): OSRM's `osrm-backend` with its stock `car.lua`, run on the dev machine only, on the same Ohio and Quebec extracts the packs are built from (`$OMP_WORK/osrm/`). It is a build-time tool for checks; nothing from it goes into a pack or the app. The dev machine has neither Docker nor Podman yet: install Podman (`apt install podman`, needs the owner's sudo) and use the project's image, or build OSRM from source if that is easier. It is set up in 3.1, where its profile is the one ours is compared against. North America is too large for it on this machine and is not needed.

**Routing without a window:** step 3.4 adds `omp-builder route <pack> <lat,lon> <lat,lon>… [--avoid tolls,ferries] [--directions] [--geojson <file>]` and `route --check`, thin wrappers over `omp-core`, so routing is tested in a session without the app, as `search` and `here` are. Step 3.6 adds a route to `omp-app --render`, so the overlay is checked with snapshots.

---

## 3.1 Car profile and road graph

Build the car graph in the builder from OSM, as its own stage, and measure it. No CH yet.

- **Read** the routable ways in their own PBF passes after the address and search stages have freed their memory (as `measure --routing graph` does, through `pbf::Pbf`'s typed passes): `highway` from the whitelist's road classes, `route=ferry` ways, and the nodes they use. Keep only what routing needs per way: class, speed, flags, the way ID (for the street table and names), and the node refs. Node locations come from the needed-nodes store, limited to road nodes.
- **The car profile**, in `omp-core` so the builder and tests share it (`omp_core::profile`), modelled on OSRM's `car.lua`:
  - **Access:** `access`, `vehicle`, `motor_vehicle` and `motorcar`, most specific first. `no` and `private` drop the way; `destination` and `delivery` keep it with a time penalty, so routes use it only to start or end there. `highway=construction`, `proposed`, `track`, `path` and `footway` are not routable.
  - **Direction:** `oneway=yes|-1|no`, `junction=roundabout|circular`, and motorways and their links one-way by default.
  - **Speed:** `maxspeed` (parsing km/h, `mph`, and `CA:urban`-style zone values), scaled by a factor below 1 (OSRM uses 0.8) since traffic seldom drives at the limit; otherwise a default by class, lower for unpaved `surface` and for `service` subtypes (parking aisles, drive-throughs, driveways). Travel time in tenths of a second, as phase 0.
  - **Ferries:** `route=ferry` with `motor_vehicle` or `motorcar` not `no`, timed from `duration` where present, else a default speed (about 20 km/h, plus a fixed boarding time). Kept only where both ends meet a road.
  - **Barriers:** `barrier=gate|lift_gate|toll_booth|…` nodes pass with a penalty; `bollard`, `block`, `jersey_barrier` and the like split the road.
  - **Traffic signals:** `highway=traffic_signals` nodes add a delay (OSRM's 2 s to start; tuned in 3.5) to the edges through them. A signal is a node, not a turn, so this needs no turn costs, and in towns it is much of what OSRM's penalties add.
  - **Flags per edge:** `toll` (`toll=yes`, and `barrier=toll_booth` on the edge's node) and `ferry`. They feed the avoid options (decision 3).
  - **Checked against OSRM:** go through `car.lua` rule by rule and write down where ours differs and why (OSRM also prices turns and U-turns, which ours does not; decision 2). Then run OSRM on Ohio and Quebec and compare the two graphs' coverage: ways OSRM routes that ours drops, and the reverse.
- **Turn restrictions** (decision 2): `type=restriction` relations (and `restriction:motorcar`, minus `except=motorcar`) with `no_*` and `only_*` values, applied by **splitting the via node**: a node with restrictions gets one copy per incoming way, each copy with only the turns allowed from that way. Via-way restrictions split the via way's nodes along it the same way. Count the restrictions applied, those with missing or broken members (cut at the extract edge, or members that do not touch), and the nodes added.
- **Connected components:** find the strongly connected components. Small ones (a gated community, an island parking lot, a mapping error) are kept, but marked, so snapping (3.4) prefers the large network and a route never starts inside a pocket it cannot leave.
- **Measure** on Ohio, Quebec and North America: nodes, edges, nodes added by restrictions, components and their sizes, ways dropped by access, and the time and peak RSS of the stage. Write the graph to a temp file so 3.2 can be rerun alone, and cache it under `--reuse-stages`.

**Done when:** OSRM runs on Ohio and Quebec, the profile differences from `car.lua` are listed in this plan, Ohio, Quebec and North America graphs are built with the profile and restrictions, the counts above are in the size table, a unit test covers each profile rule and restriction type on a hand-built graph, and the stage fits under the cap on North America.

## 3.2 Contraction hierarchy

Contract the graph in the builder with our own CH (decision 1).

- **Node order:** by the usual priority (edge difference, contracted neighbours, depth; the weights to tune on Ohio), with lazy updates.
- **Parallel contraction:** contract an independent set of nodes in each round across threads, as in the CH literature, so North America takes minutes, not hours. Single-threaded `fast_paths` took 20.5 s for Ohio's 2.2M nodes; North America's 62.5M would be well over an hour on one thread.
- **Avoid flags in the hierarchy** (decision 3): a shortcut carries the union of its edges' flags, and a **witness path only removes a shortcut when its own flags are a subset of the shortcut's**. Then, for any set of avoided flags, the hierarchy without the flagged edges still holds every shortest path of the graph without them, so one CH answers all four combinations of tolls and ferries exactly (label-constrained contraction, Rice and Tsotras 2010). The cost is more shortcuts, because witnesses that use a toll road or ferry no longer count. Toll and ferry edges are rare (100k of North America's 38.8M routable ways have `toll`), so the overhead should be small. **Measure it on Ohio and Quebec first**, against the same CH without flags; if it is over about 10%, write it up for the owner before going on.
- **Memory and time** on North America: the contraction graph, witness searches and the order. Spill the base graph to disk if needed; record wall time and peak RSS. Shortcut middle nodes are kept for unpacking.
- **Check** against Dijkstra (and against `fast_paths` on the same graph without flags): 10,000 random pairs on Ohio and Quebec, every combination of avoid flags. Times must match exactly.

**Done when:** Ohio, Quebec and North America CHs are built, the flag overhead is measured and the avoid approach settled with the owner, the Dijkstra check passes, and the CH edge counts, contraction time and peak RSS are in the size table.

## 3.3 Routing format and core reader

Specify the routing sections in format.md (format 0.5), write them from `build`, and read them in `omp-core` with positioned reads (phase 1 decision 1).

- **Sections** (names from format.md's provisional note; final list in this step):
  - `RCHN`: per node its position, its CH level, the first of its up and down edges, and its component flag.
  - `RCHE`: up and down edges in CSR form: target, weight (24 bits of tenths of a second, up to 466 hours), the avoid flags and a shortcut bit. Shortcut middle nodes in a separate array indexed by shortcut rank, so base edges do not pay for them.
  - `RGEO`: per base edge its geometry (simplified to about 2 m, delta-coded, in zstd blocks like `SGEO`), its length, its way's street ID in `SSTR` (or a name and `ref` in a small table for ways with no street: `_link`s, unnamed service roads, ferries) and the maneuver data of 3.5 (`junction:ref`, `destination`, `destination:ref`).
  - `RGRD`: z14 cells to the base edges passing through them, as `SGRD` does for streets, for snapping.
- **Layout for cold reads.** A CH query touches few nodes, but they are scattered: on a USB 2 stick each random 4 KiB read costs about a millisecond, and the top of the hierarchy is touched by almost every long route. Order the nodes so the top levels sit together (a few tens of MiB that one long read brings into the cache), and the rest along a Hilbert curve, so local searches and unpacking read nearby pages. Compare on North America: fixed-width records (one page read per node) against varint-coded blocks (smaller, but a block to decode per node), by bytes read and time per query, cold and warm.
- **Reader:** `omp_core::route`, with a block cache budgeted like the search readers', and counted in the F3 overlay.
- **Size:** measure every section on Ohio, Quebec and North America, against criterion 7's 4.5 GiB.

**Done when:** format.md specifies the routing sections, `build` writes them, `inspect --readers` reads and cross-checks them on Ohio and Quebec, the North America sizes are in the size table, and the layout is chosen from cold-read numbers.

## 3.4 Queries, snapping and multi-stop (`omp-core`)

- **Query:** bidirectional CH search with stall-on-demand, skipping edges whose flags are avoided, then unpacking shortcuts to base edges and their geometry. Legs of a multi-stop route are separate queries, run in order and shared through the reader's cache.
- **Snapping** a point to the graph: the nearest base edge in `RGRD`, preferring the large component (3.1), with both directions of the edge as candidate starts (an address on a one-way street is reached from its direction only). A search result that is an address or a street snaps to its own street's edges first, so "123 Main St" does not start on the interstate beside it. Report the snap distance; over 500 m the app warns.
- **No route:** when a leg has no path with the avoid options, try again without them and say which one the route has to break ("This route uses a ferry"). When there is none at all (another landmass, a pocket), say so.
- **CLI:** `omp-builder route` prints the legs, distance, time and snap distances, and `--geojson` writes the line. `route --check` runs the random pairs against a Dijkstra in the same reader (criterion 2), checks every path against the restriction list, and reports query latency.

**Done when:** `route` works on the Ohio, Quebec and North America packs, `route --check` passes criterion 2, and warm latency on the dev machine is recorded for short and coast-to-coast routes.

## 3.5 Directions and the route set

Turn a path into directions, and write the hand-written route set.

- **Maneuvers** from the base edges of the path, at each node where the road changes, modelled on OSRM's guidance code (`src/extractor/guidance` and `src/engine/guidance` in `osrm-backend`): its turn types, angle thresholds, and how it collapses name changes and short segments are a tested starting point. Its output on the same routes is a check (below).
  - depart and arrive (with side of the street, from the snap), and the stops between legs;
  - turn left, right, slight and sharp, and U-turn, from the angle between the edges' geometry;
  - continue, when only the name changes (collapsing short name changes and dual carriageways' gaps);
  - keep left or right at a fork, take the ramp, merge, take exit `junction:ref` toward `destination`;
  - roundabouts ("take the 2nd exit onto …"), counted from the exits on the ring;
  - take the ferry, and leave it.
- **Names:** the street's name and its `ref`s from `SSTR` ("US 33 / Main St"), with the direction of the route relation where OSM has it ("I-71 North"). Directions text is English (decision 11).
- **Each step** has its distance, time and the cumulative values, in the units from `config.toml`.
- **The route set:** about 60 routes in a TSV kept in the repo next to the search query set, each with start, end, stops, avoid options and expectations (roads that must and must not be used, a restriction that must be honoured, steps that must appear, a time range). `route --check --set` runs it. Sources: known junctions and restrictions in Ohio and Quebec, the toll roads and ferries, and cross-border routes.
- **Against OSRM** (criterion 4): the same routes and 1,000 random pairs each on Ohio and Quebec through OSRM's HTTP API on the dev machine, saved as TSV next to the route set so the check reruns without OSRM. `route --check --osrm <tsv>` compares time, distance, how much of the two lines overlap, and the number of steps. Expect our times a little shorter, since OSRM adds turn penalties: if the median ratio is well off 1, tune the profile's speed factor and signal delay (3.1) on Ohio and confirm on Quebec. Look at the pairs with the largest differences and sort them into causes (profile, restriction, snapping, turn costs).
- **What skipping turn costs costs** (decision 2): run OSRM a second time with its turn and U-turn penalties set to 0, on the same pairs. The difference between its two runs, in time and in roads chosen, is the effect of turn costs alone, split by trip length (in town, regional, highway). Write it up; if it is large on short trips, the options go to the owner (a small fixed delay per intersection, or an edge-based graph after all).

**Done when:** `route --directions` gives readable directions for the whole set, the set passes criterion 3 on the Ohio, Quebec and North America packs, and the OSRM comparison of criterion 4 is recorded with its causes.

## 3.6 Routing in the app: stops, worker and overlay

- **Stops:** the right-click menu gets "Route from here", "Route to here" and "Add stop"; a search result gets the same. Stops are numbered markers on the map, drawn like the search marker.
- **Worker:** routing runs on a worker like search's (`search/worker.rs` is generic over an `Engine`, so it is reused or copied), with stale answers dropped and a new request cancelling the old.
- **Overlay:** the route line drawn by the map renderer after the roads and before the labels, so labels stay readable, with a halo. Simplified per zoom from the full geometry and tessellated with `lyon`, re-done only when the zoom band changes. The view fits the route when it first arrives.
- **Render snapshots:** `omp-app --render` takes `--route lat,lon;lat,lon…` and draws it, for checks without a window.

**Done when:** a route can be made from the map and from search, it is drawn on the map at every zoom without frame drops (bench frame p95 unchanged), and a render snapshot shows a route in Ohio and in Quebec.

## 3.7 Route panel and directions list

- **Panel:** the stops in order (drag to reorder, remove, swap start and end), the avoid options (tolls, ferries) as checkboxes that reroute at once, and the totals: distance and driving time, per leg and in all.
- **Directions list:** the steps of 3.5 with icons, distance and time; clicking one zooms to it and highlights its part of the route. A notice when the route breaks an avoid option or a stop snapped far away.
- **Units** from the settings already in `config.toml` (miles, kilometres).
- **Keyboard:** Escape closes the panel as it does the search results.

**Done when:** every item above works on the North America pack, with a headless UI test of the stop list like search's `ui_tests.rs`.

## 3.8 Performance, accuracy and hardware checks

As phase 2 step 2.9, with routing added:
- Rebuild North America with routing; update the size table and roadmap §4 with measured sizes.
- Run `route --check` and the route set on North America.
- Bench with routes on the dev machine (warm, and cold from the FAT32 USB 2 stick), the Latitude 7490 and the E6430: route latency short, long and with 10 stops; time to the overlay and directions; RSS peak; frame time while a route is shown; start-up.
- Check binary size and that `cargo tree` still has no network crate.

**Done when:** every exit criterion is measured on every machine, with any that miss written up for the owner.

---

## Order of work

```
3.1 profile + graph + OSRM ─► 3.2 CH (flag overhead) ─► 3.3 format + reader ─► 3.4 queries + CLI ─┬─► 3.5 directions + route set ─┬─► 3.7 panel ─► 3.8 checks
                                                                                                        └─► 3.6 app stops + overlay ───┘
```

- 3.2's flag measurement on Ohio comes first in that step: the avoid approach decides what `RCHE` holds.
- Build the North America graph at the end of 3.1 and the CH at the end of 3.2, so memory and time surprises show up early.
- 3.5 and 3.6 both need 3.4's queries and can run side by side; 3.7 needs both.

## Decisions

Proposed while planning and confirmed by the owner (2026-10-27):
1. **Our own CH**, not `fast_paths` or OSRM's: contraction in the builder, queries in `omp-core`. `fast_paths` has no avoid flags, no layout control and its own serialization; it stays in `measure` as a check.
2. **A node-based graph, with restricted via nodes split** (3.1), and **no turn costs**, to keep the pack small. An edge-based graph (OSRM's) would also price left turns and U-turns, giving better times and fewer odd turns, but it has roughly 2–3× the nodes, which would take the CH from about 3 GiB to 6–9 GiB. Splitting adds a node per restricted approach, a small fraction of 62.5M. Traffic signals are priced anyway, as node delays (3.1). Step 3.5 measures what turn costs would have changed.
3. **Avoid tolls and avoid ferries are hard exclusions**, answered by one CH whose witness rule respects the flags (3.2). If a leg has no route without them, it is routed with them and the panel says so.
4. **Quickest route only.** Streets & Trips also offered "shortest"; that is a second metric, so a second CH (about 3 GiB more). Left for later.
5. **Stops in the order given.** Reordering them for the shortest trip is phase 4.
6. **Positioned reads with a budgeted cache**, as for tiles and search (phase 1 decision 1).
7. **The route's geometry is stored in the pack** (`RGEO`), not read from the z14 road tiles, which are clipped and merged (as phase 2 found for streets).
8. **Road names come from the street table** (phase 2 decision 2), with a small table for ways that have no street.
9. **Speeds from a fixed profile:** no traffic, time of day or ferry timetables. Border crossings add no time.
10. **No "avoid highways".** Most trips need motorways (interstates, freeways and other limited-access roads), so the option would rarely be used, and motorways carry most of the witness paths in the CH, so it would add the most shortcuts. Left for later.
11. **Directions in English only**, with names as OSM writes them ("Rue Sainte-Catherine"). French directions for Quebec are left for later.

Settled by the owner (2026-10-27):
12. **OSRM is the reference**, run on the dev machine for Ohio and Quebec: for driving times and route choice (criterion 4, step 3.5), for the car profile (3.1), and for how directions are built (3.5). Its design is borrowed where it fits (OSRM is BSD-2-Clause; ideas are free to use, and any code adapted from it keeps its copyright notice), except its edge-based graph and turn costs (decision 2).

Open: none.

## Out of scope for phase 3

- Stop optimization, departure and arrival times, overnight and fuel stops (phase 4).
- Saving routes, and printing or exporting directions (phase 4).
- Dragging the route to add a via point; "avoid this area" (roadmap §5: it would need customizable routing).
- Live GPS and turn-by-turn navigation.
- Truck, RV, bike and foot profiles.
- A separate "shortest" metric (decision 4), "avoid highways" (decision 10) and turn costs (decision 2).
- Builder memory for a 16–32 GB machine (phase 6).

## Carried from earlier phases

- **Snapping uses the street table and grid** kept general in phase 2 for this (decision 2 there): address and street results snap to their own street (3.4).
- **Profiling the address merge** (refactor R4.5) matters more once routing adds its stages to `build`.

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
| 3.1 Car profile and road graph | **Done** 2026-10-27 | North America 63.7M nodes / 152.7M edges, 511k restrictions applied (+0.8% nodes), stage 231 s, build peak 47.4 GiB. OSRM 6.0.0 runs on Ohio and Quebec; coverage differs only where the profile table says |
| 3.2 Contraction hierarchy | **Done** 2026-10-27 | Exact against Dijkstra (0 mismatches; Ohio and Quebec 10,000 pairs × 4 avoid combinations); flag overhead 0.005%, so decision 3 stands; North America 1.65 CH edges per base edge (about 3.5 GiB), stage 293 s, build peak unchanged at 47.7 GiB |
| 3.3 Routing format and core reader | **Done** 2026-10-27 | Format 0.5: `RHDR`, `RCHE`, `RGEO`, `RNAM`, `RGRD`, `RCPY`, `RJCT`. Varint blocks of 32 nodes with middle nodes inline, top 32 MiB first; North America 2,508 MiB (criterion 7: 4.5 GiB), build peak unchanged at 48.1 GiB |
| 3.4 Queries, snapping and multi-stop (`omp-core`) | **Done** 2026-10-27 | Stall-on-demand halves settled nodes; `route --check` exact on Ohio and Quebec (10,000 pairs × 4) and North America (1,000 × 4), 0 forbidden paths driven; warm short trips 0.17 ms p50, coast to coast about 100 ms; caches 16 + 16 MiB |
| 3.5 Directions and the route set | Not started | |
| 3.6 Routing in the app: stops, worker and overlay | Not started | |
| 3.7 Route panel and directions list | Not started | |
| 3.8 Performance, accuracy and hardware checks | Not started | |

## Environment notes

As in [phase 2](phase2-plan.md#environment-notes): builder data in `$OMP_WORK`, `target/` on `/`, packs in `$OMP_WORK/packs/`. The phase 2 inputs (Geofabrik `261001` extracts, address sources) stay current unless a step needs fresher OSM data.

**Test regions:** Delaware for fast loops; **Ohio** (the Ohio Turnpike's tolls, the Lake Erie island ferries, dense interstate junctions, 20,859 restrictions) and **Quebec** (ferries across the St. Lawrence, the toll A-25 and A-30 bridges, French road names in directions, many roundabouts) for every builder change; then North America. Ontario is checked once in 3.5 for the toll 407 ETR.

**Memory:** run North America builds under `systemd-run --user --scope -p MemoryMax=56G -p MemorySwapMax=0`, as before.

**OSRM as the reference** (decision 12): OSRM's `osrm-backend` with its stock `car.lua`, run on the dev machine only, on the same Ohio and Quebec extracts the packs are built from (`$OMP_WORK/osrm/`). It is a build-time tool for checks; nothing from it goes into a pack or the app. It runs in Podman from the project's image `ghcr.io/project-osrm/osrm-backend:v6.0.0`, set up in 3.1 (commands in the 3.1 note). North America is too large for it on this machine and is not needed.

**Routing without a window:** step 3.4 added `omp-builder route <pack> <stop> <stop>… [--avoid tolls,ferries] [--pieces] [--geojson <file>]` (a stop is `lat,lon` or text to search for) and `route --check N [--pbf <extract>]`; 3.5 adds `--directions`. They are thin wrappers over `omp-core`, so routing is tested in a session without the app, as `search` and `here` are. Step 3.6 adds a route to `omp-app --render`, so the overlay is checked with snapshots.

---

## 3.1 Car profile and road graph

Build the car graph in the builder from OSM, as its own stage, and measure it. No CH yet.

> **Done 2026-10-27.** Done: `omp_core::profile` (the car profile, with a unit test per rule); the builder's `graph` module (three PBF passes, edges between intersections with signal, barrier and toll-booth delays and flags on the edges, blocking barriers cut, loop drives split at their middle node, ferries kept only where both ends meet a road, restricted roads charged on entry); turn restrictions by node splitting, done as an Aho–Corasick automaton over forbidden node paths, so via-node, via-way and overlapping restrictions share one rule (a test checks every pair on a hand-built grid against a search over paths); strongly connected components, nodes in components under 1,000 marked small. It runs as `build`'s stage 5 (cached under `--reuse-stages`, which is the temp file 3.2 reruns from) and alone as `omp-builder graph <pbf> [--json] [--save]`. Counts for Ohio, Quebec and North America are in the [size table](size-table.md#phase-3-step-31-the-car-graph). Restrictions add 0.7–2.1% of the nodes. North America alone: 188 s, peak 25.2 GiB. In `build --addresses all --bands 3`: stage 231 s, build peak 47.4 GiB (45.3 GiB before the stage; 47.2 GiB in R4), wall time 40.5 min. That leaves about 8 GiB under the cap for 3.2's contraction, which may have to free the graph's input first or spill.
>
> **OSRM** 6.0.0 runs in Podman (`ghcr.io/project-osrm/osrm-backend:v6.0.0`) on the Ohio and Quebec extracts in `$OMP_WORK/osrm/`, extracted, partitioned and customized for MLD, so `osrm-routed --algorithm mld` serves step 3.5 (Columbus to Cleveland: 230 km, 164 min). Extracted with [`tools/osrm/car-log.lua`](../tools/osrm/car-log.lua), which runs the stock `car.lua` unchanged and logs each way's verdict; [`tools/osrm/coverage.py`](../tools/osrm/coverage.py) compares that with `omp-builder graph --way-ids`. Every difference has a cause in the table below:
>
> | | Ohio | Quebec |
> |---|---:|---:|
> | Ways both keep | 1,313,012 | 530,732 |
> | OSRM only: `access=private` (it keeps them as start or end only) | 249,564 | 127,798 |
> | OSRM only: `footway`, `path`, `corridor`, `track`, … with an access tag that allows (`access=yes` on a footway is meant for walkers) | 1,006 | 1,158 |
> | OSRM only: ferries that meet no road | 5 | 30 |
> | Ours only: `width` ≤ 1.9 m, which OSRM reads as a size limit (mostly `width=0`–`1.5` on service roads, likely mistagged) | 53 | 21 |
> | Ours only: a ferry with `maxweight=5`, which OSRM reads as 5 kg | 0 | 1 |
>
> Left open for 3.5: the time comparison and tuning (speed factor, signal and barrier delays, ferry boarding).
>
> Commands, from `$OMP_WORK/osrm/` with the extract copied in: `podman run --rm -v $PWD:/data ghcr.io/project-osrm/osrm-backend:v6.0.0 osrm-extract -p /data/car-log.lua /data/ohio-261001.osm.pbf > ohio-extract.log`, then `osrm-partition` and `osrm-customize` on `/data/ohio-261001.osrm`.

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

### Profile differences from `car.lua`

`omp_core::profile` against `car.lua` and `lib/way_handlers.lua` of osrm-backend 6.0.0, rule by rule. Where nothing is listed, ours does what OSRM does: the access hierarchy (`motorcar`, `motor_vehicle`, `vehicle`, `access`, each with `:forward` and `:backward`), the default speeds by class, `maxspeed` (with `maxspeed:advisory` first) times 0.8 replacing the default, the `surface`, `tracktype` and `smoothness` caps, the closed-way tags (`area=yes`, `construction` but `no`/`widening`/`minor`, `proposed`, `oneway=reversible`, `impassable`, `status=impassable`, `service=emergency_access`), `maxheight` under 2.0 m and `maxwidth` up to 1.9 m, HOV-only ways as restricted, `toll=yes` per direction, the barrier rules for nodes (an access tag decides alone; the barrier whitelist; rising bollards and flat kerbs pass), the signal delay of 2 s with its direction, and restrictions (`restriction:motorcar`, `motor_vehicle`, `vehicle`, then `restriction`; `except`; conditional ones ignored; via nodes and via ways).

| Rule | OSRM | Ours | Why |
|---|---|---|---|
| Turn and U-turn costs | Sigmoid up to 7.5 s by angle, 20 s more for a U-turn | None | Decision 2; measured in 3.5 |
| Stop signs, give-way | 2 s and 1 s on turns, with a minor-road heuristic | None | They need turns to apply to the right approach; a node delay would charge the major road too |
| Signals | On the turn through the node | On each edge entering the node, in the signal's direction | No turns; a route pays once per signal either way |
| `access=private` | Kept as restricted (start or end only) | Dropped | The plan's rule; private roads are mostly driveways and gated land a route should not use |
| Restricted roads (`destination`, `delivery`, `customers`, HOV) | `max_turn_weight` on the turn into one from an open road | 3,600 s on each edge entering one from a node an open road touches; the time shown leaves it out | Same effect without turns: a route uses them only to start or end there |
| Classes without a speed (`track`, `road`, `path`, `footway`, `corridor`, …) with an access tag that allows cars | Routed at 10 km/h | Never routed | The plan's class list; `track` is mostly farm and forest roads |
| `motorway_link` without `oneway` | Two-way | One-way | The plan's rule |
| `oneway=reverse` | Not read | As `-1` | A common synonym |
| `oneway=alternating` | Weight × 0.4 | Not slowed | Rare; weight-only in OSRM |
| Slow service roads (`alley`, `parking`, `parking_aisle`, `driveway`, `drive-through`) | Weight × 0.5, duration unchanged | Speed × 0.5, so the time too | One metric; a parking aisle is driven slowly |
| Narrow roads (`width` ≤ 3 m, or one lane both ways) | Weight × 0.5 | Not slowed | Weight-only in OSRM; `width` is sparse in North America |
| `side_road=yes` | Weight × 0.8 | Not slowed | Weight-only |
| Zone `maxspeed` (`CA:urban`, `CA:ON:rural`) | European defaults: urban 50, rural 90, trunk 110, motorway 130, plus a table of countries | urban 50, rural 80 (Quebec 90), trunk 90, motorway 100, living street 10 | North American limits |
| `maxspeed=walk` | Not read | 5 km/h | |
| Ferries without `duration` | 5 km/h | 20 km/h | Car ferries run at 20–30 km/h; OSRM's 5 stands in for waiting |
| Ferry waiting and boarding | None (or in the 5 km/h) | 600 s per crossing, with or without `duration` | |
| Ferries that meet no road at one end | Kept (a pocket the snapping avoids) | Left out | The plan's rule; most are cut at the extract's edge |
| `route=shuttle_train`, `bridge=movable` | 10 km/h; 5 km/h | Not read | No car trains in North America OSM; movable bridges are rare |
| `maxweight`, `maxlength`; `width`, `est_width`, `narrow` for size | Below 2,000 kg or 4.8 m blocks; widths checked | Not read | Rare for cars, with messy units |
| Barriers that pass | No delay | `gate`, `lift_gate`, `sally_port`, `toll_booth` 10 s; others 0 (border crossings add none, decision 9) | The plan's rule |
| `access=private` (or `destination`) on a barrier node | Passes (restricted values are not blocks) | `private` blocks, `destination` passes | Consistent with dropping private ways |
| `barrier=toll_booth` | Not a toll | Marks the edges through it as toll | Toll booths on roads without `toll=yes` |
| Restriction members | | `from` and `to` ways must start or end at the via node (or at the via ways' ends); otherwise counted as broken | A way through the via node is ambiguous |

## 3.2 Contraction hierarchy

Contract the graph in the builder with our own CH (decision 1).

> **Done 2026-10-27.** Done: the builder's `ch` module. It contracts in parallel rounds of independent nodes, and the witness searches skip the whole round. A shortcut carries the union of its flags; a witness counts only if its flags are a subset. Parallel arcs are kept while neither dominates (shorter with fewer flags). `build` runs it as its own stage after the graph (cached under `--reuse-stages`). `omp-builder ch <graph|pbf> [--check N] [--fast-paths] [--no-flags] [--priority a,b,c] [--settled n] [--share f] [--json]` runs it alone on a graph saved by `graph --save`. Unit tests compare every pair and every avoid combination with Dijkstra on random grids with tolls and ferries. They also unpack every route to base edges, and cover a toll road beside a free road, equal paths contracted in one round, and pruned parallel edges.
>
> - **Flag overhead** (same parameters, with and without flags): Ohio 8,671,809 against 8,671,344 CH edges (+0.005%), Quebec 3,072,557 against 3,072,585 (noise). The flagged edges are too few to matter, far under the 10% line, so the avoid approach stands as planned (decision 3) and `RCHE` keeps the flags.
> - **Tuning on Ohio** (CH edges per base edge, mean nodes a query settles): weights 2,1,1 for edge difference, contracted neighbours and depth gave 1.75 and 770 when every local minimum was contracted each round. Allowing only the lowest-priority 1% of the remaining nodes per round gave 1.63 and 634 (5%: 1.65; 0.2%: no better). The weights matter less: 8,2,4 gives 1.60 and 634; 16,1,4 gives 1.56 but 773. Chosen: 8,2,4, 1%, witness searches up to 500 settled nodes (100 or 2,000 change little). `fast_paths` (sequential, lazy updates) gets 1.45 on Ohio and 1.51 on Quebec, so ours has about 10% more edges, the price of parallel rounds. Worth another look in 3.3 if size is tight.
> - **Check against Dijkstra and `fast_paths`:** 10,000 random pairs (outside small components) × 4 avoid combinations. Ohio and Quebec: **0 mismatches, 0 bad paths** (unpacked routes that do not chain, use an avoided flag or do not add up). `fast_paths` gives the same weight for all 10,000 pairs on both. North America: 200 pairs × 4, 0 mismatches. Quebec's 314 unreachable pairs are islands and other large components with no road link, and Dijkstra agrees.
> - **North America:** 288 s, peak 19.6 GiB alone (graph loaded from `graph --save`); 1.65 CH edges per base edge, about 3.5 GiB at 12 bytes per node and edge, against phase 0's 3.2 GiB estimate. In `build --addresses all --bands 3`: the stage takes 293 s, the same CH (contraction is deterministic), and the build peak stays at 47.7 GiB, set earlier by the graph read and the address merge, so contraction fits beside what is left. Wall time 46.0 min (3.1: 40.5). Log in `$OMP_WORK/measure/ch/`. Counts are in the [size table](size-table.md#phase-3-step-32-the-contraction-hierarchy).
>
> Left for 3.3 and 3.4: the query here (in `ch::check`) is a plain bidirectional search with no stall-on-demand, and it unpacks a shortcut by finding its two halves at the middle node. The longest CH edge is 75 h (North America), inside the planned 24-bit weights.

- **Node order:** by the usual priority (edge difference, contracted neighbours, depth; the weights to tune on Ohio), with lazy updates.
- **Parallel contraction:** contract an independent set of nodes in each round across threads, as in the CH literature, so North America takes minutes, not hours. Single-threaded `fast_paths` took 20.5 s for Ohio's 2.2M nodes; North America's 62.5M would be well over an hour on one thread.
- **Avoid flags in the hierarchy** (decision 3): a shortcut carries the union of its edges' flags, and a **witness path only removes a shortcut when its own flags are a subset of the shortcut's**. Then, for any set of avoided flags, the hierarchy without the flagged edges still holds every shortest path of the graph without them, so one CH answers all four combinations of tolls and ferries exactly (label-constrained contraction, Rice and Tsotras 2010). The cost is more shortcuts, because witnesses that use a toll road or ferry no longer count. Toll and ferry edges are rare (100k of North America's 38.8M routable ways have `toll`), so the overhead should be small. **Measure it on Ohio and Quebec first**, against the same CH without flags; if it is over about 10%, write it up for the owner before going on.
- **Memory and time** on North America: the contraction graph, witness searches and the order. Spill the base graph to disk if needed; record wall time and peak RSS. Shortcut middle nodes are kept for unpacking.
- **Check** against Dijkstra (and against `fast_paths` on the same graph without flags): 10,000 random pairs on Ohio and Quebec, every combination of avoid flags. Times must match exactly.

**Done when:** Ohio, Quebec and North America CHs are built, the flag overhead is measured and the avoid approach settled with the owner, the Dijkstra check passes, and the CH edge counts, contraction time and peak RSS are in the size table.

## 3.3 Routing format and core reader

Specify the routing sections in format.md (format 0.5), write them from `build`, and read them in `omp-core` with positioned reads (phase 1 decision 1).

> **Done 2026-10-27.** The sections are specified in [format.md](format.md#routing) (format 0.5) and written by the builder's `routing` module after the CH stage; `omp_core::route` reads them, with block caches (nodes 8 MiB, segments 4 MiB, the small sections 1 MiB each) counted in the app's F3 overlay (a `routing:` line; the app opens the reader off the UI thread and does nothing else with it until 3.6). The graph stage now keeps each stretch between graph nodes as a segment with its geometry (simplified to 2 m), class, roundabout flag and label, and the exit numbers; the street table hands routing a way-to-street map before it is freed, so segments carry `SSTR` street IDs (42.5M of North America's 80.2M segments; most of the rest are unnamed service roads).
>
> - **Sections, final list:** `RHDR` (JSON header), `RCHE` (the hierarchy), `RGEO` (segments: end nodes, class, street, label, length, both directions' time, flags and arrival node, points), `RNAM` (labels: ferry names and `destination`, `destination:ref`, `junction:ref`), `RGRD` (z14 cells to segments, each cell's own segments a run of IDs plus the ones crossing it), `RCPY` (restriction copies of each node and the segment directions they may leave by, for snapping a destination exactly), `RJCT` (exit numbers by node). The planned `RCHN` and a separate middle-node array were dropped (below). Large sections span chunks; blocked sections may now be spanning (`Writer::spanning_offsets`, `route::write_spanning`). No node positions or CH levels are stored: the query does not need them and segments carry the geometry.
> - **Layout, from page counts** (`omp-builder ch --layout N` and every `build` print it, from the builder's queries on 500 long random pairs and 500 short ones; pages of 4 KiB; "outside" = not in the top). Per long North America route: fixed-width records (8 B per node and per edge) with a middle-node array 3,485 pages, 1,936 outside the top; varint blocks of 32 nodes with the array apart 2,420 / 1,331; **varint blocks with each edge's middle node or segment inline 2,138 / 732**. Short routes: 84 / 59, 53 / 38, **44 / 23**. Ohio: 1,033 / 108, 593 / 99, **566 / 12**. Inline is also the smallest (North America `RCHE` 1,245 MiB, against 1,925 with the array apart and 3,378 fixed). Unpacking dominates long routes, and with the middle nodes inline it reads only the middle nodes' blocks. Block size: 16 nodes saves 4% of the pages outside the top but doubles the index; 64 adds 8%. Chosen: 32 nodes per block (North America index 8 MiB in RAM), and a top of about 32 MiB, sized from the encoded bytes (532k nodes on North America, which hold 98% of a long route's settled nodes).
> - **Cold and warm** (`inspect --readers`, dev machine, SATA SSD with each chunk's pages dropped from the file cache; no USB stick was at hand, so the USB 2 runs are left to 3.8): North America long routes 226 ms p50 cold (3.4 MiB of blocks read), 177 ms with the top read first; short routes 5.3 ms cold (0.1 MiB). Warm, with the plain query of this step (no stall-on-demand): long 17.7 ms p50 (the 8 MiB node cache is too small for a continental route's 2,231 settled nodes; 3.4 tunes it), short 0.04 ms. Ohio: long 0.25 ms warm, 86 ms cold; Quebec 0.11 ms and 34 ms. At about 1 ms per random read on a USB 2 stick, a long North America route's 2,138 pages would take some 2 s cold, or 0.7 s once the top (34 MiB, about 1 s sequential) is in the cache, so the app should read the top in the background when a pack opens (3.6).
> - **Checks:** `inspect --readers` on Delaware, Ohio, Quebec and North America: every base edge matches its segment direction (ends, weight, flags), every shortcut unpacks to its weight (a sample on North America), every segment is home in its first point's cell, every copy's directions start at its plain node, heads are the end node or its copy; on Ohio and Quebec 200 pairs × 4 avoid combinations through the reader against Dijkstra over its base edges: **0 mismatches, 0 bad paths**. A unit test writes the sections into a pack with 8 KiB chunks, reads them back and compares every route of a hand-built network (toll road, ferry, restriction, exit) with Dijkstra on the graph.
> - **Size:** North America 2,508 MiB (`RCHE` 1,245, `RGEO` 1,248, `RGRD` 10.9, `RCPY` 2.1, `RNAM` 1.4, `RJCT` 0.2), within criterion 7's 4.5 GiB; Ohio 86.7 MiB, Quebec 31.9. Details in the [size table](size-table.md#phase-3-step-33-the-routing-sections).
> - **Builder:** the graph stage alone peaks at 28.4 GiB on North America (25.2 before; the segments' geometry), the sections take 103 s, and `build --addresses all --bands 3` peaks at 48.1 GiB as before (set by the address merge), 48.2 min wall time.
>
> Left for 3.4: stall-on-demand and a larger node cache for continental routes; snapping (`RGRD`, `RCPY`, `head`); reading the top in the background.

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

> **Done 2026-10-27.** `omp_core::route` is now a module: `query` (the search) and `trip` (snapping, legs, trips); the builder gained `omp-builder route` and `route --check` (`crates/builder/src/route/`). Logs in `$OMP_WORK/measure/route34/`.
>
> - **Query:** a bidirectional search with stall-on-demand, from any number of start and end nodes, each with the weight already driven. A snapped stop starts from the far end of each direction of its segment (the direction's `head`, so a copy where a restriction made one) and ends at the near end or any copy of it whose `RCPY` leaves include the segment direction. A start and end on one segment also try the direct way along it. Stall-on-demand roughly halves the nodes settled: Ohio long routes 621 → 291, North America 2,231 → about 600.
> - **Snapping:** the nearest non-ferry segment in `RGRD`, trying radii from 50 m to 50 km. A stop's own street (the `SSTR` ID of a street or address hit) wins within 1 km. A road in a small component loses to one in the large component that is at most 500 m farther. `Snap` gives the distance; over `FAR_SNAP_M` (500 m) the app should warn.
> - **No route:** `leg_avoiding` drops avoid options from the fewest up and takes the quickest leg that breaks the fewest; `Leg::broken` says which ones it broke. When nothing works at all, the leg is `None` (for example the Magdalen Islands from Quebec City: their ferry lands in PEI, outside the extract). A trip's legs run in order through the same caches.
> - **Criterion 2, `route --check`:** random plain nodes outside small components, 10 targets per start, so one Dijkstra over the reader's base edges serves 10 pairs, for each of the 4 avoid combinations. Each path is checked: it must chain, add up and avoid what it should. With `--pbf`, the graph is rebuilt and each path's node sequence is checked against the restrictions' forbidden paths (`Graph::forbidden`, not saved by `graph --save`). Segments are mapped by the same reordering, and sampled first points are checked to agree. **Ohio:** 10,000 pairs × 4, 0 differ, 0 bad paths, 264,590 restricted approaches driven, 0 forbidden paths. **Quebec:** 10,000 × 4, 0 / 0, 439,961 approaches, 0 forbidden (258 pairs are unreachable without options: islands, as in 3.2). **North America:** 1,000 × 4, 0 / 0, 63,597 approaches, 0 forbidden; 702 s, peak RSS 28.4 GiB.
> - **Warm latency on the dev machine** (point to point, snapping and geometry included, each trip asked twice and the second timed). Short trips (1–100 km, about 70 km on average): Ohio 0.19 ms p50, 0.38 ms p95; Quebec 0.15 / 0.32; North America 0.17 / 0.43, and 3.0 / 7.8 ms the first time asked. Random long North America trips (2,271 km on average): 40 ms p50, 84 ms p95. Coast to coast: Miami–Seattle (5,318 km) 98 ms, Halifax–Los Angeles (5,829 km) 101 ms, Toronto–Vancouver 86 ms; Cincinnati–Cleveland 0.77 ms. These are well inside criterion 5's 50 and 500 ms. Search and unpacking take 1.7 ms of a coast-to-coast route. The rest is decoding its 1,063 `RGEO` blocks (about 20 µs of zstd and 50 µs of varints and one points allocation per segment each), which no cache size helps. If the E6430 or cold USB reads need it in 3.8, the place to start is a flat points array per block.
> - **Caches**, from these runs: the node cache went from 8 to **16 MiB** (8 took first-asked short North America trips from 2.5 to 8.6 ms; 32 and 64 MiB gain nothing), the segment cache from 4 to **16 MiB** (4 took a warm Cincinnati–Cleveland from 0.7 to 14 ms; 32 MiB gains nothing). After the checks the readers hold 33–41 MiB, all counted in the F3 overlay. The shared block cache now evicts down to 3/4 of its budget in one pass, instead of scanning every entry on every miss.
> - **Profile, for 3.5:** times look slow on highways. Cincinnati–Cleveland is 406 km in 4 h 53 min (83 km/h on average, mostly on I-71 with its 70 mph limit). This is the speed factor and motorway speeds of 3.1, which 3.5 tunes against OSRM.
>
> Left open: no cancellation of a running query yet (3.6's worker); side of the street for depart and arrive (3.5); criterion 1's address-pair coverage (3.8).

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

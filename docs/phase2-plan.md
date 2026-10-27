# Phase 2 Plan: Search and Addresses

*Draft 1, 2026-10-25. Expands Phase 2 of [roadmap.md](roadmap.md#phase-2-search-and-addresses). Builds on the address spike ([phase 0 step 0.4](phase0-plan.md#04-address-spike-ohio-first-then-ontario-and-quebec), [0.5](phase0-plan.md#05-full-north-america-run)), the address format in [format.md](format.md#addresses), and the pack and app from [phase 1](phase1-plan.md).*

## Purpose

Phase 2 makes the map searchable. When it is done, someone can type "123 Main St, Springfield, IL", "Tim Hortons Kingston", "Lac Saint-Jean" or "45201" into the app on a laptop with no network, see suggestions while typing, pick one and fly there. They can also click anywhere on the map and see what is there: the nearest address, the street, the town, county and state. Addresses that come from a TIGER or OSM interpolation range, or from an NAR blockface point, are marked "approximate".

Phase 0 showed the data is there (151.5M merged addresses in North America at 2.18 bytes each) and roughly what search costs (253 MiB or less). What it did not build:
- **The admin hierarchy.** Nothing yet says which city, county and state a street or address is in.
- **Street geometry.** Addresses are stored as offsets along a street, so turning one into a position needs the street's line. Phase 0 did not count that cost.
- **Random access.** `ALSE`, `ASUF` and `AUNI` are one zstd frame each in format.md, which works for a region but not for a continent (`ALSE` is 103 MiB in North America).
- **Ranking, query parsing and fuzzy matching**, which phase 0 left out of scope.
- **Address merging inside `build`.** `addr-spike` measures; nothing writes addresses into a pack.

Routing (phase 3) will want the street table and the spatial grid for snapping a click to a road, so keep them general.

### Exit criteria (expanded)

The roadmap's items are "search suggestions as you type, ranking by importance and distance from the map view, address lookup (with 'approximate' marking), and a 'what's here' lookup on click". In numbers, over the North America pack:

1. **Coverage.** The pack holds the merged address store: 151.5M addresses within 1% of phase 0.5's count (or more, if fuzzy attach in 2.4 recovers unattached points), 32M units, and the TIGER and OSM interpolation ranges. Every US state, Canadian province and Mexican state has places, streets and POIs in search.
2. **Right answers.** A fixed query set (step 2.6, kept in the repo) passes:
   - **random addresses** drawn from the merged store, queried as "number street, city, state": the top result is that address, or an approximate result within 100 m, in at least 97% of cases;
   - **hand-written queries** (places, POIs, abbreviations, French names, missing accents, one-letter typos, postcodes, units): the expected result is first in at least 90% and in the top 5 in at least 98%;
   - **"what's here"** at random address points gives that address (or one within 30 m on the same street) in at least 95%.
3. **Fast.** Suggestions update within **100 ms** p95 of a keystroke on the dev machine, and within **250 ms** on the Latitude E6430, warm. The first query after a cold start from the USB 2 stick returns within **1 s**. "What's here" answers within 200 ms warm and 1 s cold. Typing never drops frames: search runs off the UI thread.
4. **Memory.** Peak RSS stays under 300 MB on the dev machine after the bench flight plus a scripted set of searches (the 7490's 373 MiB on DX12 stays accepted, phase 1 decision 21). What search keeps in RAM is budgeted and counted in the F3 overlay.
5. **Start-up unchanged.** Start-up to first full frame stays under 3 s from USB 2, cold; search data loads in the background or on first use.
6. **Size.** Search and addresses add at most **1.5 GiB** to the North America pack (phase 0: 253 + 365 MiB, plus the street geometry and admin polygons that were not counted). The app stays under 30 MB.
7. **Builder.** `omp-builder build` writes tiles, search and addresses in one run for North America on the dev machine, under the 56 GB cap used in phase 0, with wall time and peak RSS recorded.

## Status

| Step | Status | Notes |
|---|---|---|
| 2.1 Admin hierarchy in the builder | **Done** 2026-10-25 | `SADM` + `SAPL` in `build`; North America 54,937 areas, 14.3 MiB. Postal town for 70.5% of streets; fallback order changed (decision 4, revised and approved). |
| 2.2 Street table and street geometry | **Done** 2026-10-25 | `SSTR` + `SGEO` in `build`; North America 10.0M streets, 332 MiB (geometry 241 MiB). 99.6% of sampled addresses within 5 m of the spike's point (the rest are addresses far from a curving street). |
| 2.3 Search entries and index | **Done** 2026-10-25 | `SENT` + `STWN` + `SFST` + `SPST` in `build`; North America 12.55M entries, 220.5 MiB. Postcodes per street with exceptions: 7.6 MiB, so they will be shown (decision 5); stored in 2.4. |
| 2.4 Address store in `build` | **Done** 2026-10-26 | `build --addresses`: `ASTR` `ASUF` `AUNI` `ALSE` `ARNG` `APCD`; North America 151.2M addresses (−0.17%), 89.9% attached (was 88.9%) with alias and fuzzy attach, 392 MiB. Build 51 min, 49.5 GiB. |
| 2.5 Spatial grid and core readers | **Done** 2026-10-26 | Readers in `omp-core` (`search`, `addr`, `street`, `admin`, `grid`); `SGRD` at z14 (North America 7.8 MiB); format 0.4 specifies every search and address section. Ohio and Quebec read and cross-checked by `inspect --readers`; lookups under 1 ms p95 warm. |
| 2.6 Query parsing, matching and ranking (`omp-core`) | **Done** 2026-10-26 | `omp_core::find` + `query` + `fuzzy` + `here`; `omp-builder search` / `here`. Query set: 1,000 random addresses (Ohio, Quebec, Ontario) and 241 hand-written queries. Ohio / Quebec: addresses 99.4 / 97.3% first, hand-written 98.9 / 98.7% first and 100% top 5, "what's here" 100%; warm p95 17.0 / 15.3 ms. No format change. |
| 2.7 Search UI | **Done** 2026-10-26 | Search box, suggestions, results panel, fly-to and marker (`app/src/search.rs`); queries on one worker, stale answers dropped. North America rebuilt to format 0.4 (53 min, 49.4 GiB). Bench keystroke latency p95 53 ms Ohio, 89 ms North America, warm; RSS peak 193 / 239 MiB. The on-map credit is now OpenStreetMap's only. |
| 2.7b Progressive search | **Done** 2026-10-26 | `find::Session`, 4 levels sharing one run; the worker goes deeper after a 150 ms pause, cancelled by the next keystroke. North America, town cut to 4 letters: 71.3 → **89.3%** first (94.0% in the 8 suggestions), shown within 1.74 s of the pause; level 0 unchanged. Enter after a pause ≤ 8 ms. |
| 2.8 "What's here" | **Done** 2026-10-26 | Right-click menu ("What's here?", coordinates to copy) and a panel at the point (`app/src/here.rs`), answered by the search worker through `Search::whats_here`, shared with `omp-builder here`. At the address points: North America 99.7%, Ohio / Quebec / Ontario 100 / 100 / 99.7%. Bench: 1.9–9.4 ms from the request to the panel, warm. |
| 2.9 Performance, accuracy and hardware checks | Not started | |

## Environment notes

As in [phase 1](phase1-plan.md#environment-notes): builder data in `$OMP_WORK`, `target/` on `/`, packs in `$OMP_WORK/packs/`. The address inputs from phase 0.5 are in `$OMP_WORK/inputs/{openaddresses,tiger,statcan}`; their fetch is from 2026-10, so they need no refetch unless a source is broken.

**Test regions:** Delaware for fast loops, **Ohio** (US: OpenAddresses, TIGER, many unincorporated townships) and **Quebec** (French names, NAR, accents) for every builder change, then North America. Ontario is checked once in 2.4 for NAR blockface points and Canadian unit formats.

**Memory:** run North America builds under `systemd-run --user --scope -p MemoryMax=56G -p MemorySwapMax=0`, as in phase 0.

**Search without a window:** step 2.6 adds `omp-builder search <pack> "<query>" [--near lat,lon]` and `omp-builder here <pack> <lat,lon>`, thin wrappers over `omp-core`, so the search code is tested and reviewed in a session without the app, the same way `--render` is for the renderer.

---

## 2.1 Admin hierarchy in the builder

Every search entry and every address needs "where": the town, county, state or province, and country. OSM has this as `boundary=administrative` relations, which the `boundaries` layer already reads for lines.

- **Assemble admin areas** as polygons from relations at admin levels 2 (country), 4 (state, province), 6 (county, census division or MRC), 7 (US civil township; added in 2.1, Ohio has 1,311), 8 (city, town, township, municipality) and, where they exist, 3, 5 (Quebec's regions) and 9–10 (boroughs, neighbourhoods; display only). Reuse the ring assembly from `measure/geom.rs`. Keep `name`, `admin_level`, `ISO3166-2` where present (to get `OH`, `QC`, `ON` for display and queries), `population` and the `label` or `admin_centre` member.
- **The town shown is the postal town** (decision 4), the one written on an envelope. Admin areas often differ from it: much of the US is unincorporated, a Butler County, Ohio address may lie in a township that nobody writes, and 14% of NAR addresses have a mailing municipality that is not their legal one. The address sources carry the postal town: NAR `MAIL_MUN_NAME` (essentially 100%), OpenAddresses `city` (82% of Ohio's records, 91–100% in the large county sources, but often blank in rural ones: Adams 74% blank, Stark 55%) and OSM `addr:city`. TIGER has none.
  - Each street record (2.2) takes the postal town most of its addresses use.
  - Streets with no postal town (no address points, or only blank ones), places and POIs fall back to the admin 8 name when it is a city, town or village, otherwise the nearest `place=city|town|village|hamlet` node's name.
  - The admin 8 name is indexed as an alias, so "Liberty Township" and "Hamilton" both find a street that is shown as Hamilton.
  - Measure on Ohio and Quebec, and per state on North America: the share of streets that get a postal town, and how often the fallback names a different town than the street's neighbours.
- **Canada:** admin 8 is the municipality (Quebec's municipalities, Ontario's lower-tier municipalities), admin 6 the county, MRC or regional municipality. Check that Montréal's boroughs (admin 9–10) do not hide the city.
- **Assign** each place, POI and street (2.2) its chain of admin area IDs by point in polygon, on the builder's grid. Use one point per street (its middle) unless the street crosses a city boundary (2.2 splits those).
- **Store** for the app: the admin table (names, levels, abbreviations, parent, label point), and polygons simplified to about 50 m for "what's here" and for query filters ("Springfield, IL" means inside Illinois). Measure the size on North America; at about 50k areas this should be tens of MiB.

**Done when:** Ohio and Quebec have an admin table and polygons, every street and place has a chain, the Ohio unincorporated numbers are recorded, and the North America size is measured.

> **Done 2026-10-25** (the commit that adds `crates/builder/src/admin/`).
> - **Code.** `admin/read.rs` reads `boundary=administrative` relations (levels 2–10), their member ways and nodes, label and admin-centre nodes, and named `place=city|town|village|hamlet` nodes, in three PBF passes of its own. `admin/index.rs` is a z12 grid for point in polygon (boundary cells keep their edges and their centre's side; inside cells are whole), tested against a brute-force test. `admin/mod.rs` orders areas by level and Morton code, finds each one's parent and label point, and flags level 8 **towns** (not townships, by `border_type`, `place` and name). `admin/encode.rs` writes `SADM` and `SAPL` (format.md, *provisional*), with a decode test. `build` writes both sections and gives every place and POI its chain (coverage printed). `omp-builder admin <pbf> --json … [--at lat,lon]` reports counts and sizes and prints chains. `addr-spike` reads each source's town (OpenAddresses `city`, NAR `MAIL_MUN_NAME`, ODA `city`, OSM `addr:city`) and reports the town per street (`addr/towns.rs`, `towns` in its JSON, samples in `towns.tsv`).
> - **Size** ([size-table.md](size-table.md#phase-2-step-21-admin-areas)): North America 54,937 areas (32,881 at level 8), `SADM` 1.74 MiB + `SAPL` 12.52 MiB = **14.3 MiB**, within the "tens of MiB" estimate. Ohio 0.37 MiB, Quebec 0.97 MiB. Reading takes 2 min and 1.7 GiB on North America.
> - **Chains.** Ohio and Quebec packs (`$OMP_WORK/measure/admin/`): every street in the state or province has a chain (`without_chain` 0). In Ohio's pack, 42 of 6,747 places and 119 of 86,136 POIs are in no area, presumably at the extract's edge beyond the state, where it has no boundaries (not checked one by one). Montréal and Québec keep level 8 under their boroughs (levels 9–10); no Quebec street is in a borough without its city. Regional extracts have no country (the level 2 relation is cut off), so `parent` stops at the state there; the North America pack has all 7 countries.
> - **The town per street** (`addr-spike`, `$OMP_WORK/measure/towns/`, North America table in `na-table.md`). A street takes the postal town most of its address records give, after matching the source's spelling to an admin area or nearby place ("BRUNSWICK CITY" and "Brunswick" are one town; "SAINT-THÉOPHILE" shows as "Saint-Théophile").
>
>   | | Ohio | Quebec | North America |
>   |---|---:|---:|---:|
>   | Streets | 223,218 | 124,805 | 8,071,213 |
>   | Postal town from addresses | 64.0% | 90.4% | 70.5% |
>   | Fallback: neighbours / area / place / none | 23.6 / 0.5 / 12.0 / 0% | 7.1 / 2.3 / 0.1 / 0.1% | 19.7 / 1.4 / 8.3 / 0.1% |
>   | Hold-out: fallback order names the postal town | 95.9% | 98.2% | 93.8% |
>   | Hold-out: the plan's order (area, then nearest place) | 70.8% | 96.8% | 59.4% |
>   | Postal town ≠ the town area it lies in | 9.6% | 2.5% | 11.8% |
>   | Streets / addresses in no city, town or village area | 42.1% / 37.2% | 1.2% / 0.7% | 48.4% / 41.6% |
>
>   **Ohio's unincorporated numbers:** 94,037 of 223,218 streets (42.1%) and 1,842,916 of 4,946,534 addresses (37.2%) are in no city or village, only a township (level 7) or county. 37,068 of those streets have a postal town from their addresses. Where a street lies in a town area, its addresses name another town 9.6% of the time, mostly real postal towns (Ottawa Hills streets are "Toledo", Brooklyn's are "Cleveland").
> - **The fallback order changed** (decision 4, revised; approved by the owner). The hold-out test hides the postal town of each street that has one and asks each fallback for it. The plan's order (the admin 8 town, else the nearest place) gets 59.4% right across North America: nearest place nodes are often hamlets and subdivisions ("Young Subdivision") rather than the postal town (39.6% right in Ohio). The order now used is: the postal town most streets within about 1 km give, **counting only streets in the same municipality or township**; then the admin 8 town; then the same neighbours anywhere; then the nearest city, town or village; then the nearest place of any kind. It gets 93.8% right. Neighbours anywhere first was better in Ohio (91.5%) but worse in Quebec (94.7%); the area first, the reverse (89.0% and 97.3%).
> - **Build cost:** `addr-spike --each` with towns took 23.4 min and 34 GiB on North America (phase 0: 18.7 min, 28.7 GiB); the admin read is 2 min of that.
> - **Left open:**
>   - The **town flag** is wrong in a few states, where the postal town differs from the "town" area for most streets: New York (98%; NY's towns are level 7 and its level 8 areas are not the postal towns), Wisconsin (59%, "Town of …" at level 8), New Brunswick (100%), Alberta (85%), Manitoba (77%), Yukon and Hawaii. Area-based fallbacks there are rare (under 3% of streets except Wisconsin's 17%), but "what's here" (2.8) shows the area too. Fix per state in 2.2 or 2.8, checking the `towns.tsv` samples.
>   - **Puerto Rico and the Virgin Islands** got no streets: their level 4 areas do not carry `US-PR` / `US-VI`. Not in the address sources' scope either.
>   - 8.3% of North America's streets still fall back to a place node (12.0% in Ohio), rural streets with no addressed neighbour within 1 km; how often that is right is not measured (the hold-out only covers streets that have neighbours).
>   - The North America pack was not rebuilt: its size is from `omp-builder admin`, which runs the same encoder as `build`.

## 2.2 Street table and street geometry

Addresses refer to streets by street ID (format.md). Search shows streets as results. Both need one shared table, and positions along a street need the street's line.

- **Street records**, as in `addr/streets.rs`: connected named road ways with the same normalized key. Two changes:
  - **split where the postal town changes** along the street (the town of its addresses, 2.1), or at a city boundary (admin 8) where it has no addresses, so each street has one town and "Main St" through four towns gives four results;
  - **split very long streets** at a length cap (to tune; start at 20 km), so a 200 km rural route is not one record whose offsets overflow or whose middle point names the wrong county.
  Keep the `ref` (from `routes`) with the street, so "SR 4" and "Route 138" match roads by number, as in phase 0's normalization.
- **Geometry:** one polyline per street record, simplified to about 2 m (addresses are quantized to 5 m), in tile-grid units, delta-coded and zstd-compressed in blocks of streets. Offsets are measured along this line, from its first point; this replaces the spike's offsets so both sides agree exactly. A street made of a branching set of ways (a loop with spurs) is stored as several parts with offsets running on across them.
- **Measure** street count, parts and geometry bytes on Ohio, Quebec and North America. Phase 0's search spike counted about 8M streets (name and z12 tile); at 30–60 bytes each the geometry would be 250–500 MiB, which is the biggest unknown in the size budget. If it is much more, the fallbacks are coarser simplification (5 m), or reading road lines from the z14 tiles at query time (fragile: tiles are clipped and merged).
- **Format:** street table and geometry sections (proposed `SSTR` and `SGEO`) in format.md, replacing the provisional note under Search.

**Done when:** Ohio and Quebec packs carry the street table and geometry, a test resolves every address in a sample to a position within 5 m of where the spike put it, and the North America geometry size is in the size table.

> **Done 2026-10-25** (the commit that adds `crates/builder/src/street/`).
> - **Code.** `street/mod.rs` builds the table from the named highway ways (`addr/osm.rs`, which now also keeps each way's `highway` class and which of its names are refs, and has a streets-only read for `build`). Ways are cut into **pieces** where they enter another admin 8 area and every 5 km. Each piece takes the postal town most of its address votes give, which spreads to touching pieces of the same name in the same admin 8 area that have none; the other pieces are labelled by their admin 8 area. A street is a run of touching pieces with the same key and label, which stops growing at 20 km. Its pieces are chained into parts (from the westmost free end, straight on at each node), quantized, simplified to 2 m, and placed on a Morton curve of the street's middle. Streets with no postal town use the decision 4 fallbacks, now shared with `addr/towns.rs` (`admin::towns::Fallbacks`). `Table::locate(way, point)` gives the street, offset, distance and side for an address, for step 2.4. `street/encode.rs` writes and decodes `SSTR` and `SGEO` ([format.md](format.md#streets), *provisional*). `build` writes both sections. `omp-builder streets <pbf>` reports counts and sizes. `addr-spike` builds the table with its postal town votes and runs the check below (`streets` in its JSON, worst cases in `streets-check.tsv`). Unit tests cover the town and length splits, the parts, locating points and the section round trip.
> - **Part offsets.** Offsets run on from one part to the next, but each part starts on a fresh 5 m step past the end of the previous one, so an offset rounded to 5 m belongs to one part only. The spike laid ways end to end with no gap, so an offset at the end of one way was also the start of the next. That put 0.16% of Ohio's sampled addresses (0.19% of Quebec's) more than 5 m from the spike's own point, up to 16.6 km away: it is a flaw in phase 0's A3 offsets, which the table removes.
> - **Check** (200,000 merged addresses per region; each placed on the table by its way and position, decoded from the encoded sections, and compared with the spike's foot point on its way):
>
>   | | Ohio | Quebec |
>   |---|---:|---:|
>   | Within 5 m, exact offsets | 99.58% | 99.64% |
>   | p50 / p99 / max (m) | 0.40 / 3.52 / 144 | 0.35 / 3.37 / 90 |
>   | Within 5 m, both offsets rounded to 5 m | 98.59% | 98.84% |
>   | Same side of the street | 99.52% | 99.45% |
>
>   Not every address in the sample is within 5 m, so this falls short of the **Done when** line. Of the 0.4% more than 5 m off, 87% (Ohio) and 78% (Quebec) are more than 25 m from their street, at a cul-de-sac or a curve, where the nearest point on the line moves a long way when the line is simplified by 2 m. Most of the rest are nearer the street on the table than in the spike, because the table also projects onto the start of the next way. The decoded position is a point on the street either way, since A3 stores no distance from it. Accepted by the owner (2026-10-25), as is the new part-offset rule.
> - **Sizes** ([size-table.md](size-table.md#phase-2-step-22-streets)): North America 10.0M streets (0.51M with several parts, at most 54), 90.8M stored points; `SSTR` 90.9 MiB + `SGEO` 240.7 MiB = **332 MiB**, about 34 bytes per street. The geometry is below the 250–500 MiB estimate, so no fallback is needed. Ohio's pack has 253,216 streets (7.7 MiB), Quebec's 136,176 (5.8 MiB).
> - **Postal town splits** (`addr-spike`): Ohio 250,462 streets, 151,491 of them named by their addresses' postal town; Quebec 135,367, 117,244 by postal town. Without addresses (as `build` is until 2.4): 253,216 and 136,176.
> - **Build cost:** `omp-builder streets` on North America took 6.4 min at 20 GiB peak (3 min to read the ways, 39 s to build the table). `build` takes about 4–6 s more on Ohio and Quebec.
> - **Left open:**
>   - `build` has no addresses yet, so its streets are split and named by area only. Step 2.4 passes the address votes in (the `addr-spike` path already does).
>   - A way can only be cut at a vertex, so a long segment with no vertices passes the caps: North America's longest street is 79 km.
>   - Streets crossing the edge of a regional extract are cut there. Each pack is built on its own, so this does not matter.
>   - The per-state town flag fix left open in 2.1 (New York, Wisconsin, New Brunswick, Alberta, Manitoba, Yukon, Hawaii) was not done here; it moves to 2.8, where "what's here" shows the area.

## 2.3 Search entries and index

Build the search index for places, admin areas, streets, POIs and postcodes.

- **Entries** (`SENT`): kind, class, name, position, admin chain, importance, and a reference to the street record or admin area. Stored in blocks sorted by entry ID, read with positioned reads; a small block index stays in RAM.
- **Names indexed** (decision 6): `name`, plus `name:en`, `name:fr`, `alt_name`, `short_name`, `official_name`, `old_name` and, for POIs, `brand` as aliases, so "Mexico City" finds Ciudad de México and "Montreal" finds Montréal (accents are folded anyway). Results are shown under `name`, as on the map (phase 1 decision 12).
- **POIs:** the POIs in the tiles' whitelist, the 11 classes on the map (0.8M in North America in phase 0), so every POI search result also shows on the map. A larger POI set, for both the map and search, is left for later (decision 7).
- **Postcodes** (decision 5): US ZIP codes and Canadian postal codes as entries at the centroid of the addresses that carry them (from OpenAddresses, NAR and OSM `addr:postcode`). Picking one flies to that point at a zoom that fits the spread of its addresses; there is no outline, since the sources give no postcode boundaries. This costs no bytes per address. Showing a postcode in each address result and in "what's here" needs one per street record (with exceptions where a street crosses a ZIP boundary) or per address: measure the per-street option and add it if it is small.
- **Importance** for ranking, one byte per entry: places by class and population, admin areas by level, streets by road class and length, POIs by class (airports and hospitals above parking lots). Kept simple and tuned in 2.6.
- **Tokens:** `omp_core::normalize::tokens` and the street normalization from phase 0, so "St" = "Street", "N" = "North", "21st" = "21", French types and articles, accents folded. Both forms of an abbreviation map to one token.
- **Index:** an `fst` map from token to postings (`SFST`, `SPST`). In phase 0 the North America `fst` was about 2 MB per band (5 MB in all), so the app holds it in RAM. Postings are delta-coded entry IDs ordered by importance blocks, so the top candidates for a common token ("Main", "Rue") are read first without decoding the whole list. Measure the long postings lists ("Street", "Road") and leave out of postings any token that would match millions of entries without saying anything, keeping it only for exact phrase checks.

**Done when:** Ohio, Quebec and North America packs have search sections, their sizes per part are in the size table, and the per-street postcode cost is measured and decided.

> **Done 2026-10-25** (the commit that adds `crates/builder/src/search/`).
> - **Code.** `search/mod.rs` builds the entries: one per street record (at its middle, with its town, alternate names and refs), admin area, place, POI, `labels` feature (lakes, rivers, parks; so "Lac Saint-Jean" is found) and postcode. A place node inside an admin area of a matching level with the same name key stands for it (one entry, referring to the area). Alternative names and `brand` come from one more PBF pass over the features' IDs (1 min on North America). POIs, minor places and postcodes get their town from the decision 4 fallbacks. Importance is one byte per kind (`*_importance`). Entry IDs run by importance bucket, then Morton order, so postings in ID order list the important candidates first. `search/encode.rs` writes and decodes the four sections ([format.md](format.md#search), *provisional*). Tokens are `omp_core::normalize::search_tokens` (the street key's words, so "Mt"/"Mount", "OH 161"/"State Route 161" agree; possessives joined, French elisions split), shared with the query side; `normalize::postcode` parses US and Canadian codes. `inspect --search "<words>"` lists the entries posted to each token. Unit tests cover the ID order, the round trip through all four sections, generic-token stopping and importance.
> - **Postcodes.** `build` has no addresses until 2.4, so `addr-spike` now reads OpenAddresses `postcode`, NAR `MAIL_POSTAL_CODE`, ODA `postal_code` and OSM `addr:postcode`, gives each merged address the postcode of any record in its cluster, and writes `<samples>/postcodes.tsv` (median point, radius holding 90% of addresses), which `build --postcodes` reads, merging codes that cross a state line and dropping rows over 50 km from the largest. Step 2.4 moves this into `build`.
> - **Sizes** ([size-table.md](size-table.md#phase-2-step-23-search)): Ohio 5.8 MiB, Quebec 7.5 MiB, North America **220.5 MiB** (`SENT` 173.9, `STWN` 0.2, `SFST` 4.7, `SPST` 41.7; 18.4 bytes per entry), against phase 0's estimate of 253. The `fst` is 4.7 MiB, as phase 0 found, and stays in RAM (decision 1). 63.6 MiB of `SENT` are street names also in `SSTR`.
> - **Long postings lists:** "road" 1.81M entries, "street" 1.59M, "drive" 1.23M, "avenue" 0.92M, "calle" 0.75M. Generic tokens (street types, directions, articles) in more than 100,000 entries are left out of postings except for the entries made only of such tokens ("Avenue Road"): 19 tokens in North America, none in Ohio or Quebec.
> - **Per-street postcode cost, decided** (decision 5): the street's commonest postcode is right for 93.5% of attached addresses (US 97.5%, Canada 57.1%). One per street with exceptions is 7.6 MiB for North America, one per address 8.9 MiB; both are small, so postcodes are shown with addresses and in "what's here", stored per street with exceptions in the address sections (step 2.4).
> - **Build cost:** North America `build` 34.2 min at 47.2 GiB peak (was 24.3 min, 43.1 GiB without the street table and search), `addr-spike --each` 28.5 min at 39.0 GiB; Ohio 24 s, Quebec 61 s. `verify` passes on all three packs.
> - **Left open:**
>   - Street names are in both `SENT` and `SSTR` (63.6 MiB); whether street entries should drop theirs is for 2.6, with the reader.
>   - Spanish street words ("calle", "privada", "avenida") are not generic, so Mexico's "calle" keeps a 0.75M list (decision 3 keeps Spanish out of scope; revisit in 2.6 if it is slow).
>   - The admin 8 name is not posted as an alias of streets (decision 4); entries carry their admin area, so 2.6 filters "Liberty Township" through the chain instead.
>   - Streets are still split by area only in `build`, and their entries are often several per road (carriageways, town splits: "Wilmington Expressway" is 7); grouping them in results is for 2.6.
>   - Waterway lines (rivers drawn as lines) are not entries; only named water and park areas from the `labels` layer are.

## 2.4 Address store in `build`

Move the address merge from `addr-spike` into `build`, writing the A3 sections keyed to the street table from 2.2.

- **Pipeline:** the same sources and rules as phase 0 (OSM, OpenAddresses, NAR building and blockface points, TIGER ranges; attach within 200 m, dedup at 250 m, OSM wins; decisions 9–13), run per state and province as `--each` does, after the tile bands so their memory is free. `--exclude-license` stays.
- **Better attach.** 11% of North America's addresses matched no street (103 MiB in `ALSE`, a third of the address bytes). Phase 0 found the causes: streets missing from OSM, spelling differences (Dixon/Dixion), and prefixes like "Oro-Medonte Line 4". Add fuzzy matching (edit distance 1 on a token of 5+ letters, only when there is one candidate within the distance), and matching through the postal city alias. Measure the attach rate and the false matches on the phase 0 samples.
- **Format changes for random access:** `ALSE` is split into blocks by spatial cell (for "what's here") with names per block, and `ASUF` and `AUNI` into blocks aligned with `ASTR` blocks, as format.md already allows. Check whether 4 KiB `ASTR` blocks are worth their 3.6% for faster cold lookups on USB.
- **Postcodes** (from 2.3): compute the postcode points in `build` instead of reading `addr-spike`'s `postcodes.tsv`, and store one postcode per street with exceptions (7.6 MiB for North America, decision 5) in the address sections.
- **Unattached addresses in search:** an `ALSE` point has its own street name. Index those names too, so a street missing from OSM can still be found by its addresses.
- **Attribution:** the pack's `META` lists every address source used, with its license and attribution text, from the fetch manifest (roadmap §3 and §8). The app's About window (phase 1 step 1.9) shows them.
- **Build cost:** record wall time and peak RSS for Ohio, Quebec and North America. Phase 0's `addr-spike --each` took 18.7 min and 30.8 GB, and `build` peaks at 43 GB, so they must run one after another.

**Done when:** `build` writes the address sections for Ohio, Quebec and North America; the address counts match phase 0.5 within 1% (or explain the change from fuzzy attach); the attach rate is recorded; and format.md is updated.

> **Done 2026-10-26** (the commit that adds `crates/builder/src/addr/{attach,merge,store}.rs`).
> - **Code.** `addr-spike`'s merge is split into stages (`select`, `extract`, `normalize`, `attach_records`, `dedup`, `clusters`, `fallback_ranges` in `addr/mod.rs`) that the spike and `build` share. `addr/merge.rs` runs them per state and province, as `--each` does, against one list of street ways (`osm::split_indexed` gives each subdivision the indexes of the ways within about 5 km), and keeps each merged address's way, its postcode, units and the postal town votes. `build --addresses US-OH` (or `all`; `--address-sources`, `--exclude-license`) now reads the addresses after the tile bands, builds the street table with the votes (streets split by postal town, as `addr-spike` did in 2.2), places the addresses on it (`Table::locate`, `Table::project_onto` for range ends) and writes six sections (`addr/store.rs`). It computes the postcode points itself (`postcodes::combine` merges a code across state lines as `--postcodes` did; that option is gone). Each pack is checked as it is written: every section is decoded and counted. Unit tests: attach matching, section round trip with tiny blocks, natural unit order.
> - **Better attach** (`addr/attach.rs`), after the exact and loose keys fail at 200 m: **alias**, a town's name dropped from the start or end of the street name (the record's postal town or an admin area of level 5 and up holding it: "ASHLAND CO RD 500", "Sideroad 20 Arran", "County Road 19 West Norfolk"), when what is dropped and what is left each have a telling word; then **fuzzy**, a street within 200 m whose key differs only in spaces, or by one edit (insertion, deletion, substitution, swap of neighbours) in one word of 5+ letters, when exactly one such key is near. Of about 350 sampled alias and fuzzy attaches reviewed (`fuzzy-attached.tsv` in Ohio, Quebec and Ontario), 3 are doubtful (Cotton Ct → Colton Court, Loveland–Miamiville Road → Paxton Guinea Road, Oxford Road 33 → Road 78) and none clearly wrong; the rest are spelling variants of the same street (Perrault/Perreault, Rvierside, Dixion) or county route prefixes.
> - **Attach rate** (merged addresses on a street, against the 2.3 rerun of `addr-spike` on the same inputs): Ohio 94.25 → **95.28%**, Quebec 96.14 → **96.37%**, Ontario 94.0 → **94.72%**, North America 88.9 → **89.89%** (US 88.6 → 89.55, Canada 92.2 → 92.78). North America's 327.0M records: 277.0M by the exact key, 22.6M loose, 0.13M alias, 2.29M fuzzy, 25.0M unattached.
> - **Counts:** North America **151,245,121** addresses, 0.17% below phase 0.5's 151,500,852 (US −0.16%, Canada −0.23%): a record that now attaches dedups with the copy already on its street. The drop is largest where attach was worst: Prince Edward Island −3.0% (attach 70 → 76%), Mississippi −2.1%, Wyoming −1.9%. Ohio 4,941,837 (the spike's count less the 27 OSM points outside `US-OH.poly`), Quebec 4,232,265, Ontario 5,508,813 (274,283 NAR blockface points, flagged approximate). Units 32.0M; 10.58M TIGER and OSM ranges kept of 38.3M.
> - **Format** ([format.md](format.md#addresses), *provisional* until 2.5): every address section is zstd blocks behind an index (`count nblocks (first offset length)*`, as `SSTR`); `AIDX` is gone. `ASTR` runs carry the street's postcode; `ASUF` (suffixes and postcode exceptions) and `AUNI` have one block per `ASTR` block, by ordinal; `ALSE` holds whole z14 cells per block, with a name table per block and a postcode per point; `ARNG` is blocked by street; `APCD` lists the postcodes. Postcodes are one per street with exceptions (decision 5): 895,148 codes, 7.86M exceptions in North America.
> - **Block size: 16 KiB.** North America's address sections are 392.2 MiB at 16 KiB, 412.0 at 4 KiB (+5.1%) and 386.7 at 32 KiB. A 16 KiB `ASTR` block is about 10 KiB compressed, which the USB 2 stick (36 MiB/s, 1.2 ms p95 per read, step 1.10) reads in about 0.3 ms more than a 4 KiB one, and decodes (with its `ASUF` and `AUNI` blocks) in 0.14–0.3 ms. Neither matters against the 1 s cold target, so 4 KiB blocks are not worth their 5%.
> - **Unattached street names in search:** a new entry kind (6) for street names that only unattached addresses give, with no OSM way of the same key within about 1 km of them: one per street key and deepest admin area, at the median address, shown with the postal town most give. North America 1.95M entries covering 12.2M of the 15.3M unattached addresses, 55 MiB of `SENT` before compression.
> - **Attribution:** `META` lists every address file read, from the manifest (North America 5,418 sources, 246 KiB; Ohio 182). The About window shows one row per kind of source with its file count per license, and the attribution lines in a scroll area.
> - **Sizes** ([size-table.md](size-table.md#phase-2-step-24-addresses)): North America addresses **392.2 MiB** (2.34 B per address, ranges apart), against phase 0's 315 + 50; search 249.8 MiB (was 220.5); the pack 7,202.6 MiB in 2 chunks.
> - **Build cost:** North America `build --addresses all` **51.0 min at 49.5 GiB** (step 2.3: 34.2 min, 47.2 GiB). The bands peak at 44.5 GiB, now that the street table waits until after them; then the OSM address read takes 2.6 min (45.7 GiB) and the merge 14.0 min (49.5 GiB), the street table 2.3 min and the address sections 49 s. A first run that kept the street table and the attached records while encoding peaked at 54.0 GiB, too close to the 56 GB cap; both are now freed once the addresses are placed. Ohio 37 s at 5.0 GiB (was 24 s), Quebec 90 s at 9.0 GiB (was 61 s), Ontario 78 s at 8.8 GiB. `verify` passes on all four packs.
> - **Determinism:** the OSM address read now sorts its points on every field and its interpolation ways by ID (blocks arrive in any order), after two North America runs differed by 2 ranges and 15 postcode exceptions. Two `--deterministic` Ohio builds give identical sections. The North America numbers above are from the run before this fix.
> - **Left open:**
>   - Unit and `ALSE` street names are stored as the sources give them ("BSMT", "APUP", "THORNBIRD CRES"); the reader (2.5, 2.6) formats them. Address-only street entries prefer a spelling in mixed case.
>   - Whether 1.95M address-only street entries are worth their place in search (some are streets OSM has under a quite different name) is for 2.6 to judge with the query set.
>   - `ALSE` is 106.8 MiB, more than phase 0's 103.5 with 1.5M fewer points: names are repeated per block, and each point has a postcode.

## 2.5 Spatial grid and core readers

The readers the app and the CLI share, in `omp-core`, and the grid for lookups by position.

- **Spatial grid** (`SGRD`): for each cell (z13 or z14, to measure), the street records with a part in it, and the `ALSE` block and admin areas that touch it. Sparse, Hilbert-ordered like `TIDX`, read block by block.
- **`omp-core::search`:** open the search sections, hold the `fst` and block indexes, read entries and postings. **`omp-core::addr`:** read a street's run, suffixes and units, resolve a house number to a position (point or interpolated), and list the addresses near a position. **`omp-core::admin`:** names and chains, point-in-polygon.
- Positioned reads with small caches, `Send + Sync`, as `TileReader` (phase 1 decision 1). Report the bytes they hold for the RSS budget.
- **Format 0.4:** every new section in format.md, the search part marked **Specified**. Older packs without search still open; the app hides search for them.
- Round-trip tests with a test writer, as in phase 1 step 1.1: streets with several parts, a street at a chunk boundary, units and suffixes, ranges, unattached points, empty cells.

**Done when:** the readers round-trip a test pack and read the Ohio and Quebec packs, and format.md describes every search and address section.

> **Done 2026-10-26** (the commit that adds `crates/core/src/{search,addr,street,admin,grid}.rs`).
> - **Readers** in `omp-core`, each over an `Arc<Pack>` with positioned reads, `Send + Sync`, holding block indexes in RAM and decoded blocks in small LRU caches (`blocked.rs`, shared by all): `search::Index` (the `fst`, towns, entries, postings, optionally only a list's first IDs), `admin::Admin` (`SADM` in RAM, polygons from `SAPL`, chains), `street::Streets` (records, geometry, offsets, projecting a point onto a street), `addr::Addresses` (a street's run with suffixes, postcodes and units, its ranges, the unattached points of a z14 cell, postcodes) and `grid::Grid`. `search::Search` opens them together (`None` for a pack without search, built before 0.4) and does the lookups that need several: `address_point`, `locate(street, number, suffix)` (the points with that number, the exact suffix first, else the ranges holding it, interpolated and marked approximate), `streets_near` and `addresses_near(point, radius)`. `geo.rs` has the projection, metres per unit and Morton codes. Each reader reports the bytes it holds (`heap_bytes`) and the bytes read into its caches, for the F3 overlay in 2.7.
> - **Spatial grid** (`SGRD`, `builder/src/street/grid.rs`): per z14 cell, the streets with a segment in it (walking the cells each segment crosses) and whether it has unattached addresses. Hilbert-ordered, in 16 KiB zstd blocks behind the shared index. Admin areas are **not** in the grid, against the plan: the reader finds their candidates from the bounding boxes in `SADM`, which it holds anyway, and `SAPL` decides, so a cell list would only add bytes. z14 against z13 and z15:
>
>   | | Ohio z13 / **z14** / z15 | Quebec z13 / **z14** / z15 | North America z13 / **z14** / z15 (no unattached points) |
>   |---|---:|---:|---:|
>   | Cells | 8,210 / **31,943** / 114,343 | 22,913 / **62,339** / 158,578 | 654,638 / **2,057,513** / 5,821,315 |
>   | Streets per cell, p99 (max) | 329 (863) / **102 (379)** / 36 (148) | 129 (416) / **56 (198)** / 26 (81) | 291 (2,620) / **100 (1,008)** / 38 (418) |
>   | `SGRD` (MiB) | 0.08 / **0.18** / 0.38 | 0.08 / **0.16** / 0.31 | 3.81 / **7.82** / 15.42 |
>
>   z14 is kept: it matches `ALSE`'s cells, so a cell's `loose` flag names one `ALSE` block; z13 reads three times the streets in a dense cell, and z15 doubles the size for lookups that are already well under a millisecond.
> - **Format 0.4** ([format.md](format.md)): the admin, street, search and address sections are **Specified**; `SGRD` is new; `SAPL` takes the shared block layout (`count` added). The reader opens 0.3 packs too (their tiles; `Search::open` needs 0.4), so phase 1 packs still open and the app can hide search for them.
> - **Tests.** A test writer in `omp-core` (`search_tests.rs`) writes every section from format.md on its own, at 1, 2 and 1,000 items per block, into a pack with 8 KiB chunks (sections in different chunk files), and reads it back: a street with two parts (offsets run on), a one-point street, streets across z14 cells, suffixes, units, postcode exceptions, ranges both ways and by parity, unattached points with units, empty cells, chains through parents, postings read in part, and packs without the grid or without addresses. The builder's tests read the builder's own `SADM`/`SAPL`, `SSTR`/`SGEO`/`SGRD`, search and address sections back through the core readers (chains equal the builder's, offsets and points equal `Along` and `point_at`).
> - **Ohio and Quebec** packs rebuilt with the grid (`$OMP_WORK/measure/grid25/`; Ohio 39 s at 6.0 GiB, Quebec 91 s at 8.8 GiB; `verify` passes). `inspect --readers` reads every street, cell, address, unattached point and range through the readers, then checks about 2,000 samples of each: every sampled entry is in the postings of its tokens, street middles have an admin chain and are within 1 m of themselves in `streets_near`, every sampled address is found by `locate` at its point and by `addresses_near`, every unattached point is in a `loose` cell and near itself, and range midpoints interpolate. All pass on both packs. Timings on the dev machine, with the pack in the OS page cache (cold USB reads are for 2.9):
>
>   | p95 (ms), first call / again | Ohio | Quebec |
>   |---|---:|---:|
>   | Entry | 0.072 / 0.000 | 0.072 / 0.000 |
>   | Postings of a token | 0.116 | 0.142 |
>   | Admin chain | 0.010 / 0.010 | 0.007 / 0.006 |
>   | Streets within 100 m | 0.520 / 0.014 | 0.407 / 0.011 |
>   | Locate a number on a street | 0.224 / 0.004 | 0.374 / 0.031 |
>   | Addresses within 100 m | 0.575 / 0.047 | 0.888 / 0.232 |
>
>   **Memory:** the readers hold 0.8 MiB on opening Ohio or Quebec and about 20 MiB after the 2,000-sample run, their cache budgets (24 MiB in all) nearly full.
> - **Admin chains** come from the stored polygons, simplified to 50 m, while the builder gave each street the area of its middle from the full ones: 33 of 2,003 sampled Ohio streets (26 of 2,020 in Quebec) get a chain without their street's area, all within 60 m of that area's stored edge.
> - **Left open:**
>   - North America was not rebuilt; its grid size is from `omp-builder streets` (5.9 min, 19.8 GiB), which builds the same street table without addresses, so it has no cells holding only unattached addresses and its streets are not split by postal town. 2.9 rebuilds it with addresses.
>   - The builder keeps its own decoders (`store::decode_*`, `street::encode::decode`, `search::encode::decode_*`) for the checks it runs as it writes and for `inspect --search`; they could move to the core readers.
>   - States and countries in a chain come from the deepest area's parents, so a point in an area that straddles a state line gets the state of that area's label point; not seen in the samples.

## 2.6 Query parsing, matching and ranking (`omp-core`)

The search engine itself, in `omp-core`, so it is tested without the app.

- **Parsing** into parts, each optional: house number with suffix ("123A", "123 1/2"), unit ("Apt 4", "Unit 101", Canadian "4-123"), street, locality, state or province (names and codes: "IL", "Illinois", "QC"), postcode, and free text for places and POIs. Try the few plausible readings of a query rather than one fixed grammar; "Kingston" is a place, and "100 Kingston" is an address on Kingston Rd.
- **As you type:** the last token is a prefix (an `fst` range), the earlier tokens are whole words. Fuzzy: edit distance 1 for tokens of 4+ letters and 2 for 8+, using `fst`'s Levenshtein automaton, scored below exact matches.
- **Ranking:** text match (all tokens, in order, exact or fuzzy), importance (2.3), and distance from the map view's centre, scaled by zoom: at z5 the continent is near, at z14 the neighbourhood is. Admin filters from the query ("Springfield, IL") are hard filters when they match an admin area. Ties go to the larger place.
- **Address results:** an exact point gives its position; otherwise the TIGER or OSM range on the right side and parity gives an interpolated position, marked approximate, as are NAR blockface points (`approx`). A number with no point and no range gives the street, with "number not found". Units are listed under their address.
- **CLI:** `omp-builder search <pack> "<query>" [--near lat,lon] [--zoom z] [--json]` and `omp-builder here <pack> <lat,lon>`.
- **Query set** (`crates/core/tests/search/`, kept in the repo, run in CI on a small test pack and by hand on North America):
  - 1,000 random merged addresses from Ohio, Ontario and Quebec (and later all of North America), with their expected positions;
  - about 200 hand-written queries: cities that share a name (Springfield, London ON vs London KY, Paris), POIs by name and brand, French queries and missing accents, abbreviations both ways, numbered streets, routes by number, typos, postcodes, units, and queries that should give nothing.

**Done when:** the query set meets exit criterion 2 on the Ohio and Quebec packs through the CLI, and a warm query takes under 20 ms p95 on the dev machine.

> **Done 2026-10-26** (the commit that adds `crates/core/src/find.rs`).
> - **Code** (`omp-core`). `query.rs` parses a query: words and commas, a leading house number ("123A", "123 1/2", Canadian "4-123" read both as unit 4 at 123 and as 4-123; not an ordinal before a street type, "4e Rue"), a unit ("Apt 4", "#4", "app. 4", "Ste 4" only before a number) and a trailing postal code. `find.rs` tries the readings: the last one to three words as a state, province or country (names, alternative names, codes and "Ont", "Que", …) or not, the rest split into a name and a locality at each comma (or at every word without commas), with and without the house number. Candidates are the entries holding every telling token of the name (types, directions, articles and "and", "city", "Dr", "Jr" may be missing), read in importance order, from each region of the reading: each locality (the places, admin areas and postal codes its words name, grown 10 km, since postal towns reach well past their limits), else the postal code, the state, the view (half and four times its scale) or the most important anywhere. Scored by name match (exact 1, prefix 0.9, one edit 0.85, two 0.7; a direction the name contradicts rules it out), words of the name the query leaves out, the locality (postal town 1, an admin area below the state 0.9, only inside 0.8, else ruled out), importance, distance from the view scaled by zoom and from the locality, and a bonus for places that match the whole query. A house number is located on the streets that score near the best: the point (the exact suffix, then non-approximate, then OSM's), else the range on that side (approximate), else an unattached point of the street's name nearby ("ch. Arnold" for Chemin Arnold), else the street, marked "number not found"; units and postcodes come with it. A second pass matches within edits: every token if nothing matched exactly, else the rare ones (under 30 entries: "Clevland" is a street, but Cleveland is meant) unless an address was found. A street's records are grouped in the results. `fuzzy.rs` is an `fst` automaton whose state is a row of the edit-distance table (with swaps), instead of `fst`'s Levenshtein automaton, which builds a whole DFA first and needs a feature. `here.rs` gives the nearest address within 100 m (point, else interpolated on the nearest street's range on that side), the nearest street, the entries close by and the admin chain.
> - **Spatial lookups with no format change.** Entry IDs run by importance bucket, then Morton code (format.md), so in each bucket the entries of an aligned square are one run of IDs. `Index::intervals` finds them by binary search on the blocks' first keys (8 bytes per block, filled as blocks are read), and a postings list is cut to a town or the view without reading entries; `entries_in` lists the entries of a bbox for "what's here". format.md now says so.
> - **CLI:** `omp-builder search <pack> "<query>" [--near lat,lon] [--zoom z] [--prefix] [--json]`, `--check <files>` runs query sets, `--sample <n> --out <file>` draws random addresses; `omp-builder here <pack> <lat,lon> [--json]`.
> - **Query set** (`crates/core/tests/search/`, README there): `addresses-{ohio,quebec,ontario}.tsv`, 1,000 random merged addresses as "number street, town, state" (334, 333, 333); `queries.tsv`, 241 hand-written queries in 18 categories; `fixture.tsv`, 20 queries on the core test pack, run by `cargo test` with checks of approximate results, units, postcodes, "number not found", typing, the view and "what's here". Results (`$OMP_WORK/measure/query26/check-*.txt`; Ontario rebuilt to format 0.4 for it, 75 s, 9.1 GiB):
>
>   | | Ohio | Quebec | Ontario |
>   |---|---:|---:|---:|
>   | Random addresses: first at its point (10 m, or 100 m if approximate) | **99.4%** | **97.3%** | 95.8% |
>   | … or another point of the same address | 100% | 100% | 100% |
>   | Hand-written: first / top 5 | **98.9% / 100%** (90) | **98.7% / 100%** (79) | 98.6% / 100% (72) |
>   | "What's here" at the address points (that address, or one within 30 m on the street) | **100%** | **100%** | 99.7% |
>   | Query p50 / p95 / max, warm (ms) | 3.5 / **17.0** / 53 | 2.7 / **15.3** / 39 | 2.7 / 14.8 / 44 |
>   | Same, typing (last word a prefix) | 3.7 / 22.2 / 73 | 2.7 / 15.5 / 42 | 2.8 / 16.1 / 66 |
>   | "What's here" p50 / p95 (ms) | 0.2 / 1.1 | 0.3 / 1.7 | 0.3 / 1.5 |
>
>   Every category is 100% in the top 5. First-place misses: "Quebec City" (the province's place node ranks above the city, which has no "Quebec City" name), "Dayton International Airport" (its access road first; the airport's name adds "James M. Cox"), "Ottawa airport" (the Carp airport first). Warm means the pack in the OS page cache; cold reads from USB are for 2.9.
> - **Duplicate address points.** Every address of the random set is found, but 2.5% (Ontario 4.2%) at another point of the same number on the same street, 10 m to 2.8 km away: the merge keeps two points of one address when its sources put them more than 250 m apart (or put one on the street and one unattached). That is the address store, not search; worth a look with the owner before 2.9.
> - **Memory:** the `SENT` entry cache is 12 MiB (was 4): at 4 MiB the Ohio p95 was 22 ms, at 8 MiB 20.0 ms. The readers hold about 28 MiB after 400 queries (cache budgets 32 MiB in all), against 20 MiB in 2.5.
> - **Left open from 2.3 and 2.4, now settled:** street names stay in both `SENT` and `SSTR` (scoring reads about 200 entries per query; reading their street records instead would add a block per candidate); street records are grouped in results; admin names filter streets through the chain; address-only street entries stay (unattached addresses are found through them, and also through an OSM street of their name); unit and address-only names are shown in mixed case. Importance stays as the builder sets it; ranking is tuned at query time.
> - **Left open:**
>   - North America: the query set, the timings and the `calle` postings (0.75M, from 2.3) are for 2.9, with the rebuilt pack.
>   - POIs outside the 11 map classes (museums, zoos, theme parks, cafés such as most Tim Hortons) are not searchable (decision 7); the query set uses POIs that are.
>   - Typing p95 on Ohio is 22 ms (prefix completions are read in part); the keystroke target is in 2.7 (100 ms).

## 2.7 Search UI

- **Search box** in the top bar, focused with Ctrl+F or `/`. Suggestions in a list under it as you type, up to 8, each with the name, a kind icon, the admin context ("Springfield, Illinois") and distance from the view centre. Arrow keys, Enter and Escape work; Enter with no selection takes the first.
- **Off the UI thread:** queries run on one worker; a new keystroke replaces the pending query, and stale answers are dropped, as the tile loader does. The UI repaints when an answer arrives, so the idle CPU stays near 0%.
- **Picking a result** flies the camera there at a zoom that fits it (a street's extent, a city's area, z17 for an address), and drops a marker with a label. "Approximate" shows on the marker and in the result line for interpolated and blockface addresses. Escape or a clear button removes it.
- **Results panel** for Enter on a query with many results: a scrollable list, as Streets & Trips did.
- **Packs without search** (built before phase 2) show the box disabled, with a note.
- Keep the last few searches in `config.toml`.
- Add searches to the bench: a scripted set of typed queries (one character at a time) with latency per keystroke in the JSON, for criteria 3 and 4.

**Done when:** on the Ohio and North America packs, typing gives suggestions, picking one flies there with a marker, approximate results are marked, and the bench reports keystroke latency.

> **Done 2026-10-26** (the commit that adds `crates/app/src/search.rs`).
> - **Code** (`omp-app`). `search.rs`: a `Worker` thread opens the pack's search readers when the pack opens (start-up does not wait; a 0.3 pack gives `Missing`) and runs `omp_core::find`; a new query replaces the one waiting, and `Session` drops answers to anything but the latest query (counted in the overlay). The box sits at the top left (not a top bar: the map has none; Layers and Menu float at the top right the same way). Ctrl+F or a typed `/` focus it; up to 8 suggestions show the kind icon, name, an "approximate" or "number not found" tag, the town and postcode (and units), and the distance from the view centre in the scale bar's units. Arrows, Enter (the selection, else a 50-result search that flies to the first and opens the results panel when there is more than one), Escape (closes the suggestions, then leaves the box; on the map closes the results, then removes the marker) and the × clear button. Enter keeps the last word a prefix, as the suggestions do ("Colum" + Enter is Columbus, not Clum Airport). Picking flies the camera (`camera.rs`, `Motion::fly_to`: out, across at a steady speed on screen, and in; a drag, wheel or key stops it) to z17 for an address, else to fit the hit's radius in 40% of the window, and leaves a pin with name, town, postcode and "Approximate" or "Number not found". The last 8 searches are in `config.toml` (`recent_searches`) and listed when the box is empty. A pack without search shows the box disabled, "No search in this map", with the reason on hover. The F3 overlay shows the open time, the readers' bytes held and read, stale answers dropped and the last query's find and display times.
> - **Kind icons** are glyphs of egui's monochrome emoji fonts, enabled through a direct `egui` dependency (feature `monochrome_emoji_fonts`): +0.74 MB, binary 14.0 MB.
> - **Focus:** egui applies a text field's focus filter only from its second focused frame, so Escape or an arrow straight after Ctrl+F would take the focus away (and Escape then acted on the map). `SearchUi` handles both (`move_focus(None)`, and an Escape that took the focus is treated as the box's). A headless egui test drives the keys (`search::tests::keyboard`); no window automation tool was available, so the keys were not tried by hand.
> - **Credit line:** only "© OpenStreetMap contributors" stays on the map (ODbL); the pack's whole attribution, which lists dozens of address sources (hundreds on North America) and covered the bottom right of the map since 2.4, is in About this map, which the line and Menu open (owner's request).
> - **Bench:** a step's `search` is typed one character at a time (`keystroke_ms`, default 150, from the keystroke, each waiting for its answer), then Enter, then a jump to the first result and a wait for detail. The JSON has per-step `search` (keystroke latency and `find` time, Enter, the top result, timeouts) and a run-level `search` (open time, first keystroke, every keystroke's latency, reader bytes). `OMP_BENCH_SHOTS=<dir>` saves screenshots of the suggestions and of the result. Both flights end with 8 searches.
> - **North America rebuilt** to format 0.4 with the grid (`build --addresses all --bands 3`, 53.0 min, peak RSS 49.4 GiB, 7,210.5 MiB; log `$OMP_WORK/measure/ui27/build-na.log`); the 0.3 pack is kept as `packs/north-america-0.3`.
> - **Bench** on the dev machine, packs in the page cache (`$OMP_WORK/measure/ui27/bench-*.json`):
>
>   | | Ohio | North America |
>   |---|---:|---:|
>   | Search readers open (background) | 6 ms | 20 ms |
>   | Keystrokes: p50 / p95 / max (ms) | 9.8 / **53.3** / 227 | 24.5 / **89.5** / 140 |
>   | Enter: max (ms) | 22.6 | 114 |
>   | Readers hold / read (MiB) | 27.1 / 127 | 43.0 / 273 |
>   | RSS before the searches / peak (MiB) | 127 / **193** | 167 / **239** |
>   | Idle CPU after the flight | 0.40% | 0.40% |
>
>   Latency is from the keystroke to the frame showing its suggestions, nearly all of it `find`. The slowest North America query is the address "1100 E 9th St, Cleveland, OH" (p95 138 ms); Ohio's 227 ms max is one keystroke of "333 W Broad St". Every Ohio search finds the expected result first, approximate address included. RSS grows by about 1.5 times what the readers hold (Ohio +66 MiB, North America +67), presumably freed query buffers kept by the allocator; under the 300 MB of criterion 4.
> - **Follow-up, typing an address with the view elsewhere** (owner's report, 2026-10-26): an address in a small town that shares its name with larger towns in other states was only found once the state was typed in full; "…, <town>, <state cut short>" gave nothing. Three causes in `find.rs`, fixed: a state was only read from whole words (now also a word of 3+ letters after a comma that begins at most 3 state or province names); a last part after a comma that named nothing yet made the whole query give nothing (now, while typing and only when nothing is found, the query is tried without it; the app sends every query as typing, and a trailing space still ends the word); and an address looked only in the 6 most important towns of the name (now, when they hold no address with the number, in up to 30, 30 candidates each past the first 6). On 300 random North America addresses typed with the whole continent in view (`--sample`, not kept): state cut to 4 letters 29.7 → **96.7%** first, no state 91.0 → **96.0%**, full state 97.7% unchanged; the Ohio, Quebec and Ontario query sets are unchanged. The fixture test covers a state being typed and a last part that names nothing. Bench keystroke p95 on North America 88.5 ms (was 89.5).
> - **Left open, for 2.9:**
>   - North America results that are wrong first: "45201" gives 05201 Bennington, VT (a fuzzy postcode; 45201 is a Cincinnati PO-box code with no addresses, so not in `APCD`), and "Tim Hortons Kingston" gives a McDonald's in Kingston (most Tim Hortons are cafés, which are not searched, decision 7; a fuzzy brand match should not win). Street records of one street are not always grouped: "Rue Sainte-Catherine Ouest" shows three times. The query set on North America will measure these.
>   - Keystroke p95 on North America (89.5 ms warm) leaves little room under 100 ms; the E6430 target (250 ms) and the cold first query are 2.9's.

## 2.7b Progressive search

*Added 2026-10-26 at the owner's request, after the follow-up to 2.7.* Suggestions come back in tens of milliseconds because `find` reads a bounded slice of the data: the 6 most important towns of a name (30 when they hold no such address), 300 candidates per region, 64 expansions of a word being typed (3,000 IDs each), 30 streets to locate a number on, edits only for rare words. Anything past those bounds is missed until more is typed. A partly typed town can match only a less likely address or street ("number not found"). Someone could then decide the address is not in the pack and stop typing, when a few more letters would have found it.

The idea: answer at once as now, then, while the user pauses, keep searching deeper and add what turns up, with a visible "searching more" row until there is nothing left to try. A new keystroke starts over at the fast level.

- **Levels share one run** (decided 2026-10-26 with the owner). A deeper level redoes the work of the one before, but cheaply: with bounds growing about 4× per level, levels 0–3 cost about 85 units against 64 for the last alone, and most of the repeat is reads, which the query's caches can hold. A design that never repeats work (each bounded source a cursor that a level advances, new candidates merged into a running top list) was considered and not taken: `find`'s passes depend on each other's results (the pass within edits only if nothing matched exactly, readings without a number skipped once an address is found, the wider town pass only when the first towns hold no such address, the unfinished pass only when nothing is found, duplicates removed across all), so it would mean rewriting most of `find.rs` to save only the rescoring in memory.
  - **`Search::start(&Request) -> Session`** holds the parsed query and the `Run` (its postings, entries, localities, names and candidate lists), and **`Session::level(d, cancel)`** runs level *d* with those caches. `find` stays as level 0 of a new session, so the CLI, the tests and the query sets do not change. The worker owns the session (`Run` holds `Rc`s, so it stays on the worker thread) and drops it on a new query.
  - **Caches that a larger bound would defeat:** postings are cached by token and limit, and localities by how many are kept, so asking for more reads them again. Cache by token (or reading) only, keep the longest read and take a slice of it. Also cache scored candidates by reading and entry, so a level scores only entries it has not seen.
  - **Levels add kinds of work, not only more of it.** Level 0 is today's `find`. Level 1 always runs the wider town pass and the pass within edits (new work, not a repeat). Level 2 reads every town of the name and about 4× the expansions of a word being typed, candidates per region and streets to locate a number on. Level 3 lifts the bounds for the readings found. Each level's hits are merged with the session's hits so far, so the list only grows or improves.
  - **Measure first.** Before building the rest, keep one `Run` and run `all` again with larger bounds, to see what the repeat costs with the caches shared and without. Then measure on North America how long each level takes and what it adds (the random address sets typed with the state cut short, with no state, and with the town cut to its first letters), and set the bounds from that. The `Run`'s caches grow with depth but live only for one query; record their peak at the deepest level.
- **Worker.** After answering a query at level *d*, if no newer query is waiting, run level *d*+1 for it, up to the last. A new query preempts between levels. Also within a level: `Session::level` checks a cancel flag (an atomic generation number) between readings and towns and returns early, so a deep level never delays the next keystroke's fast answer. Optionally a short pause (100–200 ms) before level 1, so fast typing never starts deep work. Once the last level is done the worker sleeps, and idle CPU is back near 0%.
- **UI.** A level's answer replaces the list for that query, merged so that rows do not jump: the highlighted row (arrow keys) and the row under the pointer stay put, and new rows go below them or in rank order beneath. A last row shows "Searching smaller towns…" (or a spinner) while deeper levels run, and "No more results" (or nothing) once the last is done. If the last level finds nothing, the message says so ("Nothing found"), which then really means nothing within the pack. Enter runs the deepest level at once for the results panel.
- **Bench and overlay.** Keystroke latency stays level 0. The bench also reports each level's time after a pause, and when the expected result first appears (for the search steps given an expected name). The F3 overlay shows the current level.
- **Budget.** Level 0 keeps criterion 3 (100 ms p95 per keystroke on the dev machine, 250 ms on the E6430). A deep level may take up to a second or two on the E6430, since it runs only during a pause and is cancelled by the next keystroke. Memory stays within the readers' cache budgets (no larger caches for deep levels).

**Done when:** on North America, typing an address and pausing finds it at some level when the town is typed only partly or the state is left out, in clearly more cases than level 0 alone (measured on the random address sets), within 2 s of the pause on the dev machine. Level 0 latency is unchanged, a keystroke during a deep level is answered as fast as without it, and the list shows when it is still searching.

> **Done 2026-10-26** (the commit that adds `find::Session`).
> - **Code.** `omp_core::find`: the bounds are a `Bounds` per level (`LEVELS`). `Search::start(&Request) -> Session` holds the parsed query and the `Run`. `Session::level(d)` runs level *d* and returns the best hits of every level run so far, or `None` once `stop_when`'s check says a newer query came; the check runs between readings, every 256 locality entries, every 64 candidates and before each street located. `find` is level 0 of a new session. The CLI, tests and query sets are unchanged: Ohio and Quebec give exactly 2.6's results at level 0. Postings are cached by token, keeping the longest read and slicing it. Localities are cached by words, state and pass, and a read of more entries keeping more serves a smaller one. `name_ids` is keyed by the expansion bound too.
>   - **Levels.** Level 0 is 2.6's `find`. Level 1 always runs the wider town pass, the pass within edits (every token) and, while typing, the pass without the words after the last comma; that pass's hits lose 20 points there, so a hit that uses the typed words ranks above one that ignores them. Level 2 multiplies the bounds by 4, and level 3 by 16: 300 → 1,200 → 4,800 candidates per region, 800 → 3,200 → 12,800 locality entries, 30 → 120 → 480 towns for an address, 64 → 256 → 1,024 expansions of 3,000 → 12,000 → 48,000 IDs, 30 → 120 → 480 streets to locate a number on. The plan's "every town of the name" at level 2 was not needed: 120 towns, then 480, found what the measurements did. From level 1, 150 entry hits are kept per reading whatever the limit, so a finished session can serve Enter's results panel.
>   - **Not built:** the cache of scored candidates. Sharing the run already makes the deep levels 2–3 times cheaper than running each alone (below), and memory stays small.
>   - **App** (`search.rs`). The worker keeps the latest query's session. After answering level 0 it waits `PAUSE` (150 ms); if no query comes, it runs levels 1–3 one at a time, sending each answer (`Answer::level`, `more`). `Worker::ask` bumps an atomic generation that the session's stop check compares, so a deep level gives up within milliseconds of a keystroke. Enter runs the remaining levels at once; when the suggestions' session is for the same text and view, Enter goes on with it, so after a pause it is answered at once. A session's caches are kept until the next query. A deeper answer keeps the rows up to the highlighted one or the one under the pointer where they were, and puts the new ones below them in rank order (`merge`). While deeper levels run, a last row shows a spinner and "Searching smaller towns and more names…" (the only row if nothing is found yet). "Nothing found" now shows only once the last level is done. The F3 overlay shows the last answer's level, whether deeper ones are running, and the session caches (current and peak). A headless test checks the merge and that a new query drops the old one's levels. The UI was not tried by hand (no window automation tool).
>   - **Builder.** `search --check` takes `--level N` (each query down to level N), `--levels` (address files level by level: first, in 8, "lost" against level 0, time per level, time from the pause to a deeper level's find, session cache peak), `--fresh` (each level alone too) and `--typed state|state:N|nostate|town:N`. Documented in `crates/core/tests/search/README.md`.
> - **Measured** on North America, dev machine, warm, 300 random addresses (`--sample 300`, kept in `$OMP_WORK/measure/levels27b/` with the logs), typed (last word a prefix), with the whole continent in view. Share of addresses first at their point at level 0 → 1 → 2 → 3, then the share among the 8 suggestions at the deepest level:
>
>   | Typed | Level 0 | 1 | 2 | 3 | In 8 at 3 | Found by levels 1–3: shown after the pause, p95 / max | All levels p95 / max |
>   |---|---:|---:|---:|---:|---:|---:|---:|
>   | full state | 97.7% | 97.7% | 98.0% | **98.3%** | 98.7% | | 0.69 / 1.23 s |
>   | state cut to 4 letters | 96.7% | 97.0% | 97.7% | **98.0%** | 98.3% | | 0.73 / 1.36 s |
>   | no state | 96.0% | 96.0% | 97.3% | **97.3%** | 98.3% | | 1.41 / 2.57 s |
>   | town cut to 4 letters, no state | 71.3% | 76.0% | 85.7% | **89.3%** | 94.0% | 55 queries: 1.33 / **1.74 s** | 1.73 / 3.17 s |
>   | town cut to 2 letters, no state | 47.0% | 56.7% | 66.3% | **76.0%** | 89.3% | 90 queries: 1.57 / 2.28 s | 2.03 / 3.08 s |
>
>   No address first at level 0 lost first place at a deeper level ("lost" 0 at every level, town cut to 4 and no state). Most addresses still missed with the town cut short have a town that is ambiguous at that point ("West…", "San ", "New ", "Town…"), and only more typing settles those. Levels alone against shared, p50 (full state): level 1 106 vs 44 ms, level 2 212 vs 131 ms, level 3 283 vs 91 ms. The session caches peak at 14.7–24.4 MiB (town cut to 2), freed with the session. With the town cut to 2 letters, 5% of the addresses deeper levels find take more than 1.57 s to show, the slowest 2.28 s, a little past the 2 s of "Done when".
> - **Query sets at the deepest level** (`--prefix --level 3`), Ohio / Quebec / Ontario: hand-written 98.9 / 98.7 / 98.6% first, addresses 99.4 / 97.3 / 95.8% first. Both are exactly 2.6's rates, so the deeper levels do not reorder what level 0 gets right. Time with every level: p95 81 / 101 / 93 ms, max 473 ms.
> - **Bench** (`$OMP_WORK/measure/levels27b/bench-*.json`). After the last keystroke the bench waits for the deeper levels and records when each was shown (`levels_ms`) and, for a step with `expect`, when that name first showed (`expected_ms`, `expected_level`). North America gains a step with the town cut short ("8723 River Road, Amar"; with the bench's view it is found at level 0).
>
>   | | Ohio | North America |
>   |---|---:|---:|
>   | Keystrokes (level 0): p50 / p95 / max (ms) | 9.4 / **49.2** / 72 | 28.8 / **90.8** / 167 |
>   | Last level shown after the last keystroke (ms) | 152–404 | 180–1,728 |
>   | Enter after the pause: max (ms) | 7.7 | 8.2 |
>   | RSS peak (MiB) | 186 | 237 |
>   | Idle CPU after the flight | 0.30% | 0.40% |
>
>   Keystroke latency is as in 2.7 (53.3 / 89.5 ms p95); the new North America step's slowest keystroke is the 167 ms max. Keystrokes typed 150 ms apart now often land while a deep level runs, so this also shows a keystroke during a deep level is answered as fast. Enter was 114 ms max in 2.7.
> - **Left open, for 2.9:**
>   - Level 0 with the town cut short is slower than the bench suggests: keystroke p95 171 ms (town cut to 4) and 174 ms (cut to 2) in `--levels`, over the 100 ms target; with the full state, 103 ms. That is 2.6's `find` with a partly typed town expanding to many towns, and the E6430 run will show how much it matters.
>   - The deep levels on the E6430: they may take 3–4 times as long, and a level-3 time of 2.3 s here would be about 7 s there. If so, the level 3 bounds come down (it adds 3.6 points of the 18 gained with the town cut to 4).

## 2.8 "What's here"

- **Trigger** (decision 8): right-click on the map opens a small menu with "What's here?" and the coordinates (copyable). Left click stays free for panning, and phase 3 will add "Route from here / to here" to the same menu. Optionally, a left click on a POI icon or a place label shows the same panel.
- **Answer:** the nearest address within 100 m (from the grid, street runs, ranges and `ALSE`), marked approximate when it is; the nearest street; POIs within about 30 px; the admin chain (city, county, state, country); and the coordinates in degrees.
- **Shown** in a small panel anchored to the point, with a marker; closed by Escape or a click elsewhere.
- Same code as `omp-builder here`.

**Done when:** right-clicking in Ohio, Quebec and Manhattan gives the expected addresses and places, and `here` on the query set's address points meets exit criterion 2.

> **Done 2026-10-26** (the commit that adds `crates/app/src/here.rs`).
> - **Core.** `Search::whats_here(p, radius, entries)` (`omp_core::here`) is what the panel and `omp-builder here` show: `Search::here`'s nearest address within `ADDRESS_RADIUS_M` (100 m) with its place line ("New York, NY 10001": the street's postal town, the state's code, the postcode; a loose address takes the nearest street's town), the nearest street, the places, POIs and named features within `entries` (postcodes and street entries left out), and the areas shown (`shown_areas`): the municipality (the deepest area at level 8, else 7), the county (6, else 5), the state (4) and the country (2). Boroughs and neighbourhoods (9–10) are left out, so Manhattan's "Community Board 5" and Québec's "Vieux-Québec–Cap-Blanc–Colline-Parlementaire" are not shown. `omp-builder here` prints the same answer, plus the whole chain with levels.
> - **App.** A right-click on the map opens a menu: "What's here?" (disabled, with the reason on hover, for a pack without search) and the point's coordinates, which a click copies as `lat, lon` to six decimals. "What's here?" sends a request to the search worker, which answers it before the next search level. A request stops a level that is running (the session's stop check also watches a "what's here waiting" flag), and the level runs again afterwards. The panel sits next to a blue dot at the point and is kept inside the map. It shows the address (with "approximate"), its place line and distance, the nearest street when the address is not on it (or when there is no address), up to 5 places and POIs within 30 screen points (15–300 m), the areas, and the coordinates with a Copy button. When the address is more than 8 points from the click, a ring marks it, joined to the dot by a line. Escape on the map closes the panel before the search results and marker; so do its × and a left click on the map. Left drag still pans. The optional left click on a POI or label was not built. A headless egui test covers the right click, the request, a stale answer being dropped, and Escape and click closing the panel. The menu was not used by hand: no window automation tool was available, so the bench drives the same code from the view centre.
> - **Bench.** A step with `here = true` asks at the view centre after its move and records the time from the request to the frame showing the panel, plus what the panel shows (`here` per step, `here_ms` over the run, a screenshot with `OMP_BENCH_SHOTS`). The Ohio flight gains 3 such steps (the Statehouse, an approximate address in Butler County, and farmland with no address). The North America flight gains 5: the Empire State Building, Times Square, Vieux-Québec, the Plateau in Montréal and the Statehouse.
> - **Results** (dev machine, packs in the page cache; `$OMP_WORK/measure/here28/`, with the screenshots):
>
>   | Point | Address shown | Nearby | Areas | Request → panel |
>   |---|---|---|---|---:|
>   | Empire State Building | 10 West 33rd Street, New York, NY 10001 (39 m) | Empire State Building, Sushi teria, NY Skyride | Manhattan, New York County, New York, United States | 8.6 ms |
>   | Times Square | 1515 Broadway, New York, NY 10019 (17 m); street 7th Avenue | Theater District, Manhattan | as above | 1.9 ms |
>   | Vieux-Québec | 2 Rue des Jardins, Québec, QC G1R 4L4 (45 m) | Jardins de l'hôtel de ville | Québec, Agglomération de Québec, Québec, Canada | 9.4 ms |
>   | Plateau, Montréal | 4311 Rue Saint-Denis, Montréal, QC H2J 2K9 (12 m) | Restaurant Azalea | Montréal, Agglomération de Montréal, Québec, Canada | 9.0 ms |
>   | Statehouse, Columbus | 1 Capitol Square, Columbus, OH 43215 (19 m); street East State Street | (none within 30 points) | Columbus, Franklin County, Ohio | 8.2–9.1 ms |
>   | Butler County, Ohio | 4665 Logsdons Meadow Drive, Liberty Township, OH, approximate | | Liberty Township, Butler County, Ohio | 5.0 ms |
>   | Farmland, Union County | none within 100 m | | Paris Township, Union County, Ohio | 3.0 ms |
>
>   Bench RSS peak 248 MiB on North America (237 in 2.7b), keystroke p95 91.2 ms (90.8), idle CPU 0.30%. App binary 14.2 MB.
> - **Exit criterion 2** ("what's here" at random address points gives that address, or one within 30 m on the same street, in at least 95%): Ohio 334/334, Quebec 333/333, Ontario 332/333 (99.7%), and North America 299/300 (99.7%, the 300 addresses of 2.7b's `addresses-na.tsv`). Warm p95 1.2–1.4 ms on the regional packs and 3.6 ms on North America.
> - **The per-state town flag** (left open in 2.1 and moved here) is not needed for "what's here": the panel names the deepest level 8 area whatever the flag says, so Wisconsin's "Town of …" and New York's villages show as they are, and the postal town comes from the street. The flag only affects the builder's fallback for streets that have no postal town (under 3% of streets in those states, 17% in Wisconsin). That fix stays open, for after 2.9.
> - **Left open, for 2.9:** "what's here" cold from the USB stick (1 s target) and on the E6430 (200 ms warm). The panel opens to the right of the point and can cover the ring at the address (Vieux-Québec, where the address lies east of the point).

## 2.9 Performance, accuracy and hardware checks

As phase 1 step 1.10, with search added:
- Rebuild North America with search and addresses; update the size table and roadmap §4 with measured sizes per part.
- Run the query set on North America; record pass rates and the failures by category.
- Bench with searches on the dev machine (warm, and cold from the FAT32 USB 2 stick), the Latitude 7490 and the E6430: keystroke latency, first-query latency, "what's here" latency, RSS peak, start-up.
- Check binary size (the `fst` crate is new to the app) and that `cargo tree` still has no network crate.

**Done when:** every exit criterion is measured on every machine, with any that miss written up for the owner, as in phase 1.

---

## Order of work

```
2.1 admin ─► 2.2 streets + geometry ─┬─► 2.3 search entries + index ─┐
                                     └─► 2.4 addresses in build ─────┴─► 2.5 grid + readers ─► 2.6 query engine ─┬─► 2.7 search UI ─┬─► 2.9 checks
                                                                                                                 └─► 2.8 what's here ─┘
```

- 2.3 and 2.4 both need the street table from 2.2 and can run side by side.
- Build the North America pack at the end of 2.3 and of 2.4, so size and memory surprises show up early.
- 2.6 can start on the Ohio pack as soon as 2.5's readers exist; the query set grows through 2.3–2.6.
- 2.7 and 2.8 share the marker code and can be done in either order.
- 2.7b (progressive search) builds on 2.7's worker and list. It comes before 2.9, which measures the search latencies it changes.

## Decisions

Settled while planning (2026-10-25), from phase 0 and 1 data:
1. **The `fst` lives in RAM;** entries, postings, streets and addresses are read with positioned reads (phase 1 decision 1). The North America `fst` was about 5 MB in phase 0.
2. **One street table**, shared by search, addresses, "what's here" and later routing, with its own geometry; addresses are offsets along that geometry.
3. **Search only covers Latin script** for now, like labels (phase 1). Mexico gets places, streets and POIs, but no address points (none of the address sources cover it).

Settled by the owner (2026-10-25):
4. **The town shown is the postal town** (2.1), taken per street from its addresses (NAR, OpenAddresses, OSM `addr:city`). Where the data has none: the admin 8 name when it is a city, town or village, otherwise the nearest place node's name. Admin names are search aliases. This is the town shown in search results and "what's here".
   - *Revised in 2.1 from measurement, approved by the owner (2026-10-25):* where the data has none, the postal town of the neighbouring streets in the same municipality or township comes first, then the admin 8 town, then neighbours anywhere, then the nearest city, town or village, then any place node. On streets whose postal town is known, this order names it 93.8% of the time in North America, the original order 59.4% (see 2.1's Done note).
5. **Postcodes** (2.3): searchable, as a point at the centroid of their addresses. They are shown with addresses only if storing one per street turns out small.
   - *Measured in 2.3:* one per street with exceptions costs 7.6 MiB for North America (one per address 8.9 MiB), so they are shown; stored per street with exceptions in step 2.4.
6. **Alternative names** (2.3): `name:en`, `name:fr`, `alt_name`, `short_name`, `official_name`, `old_name` and `brand` are searchable; only `name` is displayed.
7. **No search-only POIs.** Search covers the POIs that are on the map. A larger POI set is left for later, once the final pack sizes are known, and would be added to the map and search together ([roadmap](roadmap.md#later-or-optional)).
8. **"What's here" on right-click** (2.8), so left click stays for panning.

Open: none.

## Out of scope for phase 2

- Routing and snapping a route to the street table (phase 3), apart from keeping the street table and grid usable for it.
- Saved pins and trips, import and export (phase 4). The search marker is not saved.
- Scripts other than Latin; Spanish street-type normalization for Mexico beyond what the data needs.
- Builder memory for a 16–32 GB machine (phase 6).
- An aerial-imagery check of the conflict sample (still open from phase 0); the query set checks merge quality instead.

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
| 2.3 Search entries and index | Not started | |
| 2.4 Address store in `build` | Not started | |
| 2.5 Spatial grid and core readers | Not started | |
| 2.6 Query parsing, matching and ranking (`omp-core`) | Not started | |
| 2.7 Search UI | Not started | |
| 2.8 "What's here" | Not started | |
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
> - **Code.** `street/mod.rs` builds the table from the named highway ways (`addr/osm.rs`, which now also keeps each way's `highway` class and which of its names are refs, and has a streets-only read for `build`). Ways are cut into **pieces** where they enter another admin 8 area and every 5 km. Each piece takes the postal town most of its address votes give, which spreads to touching pieces of the same name in the same admin 8 area that have none; the other pieces are labelled by their admin 8 area. A street is a run of touching pieces with the same key and label, which stops growing at 20 km. Its pieces are chained into parts (from the westmost free end, straight on at each node), quantized, simplified to 2 m, and placed on a Morton curve of the street's middle. Streets with no postal town use the decision 4 fallbacks, now shared with `addr/towns.rs` (`admin::towns::Fallbacks`). `Table::locate(way, point)` gives the street, offset, distance and side for an address, for step 2.4. `street/encode.rs` writes and decodes `SSTR` and `SGEO` ([format.md](format.md#streets--provisional), *provisional*). `build` writes both sections. `omp-builder streets <pbf>` reports counts and sizes. `addr-spike` builds the table with its postal town votes and runs the check below (`streets` in its JSON, worst cases in `streets-check.tsv`). Unit tests cover the town and length splits, the parts, locating points and the section round trip.
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
>   Not every address in the sample is within 5 m, so this falls short of the **Done when** line. Of the 0.4% more than 5 m off, 87% (Ohio) and 78% (Quebec) are more than 25 m from their street, at a cul-de-sac or a curve, where the nearest point on the line moves a long way when the line is simplified by 2 m. Most of the rest are nearer the street on the table than in the spike, because the table also projects onto the start of the next way. The decoded position is a point on the street either way, since A3 stores no distance from it. Taken as done; the owner may want to review this.
> - **Sizes** ([size-table.md](size-table.md#phase-2-step-22-streets)): North America 10.0M streets (0.51M with several parts, at most 54), 90.8M stored points; `SSTR` 90.9 MiB + `SGEO` 240.7 MiB = **332 MiB**, about 34 bytes per street. The geometry is below the 250–500 MiB estimate, so no fallback is needed. Ohio's pack has 253,216 streets (7.7 MiB), Quebec's 136,176 (5.8 MiB).
> - **Postal town splits** (`addr-spike`): Ohio 250,462 streets, 151,491 of them named by their addresses' postal town; Quebec 135,367, 117,244 by postal town. Without addresses (as `build` is until 2.4): 253,216 and 136,176.
> - **Build cost:** `omp-builder streets` on North America took 6.4 min at 20 GiB peak (3 min to read the ways, 39 s to build the table). `build` takes about 4–6 s more on Ohio and Quebec.
> - **Left open:**
>   - `build` has no addresses yet, so its streets are split and named by area only. Step 2.4 passes the address votes in (the `addr-spike` path already does).
>   - A way can only be cut at a vertex, so a long segment with no vertices passes the caps: North America's longest street is 79 km.
>   - Streets crossing the edge of a regional extract are cut there. Each pack is built on its own, so this does not matter.

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

## 2.4 Address store in `build`

Move the address merge from `addr-spike` into `build`, writing the A3 sections keyed to the street table from 2.2.

- **Pipeline:** the same sources and rules as phase 0 (OSM, OpenAddresses, NAR building and blockface points, TIGER ranges; attach within 200 m, dedup at 250 m, OSM wins; decisions 9–13), run per state and province as `--each` does, after the tile bands so their memory is free. `--exclude-license` stays.
- **Better attach.** 11% of North America's addresses matched no street (103 MiB in `ALSE`, a third of the address bytes). Phase 0 found the causes: streets missing from OSM, spelling differences (Dixon/Dixion), and prefixes like "Oro-Medonte Line 4". Add fuzzy matching (edit distance 1 on a token of 5+ letters, only when there is one candidate within the distance), and matching through the postal city alias. Measure the attach rate and the false matches on the phase 0 samples.
- **Format changes for random access:** `ALSE` is split into blocks by spatial cell (for "what's here") with names per block, and `ASUF` and `AUNI` into blocks aligned with `ASTR` blocks, as format.md already allows. Check whether 4 KiB `ASTR` blocks are worth their 3.6% for faster cold lookups on USB.
- **Unattached addresses in search:** an `ALSE` point has its own street name. Index those names too, so a street missing from OSM can still be found by its addresses.
- **Attribution:** the pack's `META` lists every address source used, with its license and attribution text, from the fetch manifest (roadmap §3 and §8). The app's About window (phase 1 step 1.9) shows them.
- **Build cost:** record wall time and peak RSS for Ohio, Quebec and North America. Phase 0's `addr-spike --each` took 18.7 min and 30.8 GB, and `build` peaks at 43 GB, so they must run one after another.

**Done when:** `build` writes the address sections for Ohio, Quebec and North America; the address counts match phase 0.5 within 1% (or explain the change from fuzzy attach); the attach rate is recorded; and format.md is updated.

## 2.5 Spatial grid and core readers

The readers the app and the CLI share, in `omp-core`, and the grid for lookups by position.

- **Spatial grid** (`SGRD`): for each cell (z13 or z14, to measure), the street records with a part in it, and the `ALSE` block and admin areas that touch it. Sparse, Hilbert-ordered like `TIDX`, read block by block.
- **`omp-core::search`:** open the search sections, hold the `fst` and block indexes, read entries and postings. **`omp-core::addr`:** read a street's run, suffixes and units, resolve a house number to a position (point or interpolated), and list the addresses near a position. **`omp-core::admin`:** names and chains, point-in-polygon.
- Positioned reads with small caches, `Send + Sync`, as `TileReader` (phase 1 decision 1). Report the bytes they hold for the RSS budget.
- **Format 0.4:** every new section in format.md, the search part marked **Specified**. Older packs without search still open; the app hides search for them.
- Round-trip tests with a test writer, as in phase 1 step 1.1: streets with several parts, a street at a chunk boundary, units and suffixes, ranges, unattached points, empty cells.

**Done when:** the readers round-trip a test pack and read the Ohio and Quebec packs, and format.md describes every search and address section.

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

## 2.7 Search UI

- **Search box** in the top bar, focused with Ctrl+F or `/`. Suggestions in a list under it as you type, up to 8, each with the name, a kind icon, the admin context ("Springfield, Illinois") and distance from the view centre. Arrow keys, Enter and Escape work; Enter with no selection takes the first.
- **Off the UI thread:** queries run on one worker; a new keystroke replaces the pending query, and stale answers are dropped, as the tile loader does. The UI repaints when an answer arrives, so the idle CPU stays near 0%.
- **Picking a result** flies the camera there at a zoom that fits it (a street's extent, a city's area, z17 for an address), and drops a marker with a label. "Approximate" shows on the marker and in the result line for interpolated and blockface addresses. Escape or a clear button removes it.
- **Results panel** for Enter on a query with many results: a scrollable list, as Streets & Trips did.
- **Packs without search** (built before phase 2) show the box disabled, with a note.
- Keep the last few searches in `config.toml`.
- Add searches to the bench: a scripted set of typed queries (one character at a time) with latency per keystroke in the JSON, for criteria 3 and 4.

**Done when:** on the Ohio and North America packs, typing gives suggestions, picking one flies there with a marker, approximate results are marked, and the bench reports keystroke latency.

## 2.8 "What's here"

- **Trigger** (decision 8): right-click on the map opens a small menu with "What's here?" and the coordinates (copyable). Left click stays free for panning, and phase 3 will add "Route from here / to here" to the same menu. Optionally, a left click on a POI icon or a place label shows the same panel.
- **Answer:** the nearest address within 100 m (from the grid, street runs, ranges and `ALSE`), marked approximate when it is; the nearest street; POIs within about 30 px; the admin chain (city, county, state, country); and the coordinates in degrees.
- **Shown** in a small panel anchored to the point, with a marker; closed by Escape or a click elsewhere.
- Same code as `omp-builder here`.

**Done when:** right-clicking in Ohio, Quebec and Manhattan gives the expected addresses and places, and `here` on the query set's address points meets exit criterion 2.

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
- 2.7 and 2.8 share the right-click and marker code and can be done in either order.

## Decisions

Settled while planning (2026-10-25), from phase 0 and 1 data:
1. **The `fst` lives in RAM;** entries, postings, streets and addresses are read with positioned reads (phase 1 decision 1). The North America `fst` was about 5 MB in phase 0.
2. **One street table**, shared by search, addresses, "what's here" and later routing, with its own geometry; addresses are offsets along that geometry.
3. **Search only covers Latin script** for now, like labels (phase 1). Mexico gets places, streets and POIs, but no address points (none of the address sources cover it).

Settled by the owner (2026-10-25):
4. **The town shown is the postal town** (2.1), taken per street from its addresses (NAR, OpenAddresses, OSM `addr:city`). Where the data has none: the admin 8 name when it is a city, town or village, otherwise the nearest place node's name. Admin names are search aliases. This is the town shown in search results and "what's here".
   - *Revised in 2.1 from measurement, approved by the owner (2026-10-25):* where the data has none, the postal town of the neighbouring streets in the same municipality or township comes first, then the admin 8 town, then neighbours anywhere, then the nearest city, town or village, then any place node. On streets whose postal town is known, this order names it 93.8% of the time in North America, the original order 59.4% (see 2.1's Done note).
5. **Postcodes** (2.3): searchable, as a point at the centroid of their addresses. They are shown with addresses only if storing one per street turns out small.
6. **Alternative names** (2.3): `name:en`, `name:fr`, `alt_name`, `short_name`, `official_name`, `old_name` and `brand` are searchable; only `name` is displayed.
7. **No search-only POIs.** Search covers the POIs that are on the map. A larger POI set is left for later, once the final pack sizes are known, and would be added to the map and search together ([roadmap](roadmap.md#later-or-optional)).
8. **"What's here" on right-click** (2.8), so left click stays for panning.

Open: none.

## Out of scope for phase 2

- Routing and snapping a route to the street table (phase 3), apart from keeping the street table and grid usable for it.
- Saved pins and trips, import and export (phase 4). The search marker is not saved.
- Scripts other than Latin; Spanish street-type normalization for Mexico beyond what the data needs.
- Builder memory for a 16–32 GB machine (phase 5).
- An aerial-imagery check of the conflict sample (still open from phase 0); the query set checks merge quality instead.

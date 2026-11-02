# Performance Findings

What [step 5.2](phase5-plan.md#52-audit-performance-baseline-and-profiles) measured: the baseline that 5.8–5.12 compare against, what each profile showed, and one table of leads ranked by gain over effort, each assigned to a step or left with a reason. The leads P1–P5 found while reading the code in 5.1 moved here from [audit-findings.md](audit-findings.md) on 2026-11-02, so every lead is ranked together; the bugs and design findings stay there.

Everything below is from the dev machine (AMD Radeon Graphics, RADV GFX1200, Vulkan; 64 GB), on `master` at `c0b5698` plus 5.2's bench hooks, with the North America pack of 4.5 (`measure/tz45/na-pack`, format 0.6, 9,910.4 MiB in 3 chunks) and Ohio (`packs/ohio`), as in 4.11. Logs, reports, traces and profiles are in `$OMP_WORK/measure/p5/5.2/` (`warm/`, `cold/`, `trace/`, `prof/`, `build/`; the flights written for this step in `flights/`).

## How it was measured

- **Warm:** each of 4.11's flights twice from the page cache (`north-america`, `north-america-p4`, `na-print`, `ohio`); the second run is the one quoted.
- **Cold:** from the FAT32 USB 2 stick (`OMPTEST`, 8 KiB clusters), remounted before every run, which drops its pages; the app runs from `/`. Each flight twice. Besides 4.11's flights, `startup` (no route or search asked), `cold-search` and `cold-route` (asked once start-up has settled, so their reads do not compete with start-up's).
- **Read traces:** the same cold flights under `strace -f -e pread64,openat`, with each read mapped to its section (`scripts/trace.py` in the measure folder; `rss.py` and `smaps.py` beside it).
- **Profiles:** a `profiling` build (new `[profile.profiling]` in `Cargo.toml`: release with symbols, built with `-C force-frame-pointers=yes`), `perf record -g` at 999–2,999 Hz over the frame, North America and p4 flights, by thread.
- **Allocations per frame:** the bench's new `count-allocs` feature (`allocs.rs`): a counting allocator per thread, so the bench reports the UI thread's allocations per frame, per step and in all.
- **Idle:** the bench now records egui's repaint causes for each frame of its idle period (`idle_wakes` in the report).
- **Memory:** `heaptrack` over a short flight (`heap`), `/proc/<pid>/smaps` by mapping during the North America flight, and RSS polled every 10 ms through a print flight (`print-rss`: Letter map pages at 300, 360 and 600 dpi, then directions at 360 dpi for 5 and for 25 stops).
- **Builder:** `perf` over an Ohio `build --addresses US-OH`, and at 49 Hz over the whole North America build (P20).
- **Memory caps:** after one run took the desktop down (heaptrack over the whole North America flight, 2026-11-02), every heavy run goes in `systemd-run --user --scope -p MemoryMax=…`, so it is killed alone if it overruns. The North America build peaks at 49 GiB, which does not fit beside a desktop session, so its profile was run with the IDE closed (P20).

## Baseline

**Warm, dev machine** (second runs; 4.11 in brackets where it measured the same thing):

| | |
|---|---|
| North America flight | frame p95 **1.33 ms** (1.30), cpu p95 0.54, labels p95 0.44; RSS peak **328 MiB** (329); idle CPU **0.6%** (0.5); start-up 120 ms |
| Search | keystrokes p50 / p95 / max 27 / 86 / 148 ms; the first search's slowest keystroke 145 ms |
| Routes | Miami–Seattle asked as the pack opens 342 ms (345; trip 140, directions 185); Halifax–Los Angeles 340 ms (347); 10 stops 298 ms (294); Cincinnati–Cleveland 38 ms |
| p4 flight | coast to coast 5 stops 350 ms (345), its 6 alternatives 728 ms (711); 25 stops: order 110 ms, reordered route shown 2,337 ms (2,286); 25 long legs 5,388 ms (trip 2,178, directions 3,187) |
| Printing (p4 flight) | map 300 dpi: preview 313 ms, PDF 554 ms, RSS +95 MiB; 360 dpi: 296 / 731 ms, +96; directions 360 dpi (5 stops, 90 steps): 386 ms / 2,176 ms, +177 |
| Printing (`print-rss`) | map 300 dpi +95 MiB, 360 dpi +82, **600 dpi +243** (4.11: +330); 25-stop directions (749 steps) at 360 dpi: preview 1,037 ms and +164 MiB, PDF **14.1 s**, RSS 421–503 MiB page by page, not growing with the pages |
| Ohio flight | frame p95 0.85 ms, RSS peak 189 MiB, idle 0.2–0.3% |

The p4 flight is not 4.11's (its 600 dpi step was dropped and the directions page now prints the 5-stop route), so its print rows are this step's baseline, not a comparison.

**Cold, from the stick** (two runs each, which agree within 1%):

| | 5.2 | 4.11 | 3.9 |
|---|---:|---:|---:|
| Start-up, North America flight | **3,462–3,472 ms** | 3,466 | 2,636 |
| Start-up with nothing asked (`startup`) | 3,476–3,482 ms | | |
| First route, Miami–Seattle asked as the pack opens | **10.16–10.18 s** (trip 8.04) | 10.1 (8.0) | 7.6 (5.3) |
| Miami–Seattle alone (`cold-long`) | 9.24–9.26 s | 9.2 | 6.5 |
| Miami–Seattle once start-up has settled (`cold-route`) | **6.12–6.18 s** (trip 5.4, directions 0.74) | | |
| Columbus alone (`cold-short`) | 3.35–3.37 s | 3.4 | 3.2 |
| Search: readers open | 1,049 ms | 1,047 | |
| Keystrokes p50 / p95 / max | 38–42 / 280–281 / 996–1,002 ms | 39 / 280 / 1,000 | 39 / 292 / 1,035 |
| First search after start-up: its first answer | 1,041–1,049 ms | | |
| Slowest jump | 2,197–2,216 ms (Manhattan z12) | 2,201 | 2,229 |
| Frame p95 / RSS peak | 1.30 ms / 328–332 MiB | 1.32 / 332 | |

## What the profiles showed

### Cold reads: the FAT chain walk

The cold traces show where start-up's 3.5 s goes, and it is not bytes: start-up reads under 3 MiB before its first full frame. Two reads of a few hundred bytes take most of it:

| | Offset | Bytes | Time |
|---|---|---:|---:|
| The section table, at the end of the last chunk | 1.80 GB into `.002` | 896 | **1.57 s** |
| The tile index's first read | 2.15 GB into `.001` | 120 | **1.70 s** |

Linux's FAT driver finds a file offset by following the file's cluster chain, reading the FAT one 512-byte sector at a time, and on a freshly mounted stick none of it is cached. A test on the stick (single-byte reads after a remount) gives the rate: the first read at 0.25 GiB into a chunk takes 282 ms and 461 device reads, at 1 GiB 774 ms and 1,253 reads; a second read anywhere already walked is free. With 8 KiB clusters, a 4 GB chunk's chain is 2 MiB of FAT, about 2.6 s to walk end to end, and about 7 s for the whole pack. So every first read deep into a chunk pays up to a few seconds once per mount: start-up pays for the section table and the tile index, the routing readers 0.72 s for `RHDR` (3.27 GB into `.001`), and the first jump to Manhattan z12 its 2.2 s.

**The 3.9 → 4.11 start-up regression is not explained by layout:** 3.9's pack (`measure/private/na-pack`) put the tile index 2.24 GB into `.001` and its last chunk was 1.76 GB, about where they are now, so the two walks cost the same then. What changed between the two cold runs is left for 5.8 (the stick was refilled for 4.11; its cluster size then is not recorded).

### Cold first route

`cold-route` asks Miami–Seattle 11.6 s after start, with the search and routing readers open and the stick idle. Its 6.2 s are, by the trace:

| | Time | Reads |
|---|---:|---|
| The CH query (`RCHE`, with the top already read) | 1.5 s | about 1,500 reads, 2.7 MiB |
| Unpacking the path's geometry (`RGEO`) | **4.5 s** | **2,346 random reads**, 25 MiB |
| Directions | 0.8 s | `SSTR`, `RNAM` |

At 21 MiB/s the geometry's bytes are 1.2 s; the rest is the stick's latency per read, about 1.4 ms. Asked as the pack opens, the same route takes 10.2 s: start-up's 3.5 s, the readers opening (1 s), and the routing top's warm-up (below), which the query waits behind.

**The routing top's warm-up competes with the tiles.** Right after the routing readers open, the `routing top` thread reads 42 MiB of `RCHE` in 1 MiB reads (1.3–2 s on the stick). Tile reads issued meanwhile wait for it: in `cold-long` the six loaders' reads for Columbus z11 all took 2.69 s, and that jump was detailed after 2.7 s, against 0.55 s in `startup`, where the jump comes after the warm-up.

### Cold search

`cold-search` types two queries once start-up has settled. The address query's first level answers after 1,041–1,112 ms (target 1 s), and its deeper levels after 1.8, 4.7 and 6.7 s. Between them the two searches read **162 MiB of `SENT`'s 196 MiB, in 22,083 reads** (9.3 s of reading), plus 5.5 MiB of `SPST` in 6,884 reads. The deeper levels read most of the entry section from the stick.

### A frame

`perf` over `frame` (Manhattan z14 held 15 s, a pan, a zoom out, the continent held), warm. The UI thread is 60% of the samples. Inclusive, of those:

| | Share of samples |
|---|---:|
| `App::ui`, the app's frame code | 30% |
| of which `labels::Placer::place` | **22%** |
| egui-wgpu's `paint_and_update_textures` | 25% |
| of which egui's `Renderer::update_buffers` | 16% |
| and our `render::Renderer::prepare` (buffer writes each frame) | **13%** |
| `side_panel` | 4% |
| hashing label sprites (`text::Sprite`, std's SipHash) | 3.4% |

Label placement runs in full on every frame, including frames where nothing moves, and the label and overlay buffers are written to the GPU each frame. The frame interval (p50 0.84, p95 1.00 ms in this flight) is mostly egui's painting and the present, not the app's code (cpu p95 0.34 ms).

**Allocations per frame** (UI thread, from one frame's start to the next): a still view allocates **561 times and 612 KiB per frame**; the North America flight p50 725 (716 KiB), p95 948; with a route shown 1,510 (1.5 MiB); a frame that asks for a route or search answer up to 9,500–51,000. "A frame with nothing moving should allocate nothing" does not hold yet.

### Idle

Of the 37 frames in the North America flight's 10 s idle period, 20 are the bench's own wake-ups and **17 are egui's text cursor blinking** (`egui/src/text_selection/visuals.rs:313`): the search field still has focus after the search steps. Nothing else wakes the app: no timers or workers polling. The Ohio flight idles at 0.2–0.3%.

### Searching and routing, warm

In the North America flight the search thread is 8.8% of the samples, mostly zstd decompressing `SENT` blocks and `Index::block`. In the p4 flight the routing thread is 20%, and of its 12.2% inclusive in `worker::work`, **`Routing::segment_block` is 9.8%**: decoding whole `RGEO` blocks of segments, for the trip (`trip_until`, 4.6%) and even more for the directions (`directions_roads_until`, 6.8%). It agrees with the bench: the 25 long legs spend 3.2 s in directions against 2.2 s routing.

**P1 measured:** `ZSTD_createDCtx` is 0.06% of the routing thread's samples and 0.03% of the search thread's, against 3.25% for the block reads it serves. Not worth changing.

### Memory

At heaptrack's peak (`heap` flight: views, a search, Halifax–Los Angeles) the heap is **149 MiB** of a 318 MiB RSS. Its largest owners, by the first frame of our code on the stack: route segments (`Segment` vectors) 31 MiB, more `omp_core` vectors 12 MiB, the routing block indexes opened with the readers 8 MiB, the search worker 8 MiB, `Pack::read` buffers 5 MiB, addresses 4.5 MiB, `files::Saved` 4.2 MiB, route labels 4 MiB, glyph shaping 2.8 MiB, zstd dictionaries 3.2 MiB. By mapping, at the end of the North America flight (325 MiB): anonymous memory 217 MiB (the heap's arenas, glibc's per-thread arenas and GPU staging), shared libraries **68 MiB** (mostly Mesa's driver), `[heap]` 25 MiB, the executable 14 MiB. The tile cache's 96 MiB budget is the largest single owner in the long flight.

### The address merge (Ohio)

Ohio's `build --addresses US-OH` takes 70 s, the merge 10.4 s of it (peak 5.3 GiB). In the merge's window the samples are mostly reading the address sources: zlib inflating and `serde_json` parsing OpenAddresses' files and decoding OSM's protobuf, with about 7% of the threads' time in rayon stealing or waiting for work. `Keys::intern_names`, which P4 suspected, is 0.18%; `Streets::nearest` (attach) 3%. North America's 13.6-minute merge runs this for 66 subdivisions, so its profile is what tells whether reading or attaching dominates there: neither does, as the next section shows (P20).

### The North America build (P20)

`perf record -F 49 --call-graph fp` over `build --addresses all --bands 3 --deterministic` with the `profiling` build, in a 56 GB scope, with the IDE closed (2026-11-02; `scripts/na-build.sh`, the log `build/na.log` with each line's time in `build/na-stamps.txt`, the profile `build/na/na.perf`, reports in `build/na/rep/`). The build took **2,919 s (48.7 min), peak RSS 50,544 MiB** (49.4 GiB); the pack is 9,997.7 MiB in 3 chunks, SHA-256 of the chunks in order `27dbaceb…` (`build/na/pack.sha256`), the baseline P4, P5 and P21–P23 compare against. 989 thousand samples: on average 6.9 of the 16 threads busy.

Each stage's wall time, with the average number of threads on the CPU and the main thread's share (1.00 is the main thread on the CPU all the time):

| Stage | s | Threads | Main |
|---|---:|---:|---:|
| PBF index, time zones | 33 | 0.15 | 0.15 |
| admin | 57 | 10.4 | 0.06 |
| band 0: read / water and tiles | 235 / 78 | 8.6 / 8.8 | 0.02 / 0.32 |
| bands 1 and 2: read / water and tiles | 165 + 165 / 63 + 70 | 7.3 / 10.0 | 0.09 / 0.31 |
| addresses read | 120 | 6.0 | 0.35 |
| **addresses merged** | **810** | **3.7** | **0.65** |
| streets | 139 | 4.0 | 0.79 |
| addresses (store) | 49 | 8.9 | 0.46 |
| search | 28 | 4.6 | 0.74 |
| routing graph | 263 | 3.8 | 0.44 |
| contraction (CH) | 354 | 13.7 | 0.14 |
| routing sections | 111 | 7.6 | 0.46 |
| dictionaries, compress, write | 170 | 9.3 | 0.20 |

- **The merge is serial work, not reading or attaching.** Over its 810 s the main thread is on the CPU 65% of the time (about 525 CPU-seconds) while the workers mostly wait: 3.7 threads busy on average. `merge_one` runs the 66 subdivisions one after another, and much of each runs on the main thread. Of the main thread's samples that resolve to a statement of `merge_one` (16,850 of 25,691; the rest are in libc or the kernel, with no frame chain): `Streets::build` **40%** (`intern_names` once per way 11 points, the components' `by_node` hash map and union-find 15, the cell grid's hash maps 3, grouping and sorting the components 5), `normalize`'s loop over the records **27%** (five `Interner::intern` lookups and `postcode` per record, all with std's SipHash), `extract`'s serial part 13%, `dedup` 5%, `fallback_ranges` 5%. The parallel part (all threads' samples in the window): attach 27% inclusive, of which `Streets::nearest` 19%; reading OpenAddresses' files (zlib, `serde_json`) about 11%; `dedup`'s parallel sort 5%. So P4's `intern_names` matters at this scale (about 55 s on the main thread), and the rest of `Streets::build` and `normalize` matter more (P21).
- **Other single-threaded stretches:** the routing graph spends about 105 s on one thread (`graph::build` 38% of those samples, `NodeRanks::rank` 8%, `geom::simplify_kept` 6%; P22); the streets stage about 55 s (`street::Table::build`: `normalize::tokens`, Unicode decomposition, a sort; P23); the end of the address read about 25 s (`addr::osm::read_with`); the routing sections about 30 s (`routing::sections`). The PBF index (33 s) waits on the disk.
- **Well spread:** the contraction (`ch::witness::shortcuts`, 21% of all the build's samples) keeps 13.7 threads busy; zstd's level-19 compression is the next largest owner (about 7%).
- **P5 at scale:** the search stage is 28 s in all, so the entry sort's `morton` is a few seconds at most; `segment_order` does not show among the routing sections' owners. Seconds, as estimated.

## Leads

Sorted by estimated gain over effort; the cold-read leads first because they decide the targets that miss most (start-up 3.5 s against 2.6 s, the first route 6–10 s against 3 s). "Cost now" is on the dev machine and the stick.

| # | Where | Lead | Cost now | Estimated gain | Effort | Step | Decision |
|---|---|---|---|---|---|---|---|
| P6 | `omp-core` `pack.rs`, `tiles.rs`; the builder's writer | **Start-up's two chain walks.** The section table is written at the end of the last chunk and the tile index after the tiles, 2 GB into `.001`, so a cold open walks 1.8 + 2.15 GB of FAT chains before the first tile. Write a copy of the section table into chunk 0's header page (896 B of its 4 KiB), and put `TIDX` (69 MiB) ahead of `TDAT`, or in chunk 0. | 3.3 of start-up's 3.5 s cold. | Start-up from the stick about **0.3 s**, below 3.9's 2.6 s; the first jump's walk into `TDAT` stays. | Small in code; a format minor bump and a North America rebuild (decision 9). | 5.8 || **Owner 2026-11-02: leave.** FAT32 only (on exFAT a read 4 GB into a chunk is one device read, 0.2 ms, measured on the data disk), and only seconds off a cold start; FAT32 may be slower than other file systems. |
| P7 | the pack's chunks on FAT32 | **Every first read deep into a chunk walks its FAT chain** (about 1 s per 1.3 GiB of offset, once per mount): the routing readers' `RHDR` 0.72 s, the first Manhattan z12 jump 2.2 s, parts of the first route and search. (a) Once the first frame is up, a low-priority thread reads one byte at the end of each chunk, walking all three chains (about 7 s of the stick) before the user needs them, pausing while anything else reads; (b) measure an exFAT stick, where a contiguous file needs no chain (`NoFatChain`), and FAT32 with 32 KiB clusters (a quarter of the FAT); if either is much faster, the README says how to format the stick. | Up to 2.6 s per chunk, paid by whichever read comes first. | 1–2 s off the first jump, route and search on a fresh stick; (b) all of it, if the user formats the stick that way. | (a) Small; (b) measuring, with a stick the owner can reformat. | 5.8 || **Owner 2026-11-02: leave (a); (b) answered:** exFAT has no chain walk (measured, as P6), so sticks are best formatted exFAT, and the README says so (5.7). FAT32 stays supported and slower on first reads. The FAT32-specific code (chunks under 4 GiB, sections laid out so no record crosses a chunk) costs a division per read at run time, so making exFAT a requirement would gain nothing measurable: kept. |
| P8 | `routing/worker.rs` `routing top`, `Routing::warm_top` | **The routing top's warm-up holds up the tiles:** 42 MiB in 1 MiB reads right after the readers open; tile reads wait behind it (all six loaders 2.69 s in `cold-long`). Read it in smaller pieces (128 KiB) and only while no tile read waits, or start it after the first full frame. | The first jump after opening 2.7 s instead of 0.55 s on the stick. | About 2 s on the first jump; the route's head start kept. | Small. | 5.8 || **Owner 2026-11-02:** only if easy and it does not complicate the code; long routes are fast enough in practice. Session H. |
| P9 | `omp-core` `route::query` unpacking, `RGEO` reads | **A cold long route is mostly geometry:** Miami–Seattle reads 2,346 `RGEO` blocks (25 MiB) one at a time in path order, 4.5 of its 6.2 s. Collect the path's blocks first, read them in file order and merge near ones into larger reads (measure how close they lie: `segment_order` sorts segments in space). | 4.5 s of the cold first route. | 2–3 s, towards the 3 s target. | Medium. | 5.8 || **Owner 2026-11-02:** as P8. Session H, after a look at how close the blocks lie; dropped if it takes more than sorting and merging reads. |
| P10 | `omp-core` `Routing::segment_block`, `segment` | **Warm routes and directions decode whole segment blocks:** 80% of the routing thread's time in the p4 flight; directions are 3.2 of the 5.4 s for 25 long legs. A larger or per-request segment cache, decoding only the segments asked for, and `with_segment` instead of `segment`'s clone in the guide. Also the plan's lead on directions decoding each junction's cell. | Directions 196 of 350 ms coast to coast; 3.2 s on 25 legs. | Directions about halved. | Medium. | 5.9 || **Owner 2026-11-02:** as P8 (the easy parts: `with_segment` where a clone is not needed, the cache's size). Session H. |
| P16 | `omp-core` search's deeper levels, `SENT` | **Cold searches read most of `SENT`:** two queries read 162 of its 196 MiB in 22,083 reads; the first level answers after 1.04–1.11 s (target 1 s), the deeper ones after 1.8–6.7 s. Find which levels read what (postings of common tokens, block scans), and bound or order them. | 9.3 s of reading for two searches; the first answer 1.1 s. | The first answer under 1 s; deeper levels seconds sooner. | Medium; a look first. | 5.8 || **Owner 2026-11-02: look,** and take it if the gain is meaningful and the change plain; the current speed is workable. Session I. |
| P11 | `labels.rs` `Placer::place` | **Labels are placed again every frame,** still or not: 22% of the UI thread. Keep the last placement while the camera, the tiles in view and the overlays are unchanged. | cpu p95 0.34 ms still; more on the laptops. | A still frame's app time roughly halved. | Small to medium. | 5.3 (the label size setting re-places labels anyway), or left || **Owner 2026-11-02: fix:** nothing should be redone while the map sits still. Session G. |
| P12 | `render.rs` `Renderer::prepare` | **Buffers written to the GPU every frame:** 13% of the UI thread, plus egui's own 16%. Write them only when what they hold changes. | Part of the frame interval. | A few tenths of a millisecond per frame. | Small, after a look at what is rewritten. | with P11 || **Owner 2026-11-02: fix,** with P11. Session G. |
| P13 | the UI thread | **Allocations per frame:** 561 (612 KiB) still, 1,510 (1.5 MiB) with a route shown. Reuse the frame's vectors (labels, overlays, shapes). | Allocator churn each frame. | Small, and steadier RSS. | Small. | with P11 || **Owner 2026-11-02: fix,** with P11. Session G. |
| P14 | `text.rs` sprite slots | **Label sprites hashed with std's SipHash:** 3.4% of the UI thread. Use `foldhash` or `rustc-hash`, both already in the tree. | 3.4% of the UI thread. | About 3%. | Tiny. | with P11 || **Owner 2026-11-02: fix.** Session G. |
| P15 | egui text fields | **A focused field's cursor blinks twice a second,** the only idle wake-up (17 of 37 idle frames; 0.5–0.6% CPU, against the Ohio flight's 0.2–0.3%). Stop the blink after some seconds without input, or let the field lose focus when the map is used. | 0.3% CPU idle with a field focused; more on the laptops. | Idle CPU about 0.2%. | Tiny. | 5.7 || **Owner 2026-11-02:** the field loses focus when the map is used (clicked, dragged, scrolled); no timer to stop the blink. Session G. |
| P17 | heap and RSS | **What the 328 MiB is:** heap at most about 150 MiB (tile cache budget 96 MiB, route segments 31, routing indexes 8, search 8 + 41 of readers), libraries 68 MiB, the executable 14 MiB, and the rest glibc's arenas and GPU staging. Measure a smaller tile cache, the routing caches (8 + 8 MiB), `M_ARENA_MAX` or another allocator, each for what it saves and what it costs in time. | 328 MiB against 300 MB. | 30–60 MiB, from the cache and the arenas. | Small to try. | 5.10 || **Owner 2026-11-02: measure;** take only what does not cost speed elsewhere. Session J. |
| P18 | `pages.rs`, print preview | **The 749-step directions:** the preview adds 164 MiB and a PDF takes 14.1 s at 360 dpi; per page RSS goes up and down (421–503 MiB), so the pages do not pile up. The preview likely draws every page at once; the time is about 1.4 s per page. | +164 MiB and 14.1 s. | Preview memory to one page's; PDF time measured page by page first. | Medium. | 5.11 || **Owner 2026-11-02: yes:** keep print and preview memory from swinging. Session K. |
| P19 | `snapshot.rs` 600 dpi | **600 dpi is +243 MiB** (4.11 +330), 300 and 360 dpi +95 and +82. Banded drawing above 360 dpi, as 5.11 plans. | +243 MiB. | Under +100. | Medium. | 5.11 || **Owner 2026-11-02: yes,** for printers above 600 dpi too; the banded page must be identical to the unbanded one (a pixel comparison test). Session K. |
| P20 | the builder's North America build | **Profile the North America build** (`perf` at 49 Hz over `build --addresses all --bands 3`), which 5.2 did not run: it peaks at 49 GiB, which does not fit beside a desktop session. Run it with the IDE closed, under the 56 GB cap. | — | Says what P4 and P5 are worth at scale. | 1 hour of the machine. | 5.12 || **Owner 2026-11-02: run it,** with the IDE closed and Claude Code in a terminal. Session L, first. **Done 2026-11-02:** 2,919 s, peak 49.4 GiB; the merge (810 s) is serial work on the main thread, 3.7 threads busy on average ([the North America build](#the-north-america-build-p20)); new leads P21–P23. |
| P21 | `addr/merge.rs` `merge_one`, `addr/streets.rs` `Streets::build`, `addr/pipeline.rs` `normalize` | **The merge runs on one thread most of the time:** the 66 subdivisions one after another, and in each, `Streets::build` (per-way `intern_names`, the components' `by_node` map and union-find, the cell grid) and `normalize`'s per-record interning are serial, with std's SipHash maps. Make them parallel (prepare the ways and parse the records with rayon, build the maps per thread or as sorted vectors), use `foldhash`, and borrow the ways instead of cloning them (P4). Merging two subdivisions at once would also do, but the merge already peaks at 48.5 GiB of the 56 GB cap. | 810 s at 3.7 threads; about 525 s on the main thread. | The merge 810 → about 400–500 s (5–7 minutes off the build); covers P4. | Medium; packs must stay byte-identical. | 5.12 ||**Owner 2026-11-02: yes,** look into it. Session L. |
| P22 | `graph::build`, `pbf::nodes::NodeRanks::rank`, `geom::simplify_kept` | **The routing graph spends about 105 s on one thread** (of its 263 s). A look at what in `graph::build` is serial and can split by way or by node range. | About 105 s. | 60–90 s. | Look first. | 5.12 ||**Owner 2026-11-02: yes,** look into it. Session L. |
| P23 | `street::Table::build` | **The streets stage spends about 55 s on one thread** (of 139): `normalize::tokens`, Unicode decomposition and a sort per street name. Tokenize the names in parallel, sort with `par_sort`. | About 55 s. | About 40 s. | Small. | 5.12 ||**Owner 2026-11-02: yes,** look into it. Session L. |
| P1 | `omp-core` `Blocked::read` | Each block read makes a new zstd decompression context (`DCtx::create`, which allocates its window) and drops it: every search, street, address and routing block a cache misses. **Measured in 5.2:** `ZSTD_createDCtx` is 0.06% of the routing thread and 0.03% of the search thread. | Negligible. | None worth having. | Small: a context per reader, behind its cache's lock, or per thread. | — | **Owner:** check in 5.2. **Measured 2026-11-02:** leave. |
| P2 | `omp-builder` `tiles/mod.rs` cut | Every tile is also encoded as MVT and gzipped, for a size line only (D16). **Corrected in the third pass:** `build` calls `cut_pyramid` with `measure: false` (`build/mod.rs`), so only `measure` encodes MVT; `build` pays nothing for it. What `build` does pay for the spike is D19's second `RCHE` encoding, the layout comparison (D16), and `SENT`'s second compression (removed). | None in `build`. | None. | — | — | **Owner:** check in 5.2. Moot (third pass). |
| P3 | `snapshot.rs`, `pool.rs` | B7: the print worker's GPU slabs grew with every preview and page. 4.11's print memory (DX12 +965 MiB for one page) was measured with it. **Measured in 5.2:** a 300 dpi page is +95 MiB on the dev machine, 360 dpi +82. | Fixed. | Some of 5.11's print memory. | Done. | 5.11 measures again on the laptops | **Owner:** check in 5.2. **Measured 2026-11-02.** |
| P4 | `addr/merge.rs` `merge_one`, `addr/streets.rs` `Streets::build` | The address merge (13.6 of North America's 36.7 build minutes, refactor R4.5): each state and province clones every street way within its margin (names, node IDs, coordinates) into its own list, and `Streets::build` calls `Keys::intern_names`, a rayon `par_iter`, once per way on its one to three names, so millions of tiny parallel jobs. **Profiled on Ohio in 5.2:** `intern_names` is 0.18% of the samples; reading the sources (zlib, JSON, protobuf) dominates the merge, with 7% of rayon's time waiting for work. **In North America (P20)** the merge is 810 s with the main thread busy 65%, and per-way `intern_names` is 11% of the main thread's merge samples (about 55 s); the rest of `Streets::build` and `normalize` are more (P21). | 10.4 s of Ohio's 70 s; about 55 s of North America's 810. | About a minute in North America; with P21, more. | Small: borrow the ways, and key all names in one parallel pass. | 5.12 | **Owner 2026-10-30: yes**, as proposed. |
| P5 | `search::index`, `routing::segment_order` | Small builder waste: the entry sort computes `morton(p)` for both sides of every comparison (about 40 million entries); `segment_order` decodes each segment's whole geometry to read its first point, and `RGEO` and the grid decode it again. **P20:** the search stage is 28 s in all and `segment_order` does not show among the routing sections' owners. | Seconds (P20). | Seconds. | Small. | 5.12 | **Owner 2026-10-30: yes**, as proposed. |

## Order of work

The owner triaged the leads on 2026-11-02 (the **Decision** column). What is taken is grouped by the code it touches, one group per session, as the audit's fixes were. Each session: measure before, make the change, measure after on the same flight; mark the lead's **Decision** cell **Done** with the numbers; run the CI checks; commit and push. Heavy runs go in a memory-capped `systemd-run` scope.

| Session | Leads | Touches | Why together, and when |
|---|---|---|---|
| **G. Still frames do nothing** | P11, P12, P13, P14, P15 | `labels.rs`, `render.rs`, `text.rs`, `app/mod.rs`, the search field | One question, what a frame redoes when nothing changed, on the UI thread. Done when a still frame places no labels, writes no buffers and allocates (almost) nothing, checked with `count-allocs` on `frame.toml`, and the pixels of the frames are unchanged. **Before 5.3**, whose label size setting and UI size re-place labels and will build on the new "only when something changed" path. |
| **H. Routing, the easy parts** | P8, P10, then P9 if simple | `routing/worker.rs` (`routing top`), `omp-core` `route` (segments, unpacking) | The routing reader and its worker. P8 is the smallest and helps every cold open; P10's `with_segment` and cache size are local; P9 only if sorting and merging the path's block reads is enough. Measured warm (p4 flight) and cold from the stick (`cold-long`, `cold-route`). Beside 5.3–5.7 (as 5.8 and 5.9). |
| **I. Cold search reads** | P16 | `omp-core` search's levels | A look first: which levels read what. Taken only for a meaningful gain with plain code. Beside the UI steps (5.8). |
| **J. Memory** | P17, D14 | tile cache budget, routing caches, the allocator's arenas, the print worker | Measurements, each change weighed against what it costs in time (cold route, jumps, frames). 5.10. |
| **K. Printing** | P18, P19 | `pages.rs`, `printui.rs`, `snapshot.rs`, `render.rs` readback | The print path: banded drawing above 360 dpi with a test that the banded page equals the unbanded one pixel for pixel, the directions preview's memory, and the 749-step PDF time. 5.11. |
| **L. Builder** | P20 (done 2026-11-02), then P4 with P21, P22, P23, P5 and R10's SHA-256 check | `omp-builder` | P20 showed the North America merge is serial work on the main thread, so P4 goes in with P21 (the same functions); P22 and P23 are the other single-threaded stretches. P21–P23 taken by the owner 2026-11-02. Each change keeps the North America pack byte-identical (`27dbaceb…`), checked with a full build with the IDE closed; Ohio and Quebec for the dev loop. Then 5.12's own items. |

Not in a session: P6 and P7 (left: FAT32 only), P1 and P2 (nothing to gain), P3 (measured; the laptops in 5.11 and 5.13). The README's advice to format the stick exFAT goes with 5.7's first-run and message work.

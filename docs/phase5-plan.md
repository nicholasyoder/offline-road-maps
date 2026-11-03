# Phase 5 Plan: Settings, Map Styles, Polish and Performance

*Draft 2, 2026-10-29, with the owner's answers to draft 1's questions (decisions 6–11). Expands Phase 5 of [roadmap.md](roadmap.md#phase-5-settings-map-styles-and-polish), with the owner's changes from planning (2026-10-29): map styles are chosen in the side panel's Layers tab, which may be renamed; the "map tuning" item is dropped; the performance pass is split into separate items; and the whole project gets an audit for bugs, bad design and missed optimizations, now that the app is complete enough to look at as a whole.*

## Purpose

Phases 1–4 built every feature the roadmap promised: the map, search, routing and the trip planner. Phase 5 does not add features. It makes what exists easier to use, faster and more solid:

- **Settings in one place.** Every setting in `config.toml` can be changed from the UI and survives a restart. Today some can be changed only by editing the file (MSAA), some are scattered across the panel (units on the Layers page), and some are constants in the code (zoom speed, pan inertia, UI size).
- **Map styles.** The default style, a night style and a high-contrast style, chosen in the side panel's Map tab and switched without a restart. A night style turns the app's own UI dark as well.
- **Polish.** A list of keyboard shortcuts, tooltips, the window's size and position remembered, a first-run screen when no map is found, and plain error messages.
- **An audit of the whole project.** Read every module of the app, core and builder for bugs, fragile code and bad design that phases 1–4 left. Profile the app and the builder end to end for optimizations that were missed. The findings are written down, triaged with the owner, and fixed, folded into a later step, or left with a reason.
- **Performance, as separate items** (the roadmap's single "performance pass" split up): cold reads from the stick, long routes and directions, memory, printing, and the builder and pack. Each one starts from the audit's measurements, and each change is measured before and after.

What phase 4 left in place for this:
- `config.toml` holds the pack, the last view, the debug overlay, `msaa`, `units`, the layers, recent searches, the side panel, the schedule's settings and the print settings (`config.rs`). The window's size and position are not saved (eframe's `persistence` feature is off).
- The ☰ menu has disabled *Settings…* and *Keyboard shortcuts* entries (`side::later_in`).
- The map style is a TOML file (`data/style.toml`, compiled in), replaced by `style.toml` next to the executable or `--style`. Debug builds already reload it when it changes (`App::reload_style`), so switching at runtime is a matter of compiling another `StyleFile` against the pack's schema and swapping it in.
- The app's own UI colours are constants in `theme.rs` (`INK`, `WEAK`, `APPROX`, `ACCENT`), plus about 30 more `Color32` constants in `app.rs`, `routing/`, `printui.rs`, `search/` and `pins.rs`. `theme::apply` forces egui's light theme because the map is light.
- The route line, stop markers, pins and the print rectangle are drawn in fixed colours, not from the style.
- The print page takes the style and a text size as parameters (phase 4, "carried from earlier phases"); the print page has a style choice of its own, the map's by default (decision 8).
- The bench flights in `crates/app/bench/` (`north-america.toml`, `north-america-p4.toml`, `na-print.toml`, …) and their reports from 4.11 are the baseline for every performance item.

### Exit criteria (expanded)

1. **Settings.** Every setting in `config.toml` that a user would change is reachable from the UI (in the Settings window or the Map tab), takes effect without a restart (MSAA may ask for one), and survives a restart. A `config.toml` that is missing, from an older version, or damaged opens with defaults for what cannot be read, and says so once; it is never lost silently.
2. **Styles.** At least three built-in styles (default, night, high contrast) and any user `style.toml` are listed in the Map tab and switch at runtime within one frame's work plus the tiles' re-tessellation (aim: under 0.5 s on the dev machine at a North America view). The choice is remembered. The UI is dark with the night style. The print page has a style choice of its own, the map's by default and remembered (decision 8); a black-and-white printout of a route with the high-contrast style keeps the route, roads and labels readable (the owner's ET-2400, 4.10's open check).
3. **Polish.** Each item of 5.7 is done and checked by hand on Linux and Windows.
4. **Audit.** Every module of `omp-core`, `omp-builder` and `omp-app` has been read, and the findings are in the [audit findings](audit-findings.md) with what was done about each. No finding marked a bug is left open without the owner's say.
5. **Performance.** Each of 5.8–5.12 has before and after numbers on the dev machine and the laptops it concerns, against the targets below, with what was taken and what was not and why. Targets, from the misses phases 1–4 accepted:
   - first search from a cold USB 2 stick: **1 s** (now 1.1–1.3 s); start-up from the stick back to 3.9's 2.6 s (now 3.5 s);
   - first route from a cold USB 2 stick: **3 s** (now 6–11 s), and the 2.5–2.8 s that cold long routes lost between 3.9 and 4.11 explained;
   - coast to coast with directions on the E6430: **1.25 s** warm (4.11: Miami–Seattle 0.92 s, on its charger; 2.5 s in 3.9), kept on the laptops when off the charger;
   - RSS on the North America flight: **300 MB** on the dev machine (now 329 MiB); Windows DX12 measured again (541 MiB);
   - printing: **+100 MiB** at most at 300 and 360 dpi on every machine, 600 dpi without the GL cap, or the cap justified;
   - the pack: the size levers in roadmap §4 measured; whether to rebuild North America with one is decided when a step next touches the packs (decision 9).

   These targets are goals to aim for, not gates (decision 11): a miss is measured, written up and accepted or not by the owner, as in phases 3 and 4.
6. **No regression.** Frame p95, idle RSS and idle CPU stay as in 4.11 (1.30 ms, 329 MiB, 0.5%) or better with the default style; the binary stays under 30 MB; `cargo tree` has no network crate; every check of phases 2–4 that a change touches still passes (search accuracy sets, `route --check`, the route set, criterion 4's schedule tests).

## Status

| Step | Status | Notes |
|---|---|---|
| 5.1 Audit: bugs and design | **Done** 2026-11-02 | 10 bugs fixed (B1–B10); every finding triaged; fix sessions A–F done; the rest assigned to later steps |
| 5.2 Audit: performance baseline and profiles | Done 2026-11-02 | Baseline and all profiles done (the North America build, P20), every lead triaged by the owner and grouped into sessions G–L ([perf-findings.md](perf-findings.md#order-of-work)); session G (still frames) done 2026-11-02 |
| 5.3 Settings window | **Done** 2026-11-02 | Every value in a window of its own, tested headless and by hand on Linux and Windows; config read a value at a time and clamped |
| 5.4 Map styles in the Map tab | **Done** 2026-11-02 | Default, Night, High contrast, `style.toml` and `styles/*.toml` listed as cards, switched in 5–13 ms, remembered; overlays from `[overlay]` |
| 5.5 Dark UI | **Done** 2026-11-02 | Dark UI as its own setting (as the system / light / dark), apart from the map style; palette in `theme.rs`, system theme read on Linux and Windows; checked by hand by the owner on both |
| 5.6 High-contrast style and black-and-white printing | **Done** | 2026-11-02 |
| 5.7 UI polish | **Done** 2026-11-02 | Shortcuts table and F1 window, tooltips, window state, first-run screen, exFAT note, plain messages, print Cancel; checked by the owner on Linux |
| 5.8 Perf: cold reads from the stick | Deferred 2026-11-03 | Owner: cold reads do not feel slow in use. P8 (the routing top's warm-up) tried and left: see [perf-findings.md](perf-findings.md); P9, P16 and the regression hunt not taken |
| 5.9 Perf: long routes and directions | Deferred 2026-11-03 | Owner: routing is fast enough in use, and the segment cache stays as it is rather than spend RSS; P10 not taken |
| 5.10 Perf: memory | Not started | |
| 5.11 Perf: printing | Not started | |
| 5.12 Perf: builder and pack size | Not started | |
| 5.13 Final checks on every machine | Not started | |

## Environment notes

As in [phase 4](phase4-plan.md#environment-notes): builder data in `$OMP_WORK`, `target/` on `/`, packs in `$OMP_WORK/packs/`, North America from 4.5 (format 0.6, 9,910.4 MiB in 3 chunks) as the main pack, Delaware and Ohio for quick loops. Benches on the dev machine, warm and cold from the FAT32 USB 2 stick; the owner runs the Latitude 7490 (Debian/Vulkan and Windows 11/DX12) and the Latitude E6430 (Windows 10/GL) from the stick, as in 4.11. Logs go in `$OMP_WORK/measure/p5/<step>/`.

No format change is planned. A North America rebuild takes about 37 min, plus 85 min to copy to the stick; whether a size lever is worth one is decided when a step next touches the packs (decision 9).

---

## 5.1 Audit: bugs and design

Read the whole project, crate by crate, with fresh eyes now that every part exists. The point is to catch what step-by-step work misses: code that was right when written but not after later steps, assumptions that only hold for the test packs, and structure that grew one feature at a time.

- **Order:** `omp-app` first (most code changed most recently, and 5.3–5.7 will touch it), then `omp-core`, then `omp-builder`. In the app, the largest files first: `app.rs` (2,130 lines), `printui.rs` (1,467), `routing/mod.rs` and `routing/panel.rs`, `labels.rs`, `pins.rs`, `trips.rs`, `style.rs`, `render.rs`, `snapshot.rs`.
- **What to look for:**
  - *Bugs:* panics on user data or damaged files (`unwrap`, indexing, slicing on byte offsets in names), errors swallowed or shown as a debug string, races between the UI thread and the search, routing and print workers (a stale result shown after a newer request), cancellation that does not cancel, files written partly (config, trips, pins: written to a temporary file and renamed?), paths that break on a read-only stick or another drive letter, time zones and DST in the schedule's UI, and the GDI module's `unsafe`.
  - *Design:* modules that do too much (`app.rs` holds the frame loop, the panel, dialogs and pack opening), state kept in two places, duplicated helpers, constants that phase 5 needs to be settings or style values (zoom rate, friction, colours), dead code and placeholders (`later_in`), and names or comments that no longer match the code.
  - *UI consistency:* the same thing called by two names, buttons that look or behave differently on different pages, and messages that are not plain language (feeds 5.7).
- **Output:** each finding goes in [Audit findings](audit-findings.md) with where it is, what goes wrong (for a bug, the inputs that trigger it), how bad it is (bug / risk / design / cosmetic), and what is proposed: fix now, fold into a named step of this phase, or leave with a reason. Clear bugs with small fixes are fixed as found, each in its own commit with a test where one fits. Design and architecture problems are fixed in this phase too (decision 10); the larger ones are triaged with the owner first, for order and scope.
- **Splitting `app.rs`** is likely to be one finding. If so, it is done before 5.3, since the Settings window, style switching and the dark UI all touch it: the frame loop and map in `app.rs`, the panel, the dialogs (open, verify, about, notices) and pack opening each in a module of their own, as the refactor between phases 2 and 3 split search.

**Done when:** every module of the three crates has been read, the findings are written up and triaged with the owner, and the small bug fixes are in.

> **Done 2026-11-02.** The fix sessions A–F of the [order of work](audit-findings.md#order-of-work) are in, and B10 (found while fixing R7). The builder's tools named below are not read line by line: the owner closed the step without that read (2026-11-02), as they are tools rather than build stages. What is left of the findings belongs to other steps: R9 and R12 with the next rebuild; R5, R6 and D4 in 5.3; D5, D13 and B4's style signature in 5.4 and 5.5; R4, D7–D9 and D6's warnings notice in 5.7; D3, D14 and P1 after 5.2's measurement; R10's SHA-256 check, P4 and P5 in 5.12. D6's hidden console is still to be run on Windows.
>
> **2026-10-30.** Three passes done, covering the app, `omp-core` and the builder's stages (what each pass read, and what it skipped, is in [audit-findings.md](audit-findings.md#passes-so-far)). Bugs B1–B9 are fixed, with tests where one fits (and B10, found 2026-11-02 while fixing R7); the other findings (R, D, C and the performance leads P) are in the same file. Not read line by line: the builder's tools (`fetch/{manifest,regions,sources}` and others), which the next pass covers; `measure` and `report` were removed with D16.
>
> **Left:** the owner's triage is complete (every finding decided 2026-10-30, R9–R12, D17–D19, P4–P5 and C2 as proposed); then the fixes, in the sessions of the [order of work](audit-findings.md#order-of-work): A (R8, R1, R3; done 2026-11-02), B (R2; done 2026-11-02), C (the split of D1 before 5.3, with D2, D6, D10–D12; done 2026-11-02), D (the builder cleanup D16–D17, D19, C2, R10, R11; done 2026-11-02), E (the D15 and D18 helpers; done 2026-11-02) and F (R7 and the scale bar; done 2026-11-02). Every session is done; what the audit still owes is the line-by-line read of the builder's tools named above (R6 is done in 5.3; R9 and R12 change the pack, so they wait for the next rebuild, decision 9; P4 and P5 go to 5.12).

## 5.2 Audit: performance baseline and profiles

Measure before changing anything, and look for slow and wasteful code that the per-phase checks did not cover because each measured only its own features.

- **Baseline:** rerun 4.11's flights on the dev machine (`north-america.toml`, `north-america-p4.toml`, `na-print.toml`, `ohio.toml`), warm and cold from the stick, on the current `master`. These numbers, not 4.11's, are what 5.8–5.12 compare against, so a difference caused by the machine or the stick is not credited to a change.
- **App profiles** (`perf` with the release build's debug info, and `OMP_LOG=info` timings):
  - *a frame:* where the 1.3 ms goes (tile decode, tessellation, label placement, egui), and allocations per frame (a counting allocator in a bench build). A frame with nothing moving should allocate nothing.
  - *idle:* what wakes the app at 0.5% CPU with nothing happening: repaint requests, timers, workers polling.
  - *start-up:* each stage from `main` to the first full frame, cold from the stick; why it went from 2.6 s (3.9) to 3.5 s (4.11).
  - *search:* a keystroke cold and warm; the first search's reads (which pages of which sections).
  - *routing:* the first route cold (pages read, in order, by section, with `OMP_LOG` and `strace -e pread64` or `fincore` on the chunks), a warm coast-to-coast route and its directions, and 25 stops in a poor order through *Optimize*.
  - *printing:* RSS over time for one page at 300, 360 and 600 dpi.
  - *memory:* what makes up the 329 MiB peak (heap by owner with a counting allocator or `heaptrack`; mapped pack pages; GPU buffers).
- **Builder profile:** `perf` over a North America `build --reuse-stages` and the address merge on Ohio (refactor R4.5, left open: it is 13.6 of the 36.7 minutes).
- **Output:** a table of leads in [Performance findings](perf-findings.md) (where, what it costs now, the estimated gain, the effort), sorted by gain over effort, each assigned to one of 5.8–5.12 or left with a reason. Leads already known go in the table too, so everything is ranked together.

**Done when:** the baseline is recorded, each profile above has been run once, and the leads table is written and triaged with the owner.

> **Done 2026-11-02** (commits `c97cd87`, `65cbbbe`, `5f040b2` and this one). Everything is in [perf-findings.md](perf-findings.md), with the logs in `$OMP_WORK/measure/p5/5.2/`. The bench gained two hooks for it: a `count-allocs` feature (a counting allocator per thread, `allocs.rs`), so reports give the UI thread's allocations per frame, and the idle period's repaint causes (`idle_wakes`); and a `profiling` build profile (release with symbols).
>
> - **Baseline** (dev machine, current `master`): warm as in 4.11 (frame p95 1.33 ms, RSS 328 MiB, idle 0.6%, coast to coast 342 ms); cold from the stick start-up 3.47 s, the first route 10.2 s (6.2 s once start-up has settled), the first search's answer 1.04 s.
> - **Start-up and the first reads are FAT chain walks:** on a freshly mounted FAT32 stick, the first read at an offset deep in a chunk walks the file's cluster chain one 512-byte FAT sector at a time (about 1 s per 1.3 GiB of offset). Two such reads, the section table at the end of the last chunk and the tile index 2 GB into `.001`, are 3.3 of start-up's 3.5 s (P6, P7). 3.9's pack had the same layout, so this does not by itself explain the regression from 2.6 s.
> - **The cold first route is geometry:** 2,346 random `RGEO` reads, 4.5 of 6.2 s (P9); asked as the pack opens it also waits for start-up and the routing top's warm-up, which holds up the tiles too (P8). Two cold searches read 162 of `SENT`'s 196 MiB (P16).
> - **Warm:** decoding segment blocks is 80% of routing and directions (P10); label placement runs every frame, still or not (22% of the UI thread, P11); a still frame allocates 561 times (P13); the only idle wake-up is a focused field's cursor blink (P15). Heap at most about 150 MiB of the 328 (P17). Print: 600 dpi +243 MiB, the 749-step directions 14.1 s (P18, P19). P1 measured and left; D3 and D14 in the audit file measured.
> - **Builder:** the Ohio merge profiled (P4: reading the sources dominates, not `intern_names`).
> - **Triage (owner, 2026-11-02):** P6 and P7 left: the chain walk is FAT32's alone (on exFAT a read 4 GB into a chunk is one device read, measured on the data disk), so the README recommends exFAT sticks (5.7) and FAT32 stays supported, slower on first reads; the FAT32 chunking costs nothing measurable and stays. Routing (P8–P10) and cold search (P16) only where easy and plain. Still frames must do nothing (P11–P15: fix, before 5.3). Memory (P17) measured without costing speed; printing's memory and banded drawing, identical to unbanded (P18, P19). The leads are grouped into sessions G–L in [perf-findings.md](perf-findings.md#order-of-work).
> - **North America build profiled (P20, 2026-11-02),** with the IDE closed, in a 56 GB scope: 2,919 s, peak 49.4 GiB, pack `27dbaceb…` (the baseline for 5.12). The merge's 810 s keep only 3.7 of 16 threads busy: the main thread is on the CPU 65% of it, in `Streets::build` (40% of its merge samples; P4's `intern_names` 11 points of that, about 55 s) and `normalize`'s per-record interning (27%), not in reading or attaching. Other single-threaded stretches: the routing graph about 105 s, the streets stage about 55 s. New leads P21 (a parallel merge, covering P4), P22 and P23 for 5.12; P5 is seconds, as estimated ([the North America build](perf-findings.md#the-north-america-build-p20)).
> - **P21–P23 taken** by the owner (2026-11-02), in session L with P4 and P5.

## 5.3 Settings window

A *Settings…* window from the ☰ menu (replacing the placeholder), saved to `config.toml` as soon as a value changes.

- **Contents**, grouped:
  - *Display:* UI size (a factor on egui's zoom factor, 75–200%, with the scale the system reports shown beside it, so the 7490 under Debian, 1.67 px/pt where Windows uses 1.25, can be brought down); label size (a factor on the style's label sizes, for the map only); anti-aliasing (MSAA 1, 2 or 4; if the renderer cannot change it in place, it says it applies after a restart).
  - *Map movement:* wheel zoom per notch (now a quarter zoom), key zoom per press, key pan speed, and pan inertia (on/off and how long it glides: `camera::FRICTION`).
  - *Units* (decision 7): miles, kilometres, both, or as the pack suggests. They set the scale bar, the route page and the directions, so they move out of the Layers page.
  - *Start-up:* reopen the last view or start fitted to the pack; reopen the last pack.
  - A *Restore defaults* button for the window's values.
- **Config file:** every field keeps `#[serde(default)]` so older files load. A file that fails to parse is renamed `config.toml.bad` (not overwritten) and the app starts with defaults and says so once. Writes go to a temporary file and are renamed into place, if 5.1 finds they do not already.
- **Rendering at a new UI size:** labels, shields and icons on the map are drawn in points, so a new UI size must re-place labels and may need the glyph atlas at a new size; check that this does not re-tessellate tiles.
- **Checks:** a headless panel test that changes each value, saves, loads it again and gets the same config; by hand on the 7490 under Debian (UI size 75%) and on Windows.

**Done when:** every value above is in the window, takes effect as stated, survives a restart, and a damaged `config.toml` is kept and reported.

> **Done 2026-11-02** (commits `c2b3017`, `7f4ac98`, `fb9f3c4`; checked by hand by the owner on Linux and on Windows, where the release exe also starts without a console window, D6). Code: `app/settings.rs` (the window, opened from ☰ → *Settings…*), `config.rs` (`Display`, `Movement`, `Start`; `Config::parse`, `clamp`).
>
> - **After the first test on the laptops (2026-11-02):** the size sliders go down to 50%; the window is a window of its own (an egui immediate viewport, `show_viewport_immediate`, which falls back to a window inside the map's where the platform has no second window), with each section in a framed group with a title and a line under it, and each setting explained in small type (anti-aliasing most of all).
> - **Window:** UI size 50–200% (the slider applies when let go, as the window changes size under it; shows the system's px/pt beside it; egui's own Ctrl+plus and Ctrl+minus are kept in step with it), label size 50–200%, MSAA 1×/2×/4× (read when the window is made, so "applies after a restart"; `render::samples` now lets 2 through, D4), wheel zoom per notch, zoom per key press, arrow-key pan speed, pan inertia on/off and a glide time, units (the Layers page lost its units), reopen the last view and pack, and *Restore defaults*.
> - **Glide** is now "seconds to slow to a hundredth of its speed" (default 0.8 s, friction 0.0039 against the old constant 0.004); `Motion::step` takes the `Movement`.
> - **Label size** is a factor on the placed labels' size (`Placer::label_scale`, part of the frame's key). Tiles are not tessellated again: `prepare_tile` knows neither pixels per point nor the factor, and the glyph atlas draws at any size (the slot's em). The printed map keeps its own labels.
> - **Config (R5, R6):** read a value at a time, so one bad value loses only itself and the notice names it; a file that is not TOML at all is renamed `config.toml.bad` and the notice says so once (it is now loaded once, in `main`, since the first load renames the file). Every number is clamped to its control's range on reading, the schedule's and the print copies among them (R6). Writes already went through a temporary file (`files::write_bytes`).
> - **Tests:** the headless window test (draws the window, saves and loads every changed value, restore defaults), and the config's: a damaged file kept, one bad value, clamping.
> - **Left:** nothing. The 2× MSAA choice carries a note that not every GPU can do it.

## 5.4 Map styles in the Map tab

Built-in styles chosen in the side panel's Map tab (the Layers tab renamed, decision 6), switched at runtime.

- **Tab:** renamed **Map** (decision 6), with the style first, as a list of small cards (name and a swatch of land, water and a main road), then the layer toggles below. Units move to the Settings window (5.3).
- **Built-in styles,** compiled in as TOML like today's:
  - *Default:* today's `style.toml`.
  - *Night:* dark land and water, roads in muted warm colours with the hierarchy kept, light labels with dark halos. Tuned on screen at night brightness on the 7490 and the dev machine.
  - *High contrast:* white land, no land cover fills (or very light), black or dark grey roads with widths kept, strong labels, the route line thick and dark with a white casing. For anyone who wants a plainer, sharper map on screen, and the one to pick before printing in black and white (5.6). It is a style like the others, not a print setting; the print page has its own style choice (5.6, decision 8).
- **User styles:** `style.toml` next to the executable is listed as "Custom (style.toml)", as are any `styles/*.toml` next to it (proposed: lets a user keep several). `--style` still forces one. A style that fails to load is listed greyed out with its error as a tooltip. Debug builds still reload the selected file on change.
- **Switching:** compile the new `StyleFile` against the open pack's schema and swap it in. Measure what has to be redone: paints and widths are uniforms; tiles are tessellated per style (line widths, which classes are drawn), so the visible tiles are re-tessellated and the cache dropped. Target under 0.5 s on the dev machine at a North America view; a progress cue if it can be longer on the E6430.
- **The overlays from the style:** the route line and its casing, alternatives' grey, stop markers (start, via, end), pins, the print rectangle and the "what's here" marker get an `[overlay]` table in the style, with today's colours as the default, so the night and high-contrast styles can change them.
- **Map tuning is dropped** (owner, 2026-10-29): road widths and colours look right as they are. Anything the audit or the night and high-contrast styles turn up about label density or POI and shield priorities is fixed in the style it concerns.

**Done when:** the three built-in styles and a user `style.toml` are listed in the Map tab, switch at runtime within the target, are remembered across a restart, and the overlays follow the style.

> **Done 2026-11-02** (commit: this one). Code: `styles.rs` (the list and ids), `style.rs` (`Overlay`, `[overlay]`, `name`, `fade_to`, `[colors] text`/`halo` as label defaults, `StyleFile::swatch`), `data/night.toml`, `data/high-contrast.toml`, `side::map_ui` (the cards), `App::set_style`/`apply_style`, `Config::style`.
>
> - **Tab:** *Layers* is now *Map*: a row of cards (a swatch of land, water and a main road, and the name), then the layer toggles in two groups (`Layers::groups`, which replaces the heading placed by index, D13). A style that does not load is greyed with its error as the tooltip. Units had already moved (5.3).
> - **Styles:** ids `default`, `night`, `high-contrast`, `custom` (`style.toml`) and `custom:<file>` (`styles/*.toml`, named by its `name` or its file name); `--style` adds a "(--style)" card for the run and is not saved. An unreadable or missing choice in the config opens the default, and says so when the file is there but broken. The built-in Night and High contrast are the default's rules with other colours (generated once from it, then kept as plain files), and a test checks that they draw the same classes, labels, icons and shields. **`style.toml` next to the executable no longer replaces the default by itself:** it is a card to choose (the owner's own copy beside `target/release`, if there is one, must be picked once). `--render` still reads `--style`, else `style.toml`.
> - **Switching** compiles the style against the pack's schema and starts a new loader and cache, as the debug reload did. Measured with the new `bench/styles.toml` (a `style` key on a flight step switches as the step begins; `$OMP_WORK/measure/p5/5.4/styles.json`), warm, North America pack, dev machine: back to detail in **5–13 ms** at North America z4, Columbus z11 and z14 (target 0.5 s), RSS unchanged. Cold from a stick the tiles are read again, as any view is. No progress cue is needed at this speed; E6430 not measured.
> - **Overlay:** `[overlay]` holds the route's, restricted stretch's, picked step's, alternatives' and connector's colours, the halo, the route, picked, alternative and halo widths, the three stop markers, the "what's here" dot and the print rectangle; each left out keeps today's value. The route lines are GPU paints taken from the compiled style (so the print page, which compiles its own, follows it); the egui parts read `style::overlay()`, set when the style changes. **Pins keep their own colours:** the user chose them, and they carry meaning.
> - **B4:** the print preview's signature includes a hash of the style file, so a switch redraws it.
> - **Looks:** Night is dark slate land and water with muted warm roads (hierarchy kept) and light labels with dark halos; High contrast has white land, near-white tints, black to grey main roads with the widths kept, black labels with white halos, dark icon discs, and the route as wide as in the other styles (owner, 2026-11-02), dark blue with a white halo. Both seen at z12 and z15 on Ohio. The black-and-white printout is 5.6's check.
> - **Left:** the UI stays light with Night (5.5); (Night's tuning on the 7490 dropped by the owner, 2026-11-02); a route drawn in the high-contrast style not yet seen (the test packs on the data disk have no routing section).

## 5.5 Dark UI

The roadmap: "The UI follows a dark style."

- **Changed 2026-11-02 (owner):** the dark UI is a setting of its own (*Settings → Display → Theme*: As the system, Light, Dark; default As the system), **not tied to the map style**. Any style goes with either UI; the styles do not declare `ui`.
- **Following the system:** Windows: winit reports it and egui passes it on, including changes while running. Linux: winit 0.30 reports nothing on X11 or Wayland, so `systheme.rs` reads the desktop's settings portal (`org.freedesktop.appearance color-scheme`, set by GNOME, KDE and others) with `gdbus`, falling back to `gsettings`, every 3 s on a thread; no D-Bus library is linked. A desktop with neither tool, or with no preference set, gets light.
- The app's own colours (`INK`, `WEAK`, `APPROX`, `ACCENT`, error red, the fills of selected rows, notes and the boxes over the map) are functions in `theme.rs` that give a light or dark value by the theme `theme::apply` last set. The scale bar, coordinates and credit line sit in boxes with their own (see-through) fill, so they follow the UI theme, not the map's colours. Marks drawn on the map (pins, route, stops) keep their colours. The printed pages are always light (`PAPER_*`, `theme::style`).
- **Checks:** every page, dialog and popup by hand with the dark UI, and contrast of text and weak text against the panel at least 4.5:1 and 3:1 (a unit test for both themes).

**Done when:** the UI is dark or light by the setting or the system, whatever the map style, with no hard-coded UI colour left outside the palette.

> **Done 2026-11-02** (commit `f5dccaa`; checked by hand by the owner on Linux and on Windows). Code: `theme.rs` (palette functions, `apply`, `wants_dark`, contrast test), `systheme.rs` (Linux: settings portal via `gdbus`, then `gsettings`; Windows: egui's system theme), `Config::display.theme`, the Theme row in `app/settings.rs`. Open: none. Not seen in dark on the E6430.

## 5.6 High-contrast style and black-and-white printing

The print page has its own style choice (changed 2026-11-02, owner: decision 8), so a user can print in high contrast without switching the whole map.

- **Style** row on the print page (Map content only; the directions page has no map): *Same as the map (<name>)*, the default, or any style that loads. Kept in `config.toml` (`[print] style`, empty = the map's) so a user who views in one style and prints in another does not have to set it each time. A style that has gone falls back to the map's. It changes the preview (the signature hashes the style), *Print* and *Save as PDF*, and nothing on screen.
- Black and white: the printer's own grey output is used; no separate dithering. The check is 4.10's open one: the owner prints a route on the ET-2400 in black and white from Linux and Windows, with the default and the high-contrast style, and checks the route, roads and labels are readable with the high-contrast one.

> **Done 2026-11-02** (commit `a59d3b9`). The Style row is in (`styles::for_print`, `print::Settings::style`, `printui::Around`), with a test. The owner printed a route in black and white on the ET-2400 and found it good, and saw a route drawn in the high-contrast style on screen; both fine. Open: the check was done on one system (not stated which); the other, and the E6430, go in 5.13's hand checks.

**Done when:** the preview, *Print* and *Save as PDF* follow the style chosen on the print page, and the black-and-white printout with the high-contrast style passes the owner's check.

## 5.7 UI polish

- **Keyboard shortcuts:** a window from ☰ (replacing the placeholder) and F1, listing every key the app handles, generated from one table in the code so the list cannot drift from what the keys do. The tooltips that mention keys read from the same table.
- **Tooltips:** every icon-only button and every control whose label is not self-explanatory (found in 5.1's UI pass) gets one.
- **Window size and position** saved in `config.toml` on exit and restored on start, clamped to a monitor that still exists (a laptop off its dock). Maximized state too.
- **First run:** when no pack is found, the window says what a pack is, where the app looked (`maps/` next to the executable, the last pack), and offers *Choose a pack…*, instead of an empty map. Same when the last pack is gone (the stick at another drive letter is already handled).
- **exFAT sticks:** the README says to format the stick exFAT: on FAT32 the first read deep into each pack chunk walks the file's cluster chain, seconds on a USB 2 stick (5.2 P6, P7). FAT32 keeps working, slower to start.
- **Plain error messages:** every error a user can see is written as what happened and what to do, not an `io::Error` debug string (list from 5.1). Errors from a damaged pack say to run *Verify pack*.

**Done when:** each item works on Linux and Windows, checked by hand, and the shortcut list matches the keys (a test that each table entry has a handler).

> **Done 2026-11-02** (commit `7d316fd` and the one after it). All six items are written, with R4, D7–D9 and D6's notice from the audit; `cargo fmt`, `clippy` and the tests pass. Checked by the owner on Linux; the Windows check was waived (owner, 2026-11-02), so it goes in 5.13 if at all.
> - **Keys:** `keys.rs` holds the one table (`Command`, `TABLE`); handlers call `keys::take(ctx, Command::X)`, the ☰ menu, the tooltips and the F1 window (☰ → Keyboard shortcuts…, replacing the placeholder) read the text from it. Tests: every command has one entry, no key twice, each entry's keys are taken, each entry has a handler in the source (the arrows are held, so `ARROWS` and `arrow_down`). The keys inside the search box (↑ ↓ Enter) are listed apart in `LIST_KEYS`.
> - **Tooltips:** every icon button already had one; added to the tabs, *Fit to route*, *Use the view*, *Add pushpin*, *Save elsewhere…* and *Restore defaults*.
> - **Window:** `[window]` in `config.toml` (`size`, `pos`, `monitor`, `maximized`), kept while the window is open and restored at start; read and clamped like the other values. egui reports only the monitor the window is on, not the others', so the check is: if the monitor's size at the first frame is not the one saved (a laptop off its dock), or the window is bigger than it, the window is cut to fit and centred (`WindowState::fit`, tested). A window left off-screen on a monitor of the same size as before is not found. Wayland gives no position; the size and maximized state are still kept.
> - **First run:** with no map open the window says what a map is, where it looked (`maps/`, the last map and whether it is still there) and has *Choose a map…*.
> - **exFAT:** in the README.
> - **Plain errors (D7):** `files::io_text` for file errors; search, routing, "what's here", optimize, route and alternatives errors say what happened and what to try, and the two that mean a damaged map point to *Verify map*; map open errors name the problem in words; *Verify map* names the damaged parts as "the routing", "the map drawing" and so on rather than section IDs (`verify::damaged_parts`, tested).
> - **One word (D8):** "map" in everything users read (menu, dialogs, Settings, errors, style warnings); "pack" stays in code, `--pack` and `.omp`.
> - **Delete (D9):** trips and sets keep the first click turning the icon into "Delete?"; a pushpin's *Delete pushpin* button now does the same ("Delete pushpin?"), tested.
> - **Cancel (R4):** a *Cancel* button beside "Drawing the pages and sending them…", which stops the printout before its next page (the one being drawn finishes); leaving the print page does not stop it (owner 2026-11-02), so a printout goes on while the user works elsewhere; the status says "Cancelled; nothing was printed." Not run against a real printer.
> - **Style warnings (D6):** a notice lists up to three, with how many more, when a style names layers or props the map lacks.

## 5.8 Perf: cold reads from the stick

The first search (1.1–1.3 s against 1 s), the first route (6–11 s against 3 s) and start-up (3.5 s, up from 2.6 s in 3.9), from a cold FAT32 USB 2 stick at about 21 MiB/s.

- **First explain the regressions** that 4.11 left: cold long routes 2.5–2.8 s slower than in 3.9 on the same routing bytes, and start-up 0.8 s slower. Candidates: the pack's layout on the stick after copying it afresh (chunk fragmentation on exFAT/FAT32), `TZON` or other sections read at open, or the app reading more at start since phase 4 (trips, pins, the print page's printers). 5.2's read traces against a 3.9 build on the same stick answer it.
- **Read order:** the routing reader's first query touches pages all over the CH; reading the hot parts (the top levels of the hierarchy, the segment index) as a few large sequential reads at the first route, or in the background after start-up, may cost less than the random reads. Measure the bytes involved first: if the hot set is tens of MiB it fits; at 21 MiB/s, hundreds do not.
- **Background warm-up after start-up** (a roadmap lead): read the hot search and routing pages at low priority once the first frame is up, stopping as soon as the user does anything that reads the stick, so it never delays a frame or a keystroke. Measured on the stick: start-up, the first frame and keystrokes must not get worse.
- **Search:** the first search's reads from 5.2; the same treatment if they are scattered.

**Done when:** the regressions are explained (and fixed if they are the app's), and the first search, first route and start-up from the stick are measured before and after each change taken.

> **Deferred 2026-11-03** (owner: nothing here feels slow when running the app). The one quick win tried, P8, did not pay: reading the routing top in 128 KiB pieces changed nothing on the stick, and dropping the warm-up trades a faster first jump (1.0 to 0.71 s) and an at-open route (6.36 to 5.62 s) for a slower route asked after start-up (3.5 to 3.95 s). Numbers in [perf-findings.md](perf-findings.md) (P8); no code changed. Left open: the start-up regression, P9 and P16.

## 5.9 Perf: long routes and directions

Warm coast to coast is 0.35 s on the dev machine and 0.9–1.5 s on the laptops; 25 stops through *Optimize* until the reordered route is shown take 2.2 s on the dev machine and 8.5–14.8 s on the laptops (the route alone is 5.3 s on 25 long legs on the dev machine); alternatives coast to coast 5–9 s on the laptops.

- **Show the route's line before its directions** (roadmap lead): the trip's line and totals as soon as the legs are unpacked; the directions fill in after. The user sees the route in about half the time.
- **Directions decode each junction's cell** (phase 3 step 3.5): two thirds of the E6430's coast-to-coast time was directions. Cache decoded cells across a leg's junctions, or decode only the data the guide needs.
- **Legs in parallel** (to check): if a trip's legs are routed one after another, routing them on a few threads would cut 25 stops' 5.3 s, at the cost of a routing cache per thread (5.10). Check how the routing worker runs legs first.
- **Alternatives:** their directions are most of their time (4.2: decoding their lines and naming their steps); the same cell cache helps.

**Done when:** each change taken is measured before and after on the dev machine and, for the final numbers, the E6430 and the 7490.

> **Deferred 2026-11-03** (owner: routing is decently fast; no RSS is to be spent on a larger segment cache). Nothing taken; P10 and the lines above stay as leads.

## 5.10 Perf: memory

RSS on the North America flight: dev 329 MiB (target 300 MB), 7490 Windows DX12 541 MiB, 7490 Debian 325 MiB; E6430 GL 4,218 MiB from the driver's leak per frame presented.

- **Known leads:** routing caches of 8 + 8 MiB (−21 MiB), search's 41 MiB of readers, and whatever 5.2's heap profile shows. Each lead measured for what it saves and what it costs in time (a smaller cache may cost the cold route).
- **Windows DX12:** what the extra 200 MiB over Vulkan is (driver-side staging, the swapchain, egui's textures); try the settings wgpu exposes for it (buffer and staging sizes, present mode).
- **E6430's GL leak:** one time-boxed look (half a day): whether it depends on the present mode, the swapchain format, or a wgpu or driver setting, and whether a newer wgpu fixes it. If not, it stays the known upstream issue it was accepted as in phase 1.

**Done when:** each lead taken is measured before and after, and the RSS on every machine is recorded.

## 5.11 Perf: printing

After 4.11's fix the dev machine is within +100 MiB at 300 and 360 dpi (+82 and +96 MiB); 600 dpi is +330; DX12 was +965 before the shared device and has not been measured since; GL prints are capped at 360 dpi.

- **Stream the map's own readback into the page**, as the overlay already is (about 26 MB at 300 dpi).
- **Banded drawing** for pages above 360 dpi: draw the page in horizontal bands, each read back and written into the page before the next. Lowers 600 dpi's peak and keeps each GPU submit short, which may also lift the GL cap: the cap exists because one long submit holds wgpu's GL context for over a second and the window thread then panics ("Could not lock adapter context").
- **That panic itself:** look at whether the window thread can wait for the context instead of panicking (a wgpu setting, a newer wgpu, or splitting the submit); if banding fixes the cause, the cap goes.
- **DX12:** measure again on the 7490 under Windows with the shared device.
- **Directions pages:** 749 steps at 360 dpi take 8.6 s and +177 MiB on the dev machine; pages are drawn one at a time, so look at why memory grows with the page count.

**Done when:** printing is within +100 MiB at 300 and 360 dpi on every machine, 600 dpi is drawn in bands (and the GL cap lifted or kept with its reason), and the numbers are recorded.

## 5.12 Perf: builder and pack size

- **Address merge** (refactor R4.5, left open): profile it on Ohio and North America and take what is clear; it is 13.6 of the 36.7 minutes.
- **Pack size levers** (roadmap §4): the smallest ponds and streams (waterways are 1.1 GiB), V5 tile groups (8% smaller tiles, about 2.5× the bytes read per viewport), and land cover capped at z12 (about 0.5 GiB, but small patches drop). Measure each on Ohio with before and after screenshots and the pack's size, and write the numbers here. Whether to rebuild North America with one is left until a step next touches the packs, when the owner decides by the time available then (decision 9).
- Whatever 5.2's builder profile shows.
- **Inputs checked against `fetch`'s manifest** (R10, owner 2026-11-02): `build` compares each input's size and SHA-256 with `inputs/manifest.toml` and stops on a mismatch, naming the file ("run `fetch` again"). The size check is free; the hash re-reads the inputs (37 GB for North America), so measure it, overlapped with the build's own reads, before taking it into every build. In place before the next rebuild.

**Done when:** the address merge has been profiled and any change taken is measured; the size levers are measured on Ohio and written up.

## 5.13 Final checks on every machine

As 4.11, for this phase:
- the bench flights on the dev machine (warm and cold from the stick), the 7490 (Debian and Windows) and the E6430, with a step that switches styles and one that prints with the high-contrast style;
- exit criteria 1–6 measured on each, with any miss written up for the owner;
- hand checks from a `README.txt` on the stick: the Settings window, styles and the dark UI, the first-run screen (a stick with no `maps/`), window position on a laptop off its dock, and a black-and-white printout;
- the binary's size and `cargo tree`.

**Done when:** every exit criterion is measured on every machine, with any that miss written up for the owner.

---

## Order of work

```
First:  5.1 audit: bugs and design ─┐
        5.2 audit: perf baseline ───┴─► owner triages the findings and leads
Then:   (split app.rs if 5.1 says so) ─► perf session G (still frames) ─► 5.3 settings ─► 5.4 styles ─► 5.5 dark UI ─► 5.6 high contrast + B&W ─► 5.7 polish
Beside: 5.8 cold reads · 5.9 routes and directions · 5.10 memory · 5.11 printing · 5.12 builder and pack
Last:   5.13 checks
```

- The audits come first because they decide what the later steps fix and in what order, and because the baseline must be taken before anything changes.
- 5.3 before 5.4: the style choice is a setting, and the label size setting changes how styles are applied. 5.5 needs 5.4's style list; 5.6 needs 5.4's high-contrast style.
- The performance items are independent of the UI steps and of each other, and can be taken in the order the [leads table](perf-findings.md#leads) ranks them.
- The laptops are needed only for 5.10's and 5.11's machine-specific items and for 5.13; the owner's runs can be batched.

## Audit findings

The bugs and design findings are in [audit-findings.md](audit-findings.md) (5.1), and the performance baseline, profiles and leads in [perf-findings.md](perf-findings.md) (5.2); the fixes are worked through from there.

## Decisions

Proposed while planning (2026-10-29), from the owner's notes:
1. **Styles are chosen in the side panel's tab**, not in the Settings window (owner). The tab is renamed (decision 6).
2. **No map tuning step** (owner): road widths and colours stay. Label density and priorities are fixed only where 5.1 or the new styles show a problem.
3. **The performance pass is split** into 5.8–5.12 by area, each measured on its own, and preceded by an audit (5.2) that ranks every lead, old and new.
4. **A whole-project audit for bugs and design** (5.1) comes before the new UI work, so the Settings window and styles are built on fixed code.
5. **No format change** is planned. A size lever (5.12) that needs one is decided by the owner with Ohio's numbers.

Settled by the owner (2026-10-29), from draft 1's questions:

6. **The Layers tab is renamed Map** (question a): the style first, then the layer toggles.
7. **Units move to the Settings window** (question b), since they set the route page and directions as well as the scale bar.
8. **Printing has a style choice of its own** (question c; changed 2026-11-02). The high-contrast style is a style like the others, named for what it is, not "print": it is never switched to for printing by itself. The print page's *Style* row defaults to the map's and is remembered, so the user can print in high contrast whatever the map shows.
9. **A North America rebuild is decided when a step next touches the packs** (question d), by the time the owner has then; 5.12 only measures the size levers.
10. **The audit fixes design and architecture problems as well as bugs** (question e), such as splitting `app.rs`.
11. **Performance targets are goals to aim for** (question f), not gates; misses are measured and written up, as in phases 3 and 4.

12. **The app's release build unwinds on panic** (5.1 R8, 2026-10-30), replacing phase 0's `panic = "abort"`, so a crash in a worker becomes an error the UI can show. It costs 1.39 MiB (+9%) on Linux.

Open: none.

## Out of scope for phase 5

- New features: GPX, KML and CSV, fuel and rest suggestions, a larger POI set, GPS (roadmap *Later or optional*).
- Map tuning beyond what the night and high-contrast styles need (decision 2).
- A print-only style (decision 8): the print page chooses among the existing styles.
- A style editor in the app; users edit `style.toml` by hand, as today.
- U-turn work (phase 4 decision 22): stays as it is unless 5.9's work on directions makes the hidden-variant search cheap.
- macOS and anything for distribution (phase 6).

## Carried from earlier phases

- **Phase 4 4.11 left open:** the hand checks on each laptop (`README.txt` items a–d) and the ruler and black-and-white paper checks (`PRINT-TEST.txt`), only partly done: the black-and-white check is 5.6's; the rest go in 5.13's hand checks.
- **Phase 4:** the cold long-route regression since 3.9 (5.8); print memory, 600 dpi and the GL cap and panic (5.11); DX12 print memory not remeasured (5.11).
- **Phase 3:** the 7490 under Debian's UI scale (5.3); the cold-route and warm-long-route misses accepted in decision 15 (5.8, 5.9).
- **Refactor R4.5:** profiling the address merge (5.12).
- **Phase 1:** the E6430's GL leak (5.10, time-boxed).

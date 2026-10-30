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
- The print page takes the style and a text size as parameters (phase 4, "carried from earlier phases"); printing keeps using the style on screen (decision 8).
- The bench flights in `crates/app/bench/` (`north-america.toml`, `north-america-p4.toml`, `na-print.toml`, …) and their reports from 4.11 are the baseline for every performance item.

### Exit criteria (expanded)

1. **Settings.** Every setting in `config.toml` that a user would change is reachable from the UI (in the Settings window or the Map tab), takes effect without a restart (MSAA may ask for one), and survives a restart. A `config.toml` that is missing, from an older version, or damaged opens with defaults for what cannot be read, and says so once; it is never lost silently.
2. **Styles.** At least three built-in styles (default, night, high contrast) and any user `style.toml` are listed in the Map tab and switch at runtime within one frame's work plus the tiles' re-tessellation (aim: under 0.5 s on the dev machine at a North America view). The choice is remembered. The UI is dark with the night style. Printing uses the style on screen (decision 8); a black-and-white printout of a route with the high-contrast style keeps the route, roads and labels readable (the owner's ET-2400, 4.10's open check).
3. **Polish.** Each item of 5.7 is done and checked by hand on Linux and Windows.
4. **Audit.** Every module of `omp-core`, `omp-builder` and `omp-app` has been read, and the findings are in this plan's [Audit findings](#audit-findings) with what was done about each. No finding marked a bug is left open without the owner's say.
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
| 5.1 Audit: bugs and design | In progress | 2026-10-30: 9 bugs fixed (B1–B9); the app read in full, core and builder in part; findings below await the owner's triage |
| 5.2 Audit: performance baseline and profiles | Not started | |
| 5.3 Settings window | Not started | |
| 5.4 Map styles in the Map tab | Not started | |
| 5.5 Dark UI with the night style | Not started | |
| 5.6 High-contrast style and black-and-white printing | Not started | |
| 5.7 UI polish | Not started | |
| 5.8 Perf: cold reads from the stick | Not started | |
| 5.9 Perf: long routes and directions | Not started | |
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
- **Output:** each finding goes in [Audit findings](#audit-findings) with where it is, what goes wrong (for a bug, the inputs that trigger it), how bad it is (bug / risk / design / cosmetic), and what is proposed: fix now, fold into a named step of this phase, or leave with a reason. Clear bugs with small fixes are fixed as found, each in its own commit with a test where one fits. Design and architecture problems are fixed in this phase too (decision 10); the larger ones are triaged with the owner first, for order and scope.
- **Splitting `app.rs`** is likely to be one finding. If so, it is done before 5.3, since the Settings window, style switching and the dark UI all touch it: the frame loop and map in `app.rs`, the panel, the dialogs (open, verify, about, notices) and pack opening each in a module of their own, as the refactor between phases 2 and 3 split search.

**Done when:** every module of the three crates has been read, the findings are written up and triaged with the owner, and the small bug fixes are in.

> **In progress 2026-10-30.** First pass, in the order above. Read in full: `app.rs`, `printui.rs`, `routing/{mod,panel,worker}.rs`, `pins.rs`, `trips.rs`, `files.rs`, `config.rs`, `main.rs`, `packs.rs`, `loader.rs`, `verify.rs`, `side.rs`, `printer/{gdi,cups}.rs`. Read in part: `labels.rs` (extraction and placement), `style.rs` (compile), `render.rs` (paints, frame, readback), `snapshot.rs` (tile loading), `routing/times.rs`, `search/worker.rs`. `omp-core`: searched for panics on damaged data (indexing with decoded values, `unwrap`/`expect`) in `pack`, `tiles`, `tile`, `route/*`, `admin`, `tz`; the readers check bounds, and every `expect` left holds by construction. `omp-builder`: the build's stages, staging and pack replacement (`build/mod.rs`). Six bugs fixed (B1–B6 below), with tests for B3, B4 and B6.
>
> **Second pass 2026-10-30.** Read in full: the rest of the app (`here`, `search/{mod,session,ui,draw}`, `routing/{line,saved}`, `camera`, `pages`, `pdf`, `print`, `text`, `shapes`, `mesh`, `tiles`, `cache`, `pool`, `layers`, `scale`, `theme`, `logger`, `memory`, `printer.rs`, `printer/none.rs`; `bench.rs` skimmed, as a dev tool). In `omp-core`: `varint`, `zframe`, `geo`, `grid`, `blocked`, `fuzzy`, `meta`, `schema`, `whitelist`, `here`, `routes`, `street`, `addr`, `search`, `find/{mod,hits,numbers}`, `schedule`, the readers in `route/mod.rs` and the search and unpacking in `route/query.rs`; `find/{candidates,readings,score}`, `query` and `normalize` scanned for byte slicing and `unwrap` (every slice is at an ASCII digit boundary). In `omp-builder`: `main`, `build/cache.rs`, `fetch/{http,mod}.rs`, every module's head, and every `unwrap`/`expect` in production code (each holds by an earlier filter, or reads the builder's own temp data). Three bugs fixed, B7–B9, with tests for B7 (ignored: it needs a pack and a GPU) and B9 (which hung before the fix); R6–R8, D15–D16 and leads P1–P3 found. R8 matters most: the app's release build aborts on any panic, so R1 and the loader's and print worker's catches need the owner's decision on it.
>
> **Left:** in `omp-core`, `route/{guide,order,trip,alt,matrix}`, `profile`, `tile`, `tiles`, `pack`, `admin` and `tz` were searched for panics (first pass) but not read for design; the builder's stages (`pbf`, `tiles`, `street`, `search`, `addr`, `admin`, `graph`, `ch`, `routing`, `geom`) were not read line by line. Then the owner's triage of R3–R5, D2, D4–D5 and D7–D13 (R1, R2, R6–R8, D1, D3, D6, D14–D16 and P1–P3 decided 2026-10-30); then the fixes for R1 with R8 (unwinding first), R2, R7, D6, D15 and D16, and the split of D1 before 5.3 (R6 is done in 5.3).

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
- **Output:** a table of leads in [Audit findings](#audit-findings) (where, what it costs now, the estimated gain, the effort), sorted by gain over effort, each assigned to one of 5.8–5.12 or left with a reason. Leads already known go in the table too, so everything is ranked together.

**Done when:** the baseline is recorded, each profile above has been run once, and the leads table is written and triaged with the owner.

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

## 5.4 Map styles in the Map tab

Built-in styles chosen in the side panel's Map tab (the Layers tab renamed, decision 6), switched at runtime.

- **Tab:** renamed **Map** (decision 6), with the style first, as a list of small cards (name and a swatch of land, water and a main road), then the layer toggles below. Units move to the Settings window (5.3).
- **Built-in styles,** compiled in as TOML like today's:
  - *Default:* today's `style.toml`.
  - *Night:* dark land and water, roads in muted warm colours with the hierarchy kept, light labels with dark halos. Tuned on screen at night brightness on the 7490 and the dev machine.
  - *High contrast:* white land, no land cover fills (or very light), black or dark grey roads with widths kept, strong labels, the route line thick and dark with a white casing. For anyone who wants a plainer, sharper map on screen, and the one to pick before printing in black and white (5.6). It is a style like the others, not a print setting: printing always uses the style on screen (decision 8).
- **User styles:** `style.toml` next to the executable is listed as "Custom (style.toml)", as are any `styles/*.toml` next to it (proposed: lets a user keep several). `--style` still forces one. A style that fails to load is listed greyed out with its error as a tooltip. Debug builds still reload the selected file on change.
- **Switching:** compile the new `StyleFile` against the open pack's schema and swap it in. Measure what has to be redone: paints and widths are uniforms; tiles are tessellated per style (line widths, which classes are drawn), so the visible tiles are re-tessellated and the cache dropped. Target under 0.5 s on the dev machine at a North America view; a progress cue if it can be longer on the E6430.
- **The overlays from the style:** the route line and its casing, alternatives' grey, stop markers (start, via, end), pins, the print rectangle and the "what's here" marker get an `[overlay]` table in the style, with today's colours as the default, so the night and high-contrast styles can change them.
- **Map tuning is dropped** (owner, 2026-10-29): road widths and colours look right as they are. Anything the audit or the night and high-contrast styles turn up about label density or POI and shield priorities is fixed in the style it concerns.

**Done when:** the three built-in styles and a user `style.toml` are listed in the Map tab, switch at runtime within the target, are remembered across a restart, and the overlays follow the style.

## 5.5 Dark UI with the night style

The roadmap: "The UI follows a dark style."

- Each style declares `ui = "light" | "dark"`. `theme::apply` sets egui's theme from it instead of always light.
- The app's own colours (`INK`, `WEAK`, `APPROX`, `ACCENT`, `OUTLINE`, the shadow, and the ~30 colour constants in `app.rs`, `routing/`, `printui.rs`, `search/`, `pins.rs`) become one `Palette` with a light and a dark value each, read from the current theme. Text over the map (scale bar, credits) takes its colour from the style, since it sits on the map's land colour.
- **Checks:** every page, dialog and popup by hand with the dark UI (a screenshot set from a bench flight with a `style = "night"` step, compared side by side), and contrast of text and weak text against the panel at least 4.5:1 and 3:1.

**Done when:** the UI is dark with the night style and light with the others, with no hard-coded colour left outside the palette.

## 5.6 High-contrast style and black-and-white printing

Printing uses the style on screen, with no style choice of its own (decision 8). The high-contrast style of 5.4 is tuned so that a user who switches to it before printing gets a clear black-and-white page.

- The print page names the style the page is drawn with, and when that is not the high-contrast one, a short hint says it can be chosen in the Map tab for black-and-white printers. Nothing switches by itself.
- Black and white: the printer's own grey output is used; no separate dithering. The check is 4.10's open one: the owner prints a route on the ET-2400 in black and white from Linux and Windows, with the default and the high-contrast style, and checks the route, roads and labels are readable with the high-contrast one.

**Done when:** the preview, *Print* and *Save as PDF* follow the style on screen, and the black-and-white printout with the high-contrast style passes the owner's check.

## 5.7 UI polish

- **Keyboard shortcuts:** a window from ☰ (replacing the placeholder) and F1, listing every key the app handles, generated from one table in the code so the list cannot drift from what the keys do. The tooltips that mention keys read from the same table.
- **Tooltips:** every icon-only button and every control whose label is not self-explanatory (found in 5.1's UI pass) gets one.
- **Window size and position** saved in `config.toml` on exit and restored on start, clamped to a monitor that still exists (a laptop off its dock). Maximized state too.
- **First run:** when no pack is found, the window says what a pack is, where the app looked (`maps/` next to the executable, the last pack), and offers *Choose a pack…*, instead of an empty map. Same when the last pack is gone (the stick at another drive letter is already handled).
- **Plain error messages:** every error a user can see is written as what happened and what to do, not an `io::Error` debug string (list from 5.1). Errors from a damaged pack say to run *Verify pack*.

**Done when:** each item works on Linux and Windows, checked by hand, and the shortcut list matches the keys (a test that each table entry has a handler).

## 5.8 Perf: cold reads from the stick

The first search (1.1–1.3 s against 1 s), the first route (6–11 s against 3 s) and start-up (3.5 s, up from 2.6 s in 3.9), from a cold FAT32 USB 2 stick at about 21 MiB/s.

- **First explain the regressions** that 4.11 left: cold long routes 2.5–2.8 s slower than in 3.9 on the same routing bytes, and start-up 0.8 s slower. Candidates: the pack's layout on the stick after copying it afresh (chunk fragmentation on exFAT/FAT32), `TZON` or other sections read at open, or the app reading more at start since phase 4 (trips, pins, the print page's printers). 5.2's read traces against a 3.9 build on the same stick answer it.
- **Read order:** the routing reader's first query touches pages all over the CH; reading the hot parts (the top levels of the hierarchy, the segment index) as a few large sequential reads at the first route, or in the background after start-up, may cost less than the random reads. Measure the bytes involved first: if the hot set is tens of MiB it fits; at 21 MiB/s, hundreds do not.
- **Background warm-up after start-up** (a roadmap lead): read the hot search and routing pages at low priority once the first frame is up, stopping as soon as the user does anything that reads the stick, so it never delays a frame or a keystroke. Measured on the stick: start-up, the first frame and keystrokes must not get worse.
- **Search:** the first search's reads from 5.2; the same treatment if they are scattered.

**Done when:** the regressions are explained (and fixed if they are the app's), and the first search, first route and start-up from the stick are measured before and after each change taken.

## 5.9 Perf: long routes and directions

Warm coast to coast is 0.35 s on the dev machine and 0.9–1.5 s on the laptops; 25 stops through *Optimize* until the reordered route is shown take 2.2 s on the dev machine and 8.5–14.8 s on the laptops (the route alone is 5.3 s on 25 long legs on the dev machine); alternatives coast to coast 5–9 s on the laptops.

- **Show the route's line before its directions** (roadmap lead): the trip's line and totals as soon as the legs are unpacked; the directions fill in after. The user sees the route in about half the time.
- **Directions decode each junction's cell** (phase 3 step 3.5): two thirds of the E6430's coast-to-coast time was directions. Cache decoded cells across a leg's junctions, or decode only the data the guide needs.
- **Legs in parallel** (to check): if a trip's legs are routed one after another, routing them on a few threads would cut 25 stops' 5.3 s, at the cost of a routing cache per thread (5.10). Check how the routing worker runs legs first.
- **Alternatives:** their directions are most of their time (4.2: decoding their lines and naming their steps); the same cell cache helps.

**Done when:** each change taken is measured before and after on the dev machine and, for the final numbers, the E6430 and the 7490.

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
Then:   (split app.rs if 5.1 says so) ─► 5.3 settings ─► 5.4 styles ─► 5.5 dark UI ─► 5.6 high contrast + B&W ─► 5.7 polish
Beside: 5.8 cold reads · 5.9 routes and directions · 5.10 memory · 5.11 printing · 5.12 builder and pack
Last:   5.13 checks
```

- The audits come first because they decide what the later steps fix and in what order, and because the baseline must be taken before anything changes.
- 5.3 before 5.4: the style choice is a setting, and the label size setting changes how styles are applied. 5.5 needs 5.4's style list; 5.6 needs 5.4's high-contrast style.
- The performance items are independent of the UI steps and of each other, and can be taken in the order the leads table ranks them.
- The laptops are needed only for 5.10's and 5.11's machine-specific items and for 5.13; the owner's runs can be batched.

## Audit findings

*Filled in by 5.1 and 5.2.* One table for bugs and design, one for performance leads.

| # | Where | Finding | Kind | Proposed | Decision |
|---|---|---|---|---|---|
| B1 | `app.rs` `App::open` | Choosing a pack that cannot be opened (picker, typed folder) closed the pack already open first, leaving no map. | bug | Fix now. | **Fixed** 2026-10-30: the new pack is opened first. |
| B2 | `files.rs`, `config.rs` | Config, trips and pushpin sets were written to a temporary file and renamed, but not flushed first: a stick pulled out right after a save could leave an empty file. `config.rs` repeated the code and left its `.tmp` behind on a failed rename. | bug | Fix now. | **Fixed**: one `files::write_bytes`, which syncs the file before the rename; the config uses it. |
| B3 | `printui.rs`, `printer/cups.rs`, `printer/gdi.rs` | A printout that failed part way (a page that could not be drawn) still printed: closing `lp`'s input queued the partial PDF, and on Windows the document was deleted without `AbortDoc`. *Save as PDF* left the partial file. | bug | Fix now. | **Fixed**: `lp` is killed before its input closes, GDI calls `AbortDoc`, a failed PDF is removed. Test: `a_job_dropped_unfinished_is_not_queued`. |
| B4 | `printui.rs` `signature` | The preview took only the count of shown pushpins: a pin moved, recoloured or renamed (as many as before) left the preview out of date. The style is not in the signature either (only matters once styles switch). | bug | Fix the pins now; the style in 5.4. | **Fixed** (pins): each shown pin's point, symbol and name; test added. Style → 5.4. |
| B5 | `pins.rs` `delete_set` | A set being renamed is known by its index; deleting an earlier set made Rename apply to the wrong set. | bug | Fix now. | **Fixed**: deleting a set ends a rename. |
| B6 | `loader.rs` `work` | A panic while preparing a tile (tessellating bad data) killed that loader thread and left the tile "in flight" for good: it was never asked for again, and the loader had one thread fewer. | bug | Fix now. | **Fixed**: caught and reported as the tile's error, like a read error. Test: `a_tile_that_panics_is_an_error_and_the_loader_goes_on`. |
| B7 | `snapshot.rs` `draw_view`, `pool.rs` | The print worker's GPU pool never reused a range: the window's pool frees ranges given back at the end of each frame, but offscreen drawing has no frames, so every preview and printed page took new slab space and the worker's buffers grew for as long as it lived (Delaware, one view drawn 12 times: 16 MiB, then 32 MiB on the 12th). Likely part of 5.11's print memory. | bug | Fix now. | **Fixed** 2026-10-30: `Pool::idle` after the readback frees them at once. Test (needs a pack and a GPU, ignored): `pages_reuse_the_pool`. |
| B8 | `search/session.rs` `take`, `search/ui.rs` | Enter on a search that found nothing, or failed, showed nothing at all: the results panel opened only for two results or more, and Enter closes the suggestions, where "Nothing found" and "Search failed" are shown. | bug | Fix now. | **Fixed** 2026-10-30: the panel opens for none too, titled "Nothing found for “…”" or "Search failed: …"; test extended. |
| B9 | `omp-core` `schedule::forward` | A break every 0 hours (or a negative or NaN interval) never let the trip move on: each pass took a break of 0 s driving, so the times thread looped for good, adding events until memory ran out. The route page allows only 0.5–12 h, but `config.toml` and trip files are read as written. | bug | Fix now. | **Fixed** 2026-10-30: breaks more often than once a minute are taken as none. Test: `breaks_every_two_hours` extended (it hung before). |
| R1 | `routing/worker.rs`, `search/worker.rs`, `routing/times.rs` (towns) | The workers do not catch panics (only the print worker does): a panic in routing or search on unexpected data leaves the UI saying "Routing…" or waiting for results for good. | risk | Catch per job, answer with an error ("Routing failed on this route; Verify pack may help"). Small. | **Owner 2026-10-30: yes**, catch a crash and show an error. |
| R2 | `omp-core` readers | Damaged sections are only found by *Verify pack*; the readers check bounds, but there is no test that reading damaged data never panics. | risk | Add a test that flips bytes in each section of a small built pack and reads every tile, search and route, expecting errors, not panics. Second pass: the test runs unoptimized, where an overflow panics; in release it wraps (`addr.rs` ordinals, CH weights `d + o`), so check the sums that index or compare. `Routing::unpack_each` recurses once per shortcut level with no bound (North America's hierarchy is 245 deep): a damaged `RCHE` whose shortcut halves point back at it would overflow the stack, which aborts even with unwinding; give it a depth limit (1,000) with this test. | **Owner: yes**, a small test; the unpacking gets a depth limit. |
| R3 | `loader.rs` `Drop` | Switching packs (and a debug style reload) joins the loader threads on the UI thread; a tile read from a cold stick holds the window for its read. | risk | Detach the threads, as the routing worker does. | |
| R4 | `printui.rs` | A printout cannot be cancelled once started (several seconds for long directions); closing the page does not stop it. | design | A Cancel button (→ 5.7). | |
| R5 | `config.rs` `load` | One bad value makes the whole file fall back to defaults, and the file is overwritten 5 s later. | risk | Already in 5.3 (keep it as `config.toml.bad`, say so); also read field by field so one bad value loses only itself. | |
| R6 | `config.rs`, `trips.rs`, `routing/times.rs` | Values from `config.toml` and trip files are used as read; only the print text size and copies and a stop's minutes are clamped. B9 was one (a break every 0 h hung the times thread); others: a negative break length moves the clock back, and at most 0 h of driving a day makes 2,000 nights and gives no schedule. | risk | In 5.3: clamp each value to the range its control allows when the file is read, with R5. | **Owner: yes**, clamp each value to its control's range when the file is read (5.3). |
| R7 | `pages.rs` `render_directions` | The directions' first page puts its head (label, totals, a line per stop) above the rows without checking that it fits, and places the first row even when the head leaves no room: 25 stops at 140% text on Letter take more than the page's body, and run into the footer and off the page. | bug | → 5.11: rows start on page 2 when the head fills page 1; a head taller than a page is split. | **Owner: fix it**: what does not fit goes on the next page, never into the footer. |
| R8 | `Cargo.toml` `[profile.release]` | The app's release build has `panic = "abort"` (phase 0 Environment, for size), so `catch_unwind` does nothing there: B6's fix in the loader and the print worker's catch work only in tests and debug builds. In the shipped app any panic on any thread (a damaged tile while tessellating, wgpu's GL "Could not lock adapter context") ends the app at once, losing unsaved trip edits. R1 as decided needs unwinding too. | risk | Owner: build the app with `panic = "unwind"`, or keep `abort` and drop R1 and the catches. Cost measured on Linux, release build of this commit: 15.38 MiB → 16.77 MiB (+1.39 MiB, +9%); Windows not measured. | **Owner 2026-10-30: yes**, switch the app to `panic = "unwind"`: the size is no concern, and it makes R1 and the catches work. |
| D1 | `app.rs` (2,130 lines) | Holds the frame loop and map input, the side panel and its page wiring, five dialogs (picker, About, Verify, errors, notices), pack opening and the bench's glue. | design | Split before 5.3: `app.rs` (frame loop, map), `panel.rs` (side panel), `dialogs.rs`, pack opening into `packs.rs`, bench glue beside `bench.rs`. | **Owner: yes**, split before 5.3. |
| D2 | `app.rs` `start_loader`, `snapshot.rs` | A tile is turned into mesh + labels + upload in two places that must agree. | design | One `prepare_tile` used by both (with D1). | |
| D3 | `packs.rs`, workers | The pack is opened five times: tiles, search, routing, the towns thread and the print worker each `Pack::open` it, with their own handles and caches. | design | Measure in 5.2; a shared `Arc<Pack>` if it pays (→ 5.10, 5.8). | **Owner:** measure in 5.2; open the pack once, or at least fewer than five times. |
| D4 | `app.rs` `KEY_PAN`, wheel steps (0.25 per notch, `/400` per point), `camera.rs` `FRICTION`, `ZOOM_RATE`; `render::samples` | Constants that 5.3 makes settings. MSAA can only be 1 or 4 (2 maps to 4), while 5.3 offers 1, 2 or 4. | design | → 5.3. | |
| D5 | 10 files | About 70 colour literals: the error red three times, the map controls' white fills, overlay colours in `render.rs`, the label halo in `labels.rs`, pins, stop markers, notes. | design | → 5.4 (overlays from the style) and 5.5 (`Palette`). | |
| D6 | `main.rs`, `app.rs` | Style warnings and other diagnostics go only to stderr, which Windows users never see; and the Windows build has no `windows_subsystem`, so a console window opens beside the app. | design | Owner: hide the console in release builds (the bench and `--render` attach to the parent's console), and show style warnings in a notice. → 5.7. | **Owner: yes**, hide the console at least in release builds. |
| D7 | several | Errors shown as raw `io::Error` text: routing and search "could not be opened: …", pack open errors, trip and set rename and delete, *Verify pack* naming sections by ID ("TIDX does not match"); "what's here" answers a failed lookup with "Could not look: …". | design | List for 5.7's plain messages. | |
| D8 | UI text | The same thing called a pack and a map: "Open a pack…", "Could not open the map", "No map pack found", "Verify pack", "About this map". | cosmetic | Pick one word for users (→ 5.7). | |
| D9 | `pins.rs`, `trips.rs` | Deleting a trip or a set takes two clicks; deleting a pushpin takes one. | cosmetic | Same behaviour (→ 5.7). | |
| D10 | `side.rs` `later_in`, `routing/worker.rs` `Route::via`, `side::Page::ALL`, `app.rs` `open_print` | Placeholders and dead code: the disabled menu entries (5.3, 5.7), a field kept with `allow(dead_code)`, a page list used only by the bench, and `Units::Miles` passed where it is not used. | design | Remove with the steps that replace them. | |
| D11 | `packs::date`, `printui::exe_dir`, `app.rs` `mib`/`seconds` | Hand-written helpers: a civil date though `jiff` is a dependency; a second executable folder, not canonicalized as `main.rs`'s is, so *Save as PDF*'s default folder can differ from where trips go. | design | Clean up with D1. | |
| D12 | `routing/panel.rs` `note_row` | A night's day number is read back out of its text ("Day 2"). | design | Take it from the schedule's event. | |
| D13 | `side.rs` `layers_ui` | The POI heading goes before entry 5 of `Layers::entries` by index; units are titled "Scale bar units" though they also set the route page and directions. | design | → 5.4 (Map tab) and 5.3 (units). | |
| D14 | `printui.rs` `Worker` | The preview worker keeps its own open pack and GPU renderer after the print page closes. | design | Measure in 5.2 (→ 5.10). | **Owner:** measure in 5.2, with D3. |
| D15 | `routing/line.rs`, `routing/saved.rs`, `shapes.rs`, `text.rs`; `app.rs`, `pages.rs` | Small helpers written more than once: the distance from a point to a segment five times (twice in `line.rs`, `saved.rs`, and the per-pixel loops of `shapes.rs` and `text.rs`); the scale bar drawn by two pieces of code, the window's and the printed page's (on a white card). | design | One segment helper for the `f64` ones; the `f32` per-pixel loops stay. One scale bar with the card as an option, with D1 or 5.5 (both bars need the dark UI's colours). | **Owner: refactor**: no duplicated logic. |
| D16 | `omp-builder` `measure`, `report`, `tiles/{compress,encode}.rs`, `routing/layout.rs`, `ch/check.rs` | Phase 0's spike is still in the builder: `measure` and `report` with the V0, V3, V5 and MVT encodings and the size estimates, the `fast_paths` comparison (its only use of that dependency), and the three hierarchy layouts. Two parts run in every `build`: each tile is also encoded as MVT and gzipped, only to print its size (lead P2), and the layouts are compared (2 s on North America). About 2,550 lines. | design | Owner: retire `measure`, `report` and the `fast_paths` comparison as R3 retired `addr-spike`, and take the MVT size and the layout comparison out of `build`; or keep them as tools, out of `build`. | **Owner: remove** the old measurement code from the builder. |
| C1 | `routing/panel.rs`, `routing/mod.rs`, `side.rs` | Doc comments above the wrong functions, and module docs still describing phase 4's disabled places. | cosmetic | Fix now. | **Fixed.** |

| # | Where | Lead | Cost now | Estimated gain | Effort | Step | Decision |
|---|---|---|---|---|---|---|---|
| P1 | `omp-core` `Blocked::read` | Each block read makes a new zstd decompression context (`DCtx::create`, which allocates its window) and drops it: every search, street, address and routing block a cache misses. | Unmeasured: one allocation and setup per block; a coast-to-coast route decodes about a thousand `RGEO` blocks. | Small. | Small: a context per reader, behind its cache's lock, or per thread. | 5.2 measures, then 5.8 or 5.9 | **Owner:** check in 5.2. |
| P2 | `omp-builder` `tiles/mod.rs` cut | Every tile is also encoded as MVT and gzipped, for a size line only (D16). | Inside the tile cut's 222 s on North America (three bands of 98, 60 and 64 s). | Unknown: measure the cut without it. | Small. | 5.12 | **Owner:** check in 5.2. |
| P3 | `snapshot.rs`, `pool.rs` | B7: the print worker's GPU slabs grew with every preview and page. 4.11's print memory (DX12 +965 MiB for one page) was measured with it. | Fixed. | Some of 5.11's print memory. | Done. | 5.11 measures again | **Owner:** check in 5.2. |

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
8. **Printing uses the style on screen** (question c). The high-contrast style is a style like the others, named for what it is, not "print": it is never switched to for printing by itself, and the user picks it in the Map tab if they want it on paper.
9. **A North America rebuild is decided when a step next touches the packs** (question d), by the time the owner has then; 5.12 only measures the size levers.
10. **The audit fixes design and architecture problems as well as bugs** (question e), such as splitting `app.rs`.
11. **Performance targets are goals to aim for** (question f), not gates; misses are measured and written up, as in phases 3 and 4.

12. **The app's release build unwinds on panic** (5.1 R8, 2026-10-30), replacing phase 0's `panic = "abort"`, so a crash in a worker becomes an error the UI can show. It costs 1.39 MiB (+9%) on Linux.

Open: none.

## Out of scope for phase 5

- New features: GPX, KML and CSV, fuel and rest suggestions, a larger POI set, GPS (roadmap *Later or optional*).
- Map tuning beyond what the night and high-contrast styles need (decision 2).
- A print-only style, or a style choice on the print page (decision 8).
- A style editor in the app; users edit `style.toml` by hand, as today.
- U-turn work (phase 4 decision 22): stays as it is unless 5.9's work on directions makes the hidden-variant search cheap.
- macOS and anything for distribution (phase 6).

## Carried from earlier phases

- **Phase 4 4.11 left open:** the hand checks on each laptop (`README.txt` items a–d) and the ruler and black-and-white paper checks (`PRINT-TEST.txt`), only partly done: the black-and-white check is 5.6's; the rest go in 5.13's hand checks.
- **Phase 4:** the cold long-route regression since 3.9 (5.8); print memory, 600 dpi and the GL cap and panic (5.11); DX12 print memory not remeasured (5.11).
- **Phase 3:** the 7490 under Debian's UI scale (5.3); the cold-route and warm-long-route misses accepted in decision 15 (5.8, 5.9).
- **Refactor R4.5:** profiling the address merge (5.12).
- **Phase 1:** the E6430's GL leak (5.10, time-boxed).

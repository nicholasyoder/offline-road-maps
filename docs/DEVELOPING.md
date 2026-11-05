# Developing Offline Road Maps

How to run the app from source, what its developer modes do, and what CI checks. To build a map or the release programs, see [BUILDING.md](BUILDING.md); the plans and history are in [dev/](dev/) and [roadmap.md](roadmap.md).

The crates: `omp-core` (the pack format, shared by the builder and the app), `omp-builder` (builds map packs from OSM and address data) and `omp-app` (the map viewer; the program is `OfflineRoadMaps`, `OfflineRoadMaps.exe` on Windows).

## Running the app from source

```sh
cargo run --release -p omp-app -- --pack $OMP_WORK/packs/ohio
```

`$OMP_WORK` is the builder's work directory (default `./data`); packs are built into `$OMP_WORK/packs/<region>` (see below). A small region such as Delaware is quick to build and load. `--data-dir <dir>` makes `<dir>` the data folder (maps, config, trips, pins, styles) instead of the executable's folder; the release's Linux launcher uses it. Without `--pack` the app opens `maps/` in the data folder, else the map used last; with none to open, the window says what a map is and where it looked. Ctrl+O opens the file browser to choose a map. The window's size, place and maximized state are kept in `config.toml` and restored (a window that no longer fits the monitor is brought onto it).

**Put maps on an exFAT stick.** Format the stick exFAT, not FAT32: on FAT32 the first read deep into each map file walks the file's cluster chain, which takes seconds on a USB 2 stick. FAT32 keeps working, only slower to start.

Controls: drag to pan, scroll or double-click to zoom, arrow keys to pan, `+`/`-` to zoom, F3 for the debug overlay. F1 (or ☰ → Keyboard shortcuts) lists every key; the list is made from the same table the keys are handled with.

The side panel on the left holds the search box, the ☰ menu (open or verify a map, print, About this map, Settings, Keyboard shortcuts, the debug overlay) and four pages: Search, Route, Trips (saved trips and pushpin sets) and Layers (the layer toggles). ☰ → Settings has the UI and label sizes, anti-aliasing, how the map moves, units and what the app opens to. Ctrl+B or the « and » buttons hide and show it; drag its edge to resize it. Right-clicking the map offers "What's here?", the route items and *Add pushpin*.

Trips and pushpins are plain TOML files next to the executable, so they travel with the app on a stick: `trips/<name>.toml` (from *Save trip* on the Route page) and `pins/<set>.toml`. A trip keeps its stops by name and coordinates, never by the pack's IDs, so it opens on a newer build of the pack. Where that folder cannot be written, saving offers a file browser.

Search: Ctrl+F or `/` goes to the search box, opening the panel if needed. Suggestions show in the panel as you type; the arrow keys move through them, Enter or a click picks one and flies there with a marker, and Enter with none selected lists every result. Escape closes the suggestions, then leaves the box, then steps back in the page: on the search page it closes "what's here", then the results list, then removes the marker; on the route page it lets go of the step picked (the × in the box clears the search). A pack built before search (format 0.3 or older) opens with the box disabled.

The style is chosen in the side panel's Map tab and remembered: the built-in Default ([crates/app/data/style.toml](crates/app/data/style.toml)), Night and High contrast, then `style.toml` and every `styles/*.toml` next to the executable. `--style <file>` uses that file for the run, whatever the tab has chosen. A debug build (`cargo run -p omp-app -- --style crates/app/data/style.toml ...`) reloads the style in use when its file changes.

Other modes:

```sh
# Draw one view to a PNG, without a window
cargo run --release -p omp-app -- --render --pack $OMP_WORK/packs/ohio --center 39.96,-83.0 --zoom 12 out.png

# Fly a scripted path and write a JSON performance report (the flights end with typed
# searches; OMP_BENCH_SHOTS=<dir> also saves screenshots of their suggestions and results)
cargo run --release -p omp-app -- --bench crates/app/bench/ohio.toml --pack $OMP_WORK/packs/ohio
```

`cargo run -p omp-app -- --help` lists every option.

On Windows the release build opens no console window. Started from a terminal it writes to that terminal, but the terminal does not wait for it: run the bench and `--render` with `start /wait OfflineRoadMaps.exe …` (cmd) or `Start-Process -Wait -NoNewWindow .\OfflineRoadMaps.exe -ArgumentList '…'` (PowerShell). Debug builds keep their console.

## Checks

CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

and two more checks (also runnable by hand): `python3 tools/licenses.py <out.txt>` (every library has a licence we can ship and its notice is found) and, on pushes, the no-network-crates check for the app. `python3 tools/check-links.py` lists Markdown links whose target file is missing.


Every environment variable the code reads is listed in the [appendix of BUILDING.md](BUILDING.md#appendix-environment-variables).

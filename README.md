# Offline Road Maps

An offline road map app for North America, in Rust. The `omp` prefix on crates, environment variables and `.omp` pack files is short for Offline MaPs.

- `omp-core`: the pack format, shared by the builder and the app
- `omp-builder`: builds map packs from OSM and address data
- `omp-app`: the map viewer

Plans and progress are in [docs/roadmap.md](docs/roadmap.md) and the per-phase plans in [docs/](docs/).

## Running the app

```sh
cargo run --release -p omp-app -- --pack $OMP_WORK/packs/ohio
```

`$OMP_WORK` is the builder's work directory (default `./data`); packs are built into `$OMP_WORK/packs/<region>` (see below). A small region such as Delaware is quick to build and load. Without `--pack` the app opens `maps/` next to the executable, else the map used last; with none to open, the window says what a map is and where it looked. Ctrl+O opens the map picker. The window's size, place and maximized state are kept in `config.toml` and restored (a window that no longer fits the monitor is brought onto it).

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

On Windows the release build opens no console window. Started from a terminal it writes to that terminal, but the terminal does not wait for it: run the bench and `--render` with `start /wait omp-app.exe …` (cmd) or `Start-Process -Wait -NoNewWindow .\omp-app.exe -ArgumentList '…'` (PowerShell). Debug builds keep their console.

## Building a pack

```sh
cargo run --profile release-builder -p omp-builder -- build $OMP_WORK/inputs/osm/<region>.osm.pbf --out $OMP_WORK/packs/<region>
cargo run --profile release-builder -p omp-builder -- inspect $OMP_WORK/packs/<region>
```

`omp-builder --help` lists the other commands (`fetch`, `verify`, ...).

## Checks

CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

MIT or Apache-2.0, at your option. The bundled Noto Sans fonts are under the SIL Open Font License 1.1 ([crates/app/data/fonts/OFL.txt](crates/app/data/fonts/OFL.txt)). The POI icons are from [Maki](https://github.com/mapbox/maki), under CC0 1.0 ([crates/app/data/icons/LICENSE.txt](crates/app/data/icons/LICENSE.txt)).

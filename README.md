# Offline Road Maps

An offline road map app for North America, in Rust:

- `omp-core`: the pack format, shared by the builder and the app
- `omp-builder`: builds map packs from OSM and address data
- `omp-app`: the map viewer

Plans and progress are in [docs/roadmap.md](docs/roadmap.md) and the per-phase plans in [docs/](docs/).

## Running the app

```sh
cargo run --release -p omp-app -- --pack $OMP_WORK/packs/ohio
```

`$OMP_WORK` is the builder's work directory (default `./data`); packs are built into `$OMP_WORK/packs/<region>` (see below). A small region such as Delaware is quick to build and load. Without `--pack` the app opens `maps/` next to the executable, else the pack used last. Ctrl+O opens the pack picker.

Controls: drag to pan, scroll or double-click to zoom, arrow keys to pan, `+`/`-` to zoom, F3 for the debug overlay.

The style comes from `--style <file>`, else `style.toml` next to the executable, else the built-in [crates/app/data/style.toml](crates/app/data/style.toml). A debug build (`cargo run -p omp-app -- --style crates/app/data/style.toml ...`) reloads the style when the file changes.

Other modes:

```sh
# Draw one view to a PNG, without a window
cargo run --release -p omp-app -- --render --pack $OMP_WORK/packs/ohio --center 39.96,-83.0 --zoom 12 out.png

# Fly a scripted path and write a JSON performance report
cargo run --release -p omp-app -- --bench crates/app/bench/ohio.toml --pack $OMP_WORK/packs/ohio
```

`cargo run -p omp-app -- --help` lists every option.

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

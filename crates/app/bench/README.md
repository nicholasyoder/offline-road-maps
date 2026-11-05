# Bench flights

Scripted runs for `OfflineRoadMaps --bench <flight.toml> --pack <dir>` (see the header of each file for its exact command). Each one writes a report of frame times and times to a detailed view. Pack paths below are under `$OMP_WORK/packs/`.

| Flight | Pack | What it answers |
|---|---|---|
| `north-america.toml` | north-america | The main flight: city jumps, pans and zooms z3 to z17 (phase 1 exit criterion; the frame-time baseline) |
| `quick.toml` | north-america | First four steps of the above; a short start-up check |
| `north-america-p4.toml` | north-america | Phase 4 features at continent scale: schedule, alternatives, print preview and PDF with RSS, stop-order optimisation |
| `ohio.toml` | ohio | The same kind of flight on a state pack |
| `ohio-pins.toml` | ohio | What 10,000 pushpins cost (against `ohio.toml`) |
| `print-rss.toml` | north-america | The print page's memory over time at 300, 360 and 600 dpi |
| `print-hc.toml` | north-america | Printing after switching to the high-contrast style |
| `styles.toml` | north-america | The time to switch between map styles |

Removed in phase 6 (done checks, superseded): `na-alternatives`, `na-schedule`, `na-print`, `ohio-route`; the last commit with them is 57d56dd (`git show 57d56dd:crates/app/bench/<name>.toml`).

The final numbers for every machine are in `docs/dev/baselines/5.13/`.

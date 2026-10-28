# Route set

The routes that check routing and directions (`omp_core::route`, phase 3 step 3.5,
criterion 3), run by

```
omp-builder route <pack> --set crates/core/tests/route/routes.tsv [--region <name>] [--verbose] [--miles]
```

which routes each line of the pack's region (the pack's name, lowercase and hyphenated;
`north-america` runs every line), checks its expectations, prints the directions of the
routes that fail (all of them with `--verbose`) and the pass rate by region.

- `routes.tsv`: about 70 routes in Ohio, Quebec, Ontario and across the continent:
  Interstates and autoroutes, the Ohio Turnpike, the A-25 and A-30 toll bridges and the
  407 ETR with and without `tolls`, ferries (Lake Erie islands, the Saguenay at Tadoussac,
  the St. Lawrence, Wolfe Island, BC Ferries) with and without `ferries`, roundabouts,
  exits, two-stop trips, turn restrictions and coast-to-coast routes. The columns are in
  the file's header.
- **Roads:** a road is used when the route drives at least 200 m on pieces whose street
  has the route number (`I 71`, `I-71` and `i71` match) or a name containing the text;
  `!road` allows at most 50 m (crossing it, or meeting it at a junction). `toll` and
  `ferry` match the flags; `a/b` takes either.
- **Steps:** a text some step's English instruction must contain, ignoring case; `!text`
  for one no step may contain. A turn restriction is checked this way, by the forbidden
  turn's own instruction ("!Turn left onto Vermont Avenue"), with the stops on its `from`
  and `to` ways. A time bound would not do: with no turn costs (phase 3 decision 2), a
  route can get round a banned turn with a free U-turn at the next node.
- **Times** are typical drive times without traffic, widened. `omp-builder route <pack>
  --near lat,lon [--radius m]` lists the turn restrictions and roundabouts near a point,
  with points on their roads, for writing new lines.

## Against OSRM

Criterion 4 compares our times and roads with OSRM's on the same extract. OSRM runs on
the dev machine only (phase 3 plan, environment notes); its answers are saved, so the
comparison reruns without it:

```
omp-builder route <pack> --pairs 1000 --out <region>-pairs.tsv
tools/osrm/fetch.py <region>-pairs.tsv <region> http://localhost:5000 <region>-osrm.tsv
tools/osrm/fetch.py crates/core/tests/route/routes.tsv <region> http://localhost:5000 <region>-set-osrm.tsv
omp-builder route <pack> --osrm <region>-osrm.tsv [<other>-osrm.tsv] [--worst 20]
```

`--pairs` draws a third of the pairs 1–10 km apart, a third 10–100 km and a third
anywhere. `ohio-pairs.tsv` and `quebec-pairs.tsv` are step 3.5's, drawn from the Ohio and
Quebec packs with the default seed. OSRM's answers to them (27 MiB, mostly lines) are kept
on the dev machine in `$OMP_WORK/measure/route35/`, not in the repo. `--osrm` prints, overall and by trip length, the ratio of our time to OSRM's
(median, 10th and 90th percentiles, the share within 15%), the distance ratio, how much
of our line lies within 25 m of OSRM's, the ratio of step counts and the share of routes
with a U-turn, then the pairs with the largest time differences. Given two answer files
(stock `car.lua` and [`tools/osrm/car-noturn.lua`](../../../../tools/osrm/car-noturn.lua),
which sets the turn and U-turn penalties to 0), it also compares them with each other: the
effect of turn costs alone.

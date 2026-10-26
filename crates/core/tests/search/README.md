# Search query set

The queries that check the search engine (`omp_core::find`, phase 2 step 2.6), run by

```
omp-builder search <pack> --check crates/core/tests/search/queries.tsv \
    crates/core/tests/search/addresses-<region>.tsv [--verbose] [--prefix]
```

which prints pass rates by category, the "what's here" rate at the address points, and
query times (warm: run it twice, or after another check on the same pack).

- `queries.tsv`: hand-written queries for the Ohio, Quebec and Ontario packs (the `region`
  column is the pack's name, lowercase): places, shared names, abbreviations, French names
  and missing accents, POIs and brands, streets, numbered streets, routes, addresses, units,
  postcodes, typos and queries that should give nothing. A query passes "first" if the first
  hit has an accepted kind, contains the name text and is within the distance; "top 5" if
  one of the first five does. The column layout is in the file's header.
- `addresses-<region>.tsv`: random addresses drawn from a pack's merged store by
  `omp-builder search <pack> --sample <n> --out <file>` (deterministic), as "number street,
  town, state" with their points. "First at its point" means the first hit is an address
  within 10 m (100 m if approximate); "the same address" also counts another point of the
  same number and street within 3 km, since the store keeps two points of one address when
  its sources put them over 250 m apart.
- `fixture.tsv`: queries on the small test pack of `crates/core/src/search_tests.rs`, run by
  `cargo test` (so in CI).

The packs are built with `omp-builder build <pbf> --out <dir> --addresses <ISO>` (phase 2
plan, environment notes). Rebuilding a pack can move entries and addresses, so redraw the
address files when the merge or the street table changes.

## Search levels

The app searches each keystroke at level 0 and, while the user pauses, at deeper levels
(phase 2 step 2.7b). `--level <n>` runs every query down to level `n`, as the app shows it
after a pause. `--levels` runs address files level by level, as typed, with the whole
continent in view. For each level it prints how many addresses are first at their point
or among the 8 suggestions, how many were first at level 0 but not at that level
("lost"), and the level's time. It also prints how long after the pause a deeper level put
an address first. `--typed` types the address queries another way: `state` (the state's
name in full), `state:N` (its first N letters), `nostate`, or `town:N` (the town's first N
letters, with no state):

```
omp-builder search <pack> --check addresses-na.tsv --levels --typed town:4 [--fresh]
```

`--fresh` also times each level alone in a new session, to show what sharing the caches
across levels saves.

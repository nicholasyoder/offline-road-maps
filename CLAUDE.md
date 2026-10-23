# Offline Road Maps

Offline road map app for North America (Rust: `omp-core`, `omp-builder`, `omp-app`). Plans live in `docs/`: [roadmap.md](docs/roadmap.md) and one plan per phase (e.g. [phase0-plan.md](docs/phase0-plan.md)).

## Tracking progress

- When you finish a plan step (it meets its **Done when** line), mark it done in that phase's plan, in the same commit as the work if possible:
  - set its row in the plan's **Status** table to **Done** with the date;
  - add a short `> **Done <date>**` note under the step heading: the commit, the key results or numbers, and anything left open.
- Mark partly finished steps "In progress" with a note on what remains, so the next session can pick up from there.
- When a decision is made or a measurement replaces an estimate, update the plan (and the roadmap if it applies) rather than leaving it only in chat.

## Environment

- Builder data goes in `$OMP_WORK=/media/nicholas/Data/maps-testing` (exFAT). If it isn't mounted, run `udisksctl mount -b /dev/sda1`. Keep `target/` and executables on `/`. See the Environment notes in the phase 0 plan.
- Default branch is `master`. To push, use `git -c credential.helper= -c credential.helper='!gh auth git-credential' push origin master`; plain HTTPS has no stored credentials.
- Before committing, run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`; these are what CI runs.

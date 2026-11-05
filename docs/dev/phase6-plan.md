# Phase 6 Plan: Distribution

*Draft 1, 2026-11-04. Expands Phase 6 of [roadmap.md](../roadmap.md#phase-6-distribution). Changed with the owner while planning: the planet build moves to *Later or optional*, the attribution item is dropped (About covers it, checked in 6.6), a user-friendly binary name is added, and so is a clean-up of what development left behind.*

## Purpose

Phases 1–5 made the app. Phase 6 makes it something that can be handed to someone on a stick: a product name on the binary, a layout that works on every platform, documents a stranger can follow, and a repo and work disk without the leftovers of development. No new features and no format change.

Starting point (checked 2026-11-04):
- The binaries are `omp-app` and `omp-builder`; the window title and About already say **Offline Road Maps**.
- The app looks for `maps/`, `config.toml`, `trips/`, `pins/`, `style.toml` and `styles/` **next to the executable** (`main.rs` `exe_dir`; `config::Location::find`). Whatever folder the executable is in is the data folder. This decides the layout (6.3).
- CI builds both binaries for Linux (Ubuntu 22.04, glibc 2.35) and Windows and uploads them as artifacts. No release job, no macOS build (the network-crate check only runs `cargo tree` for macOS).
- Linux mounts FAT32 sticks without exec, so the binary cannot run from them ([phase 1 step 1.10](phase1-plan.md#110-performance-usb-and-hardware-checks)). The README now recommends exFAT; whether exFAT mounts executable on the usual desktops is not measured.
- The README is written for developers (`cargo run …`). There is no `BUILDING.md`, no end-user text, and no third-party licence file for the Rust dependencies that end up inside the binary.
- Attribution: *About this map* shows the pack's attribution plus each source's own (`dialogs.rs`), and prints carry "© OpenStreetMap contributors". So the roadmap's attribution item needs only a check (6.6).

### Out of scope
- The planet build (moved to *Later or optional*).
- A macOS build, signing or notarisation: nothing here can be tested on a Mac. Decision 5.
- Installers. The product is a folder you copy.

### Exit criteria
1. **Name.** The shipped binaries are `OfflineRoadMaps(.exe)`; no user-facing text, file name or doc says `omp-app`.
2. **Layout.** A script builds the media layout from a pack and the release binaries; the result runs on Windows and Linux from an exFAT stick, following only `README.txt`.
3. **Docs.** `docs/BUILDING.md` was followed from a clean checkout by someone (or a clean machine) that is not the maintainer's dev box and produced a working region pack, including a custom polygon region. `README.txt` fits one screen's worth of steps.
4. **Licences.** The release folder carries the app's licence, the fonts' and icons', and the Rust dependencies' notices.
5. **Clean-up.** Every item of the 6.1 inventory has an owner-approved verdict (keep, delete or move) and the approved ones are done. Nothing of the owner's (`trips/`, `pins/`, `config.toml` next to the dev binary) is touched.

## Status

| Step | State | Notes |
|---|---|---|
| 6.1 Clean-up inventory | **Done** 2026-11-04 | Tracked items done 2026-11-04 ; owner deleted the untracked leftovers in `target/`, `data/`, `tools/gl-probe/target` |
| 6.2 Binary name | **Done** 2026-11-04 | Windows exe icon unchecked until CI builds it |
| 6.3 Release layout and launchers | **Done** 2026-11-05 | Windows exe on Windows laptops and a FAT32 stick confirmed by the owner; other desktops' exFAT mount options not measured |
| 6.4 Release build and packaging script | **Done** 2026-11-05 | Version bumped to 1.0.0; Linux binary not tried on an older distribution |
| 6.5 Docs: BUILDING.md, README, README.txt | **Done** 2026-11-05 | The rest of BUILDING.md's commands are walked through for real in 6.7 |
| 6.6 Licences and attribution check | **Done** 2026-11-05 | `tools/licenses.py`; optional About entry skipped; the `README.txt` paragraph is 6.5's |
| 6.7 Clean-room check on both platforms | **Done** 2026-11-05 | Numbers not recorded (owner decision) |

Order: 6.1 first (so docs are written against the tree that will ship), then 6.2 (the name touches everything the docs quote), then 6.3 and 6.4 together, 6.5 and 6.6 in parallel with them, 6.7 last.

## 6.1 Clean-up inventory

**Rule (changed 2026-11-04): delete what is in git, list the rest.** Files tracked in the repo are removed at once, since history keeps them (`git show <commit>:<path>`). Everything else (untracked or ignored files in the repo directory, `target/`, `$OMP_WORK`) is listed, whole directories as one line, and deleted only after the owner approves. The verdicts for the second kind are in the tables below.

*In the repo (tracked)*
- **`docs/dev/screenshots/`** (29 files, 15 MB, plus `1.5c/` with 8). Found so far: nothing in the code reads them; `render.sh` regenerates the set from the North America pack, and `snapshot.rs` only mentions it in a comment. Plans link to a few by name (phase 1 step 1.5c, 1.6; phase 3 step 3.6; phase 4). They are reference shots for style tuning during phases 1–3, not regression tests (nothing compares them). **Proposal:** delete the PNGs (they stay in history), keep `render.sh` as the way to regenerate, and turn plan links to them into plain text ("removed in phase 6; `git show <commit>:docs/dev/screenshots/…`"). Then make a small new set of 4–6 hero shots from the final build for the README (map, search, route with directions, night style, print preview), each under about 300 KB.
- **`tools/gl-probe`** and **`.github/workflows/probe.yml`**: built only for the E6430 crash hunt, finished in 5.14 (no fix; `panic = "abort"` stays). Its `target/` is 1.5 GB on disk. **Proposal:** delete both from the tree; the finding is in the phase 5 plan.
- **`tools/osrm/`** (24 KB): the comparison scripts behind phase 3's accuracy numbers. **Proposal:** keep; it is how the numbers can be reproduced. Mention in BUILDING.md's "measuring" part or leave to phase docs.
- **`crates/app/bench/`** (12 flights). Several are tied to one phase's check (`ohio-pins`, `na-alternatives`, `na-schedule`, `print-rss`, `print-hc`, `styles`). **Proposal:** keep, since `--bench` stays a supported mode, but list them in one place (a short `bench/README.md`) with what each answers, and delete any that no plan or doc refers to.
- **Plans** in `docs/` (phase 0–5, refactor, audit and perf findings, size table, `format.md`): history, but `format.md` is current reference. **Proposal:** keep all; move the finished plans to `docs/history/` only if the owner wants a tidier `docs/`. Links in the roadmap and plans would need rewriting, so default is leave them.
- **Dead code behind the dev modes:** `--bench`, `--render`, `trace.rs`, the `OMP_*` environment variables. These are small and used by the checks; keep. Task: list every `OMP_*` variable the code reads in one place (BUILDING.md appendix), and delete any that nothing uses.

*Local, untracked (not in git)*
- **`target/`: 29 GB.** `debug` 19 GB, `profiling` 3.4 GB, `wt` 0.8 GB (a worktree build), `omp-builder-base` and the cross-compile dir `x86_64-pc-windows-msvc` (0.5 GB). Safe to remove and rebuild; `release/` also holds the owner's real `config.toml`, `trips/` and `pins/`, so **never `rm -r target/release`**; remove only named build subdirectories, with the owner's say.
- **`data/tmp/ohio-build.v2`** (124 MB, ignored), **`notes`** (ignored; empty-looking): check and drop or keep.
- **`$OMP_WORK` (about 250 GB):** `inputs` 37 GB (the downloads; keep, they cost time to fetch), `packs` 48 GB (keep `north-america` and the small test regions; the numbered variants `1.5`, `1.5b`, `v2`, `north-america-0.3` and `-0.6-old` are 36 GB of experiments), `refactor` 33 GB, `measure` 86 GB (reports and logs of every phase; the reports worth keeping are the ones the plans quote), `osrm` 9.8 GB, `graph` 5 GB, `data` 8.3 GB, `audit-d` and `audit-e` (0.9 GB), `tmp` 0.4 GB. **Proposal:** keep `inputs`, the final packs, and a trimmed `measure/` (reports and logs only, not the pack copies and tiles inside); delete the rest on the owner's word. This frees roughly 150 GB.

> **Tracked items, done 2026-11-04.** Deleted from the tree: `docs/dev/screenshots/*.png` and `1.5c/` (kept `render.sh`; plan links turned into plain text pointing at 57d56dd), `tools/gl-probe/` and `.github/workflows/probe.yml`. Kept: `tools/osrm/`, the plans, 8 of the 12 bench flights, listed in the new `crates/app/bench/README.md` (deleted the done or superseded `na-alternatives`, `na-schedule`, `na-print`, `ohio-route`; `quick.toml` kept as a smoke test). Left open: the README's 4–6 hero shots (6.5, from the final build) and the `OMP_*` list (BUILDING.md, 6.5).

**Done when:** the inventory table is in this section with a verdict for each item; the approved deletions are done; `grep -r screenshots docs README.md crates` finds no dead link; CI is still green.

## 6.2 Binary name


> **Done 2026-11-04** (commit below in git log). `[[bin]] name = "OfflineRoadMaps"`; USAGE, module docs, bench header, `render.sh`, bench flight comments, `ci.yml` (artifact paths, size table) and the README updated; `cargo run -p omp-app` still works. Window: `with_app_id("OfflineRoadMaps")` and `with_icon` from the compiled-in `crates/app/data/icon.png` (256 px, 12 KB; `icon.ico` with 16–256 px beside it, both rendered from `icon.svg` with inkscape/ImageMagick). Windows: `build.rs` with `winresource` (a Windows-only build-dependency) sets the icon and name; **not yet seen**: check the Explorer icon on the first CI Windows build. `cargo fmt`, clippy and tests pass; the Linux binary starts and `--help` shows the new name. Left open: the old `target/release/omp-app` is still next to the new binary for the owner to delete after switching; the old name remains only in history (plans, baselines logs) and the crate.
- Add `[[bin]] name = "OfflineRoadMaps"` to the app crate (package, crate and `omp-` library names stay: the roadmap keeps `omp` in code). On Linux the file is `OfflineRoadMaps`; on Windows `OfflineRoadMaps.exe`.
- Update everything that quotes the old path or name: the `USAGE` text and module doc in `main.rs`, the bench report header (`"omp-app {}: bench …"`), `render.sh`, `.github/workflows/ci.yml` (artifact paths and the size table), every `cargo run -p omp-app` / `target/release/omp-app` in README, plans' *future-facing* commands (not the history), and the bench files' header comments. `cargo run -p omp-app` keeps working (package name unchanged); `--bin` users need the new name.
- The window's Wayland `app_id` and X11 class: set to the product name so desktops group the window correctly.
- **Icon (decided):** one icon used in three places: the `.exe`'s resource (Explorer, taskbar), the window's icon on Windows and Linux (`ViewportBuilder::with_icon`, which sets the title bar, taskbar and Alt-Tab entry on X11 and Wayland through `app_id`), and the README. A generic map icon, drawn as SVG and rendered to PNG at 16–256 px and to `.ico`; the owner picks from a few options first (6.2 starts with those). Compiled in as one 256 px PNG (about 10 KB).
- **Windows:** an icon and version info on the `.exe` (name, version, description) so Explorer shows the product name, not a bare file. Done with a build script (`winresource`) as a build-dependency only, which the network-crate CI check (normal dependencies) does not see. Needs an icon; a simple one drawn once as SVG and rendered to `.ico`/`.png`. Optional; skip if it costs more than an hour or a new toolchain requirement on CI.
- The Linux binary gets no icon (a bare ELF has none). An optional example `.desktop` file is shipped instead (6.3). Correction: `with_icon` sets the title bar and Alt-Tab icon on X11 and Windows only; Wayland ignores it and uses the `.desktop` file's `Icon`.
- **Builder (decision 1):** `omp-builder` is a developer tool, not on the stick, and appears only in BUILDING.md. Recommend keeping its name; renaming it the same way is the same small change if the owner prefers.
- The owner's dev `target/release` holds the real `config.toml`, `trips/` and `pins/` next to `omp-app`; the renamed binary lands in the same folder and sees them. Delete the old `omp-app` file there only after the owner has switched.
- Config and data paths: `config.rs` `APP_DIR = "offline-road-maps"` (the per-user fallback folder) is an internal name; unchanged unless the owner wants the product spelling there too (it would orphan existing fallback configs, which are not used when the config sits next to the exe).

**Done when:** `cargo build --release` yields only `OfflineRoadMaps` for the app; `grep -rn "omp-app"` finds only history in plans, code comments about the crate, and the crate itself; CI builds and uploads both platforms under the new name; the Windows exe shows the name and icon in Explorer (if taken).

## 6.3 Release layout and launchers

The constraint from the code: the executable's folder holds `maps/`, `config.toml`, `trips/`, `pins/`, `style.toml`. The roadmap's sketch (`app/` with the executables, `maps/` and `README.txt` beside it) would make the data folder `app/`, so `maps/` would have to sit inside it, or the app must look one folder up. Options:

- **A (recommended): the data folder is the stick's top level.** *Changed 2026-11-05:* the Linux binary lives in `linux/`, so the only thing to run on Linux is the launcher; the Windows exe stays at the top because its folder is its data folder.

  ```
  OfflineRoadMaps/            (the stick's root or one folder on it)
    README.txt
    OfflineRoadMaps.exe       Windows
    OfflineRoadMaps.sh        Linux launcher (run with `sh`): the one thing to run on Linux
    linux/                    OfflineRoadMaps (the Linux binary), and the example
                              OfflineRoadMaps.desktop (menu entry) and its icon PNG
    maps/<pack name>/         the packs (north-america.omp.000 …) and their SHA256SUMS
    licenses/                 licence and third-party notices
    config.toml, trips/, pins/, styles/   created by the app on first use
  ```

  Simple to explain, and the user's saved trips and settings travel with the stick. One folder holds both platforms, which share `maps/` and the user's files.
- **B: `app/<platform>/` holding binaries, data folder one level up.** Needs a small change: the app takes its data folder as the executable's folder, else its parent if it contains `maps/`, and the write test for `config.toml` changes with it. Tidier on the stick (one visible folder per platform), a risk of surprising users who copy a binary elsewhere, and a platform to test: macOS later fits it better. Not recommended for a first release.

**Linux launcher.** `OfflineRoadMaps.sh` (source `tools/release/`): finds its own folder; tries `linux/OfflineRoadMaps --help` and, if the shell says it may not be executed (status 126: FAT32, `noexec`), copies the binary to `${XDG_CACHE_HOME:-~/.cache}/OfflineRoadMaps/` (only when it differs, by `cmp`) and runs that copy. Either way it passes **`--data-dir <stick folder>`** (the new option, the one code change of this phase: `main.rs` takes it instead of the executable's folder) so the config, trips, pins and `maps/` come from the stick. Run as `sh OfflineRoadMaps.sh`, which works on any mount; arguments are passed on.

**Menu entry (decided 2026-11-04):** an example `OfflineRoadMaps.desktop` and `OfflineRoadMaps.png` in `linux/` ([`crates/app/data/linux/`](../../crates/app/data/linux)), so a Linux user can add the app to their menu by editing the path and copying one file; the README.txt says how. Not installed by anything. Its `Exec` runs the `sh` launcher, so it needs the launcher of this step. (On Wayland the window itself gets no icon from `with_icon`; the compositor takes it from a `.desktop` file, which this is.)

**AppImage:** not recommended. It needs FUSE (absent on many current distributions), does not avoid the no-exec mount, and the binary is already one file with only system graphics libraries. A plain binary built on Ubuntu 22.04 (glibc 2.35) is what CI makes now.

**macOS:** none (decision 5); the layout leaves a name for it (`OfflineRoadMaps.app` at the root) and no more.

**Measure on the way:** whether exFAT sticks mount executable on this Debian (the dev machine) and on the usual Ubuntu/Mint/Fedora defaults; the README states what we measured, not what we expect.

> **Linux half done 2026-11-05.** `--data-dir <dir>` (main.rs, USAGE; an unreadable folder exits 2), `tools/release/OfflineRoadMaps.sh`, the layout above built onto a fresh 58 GB exFAT stick by `tools/package.sh`. Measured: this Debian mounts the exFAT stick with `rw,nosuid,nodev` (no `noexec`), so `sh OfflineRoadMaps.sh` runs the binary in place and `config.toml` appeared on the stick. The no-exec branch was tested in a user namespace on a `noexec` tmpfs (direct exec gives 126; the launcher copied the binary to the cache, started the app and the config landed in the `noexec` folder). **Open:** a real FAT32 stick (no loop mounts without root here; the `noexec` tmpfs is the same case for the launcher, though not for the no-4 GB-file limit), the Windows exe on a Windows laptop, and the other desktops' exFAT mount options (Ubuntu, Mint, Fedora) for the README.

> **Done 2026-11-05.** The owner confirmed the Windows exe on Windows laptops (icon shows) and the stick on a real FAT32 stick. Not measured: the exFAT mount options of Ubuntu, Mint and Fedora (the launcher copes either way, tested on a `noexec` tmpfs).

**Done when:** from a fresh exFAT stick laid out as above, the Windows exe starts on a Windows laptop, `sh OfflineRoadMaps.sh` starts the Linux build on this machine whether or not the stick allows exec (test both: the stick, and a FAT32 stick), and settings, trips and pins written there end up on the stick.

## 6.4 Release build and packaging script

- **No release workflow (decided 2026-11-05).** Releases are local builds: `tools/build-linux.sh`, `tools/build-windows.sh`, `tools/package.sh`. The existing CI (checks, plus a Linux and a Windows build uploaded as artifacts on every push) stays as the independent check. The maps are far too large to attach to anything; the README says where they come from or how to build them.
- **Linux binary and glibc (found 2026-11-05):** a plain `cargo build --release` on this Debian 13 (glibc 2.41) needs `GLIBC_2.39`, so it would not start on Ubuntu 22.04, Mint 21 or Debian 12. `tools/build-linux.sh` builds with `cargo-zigbuild` for `x86_64-unknown-linux-gnu.2.35` (what CI's Ubuntu 22.04 gave) and fails if the binary asks for anything newer than 2.35. Needs `cargo-zigbuild` and `zig` installed by the owner; `package.sh` takes its output by default. **Done 2026-11-05:** zig 0.15.2 from the ziglang.org tarball plus `cargo-zigbuild`; build 2 min 7 s, 16.6 MB, needs only glibc 2.35 and links just `libc` and `libm` (graphics libraries are loaded at run time). On the stick it starts and writes `config.toml` there. Not yet tried on an older distribution.
- **Windows binary on Linux (decided 2026-11-05):** the project must not depend on GitHub CI alone, and the owner packages on Linux, so `tools/build-windows.sh` cross-builds `x86_64-pc-windows-msvc` with `cargo-xwin` (the same target and static-CRT flag as CI; it fetches the Windows SDK and CRT on first use). CI stays as the second, independent build. **Done 2026-11-05:** the owner installed `cargo-xwin` and ran the script; it needed `~/.cargo/bin` and Debian's `/usr/lib/llvm-N/bin` (for `llvm-lib`, `llvm-rc`) added to `PATH`, which the script now does. `build.rs` now embeds the icon and version info for any Windows *target* (winresource as a build-dependency on every host; it calls `llvm-rc`). Result: a 15.1 MB PE32+ GUI exe with a `.rsrc` section (icon entries, product name); under `wine` it starts and prints `--help`. **Not yet seen:** the icon in Windows Explorer, and the exe on a real Windows machine. How to build on Windows by hand is a section of BUILDING.md (6.5). No PowerShell packaging script: the owner packages on Linux.
- **`tools/package.sh`** (no PowerShell twin, decided 2026-11-05): takes the binaries and a `maps/` directory (or a pack directory) and writes the 6.3 layout to a target folder, including `README.txt`, `licenses/`, `linux/` (the example `.desktop` and icon from `crates/app/data/linux/`), the launcher and a checksum list of the pack files, then runs the builder's `verify` on the packs so a bad copy is caught before the stick leaves the house.
- Version number: one place (`workspace.package.version`), shown in About and in `README.txt`. Bump 0.1.0 to **1.0.0** (decided), in the commit that passes 6.7, so the tag and the first stick match.

> **Done 2026-11-05 (6.4 as a whole):** version bumped to 1.0.0 (`workspace.package.version`), the first stick and the tag match.

> **`tools/package.sh` done 2026-11-05.** `tools/package.sh [--linux f] [--windows f | --no-windows] [--builder f] [--no-verify] --pack <dir>… <out>`: lays out the 6.3 tree (the version from `Cargo.toml` goes into `README.txt`), copies each pack into `maps/<name>/`, compares the copy with its source by sha256 (writes `SHA256SUMS` beside the chunks), then runs `omp-builder verify` on the copy (builds the builder if missing); refuses a non-empty target. First run, North America (9.3 GB) onto the exFAT stick: 8 min 28 s end to end, verify OK. `licenses/` holds only the app's MIT/Apache and the Noto OFL for now (6.6 adds the dependency notices); `README.txt` is a first draft (6.5). Left open: the glibc-2.35 Linux build (above), the 1.0.0 bump.

**Done when:** the three scripts produce a stick that passes 6.3's check, the Linux binary needs glibc 2.35 or older (the script checks), and the version is 1.0.0.

## 6.5 Docs

- **`docs/BUILDING.md`** (the roadmap's item):
  - What you need: Rust toolchain (version from `rust-toolchain.toml`), RAM, disk and time, from the measured builds (North America: about 35 GB of inputs, 12 GB of temporary tiles, a 47 GiB peak and about 37 minutes on the dev machine; Ohio for a quick try; [phase 0 environment notes](phase0-plan.md#environment-notes) and [risks](../roadmap.md#7-risks)). A table of region size against RAM, disk and time, from the build logs.
  - Getting the data: `omp-builder fetch` for each source (OSM, OpenAddresses, TIGER, StatCan, ocean polygons), what each license requires.
  - A step-by-step region build: Delaware (minutes) as the worked example, then a US state, then North America; `inspect` and `verify`; where the pack lands; how to put it in `maps/`.
  - **A custom region from a polygon:** how a `.poly` or GeoJSON clip is given to the builder and how to cut the OSM input to it. **First check that the builder supports it today**; if it does not, this becomes a small builder step in 6.5 (the roadmap listed the item, so assume the user expects it).
  - Rebuilding the app, running the checks, cross-compiling for Windows, building the Linux binary for glibc 2.35 (zig and `cargo-zigbuild`, as numbered steps), and the manual build on a Windows machine.
  - Appendix: every `OMP_*` environment variable, from 6.1.
- **`README.md`** (repo front page): what it is, one hero screenshot, where the release goes, links to `BUILDING.md`, the phase plans and the roadmap. The developer details now in it (render and bench modes, controls) move to a `docs/DEVELOPING.md` or stay; the rule is that the front page fits two screens.
- **`README.txt`** (bundled with a release, plain text, wraps at 80 columns): what is in the folder; how to start on Windows, Linux (the `sh` launcher); that maps live in `maps/` and may be swapped; where settings, trips and pins are kept; the exFAT advice with the reason (FAT32 works but starts slowly on a USB 2 stick); the main keys (point to F1); the known limits (car only, no live GPS, Latin-script labels only); licence and credits (pointing at About for the map data's); how to report problems.
- Generate parts of these from code where the code already has the truth (the keys list, the CLI help), so they cannot drift.

> **Done 2026-11-05.**
> - **Moved** the plans and their evidence into `docs/dev/` (phase plans, refactor plan, audit and perf findings, size table, `baselines/`, `screenshots/render.sh`), with every Markdown link, the `docs/...` paths in code comments and scripts, and `render.sh`'s relative path rewritten; `docs/` now holds `BUILDING.md`, `DEVELOPING.md`, `format.md` (code comments point at it), `roadmap.md`, `img/` and `examples/`. `tools/check-links.py` lists Markdown links whose target file is missing: 15 are left, the same 15 as before the move (plans linking to source files the refactor removed).
> - **`README.md`** is the front page (about one screen: what it is, four screenshots, how to get it, links); the developer text moved to **`docs/DEVELOPING.md`**. Four shots in `docs/img/` (Columbus map, a route to Cleveland, Chicago in the night style, a printed page; 150 to 280 KB each), made from the real North America pack with `--render` on the final build. No search shot: `--render` has no panel.
> - **`docs/BUILDING.md`**: requirements with a measured table (Delaware 12 s, 2.2 GiB; Ohio 57 s, 5.0 GiB, 193 MiB pack; Quebec 114 s, 5.8 GiB; North America 45 min, 43.5 GiB, 9.4 GiB, from the stage logs in `baselines/na-build/`; Delaware and Ohio rebuilt for this with the real commands), `fetch` and `build` for Delaware, a state, a province and North America, the custom polygon, using a map, the release scripts (tool setup as numbered steps, including zig), the manual Windows build, the checks, and the appendix of every `OMP_*` variable (19, each checked against the code; none unused).
> - **Custom region from a polygon: the builder did not support it, now it does.** `--clip-poly` only clipped the ocean, so a region cut from a state carried the whole state's addresses as unattached points (tested: Delaware's extract with `--addresses US-DE --addresses US-MD` gave 2.3 M unattached Maryland points and a pack twice the size, 14.6 to 28.2 MiB). `--clip-poly` now also drops address records and TIGER ranges outside the polygon (`addr/merge.rs`, part of the `--reuse-stages` key). Check on `docs/examples/dover.poly` (a box around Dover) with Delaware's whole extract: 463 k addresses became 34 k, the pack 14.6 to 13.6 MiB (the roads stay, since the extract was not cut). The cut itself is `osmium extract` (external). **Run end to end 2026-11-05** with osmium-tool 1.18.0, the commands as in BUILDING.md: Delaware's extract cut to `dover.poly` (1.6 MB, header bounds set), built with `--name Dover --clip-poly … --addresses US-DE` in 7 s, 668 MiB peak, a 1.3 MiB pack with 34,308 addresses; `verify` OK; rendered and shows Dover.
> - **`README.txt`** (`tools/release/`, 89 lines, 80 columns, ASCII): what is in the folder, starting it on Windows (with the unsigned-program warning) and Linux, the basics, where files are kept, the exFAT advice, limits, data licences and no warranty, where to report. The front half is the one screen of steps.
> - Decided 2026-11-05: the owner does not plan to host prebuilt maps anywhere, so the docs say to build your own. Left open: the screenshots of search; the 6.7 walk-through.

**Done when:** each doc exists; links resolve; BUILDING.md's commands were run as written (6.7).

## 6.6 Licences and attribution check

The roadmap's attribution item is covered by *About this map*; what is left is a check and one gap.
- **Check:** open the real North America pack's *About this map* and confirm it shows ODbL for OSM and every address source's licence (it should, from `META`); confirm a print carries the credit line (it does).
- **Gap:** the binary statically contains Rust crates under MIT, Apache-2.0, BSD, ISC, Zlib, MPL and others, whose licences require their notices to travel with the binary. Generate `licenses/THIRD-PARTY.txt` with `cargo about` (or `cargo-license`) for the app's dependency tree on both targets, plus `LICENSE-MIT`, `LICENSE-APACHE`, the Noto Sans OFL and the Maki CC0 note. CI checks the file is up to date or the packaging script builds it. Optionally show a *Licences* entry in About that opens the same text (embedded compressed, small); skip if it costs more than a few KB of binary.
- **Data licences in `README.txt`:** one paragraph: map data © OpenStreetMap contributors (ODbL); addresses from the sources listed in About; no warranty; do not navigate by this alone.

> **Done 2026-11-05.** `tools/licenses.py <out>` (Python standard library only; no `cargo-about` to install) writes `licenses/THIRD-PARTY.txt`; `package.sh` runs it. It takes the libraries of `cargo tree -p omp-app -e normal,no-proc-macro` for Linux and Windows (177; the build tools and proc macros do not ship), reads each package's licence files from the cargo registry, prints each distinct text once with the crates that use it (121 texts, 0.46 MB), and starts with the bundled Noto Sans (OFL), Maki icons (CC0) and the tz database note; egui's default fonts' licences come with `epaint_default_fonts`. It **fails** if a crate's licence expression cannot be met by the allowed list (MIT, Apache-2.0, BSD, ISC, Zlib, Unlicense, BSL-1.0, 0BSD, Unicode, OFL, Ubuntu font, CC0, MPL-2.0), so a GPL dependency cannot slip in; today every crate qualifies (the one `Apache-2.0 OR GPL-2.0-only` is taken as Apache). 13 packages (the egui and lyon crates, accesskit, profiling, spirv) are published without a licence file; they are all Apache-2.0 too, so they get the standard text, marked as such. The file was checked to list every crate of the tree on both targets. A CI step runs the script, so a new dependency with a bad licence fails the build. The `LICENSE-MIT` and `LICENSE-APACHE` and `NotoSans-OFL.txt` stay in `licenses/` as well. Optional *Licences* entry in About: skipped (the file is on the stick; revisit only if wanted).
>
> **About check, done 2026-11-05** on the stick's North America pack (`omp-builder inspect`, which prints the `META` that *About this map* shows): the pack's attribution is "© OpenStreetMap contributors, ODbL-1.0", then the time zones' ("timezone-boundary-builder, © OpenStreetMap contributors, ODbL-1.0"), then one attribution per address source, e.g. "City of Calgary, Contains information licensed under the Open Government Licence – City of Calgary." Some address sources have only a licence URL as their text, and at least one says "unspecified" (OpenAddresses did not give a licence); 5,419 sources in all. Not looked at in the GUI itself, and the print's "© OpenStreetMap contributors" line was not re-checked here.

**Done when:** `licenses/` is produced by the packaging script and contains every crate in `cargo tree -p omp-app`; the About check is written down here.

## 6.7 Clean-room check

On the Windows laptop (and one more if available, say the 7490 on Debian), from a stick made only with `tools/package.sh`: read only `README.txt`, start the app, open the map, search, route, save a trip, print to PDF, close; eject; insert on another machine and check the trip and settings are there. Then on a clean user account or machine (a fresh container or VM is enough for BUILDING.md), follow `BUILDING.md` to build Delaware and open it. Everything that took a guess becomes a fix to the docs. Record times and the stick's size in this file.

> **In progress 2026-11-05.** Owner ran the stick walk-through and BUILDING.md's Delaware build on a fresh machine: both worked and the pack opens in the app. Finding: osmdata.openstreetmap.de was unreachable (no route to host, also from the dev machine), so the owner built without ocean polygons. Fix: `fetch` now falls back to the unofficial North America mirror www.osmdata.org (same path and file; `fetch_one`, `mirror()`), the URL actually used goes into the manifest; BUILDING.md says so. Not yet exercised against a real outage, and the mirror has no published hash, so only the zip magic is checked. 

> **Done 2026-11-05.** The owner ran the stick walk-through on several machines and the Delaware build from BUILDING.md on a fresh machine; both passed with no missing step. Times and stick size were not recorded (owner decision); the owner noticed nothing slow or memory-hungry. The README's first screenshot is to be replaced with a full-window one (route and Route tab open), which `--render` cannot make (no panels), so it is taken by hand.

**Done when:** both walk-throughs pass with no step the docs left out, and the roadmap's phase 6 is ticked.

## Decisions

Made by the owner 2026-11-04:
1. **Builder name:** stays `omp-builder`.
2. **Layout:** A, flat (6.3).
3. **Screenshots:** delete the dev reference shots; a couple (no more) for the README.
4. **Version:** bump to 1.0.0 at some point in this phase (6.4).
5. **Icon:** option D (a street grid with a river, a yellow road and a red pin; source [`crates/app/data/icon.svg`](../../crates/app/data/icon.svg): a medium-width river curving from the top centre to the lower right, all four corners rounded), to be refined only if needed. An app icon, generic map style, shown in the window's title bar and taskbar on Linux and Windows as well as on the `.exe` (6.2). The owner reviews a few options before one is chosen.
6. **Work disk:** the maintainer's judgement (the owner does the removal by hand); outdated packs and old measurement data go, anything valuable is kept or moved into the repo, which is the project's real persistence.
7. Taken as proposed: no macOS build; remove `probe.yml` and `tools/gl-probe`.

## Work-disk verdicts (6.1)

**The owner removed the work-disk items marked Delete by hand; done 2026-11-04** (Claude's sandbox blocks bulk deletes). `$OMP_WORK` now holds `inputs/` and `packs/north-america`.

Checked 2026-11-04. The queries and route sets behind the plans' accuracy numbers are already in the repo (`crates/core/tests/search/`, `crates/core/tests/route/routes.tsv`), the builds are reproducible from `inputs/`, and the plans quote the numbers they need.

| Item | Size | Verdict (all carried out) |
|---|---|---|
| `inputs/` (OSM, OpenAddresses, TIGER, StatCan, tz, poly) | 37 GB | **Keep:** slow to download |
| `packs/north-america` (format 0.7, built 2026-11-04, the final) | 9.2 GB | **Keep** |
| `packs/delaware`, `ohio`, `ontario`, `quebec` | 1 GB | Delete: formats 0.3 and 0.4, which the current app cannot open. Rebuild when needed (Delaware takes minutes; BUILDING.md's worked example) |
| `packs/1.5`, `1.5b`, `v2`, `north-america-0.3`, `north-america-0.6-old`, `test` | 36 GB | Delete: outdated experiments |
| `refactor/`, `audit-d/`, `audit-e/` | 34 GB | Delete: before/after pack copies; the plans keep the findings |
| `osrm/`, `graph/` | 15 GB | Delete: derived (OSRM's processed graphs, the routing graph); `tools/osrm/` regenerates the OSRM side |
| `data/`, `tmp/` | 8.7 GB | Delete: temporaries |
| `measure/` | 86 GB | Delete: the plans quote the numbers they need, final runs are in `docs/dev/baselines/`; the plans' `$OMP_WORK/measure/` mentions are historical pointers |
| `whitelists/` | 8 MB | Delete: variants for finished size experiments; the real one is `crates/core/data/whitelist.toml` |
| **Copied into the repo** | 0.3 MB | `docs/dev/baselines/5.13/` (every machine's final flight logs and machine info) and `docs/dev/baselines/na-build/` (North America build stages, memory and the pack's checksums): the reference for any later regression |

Roughly 155 GB is freed. Also found: `target/release-builder/omp-builder` is older than the final pack (it reads format 0.6, the pack is 0.7), so rebuild before using it.

Not touched here (owner's call, separate): `target/` (29 GB, regenerable), `data/tmp/ohio-build.v2` in the repo directory, `notes`.

## Decisions (2026-11-05)
8. **Linux binary in `linux/`**, launcher at the top, always started with `--data-dir` (6.3).
9. **No PowerShell packaging script**; packaging is done on Linux. The Windows exe is cross-built on Linux (6.4), with CI as a second build and a manual Windows build documented in BUILDING.md (6.5).

## Risks
- **Renaming the binary breaks scripts and docs that quote it.** The grep in 6.2 is the check; CI covers the build, not the prose.
- **`--data-dir` and the launcher on odd mounts.** Test on FAT32, exFAT and ext4 with and without `noexec`.
- **A custom region may need builder work** (see 6.5), which could grow the phase.
- **The deleted dev artifacts were evidence** for numbers in the plans. Plans keep their quoted numbers; only reproducibility from the exact files is lost, and git history keeps the tracked ones.

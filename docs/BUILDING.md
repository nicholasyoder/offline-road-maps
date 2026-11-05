# Building Offline Road Maps

This guide covers everything you can build from the source:

1. **A map** (a *pack*) of a region: a city's surroundings, a state or province, or all of North America. [Part 3](#3-build-a-map).
2. **The program** for your own computer. [Part 2](#2-build-the-program).
3. **A release folder** for a USB stick: both programs (Windows and Linux), the maps and the readme, ready to copy. [Part 4](#4-make-a-release-folder), and [Part 5](#5-build-the-program-on-windows) for building on a Windows machine instead.

You do not need all three. To try the app you only need a map and the program; Delaware builds in seconds.

## 1. What you need

### Software

- **Rust**, installed with [rustup](https://rustup.rs/). The repository's `rust-toolchain.toml` names the version (1.98); rustup fetches it the first time you run `cargo` inside the repository.
- A C compiler and linker. On Debian or Ubuntu: `sudo apt install build-essential curl git`. On Windows: see [Part 5](#5-build-the-program-on-windows).
- For the program on Linux, the graphics libraries any desktop already has (Vulkan, X11 or Wayland). Nothing else.
- For maps: an internet connection while the data is downloaded. Building, and everything after, is offline.

### Hardware, for maps

Measured on the project's development machine (16 cores, 62 GB of RAM, a fast SSD for the program and an exFAT disk for the data). "Peak RAM" is the builder's peak resident memory; give the machine that much plus whatever the rest of the desktop uses.

| Region | Source data | Build time | Peak RAM | Temporary disk | Map size |
|---|---:|---:|---:|---:|---:|
| Delaware | 22 MB | 12 s | 2.2 GiB | 9 MiB | 15 MiB |
| Ohio | 325 MB | 1 min | 5.0 GiB | 124 MiB | 193 MiB |
| Quebec (with `--bands 3`) | 1.2 GB | 2 min | 5.8 GiB | | 597 MiB |
| North America (`--bands 3`, all addresses) | 19.5 GB | 45 min | 43.5 GiB | 7.6 GiB | 9.4 GiB |

The downloaded inputs are bigger than the OpenStreetMap extract alone, because addresses come from other sources. For all of North America they come to about **35 GB**: 19 GB OpenStreetMap, 10 GB OpenAddresses, 3.5 GB TIGER (US street address ranges), 2 GB Statistics Canada, 1 GB ocean polygons. Count on 50 GB of free disk for inputs, temporary files and the result.

- **RAM is the limit for big regions.** North America needs 44 GiB at its peak with `--bands 3` and does not fit on a 32 GB machine. `--bands` cuts the map in that many passes over the input, trading time for memory; more bands help less than you would hope (a state's peak barely changes). A state or province builds comfortably on a 16 GB laptop.
- Use a disk without a small file-size limit for the data (not FAT32). exFAT, ext4 and NTFS all work. The builder does not need sparse files or permissions.
- The finished map is a set of files of at most 4 GiB each (`name.omp.000`, `.001`, …), so it also fits on a FAT32 stick, but see [the stick's file system](#the-sticks-file-system).

### Where things go

The builder keeps everything in one *work directory*: downloads in `inputs/`, temporary tiles in `tmp/`, finished maps in `packs/`. Set it once:

```sh
export OMP_WORK=/path/to/a/disk/with/space     # Windows (cmd): set OMP_WORK=D:\maps
```

(Without it the builder uses `./data`. `omp-builder paths` prints what it resolved.)

## 2. Build the program

```sh
git clone https://github.com/nicholasyoder/offline-road-maps.git
cd offline-road-maps
cargo build --release -p omp-app                      # the program: target/release/OfflineRoadMaps
cargo build --profile release-builder -p omp-builder   # the map builder: target/release-builder/omp-builder
```

The first build takes a few minutes. Start the program with a map:

```sh
target/release/OfflineRoadMaps --pack $OMP_WORK/packs/delaware
```

Without `--pack` it opens `maps/` next to the program, else the map used last. `OfflineRoadMaps --help` lists every option.

A program built this way runs on machines with the same or a newer version of the C library (glibc) than yours. To make a Linux program that also runs on older distributions, and the Windows program, use [Part 4](#4-make-a-release-folder).

The commands in the rest of this guide call the builder `omp-builder`. Put the binary on your path, or use `cargo run --profile release-builder -p omp-builder -- <arguments>` in its place.

```sh
alias omp-builder=$PWD/target/release-builder/omp-builder
```

## 3. Build a map

The builder takes an OpenStreetMap extract (`.osm.pbf`), cuts it into map tiles, builds the search index, the address tables and the routing data, and writes one pack. It needs the extract and, for addresses, the address files; `omp-builder fetch` downloads both.

### 3.1 The worked example: Delaware

Download the data (about 1 GB, mostly the world's ocean polygons, which every region shares):

```sh
omp-builder fetch --region us/delaware
```

This puts into `$OMP_WORK/inputs/`: the OpenStreetMap extract from [Geofabrik](https://download.geofabrik.de/) (`osm/delaware-261001.osm.pbf`; the number is the extract's date, 2026-10-01), the ocean polygons from osmdata.openstreetmap.de (if that host does not answer, `fetch` tries its unofficial North America mirror, osmdata.org), the state's boundary, its OpenAddresses files, the US Census TIGER address ranges, and the time zone polygons. It records every file in `inputs/manifest.toml` and will not download a file twice. `--dry-run` lists what it would fetch.

Build the map:

```sh
omp-builder build $OMP_WORK/inputs/osm/delaware-261001.osm.pbf \
    --out $OMP_WORK/packs/delaware --addresses US-DE
```

It prints each stage as it goes and finishes with the pack's size (12 seconds, a 15 MiB pack). `--addresses US-DE` merges that state's address files with the addresses in OpenStreetMap itself; without `--addresses` the map has streets and places but no house numbers.

Check the result:

```sh
omp-builder inspect $OMP_WORK/packs/delaware    # header, sections, the sources and their licences
omp-builder verify  $OMP_WORK/packs/delaware    # re-reads every section and checks its checksum
```

Then look at it: `OfflineRoadMaps --pack $OMP_WORK/packs/delaware`.

### 3.2 A state or a province

The same two commands with another region. Region names are [Geofabrik's](https://download.geofabrik.de/north-america.html): `us/ohio`, `us/california`, `ontario`, `quebec`, … The `--addresses` code is the state's or province's ISO 3166-2 code.

```sh
omp-builder fetch --region us/ohio
omp-builder build $OMP_WORK/inputs/osm/ohio-261001.osm.pbf --out $OMP_WORK/packs/ohio --addresses US-OH

# A Canadian province: addresses come from Statistics Canada's National Address Register
omp-builder fetch --region quebec
omp-builder build $OMP_WORK/inputs/osm/quebec-261001.osm.pbf --out $OMP_WORK/packs/quebec --addresses CA-QC --bands 3
```

`--addresses` can be given more than once (`--addresses US-OH --addresses US-IN`) for an extract that covers several states; fetch each state's data first with its own `fetch --region`. A province's addresses come from the National Address Register and OpenAddresses, which `fetch` gets by default; the default also fetches the 2021 Open Database of Addresses, which `build` does not use unless asked.

### 3.3 All of North America

```sh
omp-builder fetch --region north-america --sources osm,water,poly,openaddresses,tiger,nar,tz
omp-builder build $OMP_WORK/inputs/osm/north-america-261001.osm.pbf \
    --out $OMP_WORK/packs/north-america --addresses all --bands 3 --deterministic
```

Download about 35 GB first (it takes hours; `--jobs` sets the parallel downloads). The build then takes about 45 minutes and **44 GiB of RAM at its peak**: close other programs, and if the machine has less than 48 GB of RAM it will not finish (use a smaller region). The `--sources` list leaves out the 2021 Open Database of Addresses, which the build does not use. `--deterministic` makes the same inputs give a byte-identical pack, which is how you can check a rebuild; leave it out if you do not care. The result is three files of 4 GiB, 4 GiB and 1.2 GiB.

To keep a machine usable during the build, run it under a memory cap, for example `systemd-run --user --scope -p MemoryMax=56G -p MemorySwapMax=0 omp-builder build …`.

### 3.4 A region you draw yourself

A region does not have to follow a state's border. You give the builder a polygon, it cuts the extract to it, and the map covers just that area (a metro area, a county, a route corridor). Ocean and addresses are clipped to the same polygon.

1. **Draw the polygon** as an Osmosis `.poly` file: a name, then one or more rings of `longitude latitude` lines, closed by repeating the first point, ending with `END` lines. [docs/examples/dover.poly](examples/dover.poly) is a small one (a box around Dover, Delaware). Tools that write the format: JOSM, [polygons.openstreetmap.fr](https://polygons.openstreetmap.fr/) (from an OpenStreetMap boundary), or QGIS (export to `.poly` with a plugin). Geofabrik's `.poly` files for each state are in `$OMP_WORK/inputs/poly/` after `fetch`, which you can edit.
2. **Download an extract that contains the polygon**: the state, or North America, as in 3.2 and 3.3. Download the address files of every state the polygon touches.
3. **Cut the extract to the polygon** with [osmium-tool](https://osmcode.org/osmium-tool/) (`sudo apt install osmium-tool`; for Windows and macOS see its site). `osmium` is not part of this project:

   ```sh
   osmium extract --polygon docs/examples/dover.poly --set-bounds \
       $OMP_WORK/inputs/osm/delaware-261001.osm.pbf -o $OMP_WORK/inputs/osm/dover.osm.pbf
   ```

   (A custom extract from [BBBike](https://extract.bbbike.org/) or any other service works too, as long as the file has its bounding box in the header, which the builder checks.)
4. **Build with the same polygon**:

   ```sh
   omp-builder build $OMP_WORK/inputs/osm/dover.osm.pbf --out $OMP_WORK/packs/dover \
       --name Dover --clip-poly docs/examples/dover.poly --addresses US-DE
   ```

   This takes 7 seconds and gives a 1.3 MiB map. `--name` is what the map is called in the program. `--clip-poly` keeps only the ocean and the addresses inside the polygon; without it the pack would carry all of the state's addresses as points with no roads around them.

`--clip-poly` does not cut the roads and land by itself: that is what step 3 is for. Given alone on a whole state's extract it would build the whole state, with addresses only inside the polygon.

### 3.5 Using a map

- **From a folder:** `OfflineRoadMaps --pack <the pack's folder>`, or ☰ → *Open map* (Ctrl+O) in the program.
- **Next to the program:** put the pack's folder in a folder named `maps` next to `OfflineRoadMaps` (`maps/delaware/delaware.omp.000`). The program opens it at start, or lists the packs there if there are several.
- **On a stick:** see Part 4.

The map is read in place from the disk and never loaded into memory whole, so a 9 GB map opens at once. Copy a pack by copying its folder; the `.omp.00x` files belong together.

## 4. Make a release folder

A release is the folder you copy onto a stick or zip: both programs, the maps, the licence notices and `README.txt`. You make it on Linux, with three scripts in `tools/`. Each of the first two needs a few tools, installed once.

### 4.1 Tools

Install the C compiler and the LLVM tools (the Windows build links with them and adds the program's icon):

```sh
sudo apt install build-essential clang llvm curl
rustup target add x86_64-pc-windows-msvc
```

`cargo install` puts programs in `~/.cargo/bin`. If your `cargo` is Debian's own package rather than rustup's, that folder may not be on your `PATH`; the scripts add it themselves.

**For the Windows program: `cargo-xwin`.**

```sh
cargo install cargo-xwin --locked
```

On first use it downloads Microsoft's Windows libraries and headers (about 1 GB, into `~/.cache/cargo-xwin`) and asks you to accept Microsoft's licence. Read it and answer when `tools/build-windows.sh` asks.

**For the Linux program: `zig` and `cargo-zigbuild`.** A plain `cargo build --release` links this machine's C library. A program built on Debian 13 needs glibc 2.39 and does not start on Ubuntu 22.04, Linux Mint 21 or Debian 12. `tools/build-linux.sh` builds against glibc 2.35 instead, with zig as the linker. Debian does not package zig; download it from ziglang.org. It is one folder and nothing else is installed. One command at a time:

1. Make two folders, and go to the first:

   ```sh
   mkdir -p ~/.local/opt ~/.local/bin
   cd ~/.local/opt
   ```

2. Download zig 0.15.2:

   ```sh
   curl -fLO https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz
   ```

3. Check the download. The command must print `OK`:

   ```sh
   echo "02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239  zig-x86_64-linux-0.15.2.tar.xz" | sha256sum -c -
   ```

   (The checksum comes from <https://ziglang.org/download/index.json>.) If it says `FAILED`, delete the file and start again at step 2.

4. Unpack it and delete the download:

   ```sh
   tar xf zig-x86_64-linux-0.15.2.tar.xz
   rm zig-x86_64-linux-0.15.2.tar.xz
   ```

5. Make `zig` a command. Use a link, not a copy: zig looks for its library files next to itself.

   ```sh
   ln -sfn ~/.local/opt/zig-x86_64-linux-0.15.2/zig ~/.local/bin/zig
   ```

6. Check it (open a new terminal if the command is not found; `~/.local/bin` must be on your `PATH`):

   ```sh
   zig version
   ```

   It prints `0.15.2`. To change the version later, repeat steps 2 to 5 with the new version's name and checksum.

Then:

```sh
cargo install cargo-zigbuild --locked
```

If zig 0.15.2 gives an error that mentions zig itself, try the newest release at <https://ziglang.org/download/> the same way.

### 4.2 Build the two programs

From the repository folder:

```sh
tools/build-windows.sh    # target/x86_64-pc-windows-msvc/release/OfflineRoadMaps.exe
tools/build-linux.sh      # target/x86_64-unknown-linux-gnu/release/OfflineRoadMaps
```

Both take a few minutes. The Linux script ends by printing `needs glibc 2.35` (or lower), and stops with an error if the program needs a newer glibc than that. The Windows program is built with the C runtime linked in, so a Windows PC needs nothing installed to run it.

### 4.3 Put it together

You need a finished map folder (Part 3) and the builder (Part 2). Format the stick exFAT and mount it. Then:

```sh
tools/package.sh --pack $OMP_WORK/packs/north-america /media/you/STICK
```

`--pack` can be repeated for several maps; each goes in `maps/<its folder name>/`. The script lays out the release folder (below), copies the maps and compares every copied file with its source by checksum (a stick that drops a write shows up here), runs `omp-builder verify` on the copies, and writes the third-party licence notices into `licenses/`. It refuses a target that is not empty. `tools/package.sh --help` lists the rest (`--no-windows`, `--linux <file>`, `--windows <file>`, `--builder <file>`, `--no-verify`). Copying North America to a USB 3 stick takes about 8 minutes.

```
OfflineRoadMaps.exe        Windows
OfflineRoadMaps.sh         Linux: the one thing to run
README.txt                 for the person who gets the stick
linux/                     the Linux program, and an example menu entry
maps/north-america/        the map and its SHA256SUMS
licenses/                  this program's and every dependency's licences
config.toml, trips/, pins/ made by the program on first use, on the stick
```

The version in `README.txt` is the one in `Cargo.toml` (`workspace.package.version`).

### The stick's file system

Use **exFAT**: it holds files over 4 GB, Windows, Linux and macOS read it, and Linux can run the program from it (the launcher also copes when it cannot). FAT32 also works, but the program is copied to `~/.cache/OfflineRoadMaps/` at each start on Linux, and on FAT32 the first read deep into each map file walks the file's cluster chain, which takes seconds on a USB 2 stick.

## 5. Build the program on Windows

You can build the Windows program on a Windows machine too, which is how CI builds it (`.github/workflows/ci.yml`).

1. Install **Build Tools for Visual Studio** (<https://visualstudio.microsoft.com/downloads/>, "Tools for Visual Studio") with the workload *Desktop development with C++*. It provides the linker, the C runtime and `rc.exe`, which adds the icon.
2. Install **rustup** from <https://rustup.rs/> (the 64-bit installer, the default MSVC toolchain), and **Git** from <https://git-scm.com/download/win>.
3. In a new terminal:

   ```bat
   git clone https://github.com/nicholasyoder/offline-road-maps.git
   cd offline-road-maps
   cargo build --release -p omp-app
   cargo build --profile release-builder -p omp-builder
   ```

   rustup installs the toolchain `rust-toolchain.toml` names on the first `cargo` command. `.cargo/config.toml` links the C runtime in statically.

4. The programs are `target\release\OfflineRoadMaps.exe` and `target\release-builder\omp-builder.exe`. To make a stick on Windows, copy `OfflineRoadMaps.exe` and a `maps\` folder with the map (`maps\<name>\<name>.omp.000` …) into one folder; the program keeps its settings, trips and pins in the folder it is in. The release layout of Part 4, with the Linux program, the launcher and the licence file, needs `tools/package.sh`, which runs on Linux.

Run the program started from a terminal with `start /wait OfflineRoadMaps.exe …` (cmd) or `Start-Process -Wait -NoNewWindow .\OfflineRoadMaps.exe -ArgumentList '…'` (PowerShell) if you want its text output: a release build opens no console of its own.

## 6. Check your changes

CI runs these, and so should you before committing:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 tools/licenses.py /tmp/THIRD-PARTY.txt     # every dependency's licence is one we can ship
```

[DEVELOPING.md](DEVELOPING.md) describes the program's developer modes (`--render`, `--bench`).

## Appendix: environment variables

`OMP_WORK` is the one a user needs. The others are diagnostics and test switches; every one the code reads is here.

| Variable | Read by | Meaning |
|---|---|---|
| `OMP_WORK` | builder, scripts | The work directory (inputs, temporary files, packs). Default `./data`. |
| `OMP_LOG` | app | Log level: `error`, `warn`, `info`, `debug`, `trace`. Default `warn` (`info` in some modes). |
| `OMP_PRINT_DPI` | app | Overrides the print resolution (default up to 300 dpi), for comparisons. |
| `OMP_PRINT_OWN_DEVICE` | app | If set, the print worker opens its own graphics device instead of sharing the window's. |
| `OMP_BAND_ROWS` | app | Rows per band when a print page's map is drawn in bands; the checks use it. |
| `OMP_BENCH_SHOTS` | app, `--bench` | A directory: the bench saves screenshots of long holds, suggestions and results there. |
| `OMP_BENCH_VSYNC` | app, `--bench` | If set, the bench keeps vsync on (it turns it off otherwise, since frame times mean nothing with it). |
| `OMP_BENCH_NO_MAP` | app, `--bench` | If set, everything but drawing the map, to see what the rest costs. |
| `OMP_WINDOW` | app, `--bench` | `WxH`: the size the bench opens its window with instead of the flight's. |
| `OMP_RESIZABLE` | app, `--bench` | If set, the bench window can be resized (it is fixed otherwise). |
| `OMP_PRESENT` | app | Presentation mode for memory checks: `fifo`, `mailbox`, `immediate` or `auto`. |
| `OMP_LATENCY` | app | Frames the swapchain may queue (with `OMP_PRESENT`). |
| `OMP_SURVIVE` | app | If set, graphics errors that nothing handles are logged instead of ending the program. |
| `OMP_TRACE` | app | If set, prints start-up diagnostics (for the crash hunt of [phase 5](dev/phase5-plan.md), step 5.14). |
| `OMP_TRACE_GPU` | app | If set, waits for the GPU at the start of each early frame, to find which frame fails. |
| `OMP_TEST_PACK` | tests | A small one-file pack with every section, for the damaged-pack tests (`cargo test -p omp-core -- --ignored`). |
| `OMP_TEST_PACK2` | tests | A second build of the same region, for the test of opening a trip on another build. |
| `OMP_DAMAGE_TRIALS` | tests | Trials per section and kind in the damaged-pack tests (default 10). |
| `OMP_DAMAGE_SECTIONS` | tests | Limits the damaged-pack tests to some sections (`RCHE,SENT`). |

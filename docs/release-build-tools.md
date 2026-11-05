# Tools for building a release on Linux

*Draft, 2026-11-05. Written for [phase 6 step 6.5](phase6-plan.md#65-docs), which folds it into `docs/BUILDING.md`. Tested on Debian 13; every step has been run through.*

A release folder holds two programs: one for Linux and one for Windows. You build both on a Linux machine, with two scripts in `tools/`. Each needs a few tools installed once. This guide installs them.

You need: a Debian or Ubuntu machine, an internet connection for the setup (not for later builds), and about 3 GB of disk for the tools. You do not need root except in step 1.

## 1. The basics

Install a C compiler and the LLVM tools (the Windows build uses them to link and to add the icon):

```sh
sudo apt install build-essential clang llvm curl
```

Install Rust the way the project expects. The repository's `rust-toolchain.toml` names the version, and `rustup` fetches it the first time you run `cargo` inside the repository. If you use Debian's own `cargo` package instead, it must be the same version.

Add the Windows target:

```sh
rustup target add x86_64-pc-windows-msvc
```

Check: `cargo --version` and `clang --version` both print a version.

## 2. Where `cargo install` puts things

`cargo install` puts programs in `~/.cargo/bin`. If `cargo` came from Debian's package, that folder may not be on your `PATH`. The build scripts add it themselves, so you do not need to fix this. To use those programs by hand, add this line to `~/.bashrc` and open a new terminal:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

## 3. The Windows build

Install `cargo-xwin`. It downloads Microsoft's Windows libraries and headers the first time it is used, so the program can be built for Windows on Linux.

```sh
cargo install cargo-xwin --locked
```

Then build:

```sh
tools/build-windows.sh
```

The first run asks you to accept Microsoft's licence for those libraries. Read it and answer. It then downloads about 1 GB into `~/.cache/cargo-xwin`; later builds reuse it.

Result: `target/x86_64-pc-windows-msvc/release/OfflineRoadMaps.exe`, with the app's icon.

## 4. Zig, for the Linux build

A plain `cargo build --release` links against the glibc of the machine it runs on. A Linux program built on Debian 13 does not start on Ubuntu 22.04, Linux Mint 21 or Debian 12, because their glibc is older. `tools/build-linux.sh` builds against glibc 2.35 instead, using **zig** as the linker. Debian does not package zig, so you download it from ziglang.org. It is one folder; nothing else is installed.

Run these one at a time.

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

5. Make `zig` available as a command. Use a link, not a copy: zig looks for its library files next to itself.

   ```sh
   ln -sfn ~/.local/opt/zig-x86_64-linux-0.15.2/zig ~/.local/bin/zig
   ```

6. Check it. Open a new terminal if the command is not found (`~/.local/bin` must be on your `PATH`; on Debian it is, once the folder exists at login):

   ```sh
   zig version
   ```

   It prints `0.15.2`.

To change the version later, repeat steps 2 to 5 with the new version number and checksum; the link in step 5 then points at the new folder.

## 5. The Linux build

Install `cargo-zigbuild`, which runs cargo with zig as the linker:

```sh
cargo install cargo-zigbuild --locked
```

Then build, from the repository folder:

```sh
tools/build-linux.sh
```

The script ends by printing `needs glibc 2.35` (or lower) and stops with an error if the program needs a newer glibc than that. Result: `target/x86_64-unknown-linux-gnu/release/OfflineRoadMaps`.

If zig 0.15.2 gives an error that mentions zig itself, try the newest release listed at <https://ziglang.org/download/>, the same way as in step 4.

## 6. Making the stick

With both programs built and a finished map in a folder (for example `$OMP_WORK/packs/north-america`):

```sh
tools/package.sh --pack /path/to/packs/north-america /media/you/STICK
```

The stick should be exFAT and empty. The script copies the map, compares the copy with the original, and checks it with the builder. `tools/package.sh --help` lists the options.

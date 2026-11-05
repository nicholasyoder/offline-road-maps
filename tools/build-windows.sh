#!/bin/bash
# Builds OfflineRoadMaps.exe for Windows (x86_64-pc-windows-msvc, static CRT) on Linux,
# into target/x86_64-pc-windows-msvc/release/. Same target and flags as the CI build
# (.cargo/config.toml).
#
# Needs, once:  rustup target add x86_64-pc-windows-msvc
#               cargo install cargo-xwin --locked
#               (cargo-xwin downloads Microsoft's CRT and Windows SDK headers/libraries on
#               first use, into ~/.cache/cargo-xwin, and asks you to accept their licence
#               with XWIN_ACCEPT_LICENSE=1 or the prompt; clang and llvm tools from your
#               distribution do the compiling; llvm-rc makes the icon resource.)
set -euo pipefail
cd "$(dirname "$0")/.."
# cargo install puts cargo-xwin in ~/.cargo/bin, which a distribution's cargo may not have on PATH.
PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
command -v cargo-xwin >/dev/null || { echo "build-windows.sh: cargo-xwin is not installed (see the top of this file)" >&2; exit 1; }
# Debian keeps llvm-lib, llvm-rc and friends (which cc-rs and winresource look for by their
# plain names) only in /usr/lib/llvm-N/bin; put the newest one last on PATH.
for d in /usr/lib/llvm-*/bin; do [ -d "$d" ] && llvm_bin=$d; done
[ -z "${llvm_bin:-}" ] || PATH="$PATH:$llvm_bin"
cargo xwin build --release --target x86_64-pc-windows-msvc -p omp-app "$@"
ls -l target/x86_64-pc-windows-msvc/release/OfflineRoadMaps.exe

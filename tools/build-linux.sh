#!/bin/bash
# Builds the Linux OfflineRoadMaps against glibc 2.35 (Ubuntu 22.04, Linux Mint 21, Debian 12
# and newer) whatever this machine's glibc is, into
# target/x86_64-unknown-linux-gnu/release/. A plain `cargo build --release` links this
# machine's glibc (2.39 on Debian 13), and the binary then does not start on older systems.
#
# Needs, once:  cargo install cargo-zigbuild --locked
#               zig (cargo-zigbuild uses its C compiler and linker): e.g. `pipx install
#               ziglang`, or a release from https://ziglang.org/download/ unpacked somewhere
#               on PATH.
set -euo pipefail
cd "$(dirname "$0")/.."
GLIBC=2.35
# cargo install puts cargo-zigbuild in ~/.cargo/bin, which a distribution's cargo may not have on PATH.
PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
command -v cargo-zigbuild >/dev/null || { echo "build-linux.sh: cargo-zigbuild is not installed (see the top of this file)" >&2; exit 1; }
command -v zig >/dev/null || { echo "build-linux.sh: zig is not installed (see the top of this file)" >&2; exit 1; }
cargo zigbuild --release --target "x86_64-unknown-linux-gnu.$GLIBC" -p omp-app "$@"
bin=target/x86_64-unknown-linux-gnu/release/OfflineRoadMaps
ls -l "$bin"

# The check that matters: the newest glibc symbol the binary asks for.
need=$(objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -n1)
echo "needs glibc $need"
if [ "$(printf '%s\n%s\n' "$need" "$GLIBC" | sort -V | tail -n1)" != "$GLIBC" ]; then
    echo "build-linux.sh: the binary needs glibc $need, newer than $GLIBC" >&2
    exit 1
fi

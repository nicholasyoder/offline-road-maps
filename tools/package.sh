#!/bin/bash
# Builds a release folder (the layout of docs/phase6-plan.md, step 6.3) for a USB stick
# or a zip. Run on Linux.
#
#   tools/package.sh [options] --pack <pack dir> [--pack <pack dir> ...] <out dir>
#
#   --pack <dir>      a pack's folder (the one holding name.omp.000); copied to
#                     <out>/maps/<folder name>/. At least one.
#   --linux <file>    the Linux binary   (default target/x86_64-unknown-linux-gnu/release/OfflineRoadMaps,
#                     from tools/build-linux.sh: glibc 2.35 or newer)
#   --windows <file>  the Windows exe    (default target/x86_64-pc-windows-msvc/release/OfflineRoadMaps.exe)
#   --no-windows      leave the Windows exe out (otherwise it must exist)
#   --builder <file>  omp-builder, to verify the copied packs (default
#                     target/release-builder/omp-builder; built if missing)
#   --no-verify       skip the verification (the copy is still compared by checksum)
#
# <out dir> may be the stick's mount point or a folder on it; it must be empty or new.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
linux_bin="$root/target/x86_64-unknown-linux-gnu/release/OfflineRoadMaps"
win_exe="$root/target/x86_64-pc-windows-msvc/release/OfflineRoadMaps.exe"
builder="$root/target/release-builder/omp-builder"
use_windows=1
verify=1
packs=()
out=

die() { echo "package.sh: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case $1 in
        --pack) [ $# -ge 2 ] || die "--pack needs a value"; packs+=("$2"); shift 2 ;;
        --linux) [ $# -ge 2 ] || die "--linux needs a value"; linux_bin=$2; shift 2 ;;
        --windows) [ $# -ge 2 ] || die "--windows needs a value"; win_exe=$2; shift 2 ;;
        --no-windows) use_windows=0; shift ;;
        --builder) [ $# -ge 2 ] || die "--builder needs a value"; builder=$2; shift 2 ;;
        --no-verify) verify=0; shift ;;
        -h|--help) sed -n '2,/^set -e/p' "$0" | sed '$d;s/^# \{0,1\}//'; exit 0 ;;
        -*) die "unknown option $1" ;;
        *) [ -z "$out" ] || die "more than one output folder"; out=$1; shift ;;
    esac
done
[ -n "$out" ] || die "no output folder (see --help)"
[ ${#packs[@]} -gt 0 ] || die "no --pack given"
[ -f "$linux_bin" ] || die "$linux_bin not found (tools/build-linux.sh builds it)"
if [ $use_windows = 1 ] && [ ! -f "$win_exe" ]; then
    die "$win_exe not found (tools/build-windows.sh builds it; or pass --no-windows)"
fi
for p in "${packs[@]}"; do
    ls "$p"/*.omp.000 >/dev/null 2>&1 || die "$p holds no pack (no *.omp.000)"
done
if [ -d "$out" ] && [ -n "$(ls -A "$out")" ]; then die "$out is not empty"; fi

version=$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$root/Cargo.toml")
[ -n "$version" ] || die "no version in Cargo.toml"

mkdir -p "$out/linux" "$out/maps" "$out/licenses"
install -m 755 "$linux_bin" "$out/linux/OfflineRoadMaps"
[ $use_windows = 0 ] || install -m 755 "$win_exe" "$out/OfflineRoadMaps.exe"
install -m 755 "$root/tools/release/OfflineRoadMaps.sh" "$out/OfflineRoadMaps.sh"
install -m 644 "$root/crates/app/data/linux/OfflineRoadMaps.desktop" \
    "$root/crates/app/data/linux/OfflineRoadMaps.png" "$out/linux/"
sed "s/@VERSION@/$version/" "$root/tools/release/README.txt" > "$out/README.txt"
install -m 644 "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$out/licenses/"
install -m 644 "$root/crates/app/data/fonts/OFL.txt" "$out/licenses/NotoSans-OFL.txt"
python3 -I "$root/tools/licenses.py" "$out/licenses/THIRD-PARTY.txt"

# The packs, each copy compared with its source by checksum (a stick that dropped a write
# shows up here, not in the car).
sums=$(mktemp -d)
trap 'rm -rf "$sums"' EXIT
for p in "${packs[@]}"; do
    name=$(basename "$(cd "$p" && pwd)")
    dest="$out/maps/$name"
    echo "Copying $name ..."
    mkdir -p "$dest"
    (cd "$p" && sha256sum *.omp.[0-9][0-9][0-9]) > "$sums/$name"
    for f in "$p"/*.omp.[0-9][0-9][0-9]; do
        cp "$f" "$dest/"
    done
    sync
    echo "Checking the copy of $name ..."
    (cd "$dest" && sha256sum -c --quiet "$sums/$name") || die "the copy of $name differs from its source"
    cp "$sums/$name" "$dest/SHA256SUMS"
done

if [ $verify = 1 ]; then
    if [ ! -x "$builder" ]; then
        echo "Building omp-builder ..."
        (cd "$root" && cargo build --profile release-builder -p omp-builder)
    fi
    for p in "$out"/maps/*/; do
        echo "Verifying $(basename "$p") ..."
        "$builder" verify "$p"
    done
fi

echo "Done: $out (version $version)"
du -sh "$out" | cut -f1

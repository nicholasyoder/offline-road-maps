#!/bin/sh
# Starts Offline Road Maps on Linux:  sh OfflineRoadMaps.sh
#
# The program is linux/OfflineRoadMaps, next to this file. Run through `sh` this works
# whatever the stick or disk is mounted with. If the system will not run programs from
# where this folder is (FAT32 sticks and "noexec" mounts), the program is copied to
# ~/.cache/OfflineRoadMaps/ and run from there. Either way the maps, settings, trips and
# pins are the ones in this folder.

here=$(cd "$(dirname "$0")" && pwd) || exit 1
bin="$here/linux/OfflineRoadMaps"

if [ ! -f "$bin" ]; then
    echo "Offline Road Maps: $bin is missing." >&2
    exit 1
fi

# Try to run it in place; 126 is the shell's "found but not allowed to execute".
"$bin" --help >/dev/null 2>&1
if [ $? -ne 126 ]; then
    exec "$bin" --data-dir "$here" "$@"
fi

cache="${XDG_CACHE_HOME:-$HOME/.cache}/OfflineRoadMaps"
copy="$cache/OfflineRoadMaps"
mkdir -p "$cache" || exit 1
if ! cmp -s "$bin" "$copy"; then
    echo "Copying the program to $cache (this disk does not allow running it in place)..." >&2
    cp "$bin" "$copy.new" && chmod +x "$copy.new" && mv "$copy.new" "$copy" || exit 1
fi
exec "$copy" --data-dir "$here" "$@"

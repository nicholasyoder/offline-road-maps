#!/bin/sh
# Renders the reference screenshots (phase1-plan.md 1.5 and 1.6) from the North America pack.
#
#   docs/screenshots/render.sh [pack dir]
#
# Default pack: $OMP_WORK/packs/north-america. Needs a release build of omp-app.
set -eu
cd "$(dirname "$0")"
app=../../target/release/omp-app
pack=${1:-${OMP_WORK:-/media/nicholas/Data/maps-testing}/packs/north-america}

shot() { # name lat,lon zoom
    "$app" --render --pack "$pack" --center "$2" --zoom "$3" --size 1000x700 "$1.png"
}

shot continent 45,-98 3.5
shot arctic-z5 70,-95 5
shot rockies-z8 39.7,-105.6 8
shot ohio-rural-z9 40.55,-83.15 9
shot chicago-z10 41.88,-87.68 10
shot sf-bay-z10 37.75,-122.3 10
shot mexico-city-z11 19.43,-99.13 11
shot montreal-z12 45.51,-73.6 12
shot ontario-lakes-z12 45.05,-79.3 12
shot manhattan-z14 40.758,-73.985 14
shot manhattan-z17 40.7505,-73.9934 17

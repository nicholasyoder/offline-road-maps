#!/bin/sh
# Renders the reference screenshots (phase1-plan.md 1.5 to 1.8, phase3-plan.md 3.6) from the
# North America pack, with the built-in style.
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
# Route shields: Ohio, Ontario and Quebec (1.8).
shot columbus-z10 39.96,-83.0 10
shot toronto-z10.5 43.72,-79.45 10.5
shot montreal-z10.5 45.52,-73.65 10.5
# Routes, the view fitted to each (phase 3 steps 3.6 and 3.7). They need a pack with routing (format
# 0.5), so they come last.
route() { # name stop;stop…
    "$app" --render --pack "$pack" --route "$2" --size 1000x700 "$1.png"
}
route route-cincinnati-cleveland "39.1031,-84.5120;41.4993,-81.6944"
route route-columbus-z14 "39.9612,-82.9988;39.9800,-83.0300"
route route-quebec-tadoussac "46.8139,-71.2080;48.1400,-69.7200"
route route-montreal "45.5017,-73.5673;45.5300,-73.6200"
route route-miami-seattle "25.7617,-80.1918;47.6062,-122.3321"
# Over the route (step 3.7): a dashed line to a stop off the road, a stretch on a private
# road (decision 14), and a step of the directions picked.
route route-cleveland-lake "41.48,-81.70;41.53,-81.72"
"$app" --render --pack "$pack" --route "38.2527,-85.7585;38.23733,-85.60603" \
    --center 38.2385,-85.6045 --zoom 15.5 --size 1000x700 route-louisville-private.png
"$app" --render --pack "$pack" --route "39.1031,-84.5120;41.4993,-81.6944" --step 6 \
    --size 1000x700 route-step.png

#!/bin/sh
# Rebuilds the bundled fonts: Noto Sans Regular, Italic and Bold, subset to Latin
# (phase1-plan.md 1.7). Needs fontTools (`pip install fonttools`).
#
# Usage: subset.sh <dir with NotoSans-v2.015.zip unpacked>
# From https://github.com/notofonts/latin-greek-cyrillic/releases/tag/NotoSans-v2.015
# (zip sha256 0c34df072a3fa7efbb7cbf34950e1f971a4447cffe365d3a359e2d4089b958f5).
set -eu
src="$1/NotoSans/unhinted/ttf"
out="$(dirname "$0")"
# Basic Latin, Latin-1, Latin Extended-A and -B, Latin Extended Additional, and the
# punctuation and symbols names use: dashes, quotes, bullet, ellipsis, prime, euro, numero.
unicodes="U+0020-007E,U+00A0-024F,U+1E00-1EFF,U+2010-2027,U+2030-203A,U+20AC,U+2116"
for style in Regular Italic Bold; do
    python3 -m fontTools.subset "$src/NotoSans-$style.ttf" \
        --unicodes="$unicodes" --layout-features=kern --no-hinting \
        --output-file="$out/NotoSans-$style.ttf"
done
cp "$1/OFL.txt" "$out/OFL.txt"

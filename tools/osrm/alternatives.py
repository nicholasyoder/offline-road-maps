#!/usr/bin/env python3
"""How often osrm-routed finds alternatives for random pairs (phase 4 step 4.2), to
compare with `omp-builder route <pack> --alternatives-random`.

    alternatives.py <pairs.tsv> <url>

The pairs are `route --pairs`'s (region, id, stops, avoid). Asks for up to two
alternatives per pair and prints, by the length of OSRM's first route, the share of pairs
with one and with two, and their stretch (duration against the first route's).
"""
import json
import sys
import urllib.request

BANDS = [(0, 10), (10, 50), (50, 200), (200, 1000), (1000, float("inf"))]


def main():
    src, url = sys.argv[1:3]
    rows = []
    with open(src, encoding="utf-8") as f:
        for line in f:
            if line.startswith("#") or not line.strip():
                continue
            c = line.rstrip("\n").split("\t")
            rows.append(c[2])
    found = []
    for n, stops in enumerate(rows):
        coords = ";".join("{},{}".format(*reversed(p.split(","))) for p in stops.split(";"))
        q = f"{url}/route/v1/driving/{coords}?overview=false&alternatives=2"
        try:
            with urllib.request.urlopen(q, timeout=120) as r:
                a = json.load(r)
        except urllib.error.HTTPError as e:
            a = json.load(e)
        if a.get("code") == "Ok":
            rt = a["routes"]
            first = rt[0]["duration"] or 1.0
            found.append((rt[0]["distance"] / 1000.0, [x["duration"] / first for x in rt[1:]]))
        if (n + 1) % 100 == 0:
            print(f"{n + 1} of {len(rows)}", file=sys.stderr)
    print(f"{len(found)} of {len(rows)} pairs routed by OSRM")
    print(f"  {'km':<12} {'pairs':>5} {'>=1':>6} {'2':>6} {'stretch p50/max':>16}")
    groups = [(f"{lo}-{hi}" if hi != float("inf") else f"{lo}+", lo, hi) for lo, hi in BANDS]
    groups += [("over 50", 50, float("inf")), ("all", 0, float("inf"))]
    for name, lo, hi in groups:
        v = [alts for km, alts in found if lo <= km < hi]
        if not v:
            continue
        st = sorted(s for alts in v for s in alts)
        p50 = st[len(st) // 2] if st else float("nan")
        mx = st[-1] if st else float("nan")
        one = 100.0 * sum(len(a) >= 1 for a in v) / len(v)
        two = 100.0 * sum(len(a) >= 2 for a in v) / len(v)
        print(f"  {name:<12} {len(v):>5} {one:>5.1f}% {two:>5.1f}% {p50:>7.3f}/{mx:<7.3f}")


if __name__ == "__main__":
    main()

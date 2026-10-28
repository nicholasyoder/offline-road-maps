#!/usr/bin/env python3
"""Route a route set or random pairs through osrm-routed and save the answers (phase 3
step 3.5), so `omp-builder route <pack> --osrm <answers>` compares with OSRM without it.

    fetch.py <set-or-pairs.tsv> <region> <url> <answers.tsv>

The input's first four columns are region, id, stops (lat,lon;lat,lon;...) and avoid
(`-`, `tolls`, `ferries`, `tolls,ferries`); lines of other regions are skipped. Stock
car.lua can exclude one class at a time, so a line that avoids both is skipped.

The answers are tab-separated: id, stops, avoid, duration (s), distance (m), steps (as
OSRM counts them: a depart and an arrive per leg), and the line as an encoded polyline at
10^-6 degrees, then the number of U-turns (steps with the `uturn` modifier); `-` for each
of the last five when OSRM finds no route.
"""
import json
import sys
import urllib.request


def main():
    src, region, url, out = sys.argv[1:5]
    rows = []
    with open(src, encoding="utf-8") as f:
        for line in f:
            if line.startswith("#") or not line.strip():
                continue
            c = line.rstrip("\n").split("\t")
            if c[0] == region or region == "all":
                rows.append(c[1:4])
    excl = {"-": "", "tolls": "toll", "ferries": "ferry"}
    with open(out, "w", encoding="utf-8") as w:
        w.write("# id\tstops\tavoid\tduration_s\tdistance_m\tsteps\tpolyline6\tuturns\n")
        for n, (ident, stops, avoid) in enumerate(rows):
            if avoid not in excl:
                continue
            coords = ";".join(
                "{},{}".format(*reversed(p.split(","))) for p in stops.split(";")
            )
            q = f"{url}/route/v1/driving/{coords}?overview=full&geometries=polyline6&steps=true"
            if excl[avoid]:
                q += "&exclude=" + excl[avoid]
            try:
                with urllib.request.urlopen(q, timeout=120) as r:
                    a = json.load(r)
            except urllib.error.HTTPError as e:
                a = json.load(e)
            if a.get("code") == "Ok":
                rt = a["routes"][0]
                steps = [s for leg in rt["legs"] for s in leg["steps"]]
                uturns = sum(s["maneuver"].get("modifier") == "uturn" for s in steps)
                w.write(
                    f"{ident}\t{stops}\t{avoid}\t{rt['duration']:.1f}\t{rt['distance']:.1f}"
                    f"\t{len(steps)}\t{rt['geometry']}\t{uturns}\n"
                )
            else:
                w.write(f"{ident}\t{stops}\t{avoid}\t-\t-\t-\t-\t-\n")
            if (n + 1) % 100 == 0:
                print(f"{n + 1} of {len(rows)}", file=sys.stderr)


if __name__ == "__main__":
    main()

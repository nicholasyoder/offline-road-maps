"""Compares the ways OSRM's car.lua routes with the ways in our graph (phase 3 step 3.1).

Usage: coverage.py <osrm-extract log from car-log.lua> <omp-builder graph --way-ids file>
"""
import sys, collections
log, ours_path = sys.argv[1], sys.argv[2]
ours = set(int(l) for l in open(ours_path))
osrm = {}
for line in open(log, errors='replace'):
    if not line.startswith('W '):
        continue
    p = line.split()
    if len(p) < 3 or not p[2] in '01':
        continue
    osrm[int(p[1])] = (p[2] == '1', dict(t.split('=', 1) for t in p[3:] if '=' in t))
routed = {i for i, (ok, _) in osrm.items() if ok}
print(f"OSRM routes {len(routed)}, ours {len(ours)}, both {len(routed & ours)}")
def key(t):
    hw = t.get('highway') or ('route=' + t.get('route', '?'))
    acc = next((f"{k}={t[k]}" for k in ('motorcar', 'motor_vehicle', 'vehicle', 'access') if k in t), '')
    extra = [f"{k}={t[k]}" for k in ('service', 'oneway', 'area', 'construction', 'maxheight', 'maxwidth', 'width', 'maxweight', 'maxlength', 'narrow', 'smoothness', 'hov') if k in t]
    return ' '.join([hw, acc] + extra).strip()
for name, ids in [("OSRM only", routed - ours), ("ours only", ours - routed)]:
    print(f"\n{name}: {len(ids)}")
    c = collections.Counter(key(osrm[i][1]) if i in osrm else '(not seen by OSRM)' for i in ids)
    for k, n in c.most_common(25):
        print(f"  {n:7} {k}")

ROADS = {'motorway','trunk','primary','secondary','tertiary','motorway_link','trunk_link','primary_link','secondary_link','tertiary_link','unclassified','residential','living_street','service'}
def cause(t):
    acc = next((t[k] for k in ('motorcar', 'motor_vehicle', 'vehicle', 'access') if k in t), None)
    if 'route' in t and 'highway' not in t: return 'ferry'
    if t.get('highway') not in ROADS: return 'class ' + ('track' if t.get('highway') == 'track' else 'footway/path/other')
    if acc == 'private': return 'private'
    return 'other: ' + key(t)
for name, ids in [("OSRM only", routed - ours), ("ours only", ours - routed)]:
    c = collections.Counter(cause(osrm[i][1]) if i in osrm else 'unseen' for i in ids)
    print(name, dict(c.most_common(12)))

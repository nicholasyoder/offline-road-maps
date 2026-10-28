//! The car profile: which OSM ways and nodes a car may use, how fast, and with which avoid
//! flags. Modelled on OSRM's `car.lua` (osrm-backend 6.0, BSD-2-Clause); where it differs,
//! the phase 3 plan (step 3.1) lists the differences and why.
//!
//! Tags are passed as `(key, value)` slices, as the builder reads them from the PBF.

/// Edge flags that the avoid options exclude.
pub const TOLL: u8 = 1;
pub const FERRY: u8 = 2;

/// Seconds added when a route enters a road it may use only to start or end there
/// (`access=destination`, `delivery`, `customers`, HOV-only), from a road it may use freely.
pub const RESTRICTED_PENALTY_S: u32 = 3600;

/// Speeds are scaled by this when they come from `maxspeed`, since traffic seldom drives at
/// the limit (OSRM's `speed_reduction`).
pub const MAXSPEED_FACTOR: f64 = 0.8;

/// Ferries without a `duration`, km/h.
pub const FERRY_KMH: f64 = 20.0;
/// Added to every ferry crossing for waiting and boarding, seconds.
pub const FERRY_BOARDING_S: f64 = 600.0;

/// Delay at a `highway=traffic_signals` node, seconds (OSRM's default; tuned in step 3.5).
pub const SIGNAL_S: u32 = 2;

/// Vehicle size for `maxheight` and `maxwidth`, metres (OSRM's).
const VEHICLE_HEIGHT_M: f64 = 2.0;
const VEHICLE_WIDTH_M: f64 = 1.9;

/// The routable `highway` values, with their default speeds in km/h (OSRM's table).
pub const HIGHWAYS: &[(&str, f64)] = &[
    ("motorway", 90.0),
    ("trunk", 85.0),
    ("primary", 65.0),
    ("secondary", 55.0),
    ("tertiary", 40.0),
    ("motorway_link", 45.0),
    ("trunk_link", 40.0),
    ("primary_link", 30.0),
    ("secondary_link", 25.0),
    ("tertiary_link", 20.0),
    ("unclassified", 25.0),
    ("residential", 25.0),
    ("living_street", 10.0),
    ("service", 15.0),
];

/// The access keys, most specific first.
const ACCESS_KEYS: &[&str] = &["motorcar", "motor_vehicle", "vehicle", "access"];

/// Access values that keep a road only for starting or ending there.
const RESTRICTED: &[&str] = &["destination", "delivery", "customers"];

/// Access values that drop a road (OSRM's blacklist, less [`RESTRICTED`]).
const DENIED: &[&str] = &[
    "no",
    "private",
    "agricultural",
    "forestry",
    "emergency",
    "psv",
];

/// `service` values driven slowly (OSRM's `service_penalties`, applied to the speed here).
const SLOW_SERVICE: &[&str] = &[
    "alley",
    "parking",
    "parking_aisle",
    "driveway",
    "drive-through",
    "drive-thru",
];
const SLOW_SERVICE_FACTOR: f64 = 0.5;

/// Highest speed on a `surface`, `tracktype` or `smoothness`, km/h (OSRM's tables).
const SURFACE_KMH: &[(&str, f64)] = &[
    ("cement", 80.0),
    ("compacted", 80.0),
    ("fine_gravel", 80.0),
    ("paving_stones", 60.0),
    ("metal", 60.0),
    ("bricks", 60.0),
    ("grass", 40.0),
    ("wood", 40.0),
    ("sett", 40.0),
    ("grass_paver", 40.0),
    ("gravel", 40.0),
    ("unpaved", 40.0),
    ("ground", 40.0),
    ("dirt", 40.0),
    ("pebblestone", 40.0),
    ("tartan", 40.0),
    ("cobblestone", 30.0),
    ("clay", 30.0),
    ("earth", 20.0),
    ("stone", 20.0),
    ("rocky", 20.0),
    ("sand", 20.0),
    ("mud", 10.0),
];
const TRACKTYPE_KMH: &[(&str, f64)] = &[
    ("grade1", 60.0),
    ("grade2", 40.0),
    ("grade3", 30.0),
    ("grade4", 25.0),
    ("grade5", 20.0),
];
const SMOOTHNESS_KMH: &[(&str, f64)] = &[
    ("intermediate", 80.0),
    ("bad", 40.0),
    ("very_bad", 20.0),
    ("horrible", 10.0),
    ("very_horrible", 5.0),
    ("impassable", 0.0),
];

/// `construction=*` values that leave a road open.
const OPEN_CONSTRUCTION: &[&str] = &["no", "widening", "minor"];

/// Barriers a car passes, with their delay in seconds; every other barrier blocks.
/// Border crossings add no time (phase 3 decision 9).
const PASSABLE_BARRIERS: &[(&str, u32)] = &[
    ("cattle_grid", 0),
    ("border_control", 0),
    ("toll_booth", 10),
    ("sally_port", 10),
    ("gate", 10),
    ("lift_gate", 10),
    ("no", 0),
    ("entrance", 0),
    ("height_restrictor", 0),
    ("arch", 0),
];

fn get<'a>(tags: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn lookup(table: &[(&str, f64)], v: Option<&str>) -> Option<f64> {
    let v = v?;
    table.iter().find(|(k, _)| *k == v).map(|(_, s)| *s)
}

/// The value of the first of `keys` that is set, separately for each direction:
/// `key:forward` and `key:backward` before `key` (OSRM's `get_forward_backward_by_set`).
fn by_direction<'a>(tags: &[(&str, &'a str)], keys: &[&str]) -> [Option<&'a str>; 2] {
    let mut out = [None, None];
    for key in keys {
        for (o, dir) in out.iter_mut().zip(["forward", "backward"]) {
            if o.is_none() {
                *o = get(tags, &format!("{key}:{dir}")).or_else(|| get(tags, key));
            }
        }
        if out.iter().all(Option::is_some) {
            break;
        }
    }
    out
}

/// How a car may use one direction of a way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Direction {
    /// km/h; for a ferry with a `duration`, the way's time is [`Way::duration_s`] instead.
    pub kmh: f64,
    /// [`TOLL`] and [`FERRY`].
    pub flags: u8,
    /// Only to start or end a route there: [`RESTRICTED`] access or HOV only.
    pub restricted: bool,
}

/// A way a car may use, in at least one direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Way {
    /// Along the way's nodes, and against them.
    pub forward: Option<Direction>,
    pub backward: Option<Direction>,
    /// A ferry's crossing time from `duration`, without boarding.
    pub duration_s: Option<f64>,
    pub ferry: bool,
}

/// Why a way with a routable `highway` value, or a ferry, is not in the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// Not a road a car uses: no `highway` of [`HIGHWAYS`] and not `route=ferry`.
    NotRoad,
    /// `access` and its more specific keys deny cars both ways.
    Access,
    /// Closed: `area=yes`, under construction, proposed, impassable, reversible,
    /// `service=emergency_access`.
    Closed,
    /// Too low or narrow for a car.
    Size,
    /// No direction left (an impassable `smoothness`, a zero speed).
    Speed,
}

/// The car profile's verdict on a way's tags.
pub fn way(tags: &[(&str, &str)]) -> Result<Way, Dropped> {
    let ferry = get(tags, "route") == Some("ferry");
    let highway = get(tags, "highway");
    let class_kmh = lookup(HIGHWAYS, highway);
    if !ferry && class_kmh.is_none() {
        return Err(Dropped::NotRoad);
    }
    if closed(tags) {
        return Err(Dropped::Closed);
    }
    if too_small(tags) {
        return Err(Dropped::Size);
    }

    let access = by_direction(tags, ACCESS_KEYS);
    let allowed = access.map(|a| !a.is_some_and(|v| DENIED.contains(&v)));
    let mut restricted = access.map(|a| a.is_some_and(|v| RESTRICTED.contains(&v)));
    if !allowed[0] && !allowed[1] {
        return Err(Dropped::Access);
    }
    let hov = get(tags, "hov") == Some("designated");
    let hov_lanes = by_direction(tags, &["hov:lanes"])
        .map(|l| l.is_some_and(|l| l.split('|').all(|lane| lane == "designated")));
    for (r, lanes) in restricted.iter_mut().zip(hov_lanes) {
        *r |= hov || lanes;
    }

    let [mut fwd, mut bwd] = allowed;
    match oneway(tags, highway) {
        Oneway::Forward => bwd = false,
        Oneway::Backward => fwd = false,
        Oneway::Both => {}
    }

    let duration_s = match ferry {
        true => get(tags, "duration").and_then(parse_duration),
        false => None,
    };
    let base = match ferry {
        true => FERRY_KMH,
        false => class_kmh.unwrap_or(FERRY_KMH),
    };
    let maxspeeds = by_direction(
        tags,
        &[
            "maxspeed:advisory",
            "maxspeed",
            "source:maxspeed",
            "maxspeed:type",
        ],
    )
    .map(|v| v.and_then(parse_maxspeed).filter(|&s| s > 0.0));
    let mut cap = f64::INFINITY;
    for (table, key) in [
        (SURFACE_KMH, "surface"),
        (TRACKTYPE_KMH, "tracktype"),
        (SMOOTHNESS_KMH, "smoothness"),
    ] {
        if let Some(s) = lookup(table, get(tags, key)) {
            cap = cap.min(s);
        }
    }
    let service = match get(tags, "service") {
        Some(s) if highway == Some("service") && SLOW_SERVICE.contains(&s) => SLOW_SERVICE_FACTOR,
        _ => 1.0,
    };
    let tolls = by_direction(tags, &["toll"]).map(|t| t == Some("yes"));

    let direction = |ok: bool, i: usize| -> Option<Direction> {
        if !ok {
            return None;
        }
        let kmh = match (ferry, maxspeeds[i]) {
            (true, _) => base,
            (false, Some(m)) => m * MAXSPEED_FACTOR,
            (false, None) => base,
        };
        let kmh = kmh.min(cap) * service;
        (kmh > 0.0).then(|| Direction {
            kmh,
            flags: if tolls[i] { TOLL } else { 0 } | if ferry { FERRY } else { 0 },
            restricted: restricted[i],
        })
    };
    let (forward, backward) = (direction(fwd, 0), direction(bwd, 1));
    if forward.is_none() && backward.is_none() {
        return Err(if fwd || bwd {
            Dropped::Speed
        } else {
            Dropped::Access
        });
    }
    Ok(Way {
        forward,
        backward,
        duration_s,
        ferry,
    })
}

/// Tags that close a way to everyone, whatever its access.
fn closed(tags: &[(&str, &str)]) -> bool {
    get(tags, "area") == Some("yes")
        || get(tags, "construction").is_some_and(|v| !OPEN_CONSTRUCTION.contains(&v))
        || get(tags, "proposed").is_some()
        || get(tags, "oneway") == Some("reversible")
        || get(tags, "impassable") == Some("yes")
        || get(tags, "status") == Some("impassable")
        || get(tags, "service") == Some("emergency_access")
}

fn too_small(tags: &[(&str, &str)]) -> bool {
    let height = ["maxheight:physical", "maxheight"]
        .iter()
        .find_map(|k| get(tags, k))
        .and_then(parse_metres);
    let width = ["maxwidth:physical", "maxwidth"]
        .iter()
        .find_map(|k| get(tags, k))
        .and_then(parse_metres);
    height.is_some_and(|h| h < VEHICLE_HEIGHT_M) || width.is_some_and(|w| w <= VEHICLE_WIDTH_M)
}

#[derive(Debug, PartialEq, Eq)]
enum Oneway {
    Forward,
    Backward,
    Both,
}

fn oneway(tags: &[(&str, &str)], highway: Option<&str>) -> Oneway {
    let v = ["oneway:motorcar", "oneway:motor_vehicle", "oneway:vehicle"]
        .iter()
        .find_map(|k| get(tags, k))
        .or_else(|| get(tags, "oneway"));
    match v {
        Some("yes" | "1" | "true") => Oneway::Forward,
        Some("-1" | "reverse") => Oneway::Backward,
        Some("no") => Oneway::Both,
        _ if matches!(highway, Some("motorway" | "motorway_link"))
            || matches!(get(tags, "junction"), Some("roundabout" | "circular")) =>
        {
            Oneway::Forward
        }
        _ => Oneway::Both,
    }
}

/// A speed limit in km/h: a number with an optional unit (`mph`, `knots`, `km/h`), or a zone
/// such as `CA:urban` or `CA:ON:rural`. `None` for values without a speed (`signals`).
pub fn parse_maxspeed(v: &str) -> Option<f64> {
    let v = v.trim().to_ascii_lowercase();
    let digits = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    if digits > 0 {
        let n: f64 = v[..digits].parse().ok()?;
        let unit = v[digits..].trim_start();
        return Some(if unit.starts_with("mph") || unit.starts_with("mp/h") {
            n * 1.609_344
        } else if unit.starts_with("knots") {
            n * 1.852
        } else {
            n
        });
    }
    match v.as_str() {
        "none" => return Some(140.0),
        "walk" => return Some(5.0),
        _ => {}
    }
    // Zones: the most specific entry, then the last part (`rural` of `ca:on:rural`).
    const ZONES: &[(&str, f64)] = &[
        ("ca:qc:rural", 90.0),
        ("urban", 50.0),
        ("rural", 80.0),
        ("trunk", 90.0),
        ("motorway", 100.0),
        ("living_street", 10.0),
    ];
    let v = v.replace('-', ":");
    lookup(ZONES, Some(&v)).or_else(|| lookup(ZONES, v.rsplit(':').next()))
}

/// A `duration` in seconds: minutes (`45`), `H:MM`, `H:MM:SS`, or ISO 8601 (`PT1H30M`,
/// `P1D`, `P2W`), as OSRM reads it.
pub fn parse_duration(v: &str) -> Option<f64> {
    let v = v.trim();
    if let Some(iso) = v.strip_prefix('P') {
        let (mut total, mut num, mut time) = (0.0, String::new(), false);
        for c in iso.chars() {
            match c {
                '0'..='9' | '.' => num.push(c),
                'T' if !time && num.is_empty() => time = true,
                _ => {
                    let n: f64 = num.parse().ok()?;
                    num.clear();
                    total += n * match (c.to_ascii_uppercase(), time) {
                        ('W', false) => 604_800.0,
                        ('D', false) => 86_400.0,
                        ('H', true) => 3600.0,
                        ('M', true) => 60.0,
                        ('S', true) => 1.0,
                        _ => return None,
                    };
                }
            }
        }
        return (num.is_empty() && total > 0.0).then_some(total);
    }
    let parts: Vec<f64> = v
        .split(':')
        .map(|p| p.parse::<u32>().ok().map(f64::from))
        .collect::<Option<_>>()?;
    let s = match parts[..] {
        [m] => m * 60.0,
        [h, m] => h * 3600.0 + m * 60.0,
        [h, m, s] => h * 3600.0 + m * 60.0 + s,
        _ => return None,
    };
    (s > 0.0).then_some(s)
}

/// Metres from `maxheight`-style values: `3.5`, `3.5 m`, `12'6"`.
fn parse_metres(v: &str) -> Option<f64> {
    let v = v.trim().replace(',', ".");
    let digits = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    let n: f64 = v[..digits].parse().ok()?;
    let rest = v[digits..].trim_start();
    if let Some(inches) = rest.strip_prefix('\'') {
        let inches: f64 = inches
            .trim_start()
            .trim_end_matches('"')
            .trim()
            .parse()
            .unwrap_or(0.0);
        return Some((n * 12.0 + inches) * 0.0254);
    }
    if rest.starts_with("ft") {
        return Some(n * 0.3048);
    }
    Some(n)
}

/// Which way along a way a node's rule applies, from `direction` tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applies {
    Both,
    Forward,
    Backward,
}

impl Applies {
    pub fn forward(self) -> bool {
        self != Applies::Backward
    }

    pub fn backward(self) -> bool {
        self != Applies::Forward
    }
}

/// What a node does to the roads through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// No car passes: the road is cut here.
    pub block: bool,
    /// Seconds added on passing it, in the directions of `applies`.
    pub delay_s: u32,
    /// A toll booth: the edges through it get [`TOLL`].
    pub toll: bool,
    pub applies: Applies,
    pub signal: bool,
}

/// The car profile's verdict on a node's tags; `None` when the node changes nothing.
pub fn node(tags: &[(&str, &str)]) -> Option<Node> {
    if tags.is_empty() {
        return None;
    }
    let mut n = Node {
        block: false,
        delay_s: 0,
        toll: false,
        applies: Applies::Both,
        signal: false,
    };
    // An access tag decides alone; the barrier is checked only without one (as OSRM does).
    match ACCESS_KEYS.iter().find_map(|k| get(tags, k)) {
        Some(a) => n.block = DENIED.contains(&a),
        None => {
            if let Some(b) = get(tags, "barrier") {
                match PASSABLE_BARRIERS.iter().find(|(k, _)| *k == b) {
                    Some(&(_, delay)) => {
                        n.delay_s = delay;
                        n.toll = b == "toll_booth";
                        n.block = b == "height_restrictor"
                            && get(tags, "maxheight")
                                .and_then(parse_metres)
                                .is_some_and(|h| h < VEHICLE_HEIGHT_M);
                    }
                    None => {
                        let rising = get(tags, "bollard") == Some("rising");
                        let flat_kerb = b == "kerb"
                            && (matches!(get(tags, "kerb"), Some("lowered" | "flush"))
                                || get(tags, "highway") == Some("crossing"));
                        n.block = !rising && !flat_kerb;
                    }
                }
            }
        }
    }
    if get(tags, "highway") == Some("traffic_signals") {
        n.signal = true;
        n.delay_s += SIGNAL_S;
        n.applies = match get(tags, "traffic_signals:direction").or(get(tags, "direction")) {
            Some("forward") => Applies::Forward,
            Some("backward") => Applies::Backward,
            _ => Applies::Both,
        };
    }
    (n.block || n.delay_s > 0 || n.toll || n.signal).then_some(n)
}

/// A turn restriction's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restriction {
    /// `no_*`: the path from the `from` way through the `via` members onto the `to` way is
    /// forbidden.
    No,
    /// `only_*`: from the `from` way through the `via` members, only the `to` way is allowed.
    Only,
}

/// Whether a `type=restriction` relation applies to cars, and of which kind. `None` for
/// other vehicles (`restriction:hgv` alone), cars in `except`, conditional-only
/// restrictions, values that are neither `no_*` nor `only_*`, and `no_right_turn_on_red`
/// and the like, which forbid a turn only on a red light (step 3.5: 138 in Ohio and 8 in
/// Quebec were applied as turn bans until then; OSRM skips them).
pub fn restriction(tags: &[(&str, &str)]) -> Option<Restriction> {
    let except = get(tags, "except").unwrap_or_default();
    if except
        .split(';')
        .any(|e| matches!(e.trim(), "motorcar" | "motor_vehicle" | "vehicle"))
    {
        return None;
    }
    let v = [
        "restriction:motorcar",
        "restriction:motor_vehicle",
        "restriction:vehicle",
        "restriction",
    ]
    .iter()
    .find_map(|k| get(tags, k))?;
    if v.contains("_on_red") {
        None
    } else if v.starts_with("no_") {
        Some(Restriction::No)
    } else if v.starts_with("only_") {
        Some(Restriction::Only)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(tags: &[(&str, &str)]) -> Result<Way, Dropped> {
        way(tags)
    }

    fn kmh(d: Option<Direction>) -> f64 {
        d.map_or(0.0, |d| (d.kmh * 100.0).round() / 100.0)
    }

    #[test]
    fn highways_are_whitelist_road_classes() {
        let wl = crate::whitelist::Whitelist::parse(crate::whitelist::DEFAULT).unwrap();
        let roads = &wl.layers[wl.layer_index("roads").unwrap()];
        for (h, _) in HIGHWAYS {
            assert!(
                roads
                    .classes
                    .iter()
                    .any(|c| c.name == *h || c.name.starts_with(&format!("{h}_"))),
                "{h}"
            );
        }
    }

    #[test]
    fn classes_and_default_speeds() {
        let r = w(&[("highway", "residential")]).unwrap();
        assert_eq!((kmh(r.forward), kmh(r.backward)), (25.0, 25.0));
        assert_eq!(w(&[("highway", "footway")]), Err(Dropped::NotRoad));
        assert_eq!(w(&[("highway", "track")]), Err(Dropped::NotRoad));
        assert_eq!(w(&[("highway", "construction")]), Err(Dropped::NotRoad));
        assert_eq!(w(&[("building", "yes")]), Err(Dropped::NotRoad));
    }

    #[test]
    fn access_most_specific_first() {
        let deny = [("highway", "residential"), ("access", "no")];
        assert_eq!(w(&deny), Err(Dropped::Access));
        let private = [("highway", "service"), ("access", "private")];
        assert_eq!(w(&private), Err(Dropped::Access));
        let cars = [
            ("highway", "residential"),
            ("access", "no"),
            ("motor_vehicle", "yes"),
        ];
        assert!(w(&cars).is_ok());
        let no_cars = [
            ("highway", "residential"),
            ("vehicle", "yes"),
            ("motorcar", "no"),
        ];
        assert_eq!(w(&no_cars), Err(Dropped::Access));
        // One direction denied.
        let fwd = [("highway", "residential"), ("motor_vehicle:backward", "no")];
        let r = w(&fwd).unwrap();
        assert!(r.forward.is_some() && r.backward.is_none());
    }

    #[test]
    fn destination_is_restricted() {
        for v in ["destination", "delivery", "customers"] {
            let r = w(&[("highway", "residential"), ("access", v)]).unwrap();
            assert!(r.forward.unwrap().restricted && r.backward.unwrap().restricted);
        }
        let hov = w(&[("highway", "motorway"), ("hov", "designated")]).unwrap();
        assert!(hov.forward.unwrap().restricted);
        let lanes = w(&[
            ("highway", "primary"),
            ("hov:lanes:forward", "designated|designated"),
            ("hov:lanes:backward", "designated|yes"),
        ])
        .unwrap();
        assert!(lanes.forward.unwrap().restricted && !lanes.backward.unwrap().restricted);
        assert!(
            !w(&[("highway", "primary")])
                .unwrap()
                .forward
                .unwrap()
                .restricted
        );
    }

    #[test]
    fn closed_and_small() {
        for t in [
            ("area", "yes"),
            ("construction", "yes"),
            ("proposed", "primary"),
            ("oneway", "reversible"),
            ("impassable", "yes"),
            ("status", "impassable"),
            ("service", "emergency_access"),
        ] {
            assert_eq!(
                w(&[("highway", "service"), t]),
                Err(Dropped::Closed),
                "{t:?}"
            );
        }
        assert!(w(&[("highway", "primary"), ("construction", "minor")]).is_ok());
        assert_eq!(
            w(&[("highway", "service"), ("maxheight", "1.9")]),
            Err(Dropped::Size)
        );
        assert_eq!(
            w(&[("highway", "service"), ("maxheight", "6'3\"")]),
            Err(Dropped::Size)
        );
        assert!(w(&[("highway", "service"), ("maxheight", "13'6\"")]).is_ok());
        assert_eq!(
            w(&[("highway", "service"), ("maxwidth", "1.8")]),
            Err(Dropped::Size)
        );
    }

    #[test]
    fn oneway_rules() {
        let one = |tags: &[(&str, &str)]| {
            let r = w(tags).unwrap();
            (r.forward.is_some(), r.backward.is_some())
        };
        assert_eq!(
            one(&[("highway", "primary"), ("oneway", "yes")]),
            (true, false)
        );
        assert_eq!(
            one(&[("highway", "primary"), ("oneway", "-1")]),
            (false, true)
        );
        assert_eq!(one(&[("highway", "motorway")]), (true, false));
        assert_eq!(one(&[("highway", "motorway_link")]), (true, false));
        assert_eq!(
            one(&[("highway", "motorway"), ("oneway", "no")]),
            (true, true)
        );
        assert_eq!(
            one(&[("highway", "primary"), ("junction", "roundabout")]),
            (true, false)
        );
        assert_eq!(
            one(&[("highway", "primary"), ("junction", "circular")]),
            (true, false)
        );
        assert_eq!(
            one(&[
                ("highway", "primary"),
                ("oneway", "yes"),
                ("oneway:motorcar", "no")
            ]),
            (true, true)
        );
    }

    #[test]
    fn speeds() {
        let s = |tags: &[(&str, &str)]| kmh(w(tags).unwrap().forward);
        assert_eq!(s(&[("highway", "primary"), ("maxspeed", "100")]), 80.0);
        assert_eq!(s(&[("highway", "primary"), ("maxspeed", "55 mph")]), 70.81);
        assert_eq!(
            s(&[("highway", "primary"), ("maxspeed", "CA:ON:rural")]),
            64.0
        );
        assert_eq!(
            s(&[
                ("highway", "primary"),
                ("maxspeed:advisory", "50"),
                ("maxspeed", "100")
            ]),
            40.0
        );
        // Surfaces cap the speed.
        assert_eq!(
            s(&[
                ("highway", "primary"),
                ("maxspeed", "100"),
                ("surface", "gravel")
            ]),
            40.0
        );
        assert_eq!(
            w(&[("highway", "primary"), ("smoothness", "impassable")]),
            Err(Dropped::Speed)
        );
        // Slow service roads.
        assert_eq!(s(&[("highway", "service"), ("service", "driveway")]), 7.5);
        assert_eq!(s(&[("highway", "service"), ("service", "spur")]), 15.0);
        // Per direction.
        let r = w(&[
            ("highway", "primary"),
            ("maxspeed:forward", "50"),
            ("maxspeed:backward", "100"),
        ])
        .unwrap();
        assert_eq!((kmh(r.forward), kmh(r.backward)), (40.0, 80.0));
    }

    #[test]
    fn maxspeed_values() {
        assert_eq!(parse_maxspeed("50"), Some(50.0));
        assert_eq!(parse_maxspeed(" 30 mph"), Some(30.0 * 1.609_344));
        assert_eq!(parse_maxspeed("40mph"), Some(40.0 * 1.609_344));
        assert_eq!(parse_maxspeed("60 km/h"), Some(60.0));
        assert_eq!(parse_maxspeed("50;30"), Some(50.0));
        assert_eq!(parse_maxspeed("CA:urban"), Some(50.0));
        assert_eq!(parse_maxspeed("CA:QC:rural"), Some(90.0));
        assert_eq!(parse_maxspeed("ca-qc:rural"), Some(90.0));
        assert_eq!(parse_maxspeed("US:motorway"), Some(100.0));
        assert_eq!(parse_maxspeed("none"), Some(140.0));
        assert_eq!(parse_maxspeed("signals"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("45"), Some(2700.0));
        assert_eq!(parse_duration("1:30"), Some(5400.0));
        assert_eq!(parse_duration("01:30:15"), Some(5415.0));
        assert_eq!(parse_duration("PT1H30M"), Some(5400.0));
        assert_eq!(parse_duration("PT20M"), Some(1200.0));
        assert_eq!(parse_duration("P1DT1H"), Some(90_000.0));
        assert_eq!(parse_duration("P1W"), Some(604_800.0));
        assert_eq!(parse_duration("PT"), None);
        assert_eq!(parse_duration("about 20 min"), None);
        assert_eq!(parse_duration("1:2:3:4"), None);
    }

    #[test]
    fn ferries_and_tolls() {
        let f = w(&[("route", "ferry"), ("duration", "PT20M"), ("toll", "yes")]).unwrap();
        assert!(f.ferry);
        assert_eq!(f.duration_s, Some(1200.0));
        assert_eq!(f.forward.unwrap().flags, TOLL | FERRY);
        assert_eq!(kmh(f.forward), FERRY_KMH);
        assert_eq!(
            w(&[("route", "ferry"), ("motor_vehicle", "no")]),
            Err(Dropped::Access)
        );
        let t = w(&[("highway", "motorway"), ("toll", "yes")]).unwrap();
        assert_eq!(t.forward.unwrap().flags, TOLL);
        let one = w(&[("highway", "primary"), ("toll:forward", "yes")]).unwrap();
        assert_eq!(
            (one.forward.unwrap().flags, one.backward.unwrap().flags),
            (TOLL, 0)
        );
    }

    #[test]
    fn nodes() {
        assert_eq!(node(&[]), None);
        assert_eq!(node(&[("name", "x")]), None);
        let gate = node(&[("barrier", "gate")]).unwrap();
        assert!(!gate.block && gate.delay_s == 10);
        let booth = node(&[("barrier", "toll_booth")]).unwrap();
        assert!(booth.toll && !booth.block);
        assert!(node(&[("barrier", "bollard")]).unwrap().block);
        assert!(node(&[("barrier", "jersey_barrier")]).unwrap().block);
        assert_eq!(node(&[("barrier", "bollard"), ("bollard", "rising")]), None);
        assert_eq!(node(&[("barrier", "kerb"), ("kerb", "lowered")]), None);
        // An access tag decides alone.
        assert_eq!(
            node(&[("barrier", "bollard"), ("motor_vehicle", "yes")]),
            None
        );
        assert!(
            node(&[("barrier", "gate"), ("access", "private")])
                .unwrap()
                .block
        );
        assert!(
            node(&[("barrier", "height_restrictor"), ("maxheight", "1.8")])
                .unwrap()
                .block
        );
        let sig = node(&[("highway", "traffic_signals")]).unwrap();
        assert!(sig.signal && sig.delay_s == SIGNAL_S && sig.applies == Applies::Both);
        let dir = node(&[
            ("highway", "traffic_signals"),
            ("traffic_signals:direction", "backward"),
        ])
        .unwrap();
        assert!(!dir.applies.forward() && dir.applies.backward());
    }

    #[test]
    fn restrictions() {
        use Restriction::*;
        assert_eq!(restriction(&[("restriction", "no_left_turn")]), Some(No));
        assert_eq!(
            restriction(&[("restriction", "only_straight_on")]),
            Some(Only)
        );
        assert_eq!(restriction(&[("restriction:hgv", "no_left_turn")]), None);
        assert_eq!(
            restriction(&[
                ("restriction", "no_left_turn"),
                ("restriction:motorcar", "only_right_turn")
            ]),
            Some(Only)
        );
        assert_eq!(
            restriction(&[("restriction", "no_u_turn"), ("except", "bicycle;motorcar")]),
            None
        );
        assert_eq!(restriction(&[("restriction", "give_way")]), None);
        assert_eq!(
            restriction(&[("restriction", "no_right_turn_on_red")]),
            None
        );
        assert_eq!(
            restriction(&[("restriction:conditional", "no_left_turn @ (Mo-Fr)")]),
            None
        );
    }
}

//! `route --depart` and `--arrive-by` (phase 4 step 4.5): the trip's schedule as the app
//! shows it, so criterion 4's trips can be checked on a pack without a window. Each stop's
//! arrival and departure in its zone, each night and break with the town it is near, and
//! each zone crossing on the way, in order.

use anyhow::{Context, Result, bail};
use jiff::civil;
use jiff::tz::TimeZone;
use omp_core::geo::P;
use omp_core::pack::Pack;
use omp_core::route::{Step, Trip};
use omp_core::schedule::{self, Breaks, Day, Kind, Local, Settings, Stay, When};
use omp_core::schema::{self, Schema};
use omp_core::search::Index;
use omp_core::tz::Zones;

use super::{RouteArgs, duration};

fn parse_time(s: &str) -> Result<civil::Time> {
    schedule::parse_time(s).with_context(|| format!("not a time: {s:?}"))
}

/// The settings from the arguments, if `--depart` or `--arrive-by` is given: the time is
/// local to the first stop, or for arrive-by to the last.
pub fn settings(args: &RouteArgs, stops: &[P], local: &Local) -> Result<Option<Settings>> {
    let (text, stop) = match (&args.depart, &args.arrive_by) {
        (Some(d), None) => (d, 0),
        (None, Some(a)) => (a, stops.len() - 1),
        (None, None) => return Ok(None),
        (Some(_), Some(_)) => bail!("give --depart or --arrive-by, not both"),
    };
    let dt: civil::DateTime = text
        .parse()
        .with_context(|| format!("{text:?} is not a date and time like 2026-07-15T08:00"))?;
    let t = local.tz(stops[stop]).to_timestamp(dt)?;
    let when = match args.depart.is_some() {
        true => When::Depart(t),
        false => When::ArriveBy(t),
    };
    let day = match args.window.as_deref() {
        Some("off") => None,
        Some(w) => {
            let (a, b) = w
                .split_once('-')
                .context("--window is START-END, e.g. 8:00-18:00, or off")?;
            Some(Day {
                start: parse_time(a)?,
                end: Some(parse_time(b)?),
                max_drive_s: args.max_drive.map(|h| h * 3600.0),
            })
        }
        None => args.max_drive.map(|h| Day {
            end: None,
            max_drive_s: Some(h * 3600.0),
            ..Day::default()
        }),
    };
    let breaks = args.break_every.map(|h| Breaks {
        every_s: h * 3600.0,
        for_s: args.break_for * 60.0,
    });
    let stays = (0..stops.len())
        .map(|_| Stay {
            s: args.stay * 60.0,
            overnight: false,
        })
        .collect();
    Ok(Some(Settings {
        when,
        stays,
        day,
        breaks,
    }))
}

/// "Sat 2026-07-15 08:00 PDT".
fn show(t: &jiff::Zoned) -> String {
    t.strftime("%a %Y-%m-%d %H:%M %Z").to_string()
}

/// " (1 h behind the departure)", or nothing in the departure's zone.
fn against(t: &jiff::Zoned, dep: &TimeZone) -> String {
    match schedule::difference(t.timestamp(), t.time_zone(), dep) {
        0 => String::new(),
        d => {
            let h = d.abs() as f64 / 3600.0;
            let side = if d < 0 { "behind" } else { "ahead of" };
            format!(" ({} h {side} the departure)", trim(h))
        }
    }
}

fn trim(h: f64) -> String {
    let s = format!("{h:.1}");
    s.strip_suffix(".0").map_or(s.clone(), str::to_owned)
}

/// "Enter Central Time: clocks go back 1 h".
pub fn crossing_text(abbr: &str, change_s: i32) -> String {
    let zone =
        schedule::zone_name(abbr).map_or_else(|| abbr.to_owned(), |n| format!("{n} ({abbr})"));
    let h = trim(f64::from(change_s.abs()) / 3600.0);
    let clocks = match change_s {
        0 => "clocks stay the same".to_string(),
        c if c > 0 => format!("clocks go forward {h} h"),
        _ => format!("clocks go back {h} h"),
    };
    format!("Enter {zone}: {clocks}")
}

/// The pack's zones and the `places` classes, for `local` and the towns.
pub fn open(pack: &Pack) -> Result<(Option<Zones>, Vec<String>)> {
    let zones = Zones::open(pack)?;
    if zones.is_none() {
        eprintln!(
            "  the pack has no TZON (format 0.{}): every time is UTC",
            pack.minor
        );
    }
    let places = match pack.section(schema::ID) {
        Some(s) => Schema::decode(&pack.read_section(s)?)?
            .layer("places")
            .map(|(_, l)| l.classes.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    Ok((zones, places))
}

/// Prints the schedule of `trip` through `stops` (named `names`).
#[allow(clippy::too_many_arguments)]
pub fn print(
    index: Option<&Index>,
    places: &[String],
    local: &Local,
    trip: &Trip,
    stops: &[P],
    names: &[String],
    steps: &[Step],
    st: &Settings,
) -> Result<()> {
    let t = std::time::Instant::now();
    let s = schedule::schedule(trip, stops, st, local).context("a leg has no route")?;
    let dep = s.departure().context("no departure")?.time_zone().clone();
    let near = |p: P| -> Result<String> {
        let Some(index) = index else {
            return Ok(String::new());
        };
        Ok(match index.town_near(p, places)? {
            Some((e, d)) if d < 1000.0 => format!(" in {}", e.name),
            Some((e, d)) => format!(" near {} ({:.0} km)", e.name, d / 1000.0),
            None => String::new(),
        })
    };
    let crossings = schedule::crossings(trip, steps, &s, local);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    // Rows in driving order: stops at their legs' ends, events and crossings at their
    // driving seconds; at the same second, an arrival, then a night, then the departure.
    let mut rows: Vec<(f64, u8, String)> = Vec::new();
    let mut base = 0.0;
    for (i, name) in names.iter().enumerate() {
        if let Some(a) = &s.arrive[i] {
            rows.push((
                base,
                0,
                format!(
                    "stop {}  {name}: arrive {}{}",
                    i + 1,
                    show(a),
                    against(a, &dep)
                ),
            ));
        }
        if let Some(d) = &s.depart[i] {
            rows.push((
                base,
                2,
                format!(
                    "stop {}  {name}: depart {}{}",
                    i + 1,
                    show(d),
                    against(d, &dep)
                ),
            ));
        }
        base += trip
            .legs
            .get(i)
            .and_then(Option::as_ref)
            .map_or(0.0, |l| l.time_s);
    }
    for e in &s.events {
        let text = match e.kind {
            Kind::Night => format!(
                "  day {} ends{}: {} to {}{}",
                e.day - 1,
                near(e.point)?,
                show(&e.from),
                show(&e.until),
                against(&e.until, &dep)
            ),
            Kind::Break => format!(
                "  break{}: {} for {}",
                near(e.point)?,
                show(&e.from),
                duration((e.until.timestamp().as_second() - e.from.timestamp().as_second()) as f64)
            ),
        };
        rows.push((e.at_s, 1, text));
    }
    for c in &crossings {
        let t = s.clock(c.at_s, false).to_zoned(c.tz.clone());
        let text = crossing_text(&c.abbr, c.change_s);
        let zone = c.tz.iana_name().unwrap_or_default();
        let near = near(c.point)?;
        let p = super::latlon(c.point);
        rows.push((
            c.at_s,
            1,
            format!("  {text} at {}{near} ({p}), {zone}", show(&t)),
        ));
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let first = s.departure().context("no departure")?;
    let last = s.arrival().context("no arrival")?;
    println!(
        "schedule: {} days, {} from leaving to arriving{}; {ms:.1} ms with the crossings",
        s.days(),
        duration((last.timestamp().as_second() - first.timestamp().as_second()) as f64),
        if s.waited {
            "; leaves at the start of the driving window"
        } else {
            ""
        }
    );
    for (_, _, text) in rows {
        println!("{text}");
    }
    Ok(())
}

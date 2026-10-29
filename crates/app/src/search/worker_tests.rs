//! The worker's tests, on a stand-in for the pack's search: a query answered level by
//! level, a stale answer dropped, a level stopped by a new keystroke, and "what's here"
//! served between levels.

use super::*;
use crate::search::session::Session;
use crate::search::testing::hit;
use omp_core::find::HitKind;

const WAIT: Duration = Duration::from_secs(5);
const LEVELS: usize = find::Session::LEVELS;

/// A stand-in for the pack's search: level `d` of "text" finds one hit, "text d". The
/// level in `slow` runs until it is stopped, the first time it runs.
#[derive(Default)]
struct Fake {
    slow: Option<(&'static str, usize)>,
    slowed: AtomicBool,
    /// The levels run, as "text d".
    log: Arc<Mutex<Vec<String>>>,
}

struct FakeLevels<'a> {
    fake: &'a Fake,
    text: String,
    stop: Option<Box<dyn Fn() -> bool>>,
    hits: Vec<Hit>,
}

impl Engine for Fake {
    type Levels<'a> = FakeLevels<'a>;
    fn start(&self, req: &Request) -> FakeLevels<'_> {
        FakeLevels {
            fake: self,
            text: req.text.to_string(),
            stop: None,
            hits: Vec::new(),
        }
    }
    fn whats_here(&self, p: P, _: f64, _: f64) -> io::Result<Shown> {
        Ok(Shown {
            p,
            address: None,
            address_place: String::new(),
            street: None,
            nearby: Vec::new(),
            areas: vec!["Ohio".into()],
            chain: Vec::new(),
        })
    }
    fn heap_bytes(&self) -> usize {
        0
    }
    fn bytes_read(&self) -> u64 {
        0
    }
}

impl Levels for FakeLevels<'_> {
    fn stop_when(&mut self, stop: impl Fn() -> bool + 'static) {
        self.stop = Some(Box::new(stop));
    }
    fn set_limit(&mut self, _: usize) {}
    fn hits(&mut self) -> io::Result<Vec<Hit>> {
        Ok(self.hits.clone())
    }
    fn level(&mut self, d: usize) -> io::Result<Option<Vec<Hit>>> {
        let name = format!("{} {d}", self.text);
        self.fake.log.lock().unwrap().push(name.clone());
        if self.fake.slow == Some((self.text.as_str(), d))
            && !self.fake.slowed.swap(true, Ordering::Relaxed)
        {
            let stop = self.stop.as_ref().unwrap();
            let t = Instant::now();
            while !stop() {
                assert!(t.elapsed() < WAIT, "{name} never stopped");
                std::thread::sleep(Duration::from_millis(1));
            }
            return Ok(None);
        }
        self.hits = vec![Hit {
            name,
            ..hit(HitKind::Address, 40.0, -83.0, 10.0)
        }];
        Ok(Some(self.hits.clone()))
    }
    fn heap_bytes(&self) -> usize {
        0
    }
}

/// A worker thread serving `fake`, and its log.
fn serving(fake: Fake) -> (Worker, Arc<Mutex<Vec<String>>>) {
    let log = fake.log.clone();
    let w = Worker::spawn(Status::Ready, move |s, tx, here_tx| {
        serve(&s, &fake, &tx, &here_tx, &|| {});
    });
    (w, log)
}

fn next(w: &Worker) -> Answer {
    w.rx.recv_timeout(WAIT).expect("an answer")
}

fn names(a: &Answer) -> Vec<String> {
    a.hits
        .as_ref()
        .unwrap()
        .iter()
        .map(|h| h.name.clone())
        .collect()
}

/// Waits for the worker to have run `level`.
fn until_run(log: &Mutex<Vec<String>>, level: &str) {
    let t = Instant::now();
    while !log.lock().unwrap().iter().any(|l| l == level) {
        assert!(t.elapsed() < WAIT, "{level} never ran");
        std::thread::sleep(Duration::from_millis(1));
    }
}

const NEAR: camera::Vec2 = [0.3, 0.4];

#[test]
fn a_query_is_answered_level_by_level() {
    let (w, log) = serving(Fake::default());
    let mut s = Session::default();
    w.ask(s.ask("Col", false, NEAR, 8.0));
    let a = next(&w);
    let t = Instant::now();
    assert_eq!(
        (a.level, a.more, names(&a)),
        (0, true, vec!["Col 0".into()])
    );
    // After the pause, the deeper levels one by one.
    for d in 1..LEVELS {
        let a = next(&w);
        assert_eq!((a.level, a.more), (d, d + 1 < LEVELS));
        assert_eq!(names(&a), [format!("Col {d}")]);
    }
    assert!(t.elapsed() >= PAUSE / 2);
    // Enter on the same text goes on with the session: its levels are done.
    w.ask(s.ask("Col", true, NEAR, 8.0));
    let a = next(&w);
    assert!(a.query.submit && a.level == LEVELS - 1 && !a.more);
    assert_eq!(names(&a), [format!("Col {}", LEVELS - 1)]);
    let run: Vec<String> = (0..LEVELS).map(|d| format!("Col {d}")).collect();
    assert_eq!(*log.lock().unwrap(), run);
}

#[test]
fn stale_answers_are_dropped() {
    let (w, _) = serving(Fake::default());
    let mut s = Session::default();
    w.ask(s.ask("Col", false, NEAR, 8.0));
    let mut answers = vec![next(&w)];
    w.ask(s.ask("Colu", false, NEAR, 8.0));
    loop {
        let a = next(&w);
        let fresh = a.query.text == "Colu";
        answers.push(a);
        if fresh {
            break;
        }
    }
    // Whatever "Col" got answered before "Colu" is stale.
    for a in answers {
        let fresh = a.query.text == "Colu";
        assert_eq!(s.take(a, None).is_some(), fresh);
    }
    assert!(s.dropped >= 1 && s.hits_for == "Colu");
    assert_eq!(s.hits[0].name, "Colu 0");
}

#[test]
fn a_new_keystroke_stops_a_level() {
    let (w, log) = serving(Fake {
        slow: Some(("Col", 1)),
        ..Fake::default()
    });
    let mut s = Session::default();
    w.ask(s.ask("Col", false, NEAR, 8.0));
    assert_eq!(next(&w).level, 0);
    until_run(&log, "Col 1");
    w.ask(s.ask("Colu", false, NEAR, 8.0));
    // The level stopped sends nothing; the new keystroke is answered.
    let a = next(&w);
    assert_eq!((a.query.text.as_str(), a.level), ("Colu", 0));
    assert_eq!(log.lock().unwrap()[..3], ["Col 0", "Col 1", "Colu 0"]);
}

#[test]
fn whats_here_is_served_between_levels() {
    let (w, log) = serving(Fake {
        slow: Some(("Col", 1)),
        ..Fake::default()
    });
    let mut s = Session::default();
    w.ask(s.ask("Col", false, NEAR, 8.0));
    assert_eq!(next(&w).level, 0);
    until_run(&log, "Col 1");
    w.ask_here(HereQuery {
        id: 7,
        p: [1 << 30, 1 << 30],
        entries_m: 500.0,
        asked: Instant::now(),
        stop: false,
    });
    let h = w.here_rx.recv_timeout(WAIT).expect("a here answer");
    assert_eq!(h.query.id, 7);
    assert_eq!(h.shown.unwrap().areas, ["Ohio"]);
    // The level it stopped runs again, then the rest.
    for d in 1..LEVELS {
        assert_eq!(next(&w).level, d);
    }
    let mut run: Vec<String> = (0..LEVELS).map(|d| format!("Col {d}")).collect();
    run.insert(1, "Col 1".into());
    assert_eq!(*log.lock().unwrap(), run);
}

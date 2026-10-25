//! "Verify pack" (phase1-plan.md 1.9): [`Pack::verify_until`] on a background thread, with
//! its progress for a progress bar, and a way to cancel it.
//!
//! [`Pack::verify_until`]: omp_core::pack::Pack::verify_until

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omp_core::pack::Pack;

/// How a check ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every section's checksum matches.
    Intact {
        sections: usize,
        bytes: u64,
    },
    /// These sections' checksums do not match.
    Damaged(Vec<String>),
    /// A read failed.
    Failed(String),
    Cancelled,
}

#[derive(Default)]
struct Shared {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
    outcome: Mutex<Option<Outcome>>,
}

pub struct Verify {
    shared: Arc<Shared>,
    started: Instant,
    /// Set when it ended.
    took: Option<Duration>,
}

impl Verify {
    /// Starts checking the pack `owner` holds (a `TileReader`'s, through `pack`); `wake` is
    /// called on progress and at the end.
    pub fn start<T: Send + Sync + 'static>(
        owner: Arc<T>,
        pack: fn(&T) -> &Pack,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("verify".into())
            .spawn(move || {
                let pack = pack(&owner);
                let mut last = Instant::now();
                let res = pack.verify_until(|done, total| {
                    s.done.store(done, Ordering::Relaxed);
                    s.total.store(total, Ordering::Relaxed);
                    if last.elapsed() > Duration::from_millis(100) {
                        last = Instant::now();
                        wake();
                    }
                    !s.cancel.load(Ordering::Relaxed)
                });
                let outcome = match res {
                    Ok(bad) if bad.is_empty() => Outcome::Intact {
                        sections: pack.sections.len(),
                        bytes: s.total.load(Ordering::Relaxed),
                    },
                    Ok(bad) => Outcome::Damaged(
                        bad.iter()
                            .map(|id| String::from_utf8_lossy(id).into_owned())
                            .collect(),
                    ),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Outcome::Cancelled,
                    Err(e) => Outcome::Failed(e.to_string()),
                };
                *s.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
                wake();
            });
        if let Err(e) = thread {
            *shared.outcome.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Outcome::Failed(format!("could not start a thread: {e}")));
        }
        Self {
            shared,
            started: Instant::now(),
            took: None,
        }
    }

    /// Bytes checked and the total; the total is 0 until the first progress report.
    pub fn progress(&self) -> (u64, u64) {
        (
            self.shared.done.load(Ordering::Relaxed),
            self.shared.total.load(Ordering::Relaxed),
        )
    }

    pub fn elapsed(&self) -> Duration {
        self.took.unwrap_or_else(|| self.started.elapsed())
    }

    /// How it ended, once it has.
    pub fn outcome(&mut self) -> Option<Outcome> {
        let o = self
            .shared
            .outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if o.is_some() && self.took.is_none() {
            self.took = Some(self.started.elapsed());
        }
        o
    }

    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Verify {
    /// Stops the check, without waiting: it ends within one 8 MiB read.
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omp_core::pack::{Info, Writer};

    fn pack(dir: &std::path::Path) -> std::path::PathBuf {
        let mut w = Writer::create(dir, "t", 1 << 20).unwrap();
        w.section(*b"META", &[1; 100]).unwrap();
        w.section(*b"TIDX", &[2; 5000]).unwrap();
        let info = Info {
            pack_id: [7; 16],
            bbox: [0, 0, 1, 1],
            build_time: 0,
            name: "T".into(),
            snapshot: String::new(),
        };
        w.finish(&info).unwrap().remove(0)
    }

    fn wait(v: &mut Verify) -> Outcome {
        let t = Instant::now();
        loop {
            if let Some(o) = v.outcome() {
                return o;
            }
            assert!(t.elapsed() < Duration::from_secs(10));
            std::thread::yield_now();
        }
    }

    #[test]
    fn reports_intact_and_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let first = pack(dir.path());
        let open = || Arc::new(Pack::open(&first).unwrap());
        let mut v = Verify::start(open(), |p| p, || {});
        assert_eq!(
            wait(&mut v),
            Outcome::Intact {
                sections: 2,
                bytes: 5100
            }
        );
        assert_eq!(v.progress(), (5100, 5100));

        // A flipped byte in META, which starts on the second page.
        let mut b = std::fs::read(&first).unwrap();
        b[4096 + 10] ^= 0xff;
        std::fs::write(&first, b).unwrap();
        let mut v = Verify::start(open(), |p| p, || {});
        assert_eq!(wait(&mut v), Outcome::Damaged(vec!["META".into()]));
    }
}

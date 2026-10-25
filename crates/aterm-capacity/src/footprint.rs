// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A RUN'S DISK FOOTPRINT (`docs/DESIGN-adaptive-build-2026-10-24.md`, DISK-ADMIT): what one
//! kind of run writes, measured on its own directories while it runs ([`Meter`]) and kept beside
//! them ([`Footprints`]), so the next run of that kind claims what the last ones wrote
//! ([`Footprints::need`]). A kind never measured here claims its cold budget, less what its
//! directories already hold.

use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use aterm_json::{Map, Value};

use crate::admit::{Claim, gib};

/// The runs [`Footprints`] keeps, newest last: a light run never hides the heavy one before it.
pub const KEPT: usize = 8;

/// How often a [`Meter`] measures: the pace of a wait line, so a waiter reads a holder's writes
/// as fresh as it says them.
pub const EVERY: Duration = Duration::from_secs(60);

/// The bytes `paths` hold on disk: each file's allocated blocks once, however many names it has
/// among them, never following a link.
#[must_use]
pub fn allocated(paths: &[PathBuf]) -> u64 {
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut total = 0u64;
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            stack.extend(
                std::fs::read_dir(&path)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path()),
            );
            continue;
        }
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::fs::MetadataExt as _;
            if meta.nlink() > 1 && !seen.insert((meta.dev(), meta.ino())) {
                continue;
            }
            meta.blocks().saturating_mul(512)
        };
        #[cfg(not(unix))]
        let bytes = {
            let _ = &mut seen;
            meta.len()
        };
        total = total.saturating_add(bytes);
    }
    total
}

/// One measured run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Footprint {
    /// The most its directories held while it ran, bytes.
    pub peak: u64,
    /// The most they held above what they held when it started.
    pub grew: u64,
    /// When it ended, unix ms.
    pub at_ms: u64,
}

/// What a run claims, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    /// Bytes.
    pub bytes: u64,
    /// The runs it was read from; `0`: the cold budget less what the directories hold.
    pub runs: usize,
    /// What the run's directories hold now.
    pub holds: u64,
    /// What a run of its kind writes into empty directories.
    pub cold: u64,
}

impl fmt::Display for Estimate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.runs == 0 && self.holds == 0 {
            write!(
                f,
                "{}, the cold budget: no run of it measured here",
                size(self.bytes)
            )
        } else if self.runs == 0 {
            write!(
                f,
                "{}, the cold budget of {} less the {} its directories hold: no run of it measured \
                 here",
                size(self.bytes),
                size(self.cold),
                size(self.holds)
            )
        } else {
            write!(
                f,
                "{}, the most the last {} run(s) of it wrote, with what its directories hold now ({})",
                size(self.bytes),
                self.runs,
                size(self.holds)
            )
        }
    }
}

/// A footprint as a person reads it: under a GiB in MiB (`24 MiB`), which [`gib`] would round
/// to nothing, else as [`gib`].
#[must_use]
pub fn size(bytes: u64) -> String {
    if bytes < crate::floor::GIB {
        format!("{} MiB", bytes.div_ceil(1 << 20))
    } else {
        gib(bytes)
    }
}

/// The runs of one kind measured here, newest last, at most [`KEPT`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Footprints {
    runs: Vec<Footprint>,
}

impl Footprints {
    /// The runs measured.
    #[must_use]
    pub fn runs(&self) -> &[Footprint] {
        &self.runs
    }

    /// Keep `run`, the oldest let go past [`KEPT`].
    pub fn push(&mut self, run: Footprint) {
        self.runs.push(run);
        let over = self.runs.len().saturating_sub(KEPT);
        self.runs.drain(..over);
    }

    /// What the next run claims when its directories hold `holds` bytes: for each run kept, what
    /// it grew them by, or what they held at its peak less what they hold now (a build directory
    /// reclaimed since is written again whole), and the most of those. Where none is kept,
    /// `cold`, what a run writes into EMPTY directories, less what they hold already: nothing
    /// where they hold it all (a warm build directory, a VM already on disk), and that first run
    /// then measures what it really writes for the next.
    #[must_use]
    pub fn need(&self, holds: u64, cold: u64) -> Estimate {
        let bytes = self
            .runs
            .iter()
            .map(|r| r.grew.max(r.peak.saturating_sub(holds)))
            .max()
            .unwrap_or_else(|| cold.saturating_sub(holds));
        Estimate {
            bytes,
            runs: self.runs.len(),
            holds,
            cold,
        }
    }

    /// The runs as the file holds them.
    #[must_use]
    pub fn encode(&self) -> String {
        let runs = self
            .runs
            .iter()
            .map(|r| {
                let mut o = Map::new();
                o.insert("peak".to_owned(), Value::from(r.peak));
                o.insert("grew".to_owned(), Value::from(r.grew));
                o.insert("at_ms".to_owned(), Value::from(r.at_ms));
                Value::Object(o)
            })
            .collect();
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("runs".to_owned(), Value::Array(runs));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The runs `text` holds; none for text that is not [`Self::encode`]'s.
    #[must_use]
    pub fn decode(text: &str) -> Footprints {
        let Ok(v) = aterm_json::from_str::<Value>(text) else {
            return Footprints::default();
        };
        if v.get("schema").and_then(Value::as_u64) != Some(1) {
            return Footprints::default();
        }
        let mut out = Footprints::default();
        for r in v
            .get("runs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let num = |k: &str| r.get(k).and_then(Value::as_u64);
            if let (Some(peak), Some(grew), Some(at_ms)) = (num("peak"), num("grew"), num("at_ms"))
            {
                out.push(Footprint { peak, grew, at_ms });
            }
        }
        out
    }

    /// The runs kept in `path`; none where it is absent or unreadable.
    #[must_use]
    pub fn load(path: &Path) -> Footprints {
        std::fs::read_to_string(path)
            .map(|t| Footprints::decode(&t))
            .unwrap_or_default()
    }

    /// Write the runs to `path`: staged in a private folder beside it, then renamed over it.
    ///
    /// # Errors
    /// Its directory could not be made, or the write failed (a full disk): the runs kept before
    /// stand, and the next run of this kind claims by them.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".footprint-")
            .tempdir_in(dir)?;
        let staged = stage.path().join("data");
        std::fs::File::create(&staged)?.write_all(self.encode().as_bytes())?;
        std::fs::rename(&staged, path)
    }
}

/// What the measuring thread shares with its [`Meter`].
struct Shared {
    peak: u64,
    claim: Option<Claim>,
}

/// THE RUN'S OWN MEASURE OF ITS WRITES: a thread that measures the run's directories every
/// [`EVERY`], tells its claim what they grew by ([`Claim::wrote`]), so a waiter counts only the
/// rest of the run's need as reserved, and keeps the most they held, which [`Self::finish`]
/// returns as the run's [`Footprint`].
pub struct Meter {
    start: u64,
    shared: Arc<Mutex<Shared>>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<u64>>,
}

impl fmt::Debug for Meter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Meter").field("start", &self.start).finish()
    }
}

impl Meter {
    /// Measure `dirs` ([`allocated`]) every [`EVERY`] from `start`, the bytes they held when the
    /// run began, holding `claim` for the run.
    #[must_use]
    pub fn start(dirs: Vec<PathBuf>, start: u64, claim: Option<Claim>) -> Meter {
        Meter::start_with(start, Box::new(move || allocated(&dirs)), claim, EVERY)
    }

    /// [`Self::start`] with its measure and its pace given.
    #[must_use]
    pub fn start_with(
        start: u64,
        mut measure: Box<dyn FnMut() -> u64 + Send>,
        claim: Option<Claim>,
        every: Duration,
    ) -> Meter {
        let shared = Arc::new(Mutex::new(Shared { peak: start, claim }));
        let (stop, stopped) = mpsc::channel::<()>();
        let theirs = Arc::clone(&shared);
        let mut look = move || {
            let now = measure();
            let mut s = theirs.lock().unwrap_or_else(PoisonError::into_inner);
            s.peak = s.peak.max(now);
            if let Some(claim) = s.claim.as_mut() {
                let _ = claim.wrote(now.saturating_sub(start));
            }
            s.peak
        };
        let thread = std::thread::Builder::new()
            .name("capacity-meter".to_owned())
            .spawn(move || {
                loop {
                    match stopped.recv_timeout(every) {
                        Err(RecvTimeoutError::Timeout) => {
                            look();
                        }
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return look(),
                    }
                }
            })
            .ok();
        Meter {
            start,
            shared,
            stop: Some(stop),
            thread,
        }
    }

    /// What the directories held when the run began.
    #[must_use]
    pub const fn started(&self) -> u64 {
        self.start
    }

    /// End the run: measure once more, let its claim go, and return what it took.
    #[must_use]
    pub fn finish(mut self) -> Footprint {
        let peak = self.halt();
        Footprint {
            peak,
            grew: peak.saturating_sub(self.start),
            at_ms: aterm_time::unix_millis().unwrap_or(0),
        }
    }

    /// Stop the thread after its last look and let the claim go: the most the directories held.
    fn halt(&mut self) -> u64 {
        drop(self.stop.take());
        let joined = self.thread.take().and_then(|t| t.join().ok());
        let mut s = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        drop(s.claim.take());
        joined.unwrap_or(s.peak).max(s.peak)
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.halt();
        }
    }
}

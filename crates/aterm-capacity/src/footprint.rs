// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A RUN'S DISK FOOTPRINT (`docs/DESIGN-adaptive-build-2026-10-24.md`, DISK-ADMIT): what one
//! kind of run writes, measured on its own directories while it runs ([`Meter`]) and kept beside
//! them ([`Footprints`]), each run tagged with the directories it measured ([`Footprint::dirs`]),
//! so the next run of that kind on those directories claims what the last ones there wrote
//! ([`Footprints::need`]). A kind never measured here claims its cold budget, less what its
//! directories already hold, and no run claims more than that budget for writing again what its
//! directories held at a peak: a peak further above what they hold now held more than one run
//! writes.

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
    allocated_until(paths, &|| false)
}

/// [`allocated`], walked by a wait ([`crate::Relief`]) that an interrupt can end: once `stopped`
/// says so, the walk ends with what it has counted.
#[must_use]
pub fn allocated_until(paths: &[PathBuf], stopped: &dyn Fn() -> bool) -> u64 {
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut total = 0u64;
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = stack.pop() {
        if stopped() {
            break;
        }
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
    /// The directories it measured, by their tag (a stable digest of their canonical paths,
    /// `atpkg::admit_disk::dirs_tag`): [`Footprints::need`] counts no run measured on other
    /// directories than the next run's, such as before they were re-pointed (the build cache's
    /// move). `None` for a run kept before runs were tagged, which counts wherever it was
    /// measured.
    pub dirs: Option<u64>,
}

impl Footprint {
    /// Whether this run was measured on the directories tagged `dirs`, or kept untagged.
    #[must_use]
    pub fn measured_on(&self, dirs: u64) -> bool {
        self.dirs.is_none_or(|d| d == dirs)
    }
}

/// What a run claims, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    /// Bytes.
    pub bytes: u64,
    /// The runs it was read from, those measured on the run's own directories
    /// ([`Footprint::measured_on`]); `0`: the cold budget less what the directories hold.
    pub runs: usize,
    /// What the run's directories hold now.
    pub holds: u64,
    /// What a run of its kind writes into empty directories.
    pub cold: u64,
    /// Whether [`Self::cold`] is the figure because it capped a kept run's peak beyond what the
    /// directories hold now: they held more than one run writes then ([`Footprints::need`]).
    pub capped: bool,
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
        } else if self.capped {
            write!(
                f,
                "{}, the cold budget, what a run of it writes into empty directories: a run among \
                 the last {} held more than that at its peak beyond what its directories hold now \
                 ({}), so they held more than one run writes then (what earlier runs left, or \
                 other directories before they were re-pointed), and it claims the budget while \
                 one of the last {KEPT} does",
                size(self.bytes),
                self.runs,
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

    /// What the next run claims when its directories, tagged `dirs` ([`Footprint::dirs`]), hold
    /// `holds` bytes: for each run kept that was measured on them ([`Footprint::measured_on`]: a
    /// run tagged with other directories was not measured here, and one kept untagged counts as it
    /// always did), what it grew them by, or what they held at its peak less what they hold now (a
    /// build directory reclaimed since is written again whole) but no more than `cold`, and the
    /// most of those. `cold` is what a run writes into EMPTY directories, so it is the most one
    /// writes again into directories emptied since: a peak further above what they hold now
    /// measured directories holding more than one run writes (what earlier runs left: the stale
    /// files of many builds, a VM disk grown over its runs; or, for a run kept untagged, other
    /// directories before they were re-pointed, as the build cache's move did), and claims `cold`
    /// ([`Estimate::capped`]). The next run's whole write, what it writes again with what it grows
    /// them by, is held to `cold`, the tightest bound while `cold` holds: after a run that grew
    /// warm directories past `cold`, emptied directories claim `cold`, not that run's peak. What a
    /// run grew them by is not capped: a run that wrote more than `cold` measured it, and its
    /// growth from emptied directories corrects a `cold` too low. Where none is kept, `cold` less
    /// what they hold already: nothing where they hold it all (a warm build directory, a VM already
    /// on disk), and that first run then measures what it really writes for the next. So where
    /// every run kept was measured on other directories, it claims as where none is kept.
    #[must_use]
    pub fn need(&self, holds: u64, cold: u64, dirs: u64) -> Estimate {
        let here = || self.runs.iter().filter(|r| r.measured_on(dirs));
        let rewrite = |r: &Footprint| r.peak.saturating_sub(holds);
        let bytes = here()
            .map(|r| r.grew.max(rewrite(r).min(cold)))
            .max()
            .unwrap_or_else(|| cold.saturating_sub(holds));
        Estimate {
            bytes,
            runs: here().count(),
            holds,
            cold,
            capped: bytes == cold && here().any(|r| rewrite(r) > cold),
        }
    }

    /// The runs as the file holds them: schema 1, each run's `dirs` key only where it is tagged,
    /// which a build that does not know the key ignores.
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
                if let Some(dirs) = r.dirs {
                    o.insert("dirs".to_owned(), Value::from(dirs));
                }
                Value::Object(o)
            })
            .collect();
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("runs".to_owned(), Value::Array(runs));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The runs `text` holds; none for text that is not [`Self::encode`]'s. A run without a
    /// `dirs` key is untagged ([`Footprint::dirs`] `None`); keys it does not know are ignored.
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
                out.push(Footprint {
                    peak,
                    grew,
                    at_ms,
                    dirs: num("dirs"),
                });
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
    /// The tag of the directories it measures ([`Footprint::dirs`]).
    dirs: Option<u64>,
    shared: Arc<Mutex<Shared>>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<u64>>,
}

impl fmt::Debug for Meter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Meter")
            .field("start", &self.start)
            .field("dirs", &self.dirs)
            .finish()
    }
}

impl Meter {
    /// Measure `dirs` ([`allocated`]) every [`EVERY`] from `start`, the bytes they held when the
    /// run began, holding `claim` for the run; `tag` names them in the run's [`Footprint`]
    /// ([`Footprint::dirs`]).
    #[must_use]
    pub fn start(dirs: Vec<PathBuf>, tag: u64, start: u64, claim: Option<Claim>) -> Meter {
        let mut meter = Meter::start_with(start, Box::new(move || allocated(&dirs)), claim, EVERY);
        meter.dirs = Some(tag);
        meter
    }

    /// [`Self::start`] with its measure and its pace given, and no tag: its run is kept
    /// untagged.
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
            dirs: None,
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

    /// End the run: measure once more, let its claim go, and return what it took, tagged with
    /// the directories it measured.
    #[must_use]
    pub fn finish(mut self) -> Footprint {
        let peak = self.halt();
        Footprint {
            peak,
            grew: peak.saturating_sub(self.start),
            at_ms: aterm_time::unix_millis().unwrap_or(0),
            dirs: self.dirs,
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

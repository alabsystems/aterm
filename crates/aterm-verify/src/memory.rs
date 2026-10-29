// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MEMORY EACH STAGE TAKES (`docs/DESIGN-adaptive-build-2026-10-24.md`, VERIFY-MEM).
//!
//! A stage's children are read by their clocks every half second, and once as each starts
//! ([`crate::exec`]'s `READ_USAGE_EVERY`, `aterm_sysprobe::group_usage`), and each read books the
//! child's process group's summed footprint into the stage's [`Meter`]: a stage's peak is the most
//! its live groups held together at any read, and never less than the largest lifetime peak the
//! kernel kept for one of their members. No sampler of its own, and no sleep.
//!
//! A stage that passed keeps its peak (0 when it started no child: [`Meter::held`]) in [`Records`],
//! one file in this user's aterm state directory ([`Book::live`]). A record is keyed by the stage,
//! the checkout it ran in (each checkout builds into a cache of its own, and a warm cache compiles
//! less than a cold one), the run's sorted crate selection for a stage that reads it (the test
//! compile and run, the doctests, the lint, and the cells a selection pulled; every other stage
//! does the same work whatever is selected), and what else changes its work: the cells asked for,
//! the test binaries run at once ([`Key`]). It keeps the last [`KEPT`] peaks, so a light run never
//! hides the heavy one before it. The next run of that stage over that key claims the most of
//! them and a fifth more, at least 1 GiB more ([`aterm_capacity::peak_claim`]); where none is
//! kept, the most any record of a wider selection in the same checkout keeps (a superset's build
//! compiles the subset's crates too; the most of them, not the least, since a wider run over a
//! warmer cache can have taken less than this one will); and where none of those either, a
//! stage that compiles ([`compiles`]) claims the cold figure, 16 GiB and at most half of
//! physical memory ([`aterm_capacity::cold_claim`]), and any other stage claims nothing
//! ([`need`]). A stage that touches no target directory ([`Lane::Pure`]: the formatter, the
//! censuses) is never measured and never claims.

use std::cell::RefCell;
use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::plan::{Lane, StageId, StageSpec};
use crate::scope::Scope;

/// The peaks one record keeps, newest last.
pub const KEPT: usize = 8;

/// The records the file keeps: past this, the one whose newest peak is oldest goes, so change
/// cones that never come back do not grow it without end.
pub const ROWS_KEPT: usize = 512;

/// The records' schema: 2 keys each record by its checkout and variant as well ([`Key`]); a file
/// of schema 1, keyed by the stage and selection alone, is read as none, and the next keep
/// writes schema 2 over it.
pub const SCHEMA: u64 = 2;

/// The record's file under this user's aterm state directory ([`Book::live`]).
pub const RECORD: &str = "verify/stage-memory.json";

/// Whether stage `id` compiles, and so claims [`aterm_capacity::cold_claim`] until a run of it is
/// measured: each such stage runs a cargo or rustdoc build of its own (a test, lint, doctest,
/// checkers or measuring compile, a cell's or the libc oracle's workspace, the export's release
/// tools, the web modules, the conformance and Objective-C drives' binaries, `spec-check`, the gpu
/// oracle's test target, the aterm the codex-alab proof drives, the lab the Linux lane boots its
/// VM with, which claims its guest's memory as a claim of its own). The stages that run only what
/// an earlier stage built (the test run, the checkers run, the measuring rows), the ones that
/// touch no target directory, and the forge's do not. Every stage is named here, so a new one is
/// placed by the compiler, not by a list it can be left out of.
#[must_use]
pub const fn compiles(id: StageId) -> bool {
    match id {
        StageId::TestCompile
        | StageId::Doctests
        | StageId::Tippy
        | StageId::Export
        | StageId::LibcOracle
        | StageId::SpecCheck
        | StageId::GpuOracle
        | StageId::CheckersCompile
        | StageId::MeasurePrebuild
        | StageId::ForeignCells
        | StageId::WasmSmoke
        | StageId::WasmGlue
        | StageId::BrowserLane
        | StageId::RedrawConformance
        | StageId::ObjcDrives
        | StageId::CodexAlabProof
        | StageId::LinuxLane => true,
        StageId::Test
        | StageId::Formatting
        | StageId::Census
        | StageId::SpecCheckers
        | StageId::MeasuringTests
        | StageId::GuiSmoke
        | StageId::Forge => false,
    }
}

/// Whether `spec` is measured and claims memory: every stage but one that touches no target
/// directory ([`Lane::Pure`]), which builds nothing.
#[must_use]
pub fn metered(spec: &StageSpec) -> bool {
    spec.lane != Lane::Pure
}

/// What one live process group of a stage holds, and what the stage held at most.
#[derive(Debug, Default)]
struct Held {
    /// Each live group's summed footprint at its last read, by its pgid.
    live: std::collections::BTreeMap<u32, u64>,
    /// The most the live groups held together at any read.
    peak: u64,
    /// The largest lifetime peak any member read had.
    lifetime: u64,
    /// Whether any read booked anything.
    read: bool,
    /// Whether any child's clock booked into the meter at all, read or not.
    started: bool,
}

/// A STAGE'S MEMORY METER: what its children's clocks read of their process groups
/// ([`Meter::book`]), each group's footprint summed over its members, the stage's held together.
#[derive(Debug, Default)]
pub struct Meter {
    held: Mutex<Held>,
}

impl Meter {
    /// A meter that has read nothing.
    #[must_use]
    pub fn new() -> Arc<Meter> {
        Arc::new(Meter::default())
    }

    /// A child of the stage started, and its clock books into this meter: a stage none of whose
    /// children is read now holds no peak ([`Meter::held`]), where one that started none held
    /// nothing.
    pub fn started(&self) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .started = true;
    }

    /// Book a read of the process group `pgid`, whose members are `members`: the group holds
    /// their footprints summed, the stage's live groups together hold the sum of each one's last
    /// read, and the stage's peak is the most that reached. A member's lifetime peak raises the
    /// least the stage's peak can be. A read that found no member (the group ended before it was
    /// read) books nothing: it is no reading of what the group held.
    pub fn book(&self, pgid: u32, members: &[aterm_sysprobe::MemberUsage]) {
        if members.is_empty() {
            return;
        }
        let mut h = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let sum = members
            .iter()
            .fold(0u64, |s, m| s.saturating_add(m.footprint));
        h.live.insert(pgid, sum);
        let together = h.live.values().fold(0u64, |s, b| s.saturating_add(*b));
        h.peak = h.peak.max(together);
        let lifetime = members.iter().map(|m| m.peak_footprint).max().unwrap_or(0);
        h.lifetime = h.lifetime.max(lifetime);
        h.read = true;
    }

    /// The group `pgid` ended: what it held is the stage's no longer.
    pub fn ended(&self, pgid: u32) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .remove(&pgid);
    }

    /// The stage's peak, bytes: the most its live groups held together at a read, and never less
    /// than the largest lifetime peak a read found; `None` when no read booked anything (no
    /// child, or a host whose groups are unread: every platform but macOS).
    #[must_use]
    pub fn peak(&self) -> Option<u64> {
        let h = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        h.read.then(|| h.peak.max(h.lifetime))
    }

    /// What the stage held at most, bytes, for its record: its [`Meter::peak`], or 0 when it
    /// started no child ([`Meter::started`]: a doctest stage over crates with no fenced Rust
    /// block), so its next run claims the margin alone and not the cold figure. `None` when
    /// children started and no read booked anything (a host whose groups are unread: every
    /// platform but macOS).
    #[must_use]
    pub fn held(&self) -> Option<u64> {
        let h = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if h.read {
            Some(h.peak.max(h.lifetime))
        } else if h.started {
            None
        } else {
            Some(0)
        }
    }
}

thread_local! {
    /// The meter of the stage this thread runs ([`metering`]).
    static CURRENT: RefCell<Option<Arc<Meter>>> = const { RefCell::new(None) };
}

/// The meter of the stage this thread runs, which each child it spawns books into
/// ([`crate::exec::run`]); `None` outside a metered stage.
#[must_use]
pub fn current() -> Option<Arc<Meter>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// This thread's stage meter ([`current`]) until the answer drops, when the one before comes
/// back.
#[must_use = "the meter is this thread's only while the guard lives"]
pub struct Metering {
    before: Option<Arc<Meter>>,
}

/// Make `meter` this thread's stage meter ([`current`]) until the answer drops: a stage's own
/// thread, and each worker thread it hands children to (the test run's pool).
pub fn metering(meter: Option<Arc<Meter>>) -> Metering {
    Metering {
        before: CURRENT.with(|c| c.replace(meter)),
    }
}

impl Drop for Metering {
    fn drop(&mut self) {
        let before = self.before.take();
        CURRENT.with(|c| *c.borrow_mut() = before);
    }
}

/// WHAT A RECORD IS KEPT FOR: a stage, the checkout it ran in, the run's crate selection, sorted,
/// where the stage reads it, and what else of the run changes its work ([`Key::of_run`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    /// The stage, by its [`StageId`]'s name.
    pub stage: String,
    /// The checkout's root, as the run named it: each builds into a cache of its own.
    pub checkout: String,
    /// The selection's crates (`-p`'s, or the change cone's), sorted and without repeats;
    /// `None` for the whole workspace, which holds every selection, and for a stage whose work
    /// is the same whatever is selected ([`reads_selection`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crates: Option<Vec<String>>,
    /// What else of the run changes the stage's work: the test binaries run at once for the test
    /// run (`jobs 4`), the cells asked for (`cells linux-arm win`) or pulled (`cells pulled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

/// Whether stage `id` does work that depends on the run's crate selection, with cells pulled by
/// it or not: the test compile and run, the doctests and the lint build and run what is selected,
/// and the cells lane, when the selection pulled it ([`crate::Ctx::cells_pulled`]), checks the
/// cells whose graph holds a selected crate. Every other stage builds and runs the same whatever
/// is selected, so its peak is kept for the workspace.
#[must_use]
pub const fn reads_selection(id: StageId, cells_pulled: bool) -> bool {
    match id {
        StageId::TestCompile | StageId::Test | StageId::Doctests | StageId::Tippy => true,
        StageId::ForeignCells | StageId::WasmSmoke | StageId::WasmGlue | StageId::BrowserLane => {
            cells_pulled
        }
        _ => false,
    }
}

impl Key {
    /// The key of stage `id` run over `scope` in a checkout named nothing, with no variant: what
    /// a test that reads one record at a time sets.
    #[must_use]
    pub fn of(id: StageId, scope: &Scope) -> Key {
        Key {
            stage: format!("{id:?}"),
            checkout: String::new(),
            crates: (!scope.is_workspace()).then(|| {
                let mut crates = scope.crates().to_vec();
                crates.sort();
                crates.dedup();
                crates
            }),
            variant: None,
        }
    }

    /// The key of stage `id` in the run `ctx`: its checkout ([`crate::Ctx::root`]), its
    /// selection where the stage reads it ([`reads_selection`]), and its variant: the requested
    /// test binaries at once for the test run, the cells for the cells lane's stages.
    #[must_use]
    pub fn of_run(id: StageId, ctx: &crate::Ctx) -> Key {
        let mut key = if reads_selection(id, ctx.cells_pulled) {
            Key::of(id, &ctx.scope)
        } else {
            Key::of(id, &Scope::workspace())
        };
        key.checkout = ctx.root.display().to_string();
        key.variant = match id {
            StageId::Test => Some(format!("jobs {}", ctx.test_jobs())),
            StageId::ForeignCells
            | StageId::WasmSmoke
            | StageId::WasmGlue
            | StageId::BrowserLane
                if ctx.cells_pulled =>
            {
                Some("cells pulled".to_owned())
            }
            StageId::ForeignCells if !ctx.cells.is_empty() => {
                let mut cells = ctx.cells.clone();
                cells.sort();
                cells.dedup();
                Some(format!("cells {}", cells.join(" ")))
            }
            _ => None,
        };
        key
    }

    /// Whether this key's selection holds `other`'s, for the same stage, checkout and variant:
    /// the workspace holds every selection, and a list holds one whose every crate it names.
    #[must_use]
    pub fn holds(&self, other: &Key) -> bool {
        self.stage == other.stage
            && self.checkout == other.checkout
            && self.variant == other.variant
            && match (&self.crates, &other.crates) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(mine), Some(theirs)) => theirs.iter().all(|c| mine.contains(c)),
            }
    }

    /// The selection as a person reads it: `the workspace`, or `-p a -p b`.
    #[must_use]
    pub fn selection(&self) -> String {
        match &self.crates {
            None => "the workspace".to_owned(),
            Some(c) if c.is_empty() => "no crate".to_owned(),
            Some(c) => c
                .iter()
                .map(|n| format!("-p {n}"))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

/// One stage's measured peak.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peak {
    /// The peak, bytes ([`Meter::peak`]).
    pub bytes: u64,
    /// When the stage ended, unix ms.
    pub at_ms: u64,
}

/// The peaks one key keeps, newest last, at most [`KEPT`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    /// What they were measured for.
    pub key: Key,
    /// The peaks.
    pub peaks: Vec<Peak>,
}

impl Row {
    /// The most it keeps.
    fn most(&self) -> Option<u64> {
        self.peaks.iter().map(|p| p.bytes).max()
    }

    /// When its newest peak was kept.
    fn newest(&self) -> u64 {
        self.peaks.iter().map(|p| p.at_ms).max().unwrap_or(0)
    }
}

/// The file's whole text, schema [`SCHEMA`].
#[derive(Serialize, Deserialize)]
struct File {
    schema: u64,
    rows: Vec<Row>,
}

/// What a lookup read ([`Records::learned`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Learned {
    /// The peak read, bytes.
    pub peak: u64,
    /// How many peaks its record keeps.
    pub runs: usize,
    /// The wider selection it was read from; `None` for the key's own record.
    pub wider: Option<Key>,
}

/// EVERY STAGE'S KEPT PEAKS ([`Row`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Records {
    rows: Vec<Row>,
}

impl Records {
    /// The records.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Keep `bytes`, measured at `at_ms`, under `key`: its record's oldest peak goes past
    /// [`KEPT`], and past [`ROWS_KEPT`] records the one whose newest peak is oldest goes.
    pub fn push(&mut self, key: Key, bytes: u64, at_ms: u64) {
        let at = match self.rows.iter().position(|r| r.key == key) {
            Some(at) => at,
            None => {
                self.rows.push(Row {
                    key,
                    peaks: Vec::new(),
                });
                self.rows.len() - 1
            }
        };
        let peaks = &mut self.rows[at].peaks;
        peaks.push(Peak { bytes, at_ms });
        let over = peaks.len().saturating_sub(KEPT);
        peaks.drain(..over);
        while self.rows.len() > ROWS_KEPT {
            let oldest = (0..self.rows.len())
                .filter(|i| *i != at)
                .min_by_key(|i| self.rows[*i].newest());
            match oldest {
                Some(i) => {
                    self.rows.remove(i);
                }
                None => break,
            }
        }
    }

    /// What `key`'s stage took: the most its own record keeps; else the most any record of a wider
    /// selection of the same stage, checkout and variant keeps ([`Key::holds`]: a superset's build
    /// compiles the subset's crates too, and the most of them, not the least, since a wider run
    /// over a warmer cache can have taken less than this one will); `None` where neither is kept.
    #[must_use]
    pub fn learned(&self, key: &Key) -> Option<Learned> {
        if let Some(row) = self.rows.iter().find(|r| r.key == *key)
            && let Some(peak) = row.most()
        {
            return Some(Learned {
                peak,
                runs: row.peaks.len(),
                wider: None,
            });
        }
        self.rows
            .iter()
            .filter(|r| r.key != *key && r.key.holds(key))
            .filter_map(|r| r.most().map(|m| (m, r)))
            .max_by_key(|(m, _)| *m)
            .map(|(peak, r)| Learned {
                peak,
                runs: r.peaks.len(),
                wider: Some(r.key.clone()),
            })
    }

    /// The records as the file holds them.
    #[must_use]
    pub fn encode(&self) -> String {
        aterm_json::to_string(&File {
            schema: SCHEMA,
            rows: self.rows.clone(),
        })
        .unwrap_or_else(|_| "{}".to_owned())
    }

    /// The records `text` holds; none for text that is not [`Self::encode`]'s.
    #[must_use]
    pub fn decode(text: &str) -> Records {
        match aterm_json::from_str::<File>(text) {
            Ok(f) if f.schema == SCHEMA => Records { rows: f.rows },
            _ => Records::default(),
        }
    }

    /// The records kept in `path`; none where it is absent or unreadable.
    #[must_use]
    pub fn load(path: &Path) -> Records {
        std::fs::read_to_string(path)
            .map(|t| Records::decode(&t))
            .unwrap_or_default()
    }

    /// Write the records to `path`: staged in a private folder beside it, then renamed over it,
    /// so a reader finds the old file or the new one whole. Two runs that write at once can lose
    /// one of their peaks; neither leaves a torn file.
    ///
    /// # Errors
    /// Its directory could not be made, or the write failed: the records kept before stand.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".stage-memory-")
            .tempdir_in(dir)?;
        let staged = stage.path().join("data");
        std::fs::File::create(&staged)?.write_all(self.encode().as_bytes())?;
        std::fs::rename(&staged, path)
    }
}

/// Why a stage claims what it does ([`StageNeed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum How {
    /// From kept peaks ([`Records::learned`]).
    Learned(Learned),
    /// No peak kept, and it compiles: the cold figure ([`aterm_capacity::cold_claim`]).
    Cold,
    /// No peak kept, and it compiles nothing.
    Light,
    /// It touches no target directory ([`metered`]): never measured, never claimed.
    Unmetered,
}

/// WHAT A STAGE CLAIMS ([`need`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageNeed {
    /// Bytes; `0` claims nothing.
    pub bytes: u64,
    /// Why.
    pub how: How,
}

impl fmt::Display for StageNeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = aterm_capacity::size(self.bytes);
        match &self.how {
            How::Learned(l) => match &l.wider {
                None => write!(
                    f,
                    "{bytes}, the most its last {} run(s) used ({}) and a fifth more, at least 1 GiB",
                    l.runs,
                    aterm_capacity::size(l.peak)
                ),
                Some(wider) => write!(
                    f,
                    "{bytes}, the most a run over a wider selection ({}) used ({}) and a fifth \
                     more, at least 1 GiB: no run over this one is kept",
                    wider.selection(),
                    aterm_capacity::size(l.peak)
                ),
            },
            How::Cold => write!(
                f,
                "{bytes} cold: no run of it is kept, and it compiles (16 GiB, at most half of \
                 physical memory)"
            ),
            How::Light => f.write_str("nothing: no run of it is kept, and it compiles nothing"),
            How::Unmetered => f.write_str("nothing: it touches no target directory"),
        }
    }
}

/// What stage `spec`, its record kept under `key` ([`Key::of_run`]), claims on a machine of
/// `physical` bytes (`None`: unread), by `records`: its learned peak and a fifth more, at least
/// 1 GiB more ([`aterm_capacity::peak_claim`]); with none kept, the cold figure for a stage that
/// compiles ([`aterm_capacity::cold_claim`]) and nothing for any other; nothing for a stage that
/// touches no target directory ([`metered`]).
#[must_use]
pub fn need(records: &Records, spec: &StageSpec, key: &Key, physical: Option<u64>) -> StageNeed {
    if !metered(spec) {
        return StageNeed {
            bytes: 0,
            how: How::Unmetered,
        };
    }
    match records.learned(key) {
        Some(l) => StageNeed {
            bytes: aterm_capacity::peak_claim(l.peak),
            how: How::Learned(l),
        },
        None if compiles(spec.id) => StageNeed {
            bytes: aterm_capacity::cold_claim(physical),
            how: How::Cold,
        },
        None => StageNeed {
            bytes: 0,
            how: How::Light,
        },
    }
}

/// THE RUN'S MEMORY BOOK: where its stages' peaks are kept, and the machine they are claimed on,
/// read once as the run begins.
#[derive(Debug)]
pub struct Book {
    /// The record's file; `None` keeps nothing (no state directory), and every stage claims as
    /// though none were kept.
    pub path: Option<PathBuf>,
    /// Physical memory, bytes; `None` where unread.
    pub physical: Option<u64>,
    /// The most memory that can ever be available here, bytes
    /// ([`aterm_capacity::Reading::reachable`]): the run's memory budget ([`Book::budget`]).
    /// `None` where physical memory is unread.
    pub reachable: Option<u64>,
    /// Held while a stage reads, adds to and writes the file: the run's stages end on threads of
    /// their own.
    lock: Mutex<()>,
}

impl Book {
    /// The live book: the record in this user's aterm state directory
    /// (`aterm_types::dirs::account_state_dir`, [`RECORD`]), and the machine read now.
    #[must_use]
    pub fn live() -> Book {
        use aterm_capacity::Machine as _;
        let reading = aterm_capacity::Live.read(None);
        Book::at(
            aterm_types::dirs::account_state_dir().map(|d| d.join(RECORD)),
            reading.physical,
            reading.reachable(),
        )
    }

    /// A book kept at `path`, on a machine of `physical` bytes of which `reachable` can ever be
    /// available: what a test sets.
    #[must_use]
    pub fn at(path: Option<PathBuf>, physical: Option<u64>, reachable: Option<u64>) -> Book {
        Book {
            path,
            physical,
            reachable,
            lock: Mutex::new(()),
        }
    }

    /// THE RUN'S MEMORY BUDGET, bytes: what its stages that claim memory may claim together
    /// ([`crate::sched::ready`]), the most that can ever be available here ([`Book::reachable`]),
    /// read once as the run began; unbounded where it is unread.
    #[must_use]
    pub fn budget(&self) -> u64 {
        self.reachable.unwrap_or(u64::MAX)
    }

    /// The peaks kept now.
    #[must_use]
    pub fn records(&self) -> Records {
        self.path.as_deref().map(Records::load).unwrap_or_default()
    }

    /// What `spec`, its record kept under `key`, claims ([`need`]).
    #[must_use]
    pub fn need(&self, spec: &StageSpec, key: &Key) -> StageNeed {
        need(&self.records(), spec, key, self.physical)
    }

    /// Keep `peak`, measured for `spec`, under `key` ([`Records::push`]); nothing for a stage
    /// that is not [`metered`], and nothing with no file.
    ///
    /// # Errors
    /// The file could not be written ([`Records::save`]).
    pub fn keep(&self, spec: &StageSpec, key: &Key, peak: u64) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if !metered(spec) {
            return Ok(());
        }
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut records = Records::load(path);
        records.push(key.clone(), peak, aterm_time::unix_millis().unwrap_or(0));
        records.save(path)
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;

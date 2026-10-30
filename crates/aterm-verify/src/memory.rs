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
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::plan::{Lane, StageId, StageSpec};
use crate::scope::Scope;

/// The record's peaks, schema and size, and the record itself, lifted into `aterm_capacity`'s
/// book so a plain build reads verify's records as its analogue
/// ([`aterm_capacity::book::chain`]): what verify keeps and reads is unchanged.
pub use aterm_capacity::book::{KEPT, Learned, Peak, ROWS_KEPT, Records, Row, SCHEMA};

/// What a record is kept for ([`StageKey::of_run`]): a stage, the checkout it ran in, the run's
/// crate selection, sorted, where the stage reads it, and what else of the run changes its work.
pub use aterm_capacity::book::Key;

/// The record's file under this user's aterm state directory ([`Book::live`]).
pub const RECORD: &str = aterm_capacity::book::VERIFY_RECORD;

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

thread_local! {
    /// The cover marker of the memory claim of the stage this thread runs ([`covering`]).
    static COVER: RefCell<Option<Arc<str>>> = const { RefCell::new(None) };
}

/// The cover marker ([`aterm_capacity::COVER_ENV`]) of the memory claim the stage this thread
/// runs holds, which each child it spawns is handed ([`crate::exec::run`]) over the run's
/// [`aterm_capacity::NO_COVER`]; `None` outside a stage that holds one.
#[must_use]
pub fn cover() -> Option<Arc<str>> {
    COVER.with(|c| c.borrow().clone())
}

/// This thread's stage cover marker ([`cover`]) until the answer drops, when the one before comes
/// back.
#[must_use = "the marker is this thread's only while the guard lives"]
pub struct Covering {
    before: Option<Arc<str>>,
}

/// Make `marker` this thread's stage cover marker ([`cover`]) until the answer drops: a stage's
/// own thread, and each worker thread it hands children to, as [`metering`] does its meter.
pub fn covering(marker: Option<Arc<str>>) -> Covering {
    Covering {
        before: COVER.with(|c| c.replace(marker)),
    }
}

impl Drop for Covering {
    fn drop(&mut self) {
        let before = self.before.take();
        COVER.with(|c| *c.borrow_mut() = before);
    }
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

/// The keys of verify's stages ([`Key`] is the book's: `aterm_capacity::book`).
pub trait StageKey {
    /// The key of stage `id` run over `scope` in a checkout named nothing, with no variant: what
    /// a test that reads one record at a time sets.
    fn of(id: StageId, scope: &Scope) -> Key;

    /// The key of stage `id` in the run `ctx`: its checkout ([`crate::Ctx::root`]), its
    /// selection where the stage reads it ([`reads_selection`]), and its variant: the requested
    /// test binaries at once for the test run, the cells for the cells lane's stages.
    fn of_run(id: StageId, ctx: &crate::Ctx) -> Key;
}

impl StageKey for Key {
    fn of(id: StageId, scope: &Scope) -> Key {
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

    fn of_run(id: StageId, ctx: &crate::Ctx) -> Key {
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
}

/// Why a stage claims what it does ([`StageNeed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum How {
    /// From kept peaks ([`Records::learned`]).
    Learned(Learned),
    /// From the peaks its stage and selection kept in another checkout
    /// ([`Records::elsewhere`]), with none kept in its own.
    Elsewhere(Learned),
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
            How::Elsewhere(l) => write!(
                f,
                "{bytes}, the most a run of it in {} used ({}) and a fifth more, at least 1 GiB: \
                 no run of it is kept in this checkout",
                l.wider
                    .as_ref()
                    .map_or("another checkout", |k| k.checkout.as_str()),
                aterm_capacity::size(l.peak)
            ),
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

/// What stage `spec`, its record kept under `key` ([`StageKey::of_run`]), claims on a machine of
/// `physical` bytes (`None`: unread), by `records`: its learned peak and a fifth more, at least
/// 1 GiB more ([`aterm_capacity::peak_claim`]); with none kept in its checkout, the most its
/// stage and selection kept in another ([`Records::elsewhere`]: a new worktree's first run, the
/// common case, would otherwise claim the cold figure for a compile some other worktree measured);
/// with none kept anywhere, the cold figure for a stage that compiles
/// ([`aterm_capacity::cold_claim`]) and nothing for any other; nothing for a stage that touches
/// no target directory ([`metered`]).
#[must_use]
pub fn need(records: &Records, spec: &StageSpec, key: &Key, physical: Option<u64>) -> StageNeed {
    if !metered(spec) {
        return StageNeed {
            bytes: 0,
            how: How::Unmetered,
        };
    }
    if let Some(l) = records.learned(key) {
        return StageNeed {
            bytes: aterm_capacity::peak_claim(l.peak),
            how: How::Learned(l),
        };
    }
    match records.elsewhere(key) {
        Some(l) => StageNeed {
            bytes: aterm_capacity::peak_claim(l.peak),
            how: How::Elsewhere(l),
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

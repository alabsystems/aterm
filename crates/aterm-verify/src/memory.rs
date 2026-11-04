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
//! warmer cache can have taken less than this one will); then the most its stage, selection and
//! variant kept in another checkout; then the most a plain `targo` build of the same selection
//! whose work holds the stage's kept ([`plain_like`], BUILD-ADMIT's book); and where none of those
//! either, a stage that compiles ([`compiles`]) claims the cold figure, 16 GiB and at most half
//! of physical memory ([`aterm_capacity::cold_claim`]), and any other stage claims nothing
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

/// The unit a live compiler process compiles ([`UnitReader`]): its command line's `--crate-name`,
/// `--test` or `--crate-type`, and its environment's `CARGO_PKG_NAME`, which cargo sets for every
/// compiler it starts ([`crate::unitgraph::UnitName`]). `None` for a process that is not one, or
/// one whose arguments could not be read (it ended first, or is another user's).
#[must_use]
pub fn read_unit(pid: u32) -> Option<crate::unitgraph::UnitName> {
    let buf = aterm_sysprobe::procargs::read(pid)?;
    let args = aterm_sysprobe::procargs::parse(&buf)?;
    unit_of(
        args.argv
            .iter()
            .map(|a| String::from_utf8_lossy(a).into_owned()),
        args.env().map(|e| String::from_utf8_lossy(e).into_owned()),
    )
}

/// [`read_unit`]'s reading of a command line `argv` and environment `env` (`KEY=value` words).
#[must_use]
pub fn unit_of(
    argv: impl IntoIterator<Item = String>,
    env: impl IntoIterator<Item = String>,
) -> Option<crate::unitgraph::UnitName> {
    let argv: Vec<String> = argv.into_iter().collect();
    let after = |flag: &str| {
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1))
            .cloned()
    };
    let crate_name = after("--crate-name")?;
    let kind = if argv.iter().any(|a| a == "--test") {
        "test".to_owned()
    } else {
        after("--crate-type")?
    };
    let package = env
        .into_iter()
        .find_map(|e| e.strip_prefix("CARGO_PKG_NAME=").map(str::to_owned))?;
    Some(crate::unitgraph::UnitName {
        package,
        kind: format!("{crate_name} {kind}"),
    })
}

/// What [`Book::keep`] did with a peak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// It is in the record.
    Kept,
    /// Light beside the heaviest peak its key kept in two weeks, which stands
    /// ([`Records::keep_unless_light`]).
    Light,
    /// Below a quarter of the peak a shaped call's claim was planned by, bytes
    /// ([`crate::shape::Part::planned`]): it compiled almost none of its work.
    LightBeside(u64),
    /// There is no record to keep it in, or the stage is not [`metered`].
    Nowhere,
}

/// The plain builds' record beside it ([`Book::plain`]): what a `targo` build a shell, an agent
/// or the lab started kept ([`plain_like`]).
pub const PLAIN_RECORD: &str = aterm_capacity::book::TARGO_RECORD;

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

/// What names a compiler process for its unit's record ([`Meter::attributing`]): read once per
/// process, `None` for one that compiles no unit or could not be read.
pub type UnitReader = fn(u32) -> Option<crate::unitgraph::UnitName>;

/// The footprint at which a stage's member is read for the unit it compiles
/// ([`Meter::attributing`]): 1 GiB, so a run reads the few heavy compilers, not every one.
pub const UNIT_FLOOR: u64 = 1 << 30;

/// What a meter that attributes peaks to units holds ([`Meter::attributing`]).
#[derive(Debug)]
struct Units {
    reader: UnitReader,
    floor: u64,
    /// Each member read so far, by pid and start: the unit it compiles (`None`: it compiles
    /// none, or could not be read) and the most it held.
    seen: std::collections::BTreeMap<(u32, u64), (Option<crate::unitgraph::UnitName>, u64)>,
}

/// CPU actually observed in a stage's child process groups. Short-lived descendants
/// can end between samples, so this is a lower bound, never a complete CPU total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObservedCpu {
    /// Sum of each observed process incarnation's largest CPU counter, nanoseconds.
    pub nanoseconds: u64,
    /// Distinct process incarnations whose counters were observed.
    pub processes: usize,
}

/// A STAGE'S RESOURCE METER: what its children's clocks read of their process groups
/// ([`Meter::book`]), each group's footprint summed over its members, the stage's held together;
/// and, for a meter that attributes them ([`Meter::attributing`]), each heavy compiler's peak by
/// the unit it compiles.
#[derive(Debug, Default)]
pub struct Meter {
    held: Mutex<Held>,
    units: Option<Mutex<Units>>,
    cpu: Mutex<std::collections::BTreeMap<(u32, u64), u64>>,
}

impl Meter {
    /// A meter that has read nothing.
    #[must_use]
    pub fn new() -> Arc<Meter> {
        Arc::new(Meter::default())
    }

    /// A meter that also attributes peaks to units: a member whose footprint reaches `floor` is
    /// read once by `reader` for the unit it compiles ([`read_unit`] live), and the most it held
    /// is that unit's ([`Meter::unit_peaks`]).
    #[must_use]
    pub fn attributing(reader: UnitReader, floor: u64) -> Arc<Meter> {
        Arc::new(Meter {
            held: Mutex::default(),
            cpu: Mutex::default(),
            units: Some(Mutex::new(Units {
                reader,
                floor,
                seen: std::collections::BTreeMap::new(),
            })),
        })
    }

    /// The most each unit's compiler held, by its name: the heavy ones the meter read
    /// ([`Meter::attributing`]), each once at most (the most of two compilers of one unit).
    #[must_use]
    pub fn unit_peaks(&self) -> std::collections::BTreeMap<crate::unitgraph::UnitName, u64> {
        let mut out = std::collections::BTreeMap::new();
        if let Some(units) = &self.units {
            let units = units.lock().unwrap_or_else(PoisonError::into_inner);
            for (name, peak) in units.seen.values() {
                if let Some(name) = name {
                    let e = out.entry(name.clone()).or_insert(0);
                    *e = (*e).max(*peak);
                }
            }
        }
        out
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
        {
            let mut cpu = self.cpu.lock().unwrap_or_else(PoisonError::into_inner);
            for member in members {
                let counter = cpu.entry((member.pid, member.start)).or_default();
                *counter = (*counter).max(member.cpu_ns);
            }
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
        drop(h);
        if let Some(units) = &self.units {
            let mut units = units.lock().unwrap_or_else(PoisonError::into_inner);
            for m in members {
                let most = m.footprint.max(m.peak_footprint);
                if let Some(seen) = units.seen.get_mut(&(m.pid, m.start)) {
                    seen.1 = seen.1.max(most);
                } else if most >= units.floor {
                    let name = (units.reader)(m.pid);
                    units.seen.insert((m.pid, m.start), (name, most));
                }
            }
        }
    }

    /// The group `pgid` ended: what it held is the stage's no longer.
    pub fn ended(&self, pgid: u32) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .remove(&pgid);
    }

    /// Retain observed CPU after groups exit, counting a process incarnation once
    /// even if it moves between groups. No sample is unknown, not zero; CPU from
    /// descendants that exited between samples and in-process verifier work is absent.
    #[must_use]
    pub fn observed_cpu(&self) -> Option<ObservedCpu> {
        let cpu = self.cpu.lock().unwrap_or_else(PoisonError::into_inner);
        (!cpu.is_empty()).then(|| ObservedCpu {
            nanoseconds: cpu
                .values()
                .fold(0_u64, |sum, value| sum.saturating_add(*value)),
            processes: cpu.len(),
        })
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
/// back. The guard stays on the thread that made it: dropped on another, it would restore
/// that thread's meter and leave this one's in place for whatever runs here next.
#[must_use = "the meter is this thread's only while the guard lives"]
pub struct Metering {
    before: Option<Arc<Meter>>,
    /// A TLS restoration guard stays on the thread where it was installed.
    _thread: std::marker::PhantomData<*mut ()>,
}

/// Make `meter` this thread's stage meter ([`current`]) until the answer drops: a stage's own
/// thread, and each worker thread it hands children to (the test run's pool). A worker makes
/// its own guard from the stage's meter; the guard itself never crosses threads:
///
/// ```compile_fail,E0277
/// // The guard sent to another thread, whose meter its drop would restore instead.
/// let guard = aterm_verify::memory::metering(None);
/// std::thread::spawn(move || drop(guard));
/// ```
pub fn metering(meter: Option<Arc<Meter>>) -> Metering {
    Metering {
        before: CURRENT.with(|c| c.replace(meter)),
        _thread: std::marker::PhantomData,
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
/// back. The guard stays on the thread that made it: dropped on another, it would restore
/// that thread's marker and leave this one's in place for whatever runs here next.
#[must_use = "the marker is this thread's only while the guard lives"]
pub struct Covering {
    before: Option<Arc<str>>,
    /// A TLS restoration guard stays on the thread where it was installed.
    _thread: std::marker::PhantomData<*mut ()>,
}

/// Make `marker` this thread's stage cover marker ([`cover`]) until the answer drops: a stage's
/// own thread, and each worker thread it hands children to, as [`metering`] does its meter.
/// A worker makes its own guard from the stage's marker; the guard itself never crosses
/// threads:
///
/// ```compile_fail,E0277
/// // The guard sent to another thread, whose marker its drop would restore instead.
/// let guard = aterm_verify::memory::covering(None);
/// std::thread::spawn(move || drop(guard));
/// ```
pub fn covering(marker: Option<Arc<str>>) -> Covering {
    Covering {
        before: COVER.with(|c| c.replace(marker)),
        _thread: std::marker::PhantomData,
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
        StageId::TestCompile
        | StageId::Test
        | StageId::Doctests
        | StageId::Tippy
        | StageId::CheckersCompile
        | StageId::SpecCheckers => true,
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

    /// The key of planned stage `spec` in the run `ctx`: a call of a shaped test compile
    /// ([`crate::shape::Part`]) keeps its record over its own crates under its variant
    /// ([`crate::shape::Part::variant`]); every other stage, an unshaped test compile included,
    /// under [`Self::of_run`].
    fn of_stage(spec: &StageSpec, ctx: &crate::Ctx) -> Key;
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
        // `-p` narrows a named `cells` as a pull does ([`crate::cells::narrows`]).
        let cells_narrowed = ctx.cells_pulled
            || (id == StageId::ForeignCells
                && matches!(ctx.scope, crate::scope::Scope::Packages(_)));
        let mut key = if matches!(id, StageId::CheckersCompile | StageId::SpecCheckers) {
            Key::of(id, &crate::stages::spec_checkers_scope(&ctx.scope))
        } else if reads_selection(id, cells_narrowed) {
            Key::of(id, &ctx.scope)
        } else {
            Key::of(id, &Scope::workspace())
        };
        key.checkout = ctx.root.display().to_string();
        key.variant = match id {
            StageId::Test | StageId::SpecCheckers => Some(format!("jobs {}", ctx.test_jobs())),
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

    fn of_stage(spec: &StageSpec, ctx: &crate::Ctx) -> Key {
        match &spec.part {
            Some(part) if part.shaped() => Key {
                stage: format!("{:?}", spec.id),
                checkout: ctx.root.display().to_string(),
                crates: Some(part.piece.crates.clone()),
                variant: part.variant(),
            },
            _ => Key::of_run(spec.id, ctx),
        }
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
    /// From the peaks a plain `targo` build of its selection kept ([`plain_like`]), with none of
    /// its own kept anywhere.
    Plain(Learned),
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
            How::Plain(l) => {
                let (verb, checkout) = l.wider.as_ref().map_or(("targo", "a checkout"), |k| {
                    (k.stage.as_str(), k.checkout.as_str())
                });
                write!(
                    f,
                    "{bytes}, the most a plain `{verb}` of it in {checkout} used ({}) and a fifth \
                     more, at least 1 GiB: verify keeps no run of it",
                    aterm_capacity::size(l.peak)
                )
            }
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

/// Whether a plain build's targets (its variant's items) hold a stage's ([`plain_work`]).
type Holds = fn(&[&str]) -> bool;

/// The plain `targo` build whose work holds verify stage `stage`'s over the same selection, and
/// which of its variants do: verify's test compile (`test --tests --no-run`: every target with
/// `test = true`) is held by a plain `targo test` of no profile, target or features that names no
/// target, `--tests` or `--all-targets`; its lint (`--all-targets`) by a plain `targo tippy` of
/// `--all-targets`. `None` for every other stage.
fn plain_work(stage: &str) -> Option<(&'static str, Holds)> {
    fn tests(targets: &[&str]) -> bool {
        targets.is_empty()
            || targets
                .iter()
                .any(|t| matches!(*t, "--tests" | "--all-targets"))
    }
    fn lint(targets: &[&str]) -> bool {
        targets.contains(&"--all-targets")
    }
    match stage {
        "TestCompile" => Some(("targo test", tests)),
        "Tippy" => Some(("targo tippy", lint)),
        _ => None,
    }
}

/// WHAT A PLAIN BUILD OF THE SAME WORK KEPT (BUILD-ADMIT's book, [`PLAIN_RECORD`]), for a stage
/// whose record `key` names its selection and no variant: the most any record of `plain` keeps,
/// in this checkout or another, for the plain build whose work holds the stage's ([`plain_work`])
/// over the same crates, of a variant that names no profile, target or features (`release`,
/// `profile …`, `target …`, `features …`, `--all-features`, `--no-default-features`) and whose
/// targets hold the stage's. A plain build of less (`--lib`, one `--test`) is never read: it
/// would bound the stage below what it builds. `None` where none is kept; its key names it.
#[must_use]
pub fn plain_like(plain: &Records, key: &Key) -> Option<Learned> {
    let (verb, holds) = plain_work(&key.stage)?;
    if key.variant.is_some() {
        return None;
    }
    let shaped = |item: &str| {
        matches!(item, "release" | "--all-features" | "--no-default-features")
            || ["profile ", "target ", "features "]
                .iter()
                .any(|p| item.starts_with(p))
    };
    plain
        .rows()
        .iter()
        .filter(|r| r.key.stage == verb && r.key.crates == key.crates)
        .filter(|r| {
            let items: Vec<&str> = r
                .key
                .variant
                .as_deref()
                .map_or_else(Vec::new, |v| v.split(", ").collect());
            !items.iter().any(|i| shaped(i)) && holds(&items)
        })
        .filter_map(|r| r.peaks.iter().map(|p| p.bytes).max().map(|m| (m, r)))
        .max_by_key(|(m, _)| *m)
        .map(|(peak, r)| Learned {
            peak,
            runs: r.peaks.len(),
            wider: Some(r.key.clone()),
        })
}

/// What stage `spec`, its record kept under `key` ([`StageKey::of_run`]), claims on a machine of
/// `physical` bytes (`None`: unread), by `records`: its learned peak and a fifth more, at least
/// 1 GiB more ([`aterm_capacity::peak_claim`]); with none kept in its checkout, the most its
/// stage and selection kept in another ([`Records::elsewhere`]: a new worktree's first run, the
/// common case, would otherwise claim the cold figure for a compile some other worktree measured);
/// with none of those, the most a plain build of the same work kept in `plain`
/// ([`plain_like`]); with none kept anywhere, the cold figure for a stage that compiles
/// ([`aterm_capacity::cold_claim`]) and nothing for any other; nothing for a stage that touches
/// no target directory ([`metered`]).
#[must_use]
pub fn need(
    records: &Records,
    plain: &Records,
    spec: &StageSpec,
    key: &Key,
    physical: Option<u64>,
) -> StageNeed {
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
    if let Some(l) = records.elsewhere(key) {
        return StageNeed {
            bytes: aterm_capacity::peak_claim(l.peak),
            how: How::Elsewhere(l),
        };
    }
    match plain_like(plain, key) {
        Some(l) => StageNeed {
            bytes: aterm_capacity::peak_claim(l.peak),
            how: How::Plain(l),
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
    /// The plain builds' record, read and never written ([`plain_like`]); `None` reads none.
    pub plain: Option<PathBuf>,
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
    /// (`aterm_types::dirs::account_state_dir`, [`RECORD`]), the plain builds' beside it
    /// ([`PLAIN_RECORD`]), and the machine read now.
    #[must_use]
    pub fn live() -> Book {
        use aterm_capacity::Machine as _;
        let reading = aterm_capacity::Live.read(None);
        let state = aterm_types::dirs::account_state_dir();
        Book::at(
            state.as_ref().map(|d| d.join(RECORD)),
            reading.physical,
            reading.reachable(),
        )
        .with_plain(state.map(|d| d.join(PLAIN_RECORD)))
    }

    /// A book kept at `path`, on a machine of `physical` bytes of which `reachable` can ever be
    /// available: what a test sets.
    #[must_use]
    pub fn at(path: Option<PathBuf>, physical: Option<u64>, reachable: Option<u64>) -> Book {
        Book {
            path,
            plain: None,
            physical,
            reachable,
            lock: Mutex::new(()),
        }
    }

    /// This book reading the plain builds' record at `plain` too ([`Book::plain`]).
    #[must_use]
    pub fn with_plain(mut self, plain: Option<PathBuf>) -> Book {
        self.plain = plain;
        self
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

    /// The plain builds' peaks kept now ([`Book::plain`]).
    #[must_use]
    pub fn plain_records(&self) -> Records {
        self.plain.as_deref().map(Records::load).unwrap_or_default()
    }

    /// What `spec`, its record kept under `key`, claims ([`need`]).
    #[must_use]
    pub fn need(&self, spec: &StageSpec, key: &Key) -> StageNeed {
        need(
            &self.records(),
            &self.plain_records(),
            spec,
            key,
            self.physical,
        )
    }

    /// Keep each unit's peak in `peaks`, read in a run in `checkout`, under its unit's key
    /// ([`crate::shape::unit_key`]), unless it was light beside the heaviest that key kept in two
    /// weeks ([`Records::keep_unless_light`]): the units a heavy test compile's planner sums. The
    /// units kept, heaviest first; none with no file.
    ///
    /// # Errors
    /// The file could not be written ([`Records::update`]).
    pub fn keep_units(
        &self,
        checkout: &str,
        peaks: &std::collections::BTreeMap<crate::unitgraph::UnitName, u64>,
    ) -> io::Result<Vec<(crate::unitgraph::UnitName, u64)>> {
        let Some(path) = &self.path else {
            return Ok(Vec::new());
        };
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let at_ms = aterm_time::unix_millis().unwrap_or(0);
        let mut kept = Vec::new();
        for (name, peak) in peaks {
            let key = crate::shape::unit_key(checkout, name);
            if Records::keep_unless_light(path, key, *peak, at_ms)? {
                kept.push((name.clone(), *peak));
            }
        }
        kept.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(kept)
    }

    /// Keep the row `key` that refuses a cut ([`crate::shape::refusal_key`]), so the planner
    /// never takes it again, under the record's lock ([`Records::update`]), so another process's
    /// write at once cannot lose it; again each run its selection's plan reads it
    /// ([`crate::shape::Part::refused`]), so its newest peak is never the oldest of a record that
    /// lets that one go past [`ROWS_KEPT`] rows. Nothing with no file.
    ///
    /// # Errors
    /// The file could not be written ([`Records::update`]).
    pub fn refuse(&self, key: &Key) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        Records::keep(path, key.clone(), 0, aterm_time::unix_millis().unwrap_or(0))
    }

    /// Keep `peak`, measured for `spec`, under `key` ([`Records::push`]); nothing for a stage
    /// that is not [`metered`], and nothing with no file. A stage that compiles ([`compiles`])
    /// keeps nothing for a run light beside the heaviest peak its key kept in the last two weeks
    /// ([`Records::keep_unless_light`]): a compile whose units were fresh (the run after a cut
    /// built them, or after a plain build) did none of the work its claim is for, and eight of
    /// those would push the heavy peak out of its record. A shaped call keeps nothing for a run
    /// below a quarter of the peak its claim was planned by ([`crate::shape::Part::planned`]),
    /// however old, even under a key that holds none: the first shaped run of a selection is
    /// often warm, and its calls' few MiB would be the first record under their keys.
    ///
    /// # Errors
    /// The file could not be written ([`Records::update`]).
    pub fn keep(&self, spec: &StageSpec, key: &Key, peak: u64) -> io::Result<Kept> {
        let Some(path) = &self.path else {
            return Ok(Kept::Nowhere);
        };
        if !metered(spec) {
            return Ok(Kept::Nowhere);
        }
        if compiles(spec.id)
            && let Some(by) = spec.part.as_ref().and_then(|p| p.planned)
            && peak < by / aterm_capacity::book::LIGHT_SHARE
        {
            return Ok(Kept::LightBeside(by));
        }
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let at_ms = aterm_time::unix_millis().unwrap_or(0);
        if compiles(spec.id) {
            return Records::keep_unless_light(path, key.clone(), peak, at_ms)
                .map(|kept| if kept { Kept::Kept } else { Kept::Light });
        }
        Records::keep(path, key.clone(), peak, at_ms).map(|()| Kept::Kept)
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;

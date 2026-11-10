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
//! does the same work whatever is selected), and what else changes its work: the cells asked for
//! ([`Key`]). It keeps the last [`KEPT`] peaks, so a light run never hides the heavy one before
//! it. The next run of that stage over that key claims the most of them and a fifth more, at
//! least 1 GiB more ([`aterm_capacity::peak_claim`]); where none is kept, the most any record of
//! a wider selection in the same checkout keeps (a superset's build compiles the subset's crates
//! too; the most of them, not the least, since a wider run over a warmer cache can have taken
//! less than this one will); then the most its stage, selection and
//! variant kept in another checkout; then the most a plain `targo` build of the same selection
//! whose work holds the stage's kept ([`plain_like`], BUILD-ADMIT's book); and where none of those
//! either, a stage that compiles ([`compiles`]) claims the lesser of two upper bounds, or the one
//! kept, each at least the most any record of a selection inside its own kept, in any checkout,
//! and a fifth more, at least 1 GiB more, never more than the cold figure: the most any record of
//! a WIDER named selection of its stage and variant kept in another checkout ([`wider_elsewhere`]:
//! a superset's build compiles the subset's crates too, the premise its checkout's wider record
//! rests on, over a cache not this checkout's), read only where no record of a selection inside
//! its own kept more and, for the test compile, no unit of its crates kept more (either shows the
//! wider run warm, its figure no bound); and what records of narrower selections of its stage and
//! variant, in any checkout, kept, where together they name every crate of its own: the sum of
//! what each part of that cover weighs, the most any record of it, or of a selection inside it,
//! kept; the cover whose sum is least; at least the most any narrower record kept ([`composed`]).
//! The sum bounds the union's peak on a premise, the one a wider selection's bound rests on too:
//! one cargo run of the union compiles no unit its parts' runs did not, with as many jobs at once,
//! so the compilers it holds together at its peak split among the parts, and each part's share is
//! units that part's own run compiled, which its peak bounds only where that run compiled them.
//! A run over a warm cache compiles little and keeps a light peak, so a part weighs at least what
//! a selection inside it kept, which its own run compiles too, and the union at least what any
//! narrower selection kept, one that straddles the parts included; a part whose every record is
//! warm still weighs light, and nothing here can tell. The margin is for the premise's other
//! gaps: the union's run can hold together two of a part's units its own run never overlapped,
//! and a dependency two parts build with different features builds once with both, a unit no
//! part's run built. A selection that no wider record elsewhere bounds and that names a crate no
//! narrower record holds claims the cold figure, 16 GiB and at most half of physical memory
//! ([`aterm_capacity::cold_claim`]); and any other stage claims nothing ([`need`]). A stage that
//! touches no target directory ([`Lane::Pure`]: the formatter, the censuses) is never measured
//! and never claims. A stage that runs test binaries ([`dispatched`]) claims nothing here and
//! keeps nothing: its dispatch claims the most its plan's binaries hold together, from each one's
//! history, once it knows which binaries run ([`crate::testrun::run_tests`]).

use std::cell::RefCell;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::plan::{Lane, StageId, StageSpec};
use crate::scope::Scope;

/// The record's peaks, schema and size, and the record itself, lifted into `aterm_capacity`'s
/// book so a plain build reads verify's records as its analogue
/// ([`aterm_capacity::book::chain`]): what verify keeps and reads is unchanged.
pub use aterm_capacity::book::{KEPT, Learned, Peak, ROWS_KEPT, Records, Row, SCHEMA, Wider};

/// What a record is kept for ([`StageKey::of_run`]): a stage, the checkout it ran in, the run's
/// crate selection, sorted, where the stage reads it, and what else of the run changes its work.
pub use aterm_capacity::book::Key;

/// The record's file under this user's aterm state directory ([`Book::live`]).
pub const RECORD: &str = aterm_capacity::book::VERIFY_RECORD;

/// What a live process of a compile is, by its command line ([`UnitReader`]): the unit it
/// compiles, and whether it is the compile's admission wrapper for that unit, waiting for or
/// running its compiler, rather than the compiler itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitRead {
    /// The unit ([`unit_of`]).
    pub unit: crate::unitgraph::UnitName,
    /// Whether it runs this very executable, which a compile names as its admission wrapper
    /// ([`crate::compile_width`]); the wrapper's marker cannot say so, as Cargo and every build
    /// script it starts carry it too.
    pub wrapper: bool,
    /// Whether the explicit `--emit` selection asks for backend code generation.
    /// Missing, unknown or unexpanded arguments are unread, never metadata-only.
    pub codegen_requested: Option<bool>,
}

/// One read of a live process's command line ([`UnitReader`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    /// It compiles this unit, or waits to as its admission wrapper.
    Unit(UnitRead),
    /// It compiles no unit.
    Nothing,
    /// Its arguments could not be read: it ended first, or is another user's.
    Unread,
}

/// The unit a live compiler process compiles ([`UnitReader`]): its command line's `--crate-name`,
/// `--test` or `--crate-type`, and its environment's `CARGO_PKG_NAME`, which cargo sets for every
/// compiler it starts ([`crate::unitgraph::UnitName`]), and whether it is the admission wrapper
/// ([`UnitRead::wrapper`]: it runs `own`, this executable).
#[must_use]
pub fn read_unit(pid: u32) -> Named {
    static OWN: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    let Some(buf) = aterm_sysprobe::procargs::read(pid) else {
        return Named::Unread;
    };
    let Some(args) = aterm_sysprobe::procargs::parse(&buf) else {
        return Named::Unread;
    };
    named(
        &args,
        OWN.get_or_init(|| std::env::current_exe().ok()).as_deref(),
    )
}

/// [`read_unit`]'s reading of one process's arguments `args`, where `own` is this executable.
#[must_use]
pub fn named(args: &aterm_sysprobe::procargs::ProcArgs2<'_>, own: Option<&Path>) -> Named {
    let env = args.env().map(|e| String::from_utf8_lossy(e).into_owned());
    let argv: Vec<String> = args
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    let codegen_requested = compiler_codegen_requested(&argv);
    match unit_of(argv, env) {
        Some(unit) => Named::Unit(UnitRead {
            unit,
            wrapper: own.is_some_and(|own| own.as_os_str().as_encoded_bytes() == args.exec_path),
            codegen_requested,
        }),
        None => Named::Nothing,
    }
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

/// Whether an explicit rustc output selection requests backend code generation.
/// Response files and missing or unknown selections cannot establish metadata-only work.
#[must_use]
pub(crate) fn compiler_codegen_requested(argv: &[String]) -> Option<bool> {
    let mut args = argv.iter();
    let mut seen = false;
    let mut codegen = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg.starts_with('@') {
            return None;
        }
        let outputs = if arg == "--emit" {
            args.next()?.as_str()
        } else if let Some(outputs) = arg.strip_prefix("--emit=") {
            outputs
        } else {
            continue;
        };
        seen = true;
        for output in outputs.split(',') {
            match output.split('=').next()? {
                "dep-info" | "metadata" | "mir" => {}
                "asm" | "llvm-bc" | "llvm-ir" | "obj" | "link" => codegen = true,
                _ => return None,
            }
        }
    }
    seen.then_some(codegen)
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
    /// There is no record to keep it in, the stage is not [`metered`], or its binaries' dispatch
    /// claims for it ([`dispatched`]).
    Nowhere,
}

/// The plain builds' record beside it ([`Book::plain`]): what a `targo` build a shell, an agent
/// or the lab started kept ([`plain_like`]).
pub const PLAIN_RECORD: &str = aterm_capacity::book::TARGO_RECORD;

/// Whether stage `id` compiles, and so claims [`aterm_capacity::cold_claim`] until a run of it,
/// or runs of narrower selections that cover its own ([`composed`]), are measured: each such
/// stage runs a cargo or rustdoc build of its own (a test, lint, doctest, checkers or measuring
/// compile, a cell's or the libc oracle's workspace, the export's release tools, the web modules,
/// the conformance and Objective-C drives' binaries, `spec-check`, the aterm the codex-alab proof
/// drives, the lab the Linux lane boots its VM with, which claims its guest's memory as a claim
/// of its own). The stages that run only what an earlier stage built (the test run, the checkers
/// run, the measuring rows), the ones that touch no target directory, and the forge's do not.
/// Every stage is named here, so a new one is placed by the compiler, not by a list it can be
/// left out of.
#[must_use]
pub const fn compiles(id: StageId) -> bool {
    match id {
        StageId::TestCompile
        | StageId::Doctests
        | StageId::Tippy
        | StageId::Export
        | StageId::LibcOracle
        | StageId::SpecCheck
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
        | StageId::ExportScan
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

/// Whether stage `id` runs test binaries through [`crate::testrun::run_tests`], whose dispatch
/// claims the memory its plan's binaries hold at once, from each one's history, once it knows
/// which run (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.4): the stage claims nothing of
/// its own ([`How::Dispatched`]) and keeps no peak.
#[must_use]
pub const fn dispatched(id: StageId) -> bool {
    matches!(id, StageId::Test | StageId::SpecCheckers)
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

/// What names a process of a compile for its unit ([`Meter::attributing`]).
pub type UnitReader = fn(u32) -> Named;

/// The least peak a unit's record keeps ([`Meter::unit_peaks`]): 1 GiB, so the record holds the
/// few heavy compilers, not every one.
pub const UNIT_FLOOR: u64 = 1 << 30;

/// One process of an attributing meter, by pid and start ([`Units::seen`]).
#[derive(Debug)]
struct Seen {
    /// What its last readable read found it to be (`None`: no unit, or never readable).
    read: Option<UnitRead>,
    /// The first and the last read that found it.
    first: std::time::Instant,
    last: std::time::Instant,
    /// The CPU its last read found, ns.
    cpu_ns: u64,
    /// The most it held, its lifetime peak included.
    most: u64,
}

/// What a meter that attributes peaks to units holds ([`Meter::attributing`]).
#[derive(Debug)]
struct Units {
    reader: UnitReader,
    floor: u64,
    /// The meter's birth, on both clocks, so a read's time can be dated ([`UnitSpan`]).
    born: (std::time::Instant, std::time::SystemTime),
    /// Each member read so far, by pid and start.
    seen: std::collections::BTreeMap<(u32, u64), Seen>,
}

/// ONE UNIT'S COMPILE AS THE STAGE'S READS SAW IT ([`Meter::unit_spans`]): its compiler's first
/// and last read, half a second apart at most from its true start and end, the CPU its last read
/// found, and the most it held; and the first read of its admission wrapper, when one was read,
/// so its wait for memory is the time between ([`crate::compile_width`]). A compiler that started
/// and ended between two reads has no span: the spans are what the reads saw, never a complete
/// list.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitSpan {
    /// The unit.
    pub unit: crate::unitgraph::UnitName,
    /// The compiler's explicit output selection, as read by [`named`]. This describes
    /// requested work, not the fraction of its CPU spent in the backend.
    pub codegen_requested: Option<bool>,
    /// When its admission wrapper was first read, ms since the Unix epoch.
    pub queued_at_unix_ms: Option<u64>,
    /// When its compiler was first read, ms since the Unix epoch.
    pub started_at_unix_ms: u64,
    /// When its compiler was last read, ms since the Unix epoch.
    pub ended_at_unix_ms: u64,
    /// The CPU its compiler's last read found, seconds.
    pub cpu_s: f64,
    /// The most its compiler held, bytes.
    pub peak_bytes: u64,
}

/// `at` in ms since the Unix epoch, dated from `born`, one instant on both clocks
/// ([`Meter::unix_ms`]).
fn unix_ms_of(born: (std::time::Instant, std::time::SystemTime), at: std::time::Instant) -> u64 {
    (born.1 + at.saturating_duration_since(born.0))
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
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
/// and, for a meter that attributes them ([`Meter::attributing`]), each compiler's span and each
/// heavy compiler's peak by the unit it compiles.
#[derive(Debug, Default)]
pub struct Meter {
    held: Mutex<Held>,
    units: Option<Mutex<Units>>,
    cpu: Mutex<std::collections::BTreeMap<(u32, u64), u64>>,
    /// Each live group's CPU rate between its last two reads, cores, and when it was last read.
    rates: Mutex<std::collections::BTreeMap<u32, (std::time::Instant, f64)>>,
}

impl Meter {
    /// A meter that has read nothing.
    #[must_use]
    pub fn new() -> Arc<Meter> {
        Arc::new(Meter::default())
    }

    /// A meter that also attributes its members to units: each member is read by `reader` for
    /// the unit it compiles ([`read_unit`] live) at every read that finds it, and is what its
    /// last readable read named. A member read between its fork and its exec still shows its
    /// parent's command line (a linker a compiler starts reads as that compiler, a compiler its
    /// wrapper starts as that wrapper); its exec settles it, and a later read corrects the first.
    /// Each compiler's reads date its span ([`Meter::unit_spans`]); the most a compiler held is
    /// its unit's peak where it reaches `floor` ([`Meter::unit_peaks`]).
    #[must_use]
    pub fn attributing(reader: UnitReader, floor: u64) -> Arc<Meter> {
        Arc::new(Meter {
            held: Mutex::default(),
            cpu: Mutex::default(),
            rates: Mutex::default(),
            units: Some(Mutex::new(Units {
                reader,
                floor,
                born: (std::time::Instant::now(), std::time::SystemTime::now()),
                seen: std::collections::BTreeMap::new(),
            })),
        })
    }

    /// The most each unit's compiler held, by its name, where that reached the meter's floor
    /// ([`Meter::attributing`]): the heavy units, each once at most (the most of two compilers of
    /// one unit). A wrapper's footprint is never its unit's.
    #[must_use]
    pub fn unit_peaks(&self) -> std::collections::BTreeMap<crate::unitgraph::UnitName, u64> {
        let mut out = std::collections::BTreeMap::new();
        if let Some(units) = &self.units {
            let units = units.lock().unwrap_or_else(PoisonError::into_inner);
            for seen in units.seen.values() {
                if let Some(read) = &seen.read
                    && !read.wrapper
                    && seen.most >= units.floor
                {
                    let e = out.entry(read.unit.clone()).or_insert(0);
                    *e = (*e).max(seen.most);
                }
            }
        }
        out
    }

    /// `at` in ms since the Unix epoch on the meter's clock: its birth's wall time and the
    /// monotonic time since, so every date it gives shares one clock (a sleep of the Mac, which
    /// that clock does not count, puts its dates behind the wall by its length). `None` for a
    /// meter that does not attribute ([`Meter::new`]).
    #[must_use]
    pub fn unix_ms(&self, at: std::time::Instant) -> Option<u64> {
        let units = self.units.as_ref()?;
        let units = units.lock().unwrap_or_else(PoisonError::into_inner);
        Some(unix_ms_of(units.born, at))
    }

    /// Each compiler the meter read at two reads at least whose first read falls in `from..=to`,
    /// as a span of its unit ([`UnitSpan`]), ordered by start: its unit's wrapper is the one of
    /// that unit first read at or before the compiler, each wrapper given to one compiler at
    /// most. A compiler read once has no length, and may be a process its fork window misnamed.
    /// Empty for a meter that does not attribute ([`Meter::new`]).
    #[must_use]
    pub fn unit_spans(&self, from: std::time::Instant, to: std::time::Instant) -> Vec<UnitSpan> {
        let Some(units) = &self.units else {
            return Vec::new();
        };
        let units = units.lock().unwrap_or_else(PoisonError::into_inner);
        let unix_ms = |at| unix_ms_of(units.born, at);
        let inside = |seen: &&Seen| seen.first >= from && seen.first <= to;
        let mut wrappers: std::collections::BTreeMap<&crate::unitgraph::UnitName, Vec<_>> =
            std::collections::BTreeMap::new();
        let mut compilers = Vec::new();
        for seen in units.seen.values().filter(inside) {
            match &seen.read {
                Some(read) if read.wrapper => {
                    wrappers.entry(&read.unit).or_default().push(seen.first);
                }
                Some(read) if seen.last > seen.first => compilers.push((seen, &read.unit)),
                Some(_) | None => {}
            }
        }
        compilers.sort_by_key(|(seen, _)| seen.first);
        for queued in wrappers.values_mut() {
            queued.sort_unstable();
        }
        compilers
            .into_iter()
            .map(|(seen, unit)| {
                let queued = wrappers.get_mut(unit).and_then(|queued| {
                    let at = queued.iter().rposition(|&q| q <= seen.first)?;
                    Some(queued.remove(at))
                });
                UnitSpan {
                    unit: unit.clone(),
                    codegen_requested: seen.read.as_ref().and_then(|read| read.codegen_requested),
                    queued_at_unix_ms: queued.map(unix_ms),
                    started_at_unix_ms: unix_ms(seen.first),
                    ended_at_unix_ms: unix_ms(seen.last),
                    cpu_s: std::time::Duration::from_nanos(seen.cpu_ns).as_secs_f64(),
                    peak_bytes: seen.most,
                }
            })
            .collect()
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
        self.book_at(pgid, members, std::time::Instant::now());
    }

    /// [`Meter::book`] as of `now`. The group's CPU rate ([`Meter::live_rate`]) is the CPU its
    /// members gained since the group's last read over the time between the reads: a member first
    /// seen brings all it has used, so a burst it started with is not left out.
    pub(crate) fn book_at(
        &self,
        pgid: u32,
        members: &[aterm_sysprobe::MemberUsage],
        now: std::time::Instant,
    ) {
        if members.is_empty() {
            return;
        }
        {
            let mut cpu = self.cpu.lock().unwrap_or_else(PoisonError::into_inner);
            let mut gained = 0_u64;
            for member in members {
                let counter = cpu.entry((member.pid, member.start)).or_default();
                gained = gained.saturating_add(member.cpu_ns.saturating_sub(*counter));
                *counter = (*counter).max(member.cpu_ns);
            }
            let mut rates = self.rates.lock().unwrap_or_else(PoisonError::into_inner);
            let rate = rates.get(&pgid).map_or(0.0, |(at, _)| {
                let span = now.saturating_duration_since(*at).as_secs_f64();
                if span > 0.0 {
                    std::time::Duration::from_nanos(gained).as_secs_f64() / span
                } else {
                    0.0
                }
            });
            rates.insert(pgid, (now, rate));
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
            let reader = units.reader;
            for m in members {
                let most = m.footprint.max(m.peak_footprint);
                let seen = units.seen.entry((m.pid, m.start)).or_insert(Seen {
                    read: None,
                    first: now,
                    last: now,
                    cpu_ns: 0,
                    most: 0,
                });
                match reader(m.pid) {
                    Named::Unit(read) => seen.read = Some(read),
                    Named::Nothing => seen.read = None,
                    Named::Unread => {}
                }
                seen.first = seen.first.min(now);
                seen.last = seen.last.max(now);
                seen.cpu_ns = seen.cpu_ns.max(m.cpu_ns);
                seen.most = seen.most.max(most);
            }
        }
    }

    /// The group `pgid` ended: what it held is the stage's no longer, nor the cores it used.
    pub fn ended(&self, pgid: u32) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .remove(&pgid);
        self.rates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&pgid);
    }

    /// The cores the stage's live process groups use now: the sum of each one's rate between its
    /// last two reads ([`Meter::book_at`]), every member of the group counted, the descendants its
    /// leader never waits for included. `None` when no group is read (every platform but macOS).
    #[must_use]
    pub fn live_rate(&self) -> Option<f64> {
        let rates = self.rates.lock().unwrap_or_else(PoisonError::into_inner);
        (!rates.is_empty()).then(|| rates.values().map(|(_, rate)| rate).sum())
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
    /// selection where the stage reads it ([`reads_selection`]), and its variant: the cells for
    /// the cells lane's stages.
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
        let mut key = if reads_selection(id, cells_narrowed) {
            Key::of(id, &ctx.scope)
        } else {
            Key::of(id, &Scope::workspace())
        };
        key.checkout = ctx.root.display().to_string();
        key.variant = match id {
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
    /// From the most a wider named selection of its stage and variant kept in another checkout
    /// ([`wider_elsewhere`]), no narrower record or unit's peak outweighing it, with none of its
    /// own kept anywhere, nor a plain build's, and it compiles: less than a cover's claim, or with
    /// no cover.
    WiderElsewhere(Wider),
    /// From records of narrower selections that name every crate of its own ([`composed`]),
    /// with none of its own kept anywhere, nor a plain build's, and it compiles: at most a wider
    /// record elsewhere's claim, or with none.
    Composed(Cover),
    /// No peak kept, and it compiles: the cold figure ([`aterm_capacity::cold_claim`]).
    Cold,
    /// No peak kept, and it compiles nothing.
    Light,
    /// It runs test binaries ([`dispatched`]): their dispatch claims what its plan holds at once.
    Dispatched,
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
            How::WiderElsewhere(w) => {
                let bound = if aterm_capacity::peak_claim(w.peak) > self.bytes {
                    "at most the cold figure (16 GiB, at most half of physical memory)"
                } else {
                    "at least 1 GiB"
                };
                write!(
                    f,
                    "{bytes}, the most a run over a wider selection ({}) in {} used ({}) and a \
                     fifth more, {bound}: no run of it is kept",
                    w.key.selection(),
                    w.key.checkout,
                    aterm_capacity::size(w.peak)
                )
            }
            How::Composed(c) => {
                let parts: Vec<String> = c
                    .parts
                    .iter()
                    .map(|p| {
                        let most = aterm_capacity::size(p.most);
                        match &p.inside {
                            None => format!("{} ({most})", p.key.selection()),
                            Some(inside) => format!(
                                "{} ({most}, kept by {} inside it)",
                                p.key.selection(),
                                inside.selection()
                            ),
                        }
                    })
                    .collect();
                let bound = if aterm_capacity::peak_claim(c.peak) > self.bytes {
                    "at most the cold figure (16 GiB, at most half of physical memory)"
                } else {
                    "at least 1 GiB"
                };
                let sum = format!(
                    "the sum of what runs of {} kept ({})",
                    and_list(&parts),
                    aterm_capacity::size(c.sum)
                );
                match &c.inside {
                    None => write!(
                        f,
                        "{bytes}, {sum} and a fifth more, {bound}: no run of it is kept"
                    ),
                    Some((inside, most)) => write!(
                        f,
                        "{bytes}, the most a run of {} inside it kept ({}), more than {sum}, and \
                         a fifth more, {bound}: no run of it is kept",
                        inside.selection(),
                        aterm_capacity::size(*most)
                    ),
                }
            }
            How::Cold => write!(
                f,
                "{bytes} cold: no run of it is kept, and it compiles (16 GiB, at most half of \
                 physical memory)"
            ),
            How::Light => f.write_str("nothing: no run of it is kept, and it compiles nothing"),
            How::Dispatched => f.write_str(
                "nothing here: its binaries' dispatch claims the most they hold together, from \
                 their history",
            ),
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

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// WHAT A WIDER SELECTION KEPT IN ANOTHER CHECKOUT, for a stage whose record `key` is kept
/// nowhere: the book's one reading of it ([`Records::wider_elsewhere`], which a plain build's
/// [`aterm_capacity::book::chain`] reads too), the most any record of its stage and variant over a
/// named selection that holds its own, in another checkout, kept, an upper bound on the premise
/// its own checkout's wider record rests on ([`Records::learned`]: a superset's build compiles the
/// subset's crates too). It is refused (`None`, as where none is kept) where it is lighter than
/// what a record of a selection inside its own, in any checkout, kept, or, for the test compile,
/// than the heaviest unit of its crates any checkout kept ([`Records::unit_floor`]): either shows
/// the wider run warm. LIMIT: a warm wider run neither outweighs answers, and nothing here can
/// tell. A one-crate selection has no narrower named selection inside it, so where it has no unit
/// floor its most wider record elsewhere always answers: for the lint, the doctests and the
/// cells, which keep no unit's peak, and for a test compile none of whose crates kept a unit
/// (one is kept only at [`UNIT_FLOOR`] or more).
#[must_use]
pub fn wider_elsewhere(records: &Records, key: &Key) -> Option<Wider> {
    records.wider_elsewhere(key, records.unit_floor(key))
}

/// A COVER OF NARROWER SELECTIONS ([`composed`]): records of a stage's narrower selections, of its
/// variant and in any checkout, that together name every crate of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    /// Each part, by selection.
    pub parts: Vec<CoverPart>,
    /// What the parts weigh, summed, bytes.
    pub sum: u64,
    /// The record of a selection inside the key's that kept the most of any, and that most,
    /// bytes, where that is more than `sum`: it straddles the parts, and its run compiled nothing
    /// the key's will not.
    pub inside: Option<(Key, u64)>,
    /// What the stage claims by, bytes: `sum`, or what `inside` kept.
    pub peak: u64,
}

/// One part of a [`Cover`]: a narrower selection, and what it weighs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverPart {
    /// The record of its selection that kept the most of it.
    pub key: Key,
    /// What it weighs, bytes: the most any record of it, or of a selection inside it, kept.
    pub most: u64,
    /// The record of a selection inside it that kept `most`, where that is more than any record
    /// of its own kept: its own run compiled less than that one did (a warmer cache).
    pub inside: Option<Key>,
}

/// The most steps [`composed`]'s search takes: past them, the least cover it found stands. Its
/// first descent finds a cover in at most a step a crate, so a selection some cover names
/// always finds one.
const COVER_STEPS: usize = 10_000;

/// A narrower selection [`composed`]'s search may take.
struct Weighed<'a> {
    /// The crates it names (bit `i`: the key's `i`th).
    names: u128,
    /// What it weighs ([`CoverPart::most`]).
    most: u64,
    /// The record of it that kept the most of it.
    key: &'a Key,
    /// The record inside it that kept `most`, where that is more ([`CoverPart::inside`]).
    inside: Option<&'a Key>,
}

/// The least-sum search of [`composed`]: each step covers the lowest crate not yet covered with
/// each part that names it, lightest first, and leaves a branch whose sum reaches the least found.
struct CoverSearch<'a> {
    /// Each narrower selection once, lightest first.
    parts: &'a [Weighed<'a>],
    /// Every crate of the key.
    full: u128,
    /// The steps left.
    steps: usize,
    /// The parts taken on this branch.
    taken: Vec<usize>,
    /// The least sum found, and its parts.
    best: Option<(u64, Vec<usize>)>,
}

impl CoverSearch<'_> {
    /// Search on from a branch that covers `covered` at `sum`.
    fn descend(&mut self, covered: u128, sum: u64) {
        if covered == self.full {
            if self.best.as_ref().is_none_or(|(least, _)| sum < *least) {
                self.best = Some((sum, self.taken.clone()));
            }
            return;
        }
        if self.steps == 0 || self.best.as_ref().is_some_and(|(least, _)| sum >= *least) {
            return;
        }
        self.steps -= 1;
        let next = 1u128 << (self.full & !covered).trailing_zeros();
        let parts = self.parts;
        for (i, part) in parts.iter().enumerate() {
            if part.names & next != 0 {
                self.taken.push(i);
                self.descend(covered | part.names, sum.saturating_add(part.most));
                self.taken.pop();
            }
        }
    }
}

/// WHAT NARROWER SELECTIONS KEPT, for a stage whose record `key` names a selection of two crates
/// or more (at most 128): the records of its stage and variant, in any checkout, over selections
/// inside its own, whose crates together name every crate of `key`'s, the cover whose sum is
/// least ([`COVER_STEPS`]). Each part weighs the most any checkout's record of it, or of a
/// selection inside it, kept ([`CoverPart::most`]): its own run compiles at least what a run of
/// a selection inside it did, so a light record of it (a run over a warm cache) bounds nothing
/// that heavier one contradicts. For the same reason the cover claims by the most any narrower
/// record kept where that is more than its sum ([`Cover::inside`]: one that straddles the
/// parts). A part may share crates with another: the sum still bounds their union. `None` where
/// a crate of the selection is in no narrower record: nothing bounds what its units take.
#[must_use]
pub fn composed(records: &Records, key: &Key) -> Option<Cover> {
    let crates = key.crates.as_deref()?;
    if crates.len() < 2 || crates.len() > 128 {
        return None;
    }
    let full = u128::MAX >> (128 - crates.len());
    let names = |theirs: &[String]| {
        theirs.iter().try_fold(0u128, |m, c| {
            crates.iter().position(|k| k == c).map(|i| m | (1u128 << i))
        })
    };
    let mut own: Vec<(u128, u64, &Key)> = Vec::new();
    for row in records.rows() {
        let k = &row.key;
        if k.stage != key.stage || k.variant != key.variant {
            continue;
        }
        let Some(mask) = k.crates.as_deref().and_then(names) else {
            continue;
        };
        let Some(most) = row.peaks.iter().map(|p| p.bytes).max() else {
            continue;
        };
        if mask == 0 || mask == full {
            continue;
        }
        match own.iter_mut().find(|p| p.0 == mask) {
            Some(p) if p.1 >= most => {}
            Some(p) => *p = (mask, most, k),
            None => own.push((mask, most, k)),
        }
    }
    if own.iter().fold(0u128, |m, p| m | p.0) != full {
        return None;
    }
    // The selection inside `within` that kept the most, where that is more than `above`; of two
    // that kept as much, the one whose crates sort first.
    let heaviest = |within: u128, above: u64| {
        own.iter()
            .filter(|q| q.0 & !within == 0 && q.1 > above)
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.2.crates.cmp(&a.2.crates)))
    };
    let mut parts: Vec<Weighed> = own
        .iter()
        .map(|&(names, most, key)| {
            let inside = heaviest(names, most);
            Weighed {
                names,
                most: inside.map_or(most, |q| q.1),
                key,
                inside: inside.map(|q| q.2),
            }
        })
        .collect();
    parts.sort_by(|a, b| {
        a.most
            .cmp(&b.most)
            .then_with(|| a.key.crates.cmp(&b.key.crates))
    });
    let mut search = CoverSearch {
        parts: &parts,
        full,
        steps: COVER_STEPS,
        taken: Vec::new(),
        best: None,
    };
    search.descend(0, 0);
    let (sum, taken) = search.best?;
    let mut parts: Vec<CoverPart> = taken
        .into_iter()
        .map(|i| CoverPart {
            key: parts[i].key.clone(),
            most: parts[i].most,
            inside: parts[i].inside.cloned(),
        })
        .collect();
    parts.sort_by(|a, b| a.key.crates.cmp(&b.key.crates));
    let inside = heaviest(full, sum).map(|q| (q.2.clone(), q.1));
    Some(Cover {
        parts,
        sum,
        peak: inside.as_ref().map_or(sum, |(_, most)| *most),
        inside,
    })
}

/// What stage `spec`, its record kept under `key` ([`StageKey::of_run`]), claims on a machine of
/// `physical` bytes (`None`: unread), by `records`: its learned peak and a fifth more, at least
/// 1 GiB more ([`aterm_capacity::peak_claim`]); with none kept in its checkout, the most its
/// stage and selection kept in another ([`Records::elsewhere`]: a new worktree's first run, the
/// common case, would otherwise claim the cold figure for a compile some other worktree measured);
/// with none of those, the most a plain build of the same work kept in `plain`
/// ([`plain_like`]); with none kept anywhere, for a stage that compiles, the LESSER of two bounds,
/// each and a fifth more, at most the cold figure: the most a wider selection kept in another
/// checkout ([`wider_elsewhere`]), and the sum of what narrower selections that cover its own
/// weigh ([`composed`]); else the cold figure ([`aterm_capacity::cold_claim`]), and nothing for
/// any other stage; nothing for a stage that touches no target directory ([`metered`]), and
/// nothing for one whose binaries' dispatch claims for them ([`dispatched`]). The two
/// are upper bounds read from records on either side of the selection, each on its premise: a
/// wider run compiled every unit this one will (the premise its checkout's wider record rests on
/// too), and the narrower runs' units together are this one's, their peaks summed
/// ([`composed`]'s). Where both premises hold, the lesser bound holds. Each is at least the most
/// any record of a selection inside its own kept: the cover is raised to it, and a wider record
/// below it, or below the test compile's heaviest unit ([`Records::unit_floor`]), is refused, not
/// raised ([`wider_elsewhere`]: that run was warm, so its figure bounds nothing, and a raised one
/// would bound only the narrower record's crates), so the lesser never answers with a light
/// record either side's floor contradicts; a light record no floor contradicts (a warm run, which
/// nothing here can tell) either can read alike, and the margin and the cold figure bound both. A
/// tie is the cover's. Only a stage that compiles reads them, so no other stage claims more than
/// before. The plan raises the unshaped test compile to at least its estimate besides
/// ([`crate::shape::shape`]), which, where narrower records are kept, is at least the heaviest
/// disjoint narrower records summed: the lesser decides the test compile's claim only where it
/// exceeds that estimate, and always decides the lint's, the doctests' and the pulled cells',
/// which no estimate raises.
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
    if dispatched(spec.id) {
        return StageNeed {
            bytes: 0,
            how: How::Dispatched,
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
        None if compiles(spec.id) => {
            let cold = aterm_capacity::cold_claim(physical);
            let wider = wider_elsewhere(records, key).map(|w| StageNeed {
                bytes: aterm_capacity::peak_claim(w.peak).min(cold),
                how: How::WiderElsewhere(w),
            });
            let cover = composed(records, key).map(|c| StageNeed {
                bytes: aterm_capacity::peak_claim(c.peak).min(cold),
                how: How::Composed(c),
            });
            match (wider, cover) {
                (Some(w), Some(c)) if w.bytes < c.bytes => w,
                (_, Some(c)) => c,
                (Some(w), None) => w,
                (None, None) => StageNeed {
                    bytes: cold,
                    how: How::Cold,
                },
            }
        }
        None => StageNeed {
            bytes: 0,
            how: How::Light,
        },
    }
}

/// Pooled compiles keep their peaks under their compiler width. Read that same history
/// when it exists; an unseen width retains the ordinary conservative estimate. In
/// particular, a pooled peak must never lower an unpooled run's claim.
#[must_use]
pub(crate) fn need_for_jobs(
    records: &Records,
    plain: &Records,
    spec: &StageSpec,
    key: &Key,
    physical: Option<u64>,
    jobs: Option<u32>,
) -> StageNeed {
    let pooled = crate::compile_width::record_key(key.clone(), jobs);
    let key = if jobs.is_some()
        && (records.learned(&pooled).is_some() || records.elsewhere(&pooled).is_some())
    {
        &pooled
    } else {
        key
    };
    need(records, plain, spec, key, physical)
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
    pub fn need(&self, spec: &StageSpec, key: &Key, jobs: Option<u32>) -> StageNeed {
        need_for_jobs(
            &self.records(),
            &self.plain_records(),
            spec,
            key,
            self.physical,
            jobs,
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
    /// that is not [`metered`], whose binaries' dispatch claims for it ([`dispatched`]), or with
    /// no file. A stage that compiles ([`compiles`])
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
        if !metered(spec) || dispatched(spec.id) {
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

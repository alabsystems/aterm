// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The leftover view's reader (`sys left`, `docs/DESIGN-leftover-processes-2026-10-16.md` §3.1,
//! §4): which rows of this uid's table are candidates, and each one's kernel facts in the pure
//! judge's plain types ([`aterm_vitals::left::Facts`]). It writes no kernel read of its own: it
//! composes this crate's readers ([`crate::process`]'s lineage, identity, markers, thread check,
//! group listing and path, [`crate::holders`]' listener, terminal and sandbox reads) through one
//! injected seam ([`LeftKernel`], [`Live`] in the window, [`Inert`] and fakes in tests).
//!
//! What it never does: read the environment or arguments of a process whose one thread the
//! thread check ([`crate::process::thread_ran`]) reads as not run (leftover M29: such a read can
//! hang on a program macOS's program check holds), read any variable but the markers
//! ([`crate::process::MARKER_VARS`], between two identity reads), read a program's arguments but
//! an aterm image's verb, or read more than [`NEW_READS`] new candidates in one reading. No read
//! is atomic with an exec, so a process that execs into a held program after its thread check can
//! still hang the environment or argument read that follows.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use aterm_vitals::left::{Facts, Held, Lin, Marks, RunTie, Waiting};

use crate::holders::HolderScan;
use crate::process::{
    Birth, EndRefusal, Ended, GroupMembers, HeldNow, Identity, KEEPS_PID_VERSIONS, Lineage,
    Markers, ProcessPresence,
};
use crate::runs::{Claims, Judge, Kernel, LiveKernel, Record, Verdict, led_groups, under_any};

/// The most candidates one reading reads anew (§3.1); the rest wait for a later reading, and the
/// view says it was bounded.
pub const NEW_READS: usize = 64;

/// The most group-mates asked for their markers on behalf of one whose own are withheld (a
/// platform binary) or unread (held).
const MATES_ASKED: usize = 8;

/// How long a reading of the listener table, or of whether a terminal's master is held, speaks
/// for later readings, ms: each scan reads every process's table, so a reading at the bars'
/// cadence takes them at most once a minute. A candidate's read that answered nothing is asked
/// again after as long. A reading for `end` ([`read_one`]) reads everything anew.
pub const SHARED_READS_MS: u64 = 60_000;

/// The kernel reads the leftover reader takes beyond a run judgment's ([`Kernel`]).
pub trait LeftKernel: Kernel {
    /// This uid's processes holding a listening unix-domain socket, from one table read
    /// ([`crate::holders::scan_unix_listeners`]), with those whose descriptors could not be
    /// matched; `None` when unread.
    fn listeners(&self) -> Option<HolderScan>;
    /// Whether a sandbox profile holds `pid` ([`crate::holders::sandboxed`]); `None` when the
    /// check fails.
    fn sandboxed(&self, pid: i32) -> Option<bool>;
    /// `pid`'s resource coalition ([`aterm_sysprobe::coalitions::coalition_of`]).
    fn coalition(&self, pid: i32) -> Option<u64>;
    /// `pid`'s arguments ([`crate::process::read_process_argv`]), asked only of an aterm image
    /// whose thread ran, for its verb.
    fn argv(&self, pid: i32) -> Option<Vec<OsString>>;
    /// Whether `pid` is an aterm instance proven the writer of its graph entry (W-writer, the
    /// caller's read), asked only of an aterm image.
    fn writer(&self, pid: i32) -> bool;
}

/// The window's [`LeftKernel`]: [`LiveKernel`]'s reads, the holders' and coalition reads, and the
/// writer read its caller supplies (`aterm_ctl::graph_writer`).
#[derive(Clone, Copy, Debug)]
pub struct Live {
    /// W-writer's read.
    pub writer: fn(u32) -> bool,
}

impl Kernel for Live {
    fn birth(&self, pid: i32) -> Option<Birth> {
        LiveKernel.birth(pid)
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        LiveKernel.presence(pid)
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        LiveKernel.lineage(pid)
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        LiveKernel.members(pgid)
    }
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        LiveKernel.exe(pid)
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        LiveKernel.markers(pid)
    }
    fn held(&self, pid: i32) -> Option<bool> {
        LiveKernel.held(pid)
    }
    fn held_now(&self) -> Option<HeldNow> {
        LiveKernel.held_now()
    }
    fn launchd_version(&self) -> Option<i32> {
        LiveKernel.launchd_version()
    }
    fn tty_master_held(&self, dev: u32) -> Option<bool> {
        LiveKernel.tty_master_held(dev)
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        LiveKernel.uid_pids()
    }
    fn now_ms(&self) -> u64 {
        LiveKernel.now_ms()
    }
    fn end(&self, token: Identity, still: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        LiveKernel.end(token, still)
    }
}

impl LeftKernel for Live {
    fn listeners(&self) -> Option<HolderScan> {
        crate::holders::scan_unix_listeners().ok()
    }
    fn sandboxed(&self, pid: i32) -> Option<bool> {
        crate::holders::sandboxed(u32::try_from(pid).ok()?)
    }
    fn coalition(&self, pid: i32) -> Option<u64> {
        aterm_sysprobe::coalitions::coalition_of(u32::try_from(pid).ok()?)
    }
    fn argv(&self, pid: i32) -> Option<Vec<OsString>> {
        crate::process::read_process_argv(pid)
    }
    fn writer(&self, pid: i32) -> bool {
        u32::try_from(pid).is_ok_and(self.writer)
    }
}

/// The inert arm: every read answers nothing, and nothing is ended.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inert;

impl Kernel for Inert {
    fn birth(&self, _: i32) -> Option<Birth> {
        None
    }
    fn lineage(&self, _: i32) -> Option<Lineage> {
        None
    }
    fn members(&self, _: u32) -> Option<GroupMembers> {
        None
    }
    fn exe(&self, _: i32) -> Option<PathBuf> {
        None
    }
    fn markers(&self, _: i32) -> Option<Markers> {
        None
    }
    fn held(&self, _: i32) -> Option<bool> {
        None
    }
    fn held_now(&self) -> Option<HeldNow> {
        None
    }
    fn launchd_version(&self) -> Option<i32> {
        None
    }
    fn tty_master_held(&self, _: u32) -> Option<bool> {
        None
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        None
    }
    fn now_ms(&self) -> u64 {
        0
    }
    fn end(&self, _: Identity, _: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        Err(EndRefusal::NotProven)
    }
}

impl LeftKernel for Inert {
    fn listeners(&self) -> Option<HolderScan> {
        None
    }
    fn sandboxed(&self, _: i32) -> Option<bool> {
        None
    }
    fn coalition(&self, _: i32) -> Option<u64> {
        None
    }
    fn argv(&self, _: i32) -> Option<Vec<OsString>> {
        None
    }
    fn writer(&self, _: i32) -> bool {
        false
    }
}

/// Whether `exe` is an aterm program's image, by its file name (`aterm`, `aterm-<verb>`), as
/// doctor's census tells one.
#[must_use]
pub fn is_aterm_image(exe: &Path) -> bool {
    exe.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name == "aterm" || name.starts_with("aterm-"))
}

/// The name of an aterm program launchd keeps by design for as long as what it serves runs,
/// by its verb (`argv[1]`): a tether's watcher ([`crate::tether::VERB`]) for the helper it
/// watches, a build's claim holder ([`crate::build_front::HOLD_VERB`]) for the build it holds
/// memory for. Neither is an orphan to report, nor a leftover to end.
#[must_use]
pub fn by_design(verb: &str) -> Option<&'static str> {
    if verb == crate::tether::VERB {
        Some("aterm tether")
    } else if verb == crate::build_front::HOLD_VERB {
        Some("aterm build hold")
    } else {
        None
    }
}

/// One row of this uid from the probe's sweep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// Its pid.
    pub pid: u32,
    /// Its parent, as the sweep read it.
    pub ppid: u32,
    /// Its process group.
    pub pgid: u32,
    /// The sweep's start stamp, in the sweep's own unit: with the pid, the key of its unique id.
    pub start: Option<u64>,
    /// Its CPU time, ns.
    pub cpu_ns: Option<u64>,
    /// When it started, unix ms, where the sweep reads that (Linux).
    pub birth_ms: Option<i64>,
}

impl Row {
    /// A sweep row in the reader's form.
    #[must_use]
    pub fn of(p: &aterm_sysprobe::procs::ProcessReading) -> Self {
        Self {
            pid: p.pid,
            ppid: p.ppid,
            pgid: p.pgid,
            start: p.start,
            cpu_ns: p.cpu_ns,
            birth_ms: p.birth_ms,
        }
    }
}

/// The rows of `sweep` a reading takes: those of `self_pid`'s user, an exited one (a zombie) left
/// out; none when the sweep does not say whose `self_pid` is.
#[must_use]
pub fn rows_of(sweep: &[aterm_sysprobe::procs::ProcessReading], self_pid: u32) -> Vec<Row> {
    let Some(uid) = sweep.iter().find(|p| p.pid == self_pid).and_then(|p| p.uid) else {
        return Vec::new();
    };
    sweep
        .iter()
        .filter(|p| p.uid == Some(uid))
        .filter(|p| p.state != aterm_sysprobe::jobs::RunState::Zombie)
        .map(Row::of)
        .collect()
}

/// Records the first `dead_seen` observation of each record of `runs` whose launcher a reading
/// found dead and that has none as read (§3.3), and takes the pair the record actually keeps into
/// the run as read. Its proposal is the highest unique id of a table of this uid read after the
/// launcher was found dead ([`crate::runs::table`]), never a sweep read before that, so whatever
/// the run started before it died is inside its window; where another reader recorded first, the
/// run takes that reader's pair instead ([`crate::runs::append_dead_seen`]). A table that does not
/// read whole proposes none, and a busy, changed, malformed or unwritable record leaves the run
/// with none: a later reading tries again, and no unrecorded horizon stands in for it.
pub fn append_dead_seen(k: &dyn LeftKernel, self_pid: u32, runs: &mut [RunRead], at_ms: u64) {
    if !runs.iter().any(|r| r.dead && r.record.dead_seen.is_none()) {
        return;
    }
    let Some((_, horizon)) = i32::try_from(self_pid)
        .ok()
        .and_then(|me| crate::runs::table(k, me))
    else {
        return;
    };
    for run in runs
        .iter_mut()
        .filter(|r| r.dead && r.record.dead_seen.is_none())
    {
        let Ok(recorded) = crate::runs::append_dead_seen(&run.path, &run.record, horizon, at_ms)
        else {
            continue;
        };
        run.record.dead_seen = Some(recorded.horizon);
        run.record.dead_seen_ms = Some(recorded.at_ms);
    }
}

/// What the host knows that the kernel does not, for one reading.
#[derive(Clone, Copy, Debug)]
pub struct Known<'a> {
    /// This process: it and its ancestors are spared.
    pub self_pid: u32,
    /// Now, unix ms.
    pub now_ms: u64,
    /// This boot's id ([`crate::process::boot_id`]): a record of another boot is no record.
    pub boot: Option<&'a str>,
    /// The run records read from this instance's rendezvous.
    pub records: &'a [(PathBuf, Record)],
    /// The programs the jobs view names as held at macOS's program check, each never asked for
    /// its environment.
    pub held: &'a [(u32, Held)],
    /// The live jobs' processes waiting behind a held one.
    pub waiting: &'a [(u32, Waiting)],
    /// The live tabs' shells, `(session key, pid)`.
    pub shells: &'a [(u64, u32)],
}

/// One run record of this boot, as a reading found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRead {
    /// Where it lives.
    pub path: PathBuf,
    /// What it says.
    pub record: Record,
    /// Its launcher reads dead on a direct read.
    pub dead: bool,
}

/// What one reading read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// Each candidate's facts, by pid.
    pub facts: Vec<Facts>,
    /// The records of this boot.
    pub runs: Vec<RunRead>,
    /// The highest unique id of this uid's table this reading saw: a process born later was not
    /// born before it.
    pub horizon: u64,
    /// launchd's pid version.
    pub launchd: Option<i32>,
    /// It stopped at [`NEW_READS`] new candidates: some wait for a later reading.
    pub bounded: bool,
}

/// What a candidate's identity keeps from one reading to the next: the reads that do not change
/// while the process runs the same image (an exec moves its identity on Apple, and its
/// executable where the kernel keeps no pid version). A read that answered nothing is asked
/// again [`SHARED_READS_MS`] after it was asked.
#[derive(Clone, Debug, Default)]
struct Kept {
    exe: Option<PathBuf>,
    /// The executable is an aterm program's image.
    aterm: bool,
    /// Its own markers, and whether its verb names a program launchd keeps by design (asked of
    /// an aterm image alone; `Some(false)` for any other), read once its thread ran; `None`
    /// while unread.
    marks: Option<Markers>,
    by_design: Option<bool>,
    /// When those were last asked, ms.
    marks_at: Option<u64>,
    /// A group-mate's, asked when its own are withheld or its thread has not run, and when.
    mate: Option<Markers>,
    mate_at: Option<u64>,
    writer: bool,
    sandboxed: Option<bool>,
    coalition: bool,
}

/// The reader: its unique-id cache, keyed by pid and sweep start, and each candidate's kept
/// reads, keyed by its identity (pid, birth and pid version), so an exec reads anew.
#[derive(Debug, Default)]
pub struct Reader {
    born: HashMap<(u32, Option<u64>), u64>,
    kept: HashMap<Identity, Kept>,
    /// The listener table and the terminals' masters as last read, each with when.
    shared: Shared,
}

/// How far one judgment of rows may read: new candidates (`None`: no bound) and how long a
/// shared table scan speaks for later readings, ms.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    new_reads: Option<usize>,
    keep_ms: u64,
}

impl Bounds {
    /// A reading's: [`NEW_READS`] new candidates, the scans kept [`SHARED_READS_MS`].
    const READING: Self = Self {
        new_reads: Some(NEW_READS),
        keep_ms: SHARED_READS_MS,
    };
    /// `end`'s re-proof: every read made anew.
    const FRESH: Self = Self {
        new_reads: None,
        keep_ms: 0,
    };
}

/// The reads one table scan answers for every candidate, kept [`SHARED_READS_MS`].
#[derive(Clone, Debug, Default)]
struct Shared {
    listeners: Option<(u64, Option<HolderScan>)>,
    masters: BTreeMap<u32, (u64, Option<bool>)>,
}

/// A record of this boot, as one reading judges its candidates against it.
struct Run<'r> {
    path: &'r Path,
    rec: &'r Record,
    id: Option<String>,
    dead: bool,
    /// A dead run's actually recorded upper bound; `None` before it is retained or while live.
    horizon: Option<u64>,
    /// Its open groups whose leader check passed, each with that leader's unique id.
    groups: Vec<(u32, u64)>,
}

/// One reading's shared reads: each made at most once, and only when a candidate needs it.
struct Pass<'p> {
    k: &'p dyn LeftKernel,
    known: &'p Known<'p>,
    runs: &'p [Run<'p>],
    claims: &'p Claims,
    roots: &'p [PathBuf],
    launchd: Option<i32>,
    by_pid: &'p HashMap<u32, &'p Row>,
    /// What the table scans said, read anew where older than `keep_ms`.
    shared: Shared,
    keep_ms: u64,
    coalition: Option<Option<u64>>,
    /// New candidate reads left this reading; `None` for no bound.
    budget: Option<usize>,
    bounded: bool,
}

impl Pass<'_> {
    /// Whether a read made at `at` speaks now.
    fn fresh(&self, at: u64) -> bool {
        self.known.now_ms.saturating_sub(at) < self.keep_ms
    }

    /// Whether a read last asked at `at` (`None`: never) is to be asked now.
    fn due(&self, at: Option<u64>) -> bool {
        at.is_none_or(|at| !self.fresh(at))
    }

    /// Whether `pid` holds a listening socket; `None` when the table did not read, or `pid`'s
    /// descriptors could not be matched against it.
    fn listens(&mut self, pid: u32) -> Option<bool> {
        if !self
            .shared
            .listeners
            .as_ref()
            .is_some_and(|(at, _)| self.fresh(*at))
        {
            self.shared.listeners = Some((self.known.now_ms, self.k.listeners()));
        }
        let scan = self.shared.listeners.as_ref()?.1.as_ref()?;
        if scan.holders.contains(&pid) {
            Some(true)
        } else if scan.unreadable.contains(&pid) || scan.sealed.contains(&pid) {
            None
        } else {
            Some(false)
        }
    }

    fn master_held(&mut self, dev: u32) -> Option<bool> {
        match self.shared.masters.get(&dev) {
            Some(&(at, held)) if self.fresh(at) => held,
            _ => {
                let held = self.k.tty_master_held(dev);
                self.shared.masters.insert(dev, (self.known.now_ms, held));
                held
            }
        }
    }

    fn own_coalition(&mut self) -> Option<u64> {
        let (k, me) = (self.k, self.known.self_pid);
        *self
            .coalition
            .get_or_insert_with(|| i32::try_from(me).ok().and_then(|me| k.coalition(me)))
    }

    /// Spends one new read; `false`, and the reading is bounded, when none is left.
    fn spend(&mut self) -> bool {
        match self.budget.as_mut() {
            None => true,
            Some(0) => {
                self.bounded = true;
                false
            }
            Some(n) => {
                *n -= 1;
                true
            }
        }
    }
}

/// The records of `known.boot` among `known.records`, each judged dead or live by a direct read
/// of its launcher.
fn this_boot<'r>(k: &dyn LeftKernel, known: &'r Known<'r>) -> Vec<Run<'r>> {
    known
        .records
        .iter()
        .filter(|(_, rec)| known.boot.is_some_and(|boot| rec.boot == boot))
        .map(|(path, rec)| {
            let dead = !crate::runs::launcher_alive(rec, k);
            Run {
                path,
                rec,
                id: rec.id.map(|i| i.text()),
                dead,
                horizon: dead.then_some(rec.dead_seen).flatten(),
                groups: led_groups(rec, k),
            }
        })
        .collect()
}

/// `me` and its ancestors the table names: never a candidate.
fn spared(me: u32, by_pid: &HashMap<u32, &Row>) -> BTreeSet<u32> {
    let mut spared = BTreeSet::from([me]);
    let mut at = me;
    while let Some(row) = by_pid.get(&at)
        && row.ppid > 1
        && spared.insert(row.ppid)
        && spared.len() < 64
    {
        at = row.ppid;
    }
    spared
}

/// The live tab whose shell `row`'s live parent chain reaches (W-chain, words only).
fn chain_of(row: &Row, by_pid: &HashMap<u32, &Row>, shells: &[(u64, u32)]) -> Option<u64> {
    let mut at = row.ppid;
    for _ in 0..64 {
        if let Some((key, _)) = shells.iter().find(|(_, shell)| *shell == at) {
            return Some(*key);
        }
        at = by_pid.get(&at).filter(|r| r.ppid > 1 && r.ppid != at)?.ppid;
    }
    None
}

/// A group-mate's markers whose thread ran and whose environment the kernel handed back, asked
/// of at most [`MATES_ASKED`] mates whose thread ran (a held mate, or a zombie, is asked
/// nothing); `None` when none answers.
fn mate_markers(k: &dyn LeftKernel, pid: i32, pgid: u32) -> Option<Markers> {
    let members = k.members(pgid)?;
    members
        .pids
        .iter()
        .copied()
        .filter(|&mate| mate != pid && mate > 1)
        .filter(|&mate| k.held(mate) == Some(false))
        .take(MATES_ASKED)
        .find_map(|mate| k.markers(mate).filter(|m| !m.withheld))
}

/// One candidate's reads this reading, as a run's own verdict ([`Judge::judge`]) asks them: its
/// lineage, executable, thread check, markers and terminal's master answered from what the
/// reading read or kept, so the verdict makes no environment read or table scan of its own;
/// another process's reads (Linux's adopter check reads the parent's lineage) are the kernel's.
struct ReadNow<'a> {
    k: &'a dyn LeftKernel,
    lin: Lineage,
    exe: Option<&'a Path>,
    held: Option<bool>,
    markers: Option<&'a Markers>,
    master: Option<bool>,
}

impl ReadNow<'_> {
    fn this(&self, pid: i32) -> bool {
        pid == self.lin.identity.pid
    }
}

impl Kernel for ReadNow<'_> {
    fn birth(&self, pid: i32) -> Option<Birth> {
        self.k.birth(pid)
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        self.k.presence(pid)
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        if self.this(pid) {
            Some(self.lin)
        } else {
            self.k.lineage(pid)
        }
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        self.k.members(pgid)
    }
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        if self.this(pid) {
            self.exe.map(Path::to_path_buf)
        } else {
            self.k.exe(pid)
        }
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        if self.this(pid) {
            self.markers.cloned()
        } else {
            self.k.markers(pid)
        }
    }
    fn held(&self, pid: i32) -> Option<bool> {
        if self.this(pid) {
            self.held
        } else {
            self.k.held(pid)
        }
    }
    fn held_now(&self) -> Option<HeldNow> {
        self.k.held_now()
    }
    fn launchd_version(&self) -> Option<i32> {
        self.k.launchd_version()
    }
    fn tty_master_held(&self, dev: u32) -> Option<bool> {
        if self.lin.tty == Some(dev) {
            self.master
        } else {
            self.k.tty_master_held(dev)
        }
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        self.k.uid_pids()
    }
    fn now_ms(&self) -> u64 {
        self.k.now_ms()
    }
    fn end(&self, _: Identity, _: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        Err(EndRefusal::NotProven)
    }
}

/// The judge's plain form of a markers read.
fn marks_of(m: &Markers) -> Marks {
    Marks {
        withheld: m.withheld,
        session: m.parent_session.clone(),
        run: m.verify_run.clone(),
    }
}

/// How `run`'s record ties a candidate of lineage `lin`, its markers `marks` and its executable
/// `exe`: `None` when it ties it by no witness and runs nothing under its roots. `auto` is asked
/// only of a dead run's member its witness ties.
fn tie(
    run: &Run<'_>,
    lin: &Lineage,
    marks: Option<&Markers>,
    exe: Option<&Path>,
    auto: impl FnOnce() -> bool,
) -> Option<RunTie> {
    let born_in = |from: u64| {
        from <= lin.uniqueid
            && (!run.dead || run.horizon.is_some_and(|horizon| lin.uniqueid <= horizon))
    };
    let group = run
        .groups
        .iter()
        .any(|&(pgid, from)| pgid == lin.pgid && born_in(from));
    let carries = run
        .id
        .as_deref()
        .is_some_and(|id| marks.and_then(|m| m.verify_run.as_deref()) == Some(id));
    let run_w = carries && born_in(run.rec.uniqueid);
    let tree = exe.is_some_and(|exe| under_any(exe, &run.rec.roots));
    if !(group || run_w || tree) {
        return None;
    }
    Some(RunTie {
        id: run.id.clone().unwrap_or_default(),
        group,
        run: run_w,
        tree,
        dead: run.dead,
        auto: run.dead && run.horizon.is_some() && (group || run_w) && auto(),
    })
}

impl Reader {
    /// A reader with nothing cached.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `pid`'s unique id, read once for each pid and sweep start.
    pub fn uniqueid(&mut self, k: &dyn LeftKernel, pid: u32, start: Option<u64>) -> Option<u64> {
        if let Some(&id) = self.born.get(&(pid, start)) {
            return Some(id);
        }
        let id = k.lineage(i32::try_from(pid).ok()?)?.uniqueid;
        self.born.insert((pid, start), id);
        Some(id)
    }

    /// Why `row` is a candidate, by the order its reads are made in (lower first): held or
    /// waiting behind a held one, orphaned, in a dead run's open group, born in a dead run's
    /// window; `None` when it is none of these.
    fn priority(&self, row: &Row, known: &Known<'_>, runs: &[Run<'_>]) -> Option<u8> {
        if known.held.iter().any(|(p, _)| *p == row.pid)
            || known.waiting.iter().any(|(p, _)| *p == row.pid)
        {
            return Some(0);
        }
        if row.ppid == 1 {
            return Some(1);
        }
        let mut dead = runs.iter().filter(|run| run.dead);
        if dead
            .clone()
            .any(|run| run.groups.iter().any(|(pgid, _)| *pgid == row.pgid))
        {
            return Some(2);
        }
        let born = *self.born.get(&(row.pid, row.start))?;
        dead.any(|run| {
            run.horizon
                .is_some_and(|horizon| (run.rec.uniqueid..=horizon).contains(&born))
        })
        .then_some(3)
    }

    /// One reading over `rows`, this uid's from the probe's sweep: each candidate's facts, at
    /// most [`NEW_READS`] of them read anew, and the records of this boot.
    pub fn read(&mut self, k: &dyn LeftKernel, rows: &[Row], known: &Known<'_>) -> Reading {
        let present: BTreeSet<(u32, Option<u64>)> = rows.iter().map(|r| (r.pid, r.start)).collect();
        self.born.retain(|key, _| present.contains(key));
        let mut horizon = 0;
        for row in rows {
            if let Some(id) = self.uniqueid(k, row.pid, row.start) {
                horizon = horizon.max(id);
            }
        }
        let runs = this_boot(k, known);
        let by_pid: HashMap<u32, &Row> = rows.iter().map(|r| (r.pid, r)).collect();
        let spared = spared(known.self_pid, &by_pid);
        let mut candidates: Vec<(u8, &Row)> = rows
            .iter()
            .filter(|r| r.pid > 1 && !spared.contains(&r.pid))
            .filter_map(|r| Some((self.priority(r, known, &runs)?, r)))
            .collect();
        candidates.sort_by_key(|(why, r)| (*why, r.pid));
        let (facts, bounded) = self.judge_rows(
            k,
            known,
            &runs,
            &by_pid,
            candidates.into_iter().map(|(_, r)| r),
            Bounds::READING,
        );
        Reading {
            facts,
            runs: runs
                .iter()
                .map(|r| RunRead {
                    path: r.path.to_path_buf(),
                    record: r.rec.clone(),
                    dead: r.dead,
                })
                .collect(),
            horizon,
            launchd: k.launchd_version(),
            bounded,
        }
    }

    fn judge_rows<'r>(
        &mut self,
        k: &dyn LeftKernel,
        known: &Known<'_>,
        runs: &[Run<'_>],
        by_pid: &HashMap<u32, &Row>,
        rows: impl Iterator<Item = &'r Row>,
        bounds: Bounds,
    ) -> (Vec<Facts>, bool) {
        let keep_ms = bounds.keep_ms;
        let claims = Claims::of(runs.iter().filter(|r| !r.dead).map(|r| r.rec));
        let roots: Vec<PathBuf> = runs
            .iter()
            .flat_map(|r| r.rec.roots.iter().cloned())
            .collect();
        let mut pass = Pass {
            k,
            known,
            runs,
            claims: &claims,
            roots: &roots,
            launchd: k.launchd_version(),
            by_pid,
            shared: std::mem::take(&mut self.shared),
            keep_ms,
            coalition: None,
            budget: bounds.new_reads,
            bounded: false,
        };
        let mut seen = HashSet::new();
        let mut facts = Vec::new();
        for row in rows {
            if let Some((f, id)) = self.facts_of(&mut pass, row) {
                seen.extend(id);
                facts.push(f);
            }
        }
        self.kept.retain(|id, _| seen.contains(id));
        facts.sort_by_key(|f| f.pid);
        let now = known.now_ms;
        pass.shared
            .masters
            .retain(|_, (at, _)| now.saturating_sub(*at) < keep_ms);
        self.shared = pass.shared;
        (facts, pass.bounded)
    }

    /// One candidate's facts, and the identity its kept reads are filed under; `None` when it is
    /// no candidate after all (launchd started it, or it is gone), or the reading's new reads
    /// ran out before it.
    fn facts_of(&mut self, pass: &mut Pass<'_>, row: &Row) -> Option<(Facts, Option<Identity>)> {
        let pid = i32::try_from(row.pid).ok()?;
        let held = pass
            .known
            .held
            .iter()
            .find(|(p, _)| *p == row.pid)
            .map(|(_, h)| *h);
        let waiting = pass
            .known
            .waiting
            .iter()
            .find(|(p, _)| *p == row.pid)
            .map(|(_, w)| *w);
        let cpu_ms = row.cpu_ns.map(|ns| ns / 1_000_000);
        let Some(lin) = pass.k.lineage(pid) else {
            // Unreadable (U7): listed by what the sweep and the jobs view say, and judged NAME;
            // nothing more is read of it.
            return (row.ppid == 1 || held.is_some() || waiting.is_some()).then(|| {
                (
                    Facts {
                        pid: row.pid,
                        ppid: row.ppid,
                        cpu_ms,
                        held,
                        waiting,
                        ..Facts::default()
                    },
                    None,
                )
            });
        };
        if lin.launchd_started(pass.launchd) {
            return None;
        }
        // A program the jobs view names as held is held, whatever its thread list says now: its
        // environment is never read (a development build's stand-in reads held this way too).
        let thread_ran = if held.is_some() {
            Some(false)
        } else {
            pass.k.held(pid).map(|held| !held)
        };
        let ran = thread_ran == Some(true);
        let id = lin.identity;
        // Where the kernel keeps no pid version (Linux) an exec keeps the identity: the
        // executable, read again each reading, says the image moved, and what was kept of the
        // image before it goes. An exec into the same executable keeps it.
        if id.idversion.is_none()
            && self
                .kept
                .get(&id)
                .is_some_and(|kept| pass.k.exe(pid) != kept.exe)
        {
            self.kept.remove(&id);
        }
        let (kept, first) = match self.kept.entry(id) {
            Entry::Vacant(slot) => {
                if !pass.spend() {
                    return None;
                }
                let exe = pass.k.exe(pid);
                let aterm = exe.as_deref().is_some_and(is_aterm_image);
                let kept = slot.insert(Kept {
                    writer: aterm && pass.k.writer(pid),
                    sandboxed: pass.k.sandboxed(pid),
                    coalition: pass
                        .own_coalition()
                        .is_some_and(|own| pass.k.coalition(pid) == Some(own)),
                    by_design: (!aterm).then_some(false),
                    aterm,
                    exe,
                    ..Kept::default()
                });
                (kept, true)
            }
            Entry::Occupied(slot) => (slot.into_mut(), false),
        };
        // What a started thread allows is read once it ran, and asked again while it answered
        // nothing; a group-mate's speaks only while its own is withheld or its thread has not
        // run. A kept candidate's reads spend one of the reading's new reads; with none left, a
        // first read since its thread ran waits its turn, and a read asked again waits a reading.
        let own_due =
            ran && (kept.marks.is_none() || kept.by_design.is_none()) && pass.due(kept.marks_at);
        let mate_wanted = |kept: &Kept| !ran || kept.marks.as_ref().is_some_and(|m| m.withheld);
        let mate_due = |kept: &Kept, pass: &Pass<'_>| {
            mate_wanted(kept) && kept.mate.is_none() && pass.due(kept.mate_at)
        };
        let may_read = first || !(own_due || mate_due(kept, &*pass)) || pass.spend();
        if !may_read && own_due && kept.marks_at.is_none() {
            return None;
        }
        if may_read && own_due {
            if kept.marks.is_none() {
                kept.marks = pass.k.markers(pid);
            }
            if kept.by_design.is_none() {
                kept.by_design = pass.k.argv(pid).map(|argv| {
                    argv.get(1)
                        .and_then(|verb| verb.to_str())
                        .is_some_and(|verb| by_design(verb).is_some())
                });
            }
            kept.marks_at = Some(pass.known.now_ms);
        }
        if may_read && mate_due(kept, &*pass) {
            kept.mate = mate_markers(pass.k, pid, lin.pgid);
            kept.mate_at = Some(pass.known.now_ms);
        }
        let mate = kept.mate.clone().filter(|_| mate_wanted(kept));
        let kept = kept.clone();
        let marks = kept.marks.as_ref().filter(|_| ran);
        let exe = kept.exe.as_deref();
        let master = lin.tty.and_then(|dev| pass.master_held(dev));
        let runs = pass
            .runs
            .iter()
            .filter_map(|run| {
                tie(run, &lin, marks, exe, || {
                    let Some(horizon) = run.horizon else {
                        return false;
                    };
                    let now = ReadNow {
                        k: pass.k,
                        lin,
                        exe,
                        held: thread_ran.map(|r| !r),
                        markers: marks,
                        master,
                    };
                    ran && Judge {
                        kernel: &now,
                        roots: &run.rec.roots,
                        id: run.id.clone(),
                        run_uniqueid: run.rec.uniqueid,
                        groups: run.groups.clone(),
                        horizon,
                        launchd: pass.launchd,
                        claims: pass.claims,
                    }
                    .judge(pid)
                        == Verdict::Proven
                })
            })
            .collect();
        let born_ms = row
            .birth_ms
            .and_then(|ms| u64::try_from(ms).ok())
            .or(KEEPS_PID_VERSIONS.then_some(lin.identity.birth.0 / 1000));
        let facts = Facts {
            pid: row.pid,
            ppid: row.ppid,
            lineage: Some(Lin {
                uniqueid: lin.uniqueid,
                parent: lin.parent,
                pgid: lin.pgid,
                launcher_version: lin.launcher_version,
                tty: lin.tty,
            }),
            token: Some(lin.identity.token()),
            exe: kept.exe.as_ref().map(|p| p.display().to_string()),
            exe_under_root: exe.is_some_and(|exe| under_any(exe, pass.roots)),
            started_ms: born_ms.map(|born| pass.known.now_ms.saturating_sub(born)),
            cpu_ms,
            thread_ran,
            held,
            waiting,
            marks: marks.map(marks_of),
            mate_marks: mate.as_ref().map(marks_of),
            listener: pass.listens(row.pid),
            sandboxed: kept.sandboxed,
            by_design: kept.by_design,
            aterm: kept.aterm,
            writer: kept.writer,
            coalition: kept.coalition,
            chain: chain_of(row, pass.by_pid, pass.known.shells),
            runs,
            claimed: pass.claims.claims(&lin, marks),
            tty_master_held: master,
        };
        Some((facts, Some(id)))
    }
}

/// A FRESH READ THAT COULD NOT BE MADE ([`read_one`]): this uid's process table did not list, so
/// whether a pid is gone is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unread;

/// `pid`'s facts on fresh state, every read made again and none bounded: what `end` re-proves
/// right before a signal. `Ok(None)` when it is gone, is this process or one of its ancestors, or
/// launchd started it.
///
/// # Errors
/// [`Unread`] when this uid's process table cannot be listed ([`crate::runs::table`]).
pub fn read_one(k: &dyn LeftKernel, pid: u32, known: &Known<'_>) -> Result<Option<Facts>, Unread> {
    let Some(lin) = i32::try_from(pid).ok().and_then(|pid| k.lineage(pid)) else {
        return Ok(None);
    };
    let (pids, _) = i32::try_from(known.self_pid)
        .ok()
        .and_then(|me| crate::runs::table(k, me))
        .ok_or(Unread)?;
    if !pids.contains(&lin.identity.pid) {
        return Ok(None);
    }
    let row = Row {
        pid,
        ppid: lin.parent,
        pgid: lin.pgid,
        ..Row::default()
    };
    let by_pid = HashMap::from([(pid, &row)]);
    let runs = this_boot(k, known);
    let mut reader = Reader::new();
    Ok(reader
        .judge_rows(
            k,
            known,
            &runs,
            &by_pid,
            std::iter::once(&row),
            Bounds::FRESH,
        )
        .0
        .pop())
}

#[cfg(test)]
#[path = "left_tests.rs"]
mod tests;

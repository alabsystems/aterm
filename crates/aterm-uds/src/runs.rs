// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verify's run records read back: the record's format, the bounded read of a whole admitted
//! snapshot, the `dead_seen` append, the kernel reads a judgment takes ([`Kernel`],
//! [`LiveKernel`]), what the live runs claim ([`Claims`]) and the A1–A7 verdict on one process
//! ([`Judge::judge`]), with W-kin: the descendants verify books of a test binary it holds
//! ([`Kin`], [`bind`], [`adopted_kin`], [`group_kin`], [`kin_at_end`]) and the sidecar that
//! keeps them beside the record ([`kin_path`], [`read_kin`]). aterm-verify writes the records
//! and runs the passes that end what this proves; the window's `sys left` and doctor read them
//! here (`docs/DESIGN-leftover-processes-2026-10-16.md` §3.3, §4.1, §4.3).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::process::{
    Birth, END_GRACE, EndControlLive, EndRefusal, Ended, GroupMembers, HeldNow, Hop, Identity,
    Lineage, MAX_HOPS, Markers, ProcessPresence, end_proven, group_members, held_programs,
    launchd_idversion, process_presence, read_birth, read_hop, read_lineage, read_process_argv,
    read_process_markers, read_process_path, read_session, thread_ran,
};

/// The kernel reads a judgment takes, injected so the classification is a unit test over a fake.
pub trait Kernel {
    /// `pid`'s birth now ([`Birth`]: microseconds on Apple, start ticks on Linux), or `None`
    /// when gone, unreadable, or not of this uid.
    fn birth(&self, pid: i32) -> Option<Birth>;
    /// Whether the pid exists when identity cannot be read. Unsupported readers stay unknown.
    fn presence(&self, _pid: i32) -> ProcessPresence {
        ProcessPresence::Unknown
    }
    /// `pid`'s lineage (flavor 18; Linux `/proc`), or `None` when gone or not ours.
    fn lineage(&self, pid: i32) -> Option<Lineage>;
    /// `pgid`'s members, or `None` when the table cannot answer.
    fn members(&self, pgid: u32) -> Option<GroupMembers>;
    /// `pid`'s executable path, or `None` (gone, deleted, not ours, or no reader).
    fn exe(&self, pid: i32) -> Option<PathBuf>;
    /// `pid`'s step of lineage with its parent's unique id ([`read_hop`], Apple), or `None`: gone,
    /// not ours, a zombie, or a kernel that names no parent by a unique id. The default reads
    /// none, so a kernel that lays out no hops books no kin by them ([`bind`], [`adopted_kin`]).
    fn hop(&self, _pid: i32) -> Option<Hop> {
        None
    }
    /// Whether `pid`'s verb (`argv[1]`) names an aterm program launchd keeps by design
    /// ([`crate::left::by_design`]: a tether's watcher, a build's claim holder), or `None` where
    /// its arguments do not read. Asked only of a process whose thread ran. The default reads
    /// none, which W-kin reads as not proven ([`Judge::judge`]).
    fn by_design(&self, _pid: i32) -> Option<bool> {
        None
    }
    /// `pid`'s session ([`read_session`]: the pid of the process that made it with `setsid`), or
    /// `None` where it does not read. The default reads none, so a kernel that lays out no
    /// sessions proves no member of a kin's group by its session ([`group_kin`]).
    fn session(&self, _pid: i32) -> Option<u32> {
        None
    }
    /// `pid`'s marker variables, or `None` when unreadable.
    fn markers(&self, pid: i32) -> Option<Markers>;
    /// Whether macOS's program check may hold `pid`: `Some(false)` only once its one thread ran ([`thread_ran`]), so a lone unstarted thread at any age, arch or volume reads `Some(true)`: its environment is never read, and it is never signalled. `None` when unreadable.
    fn held(&self, pid: i32) -> Option<bool>;
    /// This user's programs macOS's program check holds now by its whole predicate ([`held_programs`], doctor's reader: one thread list per process of this uid, a lone unstarted thread 20 s or more past its fork, not stopped, not traced, native, on a local volume, with one mount table for them all and each age its own start record's): what verify's run-start line counts, never what a pass acts on ([`Self::held`] is that). `None` when the listing fails, and off macOS.
    fn held_now(&self) -> Option<HeldNow>;
    /// launchd's pid version; `None` when unreadable.
    fn launchd_version(&self) -> Option<i32>;
    /// Whether a live process holds the master of the PTY whose slave is `dev`, a member's
    /// controlling terminal ([`crate::holders::pty_master_held`]: macOS reads this uid's
    /// descriptor tables; Linux reads every PTY a live process still has held). `None` when
    /// unknown, and only `Some(false)` lets A7 hold.
    fn tty_master_held(&self, dev: u32) -> Option<bool>;
    /// Every live pid of this uid, or `None` when the listing fails.
    fn uid_pids(&self) -> Option<Vec<i32>>;
    /// Now, unix milliseconds, for a record's `dead_seen`/`end` lines.
    fn now_ms(&self) -> u64;
    /// End the process `token` names, re-evaluating `still` before each signal ([`end_proven`]).
    ///
    /// # Errors
    /// [`EndRefusal`]: the process moved, `still` failed, or a signal was refused.
    fn end(&self, token: Identity, still: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal>;
}

/// The shipped [`Kernel`]: [`crate::process`] and [`crate::holders`].
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveKernel;

impl Kernel for LiveKernel {
    fn birth(&self, pid: i32) -> Option<Birth> {
        read_birth(pid)
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        process_presence(pid)
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        read_lineage(pid)
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        group_members(pgid)
    }
    // A deleted executable reads `None` (Linux marks its link ` (deleted)`), so it is under
    // no root: C-tree never matches a file that is gone. Under a binary translator on Linux
    // (Rosetta in an aarch64 VM) the kernel names the translator as every translated process's
    // executable. Where `binfmt_misc` hands the translator its program (its `O` flag, as
    // Rosetta's registration has it) the read is that program, or `None`
    // (`docs/measured/linux-rosetta-exe-2026-10-30.md`), as on macOS under Rosetta 2; under one
    // that opens its program by path it is the translator, under no root.
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        read_process_path(pid)
    }
    fn hop(&self, pid: i32) -> Option<Hop> {
        read_hop(pid)
    }
    fn by_design(&self, pid: i32) -> Option<bool> {
        let argv = read_process_argv(pid)?;
        Some(
            argv.get(1)
                .and_then(|verb| verb.to_str())
                .and_then(crate::left::by_design)
                .is_some(),
        )
    }
    fn session(&self, pid: i32) -> Option<u32> {
        read_session(pid)
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        read_process_markers(pid)
    }
    fn held(&self, pid: i32) -> Option<bool> {
        thread_ran(pid).map(|ran| !ran)
    }
    fn held_now(&self) -> Option<HeldNow> {
        held_programs()
    }
    fn launchd_version(&self) -> Option<i32> {
        launchd_idversion()
    }
    fn tty_master_held(&self, dev: u32) -> Option<bool> {
        crate::holders::pty_master_held(u64::from(dev))
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        let pids = crate::holders::uid_pids().ok()?;
        Some(
            pids.into_iter()
                .filter_map(|p| i32::try_from(p).ok())
                .collect(),
        )
    }
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }
    fn end(&self, token: Identity, still: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        end_proven(token, END_GRACE, &mut EndControlLive, |id| still(id))
    }
}

/// A run's launcher identity, the record's `id`: `<pid>-<birth>`, the birth in [`Birth`]'s unit
/// (microseconds on Apple, start ticks on Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunId {
    /// The launcher's pid.
    pub pid: i32,
    /// Its birth.
    pub birth: Birth,
}

impl RunId {
    /// The id as `<pid>-<birth>`.
    #[must_use]
    pub fn text(&self) -> String {
        format!("{}-{}", self.pid, self.birth.0)
    }

    /// Parse `<pid>-<birth>`, each in digits alone; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (pid, birth) = text.split_once('-')?;
        Some(Self {
            pid: digits(pid)?,
            birth: Birth(digits(birth)?),
        })
    }
}

/// `text` as a number when it is written in ASCII digits alone: a sign, a space or any other
/// spelling `str::parse` takes is no number of a record's.
fn digits<T: std::str::FromStr>(text: &str) -> Option<T> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The record's `group` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupLine {
    /// The leader's pid and process group (one, since every stage child is `process_group(0)`).
    pub pgid: u32,
    /// The leader's birth, so a reused pgid led by another process is told apart (`0`: unread).
    pub leader_birth: Birth,
    /// The leader's unique id: a member's must lie in `[leader_uniqueid, dead_seen]`.
    pub leader_uniqueid: u64,
    /// The stage's label (the child's argv, cut at a whole argument by
    /// `aterm_verify::exec::stage_label`), pct-encoded on the wire so a space survives, and cut
    /// again to at most `aterm_verify::runs::STAGE_FIELD_MAX` encoded bytes (`stage_field`).
    pub stage: String,
    /// A `closed` line followed it: released proven empty, so it holds nothing.
    pub closed: bool,
}

/// A run record read back from disk: its header fields, its roots and its stage groups.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// The launcher's id.
    pub id: Option<RunId>,
    /// The boot this record was written under.
    pub boot: String,
    /// The launcher's unique id.
    pub uniqueid: u64,
    /// When the run started, unix milliseconds.
    pub start_ms: u64,
    /// The session the run rode in (`ATERM_PARENT_SESSION_ID`), when it had one.
    pub sid: Option<String>,
    /// The canonical roots a member's exe must sit under for C-tree, the worktree first.
    pub roots: Vec<PathBuf>,
    /// The stage groups made, in order, each marked when a `closed` line released it.
    pub groups: Vec<GroupLine>,
    /// The uniqueid horizon the first reader that found the launcher dead wrote. Two readers may
    /// race to record it; the first persisted observation stands.
    pub dead_seen: Option<u64>,
    /// When that first reader found it dead, unix milliseconds.
    pub dead_seen_ms: Option<u64>,
}

impl Record {
    /// Parse a record's text.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut rec = Record::default();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            match words.next() {
                Some("run") => {
                    for kv in words {
                        let Some((k, v)) = kv.split_once('=') else {
                            continue;
                        };
                        match k {
                            "boot" => rec.boot = v.to_owned(),
                            "id" => rec.id = RunId::parse(v),
                            "uniqueid" => rec.uniqueid = digits(v).unwrap_or(0),
                            "start" => rec.start_ms = digits(v).unwrap_or(0),
                            "sid" => rec.sid = (v != "-").then(|| pct_decode(v)),
                            _ => {}
                        }
                    }
                }
                Some("root") => {
                    if let Some(path) = kv_value(words.next(), "path") {
                        rec.roots.push(PathBuf::from(pct_decode(path)));
                    }
                }
                Some("group") => {
                    let mut g = GroupLine {
                        pgid: 0,
                        leader_birth: Birth(0),
                        leader_uniqueid: 0,
                        stage: String::new(),
                        closed: false,
                    };
                    for kv in words {
                        let Some((k, v)) = kv.split_once('=') else {
                            continue;
                        };
                        match k {
                            "pgid" => g.pgid = digits(v).unwrap_or(0),
                            "leader_birth" => g.leader_birth = Birth(digits(v).unwrap_or(0)),
                            "leader_uniqueid" => g.leader_uniqueid = digits(v).unwrap_or(0),
                            "stage" => g.stage = pct_decode(v),
                            _ => {}
                        }
                    }
                    if g.pgid > 1 {
                        rec.groups.push(g);
                    }
                }
                Some("closed") => {
                    let pgid: Option<u32> = kv_value(words.next(), "pgid").and_then(digits);
                    if let Some(group) = rec.groups.iter_mut().rev().find(|g| Some(g.pgid) == pgid)
                    {
                        group.closed = true;
                    }
                }
                Some("dead_seen") if rec.dead_seen.is_none() => {
                    rec.dead_seen = kv_value(words.next(), "uniqueid").and_then(digits);
                    rec.dead_seen_ms = rec
                        .dead_seen
                        .and(kv_value(words.next(), "at").and_then(digits));
                }
                _ => {}
            }
        }
        rec
    }

    /// The groups with no `closed` line: those that may still hold members.
    pub fn open_groups(&self) -> impl Iterator<Item = &GroupLine> {
        self.groups.iter().filter(|g| !g.closed)
    }
}

/// `words.next()` as `k`'s value when it is `k=<value>`.
fn kv_value<'a>(word: Option<&'a str>, k: &str) -> Option<&'a str> {
    word?
        .split_once('=')
        .filter(|(key, _)| *key == k)
        .map(|(_, v)| v)
}

/// A record's opening lines.
#[must_use]
pub fn header_text(
    boot: &str,
    id: RunId,
    uniqueid: u64,
    start_ms: u64,
    sid: Option<&str>,
    roots: &[PathBuf],
) -> String {
    let mut text = format!(
        "run v=1 boot={boot} id={} uniqueid={uniqueid} start={start_ms} sid={}\n",
        id.text(),
        sid.map_or_else(|| "-".to_owned(), pct_encode),
    );
    for root in roots {
        text.push_str(&format!(
            "root path={}\n",
            pct_encode(&root.display().to_string())
        ));
    }
    text
}

/// The sidecar beside run record `record` (`<id>.run`) that keeps its kin: `<id>.kin`. It is
/// a file of its own because the cleanup reader of the records ([`read_records`]) refuses a
/// record with a line it does not know, and with it every record of its snapshot, as every
/// verify since a084efcf9ac (2026-10-22) reads them: a kin line in the record would have such a
/// verify, running beside this one or after it, refuse the records. (The first readers, from
/// a771dc301aa until then, skipped such a line.) [`read_records`] reads `.run` files alone.
#[must_use]
pub fn kin_path(record: &Path) -> PathBuf {
    record.with_extension("kin")
}

/// A kin sidecar's opening lines: `kin v=1 boot=<boot> id=<id> tty=<dev|->`, then one
/// `spared path=<dir>` line per directory W-kin never proves ([`KinJudge`]).
#[must_use]
pub fn kin_header_text(boot: &str, id: RunId, tty: Option<u32>, spared: &[PathBuf]) -> String {
    let mut text = format!(
        "kin v=1 boot={boot} id={} tty={}\n",
        id.text(),
        tty.map_or_else(|| "-".to_owned(), |dev| dev.to_string()),
    );
    for dir in spared {
        text.push_str(&format!(
            "spared path={}\n",
            pct_encode(&dir.display().to_string())
        ));
    }
    text
}

/// One booked kin's line: `kin pid=<n> birth=<n> uniqueid=<n> of=<pgid>`, `of` the stage group
/// whose test binary it descends from, which the record's `group` line names with its stage.
/// Every field is a number, so a line is at most [`KIN_LINE_BYTES`] bytes.
#[must_use]
pub fn kin_line(kin: &Kin, of: u32) -> String {
    format!(
        "kin pid={} birth={} uniqueid={} of={of}",
        kin.pid, kin.birth.0, kin.uniqueid
    )
}

/// The most bytes a [`kin_line`] takes, its newline included: four numbers of at most 20 digits.
pub const KIN_LINE_BYTES: usize = "kin pid= birth= uniqueid= of=\n".len() + 4 * 20;

/// The most kin lines a sidecar holds ([`read_kin`] refuses more): within the record's read bound
/// with its header and its spared lines, whatever a run's test binaries leave. A writer that
/// reaches it writes no more, and what it books past it is the live run's alone.
pub const KIN_LINES_MAX: usize = 32_768;

/// A kin sidecar read back ([`read_kin`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KinRecord {
    /// The boot it was written under.
    pub boot: String,
    /// Its run's id.
    pub id: RunId,
    /// Its run's own terminal ([`KinJudge::tty`]).
    pub tty: Option<u32>,
    /// The directories W-kin never proves for its run ([`KinJudge::spared`]).
    pub spared: Vec<PathBuf>,
    /// The kin it booked, in the order it booked them.
    pub kin: Vec<Kin>,
}

/// The kin sidecar at `path`, read under the record's shared lock and bounded as a record is:
/// `Ok(None)` for one that is not a complete supported sidecar of the run its name names (cut
/// short, a line or a field it does not know, a header naming another run), which gives no kin.
///
/// # Errors
/// The open's or the read's, [`std::io::ErrorKind::InvalidData`] for a file that is no bounded
/// regular one, or [`std::io::ErrorKind::WouldBlock`] while a writer holds it.
pub fn read_kin(path: &Path) -> std::io::Result<Option<KinRecord>> {
    Ok(parse_kin(path, &read_record_text(path)?))
}

/// [`read_kin`]'s strict reading of `text`, the sidecar at `path`.
#[must_use]
pub fn parse_kin(path: &Path, text: &str) -> Option<KinRecord> {
    if text.is_empty() || !text.ends_with('\n') {
        return None;
    }
    let mut record: Option<KinRecord> = None;
    for (index, line) in text.lines().enumerate() {
        match (line.split_whitespace().next()?, index) {
            ("kin", 0) => {
                let fields = cleanup_fields(line, &["v", "boot", "id", "tty"])?;
                let [version, boot, id, tty] = fields.as_slice() else {
                    return None;
                };
                let parsed = RunId::parse(id)?;
                if *version != "1"
                    || boot.is_empty()
                    || parsed.pid <= 1
                    || parsed.text() != *id
                    || path.file_name()?.to_str()? != format!("{id}.kin")
                {
                    return None;
                }
                let tty = match *tty {
                    "-" => None,
                    dev => Some(digits::<u32>(dev)?),
                };
                record = Some(KinRecord {
                    boot: (*boot).to_owned(),
                    id: parsed,
                    tty,
                    spared: Vec::new(),
                    kin: Vec::new(),
                });
            }
            ("spared", _) => {
                let fields = cleanup_fields(line, &["path"])?;
                let dir = cleanup_decoded(fields.first()?)?;
                if dir.is_empty() {
                    return None;
                }
                record.as_mut()?.spared.push(PathBuf::from(dir));
            }
            ("kin", _) => {
                let fields = cleanup_fields(line, &["pid", "birth", "uniqueid", "of"])?;
                let [pid, birth, uniqueid, of] = fields.as_slice() else {
                    return None;
                };
                let pid = digits::<i32>(pid).filter(|&pid| pid > 1)?;
                digits::<u32>(of)?;
                let record = record.as_mut()?;
                if record.kin.len() >= KIN_LINES_MAX {
                    return None;
                }
                record.kin.push(Kin {
                    pid,
                    birth: Birth(digits(birth)?),
                    uniqueid: digits(uniqueid)?,
                });
            }
            _ => return None,
        }
    }
    record
}

/// A failed birth read preserves the process unless a separate query confirms it holds
/// nothing: no process has the pid, or the one that has it exited and waits to be reaped.
pub fn may_match_birth(
    kernel: &(impl Kernel + ?Sized),
    pid: i32,
    matches: impl FnOnce(Birth) -> bool,
) -> bool {
    match kernel.birth(pid) {
        Some(birth) => matches(birth),
        None => !matches!(
            kernel.presence(pid),
            ProcessPresence::Absent | ProcessPresence::Exited
        ),
    }
}

/// Whether a record's launcher may still be alive; unreadable identities retain its claims.
pub fn launcher_alive(rec: &Record, kernel: &(impl Kernel + ?Sized)) -> bool {
    let Some(id) = rec.id else {
        return false;
    };
    may_match_birth(kernel, id.pid, |birth| birth == id.birth)
}

/// What the live runs claim: each run's launcher, its open stage groups, and its id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Claims {
    /// The launchers, `(pid, birth)`: a verify started inside another run's stage carries
    /// that run's id in its own environment, and is its own run's all the same.
    launchers: BTreeSet<(i32, Birth)>,
    groups: BTreeSet<u32>,
    ids: BTreeSet<String>,
}

impl Claims {
    /// What the records `live` (each one whose launcher lives) claim.
    pub fn of<'a>(live: impl Iterator<Item = &'a Record>) -> Self {
        let mut claims = Self::default();
        for rec in live {
            claims.groups.extend(rec.open_groups().map(|g| g.pgid));
            if let Some(id) = rec.id {
                claims.launchers.insert((id.pid, id.birth));
                claims.ids.insert(id.text());
            }
        }
        claims
    }

    /// Whether a live run claims `lin` with `markers`: it is a live run's launcher, its group is
    /// a live run's open group, or it carries a live run's id. Where a live run has an id to look
    /// for, markers that are unread or withheld claim it, fail-closed.
    #[must_use]
    pub fn claims(&self, lin: &Lineage, markers: Option<&Markers>) -> bool {
        let identity = (lin.identity.pid, lin.identity.birth);
        if self.launchers.contains(&identity) || self.groups.contains(&lin.pgid) {
            return true;
        }
        if self.ids.is_empty() {
            return false;
        }
        match markers {
            Some(m) if !m.withheld => m
                .verify_run
                .as_ref()
                .is_some_and(|id| self.ids.contains(id)),
            _ => true,
        }
    }
}

/// What one judgment of a process found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not the run's.
    NotOurs,
    /// Born in the run's window, running from its roots, held at the program check (or its
    /// check unreadable), and in none of its groups: whether it carries the run's id is not
    /// read.
    Unjudged,
    /// The run's, and ending it is not proven.
    Named,
    /// The run's, and ending it is proven.
    Proven,
}

/// The open groups of `rec` whose leader still reads the birth its `group` line recorded, each
/// with that leader's unique id (W-group's window opens there). A gone leader frees its pgid, so
/// its group is no witness.
pub fn led_groups(rec: &Record, kernel: &(impl Kernel + ?Sized)) -> Vec<(u32, u64)> {
    rec.open_groups()
        .filter(|g| {
            kernel
                .birth(i32::try_from(g.pgid).unwrap_or(0))
                .is_some_and(|b| b == g.leader_birth)
        })
        .map(|g| (g.pgid, g.leader_uniqueid))
        .collect()
}

/// A process a verify run booked as one of a test binary's descendants while it held that
/// binary unreaped (W-kin, `docs/DESIGN-leftover-processes-2026-10-16.md` §4.1): who it was by
/// pid, birth and unique id. A process that reads all three is that process: on Apple no other
/// process of the boot holds its unique id; on Linux the unique id is its start tick, which only
/// another process born in the same tick at the same pid could share.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kin {
    /// Its pid.
    pub pid: i32,
    /// Its birth ([`Birth`]'s unit).
    pub birth: Birth,
    /// Its unique id ([`Lineage::uniqueid`]).
    pub uniqueid: u64,
}

impl Kin {
    /// The kin `lin` reads as.
    #[must_use]
    pub const fn of(lin: &Lineage) -> Self {
        Self {
            pid: lin.identity.pid,
            birth: lin.identity.birth,
            uniqueid: lin.uniqueid,
        }
    }

    /// Whether `lin` reads as this process: its pid, its birth and its unique id all equal.
    #[must_use]
    pub fn is(&self, lin: &Lineage) -> bool {
        *self == Self::of(lin)
    }
}

/// How verify ties a child to its parent when it books it ([`bind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// By the parent's unique id the child names ([`Hop::parent_uniqueid`]), set at its fork
    /// and at each exec: Apple.
    UniqueId,
    /// By the parent the child names now, born no later than the child: where no parent is
    /// named by a unique id (Linux, [`Kernel::lineage`]).
    Birth,
}

/// This kernel's [`Link`].
pub const LINK: Link = if cfg!(target_vendor = "apple") {
    Link::UniqueId
} else {
    Link::Birth
};

/// One step of a chain [`bind`] reads.
struct Step {
    kin: Kin,
    parent: u32,
    parent_uniqueid: Option<u64>,
}

impl Step {
    fn read(kernel: &(impl Kernel + ?Sized), pid: i32, link: Link) -> Option<Self> {
        match link {
            Link::UniqueId => kernel.hop(pid).map(|h| Self {
                kin: Kin {
                    pid,
                    birth: h.identity.birth,
                    uniqueid: h.uniqueid,
                },
                parent: h.parent,
                parent_uniqueid: Some(h.parent_uniqueid),
            }),
            Link::Birth => kernel.lineage(pid).map(|l| Self {
                kin: Kin::of(&l),
                parent: l.parent,
                parent_uniqueid: None,
            }),
        }
    }

    /// Whether this step names `parent` as its parent, by `link`.
    fn names(&self, parent: &Kin, link: Link) -> bool {
        i32::try_from(self.parent).is_ok_and(|pid| pid == parent.pid)
            && match link {
                Link::UniqueId => self.parent_uniqueid == Some(parent.uniqueid),
                Link::Birth => parent.birth <= self.kin.birth,
            }
    }
}

/// What W-kin reads a process's program as ([`program`], [`program_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Program {
    /// Its thread has not run ([`Kernel::held`]), or its executable or its verb does not read.
    Unread,
    /// A program W-kin never proves, never books and books nothing through: under a
    /// [`KinJudge::spared`] directory, an aterm image ([`crate::left::is_aterm_image`]), a
    /// `trustd` (a name, which here only spares), or a verb launchd keeps by design
    /// ([`Kernel::by_design`]).
    Spared,
    /// No spared program.
    Other,
    /// Read [`Program::Other`], then no longer read as the process it was read for
    /// ([`program_of`]): it exited, or its pid was taken, while its program was read.
    Gone,
}

/// `pid`'s [`Program`], `spared` the directories W-kin never proves: its thread is read first,
/// so no executable or argument of a held program is. `reads` counts the reads made.
fn program(
    kernel: &(impl Kernel + ?Sized),
    pid: i32,
    spared: &[PathBuf],
    reads: &mut usize,
) -> Program {
    *reads += 1;
    if kernel.held(pid) != Some(false) {
        return Program::Unread;
    }
    *reads += 1;
    let Some(exe) = kernel.exe(pid) else {
        return Program::Unread;
    };
    if under_any(&exe, spared)
        || crate::left::is_aterm_image(&exe)
        || exe.file_name().is_some_and(|name| name == "trustd")
    {
        return Program::Spared;
    }
    *reads += 1;
    match kernel.by_design(pid) {
        Some(false) => Program::Other,
        Some(true) => Program::Spared,
        None => Program::Unread,
    }
}

/// The [`program`] of the process `kin` names: a reading of [`Program::Other`] stands only
/// where that pid reads as `kin` again after it ([`Kin::is`]), so the program read was that
/// process's and not one that took its pid since; [`Program::Gone`] otherwise. `reads` counts
/// the reads made.
fn program_of(
    kernel: &(impl Kernel + ?Sized),
    kin: &Kin,
    spared: &[PathBuf],
    reads: &mut usize,
) -> Program {
    let read = program(kernel, kin.pid, spared, reads);
    if read != Program::Other {
        return read;
    }
    *reads += 1;
    if kernel.lineage(kin.pid).is_some_and(|lin| kin.is(&lin)) {
        read
    } else {
        Program::Gone
    }
}

/// The kin `pid` is when its chain of parents, read hop by hop now, reaches `root` (the test
/// binary while verify holds it unreaped, so its pid names it; `None` once it may be reaped, so
/// a chain ends only at a booked kin) or a process of `booked` that still reads as booked: each
/// child names its parent by pid and, by `link`, by that parent's unique id
/// ([`Link::UniqueId`]) or by a parent born no later than it ([`Link::Birth`]), and `pid` and
/// every process of the chain between it and `root`, the booked kin it ends at included, reads
/// as no spared program ([`program_of`]: [`Program::Other`]), so `bind` gives no spared program
/// and nothing whose chain runs through one. `None` where a hop does not read (gone, another
/// user's, a zombie), a link fails, a program on the chain reads otherwise (spared, held,
/// unread, or gone), the chain meets launchd or init first (an orphan: [`adopted_kin`] is its
/// book), or it is longer than [`MAX_HOPS`]. `spared` holds the directories W-kin never proves
/// ([`KinJudge::spared`]); `reads` counts the reads made.
pub fn bind(
    kernel: &(impl Kernel + ?Sized),
    pid: i32,
    root: Option<Kin>,
    booked: &BTreeSet<Kin>,
    spared: &[PathBuf],
    link: Link,
    reads: &mut usize,
) -> Option<Kin> {
    *reads += 1;
    let child = Step::read(kernel, pid, link)?;
    if program_of(kernel, &child.kin, spared, reads) != Program::Other {
        return None;
    }
    let mut at = Step {
        kin: child.kin,
        parent: child.parent,
        parent_uniqueid: child.parent_uniqueid,
    };
    for _ in 0..MAX_HOPS {
        let parent = i32::try_from(at.parent).ok().filter(|&p| p > 1)?;
        if let Some(root) = root
            && parent == root.pid
        {
            return at.names(&root, link).then_some(child.kin);
        }
        *reads += 1;
        let up = Step::read(kernel, parent, link)?;
        if !at.names(&up.kin, link) || program_of(kernel, &up.kin, spared, reads) != Program::Other
        {
            return None;
        }
        if booked.contains(&up.kin) {
            return Some(child.kin);
        }
        at = up;
    }
    None
}

/// What a test binary's end adds to its book where the kernel names a parent by its unique id
/// ([`Link::UniqueId`], Apple): each process of this uid born after `root` (the binary) whose
/// parent unique id names `root`, a process of `booked`, or one found here, read in unique-id
/// order so a parent is read before its child, and that reads as no spared program
/// ([`program_of`]: [`Program::Other`]; `spared` as [`bind`] reads it). launchd's adoption
/// leaves a process's parent unique id, so this finds the binary's descendants no tree read
/// saw, those born after the last one among them, wherever each process between them and the
/// binary was found too; one that exec'd after its adoption names launchd from then on, and is
/// not found. A process that reads otherwise (a spared program, or one whose thread, executable
/// or verb does not read) is not returned and links nothing below it, and neither does a process
/// of `booked` that reads so now; one of `booked` that has exited links what it left, as it read
/// as no spared program when it was booked. A process in `root`'s own group (`root_pgid`) links
/// those below it and is not returned: verify's group end and its sweep are its. Neither are
/// those already in `booked`. `reads` counts the reads made: a hop per listed process, and the
/// programs read.
pub fn adopted_kin(
    kernel: &(impl Kernel + ?Sized),
    root: Kin,
    root_pgid: u32,
    booked: &BTreeSet<Kin>,
    spared: &[PathBuf],
    reads: &mut usize,
) -> Vec<Kin> {
    let Some(pids) = kernel.uid_pids() else {
        return Vec::new();
    };
    let mut hops: Vec<Hop> = pids
        .into_iter()
        .filter(|&pid| pid != root.pid)
        .filter_map(|pid| {
            *reads += 1;
            kernel.hop(pid)
        })
        .filter(|hop| hop.uniqueid > root.uniqueid)
        .collect();
    hops.sort_unstable_by_key(|hop| hop.uniqueid);
    let mut known: BTreeSet<u64> = std::iter::once(root.uniqueid)
        .chain(booked.iter().map(|kin| kin.uniqueid))
        .collect();
    let mut found = Vec::new();
    for hop in hops {
        let kin = Kin {
            pid: hop.identity.pid,
            birth: hop.identity.birth,
            uniqueid: hop.uniqueid,
        };
        if booked.contains(&kin) {
            if matches!(
                program_of(kernel, &kin, spared, reads),
                Program::Spared | Program::Unread
            ) {
                known.remove(&kin.uniqueid);
            }
            continue;
        }
        if !known.contains(&hop.parent_uniqueid)
            || program_of(kernel, &kin, spared, reads) != Program::Other
        {
            continue;
        }
        known.insert(hop.uniqueid);
        if hop.pgid != root_pgid {
            found.push(kin);
        }
    }
    found
}

/// Whether no living parent of a member of the session `leader` made, `step` that member's
/// step, reads as a spared program: its parents are climbed one by one, each linked to the step
/// below as [`bind`] links one, and each must read as no spared program ([`program_of`]:
/// [`Program::Other`]). The climb holds at the leader, at launchd or init (a parent ≤ 1), and
/// where the chain of living parents ends: at a parent that does not read and that the kernel
/// reads as gone or exited ([`Kernel::presence`]), or whose pid names a process the step below
/// does not name as its parent (it exited and its pid was taken since). It fails at a parent
/// whose program reads otherwise (spared, held, unread, or gone while it was read), at one that
/// does not read and is not read as gone (another user's), and past [`MAX_HOPS`]. So no member
/// holds that a living spared program below the leader started, itself or through living
/// processes; one whose parent has exited does, whatever that parent ran.
///
/// `climbed` keeps, for the leader's other members in the same [`group_kin`] call, the verdict
/// the climb reached at each process it passed (the member, and each parent it went on from):
/// a later climb whose step names a kept process (its step read, and linked as above) ends
/// there as climbing on would end over the same reads, without reading that process's program
/// again: a process kept as held `h` hops above it holds where the hops climbed to it and `h`
/// are within [`MAX_HOPS`] in all, and one kept as refused fails. A climb that passes
/// [`MAX_HOPS`] keeps nothing. So climbs that each end within [`MAX_HOPS`] read the program of
/// each parent they share once. [`group_kin`] reads every member's lineage before it climbs from
/// any, so each verdict a climb keeps was read after every member it serves, as a climb that
/// keeps nothing reads each parent's program after its member: a process a parent started after
/// its program was read was not born when the members were read, and is not given through that
/// parent's kept verdict. `reads` counts the reads made.
fn spares_no_living_parent(
    kernel: &(impl Kernel + ?Sized),
    step: Step,
    leader: &Kin,
    spared: &[PathBuf],
    link: Link,
    climbed: &mut BTreeMap<Kin, Climbed>,
    reads: &mut usize,
) -> bool {
    // The member, then each parent read as no spared program, in the order climbed.
    let mut path = vec![step.kin];
    let mut at = step;
    let end = loop {
        // The hop now climbed: from the last process of `path` to its parent.
        let hop = path.len();
        if hop > MAX_HOPS {
            return false;
        }
        let Ok(parent) = i32::try_from(at.parent) else {
            break Climbed::Refused;
        };
        if parent <= 1 || parent == leader.pid {
            break Climbed::Held(hop);
        }
        *reads += 1;
        let Some(up) = Step::read(kernel, parent, link) else {
            *reads += 1;
            break match kernel.presence(parent) {
                ProcessPresence::Absent | ProcessPresence::Exited => Climbed::Held(hop),
                ProcessPresence::Present | ProcessPresence::Unknown => Climbed::Refused,
            };
        };
        if !at.names(&up.kin, link) {
            break Climbed::Held(hop);
        }
        match climbed.get(&up.kin) {
            Some(&Climbed::Held(above)) => break Climbed::Held(hop + above),
            Some(&Climbed::Refused) => break Climbed::Refused,
            None => {}
        }
        if program_of(kernel, &up.kin, spared, reads) != Program::Other {
            climbed.insert(up.kin, Climbed::Refused);
            break Climbed::Refused;
        }
        path.push(up.kin);
        at = up;
    };
    match end {
        Climbed::Held(hops) if hops <= MAX_HOPS => {
            for (passed, kin) in path.into_iter().enumerate() {
                climbed.insert(kin, Climbed::Held(hops - passed));
            }
            true
        }
        Climbed::Held(_) => false,
        Climbed::Refused => {
            climbed.extend(path.into_iter().map(|kin| (kin, Climbed::Refused)));
            false
        }
    }
}

/// What a climb ([`spares_no_living_parent`]) reached from a process it passed, kept for the
/// other members of the same leader's session in one [`group_kin`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Climbed {
    /// It held this many hops above the process.
    Held(usize),
    /// It failed short of [`MAX_HOPS`]: at a parent whose program reads otherwise, at one that
    /// does not read and is not read as gone, or at a parent past `i32::MAX`.
    Refused,
}

/// The members of each group a process of `booked` leads that are proven its descendants,
/// listed while that leader reads as booked and as no spared program ([`program_of`]): each
/// born at or after it, reading as no spared program, and not in `booked`. A member of a
/// session the leader made with `setsid` (the leader and the member both read the leader's pid
/// as their session, [`Kernel::session`]) is given where its step reads as the member, no
/// living parent of it up to the leader reads as a spared program
/// ([`spares_no_living_parent`]), and the leader still reads as booked and as no spared program
/// once its members are read ([`program_of`] again), so the session they read was the one it
/// made, every process in that session descends from it, and a leader that ran a spared program
/// while they were read gives none of them; they are never tried by [`bind`], so where the
/// leader's last read fails none of them is given. The climbs of one leader's members share
/// what each reached ([`Climbed`]): a member an earlier climb passed is given or refused as that
/// climb reached it, with no read of its program, its step or its parents. Every member's
/// lineage, and where the leader made a session the session of each member that lineage keeps
/// (its group the leader's, its unique id not below the leader's, not booked), is read before
/// any member is tried, so each verdict a climb keeps was read after every member it serves:
/// a process started at a listed pid after a parent's program was read is not the member read
/// there, and is not given through that parent's kept verdict. Any other member is given where
/// it is bound to a process of `booked` by its chain of parents read now ([`bind`] with no
/// root: the binary is reaped by then, so its pid names nothing). A leader that made only a group
/// (`process_group(0)`) shares the session verify's launcher runs in, whose processes may join
/// that group by their own `setpgid`: such a joiner is proven by neither, and is not given. On
/// Apple a member whose chain passes through a kin that has exited is [`adopted_kin`]'s, by the
/// unique ids of that chain. `spared` as [`bind`] reads it; `reads` counts the reads made.
pub fn group_kin(
    kernel: &(impl Kernel + ?Sized),
    booked: &BTreeSet<Kin>,
    spared: &[PathBuf],
    link: Link,
    reads: &mut usize,
) -> Vec<Kin> {
    let mut found: Vec<Kin> = Vec::new();
    for leader in booked {
        let Ok(pgid) = u32::try_from(leader.pid) else {
            continue;
        };
        *reads += 1;
        if !kernel
            .lineage(leader.pid)
            .is_some_and(|l| leader.is(&l) && l.pgid == pgid)
            || program_of(kernel, leader, spared, reads) != Program::Other
        {
            continue;
        }
        *reads += 2;
        let made = kernel.session(leader.pid) == Some(pgid);
        let Some(members) = kernel.members(pgid) else {
            continue;
        };
        // Every member's lineage (and, where the leader made a session, the session of each member
        // its lineage keeps) is read before any member is tried, so each verdict a climb keeps is
        // read after every member it serves.
        let mut listed: Vec<(i32, Kin, bool)> = Vec::new();
        for member in members.pids.into_iter().filter(|&m| m != leader.pid) {
            *reads += 1;
            let Some(lin) = kernel.lineage(member) else {
                continue;
            };
            let kin = Kin::of(&lin);
            if lin.pgid != pgid || lin.uniqueid < leader.uniqueid || booked.contains(&kin) {
                continue;
            }
            let in_made = made && {
                *reads += 1;
                kernel.session(member) == Some(pgid)
            };
            listed.push((member, kin, in_made));
        }
        let mut in_session: Vec<Kin> = Vec::new();
        let mut climbed: BTreeMap<Kin, Climbed> = BTreeMap::new();
        for (member, kin, in_made) in listed {
            if found.contains(&kin) || in_session.contains(&kin) {
                continue;
            }
            if in_made {
                let given = match climbed.get(&kin) {
                    Some(Climbed::Held(_)) => true,
                    Some(Climbed::Refused) => false,
                    None if program_of(kernel, &kin, spared, reads) != Program::Other => {
                        climbed.insert(kin, Climbed::Refused);
                        false
                    }
                    None => {
                        *reads += 1;
                        Step::read(kernel, member, link)
                            .filter(|step| step.kin == kin)
                            .is_some_and(|step| {
                                spares_no_living_parent(
                                    kernel,
                                    step,
                                    leader,
                                    spared,
                                    link,
                                    &mut climbed,
                                    reads,
                                )
                            })
                    }
                };
                if given {
                    in_session.push(kin);
                }
            } else if bind(kernel, member, None, booked, spared, link, reads) == Some(kin) {
                found.push(kin);
            }
        }
        if !in_session.is_empty() && program_of(kernel, leader, spared, reads) == Program::Other {
            found.extend(in_session);
        }
    }
    found
}

/// What a test binary's end makes of the kin booked of it (`book`): `book`, what
/// [`adopted_kin`] adds where the kernel names a parent by its unique id ([`Link::UniqueId`],
/// Apple; Linux keeps no record that ties an orphan to the binary), and what [`group_kin`] adds
/// over both. `root` is the binary and `root_pgid` the group it led; `spared` as [`bind`] reads
/// it; `reads` counts the reads made.
pub fn kin_at_end(
    kernel: &(impl Kernel + ?Sized),
    root: Kin,
    root_pgid: u32,
    book: &BTreeSet<Kin>,
    spared: &[PathBuf],
    link: Link,
    reads: &mut usize,
) -> BTreeSet<Kin> {
    let mut kin = book.clone();
    if link == Link::UniqueId {
        let adopted = adopted_kin(kernel, root, root_pgid, &kin, spared, reads);
        kin.extend(adopted);
    }
    let grouped = group_kin(kernel, &kin, spared, link, reads);
    kin.extend(grouped);
    kin
}

/// What W-kin made of one booked kin ([`Judge::judge`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KinRead {
    /// Proven the run's to end.
    Proven,
    /// A program W-kin never proves: under a spared directory, an aterm image, a `trustd`, or a
    /// verb launchd keeps by design.
    Spared,
    /// Not proven now: held, unread, not orphaned, claimed, or on a terminal another holds.
    Unproven,
}

/// What W-kin reads beside a run's judgment ([`Judge::kin`]).
#[derive(Clone, Copy, Debug)]
pub struct KinJudge<'a> {
    /// The kin the run booked.
    pub booked: &'a BTreeSet<Kin>,
    /// The directories whose programs W-kin never proves, never books and books nothing through
    /// ([`bind`]): the run's locked toolchain, so never its `trustd`.
    pub spared: &'a [PathBuf],
    /// The run's own terminal, its launcher's session's, which each stage and the kin it left in
    /// that session inherit: A7 holds for a kin on it.
    pub tty: Option<u32>,
}

/// One run whose members a pass judges.
pub struct Judge<'a, K: ?Sized> {
    /// The kernel the judgment reads.
    pub kernel: &'a K,
    /// The run's roots (C-tree).
    pub roots: &'a [PathBuf],
    /// The run's id, which W-run reads in a member's `ATERM_VERIFY_RUN`.
    pub id: Option<String>,
    /// The run's launcher's unique id: W-run's window opens there, and where no launcher
    /// version is kept (Linux) a parent born before it adopted the member ([`Lineage::orphaned`]).
    pub run_uniqueid: u64,
    /// The open groups whose leader check passed, each with its leader's unique id, where its
    /// W-group window opens.
    pub groups: Vec<(u32, u64)>,
    /// Where both windows close: `dead_seen`, or no bound for a run that is alive.
    pub horizon: u64,
    /// launchd's pid version.
    pub launchd: Option<i32>,
    /// What the live runs claim.
    pub claims: &'a Claims,
    /// The run's booked kin (W-kin), where it keeps any.
    pub kin: Option<KinJudge<'a>>,
}

impl<K: Kernel + ?Sized> Judge<'_, K> {
    /// Whether `pid` is the run's, and whether ending it is proven, on fresh reads: A2 W-group or
    /// W-run, A3 C-tree, A4 orphaned, A5 its thread ran, A6 no live record claims it, A7 its
    /// tty's master is free. A1 (the launcher reads dead) is the caller's, which judges a dead
    /// record's members, or its own run's at its end.
    ///
    /// W-kin: a process that reads as one the run booked ([`KinJudge::booked`]) is proven, with
    /// neither C-tree nor a window, by A4, A5, A6 and A7 (which also holds on the run's own
    /// terminal, [`KinJudge::tty`]), its verb read and naming no program launchd keeps by design
    /// ([`Kernel::by_design`]), and its executable read, under no [`KinJudge::spared`] directory,
    /// no aterm image ([`crate::left::is_aterm_image`]: one the run built is still judged by
    /// W-run and C-tree) and no `trustd` (a name, which here only spares). A kin spared so (by
    /// its executable or its verb) is judged as any other process; any other kin W-kin does not
    /// prove is judged so too, and named where that finds it not the run's.
    pub fn judge(&self, pid: i32) -> Verdict {
        let Some(lin) = self.kernel.lineage(pid) else {
            return Verdict::NotOurs;
        };
        match self.kin_read(pid, &lin) {
            Some(KinRead::Proven) => Verdict::Proven,
            None | Some(KinRead::Spared) => self.judged(pid, &lin),
            Some(KinRead::Unproven) => match self.judged(pid, &lin) {
                Verdict::NotOurs => Verdict::Named,
                verdict => verdict,
            },
        }
    }

    /// Whether W-kin proves `pid` ([`Self::judge`]): what a pass names as the witness of an end.
    pub fn proves_kin(&self, pid: i32) -> bool {
        self.kernel
            .lineage(pid)
            .is_some_and(|lin| self.kin_read(pid, &lin) == Some(KinRead::Proven))
    }

    /// W-kin's reading of `pid`, which `lin` reads as: `None` where it is no booked kin. Its
    /// thread is read first, so no executable, argument or environment of a held program is.
    fn kin_read(&self, pid: i32, lin: &Lineage) -> Option<KinRead> {
        let kin = self.kin.filter(|kin| kin.booked.contains(&Kin::of(lin)))?;
        match program(self.kernel, pid, kin.spared, &mut 0) {
            Program::Unread | Program::Gone => return Some(KinRead::Unproven),
            Program::Spared => return Some(KinRead::Spared),
            Program::Other => {}
        }
        let markers = self.kernel.markers(pid);
        let proven = lin.orphaned(self.launchd, self.run_uniqueid, || {
            self.parent_uniqueid(lin)
        }) && !self.claims.claims(lin, markers.as_ref())
            && lin.tty.is_none_or(|dev| {
                kin.tty == Some(dev) || self.kernel.tty_master_held(dev) == Some(false)
            });
        Some(if proven {
            KinRead::Proven
        } else {
            KinRead::Unproven
        })
    }

    fn parent_uniqueid(&self, lin: &Lineage) -> Option<u64> {
        i32::try_from(lin.parent)
            .ok()
            .and_then(|parent| self.kernel.lineage(parent))
            .map(|parent| parent.uniqueid)
    }

    /// A2–A7 for `pid`, which `lin` reads as.
    fn judged(&self, pid: i32, lin: &Lineage) -> Verdict {
        let lin = *lin;
        let born_in = |from: u64| (from..=self.horizon).contains(&lin.uniqueid);
        let w_group = self
            .groups
            .iter()
            .any(|&(pgid, from)| pgid == lin.pgid && born_in(from));
        let in_run_window = born_in(self.run_uniqueid);
        if !w_group && !in_run_window {
            return Verdict::NotOurs;
        }
        let under_roots = || {
            self.kernel
                .exe(pid)
                .is_some_and(|exe| under_any(&exe, self.roots))
        };
        if self.kernel.held(pid) != Some(false) {
            return if w_group {
                Verdict::Named
            } else if under_roots() {
                Verdict::Unjudged
            } else {
                Verdict::NotOurs
            };
        }
        let markers = self.kernel.markers(pid);
        let w_run = in_run_window
            && self.id.as_deref().is_some_and(|id| {
                markers.as_ref().and_then(|m| m.verify_run.as_deref()) == Some(id)
            });
        if !w_group && !w_run {
            return Verdict::NotOurs;
        }
        // Without a launcher version (Linux), a parent older than the run adopted it.
        let proven = under_roots()
            && lin.orphaned(self.launchd, self.run_uniqueid, || {
                self.parent_uniqueid(&lin)
            })
            && !self.claims.claims(&lin, markers.as_ref())
            && lin
                .tty
                .is_none_or(|dev| self.kernel.tty_master_held(dev) == Some(false));
        if proven {
            Verdict::Proven
        } else {
            Verdict::Named
        }
    }
}

/// Live pids of this uid (excluding `me` and its ancestors), oldest first, and the high-water
/// unique id. No process is older than its parent, so a pass judges a parent before its
/// children: the end of an orphaned stage leader orphans its children before they are judged.
pub fn table(kernel: &(impl Kernel + ?Sized), me: i32) -> Option<(Vec<i32>, u64)> {
    let mine = kernel.lineage(me)?;
    let mut spared = vec![me];
    let mut up = mine.parent;
    while let Ok(parent) = i32::try_from(up)
        && parent > 1
        && spared.len() < 64
    {
        spared.push(parent);
        match kernel.lineage(parent) {
            Some(l) => up = l.parent,
            None => break,
        }
    }
    let mut high = mine.uniqueid;
    let mut rows = Vec::new();
    for pid in kernel.uid_pids()? {
        if spared.contains(&pid) {
            continue;
        }
        if let Some(l) = kernel.lineage(pid) {
            high = high.max(l.uniqueid);
            rows.push((l.uniqueid, pid));
        }
    }
    rows.sort_unstable();
    Some((rows.into_iter().map(|(_, pid)| pid).collect(), high))
}

/// Bound the text read even if a record grows after its size was checked.
const RECORD_BYTES: u64 = 4 * 1024 * 1024;

// A full kin sidecar, with room for its header and its spared lines, reads within the bound.
const _: () = assert!((KIN_LINES_MAX * KIN_LINE_BYTES) as u64 + 64 * 1024 <= RECORD_BYTES);

fn read_record_text(path: &Path) -> std::io::Result<String> {
    read_bounded_regular_text_with(path, RECORD_BYTES, true)
}

/// The text of the regular file at `path`, at most `limit` bytes: a final link or reparse point,
/// a FIFO, a device, a file over the bound or text that is not UTF-8 is refused. Unix opens are
/// nonblocking; ancestor directories may be links.
///
/// # Errors
/// The open's or the read's, or [`std::io::ErrorKind::InvalidData`] for what is refused.
pub fn read_bounded_regular_text(path: &Path, limit: u64) -> std::io::Result<String> {
    read_bounded_regular_text_with(path, limit, false)
}

fn read_bounded_regular_text_with(
    path: &Path,
    limit: u64,
    coherent_record: bool,
) -> std::io::Result<String> {
    let (file, _) = crate::regular_file::open(path)?;
    let _held = if coherent_record {
        Some(RecordLock::read(&file)?)
    } else {
        None
    };
    read_open_text(&file, limit)
}

/// Read the admitted handle while the caller retains any required record lock.
fn read_open_text(file: &std::fs::File, limit: u64) -> std::io::Result<String> {
    use std::io::{Read as _, Seek as _};
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a bounded regular run record",
        )
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid());
    }
    let mut input = file;
    input.seek(std::io::SeekFrom::Start(0))?;
    let cap = usize::try_from(metadata.len().min(limit).saturating_add(1)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(cap);
    input
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid());
    }
    String::from_utf8(bytes)
        .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidData, why))
}

/// The record at `path` opened to read and append: created new (mode 0600) or, with
/// `append_existing`, retained only while it is a regular file. Never through a link, never
/// blocking, and kept out of every child. The retained offset cannot overwrite another writer.
///
/// # Errors
/// The open's (including `AlreadyExists` rather than truncating a prior record), or
/// [`std::io::ErrorKind::InvalidData`] for what is not a regular file.
pub fn open_regular_write(path: &Path, append_existing: bool) -> std::io::Result<std::fs::File> {
    let invalid =
        || std::io::Error::new(std::io::ErrorKind::InvalidData, "not a regular run record");
    if append_existing && !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid());
    }
    let mut open = std::fs::OpenOptions::new();
    if append_existing {
        open.append(true);
    } else {
        open.create_new(true).append(true);
    }
    // Reading is needed for locked read-or-append transactions, and lets Windows lock the file.
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        if !append_existing {
            open.mode(0o600);
        }
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        open.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = open.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid());
        }
    }
    let _ = crate::keep_out_of_children(&file);
    Ok(file)
}

/// The writer's ordered v1 fields, each present once. Unknown text conveys no cleanup authority.
fn cleanup_fields<'a>(line: &'a str, keys: &[&str]) -> Option<Vec<&'a str>> {
    let mut words = line.split_whitespace().skip(1);
    let values = keys
        .iter()
        .map(|expected| {
            let (key, value) = words.next()?.split_once('=')?;
            (key == *expected).then_some(value)
        })
        .collect::<Option<Vec<_>>>()?;
    words.next().is_none().then_some(values)
}

fn cleanup_number(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()) && value.parse::<u64>().is_ok()
}

/// Unlike the public projection's decoder, a malformed escape or UTF-8 is unknown.
fn cleanup_decoded(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.bytes();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let hi = char::from(chars.next()?).to_digit(16)?;
            let lo = char::from(chars.next()?).to_digit(16)?;
            bytes.push((hi * 16 + lo) as u8);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

/// The boot a record's first line names, where that line is a supported header of the record
/// `path` is named for.
fn cleanup_header<'line>(path: &Path, line: &'line str) -> Option<&'line str> {
    if line.split_whitespace().next()? != "run" {
        return None;
    }
    let fields = cleanup_fields(line, &["v", "boot", "id", "uniqueid", "start", "sid"])?;
    let [version, boot, id, uniqueid, start, sid] = fields.as_slice() else {
        return None;
    };
    let parsed = RunId::parse(id)?;
    if *version != "1"
        || boot.is_empty()
        || parsed.pid <= 1
        || parsed.birth.0 == 0
        || parsed.text() != *id
        || path.file_name()?.to_str()? != format!("{}.run", parsed.text())
        || !cleanup_number(uniqueid)
        || !cleanup_number(start)
    {
        return None;
    }
    cleanup_decoded(sid)?;
    Some(*boot)
}

/// A record that does not read complete, as its lenient projection, where its header line is
/// whole and names another boot than `boot`. Its launcher and every group it names ended with
/// that boot, so it makes no claim on this one, and no writer is left to complete it: a
/// partial write (a full disk) or a kill between two writes of an earlier release leaves one.
fn earlier_boot_record(path: &Path, text: &str, boot: Option<&str>) -> Option<Record> {
    let (header, _) = text.split_once('\n')?;
    (cleanup_header(path, header)? != boot?).then(|| Record::parse(text))
}

/// Validate the cleanup protocol without changing [`Record::parse`]'s lenient projection.
fn cleanup_record(path: &Path, text: &str) -> Option<Record> {
    if text.is_empty() || !text.ends_with('\n') {
        return None;
    }
    for (index, line) in text.lines().enumerate() {
        match line.split_whitespace().next()? {
            "run" if index == 0 => {
                cleanup_header(path, line)?;
            }
            "root" if index > 0 => {
                let fields = cleanup_fields(line, &["path"])?;
                if cleanup_decoded(fields.first()?)?.is_empty() {
                    return None;
                }
            }
            "group" if index > 0 => {
                let fields =
                    cleanup_fields(line, &["pgid", "leader_birth", "leader_uniqueid", "stage"])?;
                let [pgid, birth, uniqueid, stage] = fields.as_slice() else {
                    return None;
                };
                if pgid.parse::<u32>().ok()? <= 1
                    || !cleanup_number(pgid)
                    || !cleanup_number(birth)
                    || !cleanup_number(uniqueid)
                {
                    return None;
                }
                cleanup_decoded(stage)?;
            }
            "closed" if index > 0 => {
                let fields = cleanup_fields(line, &["pgid"])?;
                let pgid = fields.first()?;
                if pgid.parse::<u32>().ok()? <= 1 || !cleanup_number(pgid) {
                    return None;
                }
            }
            "dead_seen" if index > 0 => {
                let fields = cleanup_fields(line, &["uniqueid", "at"])?;
                if !fields.iter().all(|value| cleanup_number(value)) {
                    return None;
                }
            }
            "end" if index > 0 => {
                let fields = cleanup_fields(line, &["at"])?;
                if !cleanup_number(fields.first()?) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(Record::parse(text))
}

/// Decode a complete raw archive as diagnostic data, never as native ownership or signal
/// authority. The strict current record protocol, one terminal `end`, and every observed
/// nonzero group generation's own `closed` line must be present. A zero-group run may finish.
#[must_use]
pub fn completed_archive(text: &str) -> Option<Record> {
    if text.len() as u64 > RECORD_BYTES {
        return None;
    }
    let header = text.lines().next()?;
    let fields = cleanup_fields(header, &["v", "boot", "id", "uniqueid", "start", "sid"])?;
    let id = RunId::parse(fields.get(2)?)?;
    let name = format!("{}.run", id.text());
    let record = cleanup_record(Path::new(&name), text)?;
    let mut latest = std::collections::BTreeMap::<u32, bool>::new();
    let mut ended = false;
    for line in text.lines() {
        if ended {
            return None;
        }
        match line.split_whitespace().next()? {
            "group" => {
                let fields =
                    cleanup_fields(line, &["pgid", "leader_birth", "leader_uniqueid", "stage"])?;
                latest.insert(fields.first()?.parse().ok()?, false);
            }
            "closed" => {
                let fields = cleanup_fields(line, &["pgid"])?;
                let generation = latest.get_mut(&fields.first()?.parse::<u32>().ok()?)?;
                if *generation {
                    return None;
                }
                *generation = true;
            }
            "end" => ended = true,
            _ => {}
        }
    }
    (ended
        && record
            .groups
            .iter()
            .all(|group| group.closed && group.leader_birth.0 != 0 && group.leader_uniqueid != 0))
    .then_some(record)
}

/// A complete admitted snapshot of the records in `dir`, or unknown: one record that does not
/// read complete refuses the whole snapshot, never a partial set of competing claims. A
/// directory not made yet holds none.
///
/// `boot` is the reader's own boot, where it knows it. A record of another boot competes for
/// nothing on this one, so one that is cut short is still admitted, as its lenient projection,
/// where its header line is whole ([`earlier_boot_record`]): it names its boot, which every
/// reader judges a record by, and the reader that removes earlier boots' records removes it
/// too. Without that, one record torn before a restart refused every later reading for good.
/// A record of this boot that does not read complete refuses as before, as does every
/// incomplete record for a reader that knows no boot.
///
/// # Errors
/// The directory's read, a record's, or [`std::io::ErrorKind::InvalidData`] for a record that is
/// not a complete supported one and is not shown to be another boot's. A record being written
/// returns [`std::io::ErrorKind::WouldBlock`].
pub fn read_records(dir: &Path, boot: Option<&str>) -> std::io::Result<Vec<(PathBuf, Record)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(why) => return Err(why),
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("run") {
            continue;
        }
        let text = read_record_text(&path)?;
        let rec = cleanup_record(&path, &text)
            .or_else(|| earlier_boot_record(&path, &text, boot))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{} is not a complete supported run record", path.display()),
                )
            })?;
        records.push((path, rec));
    }
    Ok(records)
}

/// Append one complete line to an existing run record.
///
/// # Errors
/// The open's or [`write_line`]'s error.
pub fn append(path: &Path, line: &str) -> std::io::Result<()> {
    let file = open_regular_write(path, true)?;
    write_line(&file, line)
}

/// Hang detector for local lock contention on a complete record write, not its file I/O.
const RECORD_WRITE_HANG: Duration = Duration::from_secs(60);

fn record_write_deadline_at(started: Instant) -> Instant {
    started + RECORD_WRITE_HANG
}

/// A file's shared read or exclusive write scope, explicitly unlocked before the file closes.
struct RecordLock<'file>(&'file std::fs::File);

impl<'file> RecordLock<'file> {
    fn read(file: &'file std::fs::File) -> std::io::Result<Self> {
        crate::keep_out_of_children(file)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "RecordLock explicitly unlocks the admitted file in Drop"
        )]
        match file.try_lock_shared() {
            Ok(()) => Ok(Self(file)),
            Err(std::fs::TryLockError::WouldBlock) => Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "a run-record writer still holds the record",
            )),
            Err(std::fs::TryLockError::Error(why)) => Err(why),
        }
    }

    fn write(file: &'file std::fs::File, deadline: Instant) -> std::io::Result<Self> {
        crate::file_lock::lock_exclusive_until(
            file,
            deadline,
            "a run-record reader or writer still holds the record",
        )?;
        Ok(Self(file))
    }
}

impl Drop for RecordLock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Write a complete run-record block through a file returned by [`open_regular_write`].
/// Calls sharing that same descriptor must retain their process-local serialization too:
/// the advisory lock serializes independently opened descriptors, not threads sharing one.
/// The file lock spans all short writes and is explicitly released on errors and unwinding.
///
/// # Errors
/// The lock's I/O error, `WouldBlock` after a minute of contention, or the write's error.
pub fn write_record_text(file: &std::fs::File, text: &str) -> std::io::Result<()> {
    write_record_with(
        file,
        text.as_bytes(),
        record_write_deadline_at(Instant::now()),
        |mut file, bytes| file.write_all(bytes),
    )
}

/// Write one run-record line, including its newline, through a retained append writer.
/// The same descriptor serialization as [`write_record_text`] is required.
///
/// # Errors
/// [`write_record_text`]'s errors.
pub fn write_line(file: &std::fs::File, line: &str) -> std::io::Result<()> {
    write_line_with(file, line, |mut file, bytes| file.write_all(bytes))
}

fn write_line_with(
    file: &std::fs::File,
    line: &str,
    mut write: impl FnMut(&std::fs::File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    write_record_with(
        file,
        &bytes,
        record_write_deadline_at(Instant::now()),
        &mut write,
    )
}

fn write_record_with(
    file: &std::fs::File,
    bytes: &[u8],
    deadline: Instant,
    mut write: impl FnMut(&std::fs::File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let _held = RecordLock::write(file, deadline)?;
    write(file, bytes)
}

/// The first dead-launcher observation actually retained by a run record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeadSeen {
    /// The observed table's high-water unique id.
    pub horizon: u64,
    /// When the observation was made, unix milliseconds.
    pub at_ms: u64,
}

/// Retain the first `dead_seen` observation, or return the one another reader recorded first.
/// The admitted header and roots must still match `expected`; group and closed lines may grow.
/// A first observation already present in `expected` must remain unchanged.
/// Reading, choosing the first observation and appending share one exclusive file-lock scope.
/// A busy record returns immediately so a later observation can retry it.
///
/// # Errors
/// `WouldBlock` while the record is held, or the open, lock, read or write error; `InvalidData`
/// for a malformed record, moved header, changed or removed admitted first observation, or an
/// append exceeding the record's byte limit.
pub fn append_dead_seen(
    path: &Path,
    expected: &Record,
    horizon: u64,
    at_ms: u64,
) -> std::io::Result<DeadSeen> {
    let file = open_regular_write(path, true)?;
    let _held = RecordLock::write(&file, Instant::now())?;
    let text = read_open_text(&file, RECORD_BYTES)?;
    let record = cleanup_record(path, &text).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "incomplete run record")
    })?;
    if record.id != expected.id
        || record.boot != expected.boot
        || record.uniqueid != expected.uniqueid
        || record.start_ms != expected.start_ms
        || record.sid != expected.sid
        || record.roots != expected.roots
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the admitted run-record header changed",
        ));
    }
    if expected.dead_seen.is_some()
        && (record.dead_seen != expected.dead_seen || record.dead_seen_ms != expected.dead_seen_ms)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the admitted first dead-run observation changed",
        ));
    }
    if let Some(horizon) = record.dead_seen {
        let at_ms = record.dead_seen_ms.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing dead_seen time")
        })?;
        return Ok(DeadSeen { horizon, at_ms });
    }
    let line = format!("dead_seen uniqueid={horizon} at={at_ms}\n");
    if text
        .len()
        .checked_add(line.len())
        .is_none_or(|bytes| bytes as u64 > RECORD_BYTES)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the dead_seen observation exceeds the run-record limit",
        ));
    }
    let mut output = &file;
    output.write_all(line.as_bytes())?;
    Ok(DeadSeen { horizon, at_ms })
}

/// The directory under `rendezvous` that run records live in: `verify-runs`, and beneath it one
/// directory per pid namespace where there are namespaces (`pid-<inode>`, Linux). A record
/// names pids, which read only in the namespace that wrote them, and namespaces share a boot,
/// so a verify in a container that shares the rendezvous never judges the host's runs as dead
/// (nor the host a container's).
#[must_use]
pub fn records_dir(rendezvous: &Path, pid_namespace: Option<&str>) -> PathBuf {
    let dir = rendezvous.join("verify-runs");
    match pid_namespace {
        Some(namespace) => dir.join(format!("pid-{namespace}")),
        None => dir,
    }
}

/// `path` with every link resolved as far as it exists, and the rest appended as written: a
/// directory a run has yet to make is spelled as it will be once made. The one spelling of a
/// run's directories: verify's run records name their roots by it, and disk admission tags a
/// run's directories by it (`atpkg::admit_disk`).
#[must_use]
pub fn canonical(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return tail.iter().rev().fold(real, |acc, name| acc.join(name));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Whether `exe` sits under one of `roots`, each side [`canonical`] at the check.
#[must_use]
pub fn under_any(exe: &Path, roots: &[PathBuf]) -> bool {
    let exe = canonical(exe);
    roots.iter().any(|root| exe.starts_with(canonical(root)))
}

/// Executable basename when it matches `^[A-Za-z0-9._+-]{1,32}$`.
#[must_use]
pub fn exe_word(exe: &Path) -> Option<String> {
    let name = exe.file_name()?.to_str()?;
    (!name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')))
    .then(|| name.to_owned())
}

/// Percent-encode a record field so it survives as one word of one line.
#[must_use]
pub fn pct_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_graphic() && b != b'%' && b != b'=' {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The inverse of [`pct_encode`]; a malformed escape is kept as written.
#[must_use]
pub fn pct_decode(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(hi) = (bytes[i + 1] as char).to_digit(16)
            && let Some(lo) = (bytes[i + 2] as char).to_digit(16)
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod append_tests;

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "runs_completed_archive_tests.rs"]
mod completed_archive_tests;

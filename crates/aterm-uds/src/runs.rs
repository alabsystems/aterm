// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verify's run records read back: the record's format, the bounded read of a whole admitted
//! snapshot, the `dead_seen` append, the kernel reads a judgment takes ([`Kernel`],
//! [`LiveKernel`]), what the live runs claim ([`Claims`]) and the A1–A7 verdict on one process
//! ([`Judge::judge`]). aterm-verify writes the records and runs the passes that end what this
//! proves; the window's `sys left` and doctor read them here
//! (`docs/DESIGN-leftover-processes-2026-10-16.md` §3.3, §4.3).

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::process::{
    Birth, END_GRACE, EndControlLive, EndRefusal, Ended, GroupMembers, HeldNow, Identity, Lineage,
    Markers, ProcessPresence, end_proven, group_members, held_programs, launchd_idversion,
    process_presence, read_birth, read_lineage, read_process_markers, read_process_path,
    thread_ran,
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
}

impl<K: Kernel + ?Sized> Judge<'_, K> {
    /// Whether `pid` is the run's, and whether ending it is proven, on fresh reads: A2 W-group or
    /// W-run, A3 C-tree, A4 orphaned, A5 its thread ran, A6 no live record claims it, A7 its
    /// tty's master is free. A1 (the launcher reads dead) is the caller's, which judges a dead
    /// record's members, or its own run's at its end.
    pub fn judge(&self, pid: i32) -> Verdict {
        let Some(lin) = self.kernel.lineage(pid) else {
            return Verdict::NotOurs;
        };
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
        let parent_uniqueid = || {
            i32::try_from(lin.parent)
                .ok()
                .and_then(|parent| self.kernel.lineage(parent))
                .map(|parent| parent.uniqueid)
        };
        let proven = under_roots()
            && lin.orphaned(self.launchd, self.run_uniqueid, parent_uniqueid)
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

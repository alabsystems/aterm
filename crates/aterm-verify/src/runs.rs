// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verify run records, stage survivor checks, and dead-run process sweeps
//! ([`end_proven`](aterm_uds::process::end_proven), [`Kernel`](crate::runs::Kernel), [`LiveKernel`](crate::runs::LiveKernel)).

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use aterm_uds::process::{
    Birth, END_GRACE, EndControlLive, EndRefusal, Ended, GroupMembers, Identity, Lineage, Markers,
    ProcessPresence, boot_id, end_proven, group_members, launchd_idversion, process_presence,
    read_birth, read_lineage, read_process_markers, read_process_path, thread_ran,
};

use crate::Ctx;
use crate::findings::tally_words;

/// `ATERM_VERIFY_RUN` ([`aterm_uds::process::VERIFY_RUN_VAR`]).
pub const VERIFY_RUN_ENV: &str = aterm_uds::process::VERIFY_RUN_VAR;

/// The kernel reads the passes take, injected so the classification is a unit test over a fake.
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
    /// launchd's pid version; `None` when unreadable.
    fn launchd_version(&self) -> Option<i32>;
    /// Whether a live process of this uid holds the master of the PTY whose slave is `dev`.
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

/// The shipped [`Kernel`]: `aterm_uds::process` and `aterm_uds::holders`.
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
    // executable (`docs/measured/linux-rosetta-2026-10-17.md`), which is under no root either,
    // so there a translated member is named, never ended.
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        read_process_path(pid)
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        read_process_markers(pid)
    }
    fn held(&self, pid: i32) -> Option<bool> {
        thread_ran(pid).map(|ran| !ran)
    }
    fn launchd_version(&self) -> Option<i32> {
        launchd_idversion()
    }
    fn tty_master_held(&self, dev: u32) -> Option<bool> {
        let master = aterm_uds::holders::pty_master_rdev(u64::from(dev))?;
        let scan = aterm_uds::holders::scan_rdev_holders(master, &[]).ok()?;
        if !scan.holders.is_empty() {
            Some(true)
        } else {
            scan.unreadable.is_empty().then_some(false)
        }
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        let pids = aterm_uds::holders::uid_pids().ok()?;
        Some(
            pids.into_iter()
                .filter_map(|p| i32::try_from(p).ok())
                .collect(),
        )
    }
    fn now_ms(&self) -> u64 {
        aterm_time::unix_millis().unwrap_or(0)
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

    /// Parse `<pid>-<birth>`; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (pid, birth) = text.split_once('-')?;
        Some(Self {
            pid: pid.parse().ok()?,
            birth: Birth(birth.parse().ok()?),
        })
    }
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
    /// The stage's label (the child's argv), pct-encoded on the wire so a space survives.
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
    /// The uniqueid horizon the first reader that found the launcher dead wrote.
    pub dead_seen: Option<u64>,
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
                            "uniqueid" => rec.uniqueid = v.parse().unwrap_or(0),
                            "start" => rec.start_ms = v.parse().unwrap_or(0),
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
                            "pgid" => g.pgid = v.parse().unwrap_or(0),
                            "leader_birth" => g.leader_birth = Birth(v.parse().unwrap_or(0)),
                            "leader_uniqueid" => g.leader_uniqueid = v.parse().unwrap_or(0),
                            "stage" => g.stage = pct_decode(v),
                            _ => {}
                        }
                    }
                    if g.pgid > 1 {
                        rec.groups.push(g);
                    }
                }
                Some("closed") => {
                    let pgid: Option<u32> =
                        kv_value(words.next(), "pgid").and_then(|v| v.parse().ok());
                    if let Some(group) = rec.groups.iter_mut().rev().find(|g| Some(g.pgid) == pgid)
                    {
                        group.closed = true;
                    }
                }
                Some("dead_seen") => {
                    rec.dead_seen = kv_value(words.next(), "uniqueid").and_then(|v| v.parse().ok());
                }
                _ => {}
            }
        }
        rec
    }

    /// The groups with no `closed` line: those that may still hold members.
    fn open_groups(&self) -> impl Iterator<Item = &GroupLine> {
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
fn header_text(
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

/// The run's open record and the facts its stage sweeps and its run-end sweep read.
#[derive(Debug)]
pub struct Runs {
    /// The record file, appended to from stage threads (each line is one `write`).
    log: Mutex<std::fs::File>,
    /// Where the record lives (removed at an orderly end).
    path: PathBuf,
    /// The boot the record names, which the run-end sweep reads the other records under.
    boot: String,
    /// This run's id (W-run) and unique id.
    id: RunId,
    uniqueid: u64,
    /// The canonical roots a survivor's exe must sit under to be ended, the worktree first.
    roots: Vec<PathBuf>,
    /// The lines the receipt carries: each stage's survivors, as their sweeps found them.
    said: Mutex<Vec<String>>,
    /// Only a fresh, admitted run-end sweep with no survivors permits removal.
    may_forget: AtomicBool,
}

/// One stage group's survivors: what verify ended and what it named and left.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupReport {
    /// The stage's label.
    pub stage: String,
    /// The exe words of the members verify ended by pid.
    pub ended: Vec<String>,
    /// The exe words of the members it named and left: not its to end, or not ended.
    pub named: Vec<String>,
    /// Whether the group was listed in full (`Some(true)`), in part (`Some(false)`), or not at all.
    pub listed: Option<bool>,
}

impl GroupReport {
    /// Proven empty: listed in full, and every survivor ended.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.listed == Some(true) && self.named.is_empty()
    }

    /// The receipt's line for the group.
    #[must_use]
    pub fn line(&self) -> Option<String> {
        let partial = self.listed == Some(false);
        (partial || !self.ended.is_empty() || !self.named.is_empty())
            .then(|| crate::findings::survivor_line(&self.stage, &self.ended, &self.named, partial))
    }
}

/// What a pass did with the processes it proved its run's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Left {
    /// Ended by pid.
    pub ended: Vec<String>,
    /// Named and left.
    pub named: Vec<String>,
    /// Born in the run's window, running from its roots, but held at the program check.
    pub unjudged: usize,
}

impl Left {
    const fn total(&self) -> usize {
        self.ended.len() + self.named.len()
    }

    /// Whether the pass leaves a member of the run it could not end, or could not judge.
    const fn leaves_any(&self) -> bool {
        !self.named.is_empty() || self.unjudged > 0
    }

    /// `ended aterm ×2; named ssh-agent ×1; 1 held at macOS's program check, not judged`.
    fn words(&self) -> String {
        let mut parts = Vec::new();
        if !self.ended.is_empty() {
            parts.push(format!("ended {}", tally_words(&self.ended)));
        }
        if !self.named.is_empty() {
            parts.push(format!("named {}", tally_words(&self.named)));
        }
        if self.unjudged > 0 {
            parts.push(format!(
                "{} held at macOS's program check, not judged",
                self.unjudged
            ));
        }
        parts.join("; ")
    }

    /// `<head> left 3 processes: <words>`, or `<head>: <words>` when it ended and named nothing.
    fn line(&self, head: &str) -> Option<String> {
        if self.total() == 0 && self.unjudged == 0 {
            None
        } else if self.total() == 0 {
            Some(format!("{head}: {}", self.words()))
        } else {
            Some(format!(
                "{head} left {} processes: {}",
                self.total(),
                self.words()
            ))
        }
    }
}

/// What one judgment of a process found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
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

/// What became of one process a pass set out to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// Signalled until gone, or SIGKILLed.
    Ended,
    /// Gone before any signal: it holds nothing.
    Gone,
    /// Still there and not ended: named.
    Left,
}

/// A failed birth read preserves the process unless a separate query confirms it holds
/// nothing: no process has the pid, or the one that has it exited and waits to be reaped.
fn may_match_birth(kernel: &impl Kernel, pid: i32, matches: impl FnOnce(Birth) -> bool) -> bool {
    match kernel.birth(pid) {
        Some(birth) => matches(birth),
        None => !matches!(
            kernel.presence(pid),
            ProcessPresence::Absent | ProcessPresence::Exited
        ),
    }
}

/// Name unreadable survivors; only changed birth or confirmed absence establishes departure.
fn settle(
    kernel: &impl Kernel,
    pid: i32,
    end: impl FnOnce(Identity) -> Option<Result<Ended, EndRefusal>>,
) -> Fate {
    let Some(lin) = kernel.lineage(pid) else {
        return if may_match_birth(kernel, pid, |_| true) {
            Fate::Left
        } else {
            Fate::Gone
        };
    };
    let answer = end(lin.identity);
    if may_match_birth(kernel, pid, |birth| birth == lin.identity.birth) {
        Fate::Left
    } else if answer.is_some_and(|a| a.is_ok_and(|e| e.termed || e.killed)) {
        Fate::Ended
    } else {
        Fate::Gone
    }
}

/// `pid`'s exe word, or `?`.
fn word_of(kernel: &impl Kernel, pid: i32) -> String {
    kernel
        .exe(pid)
        .as_deref()
        .and_then(exe_word)
        .unwrap_or_else(|| "?".to_owned())
}

impl Runs {
    /// Open the record for this run in `dir`.
    #[must_use]
    pub fn open(
        ctx: &Ctx,
        dir: &Path,
        ask: impl FnOnce(&crate::exec::Cmd, crate::exec::ExecEnv<'_>) -> crate::exec::Run,
    ) -> Option<Self> {
        let boot = boot_id()?;
        let pid = i32::try_from(std::process::id()).ok()?;
        let id = RunId {
            pid,
            birth: read_birth(pid)?,
        };
        let uniqueid = read_lineage(pid).map_or(0, |l| l.uniqueid);
        let roots = run_roots(
            &ctx.root,
            &ctx.scratch,
            &ctx.target_dir,
            &build_dirs(ctx, ask),
        );
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(format!("{}.run", id.text()));
        let sid = std::env::var(aterm_uds::process::MARKER_VARS[0]).ok();
        let header = header_text(
            &boot,
            id,
            uniqueid,
            aterm_time::unix_millis().unwrap_or(0),
            sid.as_deref(),
            &roots,
        );
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .ok()?;
        file.write_all(header.as_bytes()).ok()?;
        Some(Self {
            log: Mutex::new(file),
            path,
            boot,
            id,
            uniqueid,
            roots,
            said: Mutex::new(Vec::new()),
            may_forget: AtomicBool::new(false),
        })
    }

    /// Append one line (a newline is added), best-effort.
    fn line(&self, line: &str) {
        if let Ok(mut file) = self.log.lock() {
            let _ = writeln!(file, "{line}");
        }
    }

    /// The `ATERM_VERIFY_RUN` value every stage child carries (W-run): this run's id.
    #[must_use]
    pub fn env_value(&self) -> String {
        self.id.text()
    }

    /// Keep what one stage group's sweep found.
    fn settle_group(&self, pgid: u32, report: &GroupReport) {
        if report.closed() {
            self.line(&format!("closed pgid={pgid}"));
        }
        if let Some(line) = report.line()
            && let Ok(mut said) = self.said.lock()
        {
            said.push(line);
        }
    }

    /// Sweep remaining run processes and return survivor report lines.
    pub fn end_of_run(&self, kernel: &impl Kernel) -> Vec<String> {
        self.may_forget.store(false, Ordering::Relaxed);
        let line = match self.run_end_sweep(kernel) {
            Some(left) => {
                self.may_forget.store(!left.leaves_any(), Ordering::Relaxed);
                left.line("the run")
            }
            None => Some(
                "could not admit the run records or list this user's processes at the run's end; \
                 this run's record is kept"
                    .to_owned(),
            ),
        };
        let mut said = self.said.lock().map(|s| s.clone()).unwrap_or_default();
        said.extend(line);
        said
    }

    /// The W-run sweep.
    fn run_end_sweep(&self, kernel: &impl Kernel) -> Option<Left> {
        let records = read_records(self.path.parent()?).ok()?;
        let claims = Claims::of(
            records
                .iter()
                .map(|(_, r)| r)
                .filter(|r| r.boot == self.boot && r.id != Some(self.id))
                .filter(|r| launcher_alive(r, kernel)),
        );
        let (pids, _) = table(kernel, self.id.pid)?;
        let judge = Judge {
            kernel,
            roots: &self.roots,
            id: Some(self.id.text()),
            run_uniqueid: self.uniqueid,
            groups: Vec::new(),
            horizon: u64::MAX,
            launchd: kernel.launchd_version(),
            claims: &claims,
        };
        Some(judge.pass(&pids))
    }

    /// Finish only after an admitted run-end sweep left no named or unjudged processes.
    pub fn finish(&self, kernel: &impl Kernel) {
        if !self.may_forget.load(Ordering::Relaxed) {
            return;
        }
        self.line(&format!("end at={}", kernel.now_ms()));
        let _ = std::fs::remove_file(&self.path);
    }
}

impl crate::exec::StageGroupEnd for Runs {
    fn group_made(&self, pgid: u32, label: &str) {
        let (birth, uniqueid) = read_lineage(i32::try_from(pgid).unwrap_or(0))
            .map(|l| (l.identity.birth.0, l.uniqueid))
            .unwrap_or((0, 0));
        self.line(&format!(
            "group pgid={pgid} leader_birth={birth} leader_uniqueid={uniqueid} stage={}",
            pct_encode(label)
        ));
    }

    fn group_ended(&self, pgid: u32, label: &str) {
        let report = sweep_group(&LiveKernel, pgid, label, &self.roots);
        self.settle_group(pgid, &report);
    }
}

/// Sweep one stage group whose leader has exited unreaped (`pgid` is the leader's pid).
pub fn sweep_group(kernel: &impl Kernel, pgid: u32, label: &str, roots: &[PathBuf]) -> GroupReport {
    let mut report = GroupReport {
        stage: label.to_owned(),
        ..GroupReport::default()
    };
    let leader = i32::try_from(pgid).unwrap_or(0);
    let Some(members) = kernel.members(pgid) else {
        return report;
    };
    report.listed = Some(members.complete);
    let proven = |pid: i32| {
        kernel.lineage(pid).is_some_and(|l| l.pgid == pgid)
            && kernel.exe(pid).is_some_and(|exe| under_any(&exe, roots))
            && kernel.held(pid) == Some(false)
    };
    for member in members.pids.into_iter().filter(|&m| m != leader) {
        let word = word_of(kernel, member);
        let fate = settle(kernel, member, |token| {
            proven(member).then(|| kernel.end(token, &|id| proven(id.pid)))
        });
        match fate {
            Fate::Ended => report.ended.push(word),
            Fate::Left => report.named.push(word),
            Fate::Gone => {}
        }
    }
    report
}

/// What the live runs claim: each run's launcher, its open stage groups, and its id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Claims {
    /// The launchers, `(pid, birth)`: a verify started inside another run's stage carries
    /// that run's id in its own environment, and is its own run's all the same.
    launchers: BTreeSet<(i32, Birth)>,
    groups: BTreeSet<u32>,
    ids: BTreeSet<String>,
}

impl Claims {
    fn of<'a>(live: impl Iterator<Item = &'a Record>) -> Self {
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

    /// Whether a live run claims `lin` with `markers`.
    fn claims(&self, lin: &Lineage, markers: Option<&Markers>) -> bool {
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

/// One run whose members a pass judges.
struct Judge<'a, K> {
    kernel: &'a K,
    /// The run's roots (C-tree).
    roots: &'a [PathBuf],
    /// The run's id, which W-run reads in a member's `ATERM_VERIFY_RUN`.
    id: Option<String>,
    /// The run's launcher's unique id: W-run's window opens there, and where no launcher
    /// version is kept (Linux) a parent born before it adopted the member ([`Lineage::orphaned`]).
    run_uniqueid: u64,
    /// The open groups whose leader check passed, each with its leader's unique id, where its
    /// W-group window opens.
    groups: Vec<(u32, u64)>,
    /// Where both windows close: `dead_seen`, or no bound for a run that is alive.
    horizon: u64,
    launchd: Option<i32>,
    claims: &'a Claims,
}

impl<K: Kernel> Judge<'_, K> {
    /// Whether `pid` is the run's, and whether ending it is proven, on fresh reads.
    fn judge(&self, pid: i32) -> Verdict {
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

    /// Judge each of `pids`, end what is proven (judged again before each signal).
    fn pass(&self, pids: &[i32]) -> Left {
        let mut left = Left::default();
        for &pid in pids {
            let proven = match self.judge(pid) {
                Verdict::NotOurs => continue,
                Verdict::Unjudged => {
                    left.unjudged += 1;
                    continue;
                }
                Verdict::Named => false,
                Verdict::Proven => true,
            };
            let word = word_of(self.kernel, pid);
            let fate = settle(self.kernel, pid, |token| {
                proven.then(|| {
                    self.kernel
                        .end(token, &|id| self.judge(id.pid) == Verdict::Proven)
                })
            });
            match fate {
                Fate::Ended => left.ended.push(word),
                Fate::Left => left.named.push(word),
                Fate::Gone => {}
            }
        }
        left
    }
}

/// Live pids of this uid (excluding `me` and its ancestors), oldest first, and the high-water
/// unique id. No process is older than its parent, so a pass judges a parent before its
/// children: the end of an orphaned stage leader orphans its children before they are judged.
fn table(kernel: &impl Kernel, me: i32) -> Option<(Vec<i32>, u64)> {
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
    use std::io::Read as _;
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a bounded regular run record",
        )
    };
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(invalid());
    }
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // Open a reparse point itself, never the object it redirects to.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        open.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = open.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > RECORD_BYTES {
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
    let mut bytes = Vec::new();
    file.take(RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECORD_BYTES {
        return Err(invalid());
    }
    String::from_utf8(bytes)
        .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidData, why))
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

/// Validate the cleanup protocol without changing [`Record::parse`]'s lenient projection.
fn cleanup_record(path: &Path, text: &str) -> Option<Record> {
    if text.is_empty() || !text.ends_with('\n') {
        return None;
    }
    for (index, line) in text.lines().enumerate() {
        match line.split_whitespace().next()? {
            "run" if index == 0 => {
                let fields =
                    cleanup_fields(line, &["v", "boot", "id", "uniqueid", "start", "sid"])?;
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

/// A complete admitted snapshot, or unknown. Never build partial competing claims.
fn read_records(dir: &Path) -> std::io::Result<Vec<(PathBuf, Record)>> {
    let entries = std::fs::read_dir(dir)?;
    let mut records = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("run") {
            continue;
        }
        let text = read_record_text(&path)?;
        let rec = cleanup_record(&path, &text).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} is not a complete supported run record", path.display()),
            )
        })?;
        records.push((path, rec));
    }
    Ok(records)
}

/// Sweep processes left behind by dead verify runs recorded in `dir`.
pub fn end_dead_runs(
    dir: &Path,
    this_boot: &str,
    me: i32,
    kernel: &impl Kernel,
    out: &mut dyn FnMut(&str),
) -> usize {
    let admitted = match read_records(dir) {
        Ok(records) => records,
        Err(why) => {
            out(&format!(
                "could not admit all verify run records: {why}; all records are kept"
            ));
            return 0;
        }
    };
    let mut records = Vec::new();
    for (path, rec) in admitted {
        if rec.boot == this_boot {
            records.push((path, rec));
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    let (dead, live): (Vec<_>, Vec<_>) = records
        .into_iter()
        .partition(|(_, r)| !launcher_alive(r, kernel));
    if dead.is_empty() {
        return 0;
    }
    let claims = Claims::of(live.iter().map(|(_, r)| r));
    let Some((pids, high_water)) = table(kernel, me) else {
        out("could not list this user's processes: the dead verify runs' records are kept");
        return 0;
    };
    let launchd = kernel.launchd_version();
    let mut ended = 0;
    for (path, rec) in &dead {
        let horizon = rec.dead_seen.unwrap_or_else(|| {
            append(
                path,
                &format!("dead_seen uniqueid={high_water} at={}", kernel.now_ms()),
            );
            high_water
        });
        let groups = rec
            .open_groups()
            .filter(|g| {
                kernel
                    .birth(i32::try_from(g.pgid).unwrap_or(0))
                    .is_some_and(|b| b == g.leader_birth)
            })
            .map(|g| (g.pgid, g.leader_uniqueid))
            .collect();
        let judge = Judge {
            kernel,
            roots: &rec.roots,
            id: rec.id.map(|i| i.text()),
            run_uniqueid: rec.uniqueid,
            groups,
            horizon,
            launchd,
            claims: &claims,
        };
        let left = judge.pass(&pids);
        ended += left.ended.len();
        out(&dead_run_line(rec, &left));
        if !left.leaves_any() {
            let _ = std::fs::remove_file(path);
        }
    }
    ended
}

/// Append `line` to the record at `path`, best-effort.
fn append(path: &Path, line: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Whether a record's launcher may still be alive; unreadable identities retain its claims.
fn launcher_alive(rec: &Record, kernel: &impl Kernel) -> bool {
    let Some(id) = rec.id else {
        return false;
    };
    may_match_birth(kernel, id.pid, |birth| birth == id.birth)
}

/// Format the summary line for a dead run at the next run's start.
fn dead_run_line(rec: &Record, left: &Left) -> String {
    let root = rec
        .roots
        .first()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "?".to_owned());
    let pid = rec.id.map_or(0, |i| i.pid);
    let head = format!("the verify run of pid {pid} ({root})");
    left.line(&head)
        .unwrap_or_else(|| format!("{head} left no live processes"))
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

/// Canonical roots a survivor's exe must sit under to be ended ([`build_dirs`]).
#[must_use]
pub fn run_roots(root: &Path, scratch: &Path, target: &Path, built: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for path in [root, scratch, target]
        .into_iter()
        .chain(built.iter().map(PathBuf::as_path))
    {
        let path = canonical(path);
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
}

/// `path` with every link resolved as far as it exists, and the rest appended as written.
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

/// Build the `targo metadata` command for the main lane ([`crate::stages::main_targo`]).
#[must_use]
pub fn metadata_ask(ctx: &Ctx) -> crate::exec::Cmd {
    crate::stages::main_targo(
        ctx,
        [
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ]
        .map(String::from)
        .to_vec(),
    )
}

/// Where this run's main lane builds, as cargo itself names it.
#[must_use]
pub fn build_dirs(
    ctx: &Ctx,
    ask: impl FnOnce(&crate::exec::Cmd, crate::exec::ExecEnv<'_>) -> crate::exec::Run,
) -> Vec<PathBuf> {
    let run = ask(&metadata_ask(ctx), ctx.exec_env());
    if run.ok {
        parse_build_dirs(&run.output)
    } else {
        Vec::new()
    }
}

/// `cargo metadata`'s `target_directory` and `build_directory`, each one it names.
#[must_use]
pub fn parse_build_dirs(output: &str) -> Vec<PathBuf> {
    let Some(meta) = output
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .find_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
    else {
        return Vec::new();
    };
    ["target_directory", "build_directory"]
        .into_iter()
        .filter_map(|key| meta.get(key).and_then(aterm_json::Value::as_str))
        .filter(|dir| Path::new(dir).is_absolute())
        .map(PathBuf::from)
        .collect()
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
fn pct_encode(text: &str) -> String {
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
fn pct_decode(text: &str) -> String {
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
#[path = "runs_tests.rs"]
mod tests;

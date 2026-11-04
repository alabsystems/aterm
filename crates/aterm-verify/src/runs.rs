// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verify's run records written, stage survivor checks, and dead-run process sweeps
//! ([`end_proven`](aterm_uds::process::end_proven)). The record's format, its readers, the
//! kernel seam and the A1–A7 verdict are [`aterm_uds::runs`]'s, re-exported here.

use std::collections::BTreeMap;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_uds::process::{
    EndRefusal, Ended, Identity, boot_id, held_age, read_birth, read_lineage,
};
pub use aterm_uds::runs::{
    Claims, GroupLine, Judge, Kernel, LiveKernel, Record, RunId, Verdict, append, canonical,
    exe_word, header_text, launcher_alive, led_groups, may_match_birth, open_regular_write,
    pct_decode, pct_encode, read_bounded_regular_text, read_records, records_dir, table, under_any,
};

use crate::Ctx;
use crate::findings::tally_words;

/// `ATERM_VERIFY_RUN` ([`aterm_uds::process::VERIFY_RUN_VAR`]).
pub const VERIFY_RUN_ENV: &str = aterm_uds::process::VERIFY_RUN_VAR;

/// Where a pass tells what it ended unasked: one line per process, after the pass, never read
/// back, so it changes no verdict and no receipt line. [`Inert`] tells nobody (the default, and
/// every test's); verify's own entry tells the instance it runs under ([`ToInstance`]).
pub trait Poster: Send + Sync + std::fmt::Debug {
    /// Tells `lines`, best effort within its own bound.
    fn post(&self, lines: &[String]);
}

/// The poster that tells nobody.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inert;

impl Poster for Inert {
    fn post(&self, _: &[String]) {}
}

/// How long [`ToInstance`] gives the instance to take every line of one pass.
#[cfg(not(test))]
pub const POST_BOUND: Duration = Duration::from_secs(2);

/// Under test, the one-minute hang detector: a unit test's stand-in instance answers from a
/// thread of the test binary, which a loaded machine can keep from running for longer than 2 s,
/// and no test's property is how fast an instance answers.
#[cfg(test)]
pub const POST_BOUND: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The most bytes of one line's text an `appnotice` carries: within the 512 the instance takes.
const NOTICE_TEXT_BYTES: usize = 400;

/// Tells the aterm instance hosting the session verify runs in (the one `ATERM_PARENT_SESSION_ID`
/// names, found by its discovery graph entry: [`aterm_ctl::hosting_sock`], never another
/// instance), through aterm ctl's own client path ([`aterm_ctl::connect_then_read_token`]): one
/// `appnotice proc <line>` per line, as its owner, on a thread of its own that the pass waits for
/// at most [`POST_BOUND`] ([`within`]), where each is a `proc.auto-end` record. With no session
/// id, no instance hosting it, or none that answers in time, nothing is told, and the receipt's
/// line is the report.
#[derive(Clone, Debug, Default)]
pub struct ToInstance {
    /// The session verify runs in.
    pub sid: Option<String>,
    /// The rendezvous whose graph names the instance hosting it.
    pub dir: Option<PathBuf>,
}

impl ToInstance {
    /// The session this process's environment names, in this user's rendezvous.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            sid: std::env::var(aterm_types::domain::ENV_PARENT_SESSION_ID)
                .ok()
                .filter(|sid| !sid.is_empty()),
            dir: aterm_uds::control_socket_dir(),
        }
    }
}

impl Poster for ToInstance {
    fn post(&self, lines: &[String]) {
        if let (Some(sid), Some(dir), false) =
            (self.sid.clone(), self.dir.clone(), lines.is_empty())
        {
            let lines = lines.to_vec();
            within(POST_BOUND, move || {
                let _ = tell(&dir, &sid, &lines, Instant::now() + POST_BOUND);
            });
        }
    }
}

/// Runs `post` on a thread of its own and waits for it at most `bound`: a connect the kernel
/// blocks (a Linux listener whose backlog is full while its instance is stopped) or a reply that
/// comes in pieces holds that thread, never the pass that posts. A thread that does not start
/// tells nothing.
fn within(bound: Duration, post: impl FnOnce() + Send + 'static) {
    let (done, finished) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("verify-post".to_owned())
        .spawn(move || {
            post();
            let _ = done.send(());
        });
    if started.is_ok() {
        let _ = finished.recv_timeout(bound);
    }
}

/// `line` as one `appnotice proc` text: control characters spaced, cut at a character boundary
/// within [`NOTICE_TEXT_BYTES`] bytes, since the instance takes at most 512 and drops a longer one.
fn notice_text(line: &str) -> String {
    let mut text = String::new();
    for c in line.chars().map(|c| if c.is_control() { ' ' } else { c }) {
        if text.len() + c.len_utf8() > NOTICE_TEXT_BYTES {
            break;
        }
        text.push(c);
    }
    text
}

/// One `appnotice proc` per line to the instance `dir`'s graph says hosts `sid`, after the
/// instance token's `AUTH` (which answers nothing), on one connection; each answers one line,
/// and every write and read is done by `deadline`.
fn tell(dir: &Path, sid: &str, lines: &[String], deadline: Instant) -> std::io::Result<()> {
    let left = || {
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1))
    };
    let sid = aterm_types::control_request::Sid::parse(sid)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let sock = aterm_ctl::hosting_sock(dir, sid)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    let (stream, token) = aterm_ctl::connect_then_read_token(&sock)?;
    let token = token?;
    stream.set_write_timeout(Some(left()))?;
    let mut request = format!("AUTH {token}\n");
    for line in lines {
        request.push_str(&format!("appnotice proc {}\n", notice_text(line)));
    }
    (&stream).write_all(request.as_bytes())?;
    (&stream).flush()?;
    stream.set_read_timeout(Some(left()))?;
    let mut reader = std::io::BufReader::new(&stream);
    for _ in lines {
        let mut reply = String::new();
        if reader.read_line(&mut reply)? == 0 || Instant::now() >= deadline {
            break;
        }
    }
    Ok(())
}

/// One process a pass ended: what [`Poster::post`] tells of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Told {
    /// Its pid.
    pub pid: i32,
    /// Its executable's word.
    pub word: String,
    /// The witness that made it the run's: `group` (W-group) or `run` (W-run); C-tree held too.
    pub witness: &'static str,
    /// It outlived its `SIGTERM` and took a `SIGKILL`.
    pub killed: bool,
}

impl Told {
    /// `verify ended <word> (pid <pid>), <what>: proof <witness>,tree; <signals>; gone`.
    fn line(&self, what: &str) -> String {
        let signals = if self.killed {
            "SIGTERM, then SIGKILL"
        } else {
            "SIGTERM"
        };
        format!(
            "verify ended {} (pid {}), {what}: proof {},tree; {signals}; gone",
            self.word, self.pid, self.witness
        )
    }
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
    /// The stage groups swept whose report the leader's reap may change
    /// ([`GroupReport::awaits_reap`]) and whose leaders are not reaped yet, each with how many
    /// `group` lines the record had for its pgid when it was swept, and its sweep's report.
    swept: Mutex<Vec<(u32, usize, GroupReport)>>,
    /// How many `group` lines the record has for each pgid. A `closed` line closes the last
    /// `group` line for its pgid written before it ([`Record::parse`]), so one written after its
    /// leader is reaped, when the freed id may lead a group made since, is written only while
    /// no newer `group` line names that id ([`Self::group_reaped`]).
    made: Mutex<BTreeMap<u32, usize>>,
    /// The lines the receipt carries: each stage's survivors, as their sweeps found them.
    said: Mutex<Vec<String>>,
    /// Only a fresh, admitted run-end sweep with no survivors permits removal.
    may_forget: AtomicBool,
    /// Where the stage and run-end sweeps tell what they ended ([`Inert`] until
    /// [`Self::telling`]).
    poster: Box<dyn Poster>,
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
    /// A group listed in part reads as listed in full once the kernel finds it empty after its
    /// leader is reaped ([`Self::reaped`]).
    pub listed: Option<bool>,
}

impl GroupReport {
    /// Proven empty: listed in full, and every survivor ended.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.listed == Some(true) && self.named.is_empty()
    }

    /// Whether the group's leader's reap may change the report ([`Self::reaped`]): listed only in
    /// part, and naming nothing.
    #[must_use]
    pub fn awaits_reap(&self) -> bool {
        self.listed == Some(false) && self.named.is_empty()
    }

    /// The report once the group's leader is reaped: a group listed only in part (a `/proc`
    /// mounted with `hidepid` hides what it cannot show) whose sweep named nothing is listed in
    /// full when `empty`, one answer of the kernel about the whole group
    /// ([`crate::exec::group::empty`]), finds no process in it. Any other answer, a survivor
    /// named, or a group never listed leaves it as it was. A member the sweep ended that its new
    /// parent has not reaped yet is still in the group, so such a group may stay open.
    #[must_use]
    pub fn reaped(mut self, empty: impl FnOnce() -> Option<bool>) -> Self {
        if self.awaits_reap() && empty() == Some(true) {
            self.listed = Some(true);
        }
        self
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
    /// The ended, as a poster tells them.
    pub told: Vec<Told>,
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

/// What became of one process a pass set out to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// Signalled until gone, and SIGKILLed when `killed`.
    Ended {
        /// It outlived its `SIGTERM`.
        killed: bool,
    },
    /// Gone before any signal: it holds nothing.
    Gone,
    /// Still there and not ended: named.
    Left,
}

/// Name unreadable survivors; only changed birth or confirmed absence establishes departure.
/// The lineage read first comes back with the fate.
fn settle(
    kernel: &impl Kernel,
    pid: i32,
    end: impl FnOnce(Identity) -> Option<Result<Ended, EndRefusal>>,
) -> (Fate, Option<aterm_uds::process::Lineage>) {
    let Some(lin) = kernel.lineage(pid) else {
        let fate = if may_match_birth(kernel, pid, |_| true) {
            Fate::Left
        } else {
            Fate::Gone
        };
        return (fate, None);
    };
    let answer = end(lin.identity);
    let fate = if may_match_birth(kernel, pid, |birth| birth == lin.identity.birth) {
        Fate::Left
    } else if let Some(Ok(e)) = answer
        && (e.termed || e.killed)
    {
        Fate::Ended { killed: e.killed }
    } else {
        Fate::Gone
    };
    (fate, Some(lin))
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
        aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o700).ok()?;
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
        let file = open_regular_write(&path, false).ok()?;
        aterm_uds::runs::write_record_text(&file, &header).ok()?;
        Some(Self {
            log: Mutex::new(file),
            path,
            boot,
            id,
            uniqueid,
            roots,
            swept: Mutex::new(Vec::new()),
            made: Mutex::new(BTreeMap::new()),
            said: Mutex::new(Vec::new()),
            may_forget: AtomicBool::new(false),
            poster: Box::new(Inert),
        })
    }

    /// The record, its stage and run-end sweeps telling `poster` what they end.
    #[must_use]
    pub fn telling(mut self, poster: Box<dyn Poster>) -> Self {
        self.poster = poster;
        self
    }

    /// Append one line (a newline is added), best-effort.
    fn line(&self, line: &str) {
        if let Ok(file) = self.log.lock() {
            let _ = aterm_uds::runs::write_line(&file, line);
        }
    }

    /// The `ATERM_VERIFY_RUN` value every stage child carries (W-run): this run's id.
    #[must_use]
    pub fn env_value(&self) -> String {
        self.id.text()
    }

    /// Keep what one stage group's sweep found: its `closed` line, when the report is closed and
    /// `may_close` (the line would land on this group's `group` line), and its receipt line.
    fn settle_group(&self, pgid: u32, report: &GroupReport, may_close: bool) {
        if report.closed() && may_close {
            self.line(&format!("closed pgid={pgid}"));
        }
        if let Some(line) = report.line()
            && let Ok(mut said) = self.said.lock()
        {
            said.push(line);
        }
    }

    /// Sweep remaining run processes and return survivor report lines. A stage group whose
    /// leader's reap never came is kept as its sweep found it, listed in part, never closed.
    pub fn end_of_run(&self, kernel: &impl Kernel) -> Vec<String> {
        let unreaped = self
            .swept
            .lock()
            .map(|mut swept| std::mem::take(&mut *swept))
            .unwrap_or_default();
        for (pgid, _, report) in &unreaped {
            self.settle_group(*pgid, report, false);
        }
        self.may_forget.store(false, Ordering::Relaxed);
        let line = match self.run_end_sweep(kernel) {
            Some(left) => {
                self.may_forget.store(!left.leaves_any(), Ordering::Relaxed);
                let told: Vec<String> = left
                    .told
                    .iter()
                    .map(|t| t.line("an orphan of its own run"))
                    .collect();
                self.poster.post(&told);
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
        Some(pass(&judge, &pids))
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
        // Counted and written under one lock, which a `closed` line written after a reap reads
        // the count under too.
        let mut made = self.made.lock();
        if let Ok(made) = made.as_mut() {
            *made.entry(pgid).or_default() += 1;
        }
        self.line(&format!(
            "group pgid={pgid} leader_birth={birth} leader_uniqueid={uniqueid} stage={}",
            stage_field(label)
        ));
        drop(made);
    }

    /// A report the reap cannot change is kept at once, its leader still unreaped, so no group
    /// can be made under its id before its `closed` line; the rest wait for the reap.
    fn group_ended(&self, pgid: u32, label: &str) {
        let report = sweep_group(&LiveKernel, pgid, label, &self.roots, &*self.poster);
        if !report.awaits_reap() {
            self.settle_group(pgid, &report, true);
            return;
        }
        let lines = self
            .made
            .lock()
            .map_or(0, |made| made.get(&pgid).copied().unwrap_or(0));
        if let Ok(mut swept) = self.swept.lock() {
            swept.push((pgid, lines, report));
        }
    }

    /// The reap frees the leader's id, so the `closed` line is written only while the record's
    /// last `group` line for the id is still the one swept, and never once a group made since
    /// under the freed id has its line: that group then stays open, as an unproven one does.
    fn group_reaped(&self, pgid: u32, _label: &str) {
        let swept = self.swept.lock().ok().and_then(|mut swept| {
            let at = swept.iter().position(|(g, _, _)| *g == pgid)?;
            Some(swept.remove(at))
        });
        if let Some((_, lines, report)) = swept {
            #[cfg(unix)]
            let report = report.reaped(|| crate::exec::group::empty(pgid));
            let made = self.made.lock();
            let last = made
                .as_ref()
                .is_ok_and(|made| made.get(&pgid).copied().unwrap_or(0) == lines);
            self.settle_group(pgid, &report, last);
            drop(made);
        }
    }
}

/// The most bytes a `group` line's `stage=` field carries ([`stage_field`]): a label of
/// [`crate::exec::LABEL_MAX`] bytes whole, whatever each byte encodes to.
const STAGE_FIELD_MAX: usize = 3 * crate::exec::LABEL_MAX;

/// A `group` line's `stage=` field: `label` [`pct_encode`]d, or, when that passes
/// [`STAGE_FIELD_MAX`] bytes, each of its characters while the field and an encoded `…` fit,
/// then that `…`. So a record stays bounded whatever label a stage's group is made with.
fn stage_field(label: &str) -> String {
    let whole = pct_encode(label);
    if whole.len() <= STAGE_FIELD_MAX {
        return whole;
    }
    let cut = pct_encode("…");
    let mut field = String::new();
    let mut buf = [0u8; 4];
    for c in label.chars() {
        let encoded = pct_encode(c.encode_utf8(&mut buf));
        if field.len() + encoded.len() + cut.len() > STAGE_FIELD_MAX {
            break;
        }
        field.push_str(&encoded);
    }
    field.push_str(&cut);
    field
}

/// Sweep one stage group whose leader has exited unreaped (`pgid` is the leader's pid), and tell
/// `poster` each member it ended.
pub fn sweep_group(
    kernel: &impl Kernel,
    pgid: u32,
    label: &str,
    roots: &[PathBuf],
    poster: &dyn Poster,
) -> GroupReport {
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
    let mut told = Vec::new();
    for member in members.pids.into_iter().filter(|&m| m != leader) {
        let word = word_of(kernel, member);
        let (fate, _) = settle(kernel, member, |token| {
            proven(member).then(|| kernel.end(token, &|id| proven(id.pid)))
        });
        match fate {
            Fate::Ended { killed } => {
                told.push(
                    Told {
                        pid: member,
                        word: word.clone(),
                        witness: "group",
                        killed,
                    }
                    .line(&format!("left in stage `{label}`'s group")),
                );
                report.ended.push(word);
            }
            Fate::Left => report.named.push(word),
            Fate::Gone => {}
        }
    }
    poster.post(&told);
    report
}

/// Judge each of `pids` for `judge`'s run, end what is proven (judged again before each signal).
fn pass<K: Kernel>(judge: &Judge<'_, K>, pids: &[i32]) -> Left {
    let mut left = Left::default();
    for &pid in pids {
        let proven = match judge.judge(pid) {
            Verdict::NotOurs => continue,
            Verdict::Unjudged => {
                left.unjudged += 1;
                continue;
            }
            Verdict::Named => false,
            Verdict::Proven => true,
        };
        let word = word_of(judge.kernel, pid);
        let (fate, lin) = settle(judge.kernel, pid, |token| {
            proven.then(|| {
                judge
                    .kernel
                    .end(token, &|id| judge.judge(id.pid) == Verdict::Proven)
            })
        });
        match fate {
            Fate::Ended { killed } => {
                let group = lin.is_some_and(|l| {
                    judge.groups.iter().any(|&(pgid, from)| {
                        pgid == l.pgid && (from..=judge.horizon).contains(&l.uniqueid)
                    })
                });
                left.told.push(Told {
                    pid,
                    word: word.clone(),
                    witness: if group { "group" } else { "run" },
                    killed,
                });
                left.ended.push(word);
            }
            Fate::Left => left.named.push(word),
            Fate::Gone => {}
        }
    }
    left
}

/// The run-start line for macOS's program check: "macOS's program check: N programs waiting, the oldest started X ago (aterm listed: no)", over this user's programs [`Kernel::held_now`] counts held. A one-shot reading has no first sighting, so the age is the oldest one's fork age, an upper bound. `listed` names the app macOS holds responsible for verify, which takes aterm's place in the words, and its word (`yes`, `no` or `unknown`). `None`, with `listed` never called, when the listing fails or nothing is held.
pub fn program_check_line(
    kernel: &impl Kernel,
    listed: impl FnOnce() -> (Option<String>, &'static str),
) -> Option<String> {
    let held = kernel.held_now().filter(|held| held.count > 0)?;
    let (app, word) = listed();
    let app = app.unwrap_or_else(|| "aterm".to_owned());
    let programs = if held.count == 1 {
        "program"
    } else {
        "programs"
    };
    Some(format!(
        "macOS's program check: {} {programs} waiting, the oldest started {} ago ({app} listed: \
         {word})",
        held.count,
        held_age(held.oldest_ms)
    ))
}

/// Sweep processes left behind by dead verify runs recorded in `dir`, and tell `poster` each
/// one ended.
pub fn end_dead_runs(
    dir: &Path,
    this_boot: &str,
    me: i32,
    kernel: &impl Kernel,
    out: &mut dyn FnMut(&str),
    poster: &dyn Poster,
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
        let horizon =
            match aterm_uds::runs::append_dead_seen(path, rec, high_water, kernel.now_ms()) {
                Ok(recorded) => recorded.horizon,
                Err(why) => {
                    out(&format!(
                        "could not retain the dead-run observation for {}: {why}; record kept",
                        path.display()
                    ));
                    continue;
                }
            };
        let groups = led_groups(rec, kernel);
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
        let left = pass(&judge, &pids);
        ended += left.ended.len();
        let told: Vec<String> = left
            .told
            .iter()
            .map(|t| t.line(&format!("left by {}", dead_run_head(rec))))
            .collect();
        poster.post(&told);
        out(&dead_run_line(rec, &left));
        if !left.leaves_any() {
            let _ = std::fs::remove_file(path);
        }
    }
    ended
}

/// `the verify run of pid <launcher> (<its first root>)`.
fn dead_run_head(rec: &Record) -> String {
    let root = rec
        .roots
        .first()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "?".to_owned());
    let pid = rec.id.map_or(0, |i| i.pid);
    format!("the verify run of pid {pid} ({root})")
}

/// Format the summary line for a dead run at the next run's start.
fn dead_run_line(rec: &Record, left: &Left) -> String {
    let head = dead_run_head(rec);
    left.line(&head)
        .unwrap_or_else(|| format!("{head} left no live processes"))
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

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;

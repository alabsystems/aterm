// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab git-starts`: how many processes, and how many git processes, the sessions of one busy Mac
//! start, and what a work record that each agent session reads with git at its turn ends would add.
//!
//! It only reads. Every `--every-ms` (100 ms) it lists this uid's processes
//! ([`aterm_uds::holders::uid_pids`]) and reads each pid new to the listing, and again at the next
//! listing each one that read as none: its birth ([`aterm_uds::process::read_process_birth`]) and
//! its executable's path ([`aterm_uds::process::read_process_path`]), never its arguments or its
//! environment. Every [`SURVEY_EVERY`] it reads each local aterm's `survey facts` over its control
//! socket, from this process, so the measurement starts no process of its own. A survey read
//! counts only where every aterm that accepts on its socket answered it ([`survey_pass`]).
//!
//! - **Process starts**, every uid's, are the advance of the newest-born process's pid between
//!   listings, from the first process born during the run: macOS hands pids out in order and goes
//!   on from [`PID_WRAP_TO`] once the counter reaches [`PID_MAX`] ([`forward`]). Past a wrap the
//!   kernel skips the pids still in use, and the advance counts those too.
//! - **Git starts** are the distinct (pid, birth) of this uid whose executable is named `git`, seen
//!   in a listing after the first. A lower bound: a git that starts and ends between two listings
//!   is never seen, nor is one that a process execs after a look that read it.
//! - **Turn ends** are an agent row's `agent=` going from a turn ([`in_turn`]) to rest
//!   ([`resting`]) between two survey reads. A lower bound too: a rest shorter than a read period
//!   is missed.
//! - **The projection** replays the record's refresh rule over each agent session's turn ends
//!   ([`floor_reads`]): one read at the first look of a session that appears during the run, then
//!   one at each turn end [`FLOOR`] or more after the last read, each read making
//!   [`STARTS_PER_READ`] git starts. Its ceiling is every agent session read once per [`FLOOR`].
//!
//! The readers sit behind [`Readers`], so the counting is a unit test over a fake.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::instance::{count, take_value};
use crate::{Fail, LAB, Res};

/// The value macOS's pid counter never hands out: at it, the counter goes on from
/// [`PID_WRAP_TO`] (`PID_MAX`, `bsd/sys/proc_internal.h`).
pub const PID_MAX: u32 = 99_999;
/// Where macOS's pid counter goes on from once it reaches [`PID_MAX`] (`bsd/kern/kern_fork.c`).
pub const PID_WRAP_TO: u32 = 100;
/// The work record's floor: a session's record is read at most once per this.
pub const FLOOR: Duration = aterm_agent::harness::work_record::WORK_FLOOR;
/// How often the survey is read: shorter than the 2 s a harness holds a rest before it types
/// (`IDLE_MS`, `crates/aterm-agent/src/supervise/run.rs`), so a read lands inside each rest a
/// harness ends.
pub const SURVEY_EVERY: Duration = Duration::from_secs(1);
/// How long one instance's survey read may take before it counts as unanswered.
const SURVEY_WITHIN: Duration = Duration::from_secs(4);
/// The git starts one record read makes, fewest and most: `rev-parse` (the common dir and the top
/// level), the branch, the main ref (`origin/HEAD`, then one to three `rev-parse --verify`),
/// `merge-base`, the behind count, `diff-tree`, the person's excludes (one or two) and the status.
/// Not counted: in a checkout whose status shows a submodule modified, the probe's status read
/// adds a `status --porcelain=v2` of those submodules, and where a submodule's commit alone moved,
/// a `rev-parse --git-path` of the reflog, its `diff-tree --stdin` and a `rev-parse` of each such
/// submodule's `HEAD` (`without_lagging_submodules`, `crates/aterm-agent/src/harness/repo_work.rs`).
pub const STARTS_PER_READ: (u32, u32) = (9, 12);

/// A process [`Readers::process`] read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    /// Its birth, whole microseconds of the kernel's start record ([`aterm_uds::process::Birth`]).
    pub birth: u64,
    /// Whether its executable's file name is `git`.
    pub git: bool,
}

/// One session row of a survey read, as the turn-end count reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The session's sid.
    pub sid: String,
    /// Whether the row is an agent's (`program_kind=agent`).
    pub agent: bool,
    /// Its `agent=` word.
    pub word: String,
}

/// What the measurement reads: the live machine ([`Live`]) or a test's fake.
pub trait Readers {
    /// This uid's live pids, or `None` where the listing failed.
    fn pids(&mut self) -> Option<Vec<u32>>;
    /// `pid`'s [`Seen`], or `None` where it could not be read: gone, not this uid's, or caught
    /// while it starts.
    fn process(&mut self, pid: u32) -> Option<Seen>;
    /// The oldest survey read finished and not yet taken, with the instant it finished (ms into
    /// the run): its rows, or `None` where a local aterm did not answer it ([`survey_pass`]).
    /// `None` when every finished read was taken.
    fn survey(&mut self) -> Option<(u64, Option<Vec<Row>>)>;
}

/// One agent session the survey showed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    /// When a survey read first showed it, ms into the run.
    pub first_ms: u64,
    /// When a survey read last showed it.
    pub last_ms: u64,
    /// Whether it first showed after the run's first survey read: a session started during the run.
    pub new: bool,
    /// Its `agent=` word at the last read that showed it.
    pub word: String,
    /// When each of its turn ends was seen.
    pub turn_ends: Vec<u64>,
}

/// Every count of one run, folded one listing and one survey read at a time.
#[derive(Debug, Default)]
pub struct Tally {
    /// Listings read, and listings that failed.
    listings: u64,
    failed: u64,
    /// The first and the last listing's instant, ms into the run.
    span: Option<(u64, u64)>,
    /// The longest time between two listings.
    max_gap_ms: u64,
    /// The newest-born process seen so far, as (birth, pid).
    newest: Option<(u64, u32)>,
    /// Process starts: the newest-born process's pid advance, from the first process born during
    /// the run.
    starts: u64,
    /// The listings that showed the first process born during the run and the last that moved
    /// [`Self::newest`] on, ms into the run: the span [`Self::starts`] covers.
    counted: Option<(u64, u64)>,
    /// The previous listing's pids, but those whose read was none.
    listed: HashSet<u32>,
    /// Every git seen, as (pid, birth).
    gits: HashSet<(u32, u64)>,
    /// Survey reads, those a local aterm did not answer, the agent rows the answered ones showed
    /// in all, and those rows that read `busy`.
    surveys: u64,
    unanswered: u64,
    shown: u64,
    busy: u64,
    /// Every session's sid the survey showed.
    sessions: HashSet<String>,
    /// Every agent session, by sid.
    agents: HashMap<String, Session>,
}

/// How many pids the counter hands out after `from` up to `to`, `to` being a later-born process's:
/// past [`PID_MAX`] the counter goes on from [`PID_WRAP_TO`]. A pid below [`PID_WRAP_TO`] is
/// handed out only before the first wrap, so it is never ahead of a larger one (0).
#[must_use]
pub const fn forward(from: u32, to: u32) -> u32 {
    if to >= from {
        to - from
    } else if to >= PID_WRAP_TO && from < PID_MAX {
        (PID_MAX - from) + (to - PID_WRAP_TO)
    } else {
        0
    }
}

/// How many times the record would be read over `session`'s turn ends: once at its first look if
/// it started during the run, then at each turn end `floor` or more after the last read. A
/// session already running when the run began is taken as read longer than `floor` ago.
#[must_use]
pub fn floor_reads(session: &Session, floor: Duration) -> u32 {
    let floor = u64::try_from(floor.as_millis()).unwrap_or(u64::MAX);
    let mut last = session.new.then_some(session.first_ms);
    let mut reads = u32::from(session.new);
    for &at in &session.turn_ends {
        if last.is_none_or(|last| at >= last.saturating_add(floor)) {
            reads += 1;
            last = Some(at);
        }
    }
    reads
}

/// Whether `word` is an agent inside a turn: working (`busy`), or at an approval box the turn waits
/// on (`prompt`).
#[must_use]
pub fn in_turn(word: &str) -> bool {
    matches!(word, "busy" | "prompt")
}

/// Whether `word` is an agent at rest after a turn: at its composer (`idle`, or `survey` with
/// Claude Code's feedback card above it), having asked a question (`question`), or stopped by a
/// wall the turn ended on (`wall:<kind>`).
#[must_use]
pub fn resting(word: &str) -> bool {
    matches!(word, "idle" | "survey" | "question") || word.starts_with("wall:")
}

/// The session rows of one `survey` reply's body: its `session` lines' `sid=`, `program_kind=` and
/// `agent=`, values percent-decoded.
#[must_use]
pub fn rows_of(body: &str) -> Vec<Row> {
    body.lines()
        .filter_map(|line| line.strip_prefix("session "))
        .filter_map(|fields| {
            let mut row = Row {
                sid: String::new(),
                agent: false,
                word: String::new(),
            };
            for (key, value) in fields.split(' ').filter_map(|kv| kv.split_once('=')) {
                match key {
                    "sid" => row.sid = aterm_codec::pct::decode(value),
                    "program_kind" => row.agent = value == "agent",
                    "agent" => row.word = aterm_codec::pct::decode(value),
                    _ => {}
                }
            }
            (!row.sid.is_empty()).then_some(row)
        })
        .collect()
}

impl Tally {
    /// Fold one listing read at `at_ms` (`None`: it failed), reading with `read` each pid new to
    /// it and each the last listing read as none (each pid of the first). The first listing only
    /// learns what already runs: its processes started before the run, and its newest-born is no
    /// start of the run's.
    pub fn list(
        &mut self,
        at_ms: u64,
        pids: Option<&[u32]>,
        mut read: impl FnMut(u32) -> Option<Seen>,
    ) {
        let Some(pids) = pids else {
            self.failed += 1;
            return;
        };
        let first = self.span.is_none();
        self.listings += 1;
        self.span = Some(match self.span {
            None => (at_ms, at_ms),
            Some((from, last)) => {
                self.max_gap_ms = self.max_gap_ms.max(at_ms.saturating_sub(last));
                (from, at_ms)
            }
        });
        // The newest process this listing adds, by birth: an old process's pid can sit anywhere
        // in the counter's cycle, so only a birth says which pid the counter handed out last.
        let mut born: Option<(u64, u32)> = None;
        let mut listed = HashSet::with_capacity(pids.len());
        for &pid in pids {
            if !first && self.listed.contains(&pid) {
                listed.insert(pid);
                continue;
            }
            // A pid that reads as none is read again at the next listing: a process caught while
            // it starts can read so, and be a git by then.
            let Some(seen) = read(pid) else { continue };
            listed.insert(pid);
            if seen.git && !first {
                self.gits.insert((pid, seen.birth));
            }
            if born.is_none_or(|(birth, _)| seen.birth > birth) {
                born = Some((seen.birth, pid));
            }
        }
        match (self.newest, born) {
            (None, Some(born)) => self.newest = Some(born),
            (Some((before, from)), Some((birth, pid))) if birth > before => {
                // The first process born during the run is where the count starts: the pids
                // handed out between the newest-born before it and it are not all the run's.
                match &mut self.counted {
                    None => self.counted = Some((at_ms, at_ms)),
                    Some((_, last)) => {
                        self.starts += u64::from(forward(from, pid));
                        *last = at_ms;
                    }
                }
                self.newest = Some((birth, pid));
            }
            _ => {}
        }
        self.listed = listed;
    }

    /// Fold one survey read that finished at `at_ms`: its rows, or `None` where a local aterm did
    /// not answer it, which folds nothing more. A turn end is an agent row that the last read
    /// showed in a turn coming to rest.
    pub fn survey(&mut self, at_ms: u64, rows: Option<&[Row]>) {
        let Some(rows) = rows else {
            self.unanswered += 1;
            return;
        };
        let first = self.surveys == 0;
        self.surveys += 1;
        for row in rows {
            self.sessions.insert(row.sid.clone());
            if !row.agent {
                continue;
            }
            self.shown += 1;
            self.busy += u64::from(row.word == "busy");
            let session = self
                .agents
                .entry(row.sid.clone())
                .or_insert_with(|| Session {
                    first_ms: at_ms,
                    last_ms: at_ms,
                    new: !first,
                    word: row.word.clone(),
                    turn_ends: Vec::new(),
                });
            if in_turn(&session.word) && resting(&row.word) {
                session.turn_ends.push(at_ms);
            }
            session.word.clone_from(&row.word);
            session.last_ms = at_ms;
        }
    }

    /// One look at `at_ms`: a listing, and each survey read that finished since the last look, in
    /// the order they finished.
    pub fn look(&mut self, at_ms: u64, readers: &mut impl Readers) {
        let pids = readers.pids();
        self.list(at_ms, pids.as_deref(), |pid| readers.process(pid));
        while let Some((at, rows)) = readers.survey() {
            self.survey(at, rows.as_deref());
        }
    }

    /// The run's numbers.
    #[must_use]
    pub fn summary(&self) -> Summary {
        let (from, to) = self.span.unwrap_or((0, 0));
        let (minutes, gits) = (
            to.saturating_sub(from) as f64 / 60_000.0,
            self.gits.len() as f64,
        );
        let per_minute = |n: f64| if minutes > 0.0 { n / minutes } else { 0.0 };
        let (counted_from, counted_to) = self.counted.unwrap_or((0, 0));
        let counted_minutes = counted_to.saturating_sub(counted_from) as f64 / 60_000.0;
        let in_front = if self.surveys == 0 {
            0.0
        } else {
            self.shown as f64 / self.surveys as f64
        };
        let busy = if self.shown == 0 {
            0.0
        } else {
            self.busy as f64 / self.shown as f64
        };
        let survey_ms = u64::try_from(SURVEY_EVERY.as_millis()).unwrap_or(u64::MAX);
        let mut turns: Vec<f64> = self
            .agents
            .values()
            .map(|s| {
                let shown = s
                    .last_ms
                    .saturating_sub(s.first_ms)
                    .saturating_add(survey_ms);
                s.turn_ends.len() as f64 / (shown as f64 / 60_000.0)
            })
            .collect();
        turns.sort_by(f64::total_cmp);
        let reads: u32 = self.agents.values().map(|s| floor_reads(s, FLOOR)).sum();
        let started = self.agents.values().filter(|s| s.new).count();
        let floor_minutes = FLOOR.as_secs_f64() / 60.0;
        let ceiling = in_front / floor_minutes + per_minute(started as f64);
        Summary {
            minutes,
            listings: self.listings,
            failed: self.failed,
            mean_gap_ms: if self.listings > 1 {
                to.saturating_sub(from) / (self.listings - 1)
            } else {
                0
            },
            max_gap_ms: self.max_gap_ms,
            surveys: self.surveys,
            unanswered: self.unanswered,
            sessions: self.sessions.len(),
            agents: self.agents.len(),
            started,
            in_front,
            busy,
            process_starts: if counted_minutes > 0.0 {
                self.starts as f64 / counted_minutes
            } else {
                0.0
            },
            git_starts: per_minute(gits),
            git_starts_per_agent: if in_front > 0.0 {
                per_minute(gits) / in_front
            } else {
                0.0
            },
            turn_ends: (
                percentile(&turns, 0.5),
                percentile(&turns, 0.95),
                turns.last().copied().unwrap_or(0.0),
            ),
            reads: per_minute(f64::from(reads)),
            ceiling,
        }
    }
}

/// The nearest-rank `p` percentile of `sorted`, `0` of none.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// A run's numbers, as [`Tally::summary`] makes them and the verb prints them.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    /// The minutes between the first and the last listing.
    pub minutes: f64,
    /// Listings read, and listings that failed.
    pub listings: u64,
    pub failed: u64,
    /// The mean and the longest time between two listings.
    pub mean_gap_ms: u64,
    pub max_gap_ms: u64,
    /// Survey reads every local aterm answered, and those one did not.
    pub surveys: u64,
    pub unanswered: u64,
    /// Sessions the survey showed, agent sessions among them, and those started during the run.
    pub sessions: usize,
    pub agents: usize,
    pub started: usize,
    /// Agent sessions a survey read showed, on average.
    pub in_front: f64,
    /// The share of those rows that read `busy`.
    pub busy: f64,
    /// Process starts a minute, every uid's, over the minutes between the listings that showed
    /// the first process born during the run and the newest.
    pub process_starts: f64,
    /// Git starts seen a minute, and per agent session in front.
    pub git_starts: f64,
    pub git_starts_per_agent: f64,
    /// Turn ends a minute per agent session: the median, the 95th percentile and the most.
    pub turn_ends: (f64, f64, f64),
    /// The record's reads a minute at the floor, over the turn ends seen.
    pub reads: f64,
    /// The record's reads a minute with every agent session read once per [`FLOOR`].
    pub ceiling: f64,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (fewest, most) = (f64::from(STARTS_PER_READ.0), f64::from(STARTS_PER_READ.1));
        let floor = FLOOR.as_secs();
        writeln!(
            f,
            "sampled: {:.1} min, {} listings ({} ms apart on average, {} ms at most, {} failed), {} survey reads ({} more a local aterm did not answer)",
            self.minutes,
            self.listings,
            self.mean_gap_ms,
            self.max_gap_ms,
            self.failed,
            self.surveys,
            self.unanswered
        )?;
        writeln!(
            f,
            "sessions: {} seen, {} agent sessions ({:.1} at a time on average, {:.0} % of their rows busy, {} started during the run)",
            self.sessions,
            self.agents,
            self.in_front,
            self.busy * 100.0,
            self.started
        )?;
        writeln!(
            f,
            "process starts, every uid's (the newest-born pid's advance, from the first process born during the run): {:.0} a minute",
            self.process_starts
        )?;
        writeln!(
            f,
            "git starts seen, this uid's (a lower bound): {:.1} a minute, {:.1} a minute per agent session",
            self.git_starts, self.git_starts_per_agent
        )?;
        let (median, p95, max) = self.turn_ends;
        writeln!(
            f,
            "turn ends per agent session (a lower bound): median {median:.2}, p95 {p95:.2}, max {max:.2} a minute"
        )?;
        writeln!(
            f,
            "the record at the {floor} s floor: {:.2} reads a minute over the turn ends seen, adding {:.0}-{:.0} git starts a minute ({}-{} per read, more in a checkout whose submodule moved)",
            self.reads,
            self.reads * fewest,
            self.reads * most,
            STARTS_PER_READ.0,
            STARTS_PER_READ.1
        )?;
        writeln!(
            f,
            "its ceiling, every agent session read every {floor} s: {:.2} reads a minute, {:.0}-{:.0} git starts a minute",
            self.ceiling,
            self.ceiling * fewest,
            self.ceiling * most
        )
    }
}

/// The live machine's readers: `aterm_uds`'s process readers, and a thread that reads each local
/// aterm's survey every [`SURVEY_EVERY`].
struct Live {
    surveyed: std::sync::mpsc::Receiver<(u64, Option<Vec<Row>>)>,
}

impl Live {
    /// Start the survey thread, its reads stamped in ms since `run`.
    fn start(run: std::time::Instant) -> Result<Self, Fail> {
        let (tx, surveyed) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("git-starts-survey".into())
            .spawn(move || {
                let mut next = std::time::Instant::now();
                loop {
                    let rows = survey_pass(aterm_ctl::local_instances(), instance_survey);
                    if tx.send((ms_since(run), rows)).is_err() {
                        return;
                    }
                    let now = std::time::Instant::now();
                    // A late read moves the schedule on rather than bunching the next ones up.
                    next = (next + SURVEY_EVERY).max(now);
                    if crate::stop::sleep(next - now).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| Fail::die(format!("git-starts: the survey thread did not start: {e}")))?;
        Ok(Self { surveyed })
    }
}

impl Readers for Live {
    fn pids(&mut self) -> Option<Vec<u32>> {
        aterm_uds::holders::uid_pids().ok()
    }

    fn process(&mut self, pid: u32) -> Option<Seen> {
        use aterm_uds::process::{Birth, read_process_birth, read_process_path};
        let pid = i32::try_from(pid).ok()?;
        let birth = read_process_birth(pid)?;
        let path = read_process_path(pid)?;
        // The path is the process's whose birth was read only if that birth still reads.
        (read_process_birth(pid)? == birth).then(|| Seen {
            birth: Birth::from(birth).0,
            git: path.file_name().is_some_and(|name| name == "git"),
        })
    }

    fn survey(&mut self) -> Option<(u64, Option<Vec<Row>>)> {
        self.surveyed.try_recv().ok()
    }
}

/// One survey read of every local aterm `instances` lists, each read with `read`: their rows
/// together, or `None` where the listing failed or an aterm that accepts on its socket did not
/// answer, so a session is never taken as started during the run for an answer its aterm missed.
/// A socket that refuses the connection, or whose token is gone, is taken as an ended aterm's and
/// skipped.
#[must_use]
pub fn survey_pass<E, T>(
    instances: Result<Vec<(u32, String)>, E>,
    mut read: impl FnMut(&str) -> std::io::Result<Vec<T>>,
) -> Option<Vec<T>> {
    use std::io::ErrorKind;
    let mut rows = Vec::new();
    for (_, sock) in instances.ok()? {
        match read(&sock) {
            Ok(more) => rows.extend(more),
            Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {}
            Err(_) => return None,
        }
    }
    Some(rows)
}

/// One instance's session rows, its `survey facts` read within [`SURVEY_WITHIN`]: the facts level
/// carries each row's sid, `program_kind=` and `agent=`, and no screen excerpt or command line.
/// Connecting and the `AUTH` line need no bound: a listener whose queue is full refuses at once,
/// and the line fits a new connection's empty buffer.
fn instance_survey(sock: &str) -> std::io::Result<Vec<Row>> {
    instance_body(sock, "survey facts").map(|body| rows_of(&body))
}

/// The folders the agent sessions of one `survey content` reply's body work in: each agent row's
/// `work_tree=` where the host read one, else its `cwd=`, percent-decoded, a leading `~` taken as
/// `home`; a row naming neither, or a relative path, gives none.
#[must_use]
pub fn folders_of(body: &str, home: &std::path::Path) -> Vec<std::path::PathBuf> {
    body.lines()
        .filter_map(|line| line.strip_prefix("session "))
        .filter_map(|fields| {
            let mut agent = false;
            let (mut tree, mut cwd) = (None, None);
            for (key, value) in fields.split(' ').filter_map(|kv| kv.split_once('=')) {
                let value = (value != "-").then(|| aterm_codec::pct::decode(value));
                match key {
                    "program_kind" => agent = value.as_deref() == Some("agent"),
                    "work_tree" => tree = value,
                    "cwd" => cwd = value,
                    _ => {}
                }
            }
            let place = tree.or(cwd).filter(|_| agent)?;
            let path = match place.strip_prefix('~') {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                    home.join(rest.trim_start_matches('/'))
                }
                _ => std::path::PathBuf::from(place),
            };
            path.is_absolute().then_some(path)
        })
        .collect()
}

/// What `--read`'s work record reads came to ([`Reads::summary`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reads {
    /// The git starts and the wall time, in ms, of each read that gave a record.
    pub read: Vec<(u32, u64)>,
    /// Reads with nothing to show, and reads that did not finish.
    pub none: usize,
    pub unread: usize,
}

impl Reads {
    /// Fold one read's answer, which took `wall_ms`.
    pub fn fold(&mut self, read: &aterm_agent::harness::work_record::WorkRead, wall_ms: u64) {
        use aterm_agent::harness::work_record::WorkRead;
        match read {
            WorkRead::Read(record) => self.read.push((record.starts, wall_ms)),
            WorkRead::None => self.none += 1,
            WorkRead::Unread => self.unread += 1,
        }
    }

    /// The summary lines: how many reads gave a record, the git starts per read (fewest, median,
    /// most) and its wall time (the nearest-rank median and 95th percentile, and the most).
    #[must_use]
    pub fn summary(&self) -> String {
        let mut starts: Vec<f64> = self.read.iter().map(|(n, _)| f64::from(*n)).collect();
        let mut wall: Vec<f64> = self.read.iter().map(|(_, ms)| *ms as f64).collect();
        starts.sort_by(f64::total_cmp);
        wall.sort_by(f64::total_cmp);
        format!(
            "reads: {} gave a record, {} had nothing to show, {} did not finish\n\
             git starts per read: {:.0} fewest, {:.0} median, {:.0} most\n\
             wall time per read: p50 {:.0} ms, p95 {:.0} ms, {:.0} ms most\n",
            self.read.len(),
            self.none,
            self.unread,
            starts.first().copied().unwrap_or(0.0),
            percentile(&starts, 0.5),
            starts.last().copied().unwrap_or(0.0),
            percentile(&wall, 0.5),
            percentile(&wall, 0.95),
            wall.last().copied().unwrap_or(0.0),
        )
    }
}

/// `--read`: read the work record of each local agent session's folder `each` times, one after
/// another, under the probe's rules and the person's home, and print what they started and took.
/// It names no folder. The folders come from one `survey content` of each local aterm.
fn read_each(each: u32) -> Res {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| Fail::die("git-starts --read: HOME is not set"))?;
    let folders = survey_pass(aterm_ctl::local_instances(), |sock| {
        instance_body(sock, "survey content").map(|body| folders_of(&body, &home))
    })
    .ok_or_else(|| Fail::die("git-starts --read: a local aterm did not answer its survey"))?;
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from);
    let mut reads = Reads::default();
    for dir in &folders {
        for _ in 0..each {
            let at = std::time::Instant::now();
            let read = aterm_agent::harness::repo_work::read_work(
                dir,
                Some(&home),
                xdg.as_deref(),
                aterm_agent::harness::repo_work::PROBE_LIMIT,
                &|| false,
            );
            reads.fold(&read, ms_since(at));
            crate::stop::check()?;
        }
    }
    println!(
        "git-starts --read: {} agent sessions' folders, each read {each} times",
        folders.len()
    );
    print!("{}", reads.summary());
    Ok(0)
}

/// One instance's reply body to `request`, read within [`SURVEY_WITHIN`].
fn instance_body(sock: &str, request: &str) -> std::io::Result<String> {
    let mut client = aterm_agent::RelayClient::connect_local_beside(sock)?.bounded(SURVEY_WITHIN);
    let (head, body) = client.request_counted(request)?;
    if head.starts_with("OK") {
        Ok(body)
    } else {
        Err(std::io::Error::other(head))
    }
}

/// Milliseconds since `run`.
fn ms_since(run: std::time::Instant) -> u64 {
    u64::try_from(run.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// `lab git-starts [--minutes N] [--every-ms M]`: sample for N minutes (10), listing this uid's
/// processes every M ms (100), and print the [`Summary`]. An interrupt prints what was sampled.
/// `lab git-starts --read [--reads K]`: read each local agent session's work record K times (5)
/// and print [`Reads::summary`].
/// # Errors
/// A usage error, the survey thread not starting, no listing read, an interrupt, or no survey
/// read that every local aterm answered (after the summary).
pub fn cmd_git_starts(args: &[String]) -> Res {
    crate::stop::install();
    let mut a = args.to_vec();
    if crate::instance::take_flag(&mut a, "--read") {
        let each = match take_value(&mut a, "--reads")? {
            Some(v) => count("--reads", &v)?,
            None => 5,
        };
        if let Some(extra) = a.first() {
            return Err(Fail::usage(format!(
                "git-starts --read: unexpected '{extra}' ({LAB} help)"
            )));
        }
        return read_each(each);
    }
    let minutes = match take_value(&mut a, "--minutes")? {
        Some(v) => count("--minutes", &v)?,
        None => 10,
    };
    let every_ms = match take_value(&mut a, "--every-ms")? {
        Some(v) => count("--every-ms", &v)?,
        None => 100,
    };
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "git-starts: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let run = std::time::Instant::now();
    let end = crate::stop::deadline_after(Duration::from_secs(60 * u64::from(minutes)));
    let every = Duration::from_millis(u64::from(every_ms));
    let mut readers = Live::start(run)?;
    println!(
        "git-starts: reading for {minutes} min: this uid's processes every {every_ms} ms, each local aterm's survey every {} s",
        SURVEY_EVERY.as_secs()
    );
    let mut tally = Tally::default();
    let mut next = run;
    let stopped = loop {
        tally.look(ms_since(run), &mut readers);
        let now = std::time::Instant::now();
        if now >= end {
            break None;
        }
        // A late listing moves the schedule on rather than bunching the next ones up.
        next = (next + every).max(now);
        if let Err(stopped) = crate::stop::sleep(next - now) {
            break Some(stopped);
        }
    };
    let summary = tally.summary();
    if summary.listings == 0 {
        return Err(Fail::die(
            "git-starts: no listing of this uid's processes could be read",
        ));
    }
    if stopped.is_some() {
        println!("git-starts: cut short; the numbers are the minutes sampled");
    }
    print!("{summary}");
    if let Some(stopped) = stopped {
        return Err(stopped.into());
    }
    if summary.surveys == 0 {
        return Err(Fail::die(format!(
            "git-starts: no survey read was answered by every local aterm ({} unanswered): the turn ends and the record's projection measured nothing",
            summary.unanswered
        )));
    }
    Ok(0)
}

#[cfg(test)]
#[path = "git_starts_tests.rs"]
mod tests;

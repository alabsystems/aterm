// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side session log (`docs/DESIGN-session-log-2026-10-15.md`): one [`aterm_slog::Log`] per session under [`crate::session_timeline::SessionTimeline`].

use std::collections::BTreeMap;
use std::sync::Arc;

use aterm_slog::{
    Actor, Amend, Close, Conf, Cursor, Fields, Gap, GapWhy, IdCounters, Key, Kind, Limits, Log,
    Lvl, Mark, Node, Open, Out, Rec, RunOut, SessionOut, ShortId, Src, Stamp, Tier, TurnTracker,
    Val, Verdict,
};

/// The floor of every epoch: a test's log (`SessionLog::for_test`) takes it, and no log this build starts in production takes it or anything below [`FIRST_EPOCH`] + 1 ([`Epochs::issue`]).
pub(crate) const FIRST_EPOCH: u32 = 1;

/// THE CEILING OF EVERY EPOCH THIS PROCESS IS TOLD (the review of 6A-2): an epoch read from outside it (a restore leaf's `log_ep`, a keeper tag's `slog_ep`, a claim's recorded epoch, a carried row's) above this is foreign or corrupt and is dropped, never noted, so the allocator keeps 2^20 issues of room below `u32::MAX` and one bad value cannot make every later log of the process share `u32::MAX`.
pub(crate) const EPOCH_CEILING: u32 = u32::MAX - (1 << 20);

/// An epoch read from outside this process ([`EPOCH_CEILING`]), or `None` for one no producer of this build writes: 0 or above the ceiling.
pub(crate) fn told_epoch(ep: u32) -> Option<u32> {
    (FIRST_EPOCH..=EPOCH_CEILING).contains(&ep).then_some(ep)
}

/// THE EPOCH ALLOCATOR: it issues the first epoch of every log this process starts, fresh or uncarried (a spawn's, a restore's, an adoption told nothing of its predecessor's, a crash recovery's) as `max(wall seconds, every epoch this process issued or noted + 1, floor, 2)`, where the floor is a crash recovery's keeper tag plus one ([`Epochs::crash`]) and the noted epochs are the ones this process adopted (a carried successor's, which stays its predecessor's plus one) and the ones its restore leaves recorded (`TerminalLeafRestore::log_ep`, noted before any session starts, [`Epochs::note_leaves`]) or an adopted id's claim recorded (`identity_claim::recorded_epoch_in`, noted once the claim is held, before the id's log starts).
#[derive(Debug, Default)]
pub(crate) struct Epochs {
    last: std::sync::atomic::AtomicU32,
}

impl Epochs {
    /// An allocator that has issued nothing: a new process's.
    pub(crate) const fn new() -> Epochs {
        Epochs {
            last: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// The process's allocator, every production log's.
    pub(crate) fn process() -> &'static std::sync::Arc<Epochs> {
        static PROCESS: std::sync::OnceLock<std::sync::Arc<Epochs>> = std::sync::OnceLock::new();
        PROCESS.get_or_init(|| std::sync::Arc::new(Epochs::new()))
    }

    /// The next epoch at `at`, no lower than `floor`: above every epoch issued or noted, never below the wall clock's Unix seconds or [`FIRST_EPOCH`] + 1.
    pub(crate) fn issue(&self, at: Stamp, floor: u32) -> u32 {
        use std::sync::atomic::Ordering;
        let wall = u32::try_from(at.wall_ms / 1000).unwrap_or(u32::MAX);
        let floor = floor.min(EPOCH_CEILING.saturating_add(1));
        let base = wall.max(floor).max(FIRST_EPOCH + 1);
        let mut last = self.last.load(Ordering::Acquire);
        loop {
            let Some(next) = last.checked_add(1) else {
                aterm_log::warn!(
                    "session log: this process issued every epoch up to the last; a log repeats it"
                );
                return u32::MAX;
            };
            let ep = base.max(next);
            match self
                .last
                .compare_exchange_weak(last, ep, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return ep,
                Err(now) => last = now,
            }
        }
    }

    /// The epoch of a crash recovery (a shell the PTY keeper held after its window died): above the one the keeper's tag names for the crashed window's log (`tag_ep`, `keeper_link::Tag::slog_ep`), and issued as every other ([`Self::issue`]).
    pub(crate) fn crash(&self, tag_ep: Option<u32>, at: Stamp) -> u32 {
        self.issue(
            at,
            tag_ep
                .and_then(told_epoch)
                .map_or(0, |ep| ep.saturating_add(1)),
        )
    }

    /// Record an epoch this process holds without issuing it.
    pub(crate) fn note(&self, ep: u32) {
        if ep > EPOCH_CEILING {
            aterm_log::warn!(
                "session log: an epoch above the ceiling was dropped, not noted ({ep})"
            );
            return;
        }
        self.last.fetch_max(ep, std::sync::atomic::Ordering::AcqRel);
    }

    /// Note every epoch `layout`'s terminal leaves recorded (`TerminalLeafRestore::log_ep`): called before any session of this process starts, so a restored sid's log, an adoption told nothing and a crash recovery all take an epoch above the one its predecessor's log was at when the layout was saved.
    pub(crate) fn note_leaves(&self, layout: &crate::restore::RestoreManifest) {
        if let Some(ep) = layout.max_log_ep() {
            self.note(ep);
        }
    }
}

/// How far past its last issued id each numbered level's counter is carried across a seamless update: the ids the outgoing process may still issue between drawing the manifest and its Commit.
#[cfg(any(unix, test))]
pub(crate) const LOG_ID_RESERVE: u32 = 1 << 12;

/// How far past its last seq a session log's seq count is carried across a seamless update (`SessionRecord::log_seq`, design §2.2): the records the outgoing process may still append between drawing the manifest and its Commit (a verdict mark at every published move, its turns and steps).
pub(crate) const LOG_SEQ_RESERVE: u64 = 1 << 16;
const _: () = assert!(aterm_slog::SEQ_LIMIT + LOG_SEQ_RESERVE <= i64::MAX as u64);

/// What an adopted session's log is told of its predecessor's: the manifest's node-id counters and epoch ([`LogCarry`], `SessionRecord::log_ids`) and seq count (`SessionRecord::log_seq`, 0: none carried, an older producer's).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LogAdopt {
    /// The epoch and node-id counters, with their reserve.
    pub(crate) ids: Option<LogCarry>,
    /// The seq count, with its reserve: the successor's first seq is above it.
    pub(crate) seq: u64,
    /// The carried log (the control sidecar's `log` member, `handoff_carry::ControlCarry::take_log`): `None` when the update carried none, and the log starts over.
    pub(crate) lines: Option<Vec<String>>,
    /// For a shell the PTY keeper held through a crash: the epoch its crashed window's log was at (`keeper_link::Tag::slog_ep`).
    pub(crate) keeper_ep: Option<u32>,
    /// THE BUS MIRROR's cursor and `ms` head in the predecessor's seq space, as its export drew them under the readers' fence (the control sidecar's `log_mirror` member): only for a mirrored log the update did NOT carry, whose successor then says the old space's tail as one exact `gap why=reset`.
    pub(crate) mirror: Option<aterm_slog::Prior>,
}

/// The host's reading of both clocks: wall Unix ms (the clock shell blocks are stamped with, `aterm_types::current_time_ms`) and the process's monotonic ms ([`crate::turn_ledger::now_ms`]).
pub(crate) fn host_stamp() -> Stamp {
    Stamp::new(
        aterm_types::current_time_ms().unwrap_or(0),
        crate::turn_ledger::now_ms(),
    )
}

/// `at` moved back to the wall instant `wall_ms` on both clocks: a block stamp read at `at` becomes the stamp of the moment it names, so a run opened late still measures its `dur_ms` from when it began.
pub(crate) fn stamp_at_wall(at: Stamp, wall_ms: u64) -> Stamp {
    let wall = wall_ms.min(at.wall_ms);
    Stamp::new(wall, at.mono_ms.saturating_sub(at.wall_ms - wall))
}

const fn later(a: Stamp, b: Stamp) -> Stamp {
    if b.wall_ms > a.wall_ms { b } else { a }
}

/// Whether the wall instant `wall_ms`, read at `at`, lies before this process's monotonic clock began (a command a predecessor started before a seamless update): [`stamp_at_wall`] can only clamp it to 0 there, and a `dur_ms` measured from it would be this process's uptime, not the run's.
pub(crate) const fn predates_clock(at: Stamp, wall_ms: u64) -> bool {
    at.wall_ms.saturating_sub(wall_ms) > at.mono_ms
}

/// Where a session log reads the time: the host's two clocks, or a test's.
#[derive(Default)]
pub(crate) struct Clock(Option<Box<dyn Fn() -> Stamp + Send>>);

impl Clock {
    /// The time now.
    pub(crate) fn now(&self) -> Stamp {
        match &self.0 {
            Some(read) => read(),
            None => host_stamp(),
        }
    }

    /// A clock that reads `read` (tests drive time by hand).
    #[cfg(test)]
    pub(crate) fn reading(read: impl Fn() -> Stamp + Send + 'static) -> Clock {
        Clock(Some(Box::new(read)))
    }
}

/// What a seamless update carries of a session's log: its epoch and its node-id counters (with the reserve).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogCarry {
    /// The outgoing process's epoch.
    pub(crate) ep: u32,
    /// The counters the successor continues from.
    pub(crate) ids: IdCounters,
}

impl LogCarry {
    /// The handoff record's spelling.
    #[cfg(any(unix, test))]
    pub(crate) fn row(self) -> String {
        format!(
            "{} {} {} {}",
            self.ep, self.ids.run, self.ids.turn, self.ids.step
        )
    }

    /// A carried row read back, or `None`: four integers, an epoch of at least one and at most [`EPOCH_CEILING`] ([`told_epoch`]).
    pub(crate) fn parse(row: &str) -> Option<LogCarry> {
        let mut it = row.split_ascii_whitespace().map(str::parse::<u32>);
        let (ep, run, turn, step) = (
            it.next()?.ok()?,
            it.next()?.ok()?,
            it.next()?.ok()?,
            it.next()?.ok()?,
        );
        if it.next().is_some() {
            return None;
        }
        let ep = told_epoch(ep)?;
        Some(LogCarry {
            ep,
            ids: IdCounters { run, turn, step },
        })
    }
}

/// How a block the status sweep read stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockEnd {
    Running,
    Done(crate::session_status::CompletedBlock),
    Unended { ended_ms: Option<u64> },
}

/// One block that ran a command, as the sweep read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockFact {
    /// The block id (per session, rising).
    pub(crate) id: u64,
    /// Its OSC 133 C stamp (wall ms), if stamped.
    pub(crate) started_ms: Option<u64>,
    /// How it stands.
    pub(crate) end: BlockEnd,
    /// Its command reduced to the program ([`crate::session_status::command_detail`]): the line itself is never stored.
    pub(crate) detail: Option<String>,
}

/// What the status sweep read of the shell's blocks under its one terminal guard ([`crate::session_status::block_read`]): the blocks that ran a command from the log's [`SessionLog::block_cursor`] on, OLDEST first, at most [`crate::session_status::BLOCK_READ_MAX`] of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockRead {
    /// The blocks, oldest first.
    pub(crate) facts: Vec<BlockFact>,
    /// The newest block id this read covered: every block up to it is in `facts`, ran nothing (and never will: the prompt in front, which may yet run a command, is never covered), finished before the log opened here, or is gone from the terminal (`dropped`).
    pub(crate) through: Option<u64>,
    /// The cursor the read started from ([`SessionLog::block_cursor`]).
    pub(crate) from: Option<u64>,
    /// Block ids from the cursor on that the terminal no longer holds: a RIS cleared its block history, an ED 3 (`clear`) or a Cmd-K erased the history they lay wholly in, or it dropped its oldest before a read reached them.
    pub(crate) dropped: u64,
    /// The terminal's count of commands the shell started (each accepted OSC 133 C, [`aterm_core::terminal::Terminal::started_command_seq`]), which no clear takes back and no swallowed D hides.
    pub(crate) started: Option<u64>,
    /// The blocks from the cursor on, still held, that reached C (executing, finished, or closed by a prompt with no D).
    pub(crate) started_held: u64,
    /// Whether the block at the cursor itself is one of [`Self::started_held`] (the open `cmd` run's, whose C the log has).
    pub(crate) started_at_from: bool,
    /// Of [`Self::started_held`], the ones past where a read cut at its cap stopped: their C's are counted, their blocks not yet read.
    pub(crate) started_unread: u64,
    /// The read stopped at its cap with a block left that may yet be a fact (one that reached C): the sweep owes one more look.
    pub(crate) more: bool,
}

/// One move of the published agent verdict ([`crate::session_timeline::AgentPublication`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct AgentMove<'a> {
    /// The effective word.
    pub(crate) word: &'a str,
    /// Its detail (a box's kind, a wall's reset), unencoded.
    pub(crate) detail: Option<&'a str>,
    /// The publication's revision.
    pub(crate) rev: u64,
    /// The screen it was read from: generation and fingerprint.
    pub(crate) stamp: Option<crate::session_timeline::AgentStamp>,
    /// The reader that read it.
    pub(crate) reader: Option<aterm_phase::Program>,
    /// The foreground group it was read under.
    pub(crate) pgid: i32,
}

#[derive(Clone, Copy, Debug)]
struct CmdRun {
    blk: u64,
    node: Node,
    wall_from: Option<u64>,
    child_end: Option<Stamp>,
}

#[derive(Clone, Copy, Debug)]
enum CmdEnd {
    Exit(Option<i32>),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CSnap {
    base: u64,
    cursor: Option<u64>,
}

#[derive(Clone, Debug)]
struct PendingName {
    pgid: i32,
    name: Option<String>,
    word: String,
    detail: Option<String>,
    at: Stamp,
    seq: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Typed {
    input: Option<(u64, Actor, Option<ShortId>, Stamp)>,
    ctl: Option<(u64, Actor, u64, Option<ShortId>, Stamp)>,
}

impl Typed {
    fn replay(&self, after: u64, began: Option<Stamp>, log: &mut Log, tracker: &mut TurnTracker) {
        let came =
            |seq: u64, at: Stamp| seq > after || began.is_some_and(|b| at.wall_ms > b.wall_ms);
        let input = self.input.filter(|&(seq, _, _, at)| came(seq, at));
        let ctl = self.ctl.filter(|&(seq, _, _, _, at)| came(seq, at));
        let input_last = match (input, ctl) {
            (Some((i, ..)), Some((c, ..))) => i > c,
            _ => true,
        };
        let exchange = |log: &mut Log, tracker: &mut TurnTracker| {
            if let Some((_, by, ledger, cause, at)) = ctl {
                let _ = tracker.ctl_turn_caused(log, by, ledger, cause, at);
            }
        };
        if input_last {
            exchange(log, tracker);
        }
        if let Some((_, by, cause, at)) = input {
            tracker.note_input_caused(by, cause, at);
        }
        if !input_last {
            exchange(log, tracker);
        }
    }
}

const PENDING_NAMES_MAX: usize = 8;

const AGENT_CMDS_MAX: usize = 4;

#[derive(Clone, Copy, Debug)]
struct FgRun {
    pgid: i32,
    node: Node,
    name_fp: u64,
}

fn name_fp(name: Option<&str>) -> u64 {
    name.map_or(0, |n| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in n.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h.max(1)
    })
}

#[derive(Debug)]
struct AgentRun {
    pgid: i32,
    node: Node,
    who: aterm_phase::Program,
    by_screen: bool,
    tracker: TurnTracker,
}

/// One session's log and the run and turn state its producers keep beside it.
pub(crate) struct SessionLog {
    log: Log,
    clock: Clock,
    epochs: std::sync::Arc<Epochs>,
    consumers: mark::Consumers,
    origin: Kind,
    opened_wall: Option<u64>,
    cmd: Option<CmdRun>,
    last_blk: Option<u64>,
    c_snap: Option<CSnap>,
    cmd_end: Option<Stamp>,
    root_pgid: i32,
    fg: Option<FgRun>,
    agent: Option<AgentRun>,
    agent_cmds: Vec<Node>,
    pending: Vec<PendingName>,
    arrivals: u64,
    typed: Typed,
    ceiling: Option<IdCounters>,
    seq_ceiling: Option<u64>,
    restored_from: Option<ShortId>,
    lineage: Option<ShortId>,
    journal_start: Option<u64>,
    mirror_shared: Option<std::sync::Arc<crate::session_log_mirror::Shared>>,
    mirror_seen: u64,
    observed: observed::Observed,
    person: Option<std::sync::Arc<crate::human_input::HumanInputStamp>>,
    person_seen: u64,
    reached_seen: u64,
    marked_through: u64,
    rollout: transcript::TapAnswers,
    /// This log is the one a seamless update's predecessor wrote, carried and read ([`Self::adopt`]), not one that started over after a gap.
    carried: bool,
}

/// A person's gesture as [`SessionLog::person_input`] takes it from the seam (`crate::app_input::note_person_reaching`): the number of the gesture when it opened a burst (`HumanInputStamp::note_reaching`), and whether it reached the program ([`crate::human_input::reaches_program`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PersonGesture {
    pub(crate) burst: Option<u64>,
    pub(crate) reaches: bool,
}

/// A session ring's bounds: [`Limits::SESSION`], or, in a development build only, `ATERM_SLOG_RING=<records>,<bytes>` (`aterm_types::dev_seam!`, `None` in a release build), read once: the bus mirror's world tests reach an eviction in a handful of commands with it.
pub(crate) fn ring_limits() -> Limits {
    static RING: std::sync::OnceLock<Limits> = std::sync::OnceLock::new();
    *RING.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_SLOG_RING")
            .and_then(|v| {
                let (records, bytes) = v.to_str()?.split_once(',')?;
                Limits::new(records.parse().ok()?, bytes.parse().ok()?)
            })
            .unwrap_or(Limits::SESSION)
    })
}

impl Default for SessionLog {
    fn default() -> Self {
        SessionLog::starting(std::sync::Arc::clone(Epochs::process()), host_stamp())
    }
}

impl SessionLog {
    /// A fresh log whose first epoch `epochs` issues at `at` ([`Epochs::issue`]); a later adoption's comes from it too.
    pub(crate) fn starting(epochs: std::sync::Arc<Epochs>, at: Stamp) -> SessionLog {
        let ep = epochs.issue(at, 0);
        SessionLog::at_epoch(epochs, ep)
    }

    /// THE TEST CONSTRUCTOR: a fresh log at [`FIRST_EPOCH`], with an allocator of its own (a simulated process that issued nothing else), so a unit test that sets its clock by hand reads `ep=1` and an adoption it drives takes its hand clock's seconds.
    #[cfg(test)]
    pub(crate) fn for_test() -> SessionLog {
        let epochs = std::sync::Arc::new(Epochs::new());
        epochs.note(FIRST_EPOCH);
        SessionLog::at_epoch(epochs, FIRST_EPOCH)
    }

    fn at_epoch(epochs: std::sync::Arc<Epochs>, ep: u32) -> SessionLog {
        let mut log = Log::with_limits(ep, ring_limits());
        log.set_notify(crate::session_log_journal::notify());
        log.keep_rows(crate::session_timeline_view::row_span, 0);
        SessionLog {
            log,
            clock: Clock::default(),
            epochs,
            consumers: mark::Consumers::default(),
            origin: Kind::Spawn,
            opened_wall: None,
            cmd: None,
            last_blk: None,
            c_snap: None,
            cmd_end: None,
            root_pgid: 0,
            fg: None,
            agent: None,
            agent_cmds: Vec::new(),
            pending: Vec::new(),
            arrivals: 0,
            typed: Typed::default(),
            ceiling: None,
            seq_ceiling: None,
            restored_from: None,
            lineage: None,
            journal_start: None,
            mirror_shared: None,
            mirror_seen: 0,
            observed: observed::Observed::default(),
            person: None,
            person_seen: 0,
            reached_seen: 0,
            marked_through: 0,
            rollout: transcript::TapAnswers::default(),
            carried: false,
        }
    }
}

const fn run_out(code: Option<i32>) -> (RunOut, Conf) {
    match code {
        Some(0) => (RunOut::Ok, Conf::Exact),
        Some(_) => (RunOut::Fail, Conf::Exact),
        None => (RunOut::Unknown, Conf::Strong),
    }
}

/// The actor an `ExitActor` names.
pub(crate) fn exit_actor(actor: &crate::session_store::ExitActor) -> Actor {
    use crate::session_store::ExitActor;
    match actor {
        ExitActor::Sid(sid) => sid_actor(sid).unwrap_or(Actor::Nobody),
        ExitActor::Human => Actor::Human,
        ExitActor::Ctl => Actor::Ctl,
        #[cfg(any(unix, test))]
        ExitActor::Bridge => Actor::Bridge,
        ExitActor::Unknown => Actor::Nobody,
    }
}

/// The session `sid` names as the log's actor: a minted sid's ([`aterm_session::is_minted_sid`]), else none (a name someone chose is never a session's, and never a trusted actor).
pub(crate) fn sid_actor(sid: &str) -> Option<Actor> {
    aterm_session::is_minted_sid(sid)
        .then(|| ShortId::new(sid))
        .flatten()
        .map(Actor::sid)
}

/// The session outcome an `ExitReason` names.
pub(crate) const fn exit_out(reason: crate::session_store::ExitReason) -> SessionOut {
    use crate::session_store::ExitReason;
    match reason {
        ExitReason::ShellExit => SessionOut::ShellExit,
        ExitReason::CtlClose => SessionOut::CtlClose,
        ExitReason::UiClose => SessionOut::UiClose,
        ExitReason::WindowClose => SessionOut::WindowClose,
        #[cfg(test)]
        ExitReason::AppQuit => SessionOut::AppQuit,
        ExitReason::Unknown => SessionOut::Unknown,
    }
}

fn with(mut f: Fields, key: Key, val: Option<Val>) -> Fields {
    if let Some(val) = val {
        let _ = f.set(key, val);
    }
    f
}

fn tok(key: Key, raw: &str) -> Option<Val> {
    aterm_slog::BoundedToken::new(key, raw).map(Val::Tok)
}

impl SessionLog {
    /// The log.
    pub(crate) const fn log(&self) -> &Log {
        &self.log
    }

    /// The log, for the journal writer's one write into it: installing its wake on a log that has none (a log made before the lane published it).
    pub(crate) const fn log_mut_for_journal(&mut self) -> &mut Log {
        &mut self.log
    }

    /// The log, for the bus mirror's take, landing and close (`session_log_mirror`, phase 4): the writes a bridge makes into a log, all of the mirror's own state (`aterm_slog::Log::bus_copy`, `aterm_slog::Log::land`), under the timeline's lock.
    pub(crate) fn mirror_log(&mut self) -> &mut Log {
        self.sync_mirror();
        &mut self.log
    }

    /// The log, to write records into in a test, never synced with the mirror's state (what a session's own appends are).
    #[cfg(test)]
    pub(crate) fn log_mut_for_tests(&mut self) -> &mut Log {
        &mut self.log
    }

    /// THE STORE'S MIRROR STATE, bound when the store registers the session.
    pub(crate) fn bind_mirror(
        &mut self,
        shared: std::sync::Arc<crate::session_log_mirror::Shared>,
    ) {
        if self
            .mirror_shared
            .as_ref()
            .is_some_and(|s| std::sync::Arc::ptr_eq(s, &shared))
        {
            return;
        }
        self.mirror_shared = Some(shared);
        self.mirror_seen = 0;
    }

    /// THE PERSON'S COUNT, bound when the store registers the session: every gesture counted so far has been delivered (each reached [`Self::person_input`], or found no session to attribute).
    pub(crate) fn bind_person(
        &mut self,
        count: std::sync::Arc<crate::human_input::HumanInputStamp>,
    ) {
        let c = count.counts();
        self.person_seen = c.seq;
        self.reached_seen = c.reached;
        self.marked_through = c.seq;
        self.person = Some(count);
    }

    /// RECONCILE THE PERSON: when the count moved past what this log last delivered, a person's key found the timeline held (`note_person`'s `try_lock`): it is delivered now, before the entry that called this reaches the tracker or writes mail, so it binds the step open now (T1) and stands between mail and the turn after it.
    pub(crate) fn reconcile_person(&mut self, at: Stamp) {
        if self.person.as_ref().is_some_and(|p| {
            let c = p.counts();
            c.seq > self.person_seen || c.reached > self.reached_seen
        }) {
            self.person_input(None, at);
        }
    }

    fn sync_mirror(&mut self) {
        if let Some(shared) = self.mirror_shared.as_deref() {
            crate::session_log_mirror::sync(shared, &mut self.log, &mut self.mirror_seen);
        }
    }

    /// Where this process's journal starts this session's records (after the predecessor's journaled head, `journal_start`), or `None`: from its first.
    pub(crate) const fn journal_start(&self) -> Option<u64> {
        self.journal_start
    }

    /// Put `log` in place of this one, keeping its journal wake: no path that replaces a session's log can leave it unrung.
    pub(crate) fn replace_log(&mut self, log: Log) {
        let notify = self.log.notify().cloned();
        let rows = self.log.row_head();
        self.log = log;
        self.log.set_notify(notify);
        self.log
            .keep_rows(crate::session_timeline_view::row_span, rows);
    }

    /// Continue the timeline's row ids past `carried`, the count a seamless update's manifest carried (`SessionRecord::timeline_id`, which holds the predecessor's overlap reserve): called before the successor's first record ([`crate::session_timeline::SessionTimeline::continue_ids`]).
    pub(crate) const fn continue_rows(&mut self, carried: u64) {
        self.log.continue_rows(carried);
    }

    /// The last timeline row id this log issued, or the count it was continued at (the seamless update's id ceiling; the view's head).
    pub(crate) const fn row_head(&self) -> u64 {
        self.log.row_head()
    }

    /// Whether this log is the carried one a seamless update's predecessor wrote ([`aterm_gui_fabric::Timeline::log_carried`]).
    pub(crate) fn log_carried(&self) -> bool {
        self.carried
    }

    /// The time now, on this log's clock.
    pub(crate) fn now(&self) -> Stamp {
        self.clock.now()
    }

    /// THE CHROME'S CLAUSE (phase 3B, plan §5) at `now_ms` on [`crate::turn_ledger::now_ms`]'s clock: the clause a person's tooltip may show ([`aterm_slog::clause_of_proven`]: a `ctl` label neither shows nor hides the row beneath it) over the state a reader is shown, as the cache's key and the line it composes.
    pub(crate) fn chrome_clause(
        &self,
        now_ms: u64,
    ) -> Option<(
        crate::session_chrome::ClauseKey,
        crate::session_chrome::ClauseLine,
    )> {
        let now_wall = self.now().wall_ms;
        let st = self.log.reader_state();
        let clause = aterm_slog::clause_of_proven(st, now_wall)?;
        let until_ms = (clause.row == aterm_slog::ClauseRow::Label)
            .then(|| aterm_slog::shown_proven_label(st, now_wall).and_then(|l| l.until))
            .flatten()
            .map(|until| now_ms.saturating_add(until.saturating_sub(now_wall)));
        let head = clause.head.as_str().to_string();
        let key = crate::session_chrome::ClauseKey {
            row: clause.row,
            head: head.clone(),
            tone: clause.tone,
            by: clause.by,
            since_wall: now_wall.saturating_sub(clause.age_ms),
        };
        let by = (clause.row == aterm_slog::ClauseRow::Label)
            .then(|| crate::session_chrome::ClauseAuthor::of(&clause.by))
            .flatten();
        let line = crate::session_chrome::ClauseLine {
            head,
            since_ms: now_ms.saturating_sub(clause.age_ms),
            until_ms,
            by,
        };
        Some((key, line))
    }

    /// Replace the clock (tests).
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    /// The instant the turn tracker wants [`Self::tick`] at (monotonic ms), if a settle is pending.
    pub(crate) fn wake(&self) -> Option<u64> {
        self.agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node))
            .and_then(|a| a.tracker.wake_at())
    }

    fn resync(&mut self) {
        let log = &self.log;
        if self.agent.as_ref().is_some_and(|a| !log.is_open(a.node)) {
            self.agent = None;
        }
        if self.fg.is_some_and(|f| !log.is_open(f.node)) {
            self.fg = None;
        }
        if self.cmd.is_some_and(|c| !log.is_open(c.node)) {
            self.cmd = None;
        }
    }

    /// This session came across a seamless update (or out of the PTY keeper's custody, `keeper`): its log starts over, with the loss said first.
    pub(crate) fn adopt(&mut self, carry: LogAdopt, keeper: bool, build: Option<u64>, at: Stamp) {
        // A carry that reads continues the predecessor's log as it stood.
        if !keeper && let (Some(lines), Some(ids)) = (carry.lines.as_deref(), carry.ids) {
            match self.adopt_carried(lines, ids, carry.seq, build, at) {
                Ok(_) => return,
                Err(why) => aterm_log::warn!(
                    "update adopt: a session's carried log did not read ({}); it starts over \
                     after a handoff gap",
                    why.why()
                ),
            }
        }
        self.reset_producers();
        self.carried = false;
        let next_seq = carry.seq.saturating_add(1);
        let log = match (keeper, carry.ids) {
            (true, _) => {
                let ep = self.epochs.crash(carry.keeper_ep, at);
                let mut log = Log::with_limits(ep, ring_limits());
                log.gap_built(GapWhy::Crash, 0, build, at);
                log
            }
            (false, Some(c)) => {
                let ep = c.ep.saturating_add(1);
                self.epochs.note(ep);
                Log::after_handoff_built(ep, ring_limits(), c.ids, next_seq, build, at)
            }
            (false, None) => Log::after_handoff_built(
                self.epochs.issue(at, 0),
                ring_limits(),
                IdCounters::default(),
                next_seq,
                build,
                at,
            ),
        };
        self.replace_log(log);
        if !keeper && let Some(prior) = carry.mirror {
            self.log.note_prior_space(prior);
        }
        self.origin = Kind::Adopt;
    }

    /// THE CARRIED BLOCK CURSOR MEETS ITS ENGINE: called once the adopted session's engine is hydrated, before its reader starts, with that engine's next block id and C count (`aterm_core::terminal::Terminal::next_block_id`, `started_command_seq`).
    pub(crate) fn fit_engine(&mut self, next_block: u64, started: u64, at: Stamp) {
        let fits = self.cmd.is_none_or(|c| c.blk < next_block)
            && self.last_blk.is_none_or(|l| l < next_block)
            && self
                .c_snap
                .is_none_or(|s| s.base <= started && s.cursor.is_none_or(|c| c <= next_block));
        if fits {
            return;
        }
        aterm_log::warn!(
            "update adopt: a session's carried block cursor is past its engine's blocks (the \
             screen did not cross); its commands are read from the engine's own"
        );
        self.resync();
        if let Some(c) = self.cmd.take() {
            self.end_cmd(c, CmdEnd::Unknown, at);
        }
        self.last_blk = None;
        self.c_snap = None;
        self.opened_wall = Some(at.wall_ms);
        self.log.gap(GapWhy::Evicted, 0, at);
    }

    fn reset_producers(&mut self) {
        self.cmd = None;
        self.fg = None;
        self.agent = None;
        self.agent_cmds.clear();
        self.pending.clear();
        self.typed = Typed::default();
        self.last_blk = None;
        self.c_snap = None;
        self.cmd_end = None;
        self.ceiling = None;
        self.seq_ceiling = None;
    }

    /// This session is a restore's new shell, standing in for `prev` (the session its leaf was, when the leaf names one), whose chain of restores began at `lineage` (`prev` itself when none is named).
    pub(crate) fn note_restored(&mut self, prev: Option<&str>, lineage: Option<&str>, at: Stamp) {
        let prev = prev.and_then(ShortId::new);
        self.lineage = lineage.and_then(ShortId::new).or(prev);
        if self.opened_wall.is_none() {
            self.origin = Kind::Restore;
            self.restored_from = prev;
            return;
        }
        let Some(prev) = prev else {
            return;
        };
        let f = with(Fields::new(), Key::Prev, Some(Val::Id(prev)));
        let f = with(f, Key::Lineage, self.lineage.map(Val::Id));
        let _ = self.log.amend(
            Node::SESSION,
            Amend::new(Kind::Restore, Src::Aterm, Conf::Exact, f),
            at,
        );
    }

    /// The lineage a seamless update's leaf names for the shell it hands over (the same session, so no record): kept unless this log already has one.
    pub(crate) fn seed_lineage(&mut self, lineage: Option<&str>) {
        if self.lineage.is_none() {
            self.lineage = lineage.and_then(ShortId::new);
        }
    }

    /// The first session of the chain of restores this one continues, or `None` when it began it.
    pub(crate) const fn lineage(&self) -> Option<ShortId> {
        self.lineage
    }

    /// `open - spawn|adopt|restore parent=<sid> epoch=<ep> state=<w>`: the registry took the session in, in lifecycle state `state`.
    pub(crate) fn open_session(&mut self, parent: Option<&str>, state: Option<&str>, at: Stamp) {
        let f = with(
            Fields::new(),
            Key::Parent,
            parent.and_then(ShortId::new).map(Val::Id),
        );
        let f = with(f, Key::Epoch, Some(Val::U(u64::from(self.log.ep()))));
        let f = with(f, Key::Prev, self.restored_from.take().map(Val::Id));
        let f = with(
            f,
            Key::Lineage,
            (self.origin == Kind::Restore)
                .then_some(self.lineage)
                .flatten()
                .map(Val::Id),
        );
        let said = |f: Fields| match state {
            Some(w) => f.clone().word(Key::State, w).unwrap_or(f),
            None => f,
        };
        match self.log.open_session(
            self.origin,
            Src::Aterm,
            Conf::Exact,
            Actor::Nobody,
            said(f),
            at,
        ) {
            Ok(_) => self.opened_wall = Some(at.wall_ms),
            Err(aterm_slog::Refused::AlreadyOpen) if state.is_some() => {
                let f = said(Fields::new());
                let f = f.clone().u(Key::Carried, 1).unwrap_or(f);
                let _ = self.log.mark(
                    Mark::new(Kind::State, Src::Aterm, Conf::Exact)
                        .at(Node::SESSION)
                        .fields(f),
                    at,
                );
            }
            Err(_) => {}
        }
    }

    /// `close - out=<reason> by=<actor> code=<n> closer=<raw>`: the registry let it go.
    pub(crate) fn close_session(
        &mut self,
        out: SessionOut,
        by: Actor,
        closer: &str,
        code: Option<i32>,
        at: Stamp,
    ) {
        self.name_pending(self.cmd, None);
        self.flush_program(at);
        self.flush_shown(at);
        let f = with(Fields::new(), Key::Code, code.map(|c| Val::I(i64::from(c))));
        let f = with(
            f,
            Key::Closer,
            (closer != "-")
                .then(|| aterm_slog::BoundedText::new(Key::Closer, closer))
                .flatten()
                .map(Val::Text),
        );
        let close = Close::new(Out::Session(out), Src::Aterm, Conf::Exact)
            .by(by)
            .fields(f);
        let _ = self.log.close_session(close, at);
        self.cmd = None;
        self.fg = None;
        self.agent = None;
        let _ = self.take_close_due();
    }

    /// The first block id the sweep's next block read must report: the open `cmd` run's block, else the one past the newest the log handled (`None`: every block the terminal holds).
    pub(crate) fn block_cursor(&self) -> Option<u64> {
        self.cmd
            .map(|c| c.blk)
            .or(self.last_blk.map(|b| b.saturating_add(1)))
    }

    /// When the session node opened here (wall ms): a block that finished before it is another process's, which the block read passes over without counting it against its cap.
    pub(crate) const fn opened_wall(&self) -> Option<u64> {
        self.opened_wall
    }

    /// The status sweep's block read: OSC 133 C opens a `cmd` run (dated at its own stamp, `detail=` the program), D closes it with `out=`, `code=` and `dur_ms`, and a block that ran and finished between two sweeps gets both records from its stamps (`dur=wall`).
    pub(crate) fn shell_blocks(&mut self, read: &BlockRead, front: Option<i32>, at: Stamp) {
        self.resync();
        if self.lost_unread(read) > 0 {
            self.log.gap(GapWhy::Evicted, 0, at);
        }
        for fact in &read.facts {
            match (self.cmd.filter(|c| c.blk == fact.id), fact.end) {
                (Some(_), BlockEnd::Running) => {}
                (Some(c), BlockEnd::Done(done)) => {
                    let end = stamp_at_wall(at, done.ended_ms.unwrap_or(at.wall_ms));
                    self.cmd = None;
                    self.end_cmd(c, CmdEnd::Exit(done.exit_code), end);
                }
                (Some(c), BlockEnd::Unended { ended_ms }) => {
                    let end = stamp_at_wall(at, ended_ms.unwrap_or(at.wall_ms));
                    self.cmd = None;
                    self.end_cmd(c, CmdEnd::Unknown, end);
                }
                (None, _) if self.last_blk.is_some_and(|l| fact.id <= l) => {}
                (None, end) => {
                    // A newer block: the open one ended unseen, before this
                    // one began.
                    if let Some(c) = self.cmd.take() {
                        let began = stamp_at_wall(at, fact.started_ms.unwrap_or(at.wall_ms));
                        self.end_cmd(c, CmdEnd::Unknown, began);
                    }
                    self.open_names_before(fact, front);
                    self.new_block(fact, end, front, at);
                    self.last_blk = Some(fact.id);
                }
            }
        }
        if let Some(c) = self.cmd
            && !read.facts.iter().any(|f| f.id == c.blk)
        {
            self.cmd = None;
            self.end_cmd(c, CmdEnd::Unknown, at);
        }
        if !read.more {
            self.name_pending(self.cmd, None);
        }
        if let Some(t) = read.through {
            self.last_blk = Some(self.last_blk.map_or(t, |l| l.max(t)));
        }
        if let Some(started) = read.started {
            self.c_snap = Some(CSnap {
                base: started.saturating_sub(read.started_unread),
                cursor: self.block_cursor(),
            });
        }
    }

    fn lost_unread(&self, read: &BlockRead) -> u64 {
        let own = self.cmd.is_some_and(|c| read.from == Some(c.blk));
        let at_gone_run = |snap: CSnap| self.cmd.is_none() && snap.cursor == self.last_blk;
        match (self.c_snap, read.started) {
            (Some(snap), Some(now)) if snap.cursor == read.from || at_gone_run(snap) => {
                let held = read
                    .started_held
                    .saturating_sub(u64::from(own && read.started_at_from));
                now.saturating_sub(snap.base).saturating_sub(held)
            }
            _ => read.dropped.saturating_sub(u64::from(own)),
        }
    }

    fn after_last_cmd(&self, start: Stamp) -> Stamp {
        self.cmd_end.map_or(start, |end| later(start, end))
    }

    fn open_names_before(&mut self, fact: &BlockFact, front: Option<i32>) {
        let (Some(start), Some(front)) = (fact.started_ms, front) else {
            return;
        };
        let runs = match fact.end {
            BlockEnd::Running => true,
            BlockEnd::Done(_) => false,
            BlockEnd::Unended { .. } => return,
        };
        while self
            .pending
            .first()
            .is_some_and(|p| p.at.wall_ms < start && (p.pgid == front) != runs)
        {
            let p = self.pending.remove(0);
            self.open_named(None, p);
        }
    }

    fn new_block(&mut self, fact: &BlockFact, end: BlockEnd, front: Option<i32>, at: Stamp) {
        match end {
            BlockEnd::Running => {
                let wall = fact.started_ms.unwrap_or(at.wall_ms);
                let start = self.after_last_cmd(stamp_at_wall(at, wall));
                let wall_from = predates_clock(at, start.wall_ms).then_some(start.wall_ms);
                if let Some(node) = self.open_cmd(fact.id, fact.detail.as_deref(), front, start) {
                    self.cmd = Some(CmdRun {
                        blk: fact.id,
                        node,
                        wall_from,
                        child_end: None,
                    });
                }
            }
            BlockEnd::Done(done) => {
                let end = stamp_at_wall(at, done.ended_ms.unwrap_or(at.wall_ms));
                if !self.opened_wall.is_some_and(|t| end.wall_ms >= t) {
                    return;
                }
                let start =
                    self.after_last_cmd(stamp_at_wall(end, fact.started_ms.unwrap_or(end.wall_ms)));
                let end = later(end, start);
                if let Some(node) = self.open_cmd(fact.id, fact.detail.as_deref(), None, start) {
                    let c = CmdRun {
                        blk: fact.id,
                        node,
                        wall_from: Some(start.wall_ms),
                        child_end: None,
                    };
                    self.end_cmd(c, CmdEnd::Exit(done.exit_code), end);
                }
            }
            BlockEnd::Unended { .. } => {}
        }
    }

    fn open_cmd(
        &mut self,
        blk: u64,
        detail: Option<&str>,
        front: Option<i32>,
        at: Stamp,
    ) -> Option<Node> {
        let f = with(Fields::new(), Key::Blk, Some(Val::U(blk)));
        let f = with(f, Key::Detail, detail.and_then(|d| tok(Key::Detail, d)));
        let holds_front = |node: Node| {
            front.is_none_or(|pgid| {
                self.fg.is_some_and(|f| f.node == node && f.pgid == pgid)
                    || self
                        .agent
                        .as_ref()
                        .is_some_and(|a| a.node == node && a.pgid == pgid)
            })
        };
        let parent = self
            .log
            .open_nodes()
            .iter()
            .rev()
            .find(|e| e.node.lvl == Lvl::Run && e.t <= at.wall_ms && holds_front(e.node))
            .map_or(Node::SESSION, |e| e.node);
        self.log
            .open(
                Open::new(Kind::Cmd, Src::Shell, Conf::Strong)
                    .under(parent)
                    .fields(f),
                at,
            )
            .ok()
    }

    fn end_cmd(&mut self, c: CmdRun, how: CmdEnd, end: Stamp) {
        self.name_pending(Some(c), Some(end));
        self.end_front_beside(c.node, end);
        let front = self.end_front_under(c.node, end);
        let own = self.opened_at(c.node);
        let end = [c.child_end, front, own]
            .into_iter()
            .flatten()
            .fold(end, later);
        let wall = c.wall_from.map(|from| end.wall_ms.saturating_sub(from));
        self.close_cmd(c.node, how, wall, end);
    }

    fn opened_at(&self, node: Node) -> Option<Stamp> {
        self.log.open_entry(node).map(|e| Stamp::new(e.t, e.mono))
    }

    fn close_cmd(&mut self, node: Node, how: CmdEnd, wall_ms: Option<u64>, at: Stamp) {
        let (out, conf, code) = match how {
            CmdEnd::Exit(code) => {
                let (out, conf) = run_out(code);
                (out, conf, code)
            }
            CmdEnd::Unknown => (RunOut::Unknown, Conf::Strong, None),
        };
        let f = with(Fields::new(), Key::Code, code.map(|c| Val::I(i64::from(c))));
        let mut close = Close::new(Out::Run(out), Src::Shell, conf).fields(f);
        if let Some(ms) = wall_ms {
            close = close.dur_wall(ms);
        }
        if self.close_node(node, close, at).is_ok() {
            self.cmd_end = Some(self.cmd_end.map_or(at, |e| later(e, at)));
        }
    }

    fn close_node(
        &mut self,
        node: Node,
        close: Close,
        at: Stamp,
    ) -> Result<u64, aterm_slog::Refused> {
        self.settle_program(node, at);
        self.log.close(node, close, at)
    }

    fn descends(&self, node: Node, root: Node) -> bool {
        let mut at = node;
        for _ in 0..=aterm_slog::MAX_OPEN {
            match self.log.open_entry(at) {
                Some(e) if e.parent == root => return true,
                Some(e) => at = e.parent,
                None => return false,
            }
        }
        false
    }

    fn note_agent_cmd(&mut self, node: Node) {
        let mut at = node;
        for _ in 0..=aterm_slog::MAX_OPEN {
            let Some(e) = self.log.open_entry(at) else {
                return;
            };
            if e.kind == Kind::Cmd {
                if !self.agent_cmds.contains(&at) {
                    if self.agent_cmds.len() >= AGENT_CMDS_MAX {
                        self.agent_cmds.remove(0);
                    }
                    self.agent_cmds.push(at);
                }
                return;
            }
            at = e.parent;
        }
    }

    fn end_front_under(&mut self, parent: Node, at: Stamp) -> Option<Stamp> {
        let agent = self
            .agent
            .as_ref()
            .is_some_and(|a| self.descends(a.node, parent));
        let fg = self.fg.filter(|f| self.descends(f.node, parent));
        if !agent && fg.is_none() {
            return None;
        }
        let at = self
            .log
            .open_nodes()
            .iter()
            .filter(|e| self.descends(e.node, parent))
            .map(|e| Stamp::new(e.t, e.mono))
            .fold(at, later);
        if agent {
            self.close_agent(at);
        }
        if let Some(fg) = fg {
            self.close_run(fg.node, RunOut::Unknown, at);
            self.fg = None;
        }
        Some(at)
    }

    fn end_front_beside(&mut self, cmd: Node, end: Stamp) {
        let Some(from) = self.log.open_entry(cmd).map(|e| e.t) else {
            return;
        };
        let beside = |node: Node| {
            !self.descends(node, cmd)
                && !self.descends(cmd, node)
                && self
                    .log
                    .open_entry(node)
                    .is_some_and(|e| from <= e.t && e.t <= end.wall_ms)
        };
        let agent = self.agent.as_ref().map(|a| a.node).filter(|n| beside(*n));
        let fg = self.fg.map(|f| f.node).filter(|n| beside(*n));
        if agent.is_none() && fg.is_none() {
            return;
        }
        let roots = [agent, fg];
        let at = self
            .log
            .open_nodes()
            .iter()
            .filter(|e| {
                roots
                    .into_iter()
                    .flatten()
                    .any(|r| e.node == r || self.descends(e.node, r))
            })
            .map(|e| Stamp::new(e.t, e.mono))
            .fold(end, later);
        if agent.is_some() {
            self.close_agent(at);
        }
        if let Some(node) = fg {
            self.close_run(node, RunOut::Unknown, at);
            self.fg = None;
        }
    }

    /// A failure the person saw (`FailureMark`, the status observer's seen acknowledgement): `mark - seen by=human`, which lifts `need=failure`.
    pub(crate) fn failure_seen(&mut self, at: Stamp) {
        let _ = self.log.mark(
            Mark::new(Kind::Seen, Src::Status, Conf::Strong)
                .at(Node::SESSION)
                .by(Actor::Human),
            at,
        );
    }

    /// A HARNESS RELAUNCH LIFTS THE FAILURE IT CAUSED, AND ONLY THAT ONE.
    pub(crate) fn harness_saw_failure(&mut self, at: Stamp) -> bool {
        let mut failures = self
            .log
            .state()
            .needs
            .iter()
            .flatten()
            .filter(|n| n.word == "failure");
        let (Some(need), None) = (failures.next(), failures.next()) else {
            return false;
        };
        let (n, kind, closed_t) = (need.node, need.kind, need.t);
        if n.lvl != Lvl::Run || self.log.is_open(n) || !self.agent_cmds.contains(&n) {
            return false;
        }
        let relaunched =
            self.log.open_nodes().iter().any(|e| {
                e.node.lvl == Lvl::Run && e.kind == kind && e.node.n > n.n && e.t >= closed_t
            });
        relaunched
            && self
                .log
                .mark(
                    Mark::new(Kind::Seen, Src::Aterm, Conf::Strong)
                        .at(Node::SESSION)
                        .by(Actor::Harness),
                    at,
                )
                .is_ok()
    }

    /// The session's own process group: its shell, which in front is the session at its prompt.
    pub(crate) const fn note_root(&mut self, pgid: i32) {
        self.root_pgid = pgid;
    }

    /// The foreground group moved to `pgid`: the runs of the group that left close (`out=unknown`: leaving the front is not an exit status).
    pub(crate) fn foreground_moved(&mut self, pgid: i32, at: Stamp) {
        self.resync();
        self.name_pending(self.cmd, None);
        if self.agent.as_ref().is_some_and(|a| a.pgid != pgid) {
            self.close_agent(at);
        }
        if let Some(fg) = self.fg.filter(|f| f.pgid != pgid) {
            self.close_run(fg.node, RunOut::Unknown, at);
            self.fg = None;
        }
    }

    /// The group in front, `pgid`, named its program (the resolver's answer, off the sweep): the name waits for the next block read ([`Self::shell_blocks`]), which knows the command it ran under.
    pub(crate) fn program_named(
        &mut self,
        pgid: i32,
        program: Option<&str>,
        word: &str,
        detail: Option<&str>,
        at: Stamp,
    ) {
        if self.pending.len() >= PENDING_NAMES_MAX {
            self.pending.remove(0);
        }
        self.arrivals += 1;
        self.pending.push(PendingName {
            pgid,
            name: program.map(str::to_string),
            word: word.to_string(),
            detail: detail.map(str::to_string),
            at,
            seq: self.arrivals,
        });
    }

    fn name_pending(&mut self, under: Option<CmdRun>, until: Option<Stamp>) {
        while self
            .pending
            .first()
            .is_some_and(|p| until.is_none_or(|u| p.at.wall_ms <= u.wall_ms))
        {
            let p = self.pending.remove(0);
            self.open_named(under, p);
        }
    }

    fn open_named(&mut self, under: Option<CmdRun>, p: PendingName) {
        self.resync();
        let under = under.filter(|c| self.log.is_open(c.node));
        let at = under
            .and_then(|c| self.opened_at(c.node))
            .map_or(p.at, |open| later(p.at, open));
        let (pgid, program, word, detail) = (
            p.pgid,
            p.name.as_deref(),
            p.word.as_str(),
            p.detail.as_deref(),
        );
        let name = program.filter(|n| !n.trim().is_empty());
        let agent = name.and_then(aterm_phase::program_of);
        if let Some(name) = name
            && self
                .agent
                .as_ref()
                .is_some_and(|a| a.pgid == pgid && a.by_screen && Some(a.who) == agent)
        {
            self.confirm_agent(pgid, name, at);
            return;
        }
        if let Some(fg) = self.fg.filter(|f| f.pgid == pgid) {
            // The same group naming the program it already runs: nothing
            // changed (a seamless update's successor, whose resolver names
            // every group again, keeps the run, its agent and its turn).
            if name.is_some() && fg.name_fp == name_fp(name) {
                return;
            }
            if self.agent.as_ref().is_some_and(|a| a.node == fg.node) {
                self.agent = None;
            }
            let out = if program.is_some() {
                RunOut::Superseded
            } else {
                RunOut::Unknown
            };
            self.close_run(fg.node, out, at);
            self.fg = None;
            self.resync();
        }
        let Some(name) = name else {
            return;
        };
        let root = self.root_pgid <= 0 || pgid == self.root_pgid;
        if root && crate::quit_safety::is_shell_name(name) {
            return;
        }
        if let Some(a) = self.agent.as_mut().filter(|a| a.pgid == pgid) {
            let f = with(Fields::new(), Key::Prog, tok(Key::Prog, name));
            let _ = self.log.amend(
                a.node,
                Amend::new(Kind::Agent, Src::Status, Conf::Strong, f),
                at,
            );
            if let Some(fg) = self.fg.as_mut().filter(|f| f.pgid == pgid) {
                fg.name_fp = name_fp(Some(name));
            }
            return;
        }
        let f = with(Fields::new(), Key::Prog, tok(Key::Prog, name));
        let f = with(f, Key::Agent, agent.and_then(|a| tok(Key::Agent, a.name())));
        let kind = if agent.is_some() {
            Kind::Agent
        } else {
            Kind::Prog
        };
        let mut open = Open::new(kind, Src::Status, Conf::Strong).fields(f);
        if let Some(c) = under {
            open = open.under(c.node);
        }
        let Ok(node) = self.log.open(open, at) else {
            return;
        };
        self.fg = Some(FgRun {
            pgid,
            node,
            name_fp: name_fp(Some(name)),
        });
        if let Some(who) = agent {
            let mut tracker = TurnTracker::new(node);
            let _ = tracker.observe(
                &mut self.log,
                Verdict::from_agent_word(word, detail),
                detail,
                at,
            );
            let began = under.and_then(|c| self.opened_at(c.node));
            self.typed.replay(p.seq, began, &mut self.log, &mut tracker);
            self.agent = Some(AgentRun {
                pgid,
                node,
                who,
                by_screen: false,
                tracker,
            });
            self.note_agent_cmd(node);
        }
    }

    fn confirm_agent(&mut self, pgid: i32, name: &str, at: Stamp) {
        let Some(a) = self.agent.as_mut() else {
            return;
        };
        let f = with(Fields::new(), Key::Prog, tok(Key::Prog, name));
        let _ = self.log.amend(
            a.node,
            Amend::new(Kind::Agent, Src::Status, Conf::Strong, f),
            at,
        );
        a.by_screen = false;
        let node = a.node;
        match self.fg.as_mut() {
            Some(fg) if fg.pgid == pgid => fg.name_fp = name_fp(Some(name)),
            _ => {
                self.fg = Some(FgRun {
                    pgid,
                    node,
                    name_fp: name_fp(Some(name)),
                });
            }
        }
    }

    fn close_run(&mut self, node: Node, out: RunOut, at: Stamp) {
        let under_cmd = self.cmd.is_some_and(|c| self.descends(node, c.node));
        let closed = self.close_node(
            node,
            Close::new(Out::Run(out), Src::Status, Conf::Strong),
            at,
        );
        if closed.is_ok()
            && under_cmd
            && let Some(c) = self.cmd.as_mut()
        {
            c.child_end = Some(match c.child_end {
                Some(t) if t.wall_ms >= at.wall_ms => t,
                _ => at,
            });
        }
    }

    fn close_agent(&mut self, at: Stamp) {
        if let Some(a) = self.agent.take() {
            // The named agent's run is the front run: closing it is the
            // front run's to do (`foreground_moved`).
            if self.fg.is_none_or(|f| f.node != a.node) {
                self.close_run(a.node, RunOut::Unknown, at);
            }
        }
    }

    /// The published agent verdict moved: `mark agent word= rev= gen= fp=`, then the turn tracker's transitions.
    pub(crate) fn agent_moved(&mut self, m: AgentMove<'_>, at: Stamp) -> Option<u64> {
        self.reconcile_person(at);
        self.resync();
        self.name_pending(self.cmd, None);
        let named = m.reader.filter(|r| match r {
            aterm_phase::Program::Claude
            | aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse => true,
            aterm_phase::Program::Generic => false,
        });
        if self.agent.is_none()
            && m.word != "-"
            && let Some(reader) = named
        {
            let f = with(Fields::new(), Key::Agent, tok(Key::Agent, reader.name()));
            let mut open = Open::new(Kind::Agent, Src::Screen, Conf::Strong).fields(f);
            if let Some(parent) = self
                .fg
                .filter(|f| f.pgid == m.pgid)
                .map(|f| f.node)
                .or(self.cmd.map(|c| c.node))
            {
                open = open.under(parent);
            }
            if let Ok(node) = self.log.open(open, at) {
                self.agent = Some(AgentRun {
                    pgid: m.pgid,
                    node,
                    who: reader,
                    by_screen: true,
                    tracker: TurnTracker::new(node),
                });
                self.note_agent_cmd(node);
            }
        }
        let word = Fields::new()
            .word(Key::Word, m.word)
            .or_else(|_| Fields::new().word(Key::Word, "unknown"))
            .unwrap_or_default();
        let f = with(word, Key::Rev, Some(Val::U(m.rev)));
        let f = with(
            f,
            Key::Gen,
            m.stamp
                .and_then(|s| ShortId::new(&s.generation.to_string()))
                .map(Val::Id),
        );
        let f = with(
            f,
            Key::Fp,
            m.stamp
                .and_then(|s| ShortId::new(&format!("{:016x}", s.fp)))
                .map(Val::Id),
        );
        let mut mark = Mark::new(Kind::Agent, Src::Screen, Conf::Strong).fields(f);
        if let Some(a) = &self.agent {
            mark = mark.at(a.node);
        }
        let _ = self.log.mark(mark, at);
        let Self { log, agent, .. } = self;
        let wake = agent.as_mut().and_then(|a| {
            a.tracker.observe(
                log,
                Verdict::from_agent_word(m.word, m.detail),
                m.detail,
                at,
            )
        });
        if m.word == "-" && self.agent.as_ref().is_some_and(|a| a.by_screen) {
            self.close_agent(at);
            return None;
        }
        wake
    }

    /// A person's hands were on the session: the tracker attributes a turn that opens within 5 s to them (a run a waiting name opens later too, [`Typed`]).
    pub(crate) fn person_input(&mut self, own: Option<PersonGesture>, at: Stamp) {
        let own_reaches = own.is_some_and(|g| g.reaches);
        let Some(p) = self.person.as_ref() else {
            if let Some(g) = own {
                self.deliver_person(
                    true,
                    g.reaches,
                    (g.burst.is_some(), false),
                    own_reaches,
                    0,
                    at,
                );
            }
            return;
        };
        let c = p.counts();
        let fresh = c.seq > self.person_seen;
        let reached = c.reached > self.reached_seen;
        let burst = own
            .and_then(|g| g.burst)
            .is_some_and(|n| n > self.marked_through);
        let unmarked_burst = c.burst > self.marked_through;
        self.person_seen = self.person_seen.max(c.seq);
        self.reached_seen = self.reached_seen.max(c.reached);
        self.deliver_person(
            fresh,
            reached,
            (burst, unmarked_burst),
            own_reaches,
            c.seq,
            at,
        );
    }

    /// [`Self::person_input`] for a gesture no count names (a test's, or a log no store bound): delivered whole, as one that reached the program, `burst` its own.
    #[cfg(test)]
    pub(crate) fn human_input(&mut self, burst: bool, at: Stamp) {
        let c = self.person.as_ref().map(|p| p.counts()).unwrap_or_default();
        self.person_seen = self.person_seen.max(c.seq);
        self.reached_seen = self.reached_seen.max(c.reached);
        self.deliver_person(true, true, (burst, false), true, c.seq, at);
    }

    fn deliver_person(
        &mut self,
        fresh: bool,
        reached: bool,
        (burst, unmarked_burst): (bool, bool),
        own_reaches: bool,
        seq: u64,
        at: Stamp,
    ) {
        if !fresh && !reached && !burst {
            return;
        }
        self.resync();
        if fresh {
            self.arrivals += 1;
            self.typed.input = Some((self.arrivals, Actor::Human, None, at));
        }
        let Self { log, agent, .. } = self;
        if let Some(a) = agent.as_mut() {
            if fresh {
                a.tracker.note_input(Actor::Human, at);
            }
            if reached {
                let _ = a.tracker.person(log, at);
            }
        }
        let after_mail = reached && self.log.fold().in_since_human();
        if burst || after_mail {
            let a_bursts = burst || unmarked_burst;
            let mut f = Fields::new();
            if !(reached || own_reaches) {
                f = f.u(Key::Reached, 0).unwrap_or_default();
            }
            if !a_bursts {
                f = f.clone().word(Key::Why, "mail").unwrap_or(f);
            }
            let mark = Mark::new(Kind::Human, Src::Input, Conf::Strong)
                .at(Node::SESSION)
                .by(Actor::Human)
                .fields(f);
            let _ = self.log.mark(mark, at);
            if a_bursts {
                self.marked_through = self.marked_through.max(seq);
            }
        }
    }

    /// A `ctl turn` exchange from `by`, ledger id `ledger`, handed its input to the seam (`cmd_turn` calls this at the exchange's first byte and again at each Enter): the turn it opens carries `by=` and `ledger=` (of a run a waiting name opens later too, [`Typed`]).
    pub(crate) fn ctl_turn(
        &mut self,
        by: Actor,
        ledger: u64,
        cause: Option<ShortId>,
        at: Stamp,
    ) -> Option<u64> {
        self.reconcile_person(at);
        self.resync();
        self.arrivals += 1;
        self.typed.ctl = Some((self.arrivals, by, ledger, cause, at));
        let Self { log, agent, .. } = self;
        agent
            .as_mut()
            .and_then(|a| a.tracker.ctl_turn_caused(log, by, ledger, cause, at))
    }

    /// The host's settle wake: a resting word that has held closes its turn.
    pub(crate) fn tick(&mut self, at: Stamp) -> Option<u64> {
        self.reconcile_person(at);
        self.resync();
        let Self { log, agent, .. } = self;
        agent.as_mut().and_then(|a| a.tracker.tick(log, at))
    }

    /// What the handoff manifest carries: the epoch and the node-id counters plus [`LOG_ID_RESERVE`], remembered as this process's ceiling.
    #[cfg(any(unix, test))]
    pub(crate) const fn handoff_carry(&mut self) -> LogCarry {
        let ids = self.log.id_counters();
        let reserved = IdCounters {
            run: ids.run.saturating_add(LOG_ID_RESERVE),
            turn: ids.turn.saturating_add(LOG_ID_RESERVE),
            step: ids.step.saturating_add(LOG_ID_RESERVE),
        };
        self.ceiling = Some(reserved);
        LogCarry {
            ep: self.log.ep(),
            ids: reserved,
        }
    }

    /// Set the log's byte budget (the instance-wide sweep's share, [`crate::App::sweep_session_log_memory`]): it evicts level-aware to it at once, and `usize::MAX` gives the session bound back.
    pub(crate) fn set_log_byte_budget(&mut self, bytes: usize) {
        self.log.set_byte_budget(bytes);
    }

    /// What the handoff manifest carries of the seq count (`SessionRecord::log_seq`): the last seq plus [`LOG_SEQ_RESERVE`], remembered as this process's ceiling.
    #[cfg(any(unix, test))]
    pub(crate) const fn handoff_seq(&mut self) -> u64 {
        let last = self.log.last_seq();
        if last > aterm_slog::SEQ_LIMIT {
            self.seq_ceiling = None;
            return 0;
        }
        let carried = last + LOG_SEQ_RESERVE;
        self.seq_ceiling = Some(carried);
        carried
    }

    /// Whether this process issued a node id or a seq past the counts the last manifest carried: the successor would issue it again, so the Commit stands down.
    #[cfg(any(unix, test))]
    pub(crate) fn past_handoff_ceiling(&self) -> bool {
        self.ceiling.is_some_and(|c| {
            let now = self.log.id_counters();
            now.run > c.run || now.turn > c.turn || now.step > c.step
        }) || self.seq_ceiling.is_some_and(|c| self.log.last_seq() > c)
    }
}

/// THE INSTANCE CAP: every session log's estimated heap together (`aterm_slog::Log::footprint`, about three times what its ring's encoded bytes count), the closed-session index's included.
pub(crate) const LOG_MEMORY_CAP: usize = 64 * 1024 * 1024;
/// How often the event loop's sweep looks: at most once a second, on a turn the loop takes anyway (it arms no deadline of its own).
pub(crate) const SLOG_SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
/// The most logs one pass lowers while the whole is within an eighth over the cap: the rest wait for the next pass.
pub(crate) const SLOG_SWEEP_MAX_SHRINK: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Charge {
    footprint: usize,
    bytes: usize,
    pinned: usize,
    lowered: bool,
    budget: Option<usize>,
}

/// THE SWEEP'S STATE: the cap (a test lowers it), when it looks next, and what it last read of each pooled session's log.
#[derive(Debug)]
pub(crate) struct MemorySweep {
    /// The estimated-heap target for all live logs and the closed index.
    pub(crate) cap: usize,
    due: Option<std::time::Instant>,
    known: BTreeMap<u64, Charge>,
}

impl Default for MemorySweep {
    fn default() -> Self {
        MemorySweep {
            cap: LOG_MEMORY_CAP,
            due: None,
            known: BTreeMap::new(),
        }
    }
}

fn fair_shares(wants: &[usize], budget: usize) -> Vec<usize> {
    if wants.iter().fold(0usize, |a, w| a.saturating_add(*w)) <= budget {
        return wants.to_vec();
    }
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by_key(|&i| wants[i]);
    let mut shares = vec![0; wants.len()];
    let mut left = budget;
    for (k, &i) in order.iter().enumerate() {
        let alike = left / (wants.len() - k);
        shares[i] = wants[i].min(alike);
        left -= shares[i];
    }
    shares
}

impl crate::App {
    /// THE INSTANCE-WIDE SWEEP (design §5.1): hold every session log, and the closed-session index, under [`MemorySweep::cap`].
    pub(crate) fn sweep_session_log_memory(&mut self, now: std::time::Instant) {
        let sweep = &mut self.session_status.slog_memory;
        if sweep.due.is_some_and(|d| now < d) {
            return;
        }
        self.poll_session_journal();
        let sweep = &mut self.session_status.slog_memory;
        sweep.due = Some(now + SLOG_SWEEP_EVERY);
        let cap = sweep.cap.saturating_sub(self.closed_log_charge());
        let mut ids: Vec<u64> = Vec::new();
        for session in self.pool.iter() {
            ids.push(session.id);
            let read = session.ctx.timeline.try_lock().ok().map(|tl| {
                let log = tl.slog().log();
                (log.footprint(), log.bytes(), log.pinned_bytes())
            });
            let entry = self
                .session_status
                .slog_memory
                .known
                .entry(session.id)
                .or_default();
            if let Some((footprint, bytes, pinned)) = read {
                entry.footprint = footprint;
                entry.bytes = bytes;
                entry.pinned = pinned;
            }
        }
        let sweep = &mut self.session_status.slog_memory;
        sweep.known.retain(|id, _| ids.contains(id));
        let total: usize = sweep.known.values().map(|c| c.footprint).sum();
        if total <= cap && !sweep.known.values().any(|c| c.lowered) {
            return;
        }
        let reserved: Vec<usize> = ids
            .iter()
            .map(|id| {
                let c = sweep.known[id];
                let minimum = c.pinned.saturating_add(aterm_slog::MAX_RECORD_BYTES);
                if c.lowered && c.budget.is_some_and(|b| b <= minimum) {
                    c.footprint
                } else {
                    0
                }
            })
            .collect();
        let wants: Vec<usize> = ids
            .iter()
            .zip(&reserved)
            .map(|(id, floor)| sweep.known[id].footprint.saturating_sub(*floor))
            .collect();
        let room = cap.saturating_sub(reserved.iter().sum());
        let shares: Vec<usize> = fair_shares(&wants, room)
            .into_iter()
            .zip(reserved)
            .map(|(share, floor)| share + floor)
            .collect();
        let mut over: Vec<(usize, u64, usize)> = ids
            .iter()
            .zip(&shares)
            .filter_map(|(id, share)| {
                let c = sweep.known.get(id)?;
                (c.footprint > *share).then_some((c.footprint - share, *id, *share))
            })
            .collect();
        over.sort_by_key(|o| std::cmp::Reverse(o.0));
        let raise: Vec<u64> = if total < cap - cap / 8 {
            ids.iter()
                .filter(|id| sweep.known.get(id).is_some_and(|c| c.lowered))
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let shrink = if total > cap.saturating_add(cap / 8) {
            over.len()
        } else {
            SLOG_SWEEP_MAX_SHRINK
        };
        let mut budgets: Vec<(u64, Option<usize>)> = Vec::new();
        for (_, id, share) in over.into_iter() {
            if budgets.len() == shrink {
                break;
            }
            let c = sweep.known[&id];
            let bytes = (c.bytes as u128 * share as u128 / c.footprint.max(1) as u128) as usize;
            let budget = bytes.max(c.pinned.saturating_add(aterm_slog::MAX_RECORD_BYTES));
            if c.lowered && c.budget.is_some_and(|b| budget >= b) {
                continue;
            }
            budgets.push((id, Some(budget)));
        }
        budgets.extend(raise.into_iter().map(|id| (id, None)));
        for (id, budget) in budgets {
            let Some(session) = self.pool.get(id) else {
                continue;
            };
            let Ok(mut tl) = session.ctx.timeline.try_lock() else {
                continue;
            };
            let log = tl.slog_mut();
            log.set_log_byte_budget(budget.unwrap_or(usize::MAX));
            let (footprint, bytes) = (log.log().footprint(), log.log().bytes());
            drop(tl);
            if let Some(c) = self.session_status.slog_memory.known.get_mut(&id) {
                c.footprint = footprint;
                c.bytes = bytes;
                c.lowered = budget.is_some();
                c.budget = budget;
            }
        }
    }

    fn closed_log_charge(&self) -> usize {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .closed_logs()
            .charge()
    }
}

/// The settle wakes the event loop owes, by session: one entry per session whose turn tracker has a pending settle.
#[derive(Debug, Default)]
pub(crate) struct Wakes {
    at: BTreeMap<u64, u64>,
}

impl Wakes {
    /// Session `id`'s tracker wants a tick at `wake` (monotonic ms), or none.
    pub(crate) fn note(&mut self, id: u64, wake: Option<u64>) {
        match wake {
            Some(w) => {
                self.at.insert(id, w);
            }
            None => {
                self.at.remove(&id);
            }
        }
    }

    /// The soonest wake owed.
    pub(crate) fn next(&self) -> Option<u64> {
        self.at.values().copied().min()
    }

    /// The sessions whose wake is due at `now_ms`.
    pub(crate) fn due(&self, now_ms: u64) -> Vec<u64> {
        self.at
            .iter()
            .filter(|(_, w)| **w <= now_ms)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Whether no wake is owed.
    pub(crate) fn is_empty(&self) -> bool {
        self.at.is_empty()
    }
}

impl crate::App {
    /// Lift every session log's readers' fence (an update stood down after its worker carried the logs, `SessionLog::carry_source`): each timeline's leaf lock, one at a time.
    #[cfg(unix)]
    pub(crate) fn lift_session_log_fences(&mut self) {
        crate::session_log_journal::note_stood_down();
        for session in self.pool.iter() {
            let mut timeline = session
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            timeline.slog_mut().lift_read_fence();
            let _ = timeline.slog_take_wake();
        }
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .mirror()
            .lift_commit_fence();
    }

    /// What a footer read wakes when it moves a session's log (phase 3B, `claude_footer::LogWake`): the loop, under the `[session_log] clause` switch.
    pub(crate) fn footer_log_wake(&self) -> crate::claude_footer::LogWake {
        crate::claude_footer::LogWake::new(self.config.session_log_clause())
    }

    /// Whether session `id`'s clause moved past what its tab's chrome was composed with (phase 3B).
    pub(crate) fn clause_moved(&self, id: u64) -> bool {
        if !self.config.session_log_clause() {
            return false;
        }
        let (Some(cached), Some(s)) = (self.session_chrome.get(&id), self.pool.get(id)) else {
            return false;
        };
        let Ok(g) = s.ctx.timeline.try_lock() else {
            return false;
        };
        let now = g
            .slog()
            .chrome_clause(crate::turn_ledger::now_ms())
            .map(|(key, _)| key);
        now != cached.clause
    }

    /// Refresh session `id`'s tab chrome when its clause moved ([`Self::clause_moved`]): one compose per key change, never per record and never on a clock.
    pub(crate) fn follow_clause(&mut self, id: u64) {
        if self.clause_moved(id) {
            self.refresh_session_status_chrome(id);
        }
    }

    /// `Wake::SessionLogMoved`: the footer thread or the render path moved session `id`'s log (phase 3B), and its watchers were rung where the move took the log's owed wake.
    pub(crate) fn on_session_log_moved(&mut self, id: u64) {
        let Some(s) = self.pool.get(id) else {
            return;
        };
        let wake = s
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog_wake();
        self.session_status.slog_wakes.note(id, wake);
        self.follow_clause(id);
    }

    /// THE SESSION LOG's settle ticks: each session whose turn tracker's wake is due is ticked under its timeline's leaf lock (the lock the sweep takes on this thread), and its next wake replaces the entry.
    pub(crate) fn tick_session_logs(&mut self) {
        if self.session_status.slog_wakes.is_empty() {
            return;
        }
        let now = crate::turn_ledger::now_ms();
        for id in self.session_status.slog_wakes.due(now) {
            let (wake, owed) = self.pool.get(id).map_or((None, false), |s| {
                let mut timeline = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                (timeline.slog_tick(), timeline.slog_take_wake())
            });
            self.session_status
                .slog_wakes
                .note(id, wake.filter(|w| *w > now));
            if owed {
                self.follow_clause(id);
            }
        }
    }

    /// The soonest settle tick owed, as an instant for the loop's deadline fold (`DeadlineOwner::SessionLog`), or `None` while no turn rests.
    pub(crate) fn next_session_log_wake(&self) -> Option<std::time::Instant> {
        let wake = self.session_status.slog_wakes.next()?;
        let now = crate::turn_ledger::now_ms();
        Some(std::time::Instant::now() + std::time::Duration::from_millis(wake.saturating_sub(now)))
    }
}

/// The `log` verb's usage line.
pub(crate) const LOG_USAGE: &str = "ERR usage: log [<node>] [lvl=session|run|turn|step] [since=<seq> ep=<epoch>] [n=<count>] [kinds=<k>[,...]] [class=turn] [tier=content] [--json]\n";
/// `log n`'s usage line (the instance's node log).
pub(crate) const NODE_LOG_USAGE: &str =
    "ERR usage: log n [since=<seq> ep=<epoch>] [n=<count>] [kinds=<k>[,...]] [--json]\n";
/// The `brief` verb's usage line.
pub(crate) const BRIEF_USAGE: &str =
    "ERR usage: brief [<node>|all] [budget=<tokens>] [since=<seq> ep=<epoch>]\n";

/// A parsed `log` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogQuery {
    /// The subtree asked for (the session: all of it).
    pub(crate) node: Node,
    /// The deepest level shown.
    pub(crate) lvl: Option<Lvl>,
    /// The reader's cursor: `since=` and the `ep=` it was read in.
    pub(crate) cursor: Option<Cursor>,
    /// The newest this many matching records.
    pub(crate) n: Option<usize>,
    /// Only these kinds.
    pub(crate) kinds: Vec<Kind>,
    /// Only the turn class (what the bus mirror would carry).
    pub(crate) turn_class: bool,
    /// The tier the fields are shown to: host by default, content when asked.
    pub(crate) tier: Tier,
    /// One JSON object per record.
    pub(crate) json: bool,
}

const fn cursor_of(since: Option<u64>, ep: Option<u32>) -> Result<Option<Cursor>, ()> {
    match (since, ep) {
        (None, None) => Ok(None),
        (Some(s), Some(e)) => Ok(Some(Cursor::new(e, s))),
        (None, Some(e)) => Ok(Some(Cursor::new(e, 0))),
        (Some(_), None) => Err(()),
    }
}

fn parse_lvl(word: &str) -> Option<Lvl> {
    match word {
        "session" | "-" => Some(Lvl::Session),
        "run" => Some(Lvl::Run),
        "turn" => Some(Lvl::Turn),
        "step" => Some(Lvl::Step),
        _ => None,
    }
}

impl LogQuery {
    /// Parse a `log` request's arguments.
    pub(crate) fn parse(rest: &str) -> Result<LogQuery, &'static str> {
        let mut q = LogQuery {
            node: Node::SESSION,
            lvl: None,
            cursor: None,
            n: None,
            kinds: Vec::new(),
            turn_class: false,
            tier: Tier::Host,
            json: false,
        };
        let (mut since, mut ep) = (None, None);
        let mut node = None;
        for tok in rest.split_whitespace() {
            let bad = || Err(LOG_USAGE);
            if tok == "--json" || tok == "json" {
                q.json = true;
                continue;
            }
            let Some((k, v)) = tok.split_once('=') else {
                match Node::parse(tok) {
                    Some(n) if n != Node::INSTANCE && node.is_none() => node = Some(n),
                    _ => return bad(),
                }
                continue;
            };
            match k {
                "lvl" => q.lvl = Some(parse_lvl(v).ok_or(LOG_USAGE)?),
                "since" => since = Some(v.parse::<u64>().map_err(|_| LOG_USAGE)?),
                "ep" => ep = Some(v.parse::<u32>().map_err(|_| LOG_USAGE)?),
                "n" => q.n = Some(v.parse::<usize>().map_err(|_| LOG_USAGE)?),
                "kinds" => {
                    for w in v.split(',') {
                        match Kind::from_word(w) {
                            Kind::Unknown => return bad(),
                            kind => q.kinds.push(kind),
                        }
                    }
                }
                "class" if v == "turn" => q.turn_class = true,
                "tier" if v == "content" => q.tier = Tier::Content,
                "tier" if v == "host" => q.tier = Tier::Host,
                _ => return bad(),
            }
        }
        q.node = node.unwrap_or(Node::SESSION);
        q.cursor = cursor_of(since, ep).map_err(
            |()| "ERR usage: log since=<seq> needs ep=<epoch>, the ep= of the reply it came from\n",
        )?;
        Ok(q)
    }
}

/// A parsed `brief` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BriefQuery {
    /// The subtree asked for.
    pub(crate) node: Node,
    /// The instance-wide form (`brief all`, Owner-only).
    pub(crate) all: bool,
    /// The token budget.
    pub(crate) budget: Option<usize>,
    /// The reader's cursor.
    pub(crate) cursor: Option<Cursor>,
}

impl BriefQuery {
    /// Parse a `brief` request's arguments; the one `<node>` (or `all`) may stand anywhere among them.
    pub(crate) fn parse(rest: &str) -> Result<BriefQuery, &'static str> {
        let mut q = BriefQuery {
            node: Node::SESSION,
            all: false,
            budget: None,
            cursor: None,
        };
        let (mut since, mut ep) = (None, None);
        let mut named = false;
        for tok in rest.split_whitespace() {
            let Some((k, v)) = tok.split_once('=') else {
                if named {
                    return Err(BRIEF_USAGE);
                }
                named = true;
                match tok {
                    "all" => q.all = true,
                    t => match Node::parse(t) {
                        Some(node) if node != Node::INSTANCE => q.node = node,
                        _ => return Err(BRIEF_USAGE),
                    },
                }
                continue;
            };
            match k {
                "budget" => {
                    let b = v.parse::<usize>().map_err(|_| BRIEF_USAGE)?;
                    q.budget = Some(b.max(1));
                }
                "since" => since = Some(v.parse::<u64>().map_err(|_| BRIEF_USAGE)?),
                "ep" => ep = Some(v.parse::<u32>().map_err(|_| BRIEF_USAGE)?),
                _ => return Err(BRIEF_USAGE),
            }
        }
        q.cursor = cursor_of(since, ep).map_err(|()| {
            "ERR usage: brief since=<seq> needs ep=<epoch>, the ep= of the reply it came from\n"
        })?;
        if q.all && (q.node != Node::SESSION || q.cursor.is_some()) {
            return Err(BRIEF_USAGE);
        }
        Ok(q)
    }
}

/// THE `lvl=` / `class=turn` FILTER, the one the `log` verb and `subscribe … log` share (phase 3A), so the two never disagree: no record of a level deeper than `lvl`, and with `turn_class` only the turn class (what the bus mirror would carry).
pub(crate) fn shows(r: &Rec, lvl: Option<Lvl>, turn_class: bool) -> bool {
    lvl.is_none_or(|l| r.node.lvl <= l) && (!turn_class || r.is_turn_class())
}

fn in_subtree(log: &Log, node: Node, root: Node) -> bool {
    if root == Node::SESSION {
        return true;
    }
    let mut at = node;
    for _ in 0..=aterm_slog::MAX_OPEN + 2 {
        if at == root {
            return true;
        }
        match log.parent_of(at) {
            Some(p) => at = p,
            None => return false,
        }
    }
    false
}

fn rec_json(r: &Rec, tier: Tier) -> String {
    use aterm_json::Value;
    let mut o = aterm_json::Map::new();
    o.insert("seq".into(), Value::from(r.seq));
    o.insert("op".into(), Value::from(r.op.word()));
    o.insert("node".into(), Value::from(r.node.to_string()));
    o.insert("k".into(), Value::from(r.k.word()));
    if let Some(p) = r.par {
        o.insert("par".into(), Value::from(p.to_string()));
    }
    if r.by != Actor::Nobody {
        o.insert("by".into(), Value::from(r.by.word()));
    }
    o.insert("src".into(), Value::from(r.src.word()));
    o.insert("conf".into(), Value::from(r.conf.word()));
    o.insert("t".into(), Value::from(r.t));
    for (k, v) in r.f.iter() {
        if k.tier() > tier {
            continue;
        }
        let v = match v {
            Val::Word(w) => Value::from(*w),
            Val::U(n) => Value::from(*n),
            Val::I(n) => Value::from(*n),
            Val::Id(id) => Value::from(id.as_str()),
            Val::Tok(t) => Value::from(t.as_str()),
            Val::Text(t) => Value::from(t.as_str()),
        };
        o.insert(k.word().into(), v);
    }
    aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_string())
}

fn gap_json(g: &Gap) -> String {
    use aterm_json::Value;
    let mut o = aterm_json::Map::new();
    o.insert("op".into(), Value::from("gap"));
    o.insert("why".into(), Value::from(g.why.word()));
    o.insert("lost".into(), Value::from(g.lost));
    o.insert("resync_to".into(), Value::from(g.resync_to));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_string())
}

fn gap_line(g: &Gap) -> String {
    format!(
        "gap why={} lost={} resync_to={}",
        g.why.word(),
        g.lost,
        g.resync_to
    )
}

/// What a `log` reply needs of a log, copied under the timeline's leaf lock: the records it shows, shared with the ring (a count bump each, no field copied), and the header's numbers.
#[derive(Clone, Debug)]
pub(crate) struct LogCopy {
    recs: Vec<Arc<Rec>>,
    gap: Option<Gap>,
    more: usize,
    as_of: u64,
    ep: u32,
    oldest: u64,
    lost: u64,
}

impl LogCopy {
    /// The records `q` asks for, and the header: a filter and a share of the records shown, nothing encoded.
    pub(crate) fn copied_from(log: &Log, q: &LogQuery) -> LogCopy {
        let read = log.read_shared(q.cursor.unwrap_or(Cursor::new(log.ep(), 0)), usize::MAX);
        let matching: Vec<&Arc<Rec>> = read
            .recs
            .into_iter()
            .filter(|r| in_subtree(log, r.node, q.node))
            .filter(|r| shows(r, q.lvl, q.turn_class))
            .filter(|r| q.kinds.is_empty() || q.kinds.contains(&r.k))
            .collect();
        let more = q.n.map_or(0, |n| matching.len().saturating_sub(n));
        LogCopy {
            recs: matching.into_iter().skip(more).map(Arc::clone).collect(),
            gap: read.gap,
            more,
            as_of: log.reader_last_seq(),
            ep: log.ep(),
            oldest: log.complete_from(),
            lost: log.lost(),
        }
    }

    /// The node log's records `q` asks for (`log n`): the same header, `oldest=` the oldest live seq (the ring is a FIFO, whole from there).
    pub(crate) fn copied_from_node(log: &aterm_slog::NodeLog, q: &LogQuery) -> LogCopy {
        let read = log.read(q.cursor.unwrap_or(Cursor::new(log.ep(), 0)), usize::MAX);
        let matching: Vec<&Rec> = read
            .recs
            .into_iter()
            .filter(|r| q.kinds.is_empty() || q.kinds.contains(&r.k))
            .collect();
        let more = q.n.map_or(0, |n| matching.len().saturating_sub(n));
        LogCopy {
            recs: matching[more..]
                .iter()
                .map(|r| Arc::new((*r).clone()))
                .collect(),
            gap: read.gap,
            more,
            as_of: log.last_seq(),
            ep: log.ep(),
            oldest: log.oldest_seq(),
            lost: log.lost(),
        }
    }

    /// The copy with its header handing out `c` (`as_of=` its seq, `ep=` its epoch) in place of the log's head: a journal reply's (`aterm_slog::journal::View::reply_cursor`), which names none of the records the reply derived past the journal's head.
    pub(crate) const fn handing(mut self, c: Cursor) -> LogCopy {
        self.as_of = c.seq;
        self.ep = c.ep;
        self
    }
}

/// The `log` reply from its copy: `OK <n> v=2 as_of=<seq> ep=<e> oldest=<seq>[ lost=<n>][ more=<k>]` (`oldest=` the seq from which the window is whole, [`Log::complete_from`]), a gap line when the read crossed records the reader will never see or names another epoch (`gap why= lost= resync_to=`, or under `--json` a `{"op":"gap",…}` object, so every body line is one object), then one record per line, oldest first.
pub(crate) fn render_log_copy(c: &LogCopy, q: &LogQuery) -> String {
    #[cfg(test)]
    encode_probe();
    let mut body: Vec<String> = Vec::with_capacity(c.recs.len() + 1);
    if let Some(g) = &c.gap {
        body.push(if q.json { gap_json(g) } else { gap_line(g) });
    }
    for r in &c.recs {
        body.push(if q.json {
            rec_json(r, q.tier)
        } else {
            aterm_slog::encode(r, q.tier)
                .unwrap_or_else(|_| format!("{} {} {}", r.seq, r.op.word(), r.node))
        });
    }
    let mut out = format!(
        "OK {} v=2 as_of={} ep={} oldest={}",
        body.len(),
        c.as_of,
        c.ep,
        c.oldest
    );
    if c.lost > 0 {
        out.push_str(&format!(" lost={}", c.lost));
    }
    if c.more > 0 {
        out.push_str(&format!(" more={}", c.more));
    }
    out.push('\n');
    for line in body {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
thread_local! {
    /// A test's probe, called as a `log`, `brief`, `history`, `timeline` or `exits` reply starts encoding ([`render_log_copy`], [`BriefCopy::brief`], [`render_brief_all`], and the ring verbs' formatters in `control_session`) on this thread: what it finds held there, the encoding runs under.
    pub(crate) static ENCODE_PROBE: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run this thread's [`ENCODE_PROBE`], if a test set one.
#[cfg(test)]
pub(crate) fn encode_probe() {
    ENCODE_PROBE.with(|p| {
        if let Some(probe) = p.borrow().as_ref() {
            probe();
        }
    });
}

/// The `log` reply ([`render_log_copy`] over [`LogCopy::copied_from`]), in one call (tests).
#[cfg(test)]
pub(crate) fn render_log(log: &Log, q: &LogQuery) -> String {
    render_log_copy(&LogCopy::copied_from(log, q), q)
}

/// What a brief needs of a log, copied under the timeline's leaf lock (as [`LogCopy`]): the records the reader asked for, shared with the ring, the gap that read crossed, the fold's state, and the fold's mail links.
#[derive(Clone, Debug)]
pub(crate) struct BriefCopy {
    recs: Vec<Arc<Rec>>,
    gap: Option<Gap>,
    state: aterm_slog::SessionState,
    links: Vec<(Node, aterm_slog::MailLink)>,
}

impl BriefCopy {
    /// The records past `cursor` (all of them: none) in `node`'s subtree, the gap that read crossed, the state, and the mail links.
    pub(crate) fn copied_from(log: &Log, node: Node, cursor: Option<Cursor>) -> BriefCopy {
        let read = log.read_shared(cursor.unwrap_or(Cursor::new(log.ep(), 0)), usize::MAX);
        BriefCopy {
            recs: read
                .recs
                .into_iter()
                .filter(|r| in_subtree(log, r.node, node))
                .map(Arc::clone)
                .collect(),
            gap: read.gap,
            state: log.reader_state().clone(),
            links: log.reader_fold().mail_links().to_vec(),
        }
    }

    /// The copy with its header handing out `c` in place of the log's head ([`LogCopy::handing`]).
    pub(crate) const fn handing(mut self, c: Cursor) -> BriefCopy {
        self.state.as_of = c.seq;
        self.state.ep = c.ep;
        self
    }

    fn input(&self) -> aterm_slog::BriefInput<'_> {
        aterm_slog::BriefInput::new(
            self.recs.iter().map(|r| &**r),
            self.gap,
            &self.state,
            &self.links,
        )
    }

    /// Its brief within `budget` tokens at `now_wall`.
    pub(crate) fn brief(&self, sid: &str, budget: usize, now_wall: u64) -> aterm_slog::Brief {
        #[cfg(test)]
        encode_probe();
        self.input().scanned().fit(sid, budget, now_wall)
    }
}

/// One session's brief: the records past the reader's cursor in the subtree asked for, against the whole fold, within `budget` tokens, with the gap that read crossed (a cursor of another epoch, ahead of the head, or behind the ring) said as `log` says it.
#[cfg(test)]
pub(crate) fn render_brief(
    log: &Log,
    sid: &str,
    node: Node,
    cursor: Option<Cursor>,
    budget: usize,
    now_wall: u64,
) -> String {
    BriefCopy::copied_from(log, node, cursor)
        .brief(sid, budget, now_wall)
        .text
}

/// One session's part of `brief all`, copied under its timeline lock.
#[derive(Clone, Debug)]
pub(crate) struct BriefPart {
    /// Its session id.
    pub(crate) sid: String,
    /// Its whole log.
    pub(crate) copy: BriefCopy,
    /// The time on its log's clock.
    pub(crate) now_wall: u64,
}

impl BriefPart {
    fn rank(&self) -> (u8, std::cmp::Reverse<u64>) {
        let need = self
            .copy
            .state
            .top_need()
            .map_or(u8::MAX, aterm_slog::need_precedence);
        (need, std::cmp::Reverse(self.copy.state.last_t))
    }
}

fn past_ok_count(text: &str) -> &str {
    match text.split_once(' ') {
        Some(("OK", tail)) => tail.split_once(' ').map_or(tail, |(_, t)| t),
        _ => text,
    }
}

/// The instance-wide brief (`brief all`): `OK <n> brief all sessions=<k> v=2 budget=<b> used=<u>`, then each kept session's brief without its `OK <n> ` prefix, then `more=sessions:<k>` when sessions were left out.
pub(crate) fn render_brief_all(parts: &[BriefPart], budget: usize) -> String {
    #[cfg(test)]
    encode_probe();
    let n = parts.len();
    let fixed = aterm_slog::tokens(
        format!("OK {n:05} brief all sessions={n} v=2 budget={budget} used={budget:05}\n").len()
            + format!("more=sessions:{n}\n").len(),
    );
    let avail = budget.saturating_sub(fixed);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (parts[i].rank(), i));
    let inputs: Vec<aterm_slog::BriefInput<'_>> = parts.iter().map(|p| p.copy.input()).collect();
    let briefings: Vec<aterm_slog::Briefing<'_>> =
        inputs.iter().map(aterm_slog::BriefInput::scanned).collect();
    let floor: Vec<usize> = parts
        .iter()
        .zip(&briefings)
        .map(|(p, b)| b.floor(&p.sid, p.now_wall).used)
        .collect();
    let kept = (1..=n)
        .rev()
        .find(|&k| {
            let share = avail / k;
            order[..k].iter().all(|&i| floor[i] <= share)
        })
        .unwrap_or(0);
    let share = avail.checked_div(kept).unwrap_or(0);
    let mut keep: Vec<usize> = order[..kept].to_vec();
    keep.sort_unstable();
    let mut body = String::new();
    for &i in &keep {
        let b = briefings[i].fit(&parts[i].sid, share, parts[i].now_wall);
        body.push_str(past_ok_count(&b.text));
        body.push('\n');
    }
    if kept < n {
        body.push_str(&format!("more=sessions:{}\n", n - kept));
    }
    let lines = body.lines().count();
    let mut used = 0;
    for _ in 0..4 {
        let head = format!("OK {lines} brief all sessions={n} v=2 budget={budget} used={used}\n");
        let next = aterm_slog::tokens(head.len() + body.len());
        if next == used {
            break;
        }
        used = next;
    }
    format!("OK {lines} brief all sessions={n} v=2 budget={budget} used={used}\n{body}")
}

#[path = "session_log_mark.rs"]
mod mark;

#[path = "session_log_carry.rs"]
mod carry;
#[cfg(all(test, unix))]
pub(crate) use carry::JOURNAL_HOST_LINE;
#[cfg(any(unix, test))]
pub(crate) use carry::{PreparedSlogCarry, SlogCarrySource};
#[cfg(test)]
pub(crate) use mark::{AWAIT_LOG_USAGE, MARK_USAGE, MarkReq};
pub(crate) use mark::{
    CAUSE_USAGE, CauseRef, InputAct, InputVerb, LogAwait, LogAwaitStep, cmd_mark, validate_cause,
};

#[path = "session_log_closed.rs"]
mod closed;
#[cfg(test)]
pub(crate) use closed::ClosedSource;
#[cfg(test)]
pub(crate) use closed::{CLOSED_INDEX_CAP, CLOSED_SKELETONS, SKELETONS_BUILT};
pub(crate) use closed::{ClosedExit, ClosedKeep, ClosedLog, ClosedLogs};

#[path = "session_log_transcript.rs"]
pub(crate) mod transcript;

#[path = "session_log_observed.rs"]
pub(crate) mod observed;

#[cfg(test)]
#[path = "session_log_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "session_log_mark_tests.rs"]
mod mark_tests;

#[cfg(all(test, unix))]
#[path = "session_log_attribution_tests.rs"]
mod attribution_tests;

#[cfg(test)]
#[path = "session_log_clause_tests.rs"]
pub(crate) mod clause_tests;

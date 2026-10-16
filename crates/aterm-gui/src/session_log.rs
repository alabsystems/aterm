// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SESSION LOG, host side (`docs/DESIGN-session-log-2026-10-15.md`, slice
//! 1): one [`aterm_slog::Log`] per session, a field of
//! [`crate::session_timeline::SessionTimeline`] and so under its lock (design
//! §4.1: the allowed orderings meta → timeline and fabric → timeline are
//! unchanged, and the timeline lock stays a strict leaf — nothing here takes
//! another lock), and the producers that write it from the signals aterm
//! already observes:
//!
//! | Site | Records |
//! |---|---|
//! | `SessionStore::register` | `open - spawn\|adopt\|restore parent=<sid> epoch=<ep>` |
//! | `SessionStore::deregister_local_as` | `close - out=<reason> by= code=` |
//! | the status sweep's block read | `open r<n> cmd blk= detail=` at OSC 133 C; `close r<n> out= code= dur_ms=` at D (the program and agent runs under it first, at the same instant); both, `dur=wall`, for a block the sweep saw only finished; `gap why=evicted` for blocks the terminal dropped unread |
//! | `note_foreground_group`, `set_program` | `open`/`close r<n> prog\|agent prog=`; `amend r<n> agent prog=` when a group's name confirms the agent the screen named |
//! | `settle_agent_word` | `mark agent word= rev= gen= fp=`, and the [`TurnTracker`]'s turns and steps |
//! | `note_person` | the tracker's attribution, and `mark - human by=human` once per 30 s burst |
//! | `cmd_turn` | the tracker's `by=` and `ledger=` for the turn the exchange opens, noted at its first byte and at each Enter |
//! | `FailureMark`'s seen (the sweep, `note_session_seen`, a carried exit) | `mark - seen by=human`, which lifts `need=failure` |
//!
//! The sweep writes the block read BEFORE the foreground move: a command's D
//! closes the programs that ran under it at its own stamp, so no child closes
//! after its parent ([`SessionLog::shell_blocks`]).
//!
//! The producers make typed calls into `aterm-slog`; no payload is formatted to
//! be parsed back, nothing they write reaches a PTY, and none of them blocks on
//! a lock its caller does not already take there. Time enters as a [`Stamp`]
//! from [`Clock`], which a test replaces.
//!
//! THE SETTLE WAKE. A turn closes when its resting word has held for a second
//! (`aterm_slog::SETTLE_MS`), and nothing else may come to say so: the tracker
//! returns the instant it wants to be ticked, [`SessionLog::wake`]. The only
//! producers that can arm one run on the event loop (the sweep's
//! `publish_agent` and the input watch's `set_input`); they hand it to
//! [`Wakes`], which the loop folds under its own deadline owner
//! (`DeadlineOwner::SessionLog`) and ticks when due. An idle machine has no
//! entry and arms nothing; a `ctl turn` never leaves a settle armed (it closes
//! a resting turn at once).
//!
//! SEAMLESS UPDATES. Slice 1 carries no records: the successor's log starts
//! with `gap why=handoff epoch=<ep+1>` ([`Log::after_handoff`]). What it does
//! carry is the epoch and the three node-id counters, on the session's handoff
//! record ([`LogCarry`], `SessionRecord::log_ids`, an additive field an older
//! or newer build skips), so `t17` names one turn for as long as the session
//! lives. The counters are carried with a reserve, like the timeline's ids
//! ([`LOG_ID_RESERVE`]), because the outgoing process keeps writing until its
//! Commit; one that passes the reserve makes the Commit stand down
//! ([`SessionLog::past_handoff_ceiling`]).

use std::collections::{BTreeMap, HashMap};

use aterm_slog::{
    Actor, Amend, Close, Conf, Cursor, Fields, Gap, GapWhy, IdCounters, Key, Kind, Limits, Log,
    Lvl, Mark, Node, Open, Out, Rec, RunOut, SessionOut, ShortId, Src, Stamp, Tier, TurnTracker,
    Val, Verdict,
};

/// A session's first epoch: a fresh spawn's. Every adoption raises it: a
/// carried one to the predecessor's plus one ([`LogCarry`]), and one told
/// nothing of the predecessor's to [`adopted_epoch`].
pub(crate) const FIRST_EPOCH: u32 = 1;

/// The epoch of a successor told nothing of its predecessor's (a shell the
/// PTY keeper held through a crash; an older producer's or a rollback's
/// manifest, which carries no `log_ids`): the wall clock's Unix seconds at
/// the adoption, never below 2.
///
/// Why not [`FIRST_EPOCH`]: a reader's cursor from the predecessor
/// (`since=13 ep=1`) would match the successor's epoch, and once the new log
/// passed seq 13 the reader would read on into another process's records
/// with no `gap`, skipping the `gap why=crash` and `open - adopt` that say
/// so (design §5.1, "no silent loss"). The seconds are above every epoch a
/// spawned chain reaches (1 plus one per handoff), and above an earlier such
/// adoption's chain when at least one whole second, plus one per handoff that
/// chain made, has passed on the wall clock since; the 32 bits last until
/// 2106.
///
/// NOT GUARANTEED: nothing carries the predecessor's epoch to such a
/// successor, so two adoptions of one shell within the same wall second (an
/// aterm that crashes as it starts and is adopted again) take the same
/// epoch, and a wall clock stepped back can repeat or undercut an earlier
/// chain's. A reader whose `since=<seq> ep=<e>` names the repeated epoch then
/// reads on into the second successor's records with no `gap` once its seq
/// passes the cursor; a cursor of a lower or other epoch is still reset.
/// Strictly growing epochs here need the keeper's custody record to carry the
/// last one issued.
pub(crate) fn adopted_epoch(at: Stamp) -> u32 {
    u32::try_from(at.wall_ms / 1000)
        .unwrap_or(u32::MAX)
        .max(FIRST_EPOCH + 1)
}

/// How far past its last issued id each numbered level's counter is carried
/// across a seamless update: the ids the outgoing process may still issue
/// between drawing the manifest and its Commit. A process that issues more
/// refuses its Commit (a lossless retry) rather than let the successor issue
/// one of them again ([`SessionLog::past_handoff_ceiling`]). Sized like the
/// turn ledger's reserve (`control::TURN_ID_RESERVE`): every shell command
/// issues a run id, so a script that runs a few hundred commands while an
/// update stages must not keep the Commit retrying.
pub(crate) const LOG_ID_RESERVE: u32 = 1 << 12;

/// The host's reading of both clocks: wall Unix ms (the clock shell blocks are
/// stamped with, `aterm_types::current_time_ms`) and the process's
/// monotonic ms ([`crate::turn_ledger::now_ms`]).
pub(crate) fn host_stamp() -> Stamp {
    Stamp::new(
        aterm_types::current_time_ms().unwrap_or(0),
        crate::turn_ledger::now_ms(),
    )
}

/// `at` moved back to the wall instant `wall_ms` on both clocks: a block stamp
/// read at `at` becomes the stamp of the moment it names, so a run opened late
/// still measures its `dur_ms` from when it began. Never later than `at`. An
/// instant before this process's monotonic clock began clamps to its 0
/// ([`predates_clock`]).
pub(crate) fn stamp_at_wall(at: Stamp, wall_ms: u64) -> Stamp {
    let wall = wall_ms.min(at.wall_ms);
    Stamp::new(wall, at.mono_ms.saturating_sub(at.wall_ms - wall))
}

/// The later of two stamps (by the wall clock; `a` when they tie).
fn later(a: Stamp, b: Stamp) -> Stamp {
    if b.wall_ms > a.wall_ms { b } else { a }
}

/// Whether the wall instant `wall_ms`, read at `at`, lies before this
/// process's monotonic clock began (a command a predecessor started before a
/// seamless update): [`stamp_at_wall`] can only clamp it to 0 there, and a
/// `dur_ms` measured from it would be this process's uptime, not the run's.
/// Such a run's duration is the wall clock's (`dur=wall`).
pub(crate) fn predates_clock(at: Stamp, wall_ms: u64) -> bool {
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

/// What a seamless update carries of a session's log: its epoch and its node-id
/// counters (with the reserve). The wire row is `"<ep> <run> <turn> <step>"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogCarry {
    /// The outgoing process's epoch.
    pub(crate) ep: u32,
    /// The counters the successor continues from.
    pub(crate) ids: IdCounters,
}

impl LogCarry {
    /// The handoff record's spelling.
    pub(crate) fn row(self) -> String {
        format!(
            "{} {} {} {}",
            self.ep, self.ids.run, self.ids.turn, self.ids.step
        )
    }

    /// A carried row read back, or `None`: four integers, an epoch of at least
    /// one. Anything else carries nothing, and the successor starts at
    /// [`adopted_epoch`] (its log still opens with `gap why=handoff`).
    pub(crate) fn parse(row: &str) -> Option<LogCarry> {
        let mut it = row.split_ascii_whitespace().map(str::parse::<u32>);
        let (ep, run, turn, step) = (
            it.next()?.ok()?,
            it.next()?.ok()?,
            it.next()?.ok()?,
            it.next()?.ok()?,
        );
        if it.next().is_some() || ep == 0 {
            return None;
        }
        Some(LogCarry {
            ep,
            ids: IdCounters { run, turn, step },
        })
    }
}

/// How a block the status sweep read stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockEnd {
    /// Executing now (after its OSC 133 C).
    Running,
    /// Finished (OSC 133 D): its exit code and stamps.
    Done(crate::session_status::CompletedBlock),
    /// A prompt closed it with no D; `ended_ms` is that prompt's stamp.
    Unended {
        /// The closing prompt's OSC 133 A stamp, if stamped.
        ended_ms: Option<u64>,
    },
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
    /// Its command reduced to the program
    /// ([`crate::session_status::command_detail`]): the line itself is never
    /// stored.
    pub(crate) detail: Option<String>,
}

/// What the status sweep read of the shell's blocks under its one terminal
/// guard ([`crate::session_status::block_read`]): the blocks that ran a
/// command from the log's [`SessionLog::block_cursor`] on, OLDEST first, at
/// most [`crate::session_status::BLOCK_READ_MAX`] of them; the rest wait for
/// the next sweep (`more`), so a burst of commands is late, never lost.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockRead {
    /// The blocks, oldest first.
    pub(crate) facts: Vec<BlockFact>,
    /// The newest block id this read covered: every block up to it is in
    /// `facts`, ran nothing (and never will: the prompt in front, which may
    /// yet run a command, is never covered), finished before the log opened
    /// here, or is gone from the terminal (`dropped`).
    pub(crate) through: Option<u64>,
    /// The cursor the read started from ([`SessionLog::block_cursor`]).
    pub(crate) from: Option<u64>,
    /// Block ids from the cursor on that the terminal no longer holds: its
    /// block history was cleared (ED 3 from `clear`, Cmd-K, RIS) or dropped
    /// its oldest before a read reached them. Counted once: `through` covers
    /// them, so the next read starts at the first block still held.
    pub(crate) dropped: u64,
    /// The terminal's count of commands the shell started (each accepted OSC
    /// 133 C, [`aterm_core::terminal::Terminal::started_command_seq`]), which
    /// no clear takes back and no swallowed D hides; read whether or not a
    /// block is held. `None`: no count (a read built by hand). With
    /// [`Self::started_held`] it says whether a command ran on a block no
    /// read will see ([`SessionLog::lost_unread`]).
    pub(crate) started: Option<u64>,
    /// The blocks from the cursor on, still held, that reached C (executing,
    /// finished, or closed by a prompt with no D).
    pub(crate) started_held: u64,
    /// Whether the block at the cursor itself is one of
    /// [`Self::started_held`] (the open `cmd` run's, whose C the log has).
    pub(crate) started_at_from: bool,
    /// Of [`Self::started_held`], the ones past where a read cut at its cap
    /// stopped: their C's are counted, their blocks not yet read.
    pub(crate) started_unread: u64,
    /// The read stopped at its cap with a block left that may yet be a fact
    /// (one that reached C): the sweep owes one more look.
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
    /// The wall ms it opened at, when that predates this process's clock
    /// ([`predates_clock`]): its close measures `dur=wall`.
    wall_from: Option<u64>,
    /// The latest close of a front run that ran under it: the run itself
    /// never closes before it ([`SessionLog::end_cmd`]).
    child_end: Option<Stamp>,
}

/// How a `cmd` run ends.
#[derive(Clone, Copy, Debug)]
enum CmdEnd {
    /// Its OSC 133 D, with the exit code the shell reported, if any.
    Exit(Option<i32>),
    /// No D said how (`out=unknown`).
    Unknown,
}

/// [`SessionLog::c_snap`]: the terminal's C count every command up to the
/// cursor accounts for, and that cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CSnap {
    /// The C count at the read, less its unread blocks' C's.
    base: u64,
    /// The cursor the read left ([`SessionLog::block_cursor`]).
    cursor: Option<u64>,
}

/// A name the resolver gave the group in front ([`SessionLog::program_named`])
/// that waits for the next block read: which command it ran under is known
/// only once the blocks are read ([`SessionLog::shell_blocks`]).
#[derive(Clone, Debug)]
struct PendingName {
    pgid: i32,
    name: Option<String>,
    /// The published verdict its tracker starts from, as it stood.
    word: String,
    detail: Option<String>,
    at: Stamp,
    /// When it came, in the order of [`SessionLog::arrivals`]: its run is
    /// told the input and `ctl turn` that came after it ([`Typed`]).
    seq: u64,
}

/// What reached the session that a turn tracker attributes a turn to (a
/// person's input, [`SessionLog::human_input`]; a `ctl turn`,
/// [`SessionLog::ctl_turn`]): the latest of each, with when it came in the
/// order of [`SessionLog::arrivals`]. A name waits for a block read
/// ([`SessionLog::program_named`]), and the run it opens then is told what
/// came after the name, in that order and at its own stamps
/// ([`Self::replay`]), as it would have been had it opened at the name. The
/// latest of each is all a tracker keeps: the newest input is the one a
/// turn is attributed to, and a `ctl turn`'s ledger stands unless input
/// from another sender came after it.
#[derive(Clone, Copy, Debug, Default)]
struct Typed {
    /// A person's input: when it came, and its stamp.
    input: Option<(u64, Stamp)>,
    /// A `ctl turn`: when it came, its sender, its ledger id and its stamp.
    ctl: Option<(u64, Actor, u64, Stamp)>,
}

impl Typed {
    /// Tell `tracker` (of a run just opened, its first verdict observed) what
    /// came after arrival `after`, in arrival order. A fresh tracker has no
    /// settle armed, so this only records: the 5 s window applies when a
    /// turn opens.
    fn replay(&self, after: u64, log: &mut Log, tracker: &mut TurnTracker) {
        let input = self.input.filter(|(seq, _)| *seq > after);
        let ctl = self.ctl.filter(|(seq, ..)| *seq > after);
        let input_last = match (input, ctl) {
            (Some((i, _)), Some((c, ..))) => i > c,
            _ => true,
        };
        let exchange = |log: &mut Log, tracker: &mut TurnTracker| {
            if let Some((_, by, ledger, at)) = ctl {
                let _ = tracker.ctl_turn(log, by, ledger, at);
            }
        };
        if input_last {
            exchange(log, tracker);
        }
        if let Some((_, at)) = input {
            tracker.note_input(Actor::Human, at);
        }
        if !input_last {
            exchange(log, tracker);
        }
    }
}

/// The most names that wait for one block read: a group's exec chain
/// between two sweeps is a few; past it the oldest, which a later name
/// superseded anyway, gives way.
const PENDING_NAMES_MAX: usize = 8;

#[derive(Clone, Copy, Debug)]
struct FgRun {
    pgid: i32,
    node: Node,
}

#[derive(Debug)]
struct AgentRun {
    pgid: i32,
    node: Node,
    /// The agent it is.
    who: aterm_phase::Program,
    /// Opened because the screen named an agent no program name did (a Claude
    /// Code run as `node`): it closes when the screen stops naming it.
    by_screen: bool,
    tracker: TurnTracker,
}

/// One session's log and the run and turn state its producers keep beside it.
pub(crate) struct SessionLog {
    log: Log,
    clock: Clock,
    /// How the session came to this process: `spawn`, `adopt` or `restore`.
    origin: Kind,
    /// When the session node opened here (wall ms): a block that finished
    /// before it is another process's.
    opened_wall: Option<u64>,
    /// The `cmd` run of the block executing.
    cmd: Option<CmdRun>,
    /// The newest block id logged (or passed over as another process's).
    last_blk: Option<u64>,
    /// The C count at the last block read, less the C's of the blocks from
    /// the cursor on that read left unread, and the cursor it left: every C
    /// since landed on a block from that cursor on (or on none), so the ones
    /// no held block shows went with blocks a clear took
    /// ([`Self::lost_unread`]).
    c_snap: Option<CSnap>,
    /// The latest close of a `cmd` run: the next command never opens before
    /// it ([`Self::after_last_cmd`]).
    cmd_end: Option<Stamp>,
    /// The session's own process group (its shell's pid): the shell in front
    /// is the session at its prompt, never a run.
    root_pgid: i32,
    /// The run of the program in front.
    fg: Option<FgRun>,
    /// The agent run and its turn tracker.
    agent: Option<AgentRun>,
    /// Names given since the last block read, oldest first
    /// ([`Self::program_named`]).
    pending: Vec<PendingName>,
    /// How many names, inputs and `ctl turn`s have come: their order.
    arrivals: u64,
    /// The latest input and `ctl turn`, for a run a waiting name opens.
    typed: Typed,
    /// The counters the last handoff manifest carried ([`Self::handoff_carry`]).
    ceiling: Option<IdCounters>,
}

impl Default for SessionLog {
    fn default() -> Self {
        SessionLog {
            log: Log::new(FIRST_EPOCH),
            clock: Clock::default(),
            origin: Kind::Spawn,
            opened_wall: None,
            cmd: None,
            last_blk: None,
            c_snap: None,
            cmd_end: None,
            root_pgid: 0,
            fg: None,
            agent: None,
            pending: Vec::new(),
            arrivals: 0,
            typed: Typed::default(),
            ceiling: None,
        }
    }
}

/// The run outcome an exit status says.
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
        ExitActor::Sid(sid) => ShortId::new(sid).map_or(Actor::Nobody, Actor::Sid),
        ExitActor::Human => Actor::Human,
        ExitActor::Ctl => Actor::Ctl,
        #[cfg(any(unix, test))]
        ExitActor::Bridge => Actor::Bridge,
        ExitActor::Unknown => Actor::Nobody,
    }
}

/// The session outcome an `ExitReason` names.
pub(crate) fn exit_out(reason: crate::session_store::ExitReason) -> SessionOut {
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

/// `f` with `key` set to `val`, or `f` unchanged when the value does not fit
/// (a producer never fails on a field).
fn with(mut f: Fields, key: Key, val: Option<Val>) -> Fields {
    if let Some(val) = val {
        let _ = f.set(key, val);
    }
    f
}

/// A token value for `key`, sanitized and clamped, or `None`.
fn tok(key: Key, raw: &str) -> Option<Val> {
    aterm_slog::BoundedToken::new(key, raw).map(Val::Tok)
}

impl SessionLog {
    /// The log.
    pub(crate) fn log(&self) -> &Log {
        &self.log
    }

    /// The time now, on this log's clock.
    pub(crate) fn now(&self) -> Stamp {
        self.clock.now()
    }

    /// Replace the clock (tests).
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    /// The instant the turn tracker wants [`Self::tick`] at (monotonic ms), if
    /// a settle is pending.
    pub(crate) fn wake(&self) -> Option<u64> {
        self.agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node))
            .and_then(|a| a.tracker.wake_at())
    }

    /// Forget a run the log no longer holds open (a cascade closed it with its
    /// parent).
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

    // ---- the session -------------------------------------------------

    /// This session came across a seamless update (or out of the PTY keeper's
    /// custody, `keeper`): its log starts over, as slice 1 carries no records,
    /// with the loss said first — `gap why=handoff epoch=<ep+1>` continuing the
    /// carried node ids, or `gap why=crash` for a shell whose window died. A
    /// successor told nothing of the predecessor's epoch takes
    /// [`adopted_epoch`], so no cursor of the predecessor's reads on past the
    /// loss. Called before anything else is written.
    pub(crate) fn adopt(&mut self, carry: Option<LogCarry>, keeper: bool, at: Stamp) {
        self.log = match (keeper, carry) {
            (true, _) => {
                let mut log = Log::new(adopted_epoch(at));
                log.gap(GapWhy::Crash, 0, at);
                log
            }
            (false, Some(c)) => {
                Log::after_handoff(c.ep.saturating_add(1), Limits::SESSION, c.ids, at)
            }
            (false, None) => Log::after_handoff(
                adopted_epoch(at),
                Limits::SESSION,
                IdCounters::default(),
                at,
            ),
        };
        self.origin = Kind::Adopt;
        self.cmd = None;
        self.fg = None;
        self.agent = None;
        self.pending.clear();
        self.typed = Typed::default();
        self.last_blk = None;
        self.c_snap = None;
        self.cmd_end = None;
        self.ceiling = None;
    }

    /// This session is a cold restore's new shell (`open - restore`).
    pub(crate) fn note_restored(&mut self) {
        if self.opened_wall.is_none() {
            self.origin = Kind::Restore;
        }
    }

    /// `open - spawn|adopt|restore parent=<sid> epoch=<ep>`: the registry took
    /// the session in.
    pub(crate) fn open_session(&mut self, parent: Option<&str>, at: Stamp) {
        let f = with(
            Fields::new(),
            Key::Parent,
            parent.and_then(ShortId::new).map(Val::Id),
        );
        let f = with(f, Key::Epoch, Some(Val::U(u64::from(self.log.ep()))));
        if self
            .log
            .open_session(self.origin, Src::Aterm, Conf::Exact, Actor::Nobody, f, at)
            .is_ok()
        {
            self.opened_wall = Some(at.wall_ms);
        }
    }

    /// `close - out=<reason> by=<actor> code=<n>`: the registry let it go.
    /// Everything still open closes under it, innermost first, `out=gone`.
    pub(crate) fn close_session(
        &mut self,
        out: SessionOut,
        by: Actor,
        code: Option<i32>,
        at: Stamp,
    ) {
        // A name still waiting ran too: it opens, and closes with the rest.
        self.name_pending(self.cmd, None);
        let f = with(Fields::new(), Key::Code, code.map(|c| Val::I(i64::from(c))));
        let close = Close::new(Out::Session(out), Src::Aterm, Conf::Exact)
            .by(by)
            .fields(f);
        let _ = self.log.close_session(close, at);
        self.cmd = None;
        self.fg = None;
        self.agent = None;
    }

    // ---- shell blocks ------------------------------------------------

    /// The first block id the sweep's next block read must report: the open
    /// `cmd` run's block, else the one past the newest the log handled
    /// (`None`: every block the terminal holds).
    pub(crate) fn block_cursor(&self) -> Option<u64> {
        self.cmd
            .map(|c| c.blk)
            .or(self.last_blk.map(|b| b.saturating_add(1)))
    }

    /// When the session node opened here (wall ms): a block that finished
    /// before it is another process's, which the block read passes over
    /// without counting it against its cap.
    pub(crate) fn opened_wall(&self) -> Option<u64> {
        self.opened_wall
    }

    /// The status sweep's block read: OSC 133 C opens a `cmd` run (dated at its
    /// own stamp, `detail=` the program), D closes it with `out=`, `code=` and
    /// `dur_ms`, and a block that ran and finished between two sweeps gets both
    /// records from its stamps (`dur=wall`). A block a prompt closed with no D
    /// (an interrupted or replaced command) ends `out=unknown`. A block that
    /// finished before this process opened the session is the predecessor's,
    /// and is passed over. Blocks the terminal dropped before any read reached
    /// them, when one of them may have run a command, are said with one `gap
    /// why=evicted` ([`Self::lost_unread`]), and the cursor moves past
    /// them.
    ///
    /// A `cmd` run's close first closes, at the same instant, the program and
    /// agent runs that ran under it ([`Self::end_cmd`]): the shell's D proves
    /// they had ended, so a child never closes after its parent. No run closes
    /// before it opened, and a command never opens before the one before it
    /// closed.
    ///
    /// The names given since the last read ([`Self::program_named`]) open
    /// here, now that the blocks say which command each ran under: the first
    /// command, in the read's order, that had not ended by the name's instant
    /// (a name the resolver gave after the D of the command the log still
    /// held belongs to the next one, typed ahead), stamped no earlier than
    /// that command opened; with no command left open, the session's. A read
    /// cut at its cap (`more`) has not reached the present: a name given
    /// after every command it closed may be one of the unread commands', so
    /// it waits for the read that goes on from the cut, which places it by
    /// the same rule. A name given before that command began is its own only
    /// when `front` agrees ([`Self::open_names_before`]); otherwise it ran
    /// the command (a nested shell's), and opens first.
    ///
    /// `front` is the group in front as the sweep read it, after the blocks
    /// (`None`: not read). A command still running hangs only under a run
    /// whose group that is ([`Self::open_cmd`]), so the foreground move the
    /// same sweep applies next never closes a command's parent before its D
    /// is read.
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
        // The open run's block is gone from the terminal (a reset cleared it).
        if let Some(c) = self.cmd
            && !read.facts.iter().any(|f| f.id == c.blk)
        {
            self.cmd = None;
            self.end_cmd(c, CmdEnd::Unknown, at);
        }
        // Names given after every command the read closed: the one still
        // open ran them, or none did, once the read reached the present. A
        // cut read leaves them for the next.
        if !read.more {
            self.name_pending(self.cmd, None);
        }
        // Blocks the read passed over (nothing ran, another process's, or
        // gone from the terminal) are handled too: the next read starts past
        // them.
        if let Some(t) = read.through {
            self.last_blk = Some(self.last_blk.map_or(t, |l| l.max(t)));
        }
        // Every C up to now is accounted for, but those of the blocks a cut
        // read left unread: the next read counts its C's from here.
        if let Some(started) = read.started {
            self.c_snap = Some(CSnap {
                base: started.saturating_sub(read.started_unread),
                cursor: self.block_cursor(),
            });
        }
    }

    /// How many commands ran on blocks `read` cannot show and no read saw:
    /// the shell's C count since the last read (which every block before
    /// its cursor, and the open `cmd` run's own, accounts for), less the
    /// blocks from the cursor on still held that reached C. A clear at an
    /// idle prompt (Cmd-K, RIS) took no C; a `clear`, `reset` or any command
    /// run and cleared between two sweeps took its own; the open `cmd` run's
    /// block was read (it closes `out=unknown`), however its D fared. With no
    /// count from the read before at this cursor (the first read, or a read
    /// built by hand), every dropped id but the open run's counts.
    fn lost_unread(&self, read: &BlockRead) -> u64 {
        let own = self.cmd.is_some_and(|c| read.from == Some(c.blk));
        match (self.c_snap, read.started) {
            (Some(snap), Some(now)) if snap.cursor == read.from => {
                let held = read
                    .started_held
                    .saturating_sub(u64::from(own && read.started_at_from));
                now.saturating_sub(snap.base).saturating_sub(held)
            }
            _ => read.dropped.saturating_sub(u64::from(own)),
        }
    }

    /// `start` moved on to the last `cmd` run's close, if that is later: the
    /// shell runs one command at a time, so a run whose C reads earlier (a
    /// typed-ahead command whose predecessor's front run a late sweep closed
    /// after the D) opens as that one closes.
    fn after_last_cmd(&self, start: Stamp) -> Stamp {
        self.cmd_end.map_or(start, |end| later(start, end))
    }

    /// The names still waiting that were given before `fact`'s command began
    /// (its C), no command held then: every command this read handled before
    /// it had ended by their instant, else its close placed them. Whether the
    /// named program ran AS that command, its C read late (the PTY reader
    /// behind the sweep that saw the group), or ran it (a nested shell, whose
    /// prompt was in front first), the group in front at the read, `front`,
    /// tells: a command's program holds the front exactly while it runs (the
    /// shell takes the terminal back before it writes the D). A name whose
    /// group is in front though the command ended, or gone from the front
    /// though it runs, is not the command's: it opens now, at its own instant
    /// and at the session, and the command hangs under it
    /// ([`Self::open_cmd`]). One whose place agrees waits, to open under the
    /// command. Without a `front`, or a C stamp, or for a block never seen
    /// running whose end no D reported, every name waits.
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

    /// A block the log has not seen: one running opens its run; one finished
    /// since the session opened here gets both records from its stamps.
    /// `front` is the group in front at the read ([`Self::open_cmd`]).
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
                    // Both records from its stamps (`dur=wall`), and a name
                    // given while it ran under it.
                    let c = CmdRun {
                        blk: fact.id,
                        node,
                        wall_from: Some(start.wall_ms),
                        child_end: None,
                    };
                    self.end_cmd(c, CmdEnd::Exit(done.exit_code), end);
                }
            }
            // Never seen running and its end never reported: nothing to say
            // that is not a guess.
            BlockEnd::Unended { .. } => {}
        }
    }

    /// Open block `blk`'s `cmd` run at `at`, its start, under the newest open
    /// run that had opened by then (a nested shell's command, under the
    /// program that runs that shell), else the session: never under a run
    /// that opened after the command began, which did not run it (an agent
    /// run the log could hang only at the session, past a cut read, as its
    /// verdict moved before the read reached the command it ran in). No
    /// node opens before its parent.
    ///
    /// A command still running (`front`: the group in front at the read)
    /// hangs only under a run whose group that is. The sweep's foreground
    /// move closes every other front run next, and its close would take the
    /// command with it, `out=gone`, before its D is read: the D, its exit
    /// code and a failure's need would be lost, as the cursor has passed its
    /// block. A nested shell's command running in its own group is the
    /// session's; one that finished between two sweeps (`front` `None`:
    /// its run closes as it opens) still hangs under the shell.
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

    /// End the open `cmd` run `c` at `end`. First the names given before
    /// `end` open under it ([`Self::name_pending`]); then the program and agent
    /// runs that ran under it close at the same instant, `out=unknown` (the end of
    /// the command proves theirs; leaving the front is not an exit status),
    /// or as they opened if that is later ([`Self::end_front_under`]). The
    /// run itself never closes before it opened, nor before a child that
    /// closed later (a sweep that saw the group leave before it read the D),
    /// and one that opened before this process's clock began measures
    /// `dur=wall`. The front runs that came to the front while it ran but
    /// hang beside it close at its end too ([`Self::end_front_beside`]).
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

    /// When the open node `node` opened, on both clocks.
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
        if self.log.close(node, close, at).is_ok() {
            self.cmd_end = Some(self.cmd_end.map_or(at, |e| later(e, at)));
        }
    }

    /// Whether the open node `node` lies under the open node `root`.
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

    /// Close the front runs (the agent run, then the program run) that ran
    /// under `parent`, at `at` or, when something open under `parent` opened
    /// later (a program the resolver named after the D, which the log could
    /// only hang under the command it still held open), as the latest of
    /// them opened: nothing closes before it opened, and every one closes at
    /// the same instant, so none closes after its parent. Returns that
    /// instant, when anything closed.
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

    /// Close the front runs (the agent run, then the program run) that came
    /// to the front while the open `cmd` run `cmd` ran, which ends at `end`,
    /// but hang beside it, not under it: they opened no earlier than it and
    /// no later than `end`, and it does not hang under them (the run it hangs
    /// under ran it). Such a run is one the log had to open before a read
    /// reached the command it ran in: an agent's, at the session, as its
    /// verdict moved past a cut read ([`Self::agent_moved`]), or the agent
    /// run a screen named before any read saw its command. The command's end
    /// proves theirs, as it proves its children's, so they close with it, at
    /// `end` or as the latest node under them opened (nothing closes before
    /// it opened). Left open, the next command would hang under a run that
    /// did not run it, and the foreground move that closes the run would
    /// close that command with it, its D unread. The command's own close is
    /// not moved: they are not its children.
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

    /// A failure the person saw (`FailureMark`, the status observer's seen
    /// acknowledgement): `mark - seen by=human`, which lifts `need=failure`.
    pub(crate) fn failure_seen(&mut self, at: Stamp) {
        let _ = self.log.mark(
            Mark::new(Kind::Seen, Src::Status, Conf::Strong)
                .at(Node::SESSION)
                .by(Actor::Human),
            at,
        );
    }

    // ---- the program in front ----------------------------------------

    /// The session's own process group: its shell, which in front is the
    /// session at its prompt.
    pub(crate) fn note_root(&mut self, pgid: i32) {
        self.root_pgid = pgid;
    }

    /// The foreground group moved to `pgid`: the runs of the group that left
    /// close (`out=unknown`: leaving the front is not an exit status).
    pub(crate) fn foreground_moved(&mut self, pgid: i32, at: Stamp) {
        self.resync();
        // A name still waiting (no block read since it came) was the group's
        // that leaves: it opens under the command held, then closes.
        self.name_pending(self.cmd, None);
        if self.agent.as_ref().is_some_and(|a| a.pgid != pgid) {
            self.close_agent(at);
        }
        if let Some(fg) = self.fg.filter(|f| f.pgid != pgid) {
            self.close_run(fg.node, RunOut::Unknown, at);
            self.fg = None;
        }
    }

    /// The group in front, `pgid`, named its program (the resolver's answer,
    /// off the sweep): the name waits for the next block read
    /// ([`Self::shell_blocks`]), which knows the command it ran under. The
    /// log may still hold the command before it open (its D not yet read: a
    /// typed-ahead program named after it), so the run is not hung yet. That
    /// read comes at the session's next due look (the follow-up the answer
    /// arms is never earlier than it: up to one observation interval, more
    /// when the sweep skips the session under contention). Until then the
    /// name has no run: a person's input and a `ctl turn` that come meanwhile
    /// are kept, and the run is told them when it opens ([`Typed`]); the
    /// settle tick has no turn to settle. `word`/`detail` are the published
    /// verdict a new tracker starts from.
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

    /// Open the names given up to `until` (all, for `None`), oldest first,
    /// under `under` ([`Self::open_named`]).
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

    /// A name for the group in front, `p.pgid`, opens its run: a `prog` run
    /// (an `agent` run, with a turn tracker, for an agent's name) under the
    /// `cmd` run `under`, no earlier than that opened. The shell in front is
    /// no run. A group that names another program (an exec) supersedes its
    /// run. A group whose agent the screen already named, now naming that
    /// agent (an interpreter, `node`, renamed `claude`), CONFIRMS that run: it
    /// is amended `prog=` and lives as long as its group, its turn going on
    /// undisturbed. A new agent run's tracker, its first verdict observed, is
    /// told the input and `ctl turn` that came after the name ([`Typed`]).
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
            if self.agent.as_ref().is_some_and(|a| a.node == fg.node) {
                self.agent = None;
            }
            // Another name replaced it (an exec); no name says nothing more.
            let out = if program.is_some() {
                RunOut::Superseded
            } else {
                RunOut::Unknown
            };
            self.close_run(fg.node, out, at);
            self.fg = None;
            // Its close took the runs under it (a screen-named agent run):
            // never amend a closed node.
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
            // The screen named the agent before any name was read for its
            // group: the name refines that run.
            let f = with(Fields::new(), Key::Prog, tok(Key::Prog, name));
            let _ = self.log.amend(
                a.node,
                Amend::new(Kind::Agent, Src::Status, Conf::Strong, f),
                at,
            );
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
        self.fg = Some(FgRun { pgid, node });
        if let Some(who) = agent {
            let mut tracker = TurnTracker::new(node);
            let _ = tracker.observe(
                &mut self.log,
                Verdict::from_agent_word(word, detail),
                detail,
                at,
            );
            self.typed.replay(p.seq, &mut self.log, &mut tracker);
            self.agent = Some(AgentRun {
                pgid,
                node,
                who,
                by_screen: false,
                tracker,
            });
        }
    }

    /// The screen-named agent run of group `pgid` is confirmed by its own
    /// name `name`: amended `prog=<name>`, it is the program's run now and
    /// closes with its group, not when the screen stops naming it. Nothing
    /// closes: the program run it hangs under (the interpreter's, `node`)
    /// stays the group's front run, and the turn in progress stays open.
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
        if self.fg.is_none_or(|f| f.pgid != pgid) {
            self.fg = Some(FgRun { pgid, node });
        }
    }

    /// Close a front run (`prog` or `agent`). One that ran under the open
    /// `cmd` run is remembered there, so the command never closes before it.
    fn close_run(&mut self, node: Node, out: RunOut, at: Stamp) {
        let under_cmd = self.cmd.is_some_and(|c| self.descends(node, c.node));
        let closed = self.log.close(
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

    // ---- the agent verdict -------------------------------------------

    /// The published agent verdict moved: `mark agent word= rev= gen= fp=`,
    /// then the turn tracker's transitions. A screen that names an agent no
    /// program name did opens its `agent` run first. Returns the tracker's
    /// wake ([`Self::wake`]).
    pub(crate) fn agent_moved(&mut self, m: AgentMove<'_>, at: Stamp) -> Option<u64> {
        self.resync();
        // The sweep publishes after its block read, which opened every name
        // but past a cut; one still waiting (a publish off the sweep, or
        // past a cut read) opens first, under the command held (none, past
        // a cut), so the verdict finds its run.
        self.name_pending(self.cmd, None);
        // Only an agent's reader names a run (an exhaustive match: a new
        // agent must answer here).
        let named = m.reader.filter(|r| match r {
            aterm_phase::Program::Claude
            | aterm_phase::Program::Codex
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
        // A run the screen opened ends when the screen stops naming an agent.
        if m.word == "-" && self.agent.as_ref().is_some_and(|a| a.by_screen) {
            self.close_agent(at);
            return None;
        }
        wake
    }

    // ---- input and control -------------------------------------------

    /// A person's hands were on the session: the tracker attributes a turn
    /// that opens within 5 s to them (a run a waiting name opens later too,
    /// [`Typed`]), and the first input of a burst writes `mark - human
    /// by=human` on the session (the bytes are never recorded).
    pub(crate) fn human_input(&mut self, burst: bool, at: Stamp) {
        self.resync();
        self.arrivals += 1;
        self.typed.input = Some((self.arrivals, at));
        if let Some(a) = self.agent.as_mut() {
            a.tracker.note_input(Actor::Human, at);
        }
        if burst {
            let _ = self.log.mark(
                Mark::new(Kind::Human, Src::Input, Conf::Strong)
                    .at(Node::SESSION)
                    .by(Actor::Human),
                at,
            );
        }
    }

    /// A `ctl turn` exchange from `by`, ledger id `ledger`, handed its input to
    /// the seam (`cmd_turn` calls this at the exchange's first byte and again
    /// at each Enter): the turn it opens carries `by=` and `ledger=` (of a run
    /// a waiting name opens later too, [`Typed`]); a resting turn closes now.
    /// Returns the tracker's wake (none: the exchange leaves no settle
    /// pending).
    pub(crate) fn ctl_turn(&mut self, by: Actor, ledger: u64, at: Stamp) -> Option<u64> {
        self.resync();
        self.arrivals += 1;
        self.typed.ctl = Some((self.arrivals, by, ledger, at));
        let Self { log, agent, .. } = self;
        agent
            .as_mut()
            .and_then(|a| a.tracker.ctl_turn(log, by, ledger, at))
    }

    /// The host's settle wake: a resting word that has held closes its turn.
    /// Returns the next wake.
    pub(crate) fn tick(&mut self, at: Stamp) -> Option<u64> {
        self.resync();
        let Self { log, agent, .. } = self;
        agent.as_mut().and_then(|a| a.tracker.tick(log, at))
    }

    // ---- seamless updates --------------------------------------------

    /// What the handoff manifest carries: the epoch and the node-id counters
    /// plus [`LOG_ID_RESERVE`], remembered as this process's ceiling.
    pub(crate) fn handoff_carry(&mut self) -> LogCarry {
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

    /// Whether this process issued a node id past the counters the last
    /// manifest carried: the successor would issue it again, so the Commit
    /// stands down.
    pub(crate) fn past_handoff_ceiling(&self) -> bool {
        self.ceiling.is_some_and(|c| {
            let now = self.log.id_counters();
            now.run > c.run || now.turn > c.turn || now.step > c.step
        })
    }
}

// ---------------------------------------------------------------------------
// The settle wakes the event loop owes.
// ---------------------------------------------------------------------------

/// The settle wakes the event loop owes, by session: one entry per session
/// whose turn tracker has a pending settle. Empty on an idle machine, so the
/// loop folds nothing (`DeadlineOwner::SessionLog`).
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
    /// THE SESSION LOG's settle ticks: each session whose turn tracker's wake
    /// is due is ticked under its timeline's leaf lock (the lock the sweep
    /// takes on this thread), and its next wake replaces the entry. Nothing
    /// owed, nothing done. A wake the tick hands back that is not in the future
    /// is dropped, never re-armed: the loop cannot spin on one, and the next
    /// verdict applies an overdue settle first, dated when its word first held.
    pub(crate) fn tick_session_logs(&mut self) {
        if self.session_status.slog_wakes.is_empty() {
            return;
        }
        let now = crate::turn_ledger::now_ms();
        for id in self.session_status.slog_wakes.due(now) {
            let wake = self
                .pool
                .get(id)
                .and_then(|s| {
                    s.ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .slog_tick()
                })
                .filter(|w| *w > now);
            self.session_status.slog_wakes.note(id, wake);
        }
    }

    /// The soonest settle tick owed, as an instant for the loop's deadline
    /// fold (`DeadlineOwner::SessionLog`), or `None` while no turn rests.
    pub(crate) fn next_session_log_wake(&self) -> Option<std::time::Instant> {
        let wake = self.session_status.slog_wakes.next()?;
        let now = crate::turn_ledger::now_ms();
        Some(std::time::Instant::now() + std::time::Duration::from_millis(wake.saturating_sub(now)))
    }
}

// ---------------------------------------------------------------------------
// The `log` and `brief` verbs' reading of a log.
// ---------------------------------------------------------------------------

/// The `log` verb's usage line.
pub(crate) const LOG_USAGE: &str = "ERR usage: log [<node>] [lvl=session|run|turn|step] [since=<seq> ep=<epoch>] [n=<count>] [kinds=<k>[,...]] [class=turn] [tier=content] [--json]\n";
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

/// The `since=` / `ep=` pair: both, or neither (`since=` alone cannot say which
/// epoch's seq it is: seqs restart at a seamless update).
fn cursor_of(since: Option<u64>, ep: Option<u32>) -> Result<Option<Cursor>, ()> {
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
    /// Parse a `log` request's arguments. The flag `--json` (or bare `json`,
    /// the spelling every json-capable verb takes) and the one `<node>` may
    /// stand anywhere among them.
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
    /// Parse a `brief` request's arguments; the one `<node>` (or `all`) may
    /// stand anywhere among them.
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

/// Every node's parent, as far as the log still knows it: the open records in
/// its window, and the nodes still open.
fn parents(log: &Log) -> HashMap<Node, Node> {
    let mut map: HashMap<Node, Node> = log
        .window()
        .filter_map(|r| r.par.map(|p| (r.node, p)))
        .collect();
    for e in log.open_nodes() {
        map.insert(e.node, e.parent);
    }
    map
}

/// Whether `node` lies in the subtree of `root`.
fn in_subtree(node: Node, root: Node, parents: &HashMap<Node, Node>) -> bool {
    if root == Node::SESSION {
        return true;
    }
    let mut at = node;
    for _ in 0..=aterm_slog::MAX_OPEN {
        if at == root {
            return true;
        }
        match parents.get(&at) {
            Some(p) => at = *p,
            None => return false,
        }
    }
    false
}

/// A record as one JSON object: the line's tokens as keys, fields at or below
/// `tier`, tokens and text unencoded.
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

/// The cursor gap as one JSON object, in a `--json` reply: `{"op":"gap",
/// "why":…,"lost":…,"resync_to":…}`, so every body line parses.
fn gap_json(g: &Gap) -> String {
    use aterm_json::Value;
    let mut o = aterm_json::Map::new();
    o.insert("op".into(), Value::from("gap"));
    o.insert("why".into(), Value::from(g.why.word()));
    o.insert("lost".into(), Value::from(g.lost));
    o.insert("resync_to".into(), Value::from(g.resync_to));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_string())
}

/// The cursor gap as `log` and `brief` print it.
fn gap_line(g: &Gap) -> String {
    format!(
        "gap why={} lost={} resync_to={}",
        g.why.word(),
        g.lost,
        g.resync_to
    )
}

/// What a `log` reply needs of a log, copied under the timeline's leaf lock:
/// the records it shows and the header's numbers. The reply is rendered from
/// the copy after the lock is released ([`crate::control::cmd_log`]), so a
/// reader asking for the whole ring as JSON never holds the lock the event
/// loop's status sweep takes, under the terminal guard, for the length of
/// the encoding.
#[derive(Clone, Debug)]
pub(crate) struct LogCopy {
    recs: Vec<Rec>,
    gap: Option<Gap>,
    /// Matching records left out by `n=`.
    more: usize,
    as_of: u64,
    ep: u32,
    oldest: u64,
    lost: u64,
}

impl LogCopy {
    /// The records `q` asks for, and the header: a filter and a clone of the
    /// records shown, nothing encoded. (Named apart from every other `of`:
    /// the lock-order census resolves a call by its name.)
    pub(crate) fn copied_from(log: &Log, q: &LogQuery) -> LogCopy {
        let read = log.read(q.cursor.unwrap_or(Cursor::new(log.ep(), 0)), usize::MAX);
        let tree = (q.node != Node::SESSION).then(|| parents(log));
        let matching: Vec<&Rec> = read
            .recs
            .into_iter()
            .filter(|r| tree.as_ref().is_none_or(|p| in_subtree(r.node, q.node, p)))
            .filter(|r| q.lvl.is_none_or(|l| r.node.lvl <= l))
            .filter(|r| q.kinds.is_empty() || q.kinds.contains(&r.k))
            .filter(|r| !q.turn_class || r.is_turn_class())
            .collect();
        let more = q.n.map_or(0, |n| matching.len().saturating_sub(n));
        LogCopy {
            recs: matching[more..].iter().map(|r| (*r).clone()).collect(),
            gap: read.gap,
            more,
            as_of: log.last_seq(),
            ep: log.ep(),
            oldest: log.oldest_seq(),
            lost: log.lost(),
        }
    }
}

/// The `log` reply from its copy: `OK <n> v=2 as_of=<seq> ep=<e>
/// oldest=<seq>[ lost=<n>][ more=<k>]`, a gap line when the reader's cursor
/// is behind the ring or of another epoch (`gap why= lost= resync_to=`, or
/// under `--json` a `{"op":"gap",…}` object, so every body line is one
/// object), then one record per line, oldest first.
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
    /// A test's probe, called as a `log` or `brief` reply starts encoding
    /// ([`render_log_copy`], [`BriefCopy::brief`]) on this thread: what it
    /// finds held there, the encoding runs under.
    pub(crate) static ENCODE_PROBE: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run this thread's [`ENCODE_PROBE`], if a test set one.
#[cfg(test)]
fn encode_probe() {
    ENCODE_PROBE.with(|p| {
        if let Some(probe) = p.borrow().as_ref() {
            probe();
        }
    });
}

/// The `log` reply ([`render_log_copy`] over [`LogCopy::copied_from`]), in one call
/// (tests; the verb copies under the lock and renders after it).
#[cfg(test)]
pub(crate) fn render_log(log: &Log, q: &LogQuery) -> String {
    render_log_copy(&LogCopy::copied_from(log, q), q)
}

/// What a brief needs of a log, copied under the timeline's leaf lock (as
/// [`LogCopy`]): the records the reader asked for, the gap that read
/// crossed, and the fold's state.
#[derive(Clone, Debug)]
pub(crate) struct BriefCopy {
    recs: Vec<Rec>,
    gap: Option<Gap>,
    state: aterm_slog::SessionState,
}

impl BriefCopy {
    /// The records past `cursor` (all of them: none) in `node`'s subtree, the
    /// gap that read crossed, and the state.
    pub(crate) fn copied_from(log: &Log, node: Node, cursor: Option<Cursor>) -> BriefCopy {
        let read = log.read(cursor.unwrap_or(Cursor::new(log.ep(), 0)), usize::MAX);
        let tree = (node != Node::SESSION).then(|| parents(log));
        BriefCopy {
            recs: read
                .recs
                .into_iter()
                .filter(|r| tree.as_ref().is_none_or(|p| in_subtree(r.node, node, p)))
                .cloned()
                .collect(),
            gap: read.gap,
            state: log.state().clone(),
        }
    }

    /// Its brief within `budget` tokens at `now_wall`.
    pub(crate) fn brief(&self, sid: &str, budget: usize, now_wall: u64) -> aterm_slog::Brief {
        #[cfg(test)]
        encode_probe();
        aterm_slog::brief(sid, &self.recs, self.gap, &self.state, budget, now_wall)
    }
}

/// One session's brief: the records past the reader's cursor in the subtree
/// asked for, against the whole fold, within `budget` tokens, with the gap
/// that read crossed (a cursor of another epoch, ahead of the head, or behind
/// the ring) said as `log` says it. The text starts `OK <n> brief sid=…`.
/// In one call (tests; the verb copies under the lock and renders after it).
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
    /// Its rank among the parts `brief all` keeps first: a session that needs
    /// something (most urgent first), then the most recently active.
    fn rank(&self) -> (u8, std::cmp::Reverse<u64>) {
        let need = self
            .copy
            .state
            .top_need()
            .map_or(u8::MAX, aterm_slog::need_precedence);
        (need, std::cmp::Reverse(self.copy.state.last_t))
    }

    /// Its brief with the `OK <n> ` prefix dropped, and its token estimate.
    fn text(&self, budget: usize) -> (String, usize) {
        let b = self.copy.brief(&self.sid, budget, self.now_wall);
        let text = match b.text.split_once(' ') {
            Some(("OK", tail)) => tail
                .split_once(' ')
                .map_or(tail.to_string(), |(_, t)| t.to_string()),
            _ => b.text,
        };
        (text, b.used)
    }
}

/// The instance-wide brief (`brief all`): `OK <n> brief all sessions=<k>
/// v=2 budget=<b> used=<u>`, then each kept session's brief without its `OK
/// <n> ` prefix, then `more=sessions:<k>` when sessions were left out. The
/// whole reply fits `budget` (`used=` is its estimate): the budget, less the
/// header and the trailer, is shared out evenly among the most sessions
/// whose one-line form fits their share, ranked by need then by recent
/// activity ([`BriefPart::rank`]); each brief within its share sheds as one
/// session's brief does. When not even one fits, the header and the trailer
/// alone answer.
pub(crate) fn render_brief_all(parts: &[BriefPart], budget: usize) -> String {
    let n = parts.len();
    let fixed = aterm_slog::tokens(
        format!("OK {n:05} brief all sessions={n} v=2 budget={budget} used={budget:05}\n").len()
            + format!("more=sessions:{n}\n").len(),
    );
    let avail = budget.saturating_sub(fixed);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (parts[i].rank(), i));
    // Each part's one-line floor ([`aterm_slog::brief`] at budget 0).
    let floor: Vec<usize> = parts.iter().map(|p| p.text(0).1).collect();
    // The most sessions, top-ranked, whose floors all fit an even share: then
    // each brief at that share uses at most it, and the sum fits.
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
    let texts: Vec<String> = keep.iter().map(|&i| parts[i].text(share).0).collect();
    let mut body = String::new();
    for t in &texts {
        body.push_str(t);
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

#[cfg(test)]
#[path = "session_log_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS WATCHDOG (`docs/HARNESS-CONTRACT.md`, "The watchdog"): what aterm does about a
//! contract 4 harness that gives no sign of life, and about one that ignores a restart.
//!
//! COUNTING. A claim with an open record stream is counted on its stream lane
//! (`control_stream::serve`, [`Count`], [`look`]): no `beat` for `BEATS_MISSED × BEAT_EVERY`
//! (30 s) of the awake clock is missed beats. The count runs from the latest of the stream's
//! open, the last `beat`, the machine's last wake (`Power::Woke`) and a stopped claimant's
//! resume, so a Mac asleep misses no beat. Where the platform has no awake clock nothing is
//! counted (Windows has none, and no harness can claim there: D20).
//!
//! MISSED BEATS. The tab's state reads `unknown`, written by aterm (`mark agent word=unknown
//! src=aterm`), unless the kernel reports the claimant stopped: that reads `stopped`, and a
//! stopped claimant is never restarted for the beats it misses; its resume starts the count
//! again. Otherwise the watchdog writes `restart cause=beat`
//! ([`crate::restart_act::watchdog_restart`], which the open stream carries as a record), and
//! HX-06's scoped watch gives the harness three more beat periods ([`RESTART_BOUND`]) to obey.
//!
//! THE PROGRESS WATCH (HX-15i1). The beat is the harness's liveness; the agent's progress is
//! watched apart, on the same lane and the same awake clock ([`Count::progress`]): an `ask` the
//! stream holds open `restart_act::GRACE` past the time its record said the harness will answer
//! it, and a turn the stream holds open with no record of progress (`turn`, `tool`, `subagent`,
//! `msg`, `diff`, `ask`, `asked`) for the policy's `stall_term_after_s` ([`World::stall_after`]),
//! each read `wall:unresponsive` (`mark agent word=wall:unresponsive src=aterm`, whose stall step
//! raises the need `unresponsive`) and owe one `restart cause=stall`, bounded by that same
//! `stall_term_after_s` (D16); the policy's `stall_term = false` keeps the stall a warning, read
//! and raised and never restarted ([`World::stall_term`]). Both are counted from the latest of
//! the stream's open, a stopped claimant's resume and a hand's lift at the earliest
//! ([`Count::floor`]): an ask's `will_in_ms` after the later of its arrival and that floor, a
//! turn's bound after the later of its last progress and that floor. A harness holds its
//! answers while a hand is on the tab, and a stopped one sends nothing, so neither time is a
//! stall; a claimant the kernel reports stopped reads `stopped`, beats or none. A stall is named
//! by what it was counted from ([`StallOf`]), and the claim keeps the one it acted on, so a
//! reconnect, a wall or a pause inside it acts on it no more. The turn rule waits while an ask,
//! the stream's wall or one of its tool or subagent steps stands open: the agent then waits on a
//! person, a wall or a tool, which is no stalled turn. Missed beats come first: a harness that
//! sends none reads `unknown` or `stopped`, whatever its agent's progress. The input-stall rule
//! stays beside both for a tab with a screen ([`crate::input_stall`]).
//!
//! ENDING. A harness that ignored a restart (the beat restart, the `restart` verb, the stall
//! restart) has its process group sent `SIGTERM` and then `SIGKILL` by that watch; once the
//! claimant is proven gone by its pid and kernel start ([`after_unobeyed`]) the watchdog
//! releases its claim at once (the re-exec grace is for a harness that re-execs itself, not
//! for one aterm ended), ends the child the claim kept by the identity aterm keeps
//! ([`crate::harness_claim::KeptChild::end`]: it shares the claimant's group when kept, so the
//! group's `SIGKILL` usually ended it; this ends one that left the group), and hands the tab to
//! the relaunch: its need reads `unsupervised` until a harness claims it again. A contract 4
//! claim whose connection closed and whose re-exec grace ran out, streaming or not, is ended
//! the same way ([`past_grace`]): a dead claimant's kept child is ended; a live claimant's
//! group gets `SIGTERM`, then `SIGKILL`, and then its kept child is ended.
//!
//! Every act (the restart, the group's signals, the kept child's end) waits while a person
//! holds the tab, another hand drives it, or a person's input is younger than `quiet=`, the
//! policy's `human_grace_s` ([`hand`]). Every signal goes only to the claimant proven by its
//! pid and kernel start, to its process group while it leads its tab, or to the kept child by
//! its pid and birth.
//!
//! RELAUNCH (HX-15f). Once the harness is ended and its kept child is read gone by its identity,
//! the tab is relaunched by typing `<tool> run [--account <label>] [--home <dir>] [--model
//! <word>] -- <resume…>` ([`relaunch`]): the claim's registry row's verified tool, never a word a
//! record names; the resume arguments of the claimant's latest `session` record that names any,
//! else of the kept child's built-in snapshot ([`take_snapshot`], read again as the ending starts,
//! [`refresh_snapshot`]); the claim's `login=` as the account; `--model` only where a `restart
//! model=` named one that the harness never obeyed. Every word passes
//! [`crate::harness_claim::validate_run`] and is quoted for the shell, with a `cd` back to the
//! conversation's directory where the shell stands elsewhere, and the line waits out alab-wire's
//! back-off (`relaunch::Ladder`: 1 s, 10 s, 60 s, 600 s, forgiven after 600 s of health). An
//! attempt that types nothing (the kept child still reads at its birth, the typer held or
//! refused) is tried again on the same ladder, for as long as the tab stands handed over by this
//! ending. A claimant that released its claim itself, a stream whose last `state` was `ended`, a
//! claim whose named child aterm never kept, and a vendor with neither a `session` nor a snapshot
//! are not relaunched: the tab stays `unsupervised`.

use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use alab_wire::record::{BEAT_EVERY_MS, BEATS_MISSED};
use alab_wire::relaunch::Ladder;
use aterm_agent::harness::relaunch_type::RunLineRefusal;
use aterm_uds::process::{EndControl, EndControlLive, EndRefusal, Ended};

use aterm_agent::harness::restart_stamp::Cause;

use crate::SessionCtx;
use crate::harness_claim::{KeptChild, Proven, Scoped};
use crate::session_timeline::{Resume, StallOf, Unsupervised, Watched, WatchedClaimant};

/// Awake time with no `beat` that is missed beats: `BEATS_MISSED × BEAT_EVERY`, 30 s.
pub(crate) const MISSED_AFTER_MS: u64 = BEAT_EVERY_MS * BEATS_MISSED as u64;

/// How long a harness has to obey the watchdog's `restart cause=beat` before its group gets
/// `SIGTERM` (D16): three more beat periods of the awake clock, the harness's own heartbeat
/// contract, where the verb's and the stall's restart wait the policy's `stall_term_after_s`.
pub(crate) const RESTART_BOUND: Duration = Duration::from_millis(MISSED_AFTER_MS);

/// How long the watchdog waits for a claimant it saw signalled to be gone before it judges it
/// alive ([`after_unobeyed`], [`past_grace`]): a hang detector, not a budget.
const GONE_WITHIN: Duration = Duration::from_secs(60);

/// WHAT THE WATCHDOG READS of the machine, the kernel and the person's policy: [`Live`] in a
/// shipped build, a test's own world in a test.
pub(crate) trait World: Sync {
    /// The awake clock in milliseconds ([`aterm_uds::process::awake_ms`]); `None` where the
    /// platform has none, and then nothing is counted.
    fn awake_ms(&self) -> Option<u64>;
    /// The awake clock at the machine's last wake (`Power::Woke`); `None` before any.
    fn woke_ms(&self) -> Option<u64>;
    /// Whether the kernel reports process `pid` stopped; `None` where it cannot say.
    fn stopped(&self, pid: u32) -> Option<bool>;
    /// Whether the claimant `proven` still runs as itself (its pid at its kernel start).
    fn alive(&self, proven: &Proven) -> bool;
    /// How long a person's input holds every act back (`quiet=`).
    fn quiet(&self) -> Duration;
    /// How long an act held back waits before it looks at the hands again.
    fn look_again(&self) -> Duration;
    /// How long an open turn may make no progress before the progress watch reads it stalled,
    /// on the awake clock: the policy's `stall_term_after_s`, which also bounds the stall
    /// restart; `None` where the policy says never (`0`).
    fn stall_after(&self) -> Option<Duration>;
    /// Whether the policy restarts a stalled agent (`stall_term`, on by default): `false` keeps
    /// the stall a warning, read and raised as a need and never restarted (principle 3).
    fn stall_term(&self) -> bool;
    /// An act of the watchdog on tab `sid` waits out `hand`: told once per hand per wait.
    fn waits(&self, sid: &str, act: Act, hand: Hand);
    /// What resumes the conversation of process `pid` in tab `sid`, by its vendor's built-in
    /// relaunch snapshot (the HX-15 plan's D22); `None` where no reader reads one or it proved
    /// no conversation.
    fn snapshot(&self, sid: &str, pid: u32) -> Option<Resume>;
    /// Wait out `pause`, the relaunch's back-off, before tab `sid`'s relaunch, looking at
    /// `stands` (the tab still stands handed over by this ending) as it waits: whether it still
    /// stands at the end (`false`: the relaunch stands down).
    fn pause(&self, sid: &str, pause: Duration, stands: &dyn Fn() -> bool) -> bool;
    /// Type the relaunch `words` on tab `sid` at its shell's prompt, quoted for that shell, with
    /// a `cd` back to `cwd` where the shell stands elsewhere, under the relaunch's lane
    /// ([`aterm_agent::harness::relaunch_type::type_run_line`]).
    ///
    /// # Errors
    /// [`RunLineRefusal`]: nothing was typed, or (`Unanswered`) the line may have run.
    fn type_run(
        &self,
        sid: &str,
        words: &[String],
        cwd: Option<&str>,
    ) -> Result<(), RunLineRefusal>;
}

/// The live [`World`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Live {
    /// The `quiet=` an act this one follows already waited under ([`after_unobeyed`]); `None`
    /// reads the person's policy at each look ([`crate::harness_host::watchdog_quiet`]).
    quiet: Option<Duration>,
}

impl Live {
    /// The live world under the person's policy as it stands at each look.
    pub(crate) const POLICY: Self = Self { quiet: None };
}

impl World for Live {
    fn awake_ms(&self) -> Option<u64> {
        aterm_uds::process::awake_ms()
    }

    fn woke_ms(&self) -> Option<u64> {
        woke_ms()
    }

    fn stopped(&self, pid: u32) -> Option<bool> {
        crate::input_stall::claimant_stopped(pid)
    }

    fn alive(&self, proven: &Proven) -> bool {
        aterm_agent::harness::restart_stamp::alive_as(proven.pid, &proven.start)
    }

    fn quiet(&self) -> Duration {
        self.quiet
            .unwrap_or_else(crate::harness_host::watchdog_quiet)
    }

    fn look_again(&self) -> Duration {
        crate::restart_act::look_again()
    }

    fn stall_after(&self) -> Option<Duration> {
        crate::harness_host::stall_term_after()
    }

    fn stall_term(&self) -> bool {
        crate::harness_host::stall_term()
    }

    fn waits(&self, sid: &str, act: Act, hand: Hand) {
        aterm_log::info!(
            "harness watchdog @{sid}: its {} waits: {}",
            act.words(),
            hand.words()
        );
    }

    fn snapshot(&self, sid: &str, pid: u32) -> Option<Resume> {
        // A test build reads no live snapshot: the live read dials this window's own socket.
        if cfg!(test) {
            return None;
        }
        live_snapshot(sid, pid)
    }

    fn pause(&self, sid: &str, pause: Duration, stands: &dyn Fn() -> bool) -> bool {
        #[cfg(test)]
        if let Some(stood) = tap::paused(sid, pause, stands) {
            return stood;
        }
        aterm_log::info!(
            "harness watchdog @{sid}: the relaunch waits {}s",
            pause.as_secs()
        );
        let until = std::time::Instant::now() + pause;
        loop {
            if !stands() {
                return false;
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(self.look_again()));
        }
    }

    fn type_run(
        &self,
        sid: &str,
        words: &[String],
        cwd: Option<&str>,
    ) -> Result<(), RunLineRefusal> {
        #[cfg(test)]
        if let Some(typed) = tap::typed(sid, words) {
            return typed;
        }
        let opts = drive_opts(sid).ok_or(RunLineRefusal::Unread("no-socket"))?;
        aterm_agent::harness::relaunch_type::type_run_line(&opts, sid, words, cwd)
    }
}

/// The live read of process `pid`'s built-in relaunch snapshot in tab `sid` ([`World::snapshot`]):
/// what resumes its conversation, `None` where nothing reads it or it proved none.
fn live_snapshot(sid: &str, pid: u32) -> Option<Resume> {
    let opts = drive_opts(sid)?;
    let snap = aterm_agent::harness::relaunch::snapshot_of(&opts, sid, pid).ok()?;
    let words = aterm_agent::harness::relaunch::resume_words(&snap)?;
    Some(Resume {
        words,
        cwd: (!snap.cwd.is_empty()).then(|| snap.cwd.clone()),
    })
}

/// What the relaunch's live reads and its typer are told: the person's home, the harness state
/// directory and this window's own socket, for tab `sid` ([`crate::harness_host::window_drive_opts`]);
/// `None` where any is not known.
fn drive_opts(sid: &str) -> Option<aterm_agent::harness::upgrade_drive::Opts> {
    let mut opts = crate::harness_host::window_drive_opts(
        aterm_primer::home_dir()?,
        crate::harness_host::harness_state()?,
        Some(crate::proxy::self_sock_path()?),
    );
    opts.only_sid = Some(sid.to_string());
    Some(opts)
}

/// THE RELAUNCH'S TAP for this crate's tests that end a real harness through the live world
/// ([`Live`]): a tab tapped here has its back-off and its relaunch words recorded, and nothing
/// is slept or typed.
#[cfg(test)]
#[path = "harness_watchdog_tap_tests.rs"]
pub(crate) mod tap;

/// The awake clock at the last `Power::Woke` this process heard, plus one (`0`: none yet).
static WOKE_AT: AtomicU64 = AtomicU64::new(0);

/// The process's one sleep-and-wake listener for the watchdog, started at the first read.
static LISTENING: OnceLock<Option<aterm_uds::power::Listening>> = OnceLock::new();

/// The awake clock at the machine's last wake, as this process heard it; the first read starts
/// the listener (macOS; elsewhere nothing is heard, and Linux's awake clock stands still in a
/// suspend anyway).
fn woke_ms() -> Option<u64> {
    LISTENING.get_or_init(|| {
        aterm_uds::power::listen(Box::new(|power| {
            if power == aterm_uds::power::Power::Woke
                && let Some(now) = aterm_uds::process::awake_ms()
            {
                WOKE_AT.store(now.saturating_add(1), Ordering::Release);
            }
        }))
    });
    WOKE_AT.load(Ordering::Acquire).checked_sub(1)
}

/// What one look at a stream's beats found ([`Count::look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    /// Beats come: they are missed if none comes by `due` on the awake clock.
    Beating { due: u64 },
    /// Beats are missed and the kernel reports the claimant stopped: it reads `stopped`, and
    /// is never restarted for them.
    Stopped,
    /// Beats are missed: the tab reads `unknown`; `act` while this miss's restart is still to
    /// be written.
    Missed { act: bool },
}

/// ONE STREAM'S BEAT COUNT, on the awake clock: kept by its stream lane for the stream's life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Count {
    /// When the stream opened.
    opened: u64,
    /// When a claimant seen stopped was seen running again.
    resumed: Option<u64>,
    /// The start of the count that ran out, while it still is the count's start.
    missed: Option<u64>,
    /// The restart for that miss was written.
    restarted: bool,
    /// The claimant was last seen stopped.
    stopped: bool,
    /// The hand the restart waits out now, told once ([`World::waits`]).
    waiting: Option<Hand>,
    /// THE PROGRESS WATCH'S HAND FLOOR: the awake clock at the latest look that found a hand on
    /// the tab, or at the first look after one lifted ([`Count::hands`]).
    hand_floor: Option<u64>,
    /// The last look found a hand on the tab.
    held: bool,
    /// THE PROGRESS WATCH: the stall standing now, while it is one.
    stalled: Option<StallOf>,
}

/// What the progress watch reads of a stream ([`Count::progress`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Progress {
    /// The open ask the stream holds whose answer time comes first, counted from the watch's
    /// floor ([`Count::floor`]): that time on the awake clock, and the ask's `seq`.
    pub(crate) ask: Option<(u64, u64)>,
    /// When the turn the stream holds open last made progress, where the turn rule applies (a
    /// turn stands open, and no ask, wall, tool or subagent the stream opened stands open).
    pub(crate) turn_since: Option<u64>,
    /// How long a turn may make no progress ([`World::stall_after`]).
    pub(crate) stall_after: Option<Duration>,
}

/// What one look at a stream's progress found ([`Count::progress`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Moving {
    /// The agent makes progress, or waits on what it may: it is stalled if nothing moves by
    /// `due` on the awake clock (`None`: nothing is due).
    Yes { due: Option<u64> },
    /// It is stalled, counted from `of`.
    Stalled { of: StallOf },
}

impl Count {
    /// A count that starts as the stream opens, at `opened` on the awake clock.
    pub(crate) const fn new(opened: u64) -> Self {
        Self {
            opened,
            resumed: None,
            missed: None,
            restarted: false,
            stopped: false,
            waiting: None,
            hand_floor: None,
            held: false,
            stalled: None,
        }
    }

    /// LOOK AT THE BEATS at `now`, the last having come at `last_beat` and the machine last
    /// woken at `woke`, both on the awake clock; `stopped` reads the claimant's stop state and
    /// is read only where beats are missed or the claimant was last seen stopped. The count
    /// runs from the latest of the open, the last beat, the wake and a resume: a beat, a wake or
    /// a resume starts it again, and ends the miss it had run out on, so the next miss is
    /// restarted again. A claimant seen stopped reads stopped while the kernel says so, beats or
    /// none; the look that finds it running again is its resume.
    pub(crate) fn look(
        &mut self,
        now: u64,
        last_beat: Option<u64>,
        woke: Option<u64>,
        stopped: impl FnOnce() -> Option<bool>,
    ) -> Seen {
        let mut stopped = Some(stopped);
        if self.stopped {
            if stopped.take().and_then(|read| read()) == Some(true) {
                return Seen::Stopped;
            }
            // It runs again: the count starts again from here.
            self.stopped = false;
            self.resumed = Some(now);
        }
        let from = [Some(self.opened), last_beat, woke, self.resumed]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(self.opened);
        if self.missed.is_some_and(|at| at != from) {
            self.missed = None;
            self.restarted = false;
            self.waiting = None;
        }
        if now.saturating_sub(from) < MISSED_AFTER_MS {
            self.stopped = false;
            return Seen::Beating {
                due: from.saturating_add(MISSED_AFTER_MS),
            };
        }
        match stopped.take().and_then(|read| read()) {
            Some(true) => {
                self.stopped = true;
                Seen::Stopped
            }
            _ => {
                self.missed = Some(from);
                Seen::Missed {
                    act: !self.restarted,
                }
            }
        }
    }

    /// The restart for the miss now running out was written.
    pub(crate) const fn restarted(&mut self) {
        self.restarted = true;
        self.waiting = None;
    }

    /// The claimant was seen stopped where its beats still count (the progress watch asks the
    /// kernel before it reads a stall): it reads stopped, and the look that finds it running
    /// again is its resume ([`Self::look`]).
    pub(crate) const fn saw_stopped(&mut self) {
        self.stopped = true;
    }

    /// A look at `now` found a hand on the tab (`held`: a person's hold, another hand, a
    /// person's input within `quiet=`, [`hand`]), or none. The progress watch counts from this
    /// look while a hand holds the tab and from the first look after it lifted: a harness holds
    /// its answers and acts while a person's hand is on the tab (`docs/HARNESS-CONTRACT.md`,
    /// principle 5), so that time is no stall of its agent.
    pub(crate) const fn hands(&mut self, now: u64, held: bool) {
        if held || self.held {
            self.hand_floor = Some(now);
        }
        self.held = held;
    }

    /// WHERE THE PROGRESS WATCH COUNTS FROM, on the awake clock: the latest of the stream's
    /// open, a stopped claimant's resume and a hand's lift ([`Self::hands`]). An ask's answer
    /// time is counted from it ([`crate::session_log::SessionLog::stream_holds`]), and so is a
    /// turn's last progress.
    pub(crate) fn floor(&self) -> u64 {
        [Some(self.opened), self.resumed, self.hand_floor]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(self.opened)
    }

    /// LOOK AT THE AGENT'S PROGRESS at `now` on the awake clock: stalled once an open ask is
    /// `restart_act::GRACE` past its answer time, or the open turn's last progress, counted from
    /// the watch's floor at the earliest ([`Self::floor`]), is `stall_after` old. The stall is
    /// named by what it was counted from ([`StallOf`]): the claim keeps the one it acted on, so
    /// a stall is acted on once until that moves (the ask is answered, a record of progress
    /// comes), whatever paused it or reopened its stream meanwhile.
    pub(crate) fn progress(&mut self, now: u64, seen: Progress) -> Moving {
        let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let floor = self.floor();
        let ask = seen.ask.map(|(due, seq)| {
            (
                due.saturating_add(ms(crate::restart_act::GRACE)),
                StallOf::Ask(seq),
            )
        });
        let turn = seen.turn_since.zip(seen.stall_after).map(|(since, after)| {
            (
                since.max(floor).saturating_add(ms(after)),
                StallOf::Turn(since),
            )
        });
        let first = [ask, turn].into_iter().flatten().min_by_key(|(at, _)| *at);
        match first {
            Some((at, of)) if now >= at => {
                if self.stalled != Some(of) {
                    self.stalled = Some(of);
                    self.waiting = None;
                }
                Moving::Stalled { of }
            }
            first => {
                self.stalled = None;
                Moving::Yes {
                    due: first.map(|(at, _)| at),
                }
            }
        }
    }

    /// The restart waits out `hand`: whether that is news to tell.
    fn waits(&mut self, hand: Hand) -> bool {
        self.waiting.replace(hand) != Some(hand)
    }
}

/// An act of the watchdog ([`World::waits`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// `restart cause=beat`, or the progress watch's `restart cause=stall`.
    Restart,
    /// `SIGTERM` and `SIGKILL` to a live claimant's group past its re-exec grace.
    Group,
    /// The end of the kept child.
    Child,
    /// The relaunch line.
    Relaunch,
}

impl Act {
    const fn words(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Group => "signal to the claimant's group",
            Self::Child => "end of the kept child",
            Self::Relaunch => "relaunch",
        }
    }
}

/// What holds an act of the watchdog back ([`hand`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hand {
    /// A person's hold on the tab.
    Hold,
    /// Another hand: another driver's lease or turn, or another supervisor's unproven claim
    /// ([`crate::restart_act::another_hand`]).
    Another,
    /// A person's input younger than `quiet=`.
    Person,
}

impl Hand {
    /// What holds the act back, in words.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::Hold => "a person holds the tab",
            Self::Another => "another hand drives the tab",
            Self::Person => "a person gave the tab input within quiet=",
        }
    }
}

/// WHAT HOLDS AN ACT OF THE WATCHDOG BACK on `ctx`'s tab at `now_us`, `None` where nothing
/// does: a person's hold, another hand ([`crate::restart_act::another_hand`]), or a person's
/// input younger than `quiet`, the `quiet=` the built-in stall remedy's restart passes too.
pub(crate) fn hand(ctx: &SessionCtx, now_us: u64, quiet: Duration) -> Option<Hand> {
    if ctx.fabric.hold().is_some() {
        return Some(Hand::Hold);
    }
    if crate::restart_act::another_hand(ctx, now_us) {
        return Some(Hand::Another);
    }
    let quiet_ms = u64::try_from(quiet.as_millis()).unwrap_or(u64::MAX);
    ctx.human_input
        .ms_since(now_us)
        .is_some_and(|ms| ms < quiet_ms)
        .then_some(Hand::Person)
}

/// Wait until no hand holds `ctx`'s tab ([`hand`]) for `act`, looking again each
/// [`World::look_again`]: the live session then, `None` once it is gone.
fn free_hands(ctx: &Weak<SessionCtx>, world: &dyn World, act: Act) -> Option<Arc<SessionCtx>> {
    let mut told = None;
    loop {
        let live = ctx.upgrade()?;
        let Some(held) = hand(&live, crate::metrics::now_us(), world.quiet()) else {
            return Some(live);
        };
        if told.replace(held) != Some(held) {
            world.waits(live.self_id.as_str(), act, held);
        }
        drop(live);
        std::thread::sleep(world.look_again());
    }
}

/// ONE LOOK OF A STREAM LANE'S WATCHDOG, for the stream of `proven` open on connection `conn`
/// of `ctx`'s session: count its beats ([`Count::look`]) and watch its agent's progress
/// ([`Count::progress`]), publish what that reads of the claimant
/// ([`crate::session_timeline::SessionTimeline::set_watched`]: missed beats first, then a
/// claimant the kernel reports stopped, then a stall), and write the restart a miss or a stall
/// owes once no hand holds the tab ([`crate::restart_act::watchdog_restart`], `cause=beat`
/// bounded by [`RESTART_BOUND`], `cause=stall` by [`World::stall_after`]). A stall is acted on
/// once per claim ([`crate::session_timeline::StreamState::stall_acted`]), and under the
/// policy's `stall_term = false` it is read and raised and never restarted. Returns how long
/// the lane may wait before the next look is due; `None` where nothing is due (no awake clock,
/// a stream whose claim left it). A miss whose restart was written is looked at again at
/// [`World::look_again`], so a claimant the kernel stops meanwhile reads `stopped`; so is a
/// watched ask or turn while a hand holds the tab, so its lift is seen.
pub(crate) fn look(
    ctx: &Arc<SessionCtx>,
    proven: &Proven,
    conn: u64,
    count: &mut Count,
    world: &dyn World,
) -> Option<Duration> {
    let now = world.awake_ms()?;
    let now_us = crate::metrics::now_us();
    let (last_beat, progress, acted) = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let stream = meta
            .stream
            .as_ref()
            .filter(|stream| stream.proven == *proven && stream.open == Some(conn))?;
        meta.live_scoped(now_us)
            .filter(|scoped| scoped.proven == *proven)?;
        (stream.last_beat_ms, stream.progress_ms, stream.stall_acted)
    };
    let held = hand(ctx, now_us, world.quiet());
    count.hands(now, held.is_some());
    let seen = count.look(now, last_beat, world.woke_ms(), || {
        world.stopped(proven.pid)
    });
    let holds = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .slog()
        .stream_holds(count.floor());
    let moving = count.progress(
        now,
        Progress {
            ask: holds.ask_due,
            turn_since: (holds.turn && !holds.ask && !holds.wall && !holds.steps)
                .then(|| progress.unwrap_or(0)),
            stall_after: world.stall_after(),
        },
    );
    // A claimant the kernel stops while its beats still count reads stopped, never stalled.
    let seen = match (seen, moving) {
        (Seen::Beating { .. }, Moving::Stalled { .. })
            if world.stopped(proven.pid) == Some(true) =>
        {
            count.saw_stopped();
            Seen::Stopped
        }
        _ => seen,
    };
    let watched = match (seen, moving) {
        (Seen::Stopped, _) => Some(Watched::Stopped),
        (Seen::Missed { .. }, _) => Some(Watched::Missed),
        (Seen::Beating { .. }, Moving::Stalled { .. }) => Some(Watched::Stalled),
        (Seen::Beating { .. }, Moving::Yes { .. }) => None,
    };
    let _ = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_watched(watched.map(|watched| WatchedClaimant {
            watched,
            proven: proven.clone(),
        }));
    let after = |due: u64| Duration::from_millis(due.saturating_sub(now));
    match seen {
        Seen::Beating { due } => match moving {
            Moving::Yes { due: stall } => {
                let next = after(stall.map_or(due, |stall| stall.min(due)));
                // A hand holds the count of a watched ask or turn: its lift is looked for.
                Some(if held.is_some() && stall.is_some() {
                    next.min(world.look_again())
                } else {
                    next
                })
            }
            Moving::Stalled { of } if acted == Some(of) => Some(after(due)),
            Moving::Stalled { of } => match held {
                Some(held) => {
                    if count.waits(held) {
                        world.waits(ctx.self_id.as_str(), Act::Restart, held);
                    }
                    Some(world.look_again())
                }
                None if !world.stall_term() => {
                    aterm_log::info!(
                        "harness watchdog @{}: pid {} beats, but its agent made no progress; it \
                         reads wall:unresponsive, and the policy's stall_term = false keeps \
                         the stall a warning: no restart",
                        ctx.self_id.as_str(),
                        proven.pid
                    );
                    stall_acted(ctx, proven, of);
                    Some(after(due))
                }
                None => {
                    aterm_log::info!(
                        "harness watchdog @{}: pid {} beats, but its agent made no progress \
                         (an ask past its answer time, or a turn past stall_term_after_s); it \
                         reads wall:unresponsive",
                        ctx.self_id.as_str(),
                        proven.pid
                    );
                    // One restart for one stall: written, or left to another restart's watch.
                    let _ = crate::restart_act::watchdog_restart(
                        ctx,
                        proven,
                        Cause::Stall,
                        world.stall_after(),
                        world.quiet(),
                    );
                    stall_acted(ctx, proven, of);
                    Some(after(due))
                }
            },
        },
        Seen::Stopped => Some(world.look_again()),
        Seen::Missed { act: false } => Some(world.look_again()),
        Seen::Missed { act: true } => match held {
            Some(held) => {
                if count.waits(held) {
                    world.waits(ctx.self_id.as_str(), Act::Restart, held);
                }
                Some(world.look_again())
            }
            None => {
                aterm_log::info!(
                    "harness watchdog @{}: pid {} sent no beat for {}s of awake time; it \
                     reads unknown",
                    ctx.self_id.as_str(),
                    proven.pid,
                    MISSED_AFTER_MS / 1000
                );
                // One restart for one miss: written, or left to another restart's watch.
                let _ = crate::restart_act::watchdog_restart(
                    ctx,
                    proven,
                    Cause::Beat,
                    Some(RESTART_BOUND),
                    world.quiet(),
                );
                count.restarted();
                Some(world.look_again())
            }
        },
    }
}

/// The progress watch acted on the stall `of` of `proven`'s claim ([`look`]): kept on the
/// claim's stream state, which outlives the connection.
fn stall_acted(ctx: &SessionCtx, proven: &Proven, of: StallOf) {
    if let Some(stream) = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .stream
        .as_mut()
        .filter(|stream| stream.proven == *proven)
    {
        stream.stall_acted = Some(of);
    }
}

/// Clear what a stream lane's watchdog read of `proven` (`unknown` for missed beats,
/// `stopped`) as its stream, served on connection `conn`, closes: what follows is the grace's
/// reading, or the claim's end. A stream of the same claimant open on another connection by now
/// counts its beats, and what that lane read stands.
pub(crate) fn stream_closed(ctx: &SessionCtx, proven: &Proven, conn: u64) {
    let counted_elsewhere = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .stream
        .as_ref()
        .is_some_and(|stream| {
            stream.proven == *proven && stream.open.is_some_and(|open| open != conn)
        });
    if counted_elsewhere {
        return;
    }
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let ours = timeline
        .agent()
        .watched
        .as_ref()
        .is_some_and(|watched| watched.proven == *proven && watched.watched != Watched::Closed);
    if ours {
        let _ = timeline.set_watched(None);
    }
}

/// Why the watchdog ends a harness ([`end_harness`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// It ignored a restart, and the scoped watch's `SIGTERM` and `SIGKILL` of its group left it
    /// gone: the re-exec grace is skipped.
    Unobeyed,
    /// Its connection closed and its claim's re-exec grace ran out (any contract 4 claim,
    /// streaming or not, D28).
    Grace,
    /// It exited, and the status sweep released its claim, grace or not
    /// ([`crate::session_timeline::publish_release_proven_if_gone`]).
    Exited,
}

/// What the watchdog's ending came to ([`end_harness`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// The session is gone: nothing was done.
    SessionGone,
    /// Another ending of the same claimant runs (an unobeyed restart's and the sweep's release
    /// of the same exit): it does the work.
    Running,
    /// The claimant still runs (it left its tab, or outlived its signals): its kept child was
    /// left alone and the tab handed to nobody.
    ClaimantLives,
    /// A claim placed while the ending waited supervises the tab (the claimant reconnected and
    /// claimed it again, another supervisor holds it, or another claim keeps the kept child):
    /// nothing was signalled, and the kept child was left alone.
    Reclaimed,
    /// The harness is ended and the tab handed to the relaunch (`unsupervised`): what the kept
    /// child's end came to, `None` where the claim kept none.
    Ended {
        child: Option<Result<Ended, EndRefusal>>,
    },
}

/// THE WATCHDOG'S ENDING OF THE HARNESS `scoped` held by `holder` on `ctx`'s tab, for `why`,
/// reading `world` and ending the kept child through `control`. Every act first waits out the
/// hands on the tab ([`hand`]).
///
/// 0. The kept child's snapshot is read again while it still reads at its birth
///    ([`refresh_snapshot`]): the conversation it runs now, which the relaunch resumes.
/// 1. A claimant that still runs (past its grace) has its process group sent `SIGTERM`, then
///    `SIGKILL` ([`crate::restart_act::end_scoped_group`], which signals the group only while
///    the claimant leads its tab at its kernel start); one that outlives them, or that left its
///    tab, is let be ([`Ending::ClaimantLives`]). Any claim standing on the tab once the hands
///    lift (the claimant reconnected and claimed again, or another supervisor did) stops the
///    ending ([`Ending::Reclaimed`]): the lapsed claim is gone, so a standing one is new.
/// 2. The claim is released where it still stands under the same authority: an unobeyed
///    restart's claimant is gone, so the re-exec grace its closed connection would have kept
///    is skipped.
/// 3. The kept child is ended by the identity aterm keeps
///    ([`crate::harness_claim::KeptChild::end`]): a pid that no longer reads at its birth is
///    never signalled, and a child another standing claim keeps by then is left alone
///    ([`Ending::Reclaimed`]).
/// 4. The tab is handed to the relaunch: its need reads `unsupervised`
///    ([`crate::session_timeline::SessionMeta::unsupervised`]) until any claim is placed on it,
///    unless a claim stands on it already or an earlier ending handed it over. The ending that
///    handed it over relaunches it ([`relaunch`]) while that handover stands.
pub(crate) fn end_harness<C: EndControl>(
    ctx: &Weak<SessionCtx>,
    holder: &str,
    scoped: &Scoped,
    why: Why,
    world: &dyn World,
    control: &mut C,
) -> Ending {
    let proven = &scoped.proven;
    let Some(_one) = Ends::of(ctx, proven) else {
        return Ending::Running;
    };
    let Some(sid) = ctx.upgrade().map(|live| live.self_id.as_str().to_string()) else {
        return Ending::SessionGone;
    };
    if let Some(live) = ctx.upgrade() {
        refresh_snapshot(&live, scoped, world, control);
    }
    if world.alive(proven) {
        let Some(live) = free_hands(ctx, world, Act::Group) else {
            return Ending::SessionGone;
        };
        // The claim this ending follows was released (its grace lapsed, or the sweep released
        // it): one standing now was placed while the hands held the act back.
        let claimed = live
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .live_claim(crate::metrics::now_us())
            .is_some();
        drop(live);
        if claimed {
            aterm_log::info!(
                "harness watchdog @{sid}: {holder} ({}): a claim stands on the tab again; \
                 nothing is signalled",
                why.words()
            );
            return Ending::Reclaimed;
        }
        let signalled = match crate::harness_host::harness_state() {
            Some(state) => crate::restart_act::end_scoped_group(ctx, &state, proven),
            None => {
                aterm_log::info!(
                    "harness watchdog @{sid}: no harness state directory; pid {} is not \
                     signalled",
                    proven.pid
                );
                crate::restart_act::GroupEnd::Unsent
            }
        };
        if signalled == crate::restart_act::GroupEnd::Claimed {
            return Ending::Reclaimed;
        }
        let gone = signalled == crate::restart_act::GroupEnd::Signalled
            && (aterm_agent::harness::upgrade_wake::wait_exit_as(
                proven.pid,
                &proven.start,
                GONE_WITHIN,
            ) || !world.alive(proven));
        if !gone {
            aterm_log::info!(
                "harness watchdog @{sid}: {holder} ({}) still runs; its kept child is left",
                why.words()
            );
            return Ending::ClaimantLives;
        }
    }
    let Some(live) = free_hands(ctx, world, Act::Child) else {
        return Ending::SessionGone;
    };
    if kept_by_another(&live, scoped) {
        aterm_log::info!(
            "harness watchdog @{sid}: {holder} ({}): another claim keeps its child; it is left",
            why.words()
        );
        return Ending::Reclaimed;
    }
    let now_us = crate::metrics::now_us();
    if crate::session_timeline::publish_release_scoped(
        &live,
        scoped,
        now_us,
        aterm_slog::Actor::Aterm,
    )
    .published()
    {
        aterm_log::info!(
            "harness watchdog @{sid}: {holder} is gone ({}); its claim is released, no re-exec \
             grace kept",
            why.words()
        );
    }
    drop(live);
    let child = scoped.child.map(|kept| {
        let ended = kept.end(control, aterm_uds::process::END_GRACE);
        aterm_log::info!(
            "harness watchdog @{sid}: {holder}'s kept child pid {}: {ended:?}",
            kept.pid
        );
        ended
    });
    let Some(live) = ctx.upgrade() else {
        return Ending::SessionGone;
    };
    // A tab another ending handed over already keeps its need's key (and so its age).
    let handed = {
        let mut meta = live.meta.lock().unwrap_or_else(|p| p.into_inner());
        let now_us = crate::metrics::now_us();
        let fresh = meta.live_claim(now_us).is_none() && meta.unsupervised.is_none();
        fresh.then(|| {
            let handover = Unsupervised {
                at_us: now_us,
                holder: holder.to_string(),
            };
            meta.unsupervised = Some(handover.clone());
            handover
        })
    };
    if let Some(handover) = handed {
        aterm_log::info!("harness watchdog @{sid}: {holder} ended; the tab reads unsupervised");
        crate::presence::post_lease_changed(&live.self_id);
        drop(live);
        // The ending that handed the tab over relaunches it; one that found it handed over
        // already leaves the relaunch to the ending that did.
        match relaunch(ctx, scoped, &handover, world, control) {
            Ok(words) => aterm_log::info!(
                "harness watchdog @{sid}: {holder}'s tab is relaunched: {}",
                words.join(" ")
            ),
            Err(left) => aterm_log::info!(
                "harness watchdog @{sid}: {holder}'s tab is not relaunched ({left:?}); it stays \
                 unsupervised"
            ),
        }
    }
    Ending::Ended { child }
}

/// WHY A RELAUNCH TYPED NOTHING ([`relaunch`]): the tab stays `unsupervised`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Left {
    /// The session is gone.
    SessionGone,
    /// The kept child still reads at its birth: two vendor processes never run one
    /// conversation (design 556-557).
    ChildLives,
    /// A claim stands on the tab again.
    Claimed,
    /// The tab no longer stands handed over by this ending: a claim was placed and gone again
    /// while the relaunch waited, or another ending handed it over since. Its own relaunch, or
    /// none, is the tab's now.
    Superseded,
    /// The claim named a child aterm's reads did not keep: nothing reads that vendor gone, and
    /// two vendor processes never run one conversation (design 556-557).
    ChildUnkept,
    /// The claimant released its claim itself (`meta unset supervisor`): a normal exit (D23).
    Released,
    /// The claimant's stream's last applied `state` was `ended`: a normal exit (D23).
    Ended,
    /// No `session` record of the claimant naming a resume and no snapshot of its kept child (a
    /// vendor with no snapshot reader, Grok and Kimi before HX-19, D22): nothing names what to
    /// resume, and a fresh launch is never typed.
    NoRecord,
    /// The claim's registry row is not in the registry any more.
    NoRow,
    /// A word is out of the relaunch grammar or fails
    /// [`crate::harness_claim::validate_run`]: why.
    Invalid(String),
    /// The typer typed nothing.
    Untyped(RunLineRefusal),
}

/// What a relaunch reads of the ended claimant on its tab ([`relaunch`]).
struct Plan {
    /// What resumes its conversation: the arguments after `--`, and its directory.
    resume: Resume,
    /// The account label the line names: the claim's `login=` (D21; the pool's assignment from
    /// HX-29), never the one a `session` record names.
    account: Option<String>,
    /// The model a `restart model=` named for the claimant.
    model: Option<String>,
}

/// What the relaunch of the claimant `scoped` reads on `live`'s tab, under its locks (meta, then
/// the timeline): nothing where a claim stands again, the claimant released its claim itself,
/// its stream's last `state` was `ended` (D23), or its claim named a child aterm never kept; its
/// resume from its latest `session` record where that names one, else from its kept child's
/// snapshot (D22), else nothing: a `session` with no resume arguments names no conversation,
/// and typing none would start a fresh one.
fn plan(live: &SessionCtx, scoped: &Scoped) -> Result<Plan, Left> {
    let meta = live.meta.lock().unwrap_or_else(|p| p.into_inner());
    let proven = &scoped.proven;
    if meta.live_claim(crate::metrics::now_us()).is_some() {
        return Err(Left::Claimed);
    }
    if scoped.claim.child.is_some() && scoped.child.is_none() {
        return Err(Left::ChildUnkept);
    }
    if meta.relaunch.released.as_ref() == Some(proven) {
        return Err(Left::Released);
    }
    if meta
        .stream
        .as_ref()
        .is_some_and(|stream| stream.proven == *proven && stream.ended)
    {
        return Err(Left::Ended);
    }
    let said = live
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .session_of(proven)
        .filter(|session| !session.resume.is_empty())
        .map(|session| Resume {
            words: session.resume.clone(),
            cwd: session.cwd.clone(),
        });
    let resume = match said {
        Some(resume) => resume,
        None => meta
            .relaunch
            .snapshot
            .as_ref()
            .filter(|(child, _)| scoped.child == Some(*child))
            .map(|(_, resume)| resume.clone())
            .ok_or(Left::NoRecord)?,
    };
    let model = meta
        .relaunch
        .model
        .as_ref()
        .filter(|(claimant, _)| claimant == proven)
        .map(|(_, model)| model.clone());
    Ok(Plan {
        resume,
        // The account the claim states, which wins over the one the record said (D21).
        account: scoped.claim.login.clone(),
        model,
    })
}

/// THE RELAUNCH'S WORDS for the claimant `scoped`: its registry row's verified `tool`, then
/// alab-wire's grammar of `plan` (`run [--account <label>] [--model <word>] -- <resume…>`; no
/// `--home` before the account pool, D21), every word held to
/// [`crate::harness_claim::validate_run`].
fn words(scoped: &Scoped, plan: Plan) -> Result<Vec<String>, Left> {
    let registry = aterm_agent::harness::registry::current();
    let tool = registry
        .entry(scoped.claim.row.id)
        .map(|entry| entry.tool.clone())
        .ok_or(Left::NoRow)?;
    let run = alab_wire::Run {
        account: plan
            .account
            .map(|label| alab_wire::claim::Account { label, home: None }),
        model: plan.model,
        resume: plan.resume.words.into_iter().map(OsString::from).collect(),
    };
    let args = run
        .args()
        .map_err(|error| Left::Invalid(error.to_string()))?;
    let mut words = vec![tool.clone()];
    for arg in args {
        words.push(
            arg.into_string()
                .map_err(|_| Left::Invalid("a word that is no UTF-8".to_string()))?,
        );
    }
    crate::harness_claim::validate_run(&words, &tool)
        .map_err(|refusal| Left::Invalid(format!("{refusal:?}")))?;
    Ok(words)
}

/// THE RELAUNCH OF `ctx`'s TAB once the watchdog ended the harness `scoped` and handed the tab
/// over as `handover` (HX-15f; the HX-15 plan's D21 to D23):
///
/// 1. Nothing is typed for a claimant that released its claim itself, a stream whose last
///    `state` was `ended`, a tab a claim stands on again, a claim whose named child aterm never
///    kept, or a claimant with neither a `session` naming a resume nor a snapshot of its kept
///    child ([`plan`]); the words are built and validated ([`words`]): a word out of bounds types
///    nothing. None of these waits.
/// 2. The attempt waits out alab-wire's back-off (`relaunch::Ladder` on the awake clock, kept on
///    the session and counted as the wait starts, so an ending that overlaps this one backs off
///    on it: 1 s, 10 s, 60 s, then 600 s, the streak forgiven after 600 s of health), standing
///    down once the tab no longer stands handed over by this ending, then the hands on the tab
///    ([`hand`]).
/// 3. Nothing is typed while the kept child lives: it must read gone by the identity aterm keeps
///    (its pid at its birth, through `control`), so two vendor processes never run one
///    conversation.
/// 4. The line is typed at the shell's prompt ([`World::type_run`]) while the tab still stands
///    handed over by this ending, with a `cd` back to the conversation's directory. A line
///    typed, or one unanswered (it may have run), is made: a `restart model=` it carried is
///    spent with it. An attempt that typed nothing (the kept child still lives, the typer held
///    or refused) is tried again from step 1 after the ladder's next pause.
///
/// Returns the words typed, or why nothing was.
fn relaunch<C: EndControl>(
    ctx: &Weak<SessionCtx>,
    scoped: &Scoped,
    handover: &Unsupervised,
    world: &dyn World,
    control: &mut C,
) -> Result<Vec<String>, Left> {
    let sid = ctx
        .upgrade()
        .ok_or(Left::SessionGone)?
        .self_id
        .as_str()
        .to_string();
    let stands = || {
        ctx.upgrade()
            .is_some_and(|live| handed_over(&live, handover).is_ok())
    };
    let now = world.awake_ms().unwrap_or(0);
    let mut ladder = {
        let live = ctx.upgrade().ok_or(Left::SessionGone)?;
        let meta = live.meta.lock().unwrap_or_else(|p| p.into_inner());
        Ladder::carried(
            meta.relaunch.streak,
            meta.relaunch
                .last_ms
                .map(|at| Duration::from_millis(now.saturating_sub(at))),
            now,
        )
    };
    let mut pause = ladder.crashed(now);
    loop {
        let (words, cwd) = {
            let live = ctx.upgrade().ok_or(Left::SessionGone)?;
            let plan = plan(&live, scoped)?;
            let cwd = plan.resume.cwd.clone();
            let words = words(scoped, plan)?;
            // The attempt counts as its wait starts.
            let mut attempt = ladder;
            attempt.missed();
            keep_ladder(&live, &attempt, world.awake_ms().unwrap_or(0));
            (words, cwd)
        };
        if !world.pause(&sid, pause, &stands) {
            return Err(Left::Superseded);
        }
        let live = free_hands(ctx, world, Act::Relaunch).ok_or(Left::SessionGone)?;
        handed_over(&live, handover)?;
        let missed = if scoped
            .child
            .is_some_and(|kept| kept.identity(control).is_some())
        {
            Left::ChildLives
        } else {
            match world.type_run(&sid, &words, cwd.as_deref()) {
                // A line that went unanswered may have run: it counts as made, and is not typed
                // again.
                typed @ (Ok(()) | Err(RunLineRefusal::Unanswered)) => {
                    let now = world.awake_ms().unwrap_or(0);
                    ladder.relaunched(now);
                    keep_ladder(&live, &ladder, now);
                    let mut meta = live.meta.lock().unwrap_or_else(|p| p.into_inner());
                    if meta
                        .relaunch
                        .model
                        .as_ref()
                        .is_some_and(|(claimant, _)| *claimant == scoped.proven)
                    {
                        meta.relaunch.model = None;
                    }
                    drop(meta);
                    return typed.map(|()| words).map_err(Left::Untyped);
                }
                Err(refusal) => Left::Untyped(refusal),
            }
        };
        ladder.missed();
        keep_ladder(&live, &ladder, world.awake_ms().unwrap_or(0));
        pause = ladder.pause();
        aterm_log::info!(
            "harness watchdog @{sid}: the relaunch typed nothing ({missed:?}); it tries again \
             in {}s",
            pause.as_secs()
        );
    }
}

/// Whether `live`'s tab still stands handed over by the ending that wrote `handover`: no claim
/// stands on it ([`Left::Claimed`]) and its `unsupervised` is still that very handover
/// ([`Left::Superseded`]: a claim placed and gone again, or another ending's handover).
fn handed_over(live: &SessionCtx, handover: &Unsupervised) -> Result<(), Left> {
    let meta = live.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.live_claim(crate::metrics::now_us()).is_some() {
        return Err(Left::Claimed);
    }
    if meta.unsupervised.as_ref() != Some(handover) {
        return Err(Left::Superseded);
    }
    Ok(())
}

/// Keep the relaunch back-off `ladder` on `live`'s session at `now` on the awake clock.
fn keep_ladder(live: &SessionCtx, ladder: &Ladder, now: u64) {
    let mut meta = live.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.relaunch.streak = ladder.streak();
    meta.relaunch.last_ms = ladder
        .since(now)
        .map(|since| now.saturating_sub(u64::try_from(since.as_millis()).unwrap_or(u64::MAX)));
}

/// THE KEPT CHILD'S BUILT-IN RELAUNCH SNAPSHOT (the HX-15 plan's D22), as a claim on `ctx`'s tab
/// keeps `kept`: where the session holds no snapshot of that very child, its lane's snapshot is
/// read on a thread of its own ([`take_snapshot`] over the live read, [`live_snapshot`]). Called
/// for each placement that keeps a child, so it is taken when a child is first kept and again at
/// each change of child; a renewal of the same child reads nothing. A test build reads no live
/// snapshot (the live read dials this window's own socket): a session whose tests read its
/// children ([`tap::reads`]) is read through their stand-in and a kernel stand-in that reads
/// the child at its birth, and any other reads nothing.
pub(crate) fn child_kept(ctx: &SessionCtx, kept: KeptChild) {
    #[cfg(test)]
    let Some(read) = tap::reader(&ctx.meta) else {
        return;
    };
    #[cfg(not(test))]
    let read: Arc<dyn Fn(&str, u32) -> Option<Resume> + Send + Sync> = Arc::new(live_snapshot);
    if snapshot_of(&ctx.meta, kept) {
        return;
    }
    let meta = Arc::clone(&ctx.meta);
    let sid = ctx.self_id.as_str().to_string();
    let spawned = std::thread::Builder::new()
        .name(format!("aterm-harness-snapshot-{}", kept.pid))
        .spawn(move || {
            #[cfg(test)]
            let mut control = tests::Kernel::reading(kept.pid, kept.birth.0);
            #[cfg(not(test))]
            let mut control = EndControlLive;
            let _ = take_snapshot(&meta, &sid, kept, &*read, &mut control);
        });
    if let Err(e) = spawned {
        aterm_log::warn!(
            "harness watchdog @{}: the snapshot read could not start: {e}",
            ctx.self_id.as_str()
        );
    }
}

/// Whether the session holds a snapshot of the very child `kept` (its pid at its birth).
fn snapshot_of(meta: &crate::session_timeline::MetaLock, kept: KeptChild) -> bool {
    meta.lock()
        .unwrap_or_else(|p| p.into_inner())
        .relaunch
        .snapshot
        .as_ref()
        .is_some_and(|(child, _)| *child == kept)
}

/// TAKE THE KEPT CHILD'S SNAPSHOT: `read` what resumes the conversation of process `kept.pid` in
/// tab `sid`, and keep it on the session ([`crate::session_timeline::Relaunch::snapshot`]) only
/// where the child read at its birth before and after the read (through `control`) and a live
/// claim on the tab still keeps that very child. A child the session holds a snapshot of already
/// is not read again. Whether a snapshot was kept.
pub(crate) fn take_snapshot<C: EndControl>(
    meta: &crate::session_timeline::MetaLock,
    sid: &str,
    kept: KeptChild,
    read: &dyn Fn(&str, u32) -> Option<Resume>,
    control: &mut C,
) -> bool {
    if snapshot_of(meta, kept) || kept.identity(control).is_none() {
        return false;
    }
    let Some(resume) = read(sid, kept.pid) else {
        return false;
    };
    if kept.identity(control).is_none() {
        return false;
    }
    let mut meta = meta.lock().unwrap_or_else(|p| p.into_inner());
    let keeps = meta
        .live_scoped(crate::metrics::now_us())
        .is_some_and(|scoped| scoped.child == Some(kept));
    if keeps {
        meta.relaunch.snapshot = Some((kept, resume));
    }
    keeps
}

/// READ THE KEPT CHILD'S SNAPSHOT AGAIN as the watchdog ends the harness `scoped` on `live`'s tab
/// (before its group is signalled, [`end_harness`] and the scoped watch's `SIGTERM`,
/// [`snapshot_before_end`]): where its kept child still reads at its birth (through `control`)
/// before and after the read, what resumes the conversation it runs now replaces the snapshot
/// taken as the claim kept it, which may have proved none yet or a conversation the person left
/// since. A read that proves none leaves the earlier snapshot standing. Whether one was kept.
pub(crate) fn refresh_snapshot<C: EndControl>(
    live: &SessionCtx,
    scoped: &Scoped,
    world: &dyn World,
    control: &mut C,
) -> bool {
    let Some(kept) = scoped.child else {
        return false;
    };
    if kept.identity(control).is_none() {
        return false;
    }
    let Some(resume) = world.snapshot(live.self_id.as_str(), kept.pid) else {
        return false;
    };
    if kept.identity(control).is_none() {
        return false;
    }
    live.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .relaunch
        .snapshot = Some((kept, resume));
    true
}

/// [`refresh_snapshot`] in the live world, for the scoped watch of the contract 4 claim `scoped`
/// on `live`'s tab, just before it signals the claimant's group, which usually ends the kept
/// child with it.
pub(crate) fn snapshot_before_end(live: &SessionCtx, scoped: &Scoped) {
    if scoped.claim.contract == aterm_agent::harness::contract::UNIVERSAL {
        refresh_snapshot(live, scoped, &Live::POLICY, &mut EndControlLive);
    }
}

/// Whether a claim standing on `live`'s tab keeps the child `scoped` kept, other than `scoped`'s
/// own claim (which the ending releases): another harness's claim placed while the ending
/// waited, whose child it is now.
fn kept_by_another(live: &SessionCtx, scoped: &Scoped) -> bool {
    let Some(kept) = scoped.child else {
        return false;
    };
    live.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .live_scoped(crate::metrics::now_us())
        .is_some_and(|standing| !standing.same_authority(scoped) && standing.child == Some(kept))
}

/// The claimants an ending runs for now, each with its session's allocation ([`Ends`]): the
/// ending's own [`Weak`] keeps that allocation, so no other session takes its address meanwhile.
static ENDS: std::sync::Mutex<Vec<(usize, Proven)>> = std::sync::Mutex::new(Vec::new());

/// One running ending of a claimant on one session, listed in [`ENDS`] until it drops.
struct Ends((usize, Proven));

impl Ends {
    /// List an ending of `proven` on `ctx`'s session, `None` where one runs already.
    fn of(ctx: &Weak<SessionCtx>, proven: &Proven) -> Option<Self> {
        let key = (ctx.as_ptr().addr(), proven.clone());
        let mut ends = ENDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ends.contains(&key) {
            return None;
        }
        ends.push(key.clone());
        Some(Self(key))
    }
}

impl Drop for Ends {
    fn drop(&mut self) {
        ENDS.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|listed| *listed != self.0);
    }
}

impl Why {
    const fn words(self) -> &'static str {
        match self {
            Self::Unobeyed => "it ignored a restart",
            Self::Grace => "its re-exec grace ran out",
            Self::Exited => "it exited",
        }
    }
}

/// AFTER AN UNOBEYED RESTART (the beat restart, the `restart` verb, the stall restart), once
/// the scoped watch's `SIGTERM` and `SIGKILL` of the claimant's group went out: where the
/// claimant `scoped` held by `holder` is proven gone by its pid and kernel start, the watchdog
/// ends the harness ([`end_harness`], [`Why::Unobeyed`]), its acts waiting out the `quiet` the
/// watch's signals waited under; a contract 4 claim's only, since only those keep a child and
/// are relaunched. A claimant that outlives its signals is left to its claim's own end.
pub(crate) fn after_unobeyed(
    ctx: &Weak<SessionCtx>,
    holder: &str,
    scoped: &Scoped,
    quiet: Duration,
) {
    if scoped.claim.contract != aterm_agent::harness::contract::UNIVERSAL {
        return;
    }
    let proven = &scoped.proven;
    let world = Live { quiet: Some(quiet) };
    let gone =
        aterm_agent::harness::upgrade_wake::wait_exit_as(proven.pid, &proven.start, GONE_WITHIN)
            || !world.alive(proven);
    if !gone {
        aterm_log::info!(
            "harness watchdog: {holder} outlived its group's signals; nothing more is ended"
        );
        return;
    }
    let _ = end_harness(
        ctx,
        holder,
        scoped,
        Why::Unobeyed,
        &world,
        &mut aterm_uds::process::EndControlLive,
    );
}

/// PAST THE RE-EXEC GRACE of the contract 4 claim `scoped` held by `holder` on `ctx`'s tab,
/// which the window's lapse timer just released (D28): the watchdog ends the harness on a thread
/// of its own ([`end_harness`], [`Why::Grace`]).
pub(crate) fn past_grace(ctx: &Arc<SessionCtx>, holder: &str, scoped: Scoped) {
    spawn_ending(ctx, holder, scoped, Why::Grace);
}

/// THE CLAIM OF A HARNESS THAT EXITED, which the status sweep released as its foreground group
/// moved, grace or not ([`crate::session_timeline::publish_release_proven_if_gone`]): where it
/// was a contract 4 claim and its claimant is gone by its pid and kernel start, the watchdog
/// ends the child it kept and hands the tab to the relaunch on a thread of its own
/// ([`end_harness`], [`Why::Exited`]). A claimant that only left the front is let be.
pub(crate) fn released_gone(ctx: &Arc<SessionCtx>, holder: &str, scoped: Scoped) {
    if scoped.claim.contract != aterm_agent::harness::contract::UNIVERSAL
        || Live::POLICY.alive(&scoped.proven)
    {
        return;
    }
    spawn_ending(ctx, holder, scoped, Why::Exited);
}

/// [`end_harness`] in the live world, on a thread of its own.
fn spawn_ending(ctx: &Arc<SessionCtx>, holder: &str, scoped: Scoped, why: Why) {
    let weak = Arc::downgrade(ctx);
    let owned = holder.to_string();
    let spawned = std::thread::Builder::new()
        .name(format!("aterm-harness-watchdog-{}", scoped.proven.pid))
        .spawn(move || {
            let _ = end_harness(
                &weak,
                &owned,
                &scoped,
                why,
                &Live::POLICY,
                &mut aterm_uds::process::EndControlLive,
            );
        });
    if let Err(e) = spawned {
        aterm_log::warn!("harness watchdog: the ending of {holder} could not start: {e}");
    }
}

#[cfg(test)]
#[path = "harness_watchdog_tests.rs"]
pub(crate) mod tests;

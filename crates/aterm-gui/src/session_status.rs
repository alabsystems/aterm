// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The PURE status classifier for a terminal session (RFC: Tab Subject &
//! Status, build-order step 3).
//!
//! This module holds no terminal handle, takes no lock, reads no clock, and
//! draws nothing. It is a function from *already-extracted evidence* plus the
//! previous state to a [`Status`], with an injected monotonic instant. That is
//! deliberate: the classification rules are the part worth testing exhaustively,
//! and they must be testable with no GUI, no PTY, and no timing flake.
//!
//! Two rules from the RFC are load-bearing here and are asserted by tests:
//!
//! * **Strong shell evidence outranks raw screen movement.** A background job
//!   printing while the shell sits at a prompt must NOT make the session
//!   `Running` — otherwise every `tail -f &` would lie about the pane.
//! * **There is no "waiting for input" phase, because there is no honest local
//!   source for one.** RFC §6 permits the claim from exactly three places: a
//!   user pin, a trusted child protocol, or shell-integration prompt-input
//!   readiness. Aterm has no phase pin, no child protocol carries such a
//!   signal, and the one shell mark that means "the prompt is ready for a
//!   command" (`BlockState::EnteringCommand`) is already spent on
//!   [`Phase::Idle`] — promoting it would raise an attention badge on every
//!   idle shell in every tab. A quiet foreground job may be CPU-bound,
//!   sleeping, blocked on the network, in a pager, or at a REPL, so it is
//!   [`Phase::Quiet`] with [`Confidence::Heuristic`], which is honest;
//!   claiming a human is being awaited is not. The phase was specified before
//!   a source existed and has been REMOVED rather than left as scaffolding —
//!   see the `phase=` vocabulary in `docs/INTROSPECTION.md`.
//!
//! The activity primitive is the PAIR `(is_alternate_screen, content_seq)`, never
//! the sequence alone: the main and alternate grids keep independent counters, so
//! an alt-screen transition restarts the sequence and must be treated as a
//! RESYNC rather than as activity.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;

#[path = "session_program.rs"]
pub(crate) mod program;

/// At most this often is one session's screen re-read for its agent verdict,
/// whatever `tab_status`'s observation interval: 4 Hz per session.
pub(crate) const AGENT_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// The whole-screen fingerprint and only the final `tail_rows` rows, read in
/// one pass under the status sweep's terminal `try_lock`. Hash every visible
/// row plus its newline, exactly as [`crate::control::screen_text`] does, but
/// retain no full-screen String and no rows outside the live zone. A parity
/// test spans Unicode, resize and alternate-screen transitions.
pub(crate) fn screen_agent_frame(t: &Terminal, tail_rows: usize) -> (u64, Vec<String>) {
    let first = (t.rows() as usize).saturating_sub(tail_rows);
    let mut rows = Vec::with_capacity((t.rows() as usize).min(tail_rows));
    let mut hash = crate::turn_ledger::Fnv1a64::new();
    for r in 0..t.rows() as usize {
        let row = crate::control::visible_row(t, r);
        hash.update(row.as_bytes());
        hash.update(b"\n");
        if r >= first {
            rows.push(row);
        }
    }
    (hash.finish(), rows)
}

/// How often a foreground group whose screen keeps moving is re-named. A
/// group's program is resolved when the group CHANGES; this catches the one
/// change that keeps the group — an `exec` in place (`cd ~/ay && exec
/// claude` replaces the shell without a new process group).
pub(crate) const PROGRAM_RECHECK: Duration = Duration::from_secs(5);
/// One short confirmation after a newly named non-agent foreground program:
/// an interpreter can still be in the same-PGID exec window before Claude.
const PROGRAM_NAME_CONFIRM: Duration = Duration::from_millis(500);
/// After the first confirmation, a later screen move owns a single delayed
/// recheck. Four seconds from the last request keeps the total first-group
/// bound below five seconds without polling a stable named screen.
const PROGRAM_NAMED_RECHECK_FLOOR: Duration = Duration::from_secs(4);

/// A failed first process-table read must not leave a still Claude screen
/// unnamed forever. Retry promptly once, then back off to the ordinary
/// five-second cadence when process inspection stays unavailable.
fn unresolved_program_recheck(attempts: u8) -> Duration {
    match attempts {
        0 | 1 => Duration::from_millis(250),
        2 => Duration::from_secs(1),
        _ => PROGRAM_RECHECK,
    }
}

/// FNV-1a 64 over the classified rows, `\n`-joined, and the cursor's row on
/// the screen: the live-zone hash the agent verdict is re-derived on (a
/// changed content seq over an unchanged zone — a ticking clock elsewhere —
/// costs a hash, not a classification). The cursor is in it because Claude
/// Code's `idle` is the prompt box that HOLDS the cursor
/// ([`crate::presence::Cursor`]): the cursor stepping into the box drawn a
/// frame earlier changes the verdict and nothing else.
fn zone_hash(rows: &[String], cursor: crate::presence::Cursor) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let cursor = format!("{cursor:?}");
    for row in rows
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(cursor.as_str()))
    {
        for b in row.bytes().chain(std::iter::once(b'\n')) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// One session's agent-verdict watch: what the sweep last looked at, and the
/// reading it published.
#[derive(Debug, Default)]
struct AgentWatch {
    /// The screen generation the live zone was last read at. A generation, not
    /// the bare content seq: a screen switch can land the new grid on the old
    /// grid's seq, and that screen must still be read.
    seq_seen: Option<crate::control::ScreenGen>,
    /// The terminal's cursor the live zone was last read with: its moving
    /// alone is a reason to read again (the content seq does not move with
    /// it, and Claude Code's `idle` is the prompt box that holds it).
    cursor_seen: Option<crate::presence::Cursor>,
    /// [`zone_hash`] of the rows last classified.
    zone_hash: Option<u64>,
    /// When the classifier last ran — the [`AGENT_MIN_INTERVAL`] floor.
    classified_at: Option<Instant>,
    /// The program the last classification ran under; a different one forces
    /// a re-read even over an unchanged zone.
    program_seen: Option<Option<String>>,
    /// Look once more at the next due observation: the content moved at this
    /// one (or the rate floor deferred a read), so its FINAL frame — a box
    /// drawn just after a look, with nothing printed after it — is read
    /// within one interval rather than at the status FSM's next deadline.
    followup: bool,
    /// The latest output wake for this session ([`StatusObserver::note_output`]).
    /// A follow-up is owed only once the output has been QUIET for one
    /// observation interval: while it keeps arriving, each output wake's own
    /// sweep looks whenever the session is due, so a timer at the observation
    /// floor would only duplicate those looks — a 4 Hz poll for as long as a
    /// program repaints without moving the grid (the spin gate's claude row:
    /// 39 timer wakes in 10 s where a quiet loop takes 5).
    output_at: Option<Instant>,
    /// The foreground group last seen (`-1` when unknowable).
    pgid: i32,
    /// When a program resolution was last requested for this session.
    resolved_at: Option<Instant>,
    /// Requests in this foreground group, capped at three. The cap makes the
    /// unresolved retry cadence 250 ms, then 1 s, then 5 s indefinitely.
    resolve_attempts: u8,
    /// Deadline owned by a newly seen non-agent name or a later screen move.
    /// A transient `sh` can become Claude without another screen change.
    deferred_name_recheck_at: Option<Instant>,
    /// The initial short confirmation ran; later screen moves use the slower
    /// named-program floor instead of rearming a 500 ms probe per frame.
    name_confirmed: bool,
    /// The foreground group in which the agent's own screen identified the
    /// session, and the agent it identified (Claude Code by its frame, Codex
    /// by its composer): it stays that agent until the group leaves the
    /// foreground (a box hides the composer, and must not un-identify it).
    frame: Option<(i32, aterm_phase::Program)>,
    /// The published reading (`None` = not an identified agent).
    reading: Option<crate::presence::AgentReading>,
    /// [`StatusObserver::agent_seq`] when `reading` last changed.
    reading_seq: u64,
}

/// A verdict to publish: its `word`, raw `detail`, a prompt's host-side
/// subject, and the program its reader read (by name or by screen).
pub(crate) type AgentPublish = (
    &'static str,
    Option<String>,
    Option<String>,
    Option<aterm_phase::Program>,
);

/// What one observation's agent step decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentStep {
    /// The published reading changed (presence must re-fold it).
    pub(crate) reading_changed: bool,
    /// The verdict to publish on the session's timeline (`word`, raw
    /// `detail`, a prompt's host-side subject, and the agent its reader
    /// read — by name or by screen), when the zone was classified this
    /// observation.
    pub(crate) publish: Option<AgentPublish>,
}

/// Current activity of a session. This is deliberately NOT where success or
/// failure lives — see [`Outcome`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No usable evidence, or evidence conflicts. A NORMAL condition.
    #[default]
    Unknown,
    /// Session created; nothing observed yet.
    Starting,
    /// Shell at a prompt with no foreground job.
    Idle,
    /// A foreground job exists and is producing output (or just started).
    Running,
    /// A foreground job exists but no display activity has been observed
    /// recently. NOT a claim that anyone is being awaited.
    Quiet,
    /// The PTY has ended. Pairs with [`Status::last_outcome`].
    Exited,
}

impl Phase {
    /// The normative wire spelling (RFC §3), shared by the `status` verb and
    /// the timeline. Stable: a consumer may match on these.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Starting => "starting",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Quiet => "quiet",
            Self::Exited => "exited",
        }
    }
}

/// Result of the last COMPLETED unit of work. Survives phase changes until the
/// next unit starts, which is what lets a finished-and-failed command stay
/// visible while the session is honestly `Idle`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Outcome {
    #[default]
    None,
    Success,
    Failure {
        exit_code: i32,
    },
    Signal {
        signal: u8,
    },
}

impl Outcome {
    pub(crate) const fn is_failure(self) -> bool {
        matches!(self, Self::Failure { .. } | Self::Signal { .. })
    }

    /// The normative wire spelling (RFC §3). The payload (`exit_code`,
    /// `signal`) rides its own fields so this stays a bare token.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Success => "success",
            Self::Failure { .. } => "failure",
            Self::Signal { .. } => "signal",
        }
    }
}

/// Ordinal, NOT a probability. This is the contract a later interpretation tier
/// escalates against (RFC §3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Confidence {
    #[default]
    Unknown,
    Heuristic,
    Strong,
    Exact,
}

impl Confidence {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Heuristic => "heuristic",
            Self::Strong => "strong",
            Self::Exact => "exact",
        }
    }
}

/// Why the classifier reached its conclusion. Carried on the record so a
/// consumer can tell an observed fact from an inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Pin,
    ShellBlock,
    LifecycleExit,
    ForegroundJob,
    ContentActivity,
    OutputActivity,
    Stall,
    NoEvidence,
    /// A macOS consent wall is POSSIBLE for this session: its `fs_consent` is
    /// not `covered` and its shell-integration cwd is under a protected root
    /// (design §3.6). A conjunction of two observed facts — NOT a claim that a
    /// dialog is showing, which aterm cannot see. Never emitted when the cwd is
    /// unknown, i.e. whenever shell integration is absent.
    ConsentPrompt,
}

impl Reason {
    /// The normative wire spellings (RFC §3).
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pin => "pin",
            Self::ShellBlock => "shell_block",
            Self::LifecycleExit => "lifecycle_exit",
            Self::ForegroundJob => "fg_job",
            Self::ContentActivity => "content_activity",
            Self::OutputActivity => "output_activity",
            Self::Stall => "stall",
            Self::NoEvidence => "no_evidence",
            Self::ConsentPrompt => "consent_at_risk",
        }
    }
}

/// Shell-integration evidence: the strongest routinely-available signal, and the
/// only local source that can distinguish "at a prompt" from "quiet job".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellEvidence {
    Prompt,
    Entering,
    Executing,
    Complete { exit_code: Option<i32> },
}

/// How the session's PTY ended, when that is actually known.
///
/// Produced at the `Wake::Exit` edge by [`crate::App::note_session_exit`], which
/// collects the child's status with a non-blocking `waitpid` while it is still
/// an unreaped zombie. `Exited { exit_code: None }` is the honest answer for the
/// three cases in which no status exists — an adopted session, a master that
/// went unreadable without the child exiting, and a child something else already
/// reaped — and must never be read as success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Exited { exit_code: Option<i32> },
    Signalled { signal: u8 },
}

/// One sample of display activity. `content_seq` is meaningful ONLY when paired
/// with `alt_screen` (see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActivitySample {
    pub(crate) alt_screen: bool,
    pub(crate) content_seq: u64,
    /// Age of the most recent PTY output, from the session's cheap activity
    /// atomic. `None` when never observed.
    pub(crate) last_output: Option<Instant>,
    /// Age of the most recent USER KEYSTROKE aimed at this session (the
    /// window's key stamp, carried only for the window's focused session).
    /// `None` when this session is nobody's typing target.
    ///
    /// Movement alone cannot tell typing from a background `tail -f` at a
    /// prompt — both advance `content_seq` — so the live-typing marker needs
    /// evidence that a human actually pressed something.
    pub(crate) last_input: Option<Instant>,
}

/// Everything the classifier is allowed to look at. Assembling this is the
/// caller's job (and the only part that touches a lock).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Evidence {
    /// A phase pinned explicitly by the user. Outranks everything. No surface
    /// writes one yet (session metadata carries title/description/icon/role/
    /// attention — none of them a phase pin), so the branch is live but
    /// unfed — see RFC §9's remaining work.
    pub(crate) pin: Option<Phase>,
    pub(crate) shell: Option<ShellEvidence>,
    pub(crate) lifecycle: Option<Lifecycle>,
    /// `tcgetpgrp`-derived: is a foreground job distinct from the shell running?
    /// The process NAME is not available and is deliberately not modelled.
    pub(crate) foreground_job: Option<bool>,
    pub(crate) activity: ActivitySample,
}

/// Tunables. Bounds are enforced by the config layer, not here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StatusPolicy {
    /// A foreground job with no display activity for this long becomes `Quiet`.
    pub(crate) quiet_after: Duration,
    /// A candidate phase must persist this long before it is published, so
    /// spinners and intermittent output cannot flap the badge or flood the
    /// bounded timeline. Exact-confidence transitions bypass it.
    pub(crate) dwell: Duration,
}

impl Default for StatusPolicy {
    fn default() -> Self {
        Self {
            quiet_after: Duration::from_millis(5_000),
            dwell: Duration::from_millis(750),
        }
    }
}

/// The published status of one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) phase: Phase,
    pub(crate) last_outcome: Outcome,
    pub(crate) since: Instant,
    pub(crate) detail: Option<String>,
    pub(crate) confidence: Confidence,
    pub(crate) reasons: Vec<Reason>,
    /// Contradictory evidence was present. NOT set for the ordinary
    /// background-job-at-a-prompt case, which is expected rather than
    /// contradictory.
    pub(crate) conflict: bool,
}

impl Status {
    /// Whether this record is the LIVE typing state: `Idle` published from
    /// prompt-entry evidence (`ShellEvidence::Entering`) with the keystroke
    /// echo still recent, marked by the movement reason riding the record.
    /// The phase stays `Idle` on the wire — a prompt with no foreground job
    /// IS idle, and RFC §6 permits no stronger claim — but chrome may keep a
    /// "Typing …" subject up while this is true. No other `Idle` candidate
    /// carries an activity reason, so the pair is unambiguous.
    fn typing_live(&self) -> bool {
        self.phase == Phase::Idle && self.reasons.contains(&Reason::ContentActivity)
    }

    /// The verdict the smart-title coordinator's Entering→prompt decay keys
    /// on (frame audit #3): `Idle` that has actually SETTLED — at a prompt
    /// with the keystroke echo aged out past `quiet_after`. A bare `phase ==
    /// Idle` check killed the live state: `Entering` classifies as `Idle`
    /// unconditionally, so the decay engaged WHILE the user was typing and
    /// the typing subject never showed at all (review finding on the audit's
    /// fix).
    pub(crate) fn settled_idle(&self) -> bool {
        self.phase == Phase::Idle && !self.typing_live()
    }

    fn seed(now: Instant) -> Self {
        Self {
            phase: Phase::Starting,
            last_outcome: Outcome::None,
            since: now,
            detail: None,
            confidence: Confidence::Unknown,
            reasons: vec![Reason::NoEvidence],
            conflict: false,
        }
    }
}

/// One classification before dwell is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    phase: Phase,
    confidence: Confidence,
    reasons: Vec<Reason>,
    conflict: bool,
    outcome: Option<Outcome>,
}

/// Per-session classifier state. Holds only what hysteresis and activity
/// detection genuinely require.
#[derive(Clone, Debug)]
pub(crate) struct StatusFsm {
    policy: StatusPolicy,
    published: Status,
    /// Prior activity identity, for movement detection.
    prior: Option<(bool, u64)>,
    /// When display content last actually moved.
    last_movement: Option<Instant>,
    /// The freshest PTY-output stamp any observation carried
    /// ([`ActivitySample::last_output`]), folded MAX so a sample without one
    /// (the exit path's blank sample) never rolls the clock back. Tracked
    /// because [`StatusFsm::classify`] sustains `Running` on EITHER clock —
    /// grid movement OR raw output — so the wake that retires the phase must
    /// be armed off the same pair (see [`StatusFsm::owed_wake`]).
    last_output: Option<Instant>,
    /// A candidate that has not yet satisfied dwell.
    pending: Option<(Candidate, Instant)>,
}

impl StatusFsm {
    pub(crate) fn new(policy: StatusPolicy, now: Instant) -> Self {
        Self {
            policy,
            published: Status::seed(now),
            prior: None,
            last_movement: None,
            last_output: None,
            pending: None,
        }
    }

    pub(crate) fn status(&self) -> &Status {
        &self.published
    }

    /// Fold the running-command `detail` onto the published status — the name
    /// the tab chrome renders as `running <detail>` ([`summary_text`]). Kept
    /// SEPARATE from [`Self::classify`]: the classifier's job is the phase, and
    /// the command line is a fact the sweep reads from the same engine lock and
    /// hands straight through. Without this the field was seeded `None` and only
    /// ever carried across a phase flip (`apply`'s `.take()`), so every tab read
    /// plain `running` while the wire's `status detail=` — its own producer —
    /// named the program. Returns whether the value moved, the caller's signal
    /// to refold the tab.
    fn set_detail(&mut self, detail: Option<String>) -> bool {
        if self.published.detail == detail {
            return false;
        }
        self.published.detail = detail;
        true
    }

    /// The instant at which this session could publish a transition that NO new
    /// output would cause. Three exist: a candidate still serving its dwell,
    /// `Running` aging into `Quiet`, and the LIVE typing `Idle` settling into
    /// plain `Idle`. Everything else is edge-driven — a settled `Idle` pane
    /// cannot become anything until bytes arrive — so it returns `None` and
    /// costs the event loop nothing.
    ///
    /// THE CLOCK MUST TELL THE TRUTH (event-loop busy-rearm audit, item 1):
    /// each arm is the earliest instant at which [`StatusFsm::classify`] could
    /// actually CHANGE its verdict with no new evidence. `Running` is
    /// sustained by `moved_recently || output_recently`, so its retirement is
    /// keyed off the FRESHER of the two clocks. Arming off `last_movement`
    /// alone was a ~200 kHz spin: a client that repaints without moving the
    /// grid (sync-bracketed identical frames, heartbeats — a Claude Code TUI
    /// does this indefinitely) lets `last_movement` go stale past
    /// `quiet_after` while output keeps `Running` alive, so every armed
    /// instant was already in the past, the wake fired immediately, the
    /// observation gate refused or observed no change, and the loop re-armed.
    fn owed_wake(&self) -> Option<Instant> {
        if let Some((_, first_seen)) = &self.pending {
            return Some(*first_seen + self.policy.dwell);
        }
        if self.published.phase == Phase::Running {
            // A PINNED verdict is immovable, so it owes no wake. `classify`
            // answers a user pin with `Confidence::Exact` BEFORE either
            // activity clock is consulted, so the arithmetic below would return
            // `max(movement, output) + quiet_after` — an instant that recedes
            // further into the past on every turn while nothing can ever change
            // the answer. That is a permanent wake plus a pool sweep, a
            // `try_lock` and a `tcgetpgrp` per session, forever, for a status
            // that is by definition immovable. The clamp would bound it to the
            // observation rate; arming only where the verdict can actually
            // change is the law this whole accessor exists to obey.
            //
            // Removing the pin is an EDGE (a control command), which drives its
            // own observation — nothing here is needed to notice it.
            if self.published.reasons.contains(&Reason::Pin) {
                return None;
            }
            return match (self.last_movement, self.last_output) {
                (Some(movement), Some(output)) => {
                    Some(movement.max(output) + self.policy.quiet_after)
                }
                (Some(at), None) | (None, Some(at)) => Some(at + self.policy.quiet_after),
                (None, None) => None,
            };
        }
        // LIVE typing settles on the movement clock alone: `typed_recently`
        // requires movement, and ambient output cannot extend a keystroke echo.
        self.published
            .typing_live()
            .then(|| self.last_movement.map(|at| at + self.policy.quiet_after))
            .flatten()
    }

    /// Fold one observation in. Returns `true` when the PUBLISHED status changed
    /// — the caller's signal to bump a revision, repaint chrome, and append one
    /// timeline event.
    pub(crate) fn observe(&mut self, evidence: &Evidence, now: Instant) -> bool {
        self.note_activity(&evidence.activity, now);
        let candidate = self.classify(evidence, now);
        self.apply(candidate, now)
    }

    /// Track movement, treating an alt-screen transition as a RESYNC: the two
    /// grids have independent counters, so the sequence jump across a transition
    /// carries no information about whether anything happened.
    fn note_activity(&mut self, sample: &ActivitySample, now: Instant) {
        let identity = (sample.alt_screen, sample.content_seq);
        match self.prior {
            // MOVEMENT is only inferable within ONE grid: same screen, and the
            // content counter actually advanced. Both conditions belong in the
            // arm's guard — split across a nested `if`, the "same screen but the
            // counter stood still" case reads like a distinct outcome when it is
            // the same no-op as the fallthrough below.
            Some((prior_alt, prior_seq))
                if prior_alt == sample.alt_screen && sample.content_seq != prior_seq =>
            {
                self.last_movement = Some(now);
            }
            // First sample, or an alt-screen transition: adopt the new identity
            // without inferring movement.
            _ => {}
        }
        self.prior = Some(identity);
        // Fold the raw-output clock alongside the movement clock: `owed_wake`
        // arms `Running`'s retirement off whichever is fresher, because
        // `classify` keeps the phase alive on either. MAX, never overwrite —
        // a sample with no stamp must not roll the clock back.
        if let Some(at) = sample.last_output {
            self.last_output = Some(self.last_output.map_or(at, |prior| prior.max(at)));
        }
    }

    fn moved_recently(&self, now: Instant) -> bool {
        self.last_movement
            .is_some_and(|at| now.saturating_duration_since(at) < self.policy.quiet_after)
    }

    /// LIVE TYPING: a keystroke aimed at this session, still inside the one
    /// activity window, AND the grid moved for it. Movement alone was the
    /// review's regression — `EnteringCommand` is the steady state whenever a
    /// prompt is displayed, so any background output re-stuck the typing
    /// subject on an idle prompt.
    fn typed_recently(&self, sample: &ActivitySample, now: Instant) -> bool {
        sample
            .last_input
            .is_some_and(|at| now.saturating_duration_since(at) < self.policy.quiet_after)
            && self.moved_recently(now)
    }

    fn output_recently(&self, sample: &ActivitySample, now: Instant) -> bool {
        sample
            .last_output
            .is_some_and(|at| now.saturating_duration_since(at) < self.policy.quiet_after)
    }

    /// Which signal actually justified "something is happening" — grid
    /// mutation, or merely PTY bytes arriving. Recorded so a reader can tell a
    /// repaint from real output.
    fn movement_reason(&self, evidence: &Evidence, now: Instant) -> Reason {
        if self.moved_recently(now) {
            Reason::ContentActivity
        } else if self.output_recently(&evidence.activity, now) {
            Reason::OutputActivity
        } else {
            Reason::Stall
        }
    }

    fn classify(&self, evidence: &Evidence, now: Instant) -> Candidate {
        // 1. Lifecycle exit is terminal and exact.
        if let Some(lifecycle) = evidence.lifecycle {
            let outcome = match lifecycle {
                Lifecycle::Exited { exit_code: None } => Outcome::None,
                Lifecycle::Exited {
                    exit_code: Some(0), ..
                } => Outcome::Success,
                Lifecycle::Exited {
                    exit_code: Some(code),
                } => Outcome::Failure { exit_code: code },
                Lifecycle::Signalled { signal } => Outcome::Signal { signal },
            };
            return Candidate {
                phase: Phase::Exited,
                confidence: Confidence::Exact,
                reasons: vec![Reason::LifecycleExit],
                conflict: false,
                outcome: Some(outcome),
            };
        }

        // 2. An explicit user pin outranks every inferred signal.
        if let Some(phase) = evidence.pin {
            return Candidate {
                phase,
                confidence: Confidence::Exact,
                reasons: vec![Reason::Pin],
                conflict: false,
                outcome: None,
            };
        }

        let moved = self.moved_recently(now) || self.output_recently(&evidence.activity, now);

        // 3. Shell integration: the strongest routine evidence. It OUTRANKS raw
        //    screen movement, so a background job printing at a prompt stays
        //    Idle rather than masquerading as the foreground task.
        if let Some(shell) = evidence.shell {
            // A genuine contradiction: the shell claims to be executing while
            // the foreground process group is the shell itself.
            let conflict =
                matches!(shell, ShellEvidence::Executing) && evidence.foreground_job == Some(false);
            return match shell {
                // The movement reason is the foreground-job arm's own: output
                // that mutates nothing reads `output_activity`, so the record
                // says which clock keeps `Running` alive.
                ShellEvidence::Executing => Candidate {
                    phase: if moved { Phase::Running } else { Phase::Quiet },
                    confidence: Confidence::Strong,
                    reasons: if moved {
                        vec![Reason::ShellBlock, self.movement_reason(evidence, now)]
                    } else {
                        vec![Reason::ShellBlock, Reason::Stall]
                    },
                    conflict,
                    outcome: None,
                },
                // LIVE TYPING (review follow-up to frame audit #3): prompt
                // entry with the keystroke echo still moving the grid is the
                // one `Idle` that is NOT settled. Same phase on the wire, but
                // the movement reason rides the record so the chrome's typing
                // subject can stay live WHILE the user is typing — the audit's
                // decay killed that state by classifying `Entering` as plain
                // `Idle` unconditionally. The echo aging out (`quiet_after`,
                // the module's one activity window) retires the marker, which
                // is the decay itself: an abandoned half-typed prompt settles
                // to plain `Idle` with no new bytes at all (see `owed_wake`).
                // `Prompt` never carries the marker — its movement is the
                // `tail -f &` case, not a keystroke.
                ShellEvidence::Entering if self.typed_recently(&evidence.activity, now) => {
                    Candidate {
                        phase: Phase::Idle,
                        confidence: Confidence::Strong,
                        reasons: vec![Reason::ShellBlock, Reason::ContentActivity],
                        conflict: false,
                        outcome: None,
                    }
                }
                ShellEvidence::Prompt | ShellEvidence::Entering => Candidate {
                    phase: Phase::Idle,
                    confidence: Confidence::Strong,
                    reasons: vec![Reason::ShellBlock],
                    conflict: false,
                    outcome: None,
                },
                ShellEvidence::Complete { exit_code } => Candidate {
                    phase: Phase::Idle,
                    confidence: Confidence::Strong,
                    reasons: vec![Reason::ShellBlock],
                    conflict: false,
                    outcome: Some(match exit_code {
                        None => Outcome::None,
                        Some(0) => Outcome::Success,
                        Some(code) => Outcome::Failure { exit_code: code },
                    }),
                },
            };
        }

        // 4. No shell integration. A foreground-job Boolean still separates
        //    "shell is waiting for me" from "something is running".
        match evidence.foreground_job {
            Some(true) if moved => Candidate {
                phase: Phase::Running,
                confidence: Confidence::Strong,
                reasons: vec![Reason::ForegroundJob, self.movement_reason(evidence, now)],
                conflict: false,
                outcome: None,
            },
            Some(true) => Candidate {
                phase: Phase::Quiet,
                confidence: Confidence::Heuristic,
                reasons: vec![Reason::ForegroundJob, Reason::Stall],
                conflict: false,
                outcome: None,
            },
            Some(false) => Candidate {
                phase: Phase::Idle,
                confidence: Confidence::Strong,
                reasons: vec![Reason::ForegroundJob],
                conflict: false,
                outcome: None,
            },
            // 5. Nothing but the screen. Movement is weak evidence of work;
            //    silence tells us nothing at all, so say so.
            None if moved => Candidate {
                phase: Phase::Running,
                confidence: Confidence::Heuristic,
                reasons: vec![self.movement_reason(evidence, now)],
                conflict: false,
                outcome: None,
            },
            None => Candidate {
                phase: Phase::Unknown,
                confidence: Confidence::Unknown,
                reasons: vec![Reason::NoEvidence],
                conflict: false,
                outcome: None,
            },
        }
    }

    /// Publish a candidate once it has held for the dwell interval. An
    /// exact-confidence candidate (pin, lifecycle exit) is published at once —
    /// a session that has exited must not be reported as running for another
    /// three-quarters of a second.
    fn apply(&mut self, candidate: Candidate, now: Instant) -> bool {
        let outcome = candidate.outcome.unwrap_or(self.published.last_outcome);
        // A new unit of work clears the remembered result of the previous one.
        // Keyed on LEAVING rest rather than on entering Running specifically:
        // an Idle -> Quiet -> Running path (a command that prints nothing for a
        // while, e.g. `sleep 30`) never passes through Idle -> Running, and
        // would otherwise keep marking the tab with the PREVIOUS command's
        // failure for its whole run.
        let resting = |phase| matches!(phase, Phase::Idle | Phase::Unknown | Phase::Starting);
        let starts_work = !resting(candidate.phase)
            && candidate.phase != Phase::Exited
            && resting(self.published.phase);
        let outcome = if starts_work { Outcome::None } else { outcome };

        let same_phase = candidate.phase == self.published.phase;
        let immediate = candidate.confidence == Confidence::Exact;

        if same_phase {
            self.pending = None;
            let changed = outcome != self.published.last_outcome
                || candidate.confidence != self.published.confidence
                || candidate.reasons != self.published.reasons
                || candidate.conflict != self.published.conflict;
            if changed {
                self.published.last_outcome = outcome;
                self.published.confidence = candidate.confidence;
                self.published.reasons = candidate.reasons;
                self.published.conflict = candidate.conflict;
            }
            return changed;
        }

        if !immediate {
            match &self.pending {
                Some((held, first_seen))
                    if held.phase == candidate.phase
                        && now.saturating_duration_since(*first_seen) >= self.policy.dwell => {}
                Some((held, _)) if held.phase == candidate.phase => return false,
                _ => {
                    self.pending = Some((candidate, now));
                    return false;
                }
            }
        }

        self.pending = None;
        self.published = Status {
            phase: candidate.phase,
            last_outcome: outcome,
            since: now,
            detail: self.published.detail.take(),
            confidence: candidate.confidence,
            reasons: candidate.reasons,
            conflict: candidate.conflict,
        };
        true
    }
}

/// Map shell-integration block state to evidence. The ONLY place block
/// semantics are interpreted, so the classifier stays free of terminal types.
pub(crate) fn shell_evidence(term: &aterm_core::terminal::Terminal) -> Option<ShellEvidence> {
    use aterm_types::BlockState;

    let block = term.current_block().or_else(|| term.all_blocks().last())?;
    Some(match block.state {
        BlockState::PromptOnly => ShellEvidence::Prompt,
        BlockState::EnteringCommand => ShellEvidence::Entering,
        BlockState::Executing => ShellEvidence::Executing,
        BlockState::Complete => ShellEvidence::Complete {
            exit_code: block.exit_code,
        },
        _ => return None,
    })
}

/// The RUNNING command of `term`'s current shell-integration block, reduced by
/// [`command_detail`]; `None` unless a block is `Executing` and names a command.
/// The one producer of `Status.detail` (RFC §4 rung 3): the `status` record and
/// the `sessions` roster both read it under a `try_lock`, so a busy engine
/// answers `-` rather than parking a poller behind the PTY reader.
pub(crate) fn executing_detail(term: &aterm_core::terminal::Terminal) -> Option<String> {
    let block = term.current_block()?;
    if block.state != aterm_types::BlockState::Executing {
        return None;
    }
    let explicit = block
        .commandline
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let text: String = match explicit {
        // OSC 633;E: the shell said the command line itself.
        Some(explicit) => return command_detail(explicit),
        // The screen scrape reads WHOLE rows, so the command's first row still
        // carries the prompt in front of it (`$ sleep 30`); cut at the column
        // the shell marked the command start (OSC 133;B) — by display column,
        // not char count, so a wide prompt glyph does not eat the program's
        // first letter.
        None => {
            let scraped = term.block_command(block)?;
            let skip = usize::from(block.command_start_col.unwrap_or(0));
            let from = aterm_grapheme::column_to_char_index(&scraped, skip);
            scraped.chars().skip(from).collect()
        }
    };
    command_detail(&text)
}

/// Wrappers unwrapped ONE level: the interesting program is the next word.
/// `exec` is one: it replaces the shell with the program it names, so
/// `exec claude` is `claude` running, not `exec`.
const WRAPPERS: [&str; 7] = ["sudo", "env", "time", "nice", "command", "nohup", "exec"];
/// Wrapper flags that consume the following word (`sudo -u me`, `nice -n 5`,
/// `env -u VAR`, `exec -a argv0`), so that word is never mistaken for the
/// program.
const WRAPPER_FLAGS_WITH_VALUE: [&str; 4] = ["-u", "-n", "-g", "-a"];
/// Shell keywords that open a compound command this deliberately does not
/// parse: the segment reads as the keyword, and its `;`s are the keyword's
/// grammar (`for …; do …; done`), not list operators to cut at.
const KEYWORD_OPENERS: [&str; 6] = ["for", "while", "if", "until", "case", "{"];
/// The punctuation the shell's quoting or a list cut leaves attached to a word
/// (`'claude'`, `(for`, `claude)`): trimmed before any word is read.
const WORD_TRIM: [char; 8] = ['\'', '"', '`', ';', '(', ')', '&', '|'];

/// `env`'s split-string flag — `-S`, a short cluster holding it after env's
/// value-less letters (`-vS`, `-iS`), `--split-string` — hands env ONE string
/// that env splits into the program and ITS arguments (`env -S 'claude
/// --resume /secret/tok'` runs `claude`). The program is that string's first
/// word, never the string's basename, which is whatever follows its last `/`:
/// an argument's (the skeptic's third review of the D3 fix read `tok` there).
/// `Some(Some(s))`: the string is attached to the flag (`-Snode …`,
/// `--split-string=…`); `Some(None)`: it is the next word; `None`: not the
/// flag.
fn env_split_string(flag: &str) -> Option<Option<&str>> {
    if let Some(rest) = flag.strip_prefix("--split-string") {
        return match rest.strip_prefix('=') {
            Some(attached) => Some(Some(attached)),
            None => rest.is_empty().then_some(None),
        };
    }
    let cluster = flag.strip_prefix('-').filter(|c| !c.starts_with('-'))?;
    let at = cluster.find('S')?;
    // Letters before `S` must take no value of their own (`-uS` unsets `S`).
    if !cluster[..at].chars().all(|c| matches!(c, 'i' | 'v' | '0')) {
        return None;
    }
    let attached = &cluster[at + 1..];
    Some((!attached.is_empty()).then_some(attached))
}

/// The program a word in a program slot names: its BASENAME, less the
/// executable extension a Windows launcher spells out.
///
/// The basename is what follows the last `/` (or `\`, save one that escapes a
/// space: `my\ tool`), and only up to its first whitespace: a word may hold a
/// quoted or escaped space, and a program's name is never published with one —
/// `"/opt/My App/My App"` is `My`, and no reading of a line that
/// [`shell_words`] got wrong can carry an argument out behind the name.
///
/// `less.exe`, `cmd.exe` and `build.cmd` are the programs `less`, `cmd` and
/// `build` — the same name the same tool answers on every other host, and the
/// name an agent compares against — so the four extensions Windows resolves
/// for a bare command (`PATHEXT`'s `.exe`/`.com`/`.bat`/`.cmd`) are dropped,
/// case-insensitively because the filesystem that resolved them is (measured
/// 2026-09-22: a Windows launcher's `"C:\Program Files\Git\usr\bin\less.exe"
/// README.md` read `Program` under a whitespace split). ONLY those: a dotted
/// program name such as `python3.11` is a name, not a path with an extension,
/// and a general `file_stem` would cut it to `python3`.
fn basename(word: &str) -> Option<&str> {
    const EXE_EXTENSIONS: [&str; 4] = [".exe", ".com", ".bat", ".cmd"];
    let mut from = 0;
    let mut chars = word.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let escapes_space = c == '\\' && chars.peek().is_some_and(|(_, n)| n.is_whitespace());
        if c == '/' || (c == '\\' && !escapes_space) {
            from = at + c.len_utf8();
        }
    }
    let name = word[from..]
        .split_whitespace()
        .next()
        .map(|name| name.trim_end_matches('\\').trim_matches(WORD_TRIM))
        .filter(|name| !name.is_empty())?;
    let stem = EXE_EXTENSIONS.iter().find_map(|ext| {
        let cut = name.len().checked_sub(ext.len()).filter(|cut| *cut > 0)?;
        name.get(cut..)
            .filter(|tail| tail.eq_ignore_ascii_case(ext))
            .map(|_| &name[..cut])
    });
    Some(stem.unwrap_or(name))
}

/// `FOO=1` in front of a program: environment, not the program.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The raw word in a segment's PROGRAM slot: past env assignments, past the
/// prompt glyph a scrape leaves in front (`$`, `❯`: no letter or digit — `{`
/// is the one keyword with none), and past ONE wrapper with its flags, so
/// `time for …` shows `for` exactly as `time make` shows `make`. The same slot
/// [`command_detail`] reads the program from; here it says whether a segment
/// opens a compound command.
fn leading_word(segment: &str) -> Option<&str> {
    let mut words = shell_words(segment)
        .map(|w| w.trim_matches(WORD_TRIM))
        .filter(|w| !is_assignment(w) && (*w == "{" || w.chars().any(char::is_alphanumeric)))
        .peekable();
    let first = words.next()?;
    // The same basename the program slot is read by ([`basename`]), so a
    // Windows launcher's `C:\…\env.exe` is the wrapper `env` here too.
    let base = basename(first).unwrap_or(first);
    if !WRAPPERS.contains(&base) {
        return Some(first);
    }
    while let Some(next) = words.peek() {
        if WRAPPER_FLAGS_WITH_VALUE.contains(next) {
            words.next();
            words.next();
        } else if next.starts_with('-') {
            words.next();
        } else {
            break;
        }
    }
    words.next()
}

/// `segment` cut into its WORDS where the shell cuts them: at whitespace
/// outside quotes. A single- or double-quoted stretch, and a character behind a
/// backslash, belong to the word they sit in — so a program at a path with a
/// space is ONE word:
/// `'/Users//me/Library/Application Support/aterm/pkg/agents/claude' --resume x`,
/// the shape of the line the harness's live upgrade relaunches Claude Code
/// with, is that path and two arguments, never `/Users//me/Library/Application`
/// followed by `Support/…/claude'` (the live E2E of 2026-09-26 read
/// `detail=Application` for every session the upgrade moved onto the managed
/// store). Each word is a borrowed slice of `segment` with its quotes and
/// backslashes still in it — nothing is copied: [`command_detail`]'s `clean`
/// trims the quotes off a word's ends, and [`basename`] takes the program's
/// name from after the last `/`, past any quoted or escaped space in a
/// directory.
///
/// A quote that joins words is also the way ARGUMENTS reach the program's
/// word when the line is not quoted the way this reads it, and whatever
/// follows the last `/` of that word is what `detail=` would publish — so a
/// word ends at the FIRST place either of two readings ends it ([`word_end`]):
/// the POSIX one (a backslash inside single quotes is literal: `'\''`), and
/// fish's (and bash's `$'…'`), where `\'` inside single quotes is a quote
/// character that does not close them —
/// the harness's own fish relaunch line quotes `/Users//o'neil/…/claude` as
/// `'/Users//o\'neil/…/claude'`, which the POSIX reading runs on into the
/// session id. A reading that ends with a quote still open (`don't --token
/// x`, a scrape cut mid-word, or `don't '/secret/tok'`, where the argument's
/// opening quote closes the program's apostrophe and the word runs through the
/// argument) ends the word at its FIRST whitespace, quoted or not — the one
/// end that no pairing of the quotes can move into an argument.
/// Backticks and `$(…)` are not tracked, as before.
fn shell_words(segment: &str) -> impl Iterator<Item = &str> {
    let mut rest = 0;
    std::iter::from_fn(move || {
        let start = rest + segment[rest..].find(|c: char| !c.is_whitespace())?;
        let end = word_end(segment, start, false).min(word_end(segment, start, true));
        rest = end;
        Some(&segment[start..end])
    })
}

/// Where the word that opens at byte `start` of `segment` ends, read by ONE
/// quoting dialect: `escape_in_single` is fish's (and `$'…'`'s) reading, in
/// which a backslash inside single quotes escapes the next character, and
/// POSIX's when false. The end is the first whitespace outside quotes, or the
/// segment's end — unless a quote is still open there, when the word ends at
/// the first whitespace after `start`, quoted or escaped or not (the segment's
/// end if there is none). Not the first whitespace after the quote that is
/// open at the end: that quote may be an argument's, with the program's
/// apostrophe closed by the argument's opening quote (`don't '/secret/tok'`),
/// so the stretch before it is already argument text. [`shell_words`] takes
/// the nearer end of the two readings.
fn word_end(segment: &str, start: usize, escape_in_single: bool) -> usize {
    let mut quote: Option<char> = None;
    let mut chars = segment[start..].char_indices();
    while let Some((offset, c)) = chars.next() {
        let at = start + offset;
        match quote {
            None if c.is_whitespace() => return at,
            None if c == '\'' || c == '"' => quote = Some(c),
            Some(q) if c == q => quote = None,
            _ => {}
        }
        if c == '\\' && (escape_in_single || quote != Some('\'')) {
            chars.next();
        }
    }
    if quote.is_none() {
        return segment.len();
    }
    // A quote is still open, so this reading paired the line's quotes wrongly
    // somewhere — and the pair it got wrong may be the program's apostrophe
    // with the ARGUMENT's opening quote (`don't '/secret/tok'`), which runs
    // the word through that argument. The word's first whitespace is the one
    // end no mispairing can move past the program's own text.
    segment[start..]
        .find(char::is_whitespace)
        .map_or(segment.len(), |offset| start + offset)
}

/// The most a `detail=` may say about a command line: the program's basename,
/// plus its first subcommand when that word is in the program's CLOSED
/// vocabulary ([`SUBCOMMANDS`]) — never an argument. This is the RFC §4 privacy
/// rule made executable: a command line carries tokens, hostnames and paths,
/// and an agent reading `status` across a fleet must learn WHO lives in a
/// session (`claude`, `codex`, `targo test`) without learning what it was told.
/// Only the first non-flag word is looked at, and only membership in the
/// vocabulary lets it through: a value-taking flag's argument (`git -C aterm
/// status` → `git`, `kubectl -n prod get` → `kubectl`) is a directory or a
/// namespace, exactly what the rule exists to keep off the wire, and a bare
/// identifier shape cannot tell it from a subcommand. The bound is the
/// vocabulary, not flag parsing: the program's own flags are skipped and their
/// values are not, so a value that itself spells a vocabulary word passes as
/// the subcommand (`git -C status log` → `git status`) — a wrong reading, and
/// still a word from the closed list. Env-assignment prefixes
/// are skipped and one wrapper (`sudo`/`env`/`time`/`nice`/`exec`/…, matched by
/// its basename, so `/usr/bin/env` is `env`) is unwrapped, so `FOO=1 sudo -u me
/// targo --unverified test -p x` still reads `targo test` and `exec claude
/// --resume` reads `claude`. Empty or whitespace input is `None`. A WORD is the
/// shell's ([`shell_words`]): a quoted or backslash-escaped space is part of
/// it, so a program at a path with a space reads its own name —
/// `'/Users//me/Library/Application Support/aterm/pkg/agents/claude' --resume x`
/// is `claude`, never `Application`. The published name never holds
/// whitespace: it ends at the first one (`"/opt/My App/My App"` is `My`), and
/// a word ends wherever a POSIX or a fish reading of the quotes ends it, and
/// at its own first whitespace when a reading leaves a quote open — so a line
/// quoted in a way this does not read (`don't --token x`, `don't
/// '/secret/tok'`, fish's `'/Users//o\'neil/bin/claude' '--resume' 'id'`)
/// still reads a name from the program's word, never text from an argument.
/// `env -S '<program> <args>'` (and `--split-string`) hands env one string it
/// splits itself, so the program — and a subcommand from the closed list — is
/// read from that string's words ([`env_split_string`]). The program's name
/// drops a Windows executable extension ([`basename`]), so a launcher's
/// `"C:\Program Files\Git\usr\bin\less.exe" README.md` reads `less` —
/// measured 2026-09-22 reading `Program` under a whitespace split.
///
/// A COMPOUND command line names the program of the segment that is RUNNING,
/// not the word the line happens to open with — measured: `cd ~/ay && claude`
/// read `detail=cd`, in exactly the launch shape the supervise-agent skill
/// recommends, while the house rule tells an agent to read `detail=` to learn
/// whether a peer is another agent before typing into it. So the RAW line is
/// cut at its top-level list operators before any word is looked at
/// ([`split_list`]: quote- and backslash-aware, so `claude -p "a; b"` is one
/// segment), and the segment is the one the shell would still be running
/// ([`running_segment`]): `;` (and a newline) binds loosest, so the LAST
/// non-blank `;`-group is read; inside it `&&`/`||` chain left to right, the
/// right operand of `&&` runs once the left SUCCEEDED and the right operand of
/// `||` runs only if the left FAILED — so trailing `|| …` alternatives are
/// dropped and the last operand left is the answer. `cd ~/ay && exec claude`
/// → `claude`, `cd /tmp; codex` → `codex`, `make || echo failed` → `make`,
/// `cd x && claude || echo failed` → `claude`. A pipeline's `|` and a
/// background `&` are not list operators here, so the first command of a
/// pipeline still names it, as before; subshell parentheses are not tracked,
/// so `(cd ~/ay && claude)` reads `claude` — the operators cut the same way
/// inside them.
///
/// A shell keyword in a segment's program slot is the exception:
/// `for i in 1 2 3; do sleep 1; done` is `detail=for`, and `if`/`while`/
/// `case`/`until`/`{` behave the same. Its `;`s belong to the keyword's grammar
/// (`for …; do`), not to a list — cutting there answers a CLOSER (`done`, `fi`,
/// `esac`, `}`), which is never a program — and reading the body would mean
/// parsing shell grammar here, where the rule is deliberately word-shaped and
/// privacy-bounded. `for` IS what the block is executing, so that is not a
/// wrong answer to hide. The keyword need not open the LINE: `cd x && for …;
/// done` and `time for …; done` cut to `done` while only the first word was
/// looked at, so the segments are scanned left to right and the FIRST whose
/// program slot ([`leading_word`]: past assignments, a prompt glyph and one
/// wrapper) holds an opener is read — `for`, `if`, `for` — before the running
/// segment is considered at all. The `status`/`sessions` verb entries say all
/// three rules in the same words, so the wire promise matches what this
/// returns.
///
/// `title_summary/description.rs`'s `short_command` is the precedent the RFC
/// names, but not this transform: it hides every program outside its own
/// allow-list as "a command", which is exactly the answer F5 found insufficient.
pub(crate) fn command_detail(cmdline: &str) -> Option<String> {
    /// The bound on the reply: enough for `kubectl port-forward`, never a
    /// screen-scraped line.
    const MAX_CHARS: usize = 48;

    fn clean(word: &str) -> std::borrow::Cow<'_, str> {
        let word = word.trim_matches(WORD_TRIM);
        if word.chars().any(char::is_control) {
            word.chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
                .into()
        } else {
            word.into()
        }
    }
    // Which part of the line to read: the FIRST segment whose program slot
    // holds a keyword opener, else the segment that is running. A keyword's
    // `;`s are its own grammar, so the running-segment cut of `for …; do …;
    // done` — wherever the `for` sits on the line — is a closer (`done`,
    // `fi`, `esac`, `}`), never a program. The one line whose running segment
    // opens with a closer and has no opener before it is a syntax error the
    // shell never runs, so that shape is left to the cut.
    // This runs during every due status observation. Parse once and keep the
    // segment and ordinary words borrowed through the final privacy filter.
    let segments = split_list(cmdline);
    let segment = segments
        .iter()
        .map(|(_, text)| *text)
        .find(|text| leading_word(text).is_some_and(|w| KEYWORD_OPENERS.contains(&w)))
        .unwrap_or_else(|| running_segment(cmdline, &segments));
    let mut words = shell_words(segment)
        .map(clean)
        .filter(|w| !w.is_empty())
        .peekable();

    // `FOO=1 BAR=2 cmd` — the assignments are environment, not the program;
    // a word with no letter or digit at all (`$`, `%`, `❯`, `>`) is a prompt
    // glyph a scrape left in front of the command, not a program either.
    while words
        .peek()
        .is_some_and(|w| is_assignment(w) || !w.chars().any(char::is_alphanumeric))
    {
        words.next();
    }
    // Every word in the program slot is reduced to its BASENAME before it is
    // looked at, the wrapper included: a shell that reports `/usr/bin/env
    // FOO=1 codex` names the same wrapper as `env FOO=1 codex`, and comparing
    // the full path would leave the `env` in place as the "program". The
    // module-level [`basename`] says what a basename is here — up to its first
    // whitespace, less a Windows executable extension — and [`leading_word`]
    // reads the wrapper slot by the same one.
    let mut program_word = words.next()?;
    // `env -S '<program> <args>'`: the one string env splits ([`env_split_string`]).
    let mut split: Option<String> = None;
    if WRAPPERS.contains(&basename(&program_word)?) {
        let env = basename(&program_word) == Some("env");
        // Unwrap one level: skip the wrapper's own flags (and the value a
        // value-taking flag consumes), then any assignments `env` carries.
        while let Some(next) = words.peek() {
            let split_flag = env
                .then(|| env_split_string(next))
                .flatten()
                .map(|attached| attached.map(str::to_string));
            if let Some(attached) = split_flag {
                words.next();
                split = match attached {
                    Some(attached) => Some(attached),
                    None => Some(words.next()?.into_owned()),
                };
                break;
            } else if WRAPPER_FLAGS_WITH_VALUE.contains(&next.as_ref()) {
                words.next();
                words.next();
            } else if next.starts_with('-') || is_assignment(next) {
                words.next();
            } else {
                break;
            }
        }
        if split.is_none() {
            program_word = words.next()?;
        }
    }
    // The split string's words past its program stand where the line's
    // would for the subcommand below; env's own options and assignments may
    // open the string (`-S '-i FOO=1 node x'`).
    let split_words = split.as_deref().map(|string| {
        string
            .split_whitespace()
            .map(|w| w.trim_matches(WORD_TRIM))
            .filter(|w| !w.is_empty() && !w.starts_with('-') && !is_assignment(w))
            .collect::<Vec<_>>()
    });
    let program = match &split_words {
        Some(split_words) => basename(split_words.first()?)?,
        None => basename(&program_word)?,
    };
    // Bound before allocating too: truncating an owned long token would keep
    // its entire command-sized capacity in the published status record.
    let program_end = program
        .char_indices()
        .nth(MAX_CHARS)
        .map_or(program.len(), |(end, _)| end);
    let mut detail = program[..program_end].to_string();
    if let Some((_, vocabulary)) = SUBCOMMANDS.iter().find(|(p, _)| *p == program) {
        // The first word after the program that does not start with `-`, and
        // ONLY when the vocabulary owns it; otherwise the program stands
        // alone. The program's own flags are not modelled: a flag is skipped,
        // its value is not, so the value IS the candidate. Membership is the
        // whole gate, and it bounds the miss in both directions — `git -C
        // aterm status` reads `git` (the real subcommand lost behind a value
        // outside the list), `git -C status log` reads `git status` (a value
        // that spells a list word taken as the subcommand). Wrong either way,
        // never an argument: nothing outside the closed list can follow the
        // program on the wire.
        let sub = match &split_words {
            Some(split_words) => split_words.get(1).map(|w| std::borrow::Cow::Borrowed(*w)),
            None => words.find(|w| !w.starts_with('-')),
        };
        if let Some(sub) = sub.filter(|s| vocabulary.contains(&s.as_ref())) {
            detail.push(' ');
            detail.push_str(&sub);
        }
    }
    if let Some((end, _)) = detail.char_indices().nth(MAX_CHARS) {
        detail.truncate(end);
    }
    Some(detail)
}

/// A list operator between two segments of a command line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ListOp {
    /// `;` or a newline: the right side runs after the left, whatever it did.
    Seq,
    /// `&&`: the right side runs only after the left SUCCEEDED.
    And,
    /// `||`: the right side runs only if the left FAILED.
    Or,
}

/// `cmdline` cut at its top-level list operators — `;` and newline
/// ([`ListOp::Seq`]), `&&` ([`ListOp::And`]), `||` ([`ListOp::Or`]) — each
/// segment paired with the operator that PRECEDES it (the first with `Seq`).
/// Single and double quotes, backticks and backslash escapes are honoured, so
/// an operator inside an argument (`claude -p "a; b"`) does not cut. Nothing
/// more of the grammar is modelled: a single `|` (pipeline) or `&` (background
/// job) stays inside its segment, and `(`/`$(` nesting is not tracked. Every
/// cut lands on an ASCII operator byte, so the slices are always on char
/// boundaries.
fn split_list(cmdline: &str) -> Vec<(ListOp, &str)> {
    let bytes = cmdline.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut op = ListOp::Seq;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            // Inside quotes only the matching quote ends them; a backslash
            // escapes the next byte except inside single quotes.
            if c == b'\\' && q != b'\'' {
                i += 2;
            } else {
                if c == q {
                    quote = None;
                }
                i += 1;
            }
            continue;
        }
        let cut = match c {
            b'\\' => {
                i += 2;
                continue;
            }
            b'\'' | b'"' | b'`' => {
                quote = Some(c);
                i += 1;
                continue;
            }
            b';' | b'\n' => Some((ListOp::Seq, 1)),
            b'&' if bytes.get(i + 1) == Some(&b'&') => Some((ListOp::And, 2)),
            b'|' if bytes.get(i + 1) == Some(&b'|') => Some((ListOp::Or, 2)),
            _ => None,
        };
        match cut {
            Some((next, width)) => {
                segments.push((op, &cmdline[start..i]));
                op = next;
                i += width;
                start = i;
            }
            None => i += 1,
        }
    }
    segments.push((op, &cmdline[start..]));
    segments
}

/// The segment of a command line that is RUNNING once its predecessors have
/// had their say — the one [`command_detail`] reads. `;`/newline bind loosest,
/// so the last `;`-group with any content is taken; within it `&&`/`||` chain
/// left to right, and a trailing `|| …` is the alternative that runs only on
/// failure, so it is dropped and the last operand left is the answer. A line
/// with no list operator (or nothing but blank ones) is its own segment.
fn running_segment<'a>(cmdline: &'a str, segments: &[(ListOp, &'a str)]) -> &'a str {
    // The last nonblank operand identifies the last nonblank `;` group. Any
    // trailing blank operands would be dropped below anyway, so the group's
    // borrowed slice can end there. No nested group vectors or second parse.
    let Some(end) = segments
        .iter()
        .rposition(|(_, text)| !text.trim().is_empty())
    else {
        return cmdline;
    };
    let start = segments[..=end]
        .iter()
        .rposition(|(op, _)| *op == ListOp::Seq)
        .unwrap_or(0);
    let mut chain = &segments[start..=end];
    // `a && b || c`: `c` runs only if `a && b` failed, so it is not the running
    // segment; `a && b` is left, and its last operand is. A blank operand (a
    // trailing `&&`) is dropped the same way.
    while chain.len() > 1
        && chain
            .last()
            .is_some_and(|(op, text)| *op == ListOp::Or || text.trim().is_empty())
    {
        chain = &chain[..chain.len() - 1];
    }
    chain.last().map_or(cmdline, |(_, text)| text)
}

/// The programs whose first subcommand a `detail=` may name, each with the
/// CLOSED vocabulary it is checked against. Membership, not shape, is the
/// gate: a word outside the list is an argument by definition, whatever it
/// looks like. `make`/`just` targets are user-named, so only the conventional
/// ones are listed — `make deploy-prod` reads `make`, and the target stays in
/// the pane. Extending a list is a wording change to the wire, so the table
/// test names one row per program.
const SUBCOMMANDS: &[(&str, &[&str])] = &[
    (
        "git",
        &[
            "status",
            "commit",
            "push",
            "pull",
            "fetch",
            "log",
            "diff",
            "checkout",
            "switch",
            "rebase",
            "merge",
            "clone",
            "add",
            "stash",
            "branch",
            "tag",
            "reset",
            "restore",
            "show",
            "init",
            "bisect",
            "cherry-pick",
            "worktree",
            "remote",
            "submodule",
            "blame",
            "grep",
            "rm",
            "mv",
            "apply",
            "am",
            "format-patch",
            "describe",
            "reflog",
            "revert",
            "clean",
        ],
    ),
    ("cargo", CARGO_SUBCOMMANDS),
    ("targo", CARGO_SUBCOMMANDS),
    ("npm", NODE_SUBCOMMANDS),
    ("pnpm", NODE_SUBCOMMANDS),
    ("yarn", NODE_SUBCOMMANDS),
    (
        "aterm",
        &[
            "ctl",
            "conn",
            "pkg",
            "fleet",
            "drive",
            "ship",
            "update",
            "agents",
            "help",
            "new-tab",
            "new-window",
            "split-pane",
        ],
    ),
    (
        "atpkg",
        &[
            "install",
            "list",
            "update",
            "doctor",
            "status",
            "which",
            "run",
            "uninstall",
            "rollback",
            "pin",
            "unpin",
            "verify",
            "gc",
            "seed",
        ],
    ),
    (
        "docker",
        &[
            "compose",
            "run",
            "build",
            "ps",
            "exec",
            "pull",
            "push",
            "images",
            "logs",
            "stop",
            "start",
            "restart",
            "rm",
            "rmi",
            "inspect",
            "login",
            "logout",
            "tag",
            "volume",
            "network",
            "system",
            "container",
            "image",
            "stats",
            "kill",
            "attach",
            "cp",
            "commit",
            "save",
            "load",
            "buildx",
        ],
    ),
    (
        "kubectl",
        &[
            "get",
            "apply",
            "delete",
            "logs",
            "exec",
            "port-forward",
            "describe",
            "create",
            "edit",
            "rollout",
            "scale",
            "config",
            "cluster-info",
            "top",
            "explain",
            "patch",
            "label",
            "annotate",
            "cp",
            "attach",
            "auth",
            "drain",
            "cordon",
            "uncordon",
            "taint",
            "wait",
            "diff",
            "kustomize",
            "version",
        ],
    ),
    ("make", CONVENTIONAL_TARGETS),
    ("just", CONVENTIONAL_TARGETS),
];

/// `cargo`'s subcommands, shared with `targo` (the same tool behind a
/// verifier).
const CARGO_SUBCOMMANDS: &[&str] = &[
    "build",
    "test",
    "check",
    "run",
    "clippy",
    "fmt",
    "doc",
    "bench",
    "install",
    "update",
    "add",
    "remove",
    "new",
    "init",
    "publish",
    "clean",
    "tree",
    "metadata",
    "nextest",
    "expand",
    "miri",
    "audit",
    "deny",
    "fix",
    "package",
    "search",
    "vendor",
    "uninstall",
];

/// The subcommands the three node package managers share. A `run` SCRIPT name
/// (`npm run build:prod`) is an argument and stays in the pane.
const NODE_SUBCOMMANDS: &[&str] = &[
    "run", "install", "test", "build", "start", "dev", "ci", "add", "remove", "exec", "publish",
    "init", "link", "update", "outdated", "audit", "lint", "format", "dlx", "create", "why",
    "list",
];

/// The `make`/`just` targets common enough to be a vocabulary rather than a
/// name; anything else is the user's own word.
const CONVENTIONAL_TARGETS: &[&str] = &[
    "all",
    "build",
    "test",
    "check",
    "clean",
    "install",
    "uninstall",
    "run",
    "fmt",
    "format",
    "lint",
    "clippy",
    "doc",
    "docs",
    "release",
    "dist",
    "deploy",
    "dev",
    "watch",
    "ci",
    "bench",
    "help",
    "setup",
    "update",
];

/// Short, human display text for chrome. `None` for phases not worth showing —
/// an honest blank beats a confident "unknown".
pub(crate) fn summary_text(status: &Status) -> Option<String> {
    // Label a guess as a guess. Strong/exact conclusions read as plain facts;
    // anything weaker is suffixed so chrome never presents inference as
    // observation (RFC §16).
    let hedge = |text: String| {
        if status.confidence >= Confidence::Strong {
            text
        } else {
            format!("{text} ({})", status.confidence.as_str())
        }
    };
    let text = match (status.phase, status.last_outcome) {
        (Phase::Unknown, _) => return None,
        (Phase::Starting, _) => "starting".to_string(),
        (Phase::Running, _) => match &status.detail {
            Some(detail) if !detail.is_empty() => format!("running {detail}"),
            _ => "running".to_string(),
        },
        (Phase::Quiet, _) => "quiet".to_string(),
        (Phase::Idle, Outcome::Failure { exit_code }) => format!("failed (exit {exit_code})"),
        (Phase::Idle, Outcome::Signal { signal }) => format!("killed (signal {signal})"),
        (Phase::Idle, Outcome::Success) => "done".to_string(),
        (Phase::Idle, Outcome::None) => "ready".to_string(),
        (Phase::Exited, Outcome::Failure { exit_code }) => format!("exited ({exit_code})"),
        (Phase::Exited, Outcome::Signal { signal }) => format!("exited (signal {signal})"),
        (Phase::Exited, _) => "exited".to_string(),
    };
    // A failure is never hedged away: if the exit code says it failed, that is
    // an observed fact regardless of how the phase was reached.
    Some(if status.last_outcome.is_failure() {
        text
    } else {
        hedge(text)
    })
}

/// Map one session's status onto the tab's INDEPENDENT indicator bits.
///
/// `dirty` is never claimed here: it means unsaved editor state, which a
/// terminal has none of. A split tab folds these bits per LEAF
/// ([`crate::tab_model::aggregate_presentations`]) rather than folding phases
/// into one — a highest-attention phase would let an `Exited` pane hide a
/// `Running` sibling's failure, which is exactly what
/// [`crate::tab_model::TabIndicators`] exists to prevent.
#[must_use]
pub(crate) fn status_indicators(status: &Status) -> crate::tab_model::TabIndicators {
    crate::tab_model::TabIndicators {
        dirty: false,
        // Quiet is NOT idle: it is only ever emitted when a foreground job is
        // known to exist, and means the job has simply printed nothing for
        // `quiet_after`. Mapping busy to Running alone would dark the dot in the
        // middle of a link step and read as "finished".
        busy: matches!(status.phase, Phase::Running | Phase::Quiet),
        // A native leaf's own out-of-band mark. A terminal never raises it, and
        // the status path must never write it — that separation is what lets the
        // classifier's bit below be recomputed instead of latched.
        attention: false,
        // The outcome outlives the phase that produced it (see [`Outcome`]), so a
        // command that failed keeps its tab marked while the shell honestly sits
        // back at `Idle` — the tab is how a user learns about a pane they are
        // not looking at. A FAILURE is the only thing that raises attention:
        // there is no local evidence for "this pane is waiting for you" (see the
        // module docs), and inventing one from silence would mark half the
        // window.
        status_attention: status.last_outcome.is_failure(),
    }
}

/// Owns one [`StatusFsm`] per live session and enforces the observation budget:
/// at most one classification per session per `min_interval`, so an output flood
/// cannot turn into a classification flood.
#[derive(Debug)]
pub(crate) struct StatusObserver {
    policy: StatusPolicy,
    min_interval: Duration,
    /// Last applied `tab_status_badge`. Held here purely so a flip is
    /// DETECTABLE — it gates chrome, not classification, so nothing else in
    /// this module reads it.
    badge: bool,
    sessions: std::collections::HashMap<u64, SessionSlot>,
    /// MIN OVER `sessions`' deadlines — a LOWER BOUND on it, never an upper
    /// one. This is the whole of MPT-4's fix: "which sessions are past their
    /// per-session deadline" is a min-over-deadlines question, and it was
    /// answered by a full `pool.iter()` scan with a `HashMap` probe per session
    /// at the TOP of every `Wake::Output` arm — i.e. thousands of times a
    /// second under a flood, to usually find zero. `None` means "no slot is
    /// known to be rate-limited", which for a fresh observer is exactly right:
    /// the gate stays open.
    ///
    /// LOWER BOUND, DELIBERATELY: [`StatusObserver::observe`] only folds the
    /// new deadline in (an O(1) `min`), which can leave this EARLIER than the
    /// true minimum after a slot is pushed forward; the exact value is restored
    /// by [`StatusObserver::note_swept`] at the end of each sweep. Too-early
    /// means one extra scan; too-late would mean a missed classification, and
    /// no path here can produce it.
    next_due_any: Option<Instant>,
    /// The `SessionPool::insert_epoch` observed at the last COMPLETED sweep.
    /// While it still matches, every live session is one this observer has been
    /// offered, so `next_due_any` covers the whole pool; a mismatch means a
    /// brand-new session may be waiting for its first classification and the
    /// gate must open regardless of the deadlines. See `SessionPool::insert_epoch`.
    swept_pool_epoch: u64,
    /// Pooled sessions the last full sweep reached but could NOT classify —
    /// their terminal `try_lock` lost to the PTY reader — so they still have no
    /// slot. Every output wake retries exactly these (not the pool) until each
    /// is classified. See [`StatusObserver::note_swept`].
    unclassified: Vec<u64>,
    /// THE SERVER'S AGENT VERDICT, per session ([`AgentWatch`]).
    agents: std::collections::HashMap<u64, AgentWatch>,
    /// Monotonic across every session and every [`Self::clear`]: the sequence
    /// number a changed reading is stamped with, so presence folds only a
    /// NEWER reading than the one it holds.
    agent_seq: u64,
    /// [`Self::agent_seq`] at the last [`Self::clear`]: the sequence an
    /// unwatched session answers with, so a reading from before the clear is
    /// retired rather than kept.
    agent_cleared_at: u64,
    /// Names a foreground group's program off the event loop.
    programs: program::ProgramResolver,
    /// THE INPUT WATCH (2026-09-24): which sessions have input their program
    /// has not read, and the stall published for each
    /// ([`crate::input_stall::InputWatches`]). Not part of the `tab_status`
    /// subsystem — [`Self::clear`] leaves it alone, because a published stall
    /// is also a server attention entry that only its own probe may clear.
    pub(crate) inputs: crate::input_stall::InputWatches,
    /// When each Claude Code session's footer facts were last asked for, so a
    /// sweep at the classification rate asks at most every
    /// [`crate::claude_footer::RECHECK`].
    footer_asked: std::collections::HashMap<u64, FooterAsk>,
}

#[derive(Clone, Copy, Debug)]
struct FooterAsk {
    at: Instant,
    pgid: i32,
    stopped: bool,
}

#[derive(Debug)]
struct SessionSlot {
    fsm: StatusFsm,
    next_due: Instant,
    /// Bumped only when the PUBLISHED status changes, so chrome can skip
    /// recomposition on an unchanged frame.
    revision: u64,
    /// STICKY: how this session's PTY ended, once it has. Sticky because a
    /// `--hold` pane outlives its shell and keeps being swept — without this the
    /// next sweep would find `tcgetpgrp` failing, read that as "no foreground
    /// job", and publish `Idle`, which chrome renders as "ready". A dead pane
    /// claiming to be a shell at a prompt is the one lie this field exists to
    /// prevent.
    lifecycle: Option<Lifecycle>,
}

impl StatusObserver {
    pub(crate) fn new(policy: StatusPolicy, min_interval: Duration) -> Self {
        Self {
            policy,
            min_interval,
            // Matches `tab_status_badge`'s default, so the first reconfigure
            // after startup does not report a phantom flip.
            badge: true,
            sessions: std::collections::HashMap::new(),
            // No slots yet: the gate is open until the first sweep records one.
            next_due_any: None,
            swept_pool_epoch: 0,
            unclassified: Vec::new(),
            agents: std::collections::HashMap::new(),
            agent_seq: 0,
            agent_cleared_at: 0,
            programs: program::ProgramResolver::default(),
            inputs: crate::input_stall::InputWatches::default(),
            footer_asked: std::collections::HashMap::new(),
        }
    }

    /// Ask for `session`'s footer facts when Claude Code is the program in
    /// front and the last ask is at least [`crate::claude_footer::RECHECK`]
    /// old. The answer arrives on the resolver thread; a change wakes the loop.
    pub(crate) fn request_footer_if_due(
        &mut self,
        session: u64,
        timeline: &std::sync::Arc<std::sync::Mutex<crate::session_timeline::SessionTimeline>>,
        program: Option<&str>,
        pgid: i32,
        now: Instant,
    ) {
        if program != Some("claude") || pgid <= 0 {
            // A held pane can outlive Claude with no valid foreground group;
            // it will not be retired, and the program resolver cannot name a
            // replacement to stop. Use the group we last asked for, or the
            // timeline's still-named Claude group if only the program worker
            // made the first request. Mark the stop so an idle sweep does not
            // send another message every observation interval.
            let prior = self.footer_asked.get(&session).copied();
            if prior.is_some_and(|ask| ask.stopped)
                || (prior.is_none() && !(pgid <= 0 && program == Some("claude")))
            {
                return;
            }
            let current = timeline.lock().unwrap_or_else(|p| p.into_inner());
            // The sampled program was read before the resolver's answer. Hold
            // this leaf lock through the stop send: if a newer Claude answer
            // already landed, keep it; if it lands next, its request follows
            // this stop in channel order.
            if pgid > 0
                && current.agent().program_pgid == pgid
                && current.agent().program.as_deref() == Some("claude")
            {
                return;
            }
            let old = prior.map(|ask| ask.pgid).or_else(|| {
                (pgid <= 0 && program == Some("claude")).then_some(current.agent().program_pgid)
            });
            if let Some(old) = old.filter(|old| *old > 0) {
                self.footer_asked.insert(
                    session,
                    FooterAsk {
                        at: now,
                        pgid: old,
                        stopped: true,
                    },
                );
                crate::claude_footer::stop(session, old);
            }
            return;
        }
        let due = self.footer_asked.get(&session).is_none_or(|ask| {
            ask.stopped
                || ask.pgid != pgid
                || now.saturating_duration_since(ask.at) >= crate::claude_footer::RECHECK
        });
        if due {
            // The sampled program can become stale while this sweep runs. A
            // worker may have named a different program and stopped its watch
            // already; hold the leaf lock through this send so that stale
            // sample cannot restart the stopped watch after that transition.
            let current = timeline.lock().unwrap_or_else(|p| p.into_inner());
            if current.agent().program_pgid != pgid
                || current.agent().program.as_deref() != Some("claude")
            {
                return;
            }
            self.footer_asked.insert(
                session,
                FooterAsk {
                    at: now,
                    pgid,
                    stopped: false,
                },
            );
            crate::claude_footer::request_footer(session, timeline, pgid);
        }
    }

    /// Session `session`'s published input stall, if any.
    pub(crate) fn input_stall(&self, session: u64) -> Option<&crate::input_stall::InputStallFact> {
        self.inputs.published(session)
    }

    /// Whether session `session`'s input is being watched — its output wakes
    /// then probe it (at most every `input_stall::PROBE_MIN_GAP`).
    pub(crate) fn input_armed(&self, session: u64) -> bool {
        self.inputs.armed(session)
    }

    /// Stop watching `session`'s input; returns the stall it had published.
    pub(crate) fn forget_input(
        &mut self,
        session: u64,
    ) -> Option<crate::input_stall::InputStallFact> {
        self.inputs.forget(session)
    }

    /// The input watch's earliest deadline (`DeadlineOwner::InputWatch`);
    /// `None` on a machine with no unread input, which arms nothing.
    pub(crate) fn next_input_wake(&self) -> Option<Instant> {
        self.inputs.next_wake()
    }

    /// Could ANY known session be past its deadline right now? The O(1) half of
    /// the sweep's gate (the other half is the pool epoch — see
    /// `swept_pool_epoch`). `None` (nothing known) answers YES, so an observer
    /// that has never seen a session never gates anything out.
    pub(crate) fn any_due(&self, now: Instant) -> bool {
        self.next_due_any.is_none_or(|due| now >= due)
    }

    /// A sweep just walked the whole pool: make `next_due_any` EXACT again,
    /// bank `pool_epoch`, and record the pooled sessions the sweep left
    /// slot-less. O(slots), on the classification path only — which is 4/s at
    /// the default interval, not the burst rate the gate spares.
    ///
    /// `unclassified` is the load-bearing part. A sweep can leave a session
    /// unclassified: the classify path takes the session's terminal with
    /// `try_lock` and SKIPS the session when the PTY reader holds it — which,
    /// under exactly the output flood this gate exists to survive, is not
    /// rare. A session skipped that way still has no slot, so `due()` would
    /// report it due and the whole-pool scan WOULD classify it on the next
    /// wake, while `next_due_any` (a min over slots that do not include it)
    /// would not. Banking the epoch with nothing else would close the gate
    /// over a session it has never seen. Recording the id keeps the gate open
    /// for exactly that session — the next wake retries IT, not the pool —
    /// until it is classified, which keeps the gate EXACTLY equivalent to the
    /// scan it replaces, the only property that makes it safe to ship. (This
    /// used to leave the epoch un-banked instead, which re-walked the whole
    /// pool on every wake while the newcomer stayed contended.)
    pub(crate) fn note_swept(&mut self, pool_epoch: u64, unclassified: Vec<u64>) {
        self.swept_pool_epoch = pool_epoch;
        self.unclassified = unclassified;
        self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
    }

    /// Whether a sweep left a pooled session slot-less (see
    /// [`StatusObserver::note_swept`]) — the one reason the gate opens while
    /// every known deadline is in the future and no session is new.
    pub(crate) fn has_unclassified(&self) -> bool {
        !self.unclassified.is_empty()
    }

    /// Hand the unclassified ids to a retry pass (which puts back the ones it
    /// still could not classify with [`Self::restore_unclassified`]).
    pub(crate) fn take_unclassified(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.unclassified)
    }

    /// See [`Self::take_unclassified`].
    pub(crate) fn restore_unclassified(&mut self, ids: Vec<u64>) {
        self.unclassified = ids;
    }

    /// Whether this observer holds a slot for `session` — i.e. whether it has
    /// ever successfully classified it. See [`StatusObserver::note_swept`].
    pub(crate) fn knows(&self, session: u64) -> bool {
        self.sessions.contains_key(&session)
    }

    /// See [`StatusObserver::note_swept`].
    pub(crate) fn swept_pool_epoch(&self) -> u64 {
        self.swept_pool_epoch
    }

    /// Whether this session is allowed a classification now. Callers check this
    /// BEFORE taking the terminal lock, so a rate-limited session costs nothing.
    pub(crate) fn due(&self, session: u64, now: Instant) -> bool {
        self.sessions
            .get(&session)
            .is_none_or(|slot| now >= slot.next_due)
    }

    /// Charge a session's observation interval for a sample that could NOT be
    /// taken — the sweep reached it and refused, rather than never reaching it.
    ///
    /// THE CLAMP NEEDS A FLOOR THAT ALWAYS ADVANCES (busy-rearm audit, item 2).
    /// [`StatusObserver::next_wake`] clamps each slot to `next_due` and calls
    /// that a structural bound of the observation rate — but `next_due` moves
    /// in exactly one place, [`StatusObserver::observe`], and the sweep has a
    /// second refusal that never reaches it: the terminal `try_lock` lost to
    /// the PTY reader. A slot whose owed instant is already past and whose lock
    /// stays contended therefore re-arms the SAME past instant on every turn —
    /// `WaitUntil(past)` fires, the sweep skips, nothing advances — which is
    /// the event-loop-rate spin the clamp was written to make impossible, under
    /// precisely the sustained-output workload that contends the lock.
    ///
    /// Charging the interval is also the honest accounting: this subsystem's
    /// contract is at most one classification attempt per session per
    /// `min_interval`, and a sample we could not take is an attempt spent. It
    /// costs a contended session one interval of delay in a transition it could
    /// not have observed anyway.
    ///
    /// Only a KNOWN session is charged. A slot-less session contributes no
    /// deadline, so it cannot spin the loop, and inserting a slot here would
    /// bank the pool epoch over a session that has never been classified — the
    /// exact defect [`StatusObserver::note_swept`] refuses.
    pub(crate) fn note_skipped(&mut self, session: u64, now: Instant) {
        let Some(slot) = self.sessions.get_mut(&session) else {
            return;
        };
        let due = now + self.min_interval;
        slot.next_due = due;
        // What the refused look would have read is owed one interval on —
        // the PTY reader holding the lock is output arriving.
        self.note_output(session, now);
        // Same LOWER-bound fold as `observe`; `note_swept` restores exactness.
        self.next_due_any = Some(self.next_due_any.map_or(due, |min| min.min(due)));
    }

    /// Fold one observation in. Returns `true` when the published status changed.
    ///
    /// A recorded exit OVERRIDES the caller's `lifecycle` field: the PTY ending
    /// is a fact about the session, and no later sample of a dead pane can
    /// un-end it.
    pub(crate) fn observe(&mut self, session: u64, evidence: &Evidence, now: Instant) -> bool {
        let policy = self.policy;
        let slot = self.sessions.entry(session).or_insert_with(|| SessionSlot {
            fsm: StatusFsm::new(policy, now),
            next_due: now,
            revision: 1,
            lifecycle: None,
        });
        slot.next_due = now + self.min_interval;
        let changed = match slot.lifecycle {
            Some(lifecycle) => {
                let mut evidence = *evidence;
                evidence.lifecycle = Some(lifecycle);
                slot.fsm.observe(&evidence, now)
            }
            None => slot.fsm.observe(evidence, now),
        };
        if changed {
            slot.revision = slot.revision.saturating_add(1);
        }
        // FOLD, never recompute. Keeping `next_due_any` a LOWER bound on the
        // true minimum is what makes the O(1) gate safe without an O(slots)
        // pass per observation; `note_swept` restores exactness once per sweep.
        // Folded here, after the slot borrow has ended, rather than beside the
        // assignment above.
        let due = now + self.min_interval;
        self.next_due_any = Some(self.next_due_any.map_or(due, |min| min.min(due)));
        changed
    }

    /// Record that this session's PTY ended, and classify it at once.
    ///
    /// Called from the `Wake::Exit` edge rather than discovered on the sweep:
    /// the exit status is only collectable in the window before teardown, and
    /// polling the session store from the observation path would put a second
    /// lock on the output path for a fact that arrives as an event anyway.
    /// Returns whether the published status changed.
    pub(crate) fn note_exit(
        &mut self,
        session: u64,
        lifecycle: Lifecycle,
        evidence: &Evidence,
        now: Instant,
    ) -> bool {
        let policy = self.policy;
        self.sessions
            .entry(session)
            .or_insert_with(|| SessionSlot {
                fsm: StatusFsm::new(policy, now),
                next_due: now,
                revision: 1,
                lifecycle: None,
            })
            .lifecycle = Some(lifecycle);
        // Exit is `Confidence::Exact`, so it bypasses dwell and publishes on
        // this very call — a session that has ended must not be reported as
        // running for another three-quarters of a second.
        self.observe(session, evidence, now)
    }

    /// Earliest instant at which SOME session owes a time-driven transition, for
    /// the event loop's wait deadline. Without this the classifier is purely
    /// edge-driven: a build that finishes and then prints nothing leaves its tab
    /// showing busy forever, because the observation that would retire it never
    /// runs.
    /// Remember the badge switch so a flip is detectable. Returns whether it
    /// actually moved, which is the caller's signal to refold every tab.
    pub(crate) fn set_badge(&mut self, badge: bool) -> bool {
        std::mem::replace(&mut self.badge, badge) != badge
    }

    pub(crate) fn next_wake(&self) -> Option<Instant> {
        // THE AGENT FOLLOW-UP: a session whose content moved at its last look,
        // or whose output landed between looks, is looked at once more, so the
        // frame an agent drew last (an approval box, then silence) is read
        // within one interval of it. The look waits for the output to pause
        // (`output_at + min_interval`, never before the slot's next due
        // instant): output still arriving brings its own due looks, and a
        // timer that fires between them is a poll.
        let agent_wake = self
            .agents
            .iter()
            .filter_map(|(id, w)| {
                let slot = self.sessions.get(id)?;
                let followup = w.followup.then(|| {
                    w.output_at.map_or(slot.next_due, |at| {
                        at.checked_add(self.min_interval)
                            .unwrap_or(at)
                            .max(slot.next_due)
                    })
                });
                // A missing argv[0] can be transient (a group just exec'd).
                // An unchanged screen cannot cause another request itself,
                // so this deadline is the only route to a retry. The slot's
                // observation floor prevents a past deadline from spinning.
                let unresolved = if w.pgid > 0 && w.program_seen == Some(None) {
                    w.resolved_at.map(|at| {
                        (at + unresolved_program_recheck(w.resolve_attempts)).max(slot.next_due)
                    })
                } else {
                    None
                };
                let confirmation = w
                    .deferred_name_recheck_at
                    .filter(|_| w.pgid > 0)
                    .map(|at| at.max(slot.next_due));
                [followup, unresolved, confirmation]
                    .into_iter()
                    .flatten()
                    .min()
            })
            .min();
        let owed = self
            .sessions
            .values()
            .filter_map(|slot| {
                // STRUCTURAL CLAMP (busy-rearm audit, item 2): an armed wake
                // at which observation is forbidden is a contradiction —
                // `due()` refuses this slot until `next_due`, so a deadline
                // before its own observation gate can only fire, be refused,
                // and re-arm. Clamping per slot converts ANY future bug of
                // that class from an event-loop-rate spin to the observation
                // rate (<= 4 Hz at the default interval), and retires the
                // bounded dwell-edge spin (owed = first_seen + dwell in the
                // past while the gate still had up to `min_interval` to run).
                slot.fsm.owed_wake().map(|owed| owed.max(slot.next_due))
            })
            .min();
        match (owed, agent_wake) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The resolver finished after this session's ordinary one-interval
    /// follow-up. Arm one more observation at the slot's existing floor; the
    /// worker never takes the terminal lock and posts only when the timeline's
    /// current foreground program actually changed. A retired session is a
    /// no-op. This is a level, so a burst of completions still owns one wake.
    pub(crate) fn note_program_answered(&mut self, session: u64) {
        if self.sessions.contains_key(&session)
            && let Some(watch) = self.agents.get_mut(&session)
        {
            watch.followup = true;
        }
    }

    pub(crate) fn status(&self, session: u64) -> Option<&Status> {
        self.sessions.get(&session).map(|slot| slot.fsm.status())
    }

    /// The published agent reading presence folds: its sequence number and
    /// the reading (`None` = not an identified agent). A session never
    /// classified answers the sequence of the last [`Self::clear`] (`0` before
    /// any), which retires a reading from before it and applies nothing else.
    pub(crate) fn agent_reading(
        &self,
        session: u64,
    ) -> (u64, Option<crate::presence::AgentReading>) {
        match self.agents.get(&session) {
            Some(w) if w.reading_seq > 0 => (w.reading_seq, w.reading.clone()),
            _ => (self.agent_cleared_at, None),
        }
    }

    /// OUTPUT ARRIVED for `session` at `now` (the output wake, before its
    /// sweep): its agent verdict owes a look once the output pauses. A sweep
    /// that looks now answers for itself (its look sets the follow-up from
    /// what it read); one that cannot — the session not due yet, the whole
    /// sweep gated, the terminal contended — leaves this armed, so the FINAL
    /// frame of a burst that landed between looks is read within one interval
    /// of it, never at whatever unrelated deadline comes next (a box answered
    /// and a static busy screen drawn read `agent=prompt` for seconds; an
    /// exited agent's shell prompt left `program=claude` standing). No poll:
    /// one deadline, armed by the output itself and pushed on by each further
    /// burst, so it fires only after the last one ([`Self::next_wake`]).
    pub(crate) fn note_output(&mut self, session: u64, now: Instant) {
        if let Some(w) = self.agents.get_mut(&session) {
            w.followup = true;
            w.output_at = Some(now);
        }
    }

    /// Whether this observation should read the live zone (the caller holds
    /// the terminal guard): the screen generation or the terminal's `cursor`
    /// moved since the last read, or the
    /// published program is not the one the zone was last judged under — and
    /// the [`AGENT_MIN_INTERVAL`] floor allows it. A read the floor defers
    /// arms a follow-up instead.
    pub(crate) fn agent_zone_wanted(
        &mut self,
        session: u64,
        generation: crate::control::ScreenGen,
        cursor: crate::presence::Cursor,
        program: &Option<String>,
        now: Instant,
    ) -> bool {
        let w = self.agents.entry(session).or_insert_with(|| AgentWatch {
            pgid: -1,
            ..AgentWatch::default()
        });
        let moved = w.seq_seen != Some(generation)
            || w.cursor_seen != Some(cursor)
            || w.program_seen.as_ref() != Some(program);
        if !moved {
            return false;
        }
        let floor = w
            .classified_at
            .is_none_or(|at| now.saturating_duration_since(at) >= AGENT_MIN_INTERVAL);
        if !floor {
            w.followup = true;
        }
        floor
    }

    /// One observation's agent step, after the terminal guard is released:
    /// `rows` is the live zone when [`Self::agent_zone_wanted`] asked for it
    /// (under the same `program`), `cursor` the terminal's cursor read under
    /// the same guard, `pgid` the foreground group, `resolving`
    /// whether a program resolution was just requested for it. Classifies
    /// through [`crate::presence::agent_verdict`] only when the zone's hash
    /// or the program changed.
    #[allow(
        clippy::too_many_arguments,
        reason = "one observation's facts, each read under a different lock or \
                  syscall by the sweep; a struct would only rename the list"
    )]
    pub(crate) fn agent_observe(
        &mut self,
        session: u64,
        generation: crate::control::ScreenGen,
        rows: Option<Vec<String>>,
        cursor: crate::presence::Cursor,
        program: Option<String>,
        pgid: i32,
        resolving: bool,
        now: Instant,
    ) -> AgentStep {
        let w = self.agents.entry(session).or_insert_with(|| AgentWatch {
            pgid: -1,
            ..AgentWatch::default()
        });
        let group_moved = w.pgid != pgid;
        // THE GROUP MOVED since the last look (from one this watch saw): the
        // `program` — read from the timeline under the terminal guard, before
        // this sweep's `note_foreground_group` forgot it — names the group
        // that LEFT. The zone is judged under no name (a group still being
        // named identifies nothing), never under the departed agent's: its
        // last frame, read by Claude Code's reader, published the departed
        // agent's reader again, and the in-GUI host went on seeing an agent
        // that had exited until a later look (the relaunch missed exits that
        // way). A first look (`-1`: no group seen yet) takes the name as read.
        let program = if w.pgid > 0 && group_moved {
            None
        } else {
            program
        };
        if group_moved {
            w.pgid = pgid;
            w.deferred_name_recheck_at = None;
            w.name_confirmed = false;
            if w.frame.is_some_and(|(g, _)| g != pgid) {
                w.frame = None;
            }
        }
        let program_moved = w.program_seen.as_ref() != Some(&program);
        let screen_moved = w.seq_seen != Some(generation) || w.cursor_seen != Some(cursor);
        if let Some(name) = program.as_deref() {
            // A later exec in this SAME group can make the next lookup miss.
            // Once this name is known, that is a fresh retry episode.
            w.resolve_attempts = 0;
            // App can carry the OLD group's program on the group-change
            // sweep. Only a later observation of this group can arm a name
            // confirmation. After one confirmation, a NEW screen movement
            // arms one later recheck even if this frame then stays forever.
            // A moving screen cannot cause process-table polling faster than
            // the four-second floor once the first confirmation has run.
            let recognized_agent = aterm_phase::program_of(name).is_some();
            if pgid > 0
                && !group_moved
                && !resolving
                && w.deferred_name_recheck_at.is_none()
                && !recognized_agent
                && (!w.name_confirmed || screen_moved || program_moved)
            {
                let floor = if w.name_confirmed {
                    PROGRAM_NAMED_RECHECK_FLOOR
                } else {
                    PROGRAM_NAME_CONFIRM
                };
                let since_request = w.resolved_at.map_or(now, |at| at + floor);
                w.deferred_name_recheck_at = Some((now + PROGRAM_NAME_CONFIRM).max(since_request));
            }
            if recognized_agent {
                // Once the name itself identifies Claude/Codex, a remaining
                // non-agent confirmation would only waste a process lookup.
                w.deferred_name_recheck_at = None;
                if !group_moved && !resolving {
                    w.name_confirmed = true;
                }
            }
        } else {
            // A named group's confirmation is irrelevant once the resolver
            // has lost its name. The bounded unknown-name retry owns it now.
            w.deferred_name_recheck_at = None;
            w.name_confirmed = false;
        }
        let Some(rows) = rows else {
            // Not read this time (unchanged, or deferred by the floor): look
            // again next interval if anything is still owed — a moved seq or
            // program, or a resolution in flight whose answer is not yet read.
            w.followup = resolving || program_moved || screen_moved;
            return AgentStep::default();
        };
        let moved = screen_moved;
        w.seq_seen = Some(generation);
        w.cursor_seen = Some(cursor);
        w.followup = moved || resolving;
        // The zone the verdict reads ([`crate::presence::live_zone`]): a box
        // drawn above the last rows of a mostly blank pane moves it, and so
        // does the cursor's row.
        let hash = zone_hash(crate::presence::live_zone(&rows), cursor);
        if w.zone_hash == Some(hash) && !program_moved {
            return AgentStep::default();
        }
        w.zone_hash = Some(hash);
        w.program_seen = Some(program.clone());
        w.classified_at = Some(now);
        let known = w.frame.filter(|(g, _)| *g == pgid).map(|(_, p)| p);
        // A foreground group with no name yet: the resolver is naming it.
        let pending = program.is_none() && pgid > 0;
        // Behind the reader's panic fence: this runs on the window's thread.
        let verdict = crate::presence::agent_verdict_guarded(
            session,
            program.as_deref(),
            pending,
            known,
            &rows,
            cursor,
            now,
        );
        if let crate::presence::AgentVerdict::Agent {
            by_frame: true,
            program,
            ..
        } = &verdict
        {
            w.frame = Some((pgid, *program));
        }
        let reading = verdict.reading().cloned();
        let reading_changed = reading != w.reading;
        if reading_changed {
            self.agent_seq += 1;
            w.reading = reading;
            w.reading_seq = self.agent_seq;
        }
        // The box's command or path, for the menu row and the notification
        // only: read by the same reader, from the same zone, as the verdict.
        let subject = verdict.subject().map(str::to_string);
        AgentStep {
            reading_changed,
            publish: Some((verdict.word(), verdict.detail(), subject, verdict.program())),
        }
    }

    /// Whether `session`'s foreground program should be (re-)named now: its
    /// group just changed (`group_changed`), an unknown name has reached its
    /// bounded retry deadline, a one-shot known-name confirmation is due, or
    /// the screen moved and the last naming is older than [`PROGRAM_RECHECK`]
    /// (an `exec` in place). Call before
    /// [`Self::agent_observe`], which records the generation.
    pub(crate) fn program_due(
        &self,
        session: u64,
        group_changed: bool,
        program: Option<&str>,
        generation: crate::control::ScreenGen,
        now: Instant,
    ) -> bool {
        if group_changed {
            return true;
        }
        self.agents.get(&session).is_some_and(|w| {
            let confirmation_due = program.is_some_and(|p| aterm_phase::program_of(p).is_none())
                && w.pgid > 0
                && w.deferred_name_recheck_at.is_some_and(|at| now >= at);
            confirmation_due
                || w.resolved_at.is_some_and(|at| {
                    let age = now.saturating_duration_since(at);
                    (program.is_none()
                        && w.pgid > 0
                        && age >= unresolved_program_recheck(w.resolve_attempts))
                        || (w.seq_seen != Some(generation) && age >= PROGRAM_RECHECK)
                })
        })
    }

    /// Charge one resolver request and advance its bounded retry episode. The
    /// App path has already created an `AgentWatch` via `agent_zone_wanted`;
    /// `entry` also makes this helper safe for a caller that has not done so.
    fn note_program_request(&mut self, session: u64, pgid: i32, now: Instant) {
        let w = self.agents.entry(session).or_insert_with(|| AgentWatch {
            pgid: -1,
            ..AgentWatch::default()
        });
        if w.pgid != pgid {
            w.resolve_attempts = 0;
            w.deferred_name_recheck_at = None;
            w.name_confirmed = false;
        } else if w.deferred_name_recheck_at.take().is_some() {
            // A confirmation was requested. Its next screen movement may
            // schedule another, but only at the named-program floor.
            w.name_confirmed = true;
        }
        w.resolved_at = Some(now);
        w.resolve_attempts = w.resolve_attempts.saturating_add(1).min(3);
    }

    /// Ask the resolver to name `pgid`'s leader into `timeline`, off-thread.
    /// `shell` is the session's shell pid: a leader that is its direct child
    /// also measures the shell's PATH (`program::leader_facts`).
    pub(crate) fn request_program(
        &mut self,
        session: u64,
        timeline: &std::sync::Arc<std::sync::Mutex<crate::session_timeline::SessionTimeline>>,
        pgid: i32,
        shell: i32,
        now: Instant,
        proxy: Option<&winit::event_loop::EventLoopProxy<crate::Wake>>,
    ) {
        self.note_program_request(session, pgid, now);
        self.programs.request(session, timeline, pgid, shell, proxy);
    }

    pub(crate) fn revision(&self, session: u64) -> u64 {
        self.sessions.get(&session).map_or(0, |slot| slot.revision)
    }

    /// Fold the running-command `detail` onto `session`'s published status (F5),
    /// read by the sweep under the SAME `try_lock` it already holds so the
    /// chrome's `running <detail>` and the wire's `status detail=` cannot
    /// disagree about who lives here. A session with no slot (its first
    /// classification lost the lock) is a no-op — the next sweep that classifies
    /// it carries the detail too. Bumps the revision so chrome recomposes on a
    /// program change even when the phase held; returns whether it moved.
    pub(crate) fn set_detail(&mut self, session: u64, detail: Option<String>) -> bool {
        let Some(slot) = self.sessions.get_mut(&session) else {
            return false;
        };
        if slot.fsm.set_detail(detail) {
            slot.revision = slot.revision.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// Drop a retired session's state. Without this the map would grow for the
    /// process lifetime as tabs open and close.
    pub(crate) fn retire(&mut self, session: u64) {
        self.agents.remove(&session);
        let _ = self.inputs.forget(session);
        self.programs.retire(session);
        crate::claude_footer::stop_session(session);
        self.footer_asked.remove(&session);
        if self.sessions.remove(&session).is_some() {
            // The removed slot may have BEEN the minimum, and a stale-early
            // bound would only cost a scan — but the exact value is one cheap
            // pass over a map that just shrank, and retirement is rare.
            self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
        }
    }

    /// Adopt a new policy at runtime. Returns whether anything actually moved,
    /// so the caller can skip the chrome fan-out on a no-op edit.
    ///
    /// Every LIVE session is rewritten, not just the observer's own field:
    /// [`StatusFsm`] holds its own copy of the policy (taken once at
    /// construction), so updating the observer alone would apply a Settings edit
    /// to sessions opened AFTER it and silently leave every existing tab on the
    /// old numbers. `next_due` is clamped down for the same reason — a shortened
    /// interval that waited out the OLD one would read as "the setting did
    /// nothing".
    pub(crate) fn reconfigure(
        &mut self,
        policy: StatusPolicy,
        min_interval: Duration,
        now: Instant,
    ) -> bool {
        let moved = self.policy.quiet_after != policy.quiet_after
            || self.policy.dwell != policy.dwell
            || self.min_interval != min_interval;
        if !moved {
            return false;
        }
        self.policy = policy;
        self.min_interval = min_interval;
        for slot in self.sessions.values_mut() {
            slot.fsm.policy = policy;
            slot.next_due = slot.next_due.min(now + min_interval);
        }
        // Every deadline just moved DOWN, so the gate's bound must follow it
        // down too — otherwise a shortened interval would still "do nothing"
        // until the old one expired, the exact bug the clamp above fixes.
        self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
        true
    }

    /// Forget every session's classifier state. Used when `tab_status` is turned
    /// OFF: the records describe a subsystem that is no longer running, and
    /// keeping them would let a stale phase sit on a tab forever.
    pub(crate) fn clear(&mut self) -> bool {
        let had = !self.sessions.is_empty();
        self.sessions.clear();
        self.programs.clear_pending();
        self.footer_asked.clear();
        // The agent readings describe the same stopped subsystem: retire them
        // (presence folds the post-clear sequence as "no reading").
        self.agents.clear();
        self.agent_seq += 1;
        self.agent_cleared_at = self.agent_seq;
        // No slots ⇒ no known deadline ⇒ the gate is open again, which is what
        // a re-enabled subsystem needs (every session is unclassified).
        self.next_due_any = None;
        self.unclassified.clear();
        had
    }
}

impl crate::App {
    /// Classify every live session that is due, gathering evidence under
    /// `try_lock` only. Returns the sessions whose published status — or
    /// published agent reading — changed; the caller refreshes each one's
    /// chrome and presence.
    ///
    /// This runs on the output path, so its cost per session is: one map
    /// lookup, and — only when due — one uncontended try-lock holding the
    /// terminal for two field reads plus a block peek. A session that is not due
    /// never touches the lock at all, which is what keeps an output flood from
    /// becoming a classification flood. Contention is a SKIP, never a wait: the
    /// next sweep re-observes, and a missed sample only delays a transition by
    /// the observation interval.
    ///
    /// `tab_status = false` is a REAL off: the sweep returns before touching the
    /// pool, so a user who does not want this subsystem pays no lock attempt, no
    /// `tcgetpgrp`, and no map lookup for it.
    pub(crate) fn observe_session_statuses(&mut self, now: std::time::Instant) -> Vec<u64> {
        if !self.config.tab_status_or_default() {
            return Vec::new();
        }
        // THE O(1) DEADLINE GATE (MPT-4). "Which sessions are past their
        // 250 ms deadline" is a min-over-deadlines question, and it used to be
        // answered by folding the whole pool through a `HashMap` probe per
        // session — at the PTY reader's batch rate, thousands of times a
        // second, to usually find zero. At 120 sessions and a 3000-wake/s
        // flood that is ~360k probes/s of UI-thread time inside exactly the
        // workload the wake-coalescing design exists to survive.
        //
        // Both halves are needed and neither is sufficient: `any_due` covers
        // every session this observer KNOWS, and the pool epoch covers the one
        // it cannot — a brand-new session has no slot, `due()` reports an
        // unknown id as due immediately, and gating on deadlines alone would
        // make a fresh tab wait a whole interval for its first classification.
        //
        // The third term is the newcomer a sweep reached but could not
        // classify, because the PTY reader held its terminal: it still has no
        // slot, so the deadlines cannot speak for it, and it is owed a retry on
        // the very next wake. Those ids are RECORDED at the end of every full
        // sweep (`StatusObserver::note_swept`), so the retry visits just them —
        // it used to keep the pool epoch un-banked instead, which re-walked
        // EVERY session on every output wake for as long as one newcomer's
        // lock stayed contended (a flooding background tab): O(sessions) of
        // UI-thread time between redraw admission and the frame it admitted.
        let pool_epoch = self.pool.insert_epoch();
        let retry_only = pool_epoch == self.session_status.swept_pool_epoch()
            && !self.session_status.any_due(now);
        if retry_only && !self.session_status.has_unclassified() {
            return Vec::new();
        }
        let retry = if retry_only {
            self.session_status.take_unclassified()
        } else {
            Vec::new()
        };
        let pool = &self.pool;
        let sessions: Box<dyn Iterator<Item = &crate::Session> + '_> = if retry_only {
            Box::new(retry.iter().filter_map(|id| pool.get(*id)))
        } else {
            Box::new(pool.iter())
        };
        let mut changed = Vec::new();
        // The sweep never changes pool membership: borrow each due session
        // directly instead of collecting ids, looking them up again, and
        // cloning its terminal Arc on every observation interval.
        for session in sessions {
            #[cfg(test)]
            crate::work_counts::status_probe();
            let id = session.id;
            if !self.session_status.due(id, now) {
                continue;
            }
            let (master, pid) = (session.master, session.pid);
            // Real PTY-output age from the session's existing activity atomic —
            // overwritten for EVERY consumed burst and never cleared by
            // presentation, so a hidden pane's output still ages honestly.
            let latest_output_ns = session
                .latest_output_activity_ns
                .load(std::sync::atomic::Ordering::Relaxed);
            // THE REFUSAL COMES FIRST. Contention is a SKIP, and everything
            // below the lock is evidence this sweep will throw away — so a
            // contended session must cost one atomic try and nothing else. Read
            // the other way round, the `tcgetpgrp` syscall and the O(windows)
            // focus scan were paid per due session on every turn under exactly
            // the output flood that makes the lock contended, inside the
            // workload the O(1) gate exists to survive.
            //
            // A refusal also CHARGES the observation interval
            // (`note_skipped`): without it `next_due` never advances on this
            // path, and `next_wake`'s structural clamp — which floors every
            // armed instant at `next_due` — has nothing to floor against.
            let Ok(guard) = session.term.try_lock() else {
                self.session_status.note_skipped(id, now);
                continue;
            };
            // Two field reads and a block peek, then the lock is released:
            // nothing below it needs the terminal, and the PTY reader must not
            // wait behind a syscall. The running-command `detail=` (F5) is read
            // from this SAME guard so the chrome's `running <detail>` and the
            // wire's `status detail=` share one producer and one lock attempt;
            // a contended engine is skipped above, so reaching here means the
            // read is non-blocking.
            let shell = shell_evidence(&guard);
            let alt_screen = guard.is_alternate_screen();
            let content_seq = guard.content_seq();
            let generation = crate::control::screen_gen(&guard);
            let detail = executing_detail(&guard);
            // THE AGENT VERDICT's one read of the screen, under this SAME
            // guard: the live zone's rows, only when the content moved since
            // the last read (and at most 4 Hz). Classified after the guard is
            // released — the PTY reader never waits on `aterm_phase`.
            // The published program is read here, under the guard (the
            // timeline is a strict leaf: nothing holding it takes a terminal),
            // so the zone is judged under the program it was read with.
            let program = session
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .agent()
                .program
                .clone();
            // The read is the WHOLE screen, once, through the one construction
            // `status hash=` hashes ([`crate::control::screen_text`]): its hash
            // and generation are the verdict's stamp (`agent_gen=`/`agent_fp=`),
            // and its last rows are the zone classified.
            // The terminal's cursor, under this SAME guard as the rows: Claude
            // Code's `idle` is the prompt box that holds it (`presence::Cursor`),
            // and its moving alone is a reason to read the zone again. Its row
            // is the grid's, the rows' own (`visible_row` reads the live rows
            // whatever the scroll position).
            let cursor = crate::presence::Cursor::At(Some(usize::from(guard.cursor().row)));
            let read = self
                .session_status
                .agent_zone_wanted(id, generation, cursor, &program, now)
                .then(|| {
                    // The whole screen: the verdict reads its live zone (the
                    // last CLASSIFY_ROWS of the content, `presence::live_zone`),
                    // and the whole for a box the zone cuts
                    // (`presence::agent_verdict`).
                    let (fp, zone) = screen_agent_frame(&guard, usize::MAX);
                    let stamp = crate::session_timeline::AgentStamp { generation, fp };
                    (zone, stamp)
                });
            drop(guard);
            let (zone, stamp) = match read {
                Some((zone, stamp)) => (Some(zone), Some(stamp)),
                None => (None, None),
            };
            let last_output = match latest_output_ns {
                0 => None,
                ns => self
                    .lat_epoch
                    .checked_add(std::time::Duration::from_nanos(ns)),
            };
            // The keystroke stamp of the window this session is FOCUSED in —
            // the only session a human is typing into. A background tab's
            // sample carries `None`, so its prompt never claims live typing.
            let last_input = self.windows.iter().find_map(|(wid, ws)| {
                (self.focused_session_id(*wid) == Some(id))
                    .then_some(ws.last_key_at)
                    .flatten()
            });
            // Foreground-job PRESENCE only: `tcgetpgrp` gives a Boolean, not a
            // process name (the name is not available on the session path at
            // all), so the classifier is deliberately built to need only this.
            //
            // Routed through `App::job_probe` rather than calling
            // `foreground_is_job` directly. On unix that is the same cheap
            // per-fd `tcgetpgrp` this always was. On WINDOWS there is no
            // `tcgetpgrp`, so the answer comes from a system-wide process-table
            // walk (~3 ms measured) — and calling it here, once per due session
            // per sweep, is what made an idle EIGHT-tab window cost ~4x an idle
            // one-tab window at the same two frames a second: eight walks every
            // 250 ms, ~100 ms of CPU per idle second, none of it visible to the
            // scheduler's deadline counters because it rides the per-turn sweep
            // rather than any armed wake. `JobProbe` keeps the verdict and its
            // freshness rule and removes only the duplication: one capture per
            // sweep for every due session, and one per
            // `quit_safety::JOB_PROBE_MAX_AGE` while no session's evidence moves.
            let foreground_job = (master >= 0 && pid > 0).then(|| {
                self.job_probe.is_job(
                    id,
                    master,
                    pid,
                    crate::quit_safety::JobEvidenceKey {
                        content_seq,
                        alt_screen,
                        output_ns: latest_output_ns,
                        input: last_input,
                    },
                    now,
                )
            });
            let evidence = Evidence {
                pin: None,
                shell,
                // The exit fact arrives as an EVENT (`note_session_exit`) and is
                // held sticky on the slot, so the sweep never has to discover it.
                lifecycle: None,
                foreground_job,
                activity: ActivitySample {
                    alt_screen,
                    content_seq,
                    last_output,
                    last_input,
                },
            };
            // The classifier decides the PHASE; the running command is a
            // separate fact the same guard already read, folded onto the
            // published status the chrome renders. Either moving is a reason to
            // refold the tab, so a program change under a steady `Running`
            // (`running targo test` -> `running claude`) still repaints.
            let status_changed = self.session_status.observe(id, &evidence, now);
            let detail_changed = self.session_status.set_detail(id, detail);
            // PROGRAM IDENTITY + AGENT VERDICT. The foreground group is one
            // `tcgetpgrp` (the job probe above asked the same question); only
            // a CHANGED group costs a resolution, and that runs off this
            // thread. The verdict is published on the session's timeline —
            // the store `status`, `sessions`, `await agent` and the events
            // digest all read — and a move wakes this session's subscribers
            // now rather than on their next 250 ms tick.
            let pgid = if master >= 0 {
                crate::quit_safety::foreground_pgrp(master)
            } else {
                -1
            };
            let previous_group = (pgid > 0)
                .then(|| {
                    let mut timeline = session
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    let previous = timeline.agent().program_pgid;
                    timeline.note_foreground_group(pgid).then_some(previous)
                })
                .flatten();
            let group_changed = previous_group.is_some();
            if let Some(previous) = previous_group {
                crate::claude_footer::stop(id, previous);
            }
            let resolve = pgid > 0
                && self.session_status.program_due(
                    id,
                    group_changed,
                    program.as_deref(),
                    generation,
                    now,
                );
            if resolve {
                self.session_status.request_program(
                    id,
                    &session.ctx.timeline,
                    pgid,
                    pid,
                    now,
                    self.proxy.as_ref(),
                );
            }
            // A LIVE PATH MEASUREMENT the registry's mark does not reflect yet
            // (gap audit 2026-09-24): the mark is LOWERED, so `path=`, the
            // handoff's carry and the managed-current count all read the
            // measured fact — a healed shell stops reading frozen. Never
            // RAISED by a measurement (review of 2026-09-25): the evidence is
            // one job's exec environment, a `PATH=` override reads frozen, and
            // the mark means "adopted from before this update". Owed once per
            // live reading; a leaf lock, then the registry's write lock.
            let lowered = session
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take_path_lowered();
            if lowered {
                self.store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear_frozen_path(id);
            }
            // THE CLAUDE CODE FOOTER: the facts aterm paints over its
            // permission-mode row, re-read at most every RECHECK while it is
            // the program in front (`crate::claude_footer`).
            self.session_status.request_footer_if_due(
                id,
                &session.ctx.timeline,
                if group_changed {
                    None
                } else {
                    program.as_deref()
                },
                pgid,
                now,
            );
            let step = self
                .session_status
                .agent_observe(id, generation, zone, cursor, program, pgid, resolve, now);
            let moved = match (step.publish, stamp) {
                (Some((word, detail, subject, reader)), Some(stamp)) => session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .publish_agent(word, detail, subject, reader, stamp),
                // Read, and the zone was unchanged: the verdict stands on this
                // newer screen.
                (None, Some(stamp)) => {
                    session
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .note_agent_stamp(stamp);
                    false
                }
                _ => false,
            };
            if moved && self.subscribers.any() {
                self.subscribers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .notify(id);
            }
            if status_changed || detail_changed || step.reading_changed {
                changed.push(id);
            }
        }
        if retry_only {
            // A newcomer retry: keep the ones still live and still slot-less.
            let still: Vec<u64> = retry
                .into_iter()
                .filter(|id| self.pool.get(*id).is_some() && !self.session_status.knows(*id))
                .collect();
            self.session_status.restore_unclassified(still);
            return changed;
        }
        // This pass walked the whole pool, so the gate's bound can be made
        // exact. O(slots), once per sweep — and a sweep only happens when
        // something WAS due, i.e. at the classification rate (~4/s at the
        // default interval), never at the burst rate.
        //
        // A `try_lock` that lost to the PTY reader leaves a session
        // unclassified and slot-less, and the scan this gate replaces would
        // have retried it on the very next wake. Its id is recorded, so the
        // gate retries exactly those; see `StatusObserver::note_swept`.
        let unclassified: Vec<u64> = self
            .pool
            .iter()
            .map(|session| session.id)
            .filter(|id| !self.session_status.knows(*id))
            .collect();
        self.session_status.note_swept(pool_epoch, unclassified);
        changed
    }

    /// Record a session's PTY exit and publish `Exited` immediately.
    ///
    /// Called at the TOP of the `Wake::Exit` arm, before anything closes: the
    /// child is still an unreaped zombie at that instant, which is the only
    /// window in which its status can be collected (`aterm_pty` reaps on the
    /// teardown thread and throws the status away). `None` from the collector is
    /// carried through as `Outcome::None` — an adopted session, a master that
    /// went unreadable without the child exiting, and an already-reaped child
    /// are all genuinely unknown, and a fabricated success would be worse than a
    /// blank.
    ///
    /// Whether anyone SEES this depends on `--hold`: without it the tab closes
    /// and the slot is retired moments later. With it the pane survives its
    /// shell, and this is what stops the next sweep — which finds `tcgetpgrp`
    /// failing and reads that as "no foreground job" — from publishing `Idle`
    /// and rendering a dead pane as "ready".
    pub(crate) fn note_session_exit(&mut self, session: u64) {
        if !self.config.tab_status_or_default() {
            return;
        }
        let Some(pooled) = self.pool.get(session) else {
            return;
        };
        // `collect_exit_status` REAPS the zombie, which frees the pid. Latch that
        // on the session so teardown cannot later `killpg` a number the kernel
        // has since reissued — under `--hold` the session outlives this by
        // minutes.
        let collected = aterm_pty::collect_exit_status(pooled.pid);
        if collected.is_some() {
            pooled
                .child_reaped
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let lifecycle = match collected {
            Some(aterm_pty::ChildExit::Code(code)) => Lifecycle::Exited {
                exit_code: Some(code),
            },
            Some(aterm_pty::ChildExit::Signal(signal)) => Lifecycle::Signalled { signal },
            None => Lifecycle::Exited { exit_code: None },
        };
        // The remaining evidence is irrelevant — a lifecycle exit short-circuits
        // classification — so this deliberately takes NO terminal lock on the
        // exit path, where the reader thread has just finished with it.
        let evidence = Evidence {
            pin: None,
            shell: None,
            lifecycle: Some(lifecycle),
            foreground_job: None,
            activity: ActivitySample {
                alt_screen: false,
                content_seq: 0,
                last_output: None,
                last_input: None,
            },
        };
        if self
            .session_status
            .note_exit(session, lifecycle, &evidence, std::time::Instant::now())
        {
            self.refresh_session_status_chrome(session);
        }
    }

    /// Adopt a new `tab_status*` generation. Called from the one config
    /// publication point, after `self.config` is live.
    pub(crate) fn reconfigure_session_status(&mut self) {
        let now = std::time::Instant::now();
        // Turning the master switch OFF must actually retire what is on screen:
        // the published records describe a classifier that is no longer running,
        // and a tab left holding the last phase it saw would be a permanent lie.
        let moved = if self.config.tab_status_or_default() {
            self.session_status.reconfigure(
                self.config.tab_status_policy(),
                self.config.tab_status_observe_interval(),
                now,
            )
        } else {
            // The Claude Code footer rides this sweep (its facts are keyed by
            // the foreground group the sweep keeps current): with the sweep
            // off, forget them, or the next Claude in a tab would be painted
            // with the last one's model and branch (`crate::claude_footer`).
            // Cancel the program resolver before sending footer stops, so an
            // in-flight Claude identification cannot enqueue a watch after
            // this sweep has stopped it.
            let cleared = self.session_status.clear();
            let mut footers = false;
            for session in self.pool.iter() {
                crate::claude_footer::stop_session(session.id);
                footers |= session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear_claude_footer();
            }
            cleared || footers
        };
        // The badge switch changes no POLICY — it only decides whether a record
        // reaches chrome — so `reconfigure` cannot see it move. Track it here or
        // toggling "show status on tabs" leaves every existing badge exactly as
        // it was until some unrelated status transition happens to repaint it.
        let badge = self.config.tab_status_badge_or_default();
        let badge_moved = self.session_status.set_badge(badge);
        if !moved && !badge_moved {
            return;
        }
        // Every window, not just the focused one: the badge is how a background
        // tab reports itself, so a policy change that only settled the front tab
        // would leave the rest showing the old classification.
        let windows: Vec<_> = self.windows.keys().copied().collect();
        for wid in &windows {
            let tabs: Vec<_> = self.windows[wid]
                .tab_set
                .tabs()
                .iter()
                .map(|tab| tab.id)
                .collect();
            for tab_id in tabs {
                self.refresh_tab_status_indicators(*wid, tab_id);
            }
        }
        self.refresh_tab_chrome_windows(windows);
    }

    /// Short status text for this session's chrome, or `None` when there is
    /// nothing honest to say.
    pub(crate) fn session_status_text(&self, session: u64) -> Option<String> {
        self.session_status.status(session).and_then(summary_text)
    }

    pub(crate) fn session_status_revision(&self, session: u64) -> u64 {
        self.session_status.revision(session)
    }

    /// This session's contribution to its tab's indicator bits. A session with
    /// nothing published yet contributes nothing, rather than a guess.
    ///
    /// `tab_status_badge = false` contributes nothing either, WITHOUT stopping
    /// classification: the record stays readable through the `status` verb and
    /// the tooltip, and only the tab chrome goes quiet. (The master
    /// `tab_status` switch is enforced one level up, where it saves the work
    /// rather than discarding it.)
    pub(crate) fn session_status_indicators(
        &self,
        session: u64,
    ) -> crate::tab_model::TabIndicators {
        if !self.config.tab_status_badge_or_default() {
            return crate::tab_model::TabIndicators::default();
        }
        self.session_status
            .status(session)
            .map_or_else(Default::default, status_indicators)
    }

    /// Fan ONE session's published status change out to the chrome that shows
    /// it.
    ///
    /// The two halves are addressed differently on purpose. The label/tooltip
    /// text is composed from the FOCUSED view alone, so it follows `tab.focus`.
    /// The indicator bits are folded across every LEAF, so a background pane of
    /// a split must schedule its window too — surfacing a pane the user cannot
    /// see is the whole point of the aggregate. Both halves are refreshed in one
    /// pass per window: `refresh_window_tabs` takes every tab's terminal lock,
    /// and this runs on the output path.
    pub(crate) fn refresh_session_status_chrome(&mut self, session: u64) {
        // TITLE FOLLOWS THE SETTLED PHASE (frame audit #3): push this publish's
        // idle verdict into the smart-title coordinator BEFORE the chrome below
        // recomposes, so a stale "Typing a command" subject decays to the
        // prompt-state description in the same repaint that carries the phase
        // change — instead of holding the titlebar for minutes after `status`
        // started answering `phase=idle`. Runs at publish (transition) rate; a
        // verdict that moves nothing visible is a cheap flag write.
        //
        // SETTLED is the verdict, never bare `Idle` (review finding on the
        // audit's fix): `Entering` classifies as `Idle`, so a bare phase check
        // decayed the typing subject WHILE the user was typing and it never
        // showed at all. `Status::settled_idle` keeps the verdict false while
        // the keystroke echo is fresh and flips it once the echo has aged out.
        let idle = self
            .session_status
            .status(session)
            .is_some_and(Status::settled_idle);
        let _ = self.title_summaries.note_phase_settled(session, idle);
        let mut windows = self.windows_with_focused_session(session);
        for (wid, tab_id) in self.tabs_viewing_session(session) {
            if self.refresh_tab_status_indicators(wid, tab_id) && !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        self.refresh_tab_chrome_windows(windows);
    }

    pub(crate) fn retire_session_status(&mut self, session: u64) {
        self.session_status.retire(session);
        // The job oracle's per-session evidence describes a pool entry that no
        // longer exists; without this the map would grow one entry per closed
        // tab for the process lifetime.
        self.job_probe.forget(session);
    }

    /// The bridge's roster-round status read. Each row carries the registry's
    /// stable sid and launch nonce beside `status`'s revision, hold, detail and
    /// agent fields, so a
    /// local id reused between two roster reads cannot be mistaken for the
    /// session the bridge sampled earlier. A vanished session contributes no
    /// row; the bridge then retries its old `@sid status` path for that one sid.
    ///
    /// This is a pure read. There is no cursor or batch-owned state: the point
    /// is one event-loop wake for the roster. It deliberately avoids `status`'s
    /// subject, full-screen hash and consent projection: doing those N times
    /// in one event-loop turn would trade N wakes for a long typing stall. A
    /// background timeline writer must not hold this main-thread read either:
    /// the batch marks only its agent field deferred until the next round.
    pub(crate) fn session_statuses_record(&self) -> String {
        let snapshot = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        let mut rows = Vec::with_capacity(snapshot.len());
        for h in snapshot {
            let Some(record) = self.fabric_status_fields(h.local_id) else {
                continue;
            };
            // The registry may change on another thread while the status is
            // read. Never stamp a new local-id occupant with the old key.
            let same_generation = self
                .store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_local(h.local_id)
                .is_some_and(|live| live.sid == h.sid && live.nonce == h.nonce);
            if same_generation {
                rows.push(format!(
                    "{} {} {} {record}",
                    h.local_id,
                    h.sid.as_str(),
                    h.nonce.to_hex()
                ));
            }
        }
        let mut out = format!("OK {}\n", rows.len());
        for row in rows {
            out.push_str(&row);
            out.push('\n');
        }
        out
    }

    /// The fields the bridge extracts from `@sid status`. Keep
    /// their producers and fallback exactly aligned with that record: `hold`
    /// from the session fabric leaf, `detail` from one non-blocking terminal
    /// read or the last observed status, and `revision=0` until classified.
    fn fabric_status_fields(&self, session: u64) -> Option<String> {
        let pooled = self.pool.get(session)?;
        let hold = u8::from(pooled.ctx.fabric.hold().is_some());
        let detail = {
            let term = match pooled.term.try_lock() {
                Ok(t) => Some(t),
                Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            };
            term.as_deref().and_then(executing_detail)
        };
        let status = self.session_status.status(session);
        let (revision, detail) = match status {
            None => (0, detail.as_deref()),
            Some(status) => (
                self.session_status.revision(session),
                detail.as_deref().or(status.detail.as_deref()),
            ),
        };
        let detail = detail.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode);
        let agent = match pooled.ctx.timeline.try_lock() {
            Ok(timeline) => Some(timeline.agent().word),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner().agent().word),
            // The bridge still receives hold/detail now. The explicit marker
            // makes it keep the last published agent verdict; an older bridge
            // instead retries the full per-session status read.
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        Some(match agent {
            Some(word) => format!(
                "sid={session} revision={revision} hold={hold} detail={detail} agent={word}"
            ),
            None => format!(
                "sid={session} revision={revision} hold={hold} detail={detail} agent_deferred=1"
            ),
        })
    }

    /// Project one session's SUBJECT + STATUS onto the `status` verb's reply
    /// body (RFC §8). The caller adds the `OK ` prefix and the newline.
    ///
    /// Runs on the event loop, so the Subject ladder uses the same discipline as
    /// tab titles: the `ctx.meta` LEAF lock first (released before anything
    /// else is taken), then ONE `try_lock` on the terminal that serves both the
    /// subject rungs and the running-command `detail=`, with contention
    /// reported as `subject_source=unavailable` / `detail=-` rather than
    /// silently answered from a lower rung. A driver polling under load must
    /// be able to tell "the title changed" from "I could not look". A pin
    /// answers the SUBJECT without the terminal, but the `try_lock` is still
    /// attempted (never waited on) so a pinned session keeps its `detail=`.
    ///
    /// Deliberately NO `window=` here (nor on `meta`): `status` is what a driver
    /// polls, and window membership lives on the main thread, so answering it
    /// would put an event-loop hop under every poll — a latency regression, not
    /// a feature. `sessions` pays that hop once for the whole roster; `dims`
    /// already names the window. Do not add the hop to this record.
    pub(crate) fn session_status_record(&self, session: u64) -> Result<String, String> {
        let Some(pooled) = self.pool.get(session) else {
            return Err(format!("no such session {session}"));
        };
        let enabled = self.config.tab_status_or_default();
        // FABRIC, additive (§11.2). `hold=` is this session's standing halt, local or fleet
        // — a driver polling `status` learns why its `send` will answer `ERR
        // halted` without a second round trip — and `fabric=` is the INSTANCE's
        // bridge state, which is the other half of the same question: a
        // `disconnected` bridge is itself a held state, because the halt does not
        // depend on that process staying alive. Both are plain flag reads on leaf
        // state, so they cost this polled record nothing. The tail after it —
        // `fabric_rtt_ms=` / `fabric_link_age_ms=` (round 13) — is the bridge's
        // last reported ack to the BROKER and its age: one leaf-mutex read.
        let hold = u8::from(pooled.ctx.fabric.hold().is_some());
        let fabric = crate::fabric::fabric_state();
        let fabric_tail = crate::fabric::fabric_status_tail();
        // IDENTITY (session identities, 2026-09-17), additive and LAST: the
        // agent identity the session was spawned under — the same word the
        // `sessions` roster carries, from the same spawn-time field — `-` for
        // the human's own agent config. A plain field read on the pooled
        // session; this polled record pays nothing for it.
        let identity = pooled
            .identity
            .as_deref()
            .map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode);
        let pin = {
            let meta = pooled.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            meta.presentation_value("title")
        };
        let term = match pooled.term.try_lock() {
            Ok(t) => Some(t),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        // The RAW shell-integration cwd, taken from the SAME guard as the
        // subject rungs: `consent_at_risk` is a lexical test against the
        // protected roots, and it must not re-take a lock this scope already
        // holds (nor answer from a display-abbreviated string).
        let reported_cwd = {
            use crate::cwd_native::ReportedCwd as _;
            term.as_deref().and_then(|t| {
                t.native_working_directory()
                    .map(std::borrow::Cow::into_owned)
            })
        };
        let (subject, subject_source) = match pin {
            Some(title) if !title.is_empty() => (Some(title), "pin"),
            // Rungs 2 and 4 (RFC §4). Rung 5 (shell name) is not resolvable
            // here, so an empty terminal answers `-`/`unavailable` instead of
            // inventing one.
            _ => {
                let rungs = |t: &aterm_core::terminal::Terminal| {
                    use crate::cwd_native::ReportedCwd as _;
                    let osc = t.title().to_string();
                    if osc.is_empty() {
                        (
                            // The cwd rung is user-facing text, so it takes the
                            // native path, not the engine's RFC 8089 URI path.
                            t.native_working_directory()
                                .map(|cwd| crate::app_tabs::home_abbreviated(&cwd)),
                            "cwd",
                        )
                    } else {
                        (Some(osc), "osc")
                    }
                };
                match term.as_deref() {
                    Some(t) => rungs(t),
                    None => (None, "unavailable"),
                }
            }
        };
        // Rung 3 (command-derived), sanitized: what is RUNNING in the session,
        // read from the same guard. `-` when nothing executes or the lock was
        // contended — the roster's `detail=` is the same value from the same
        // producer, so the two verbs can never disagree about who lives here.
        let detail = term.as_deref().and_then(executing_detail);
        // THE SETTLED SCREEN'S STAMP, read under the SAME guard as `detail=` so
        // the two cannot describe different instants. `seq=` is the terminal's
        // `content_seq` and `hash=` is FNV-1a-64 of the UNTRIMMED visible screen
        // — byte for byte the pair `turn` returns and `history` keeps for a turn
        // id, so a report built from this screen can be matched against the
        // ledger rather than believed (a manager's fold of a report). A session
        // whose terminal could not be locked answers `-`/`-`: the poll never
        // waits on the guard for a field it can say it does not have.
        let stamp = term.as_deref().map(crate::control::screen_stamp);
        let (seq, hash) = stamp.map_or_else(
            || ("-".to_string(), "-".to_string()),
            |(seq, hash)| (seq.to_string(), format!("{hash:016x}")),
        );
        // `gen=<epoch>.<seq>`, from the same guard: the screen generation a
        // fenced press names (`key if-gen=`). `seq=` alone repeats after an
        // alternate-screen re-entry; see [`crate::control::ScreenGen`].
        let generation = term.as_deref().map_or_else(
            || "-".to_string(),
            |t| crate::control::screen_gen(t).to_string(),
        );
        // `integration=<on|off|degraded>`, from the same guard: whether this
        // session's OSC 133/633 marks can reach the engine. `degraded` = a
        // nonce is required and none is in use (an adopted shell whose handoff
        // did not carry it, or one whose re-key waits for its next prompt —
        // `shell_rekey`), so `detail=` and blocks are dark — and `program=`
        // below is how its program is still named.
        let integration = term
            .as_deref()
            .map_or("-", |t| t.shell_integration_posture().as_str());
        // …and which integration BODY the shell signs that it runs (2026-09-26,
        // `shell_body`), from the same guard: the revision and the posture the
        // `integration_rev=` word below is read from, with the registry's frozen
        // mark.
        let body = term.as_deref().map(|t| {
            (
                t.shell_integration_rev().map(str::to_owned),
                t.shell_integration_posture(),
            )
        });
        drop(term);
        // `supervisor=<holder|->`: the live supervisor claim (`meta set
        // supervisor`), so a poll shows that something is answering this
        // session's prompts. A leaf lock, taken after the terminal guard.
        let supervisor = pooled
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .live_supervisor(crate::metrics::now_us())
            .map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode);
        // `input= input_bytes= input_wait_ms= fg_rss_mb=` (2026-09-24), after
        // `integration=`: whether the program is READING its input, probed
        // live — the fact a frozen program's unmoving screen cannot show
        // ([`crate::input_stall::status_input`]). `-` off macOS or off a tty.
        // Unread input with no watch looking wakes one.
        let input = crate::input_stall::status_input(
            &pooled.ctx.sink,
            self.session_status.input_stall(session),
            self.session_status.input_armed(session),
        );
        // THE PROGRAM AND THE AGENT VERDICT (`program= agent= agent_detail=
        // agent_rev= agent_since_ms=`): the sweep's publication, read from the
        // session timeline — the same store the `sessions` row, `await agent`
        // and `EVENT agent` read, so no two surfaces disagree.
        let agent = pooled
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .agent()
            .wire_fields();
        // `human_ms=<ms|->`, just after `supervisor=`: how long ago a PERSON
        // last had a hand on this session through a window
        // ([`crate::human_input`]; control verbs never stamp it), `-` for
        // never — what a supervisor keeps its hands off for `[harness]
        // human_grace_s`, and waits on before it keys a dialog a person may
        // be navigating. One atomic load.
        let human_ms = pooled.ctx.human_input.wire(crate::metrics::now_us());
        // THE TWO ADDITIVE FIELDS (design §5.2). Additive: `schema=1` does not
        // move. Computed after the terminal guard is released, from the cwd
        // that guard already produced.
        let consent = self.session_consent(session, reported_cwd.as_deref());
        let opt = |value: Option<&str>| {
            value.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode)
        };
        // THE ROUND-19 TAIL (design §5): `hand= level= story=`, additive after
        // the fabric fields. Read from the presence slot the wakes keep current
        // — plain field reads, no lock, no classifier — so the poll pays nothing
        // it did not already pay.
        let presence = self.presence_status_tail(session);
        // `path=<frozen|live>` (2026-09-22), after `story=` and BEFORE the
        // `seq=`/`hash=` stamp, which stays last by contract: whether this
        // session's shell fronts aterm's managed `agents/` on PATH. `frozen` is
        // the registry's adoption mark ([`crate::session_store::SessionStore::
        // has_frozen_path`]): a shell spawned by a build before the self-healing
        // sessions (2026-09-16) and carried across the update(s) since, so
        // `claude`/`codex` typed in it run the foreign copies until the hook is
        // sourced there. An UPPER BOUND — sourcing the hook is not reported back
        // — the same one the managed-current row's tab count is. One registry
        // read, no lock the poll did not already take elsewhere.
        //
        // `history_lost=<n>` (2026-09-26), from the same registry read, after
        // the owner columns and before the `gen=`/`seq=`/`hash=` stamp: the
        // history lines this session's update handoffs could not carry, over
        // every handoff it crossed (`crate::handoff_history`) — `0` when every
        // line crossed or it never crossed one. The one place the per-tab count
        // is answerable; the band says the update's total once.
        //
        // `integration_rev=<current|stale:<rev>|frozen|->` (2026-09-26), from the
        // same registry read and the terminal guard above, after `history_lost=`
        // and before the stamp: which integration body the shell runs against
        // the one this build ships (`shell_body::status_word`) — `frozen` for a
        // shell adopted with an integration from before loaders, which no body
        // pointer reaches.
        let (mark, history_lost, integration_frozen) = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            (
                store.has_frozen_path(session),
                store.history_lost(session),
                store.is_integration_frozen(session),
            )
        };
        let integration_rev = crate::shell_body::status_word(
            body.as_ref()
                .map(|(rev, posture)| (rev.as_deref(), *posture)),
            integration_frozen,
        );
        // `path=` MEASURED where it can be (2026-09-24): a reading of the
        // shell's exported PATH from its child's environment wins over the
        // carried mark, and `path_evidence=` says which it is; `copy=` and
        // `upgrade=` ride after `supervisor=` ([`SessionTimeline::owner_columns`]).
        let (path, owner) = pooled
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .owner_columns(mark, crate::upgrade_host::now_s());
        let Some(status) = self.session_status.status(session) else {
            // Never classified. Distinct from `phase=unknown`, which IS a
            // classification ("evidence was looked for and none was usable").
            // `detail=` is the engine's fact, not the classifier's, so it is
            // answered here too — a poller must not see it flip from `-` to a
            // command on the first publish of an unchanged block.
            return Ok(format!(
                "schema=1 sid={session} subject={} subject_source={subject_source} observed=false \
                 phase=unknown since_ms=- outcome=none exit_code=- signal=- detail={} \
                 confidence=unknown reasons=- attribution={} fs_consent={} conflict=false \
                 revision=0 enabled={enabled} hold={hold} fabric={fabric} {fabric_tail} \
                 identity={identity} {presence} path={path} {agent} integration={integration} \
                 {input} supervisor={supervisor} human_ms={human_ms} {owner} \
                 history_lost={history_lost} integration_rev={integration_rev} gen={generation} seq={seq} hash={hash}",
                opt(subject.as_deref()),
                opt(detail.as_deref()),
                consent.attribution.as_str(),
                consent.fs_consent.as_str(),
            ));
        };
        // `Status::since` is a raw `Instant`, so the reply carries an AGE rather
        // than a timestamp — there is no epoch a reader could align it against.
        let since_ms = std::time::Instant::now()
            .saturating_duration_since(status.since)
            .as_millis();
        let (exit_code, signal) = match status.last_outcome {
            Outcome::Failure { exit_code } => (exit_code.to_string(), "-".to_string()),
            Outcome::Signal { signal } => ("-".to_string(), signal.to_string()),
            Outcome::None | Outcome::Success => ("-".to_string(), "-".to_string()),
        };
        // `consent_at_risk` rides the reasons list rather than the classifier:
        // the FSM is a pure function of terminal evidence and has no consent
        // input, and this token is a join of two facts it never sees.
        let mut reason_tokens: Vec<&str> = status
            .reasons
            .iter()
            .map(|reason| reason.as_str())
            .collect();
        if consent.at_risk {
            reason_tokens.push(Reason::ConsentPrompt.as_str());
        }
        let reasons = if reason_tokens.is_empty() {
            "-".to_string()
        } else {
            reason_tokens.join(",")
        };
        Ok(format!(
            "schema=1 sid={session} subject={} subject_source={subject_source} observed=true \
             phase={} since_ms={since_ms} outcome={} exit_code={exit_code} signal={signal} \
             detail={} confidence={} reasons={reasons} attribution={} fs_consent={} \
             conflict={} revision={} enabled={enabled} hold={hold} fabric={fabric} {fabric_tail} \
             identity={identity} {presence} path={path} {agent} integration={integration} \
             {input} supervisor={supervisor} human_ms={human_ms} {owner} \
             history_lost={history_lost} integration_rev={integration_rev} gen={generation} seq={seq} hash={hash}",
            opt(subject.as_deref()),
            status.phase.as_str(),
            status.last_outcome.as_str(),
            opt(detail.as_deref().or(status.detail.as_deref())),
            status.confidence.as_str(),
            consent.attribution.as_str(),
            consent.fs_consent.as_str(),
            status.conflict,
            self.session_status.revision(session),
        ))
    }
}

#[cfg(all(test, windows))]
mod idle_cost_tests {
    use std::time::{Duration, Instant};

    /// A pid that certainly owns no child process, so the classifier's verdict
    /// is stable however the test binary's own process tree happens to look.
    const CHILDLESS_PID: i32 = 0x7FFF_FFF0;

    /// Stage `extra` additional pooled sessions that LOOK like real ConPTY
    /// sessions to the status observer: a non-negative master (windows hands
    /// out opaque registry keys from `0x4000_0000`) and a live-looking pid.
    /// Both are what arms the `foreground_job` evidence — the headless
    /// fixture's `master = -1, pid = -1` stubs skip it entirely, which is
    /// exactly why the cost this test pins had never been priced.
    fn stage_pty_like_sessions(app: &mut crate::App, count: u64) {
        for i in 0..count {
            let mut session = crate::stub_session(1000 + i);
            session.master = 0x4000_0000 + i32::try_from(i).expect("small fixture");
            session.pid = CHILDLESS_PID;
            app.pool.insert(session);
        }
    }

    /// Run `secs` of SIMULATED idle event loop: the observer sweep
    /// `about_to_wait` runs on every turn, at the ~4 turns a second an idle
    /// blinking cursor produces. Returns how many system process-table
    /// captures the foreground-job oracle bought.
    fn idle_captures(pty_sessions: u64, secs: u64) -> u64 {
        let mut app = crate::App::headless_for_test();
        stage_pty_like_sessions(&mut app, pty_sessions);
        let t0 = Instant::now();
        let turns = secs * 4;
        for turn in 0..turns {
            let now = t0 + Duration::from_millis(250 * turn);
            let _ = app.observe_session_statuses(now);
        }
        app.job_probe.capture_count()
    }

    /// THE IDLE-WAKE REGRESSION GUARD (bundle `idle-wakes`).
    ///
    /// The tab-status observer asks "is a foreground job running?" once per due
    /// session on every event-loop turn. On unix that is a `tcgetpgrp` on the
    /// session's own fd. On WINDOWS there is no such call, so the answer came
    /// from a system-wide process-table walk — measured at ~3.1 ms on the
    /// machine this was found on — bought once per session per sweep. An idle
    /// window with eight restored tabs was therefore spending roughly a tenth
    /// of a CPU second per idle second discovering that eight idle prompts
    /// were still idle, and none of it showed up in the scheduler's deadline
    /// counters because it rides the per-turn sweep rather than any armed wake.
    ///
    /// What must hold now: the number of captures an IDLE window buys does not
    /// depend on how many tabs it has, and is bounded by the ceiling rather
    /// than by the sweep rate.
    #[test]
    fn an_idle_windows_job_probe_cost_does_not_scale_with_tab_count() {
        const SECS: u64 = 30;
        let ceiling_refreshes = SECS / crate::quit_safety::JOB_PROBE_MAX_AGE.as_secs() + 1;

        let one_tab = idle_captures(1, SECS);
        let eight_tabs = idle_captures(8, SECS);
        println!(
            "IDLE JOB PROBES over {SECS}s: 1 tab = {one_tab} captures, \
             8 tabs = {eight_tabs} captures (ceiling allows {ceiling_refreshes})"
        );

        assert_eq!(
            eight_tabs, one_tab,
            "an idle window's process-table captures must not scale with tab \
             count (8 tabs bought {eight_tabs}, 1 tab bought {one_tab})"
        );
        assert!(
            eight_tabs <= ceiling_refreshes,
            "{SECS}s of idle must buy at most one capture per \
             JOB_PROBE_MAX_AGE ({ceiling_refreshes}), bought {eight_tabs}"
        );
        assert!(
            eight_tabs >= 1,
            "the fixture must actually reach the probe — a staged session whose \
             master/pid never arm `foreground_job` would price an early return"
        );
    }

    /// The other half: the probe still WORKS. A session whose evidence moves
    /// gets a capture taken after the movement, so a real job start is never
    /// answered from a stale process table.
    #[test]
    fn a_session_whose_grid_moves_buys_a_fresh_capture() {
        let mut app = crate::App::headless_for_test();
        stage_pty_like_sessions(&mut app, 3);
        let t0 = Instant::now();
        let _ = app.observe_session_statuses(t0);
        let settled = app.job_probe.capture_count();
        assert!(settled >= 1, "the first sweep captures");

        // A later sweep inside the ceiling with nothing moving: no capture.
        let t1 = t0 + Duration::from_millis(500);
        let _ = app.observe_session_statuses(t1);
        assert_eq!(
            app.job_probe.capture_count(),
            settled,
            "a settled sweep must buy nothing"
        );

        // Now write to one staged session's grid — the `content_seq` movement
        // a starting job produces — and sweep again inside the ceiling.
        let term = app.pool.get(1000).expect("staged session").term.clone();
        term.lock()
            .unwrap_or_else(|p| p.into_inner())
            .process(b"$ cargo build\r\n");
        let t2 = t1 + Duration::from_millis(500);
        let _ = app.observe_session_statuses(t2);
        assert_eq!(
            app.job_probe.capture_count(),
            settled + 1,
            "moved evidence must force exactly one fresh capture"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::session_timeline::SessionTimeline;

    /// `seq=<n|-> hash=<hex16|->`, last on the record and in that order — the
    /// pair a stamped report opens with, and the one
    /// `history` can be held against.
    fn assert_stamp_rides_last(record: &str) {
        let stamp = record
            .rsplit_once(" seq=")
            .expect("the record ends with the stamp")
            .1;
        let (seq, hash) = stamp.split_once(" hash=").expect("seq= then hash=");
        assert!(
            seq == "-" || seq.chars().all(|c| c.is_ascii_digit()),
            "seq={seq:?} in {record}"
        );
        assert!(
            hash == "-" || (hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit())),
            "hash={hash:?} in {record}"
        );
    }
    use super::*;

    const QUIET: Duration = Duration::from_millis(5_000);
    const DWELL: Duration = Duration::from_millis(750);

    fn policy() -> StatusPolicy {
        StatusPolicy {
            quiet_after: QUIET,
            dwell: DWELL,
        }
    }

    #[test]
    fn stale_claude_sample_cannot_restart_a_stopped_footer_watch() {
        let now = Instant::now();
        let mut observer = StatusObserver::new(policy(), Duration::from_millis(50));
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut current = timeline.lock().unwrap();
            current.note_foreground_group(42);
            current.set_program(42, Some("claude".into()));
            current.set_program(42, Some("zsh".into()));
        }
        // The UI sampled Claude before the worker published zsh and sent its
        // stop. A later request from that stale sample would revive the watch.
        observer.request_footer_if_due(7, &timeline, Some("claude"), 42, now);
        assert!(!observer.footer_asked.contains_key(&7));

        {
            let mut current = timeline.lock().unwrap();
            current.note_foreground_group(43);
            current.set_program(43, Some("claude".into()));
        }
        observer.request_footer_if_due(7, &timeline, Some("claude"), 42, now);
        assert!(!observer.footer_asked.contains_key(&7));
    }

    fn blank(seq: u64) -> ActivitySample {
        ActivitySample {
            alt_screen: false,
            content_seq: seq,
            last_input: None,
            last_output: None,
        }
    }

    fn evidence(activity: ActivitySample) -> Evidence {
        Evidence {
            pin: None,
            shell: None,
            lifecycle: None,
            foreground_job: None,
            activity,
        }
    }

    /// Drive a candidate past the dwell gate: observe, wait, observe again.
    fn settle(fsm: &mut StatusFsm, ev: &Evidence, at: Instant) -> Instant {
        fsm.observe(ev, at);
        let later = at + DWELL;
        fsm.observe(ev, later);
        later
    }

    #[test]
    fn shell_prompt_outranks_background_output() {
        // The `tail -f &` case: content is moving, but the shell is at a prompt.
        // Screen movement must NOT promote the session to Running.
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Prompt);

        fsm.observe(&ev, t0);
        ev.activity.content_seq = 2;
        let t1 = settle(&mut fsm, &ev, t0 + Duration::from_millis(10));

        assert_eq!(fsm.status().phase, Phase::Idle);
        assert_eq!(fsm.status().confidence, Confidence::Strong);
        assert!(fsm.status().since <= t1);
    }

    /// The RFC's narrowest rule, restated as a property of the whole phase
    /// vocabulary now that `waiting_input` has been removed for want of an
    /// honest source: NOTHING the classifier can observe locally may ever raise
    /// attention on a pane that has merely gone silent. A pager, a REPL, a
    /// password prompt and a `sleep` are indistinguishable from here.
    #[test]
    fn silence_is_quiet_and_never_asks_for_the_user() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(7));
        ev.foreground_job = Some(true);

        settle(&mut fsm, &ev, t0 + QUIET * 2);

        assert_eq!(fsm.status().phase, Phase::Quiet);
        assert_eq!(fsm.status().confidence, Confidence::Heuristic);
        assert!(
            !status_indicators(fsm.status()).wants_attention(),
            "a silent pane must never claim to be waiting for a human"
        );
    }

    /// The complete phase vocabulary, pinned against its wire spellings. This is
    /// the `status` verb's contract: a phase the classifier cannot produce has
    /// no business being in the enum, and a consumer matching these tokens must
    /// be able to trust the list is exhaustive.
    #[test]
    fn every_phase_is_reachable_and_has_a_wire_spelling() {
        let spellings = [
            (Phase::Unknown, "unknown"),
            (Phase::Starting, "starting"),
            (Phase::Idle, "idle"),
            (Phase::Running, "running"),
            (Phase::Quiet, "quiet"),
            (Phase::Exited, "exited"),
        ];
        for (phase, wire) in spellings {
            assert_eq!(phase.as_str(), wire);
        }

        // Each one, produced by the classifier from real evidence — no phase in
        // the vocabulary is scaffolding.
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        assert_eq!(fsm.status().phase, Phase::Starting, "seeded");

        let mut ev = evidence(blank(1));
        settle(&mut fsm, &ev, t0);
        assert_eq!(fsm.status().phase, Phase::Unknown, "no evidence");

        ev.foreground_job = Some(false);
        let t1 = settle(&mut fsm, &ev, t0 + Duration::from_millis(10));
        assert_eq!(fsm.status().phase, Phase::Idle);

        ev.foreground_job = Some(true);
        ev.activity.content_seq = 2;
        let t2 = settle(&mut fsm, &ev, t1 + Duration::from_millis(10));
        assert_eq!(fsm.status().phase, Phase::Running);

        ev.activity.last_output = None;
        let t3 = settle(&mut fsm, &ev, t2 + QUIET * 2);
        assert_eq!(fsm.status().phase, Phase::Quiet);

        ev.lifecycle = Some(Lifecycle::Exited { exit_code: Some(3) });
        fsm.observe(&ev, t3 + Duration::from_millis(10));
        assert_eq!(fsm.status().phase, Phase::Exited);
        assert_eq!(fsm.status().last_outcome, Outcome::Failure { exit_code: 3 });
    }

    #[test]
    fn alt_screen_transition_is_a_resync_not_activity() {
        // Entering the alternate screen restarts content_gen. A naive
        // comparison would read that jump as a burst of work.
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(ActivitySample {
            alt_screen: false,
            content_seq: 900,
            last_input: None,
            last_output: None,
        });
        ev.foreground_job = Some(true);
        fsm.observe(&ev, t0);

        ev.activity = ActivitySample {
            alt_screen: true,
            content_seq: 3,
            last_input: None,
            last_output: None,
        };
        settle(&mut fsm, &ev, t0 + Duration::from_millis(10));

        assert_eq!(
            fsm.status().phase,
            Phase::Quiet,
            "an alt-screen sequence restart must not count as movement"
        );
    }

    #[test]
    fn completion_keeps_the_outcome_while_the_phase_goes_idle() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });

        settle(&mut fsm, &ev, t0);

        assert_eq!(
            fsm.status().phase,
            Phase::Idle,
            "finished work is not a phase"
        );
        assert_eq!(fsm.status().last_outcome, Outcome::Failure { exit_code: 1 });
        assert!(fsm.status().last_outcome.is_failure());
    }

    #[test]
    fn a_new_unit_of_work_clears_the_previous_outcome() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(2) });
        let t1 = settle(&mut fsm, &ev, t0);
        assert!(fsm.status().last_outcome.is_failure());

        ev.shell = Some(ShellEvidence::Executing);
        ev.activity.content_seq = 2;
        settle(&mut fsm, &ev, t1 + Duration::from_millis(10));

        assert_eq!(fsm.status().phase, Phase::Running);
        assert_eq!(fsm.status().last_outcome, Outcome::None);
    }

    #[test]
    fn dwell_suppresses_flapping_but_exit_publishes_immediately() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(false);
        settle(&mut fsm, &ev, t0);
        assert_eq!(fsm.status().phase, Phase::Idle);

        // A single contrary observation inside the dwell window changes nothing.
        ev.foreground_job = Some(true);
        ev.activity.content_seq = 2;
        assert!(!fsm.observe(&ev, t0 + Duration::from_millis(50)));
        assert_eq!(fsm.status().phase, Phase::Idle);

        // An exact-confidence transition bypasses dwell entirely.
        ev.lifecycle = Some(Lifecycle::Exited { exit_code: Some(0) });
        assert!(fsm.observe(&ev, t0 + Duration::from_millis(60)));
        assert_eq!(fsm.status().phase, Phase::Exited);
        assert_eq!(fsm.status().last_outcome, Outcome::Success);
        assert_eq!(fsm.status().confidence, Confidence::Exact);
    }

    #[test]
    fn no_evidence_reports_unknown_rather_than_guessing() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let ev = evidence(blank(1));

        settle(&mut fsm, &ev, t0);

        assert_eq!(fsm.status().phase, Phase::Unknown);
        assert_eq!(fsm.status().confidence, Confidence::Unknown);
        assert_eq!(fsm.status().reasons, vec![Reason::NoEvidence]);
    }

    /// THE `Executing` ARM NAMES THE CLOCK THAT IS ALIVE
    /// (`docs/EFFECTS-AND-WAKE-FOLLOWUPS-2026-08-24.md` item 21). On a
    /// shell-integrated session, output that mutates nothing must read
    /// `output_activity` — the same `movement_reason` the foreground-job arm
    /// uses — or `ctl status` cannot say whether the movement clock has gone
    /// stale (the in-band witness the spin gate's fixture relies on). RED
    /// before: the arm hardcoded `ContentActivity` whenever anything moved.
    #[test]
    fn executing_names_output_only_movement_as_output_activity() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Executing);
        ev.activity.last_output = Some(t0);
        let t1 = settle(&mut fsm, &ev, t0);
        assert_eq!(fsm.status().phase, Phase::Running);
        assert_eq!(
            fsm.status().reasons,
            vec![Reason::ShellBlock, Reason::OutputActivity],
            "bytes arrived and the grid never moved"
        );

        // Control: the grid moves, and the record says so.
        ev.activity.content_seq = 2;
        ev.activity.last_output = Some(t1);
        settle(&mut fsm, &ev, t1 + Duration::from_millis(10));
        assert_eq!(fsm.status().phase, Phase::Running);
        assert_eq!(
            fsm.status().reasons,
            vec![Reason::ShellBlock, Reason::ContentActivity],
            "a moved grid is content activity"
        );
    }

    #[test]
    fn executing_without_a_foreground_job_is_flagged_as_conflicting() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Executing);
        ev.foreground_job = Some(false);

        settle(&mut fsm, &ev, t0);

        assert!(
            fsm.status().conflict,
            "contradictory evidence must be visible"
        );
    }

    fn published(phase: Phase, last_outcome: Outcome) -> Status {
        Status {
            phase,
            last_outcome,
            since: Instant::now(),
            detail: None,
            confidence: Confidence::Strong,
            reasons: Vec::new(),
            conflict: false,
        }
    }

    #[test]
    fn indicators_map_work_and_failure_without_claiming_unsaved_state() {
        use crate::tab_model::TabIndicators;

        let cases = [
            (Phase::Running, Outcome::None, (true, false)),
            // An exited session with NO collectable status raises nothing: an
            // adopted shell, or a master that went unreadable, is unknown — not
            // a failure worth marking a tab for.
            (Phase::Exited, Outcome::None, (false, false)),
            // A finished failure keeps the tab marked while the shell honestly
            // sits back at a prompt.
            (
                Phase::Idle,
                Outcome::Failure { exit_code: 1 },
                (false, true),
            ),
            (Phase::Exited, Outcome::Signal { signal: 9 }, (false, true)),
            // Running AND failed: the previous unit's result is still the thing
            // worth showing, and neither bit hides the other.
            (
                Phase::Running,
                Outcome::Failure { exit_code: 2 },
                (true, true),
            ),
            (Phase::Idle, Outcome::Success, (false, false)),
            // Quiet is work in flight that has simply printed nothing recently
            // (it is only reachable with a known foreground job), so the dot
            // must STAY lit — darking it mid-link reads as "finished".
            (Phase::Quiet, Outcome::None, (true, false)),
            (Phase::Starting, Outcome::None, (false, false)),
            (Phase::Unknown, Outcome::None, (false, false)),
        ];
        for (phase, outcome, (busy, attention)) in cases {
            assert_eq!(
                status_indicators(&published(phase, outcome)),
                TabIndicators {
                    dirty: false,
                    busy,
                    // A terminal NEVER writes the out-of-band bit: that
                    // separation is what lets the classifier's own bit be
                    // recomputed rather than latched.
                    attention: false,
                    status_attention: attention,
                },
                "{phase:?} with {outcome:?}"
            );
        }
    }

    /// The latch defect: the classifier is edge-driven by PTY output, but the
    /// transition OUT of Running is time-gated. A build that finishes and prints
    /// nothing more would keep its busy dot lit forever unless the observer can
    /// tell the event loop it still owes a wake.
    #[test]
    fn a_finished_pane_owes_a_wake_so_its_busy_dot_can_retire() {
        let t0 = Instant::now();
        let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(true);
        // The first sample only ADOPTS the activity identity (it cannot know
        // whether anything moved), so movement needs a second observation and
        // publication needs a third once dwell is served.
        observer.observe(1, &ev, t0);
        let t1 = t0 + Duration::from_millis(10);
        ev.activity.content_seq = 2;
        observer.observe(1, &ev, t1);
        let t2 = t1 + DWELL;
        ev.activity.content_seq = 3;
        observer.observe(1, &ev, t2);
        assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));
        assert!(
            status_indicators(observer.status(1).expect("published")).busy,
            "a running pane lights the dot"
        );

        // Nothing more will ever be written to this PTY. The event loop must
        // still be told to come back, or the dot never goes out.
        let owed = observer.next_wake().expect("a Running pane owes a wake");
        assert!(owed > t0, "the owed wake is in the future, not a spin");

        // At that wake the pane ages out of Running with no new bytes at all.
        let later = t2 + QUIET * 2;
        observer.observe(1, &ev, later);
        observer.observe(1, &ev, later + DWELL);
        assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Quiet));

        // A pane at rest owes nothing, so an idle machine parks instead of
        // spinning on a deadline it does not need.
        let mut resting = evidence(blank(9));
        resting.foreground_job = Some(false);
        observer.observe(2, &resting, later);
        observer.observe(2, &resting, later + DWELL);
        assert_eq!(observer.status(2).map(|s| s.phase), Some(Phase::Idle));
        assert!(
            !status_indicators(observer.status(2).expect("published")).busy,
            "an idle pane shows no dot"
        );
    }

    #[test]
    fn split_leaves_fold_without_one_pane_hiding_another() {
        use crate::tab_model::{TabIndicators, TabPresentation, ViewId, aggregate_presentations};

        let running = ViewId::from_stored(1);
        let failed = ViewId::from_stored(2);
        let leaf = |title: &str, status: &Status| {
            let mut presentation = TabPresentation::terminal(title);
            presentation.indicators = status_indicators(status);
            presentation
        };
        // The focused pane is the running one; the failure belongs to a sibling
        // the user is not looking at. Folding PHASES instead would have kept one
        // phase only, and the sibling's outcome would have vanished.
        let aggregate = aggregate_presentations(
            running,
            [
                (
                    running,
                    leaf("build", &published(Phase::Running, Outcome::None)),
                ),
                (
                    failed,
                    leaf(
                        "tests",
                        &published(Phase::Idle, Outcome::Failure { exit_code: 1 }),
                    ),
                ),
            ],
        )
        .expect("aggregate");

        assert_eq!(aggregate.title, "build", "focus still supplies the label");
        assert_eq!(
            aggregate.indicators,
            TabIndicators {
                dirty: false,
                busy: true,
                attention: false,
                status_attention: true,
            }
        );
    }

    /// End to end on the real fan-out: a BACKGROUND pane's transition must reach
    /// the tab its window renders. The status path is leaf-addressed precisely
    /// because a focus-addressed one would see nothing here.
    #[test]
    fn a_background_pane_going_busy_marks_its_tab() {
        use crate::tab_model::TabIndicators;

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        // The new pane takes focus, so session 0 becomes the background leaf.
        let focused = app.split_active_stub_tab(wid);
        assert_ne!(focused, 0, "the split spawns a second session");

        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(true);
        ev.activity.last_output = Some(t0);
        assert!(!app.session_status.observe(0, &ev, t0), "dwell holds first");
        let t1 = t0 + DWELL;
        ev.activity.last_output = Some(t1);
        assert!(app.session_status.observe(0, &ev, t1), "Running publishes");

        app.refresh_session_status_chrome(0);

        let tab = app.windows[&wid]
            .tab_set
            .active()
            .expect("active tab")
            .presentation
            .indicators;
        assert_eq!(
            tab,
            TabIndicators {
                dirty: false,
                busy: true,
                attention: false,
                status_attention: false,
            }
        );
        assert!(
            app.tab_strip_metadata(wid).first().expect("one tab").busy,
            "the bits both strip renderers read must carry it"
        );
    }

    /// THE ATTENTION LATCH. `refresh_tab_status_indicators` used to OR the
    /// STORED bit back in to preserve the two out-of-band native writers — and
    /// so ORed back its OWN previous contribution, marking a tab for the life of
    /// the process after one failed command. On a tab mixing terminal and native
    /// leaves the stale terminal bit could then never be cleared.
    #[test]
    fn a_cleared_failure_releases_the_tab_while_a_native_sibling_keeps_its_own_mark() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        // A MIXED tab: the terminal leaf (session 0) plus a native sibling.
        let native = app
            .view_store
            .insert_native(crate::tab_model::AppInstanceId::from_stored(7))
            .expect("native view identity space");
        let split = app
            .windows
            .get_mut(&wid)
            .and_then(|window| window.tab_set.active_mut())
            .is_some_and(|tab| tab.split_focused(crate::tab_model::SplitAxis::Vertical, native));
        assert!(split, "mixed terminal + native tab");
        let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;

        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
        settle_observer(&mut app.session_status, 0, &ev, t0);
        app.refresh_tab_status_indicators(wid, tab_id);
        assert!(
            attention_of(&app, wid, tab_id),
            "a failed command marks the tab"
        );

        // The native sibling raises its own attention OUT OF BAND (a failed
        // document shutdown / an update announcement): not derivable from the
        // runtime presentation, so the status path must never erase it — and
        // note it does so while the classifier's own bit is ALSO set, which one
        // shared bool could not have told apart.
        app.windows
            .get_mut(&wid)
            .and_then(|window| window.tab_set.active_mut())
            .expect("active tab")
            .presentation
            .indicators
            .attention = true;

        // A new unit of work clears the terminal's outcome.
        ev.shell = Some(ShellEvidence::Executing);
        ev.activity.content_seq = 2;
        ev.activity.last_output = Some(t0 + Duration::from_millis(10));
        let t1 = settle_observer(
            &mut app.session_status,
            0,
            &ev,
            t0 + Duration::from_millis(10),
        );
        assert_eq!(
            app.session_status.status(0).map(|s| s.last_outcome),
            Some(Outcome::None),
            "the classifier itself has let go of the failure"
        );
        app.refresh_tab_status_indicators(wid, tab_id);
        assert!(
            attention_of(&app, wid, tab_id),
            "the NATIVE sibling's out-of-band mark survives"
        );

        // Now the native owner clears its own bit. With the latch gone, nothing
        // is left claiming attention.
        app.windows
            .get_mut(&wid)
            .and_then(|window| window.tab_set.active_mut())
            .expect("active tab")
            .presentation
            .indicators
            .attention = false;
        app.refresh_tab_status_indicators(wid, tab_id);
        assert!(
            !attention_of(&app, wid, tab_id),
            "the terminal's own bit must be clearable, not self-sustaining"
        );

        // And the same on a PURE-terminal tab, which the latch broke too.
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(2) });
        let t2 = settle_observer(
            &mut app.session_status,
            0,
            &ev,
            t1 + Duration::from_millis(10),
        );
        app.refresh_tab_status_indicators(wid, tab_id);
        assert!(attention_of(&app, wid, tab_id));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        settle_observer(
            &mut app.session_status,
            0,
            &ev,
            t2 + Duration::from_millis(10),
        );
        app.refresh_tab_status_indicators(wid, tab_id);
        assert!(
            !attention_of(&app, wid, tab_id),
            "a passing command retires the previous failure's mark"
        );
    }

    /// What the CHROME shows: the fold both renderers and the introspection
    /// serializer read, not either owner's field alone.
    fn attention_of(
        app: &crate::App,
        wid: crate::WindowId,
        tab_id: crate::tab_model::TabId,
    ) -> bool {
        app.windows[&wid]
            .tab_set
            .get(tab_id)
            .expect("tab")
            .presentation
            .indicators
            .wants_attention()
    }

    /// [`settle`] for the observer, which owns the dwell clock through its slots.
    fn settle_observer(
        observer: &mut StatusObserver,
        session: u64,
        ev: &Evidence,
        at: Instant,
    ) -> Instant {
        observer.observe(session, ev, at);
        let later = at + DWELL;
        observer.observe(session, ev, later);
        later
    }

    /// A PTY exit publishes `Exited` immediately and STAYS there. Under `--hold`
    /// the pane outlives its shell and keeps being swept; with the child gone
    /// `tcgetpgrp` fails, which reads as "no foreground job" — and would have
    /// published `Idle`, which chrome renders as "ready". A dead pane claiming
    /// to be a shell at a prompt is the exact lie the sticky lifecycle prevents.
    #[test]
    fn an_exited_pane_stays_exited_instead_of_reverting_to_ready() {
        let mut app = crate::App::headless_for_test();
        app.hold = true;
        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(true);
        ev.activity.last_output = Some(t0);
        settle_observer(&mut app.session_status, 0, &ev, t0);
        assert_eq!(
            app.session_status.status(0).map(|s| s.phase),
            Some(Phase::Running)
        );

        // The stub session's pid is not a real child, so the collector honestly
        // answers "unknown" rather than inventing a code.
        app.note_session_exit(0);
        assert_eq!(
            app.session_status.status(0).map(|s| s.phase),
            Some(Phase::Exited),
            "exit is Exact and bypasses dwell"
        );
        assert_eq!(
            app.session_status.status(0).map(|s| s.last_outcome),
            Some(Outcome::None),
            "an uncollectable status is unknown, NEVER success"
        );

        // The `--hold` sweep: the child is gone, so `tcgetpgrp` reports no
        // foreground job. Without the sticky fact this republished `Idle`.
        let mut after = evidence(blank(2));
        after.foreground_job = Some(false);
        let t1 = t0 + DWELL * 4;
        settle_observer(&mut app.session_status, 0, &after, t1);
        assert_eq!(
            app.session_status.status(0).map(|s| s.phase),
            Some(Phase::Exited),
            "a dead pane must not claim to be a shell at a prompt"
        );
        assert_eq!(
            app.session_status_text(0).as_deref(),
            Some("exited"),
            "and chrome must not read 'ready'"
        );
    }

    /// A collected code becomes the session's outcome, and a failure marks the
    /// tab. This is what `Lifecycle` is FOR — the enum stopped being scaffolding
    /// when the exit edge started feeding it.
    #[test]
    fn a_collected_exit_code_becomes_the_outcome() {
        let t0 = Instant::now();
        let cases = [
            (
                Lifecycle::Exited { exit_code: Some(0) },
                Outcome::Success,
                false,
            ),
            (
                Lifecycle::Exited { exit_code: Some(7) },
                Outcome::Failure { exit_code: 7 },
                true,
            ),
            (Lifecycle::Exited { exit_code: None }, Outcome::None, false),
            (
                Lifecycle::Signalled { signal: 9 },
                Outcome::Signal { signal: 9 },
                true,
            ),
        ];
        for (lifecycle, outcome, attention) in cases {
            let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
            let ev = evidence(blank(1));
            assert!(observer.note_exit(1, lifecycle, &ev, t0), "{lifecycle:?}");
            let status = observer.status(1).expect("published");
            assert_eq!(status.phase, Phase::Exited, "{lifecycle:?}");
            assert_eq!(status.last_outcome, outcome, "{lifecycle:?}");
            assert_eq!(status.confidence, Confidence::Exact, "{lifecycle:?}");
            assert_eq!(status.reasons, vec![Reason::LifecycleExit], "{lifecycle:?}");
            assert_eq!(
                status_indicators(status).status_attention,
                attention,
                "{lifecycle:?}"
            );
        }
    }

    /// A live policy edit must reach sessions that ALREADY exist. The FSM holds
    /// its own copy of the policy, so updating only the observer would apply a
    /// Settings change to future sessions and silently leave every open tab on
    /// the old numbers.
    #[test]
    fn a_policy_edit_reaches_live_sessions_and_reopens_the_deadline() {
        let t0 = Instant::now();
        let mut observer = StatusObserver::new(policy(), Duration::from_millis(250));
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(true);
        settle_observer(&mut observer, 1, &ev, t0);

        // A candidate serving its dwell owes a wake at first_seen + dwell.
        ev.foreground_job = Some(false);
        let t1 = t0 + DWELL;
        observer.observe(1, &ev, t1);
        let before = observer
            .next_wake()
            .expect("a pending candidate owes a wake");
        assert_eq!(before, t1 + DWELL);

        assert!(
            observer.reconfigure(
                StatusPolicy {
                    quiet_after: QUIET,
                    dwell: Duration::from_millis(10),
                },
                Duration::from_millis(10),
                t1,
            ),
            "a real change reports that it moved"
        );
        let after = observer.next_wake().expect("still owed");
        assert_eq!(
            after,
            t1 + Duration::from_millis(10),
            "the LIVE session's dwell moved, not just the observer's field"
        );
        assert!(
            observer.due(1, t1 + Duration::from_millis(10)),
            "the shortened interval takes effect now, not after one old interval"
        );
        assert!(
            !observer.reconfigure(
                StatusPolicy {
                    quiet_after: QUIET,
                    dwell: Duration::from_millis(10),
                },
                Duration::from_millis(10),
                t1,
            ),
            "a no-op edit costs no fan-out"
        );
    }

    /// `tab_status = false` is a REAL off: no classification at all, and the
    /// records already on screen are retired rather than frozen.
    #[test]
    fn turning_tab_status_off_stops_classifying_and_retires_the_records() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;
        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
        settle_observer(&mut app.session_status, 0, &ev, t0);
        app.refresh_tab_status_indicators(wid, tab_id);
        assert!(attention_of(&app, wid, tab_id));

        app.config.tab_status = Some(false);
        app.reconfigure_session_status();
        assert!(
            app.session_status.status(0).is_none(),
            "a stale phase must not sit on a tab after the classifier stops"
        );
        assert!(
            !attention_of(&app, wid, tab_id),
            "and the chrome settles in the same pass"
        );
        assert!(
            app.observe_session_statuses(t0 + DWELL * 4).is_empty(),
            "the sweep returns before it touches the pool"
        );
        app.note_session_exit(0);
        assert!(
            app.session_status.status(0).is_none(),
            "not even the exit edge classifies while the subsystem is off"
        );
    }

    /// `tab_status_badge = false` keeps the record and only quiets the chrome —
    /// the two switches are independent on purpose.
    #[test]
    fn the_badge_switch_quiets_chrome_without_stopping_classification() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;
        app.config.tab_status_badge = Some(false);

        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
        settle_observer(&mut app.session_status, 0, &ev, t0);
        app.refresh_tab_status_indicators(wid, tab_id);

        assert!(!attention_of(&app, wid, tab_id), "no mark on the tab");
        assert_eq!(
            app.session_status.status(0).map(|s| s.last_outcome),
            Some(Outcome::Failure { exit_code: 1 }),
            "the record is still classified and still readable"
        );
        assert_eq!(
            app.session_status_text(0).as_deref(),
            Some("failed (exit 1)")
        );
    }

    /// THE INPUT FIELDS (2026-09-24): `input= input_bytes= input_wait_ms=
    /// fg_rss_mb=` ride BOTH arms of the record, right after `integration=` and
    /// right before `supervisor=`, and the stamp still rides last. A session
    /// with no tty (every headless stub) has no reading: all four are `-`.
    #[test]
    fn the_status_record_carries_the_input_fields_before_supervisor() {
        let mut app = crate::App::headless_for_test();
        let fields = " input=- input_bytes=- input_wait_ms=- fg_rss_mb=- supervisor=";
        let unobserved = app.session_status_record(0).expect("live session");
        assert!(unobserved.contains(" observed=false "), "{unobserved}");
        let observed = {
            let mut ev = evidence(blank(1));
            ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
            settle_observer(&mut app.session_status, 0, &ev, Instant::now());
            app.session_status_record(0).expect("live session")
        };
        assert!(observed.contains(" observed=true "), "{observed}");
        for record in [unobserved, observed] {
            assert!(record.contains(fields), "{record}");
            let integration = record.find(" integration=").expect("integration=");
            let input = record.find(" input=").expect("input=");
            assert!(
                !record[integration + 1..input].contains(' '),
                "input= follows integration=: {record}"
            );
            assert_stamp_rides_last(&record);
        }
    }

    /// The two ADDITIVE fabric fields (§11.2). They answer the question a driver
    /// asks right after `ERR halted`: is this session held, and is there a bridge
    /// alive to lift it? Both arrive on BOTH arms of the record — the unobserved
    /// arm too, because a session nobody has classified yet can still be halted —
    /// and neither bumps `schema=`, which is what "additive" means here.
    #[test]
    fn the_status_record_carries_hold_and_fabric_inbox_hold() {
        crate::fabric::with_link_reset(the_status_record_carries_hold_and_fabric_body);
    }

    fn the_status_record_carries_hold_and_fabric_body() {
        let mut app = crate::App::headless_for_test();
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" observed=false "), "the unobserved arm");
        assert!(record.contains(" hold=0 "), "{record}");
        assert!(record.contains(" fabric=absent "), "{record}");
        // The round-13 tail: no bridge, so no ack and no age — then `identity=`
        // (session identities) — and after it the round-19 tail: no wake yet, so
        // no hand, a quiet level and no story.
        assert!(
            record.contains(" fabric=absent fabric_rtt_ms=- fabric_link_age_ms=- identity=- "),
            "{record}"
        );
        // Round 22 appended the SCREEN'S STAMP after the round-19 tail, the same
        // additive way, so the tail is no longer the end of the record.
        assert!(
            record.contains(" hand=- level=quiet story=0 why=- path=live program="),
            "{record}"
        );
        // The person's stamp (2026-09-25): no person has keyed session 0,
        // and the field rides after `supervisor=`; the owner's columns
        // follow it, then the history this session's handoffs lost (none:
        // it never crossed one, 2026-09-26), the integration its shell runs
        // (none signed: `-`, 2026-09-26), then the screen's stamp.
        assert!(
            record.contains(
                " supervisor=- human_ms=- path_evidence=- copy=- upgrade=- history_lost=0 integration_rev=- gen="
            ),
            "{record}"
        );
        assert_stamp_rides_last(&record);
        assert!(
            record.starts_with("schema=1 "),
            "additive, not a new schema"
        );

        // Held: the flag flips on the OBSERVED arm too, from the same leaf read.
        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        settle_observer(&mut app.session_status, 0, &ev, t0);
        let ctx = app.pool.get(0).expect("live session").ctx.clone();
        crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "main%20broken".to_string(),
                origin: "fleet".to_string(),
            }),
        );
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" observed=true "), "{record}");
        assert!(record.contains(" hold=1 "), "{record}");
        assert!(!record.contains('\n'), "one line, always");
    }

    #[test]
    fn fabric_batch_matches_single_status_fields_for_each_session() {
        let mut app = crate::App::headless_for_test();
        let second = crate::stub_session(1);
        crate::App::register_session(&app.store, &second, None);
        app.pool.insert(second);

        let assert_same = |app: &crate::App| {
            let batch = app.session_statuses_record();
            let mut lines = batch.lines();
            assert_eq!(lines.next(), Some("OK 2"), "{batch}");
            for local in [0, 1] {
                let h = app
                    .store
                    .read()
                    .unwrap()
                    .by_local(local)
                    .expect("registered session")
                    .clone();
                let row = lines.next().expect("one row per live session");
                let prefix = format!("{local} {} {} ", h.sid.as_str(), h.nonce.to_hex());
                let fields = row.strip_prefix(&prefix).expect("stable sid and nonce");
                let single = app.session_status_record(local).expect("live session");
                for key in ["sid=", "revision=", "hold=", "detail=", "agent="] {
                    let batch_value = fields
                        .split_whitespace()
                        .find(|part| part.starts_with(key))
                        .expect("batch field present");
                    let single_value = single
                        .split_whitespace()
                        .find(|part| part.starts_with(key))
                        .expect("single field present");
                    assert_eq!(batch_value, single_value, "{key}: {row}");
                }
            }
            assert!(lines.next().is_none(), "framed count is exact: {batch}");
        };

        // Both sessions start unclassified. An unavailable terminal must not
        // invent an observed detail or revision for either one.
        let locked_term = app.pool.get(0).unwrap().term.clone();
        let guard = locked_term.lock().unwrap();
        assert_same(&app);
        drop(guard);

        // One session becomes classified and held; its cached detail supplies
        // the same percent-encoded fallback when its terminal is contended.
        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        settle_observer(&mut app.session_status, 0, &ev, t0);
        assert!(
            app.session_status
                .set_detail(0, Some("Claude Code".to_string()))
        );
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "test".to_string(),
                origin: "fleet".to_string(),
            }),
        );
        let guard = locked_term.lock().unwrap();
        assert_same(&app);
        assert!(
            app.fabric_status_fields(0)
                .unwrap()
                .contains("detail=Claude%20Code")
        );
        drop(guard);
    }

    #[test]
    fn fabric_batch_does_not_wait_for_a_background_timeline_writer() {
        use std::sync::mpsc;

        let app = crate::App::headless_for_test();
        let timeline = app.pool.get(0).expect("session").ctx.timeline.clone();
        {
            let mut guard = timeline.lock().unwrap();
            guard.publish_agent(
                "prompt",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        }
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _guard = timeline.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_millis(750));
        });
        held_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let start = Instant::now();
        let batch = app.session_statuses_record();
        let elapsed = start.elapsed();
        // On the old blocking path the writer's timeout releases the lock
        // first; still report the elapsed-time failure rather than a send error.
        let _ = release_tx.send(());
        worker.join().unwrap();
        assert!(
            elapsed < Duration::from_millis(200),
            "a Fabric roster read held the main thread for {elapsed:?}"
        );
        assert!(batch.contains("agent_deferred=1"), "{batch}");
        assert!(
            !batch.contains("agent=-"),
            "a busy writer is not no agent: {batch}"
        );

        let settled = app.session_statuses_record();
        assert!(settled.contains("agent=prompt"), "{settled}");
    }

    /// IDENTITY (session identities, phase 1): the record carries the agent
    /// identity the session was spawned under — the word the `sessions`
    /// roster carries, from the same spawn-time field — `-` for the human's
    /// own agent config. It rides BOTH arms after `fabric_link_age_ms=`,
    /// additive, `schema=1` unmoved; round 19's presence tail (`hand=`,
    /// `level=`, `story=`) was appended after it, the same way.
    #[test]
    fn the_status_record_carries_the_identity_after_the_fabric_tail() {
        crate::fabric::with_link_reset(
            the_status_record_carries_the_identity_after_the_fabric_tail_body,
        );
    }

    fn the_status_record_carries_the_identity_after_the_fabric_tail_body() {
        let mut app = crate::App::headless_for_test();
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" observed=false "), "the unobserved arm");
        assert!(
            record.contains(" fabric_link_age_ms=- identity=- hand="),
            "{record}"
        );

        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .identity = Some("worker".to_string());
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" fabric_link_age_ms=- identity=worker hand="),
            "{record}"
        );
        assert!(
            record.starts_with("schema=1 "),
            "additive, not a new schema"
        );

        // The OBSERVED arm carries it too, from the same field.
        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        settle_observer(&mut app.session_status, 0, &ev, t0);
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" observed=true "), "{record}");
        assert!(
            record.contains(" fabric_link_age_ms=- identity=worker hand="),
            "{record}"
        );
        assert!(!record.contains('\n'), "one line, always");
    }

    /// The `status` verb's reply, field by field. `schema=` leads so a consumer
    /// can reject an unknown MAJOR from the first token.
    #[test]
    fn the_status_record_is_versioned_and_separates_unobserved_from_unknown() {
        // The tail it pins is `fabric=`, which is process-global instance state.
        crate::fabric::with_link_reset(
            the_status_record_is_versioned_and_separates_unobserved_from_unknown_body,
        );
    }

    fn the_status_record_is_versioned_and_separates_unobserved_from_unknown_body() {
        let mut app = crate::App::headless_for_test();

        // Never classified. This is NOT `phase=unknown`, which means the
        // classifier looked and found no usable evidence.
        let record = app.session_status_record(0).expect("live session");
        assert!(record.starts_with("schema=1 sid=0 "), "{record}");
        assert!(record.contains(" observed=false "), "{record}");
        assert!(record.contains(" phase=unknown "), "{record}");
        assert!(record.contains(" revision=0 "), "{record}");
        assert!(record.contains(" enabled=true"), "{record}");

        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(3) });
        settle_observer(&mut app.session_status, 0, &ev, t0);
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" observed=true "), "{record}");
        assert!(record.contains(" phase=idle "), "{record}");
        assert!(
            record.contains(" outcome=failure exit_code=3 signal=- "),
            "{record}"
        );
        assert!(record.contains(" confidence=strong "), "{record}");
        assert!(record.contains(" reasons=shell_block "), "{record}");
        assert!(record.contains(" conflict=false "), "{record}");
        assert!(
            !record.contains('\n'),
            "the record must stay ONE line: {record}"
        );

        // `tab_status = false` is disclosed rather than left to be inferred from
        // a wall of `unknown`.
        app.config.tab_status = Some(false);
        let record = app.session_status_record(0).expect("live session");
        // `enabled=` was the LAST field when this was written, and the assertion
        // said so with `ends_with`. Four ADDITIVE fabric fields now follow it
        // (`hold=`, `fabric=`, `fabric_rtt_ms=`, `fabric_link_age_ms=`), and
        // `identity=` (session identities) after those, and round 19's
        // `hand=`/`level=`/`story=` after that, which is exactly what the
        // record's own help promises a consumer — fields are appended and never
        // bump `schema=`. So pin the VALUE, and let the tail keep growing.
        assert!(record.contains(" enabled=false "), "{record}");
        assert!(
            record.contains(" fabric=absent fabric_rtt_ms=- fabric_link_age_ms=- identity=- "),
            "{record}"
        );
        // Round 19 appended three more (`hand=`, `level=`, `story=`), the same way.
        // Round 22 appended the SCREEN'S STAMP after the round-19 tail, the same
        // additive way, so the tail is no longer the end of the record. `path=`
        // (2026-09-22) went in between, the same way again — the stamp keeps
        // its contract (last), the tail keeps its order. 2026-09-23: `why=`
        // closes the round-19 tail, and the published `program= agent= …
        // integration=` columns sit between `path=` and the stamp.
        assert!(
            record.contains(" hand=- level=quiet story=0 why=- path=live program="),
            "{record}"
        );
        assert_stamp_rides_last(&record);

        assert!(
            app.session_status_record(9999).is_err(),
            "an unknown session is an error, not a blank record"
        );
    }

    /// THE TWO ADDITIVE FIELDS (design §5.2). They ride BOTH branches of the
    /// record — the never-classified one as well as the classified one — so a
    /// poller can never see a field appear from nowhere, and `schema=1` does
    /// not move because nothing was renamed or removed.
    ///
    /// `covered` is deliberately unreachable today: it needs Full Disk Access
    /// AND `attribution=live` AND a service §7 S4 proved covered, and S4 has
    /// not been run. `unknown` here is the honest answer, not a stub.
    #[test]
    fn the_status_record_carries_the_two_additive_consent_fields() {
        let mut app = crate::App::headless_for_test();

        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" observed=false "), "{record}");
        assert!(
            record.contains(" reasons=- attribution=live fs_consent=unknown conflict=false "),
            "the fields sit between `reasons=` and `conflict=`: {record}"
        );
        assert!(
            record.contains(" schema=1 ") || record.starts_with("schema=1 "),
            "{record}"
        );

        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        settle_observer(&mut app.session_status, 0, &ev, t0);
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" observed=true "), "{record}");
        assert!(
            record.contains(" reasons=shell_block attribution=live fs_consent=unknown conflict="),
            "{record}"
        );
        assert!(
            !record.contains("fs_consent=covered"),
            "`covered` needs a measurement nobody has taken: {record}"
        );
        assert!(!record.contains('\n'), "still ONE line: {record}");

        // An ADOPTED session says so, and still refuses to assert the TCC
        // consequence (§3.9 pt 3).
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .handoff_local_id = Some(11);
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" attribution=adopted fs_consent=unknown "),
            "{record}"
        );
    }

    /// `consent_at_risk` is emitted on the CONJUNCTION and on nothing else: it
    /// is silent while the cwd is unknown (every session without shell
    /// integration), and it appears the moment a reported cwd lands under a
    /// protected root. It is not a claim that a dialog is showing.
    #[test]
    fn consent_at_risk_rides_the_reasons_token_only_on_the_conjunction() {
        assert_eq!(Reason::ConsentPrompt.as_str(), "consent_at_risk");

        let mut app = crate::App::headless_for_test();
        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        settle_observer(&mut app.session_status, 0, &ev, t0);

        let record = app.session_status_record(0).expect("live session");
        assert!(
            !record.contains("consent_at_risk"),
            "no cwd reported, so no token: {record}"
        );

        // The conjunction half is Unix-only: the OSC 7 fed below is
        // `file://localhost<cwd>` with a `/`-rooted cwd, and on Windows a
        // protected root is `C:\Users\…` — how the Windows integration spells
        // OSC 7 is a separate question this fixture cannot ask.
        #[cfg(unix)]
        {
            // The protected roots are RESOLVED DATA from the consent module —
            // this file writes no protected-folder literal of its own (rule B13).
            let roots = aterm_containment::consent::protected_roots(&[]);
            let Some(root) = roots.first() else {
                return; // no $HOME resolved: nothing to assert against
            };
            let cwd = root.join("aterm-consent-proof");
            {
                let term = app.pool.get(0).expect("session 0").term.clone();
                let mut t = crate::term_lock(&term);
                t.process(format!("\x1b]7;file://localhost{}\x07", cwd.display()).as_bytes());
            }
            let record = app.session_status_record(0).expect("live session");
            assert!(
                record.contains(",consent_at_risk "),
                "a protected cwd with an uncovered fs_consent arms the token: {record}"
            );
            assert!(
                record.contains(" fs_consent=unknown "),
                "and the token never upgrades the verdict it rides on: {record}"
            );
        }
    }

    /// The Subject ladder under contention. A pin is answered from the LEAF lock
    /// and never touches the terminal; a contended terminal reports
    /// `unavailable` rather than silently falling to a lower rung, which a
    /// poller would otherwise read as a real title change.
    #[test]
    fn the_subject_ladder_never_blocks_and_never_fakes_a_lower_rung() {
        let app = crate::App::headless_for_test();
        let term = app.pool.get(0).expect("session 0").term.clone();

        let held = term.lock().unwrap_or_else(|p| p.into_inner());
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" subject=- subject_source=unavailable "),
            "a contended terminal is unavailable, not a lower rung: {record}"
        );

        // The pin outranks everything and is read from the metadata LEAF, so it
        // answers with the terminal mutex still held by someone else.
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set("title", Some("deploy".to_string()))
            .expect("a short title is within cap");
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" subject=deploy subject_source=pin "),
            "the pin must not need the terminal lock: {record}"
        );
        drop(held);
    }

    /// LIVE TYPING (review follow-up to frame audit #3). The audit's decay
    /// classified `Entering` as plain `Idle` unconditionally, so the settled
    /// verdict engaged WHILE the user was typing and the typing subject never
    /// showed. Typing must publish the live marker at once: prompt entry with
    /// fresh keystroke echo is the same `Idle` phase on the wire, but NOT
    /// settled — and because the phase does not change, the marker rides a
    /// same-phase reasons update and never waits out a dwell.
    #[test]
    fn typing_at_the_prompt_publishes_live_without_a_dwell() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Prompt);
        settle(&mut fsm, &ev, t0);
        assert_eq!(fsm.status().phase, Phase::Idle);
        assert!(fsm.status().settled_idle(), "a bare prompt is settled idle");

        // The first keystroke: block Entering, the echo advances the counter,
        // and the KEY ITSELF is evidence — movement alone is a background job.
        ev.shell = Some(ShellEvidence::Entering);
        ev.activity.content_seq = 2;
        let t1 = t0 + DWELL + Duration::from_millis(10);
        ev.activity.last_input = Some(t1);
        assert!(
            fsm.observe(&ev, t1),
            "the live marker publishes immediately"
        );
        assert_eq!(
            fsm.status().phase,
            Phase::Idle,
            "typing is still idle on the wire — RFC §6 permits no stronger claim"
        );
        assert!(
            !fsm.status().settled_idle(),
            "…but NOT settled, so the typing subject may show while keys land"
        );
        assert_eq!(
            fsm.status().reasons,
            vec![Reason::ShellBlock, Reason::ContentActivity],
            "the record says WHY: prompt entry plus live echo"
        );
    }

    /// THE REVIEW'S REGRESSION, pinned: `EnteringCommand` is the steady state
    /// whenever a prompt is DISPLAYED, so a background job printing at an idle
    /// prompt (`tail -f &`, a sibling build) moves the grid without anyone
    /// typing. Movement alone re-stuck the "Typing a command" subject on a
    /// window nobody had touched for minutes; the marker needs a keystroke.
    #[test]
    fn background_output_at_a_prompt_is_not_live_typing() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Entering);
        settle(&mut fsm, &ev, t0);
        assert!(
            fsm.status().settled_idle(),
            "a displayed prompt nobody typed at is settled"
        );

        // A background job prints: the counter advances, no key was pressed.
        ev.activity.content_seq = 2;
        let t1 = t0 + DWELL + Duration::from_millis(10);
        fsm.observe(&ev, t1);
        assert_eq!(fsm.status().phase, Phase::Idle);
        assert!(
            fsm.status().settled_idle(),
            "ambient output must not claim live typing"
        );
        assert_eq!(
            fsm.status().reasons,
            vec![Reason::ShellBlock],
            "no ContentActivity marker without a keystroke"
        );

        // A STALE keystroke (older than the activity window) is no better.
        ev.activity.content_seq = 3;
        ev.activity.last_input = Some(t1);
        let t2 = t1 + QUIET + Duration::from_millis(1);
        fsm.observe(&ev, t2);
        assert!(
            fsm.status().settled_idle(),
            "a keystroke aged past quiet_after cannot resurrect the subject"
        );
    }

    /// An ABANDONED half-typed prompt: the echo ages out over `quiet_after`,
    /// the observer owes the event loop a wake for a transition no new output
    /// will ever cause, and the record settles to plain `Idle` — the decay of
    /// frame audit #3, with the live state intact this time.
    #[test]
    fn an_abandoned_prompt_settles_and_owes_the_wake_that_retires_it() {
        let t0 = Instant::now();
        let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Entering);
        observer.observe(1, &ev, t0);
        let t1 = t0 + Duration::from_millis(10);
        ev.activity.content_seq = 2;
        ev.activity.last_input = Some(t1);
        observer.observe(1, &ev, t1);
        let t2 = t1 + DWELL;
        ev.activity.content_seq = 3;
        // The LAST keystroke; nothing lands after it, so the stamp ages
        // out exactly like the echo it caused.
        ev.activity.last_input = Some(t2);
        observer.observe(1, &ev, t2);
        let status = observer.status(1).expect("published");
        assert_eq!(status.phase, Phase::Idle);
        assert!(!status.settled_idle(), "typing is live");

        // No more keystrokes will ever land. The event loop must still be
        // told to come back, or the typing subject never decays.
        let owed = observer
            .next_wake()
            .expect("a live typing pane owes a wake");
        assert_eq!(
            owed,
            t2 + QUIET,
            "the settle window is quiet_after past the last echo"
        );

        // At that wake the marker retires with no new bytes at all.
        assert!(observer.observe(1, &ev, owed), "the decay publishes");
        assert!(
            observer.status(1).expect("published").settled_idle(),
            "an abandoned prompt settles to plain Idle"
        );
        // Settled: nothing left owing a wake, so an idle machine parks.
        assert_eq!(observer.next_wake(), None);
    }

    /// A command that is typed, run, and finishes returns to a SETTLED idle at
    /// the Complete mark itself: the typing marker never outlives prompt
    /// entry, so the decay needs no echo window after real work.
    #[test]
    fn a_settled_command_returns_to_settled_idle() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Entering);
        fsm.observe(&ev, t0);
        ev.activity.content_seq = 2;
        let t1 = t0 + DWELL;
        ev.activity.last_input = Some(t1);
        fsm.observe(&ev, t1);
        assert!(!fsm.status().settled_idle(), "typing is live");

        ev.shell = Some(ShellEvidence::Executing);
        ev.activity.content_seq = 3;
        let t2 = settle(&mut fsm, &ev, t1 + Duration::from_millis(10));
        assert_eq!(fsm.status().phase, Phase::Running);

        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        ev.activity.content_seq = 4;
        settle(&mut fsm, &ev, t2 + Duration::from_millis(10));
        assert_eq!(fsm.status().phase, Phase::Idle);
        assert!(
            fsm.status().settled_idle(),
            "Complete is not prompt entry: the marker does not survive it"
        );
        assert_eq!(fsm.status().last_outcome, Outcome::Success);
    }

    /// The verdict seam end to end over the REAL App wiring: while the user is
    /// typing, NEITHER push site — the publish edge
    /// (`refresh_session_status_chrome`) nor the per-observation reconcile
    /// (`note_title_activity`, which runs on the very output wakes typing
    /// produces) — may decay the typing subject; once the echo settles, both
    /// flip it to the prompt description.
    #[test]
    fn the_typing_subject_shows_while_typing_and_decays_once_settled() {
        let mut app = crate::App::headless_for_test();
        let term = app.pool.get(0).expect("session 0").term.clone();
        // OSC 133 A + B: the block is EnteringCommand — prompt entry is open.
        term.lock()
            .unwrap()
            .process(b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\");

        // Classifier: Entering with the echo moving the grid between samples.
        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Entering);
        app.session_status.observe(0, &ev, t0);
        ev.activity.content_seq = 2;
        // The keystroke behind that echo — the marker's evidence.
        ev.activity.last_input = Some(t0 + DWELL);
        assert!(
            app.session_status.observe(0, &ev, t0 + DWELL),
            "the live typing record publishes"
        );
        assert!(
            !app.session_status
                .status(0)
                .expect("published")
                .settled_idle()
        );

        // The coordinator observes the Entering block; the reconcile pushes
        // the LIVE verdict, so the typing subject stays up.
        app.note_title_activity(0);
        assert_eq!(
            app.title_summaries.activity(0, &app.config),
            Some("Typing a command"),
            "the typing subject must show WHILE the user is typing"
        );

        // The publish edge pushes the same live verdict.
        app.refresh_session_status_chrome(0);
        assert_eq!(
            app.title_summaries.activity(0, &app.config),
            Some("Typing a command"),
            "the publish edge must not decay a LIVE typing subject"
        );

        // The echo ages out with no further bytes: the abandoned prompt
        // settles, and the same edge now decays the subject.
        assert!(
            app.session_status.observe(0, &ev, t0 + DWELL + QUIET),
            "the settle publishes"
        );
        assert!(
            app.session_status
                .status(0)
                .expect("published")
                .settled_idle()
        );
        app.refresh_session_status_chrome(0);
        assert_eq!(
            app.title_summaries.activity(0, &app.config),
            Some("Ready"),
            "an abandoned prompt decays to the prompt-state description"
        );
    }

    /// THE CLAUDE-CODE SHAPE (busy-rearm audit, item 1). A TUI that emits
    /// output WITHOUT grid movement indefinitely — sync-bracketed identical
    /// repaints, heartbeats — keeps `Running` alive through `output_recently`
    /// while `last_movement` goes stale past `quiet_after`. Arming the retire
    /// wake off the movement clock alone therefore produced a PAST instant on
    /// every turn: the wake fired ~immediately, the observation gate refused
    /// or observed no change, and the loop re-armed (~200 kHz sustained, 78%
    /// CPU, input p99 335 ms — live evidence). The owed wake must be the
    /// earliest instant `classify` could actually CHANGE the verdict: the
    /// FRESHER of the two clocks plus `quiet_after`.
    #[test]
    fn output_without_movement_arms_the_retire_wake_off_the_freshest_clock() {
        let t0 = Instant::now();
        let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(true);
        ev.activity.last_output = Some(t0);
        observer.observe(1, &ev, t0);
        // One real grid movement, then the grid goes still while output flows.
        let t1 = t0 + DWELL;
        ev.activity.content_seq = 2;
        ev.activity.last_output = Some(t1);
        observer.observe(1, &ev, t1);
        assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));

        // Far past `quiet_after` on the MOVEMENT clock, Running is still
        // honestly sustained by raw output. The movement clock's instant
        // (t1 + QUIET) is long past; the owed wake must ride the output clock.
        let t2 = t1 + QUIET * 2;
        ev.activity.last_output = Some(t2);
        observer.observe(1, &ev, t2);
        assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));
        let owed = observer.next_wake().expect("Running owes its retirement");
        assert_eq!(
            owed,
            t2 + QUIET,
            "the wake is the earliest instant classify could change its verdict"
        );
        assert!(owed > t2, "an owed wake is never already in the past");

        // The user-visible contract is unchanged: at that wake the phase
        // retires with no new bytes at all — output sustains the busy state,
        // true silence ages it out.
        observer.observe(1, &ev, owed);
        observer.observe(1, &ev, owed + DWELL);
        assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Quiet));
    }

    /// The structural invariant (busy-rearm audit, item 2): an armed wake at
    /// which observation is FORBIDDEN is a contradiction — `due()` refuses the
    /// slot until `next_due`, so a deadline before it can only fire, be
    /// refused, and re-arm (the dwell-edge spin: up to `min_interval` of
    /// hot turns per status transition). `next_wake` must clamp every slot's
    /// owed instant to its own observation gate, converting any future bug of
    /// this class to at most the observation rate.
    #[test]
    fn an_armed_wake_never_precedes_its_own_observation_gate() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(250);
        let mut observer = StatusObserver::new(policy(), interval);
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(false);
        observer.observe(1, &ev, t0);

        // Re-observe near the dwell edge: the pending candidate's raw owed
        // instant (t0 + dwell) now precedes the refreshed observation gate.
        let t1 = t0 + DWELL - Duration::from_millis(50);
        observer.observe(1, &ev, t1);
        let gate = observer.sessions.get(&1).expect("slot").next_due;
        assert_eq!(gate, t1 + interval);
        assert!(
            t0 + DWELL < gate,
            "the raw owed wake precedes the gate — the shape under test"
        );
        let owed = observer
            .next_wake()
            .expect("a pending candidate owes a wake");
        assert_eq!(owed, gate, "the armed wake is clamped to the gate");
        assert!(
            observer.due(1, owed),
            "an armed wake at which observation is forbidden is a contradiction"
        );

        // The clamped wake still publishes: dwell was served well before it.
        assert!(observer.observe(1, &ev, owed), "the transition lands");
        assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Idle));
    }

    /// An IMMOVABLE verdict owes no wake. A user pin of `Running` bypasses both
    /// activity clocks, so arming off them produced an instant that receded
    /// further into the past on every turn while nothing could change the
    /// answer — a permanent wake, a pool sweep, a `try_lock` and a `tcgetpgrp`
    /// per session, forever.
    #[test]
    fn a_pinned_running_session_owes_no_time_driven_wake() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);

        // First, the UNPINNED shape, so the assertion below cannot pass just
        // because this fixture never reaches Running.
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(true);
        fsm.observe(&ev, t0);
        ev.activity.content_seq = 2;
        settle(&mut fsm, &ev, t0 + Duration::from_millis(10));
        assert_eq!(fsm.status().phase, Phase::Running);
        assert!(
            fsm.owed_wake().is_some(),
            "an inferred Running owes its retirement"
        );

        // Now pin it. The verdict is `Exact` and clock-free, so no instant
        // exists at which it could change on its own.
        ev.pin = Some(Phase::Running);
        let pinned = t0 + QUIET + Duration::from_secs(30);
        fsm.observe(&ev, pinned);
        assert_eq!(fsm.status().phase, Phase::Running);
        assert!(
            fsm.status().reasons.contains(&Reason::Pin),
            "the record carries the pin"
        );
        assert_eq!(
            fsm.owed_wake(),
            None,
            "a pin sustains Running with no clock, so it arms nothing"
        );
    }

    /// The clamp's floor must ADVANCE on the sweep's other refusal. The sweep
    /// skips a session whose terminal `try_lock` is held by the PTY reader, and
    /// that path never reaches `observe` — so before `note_skipped` the slot's
    /// `next_due` stood still, `next_wake` re-armed the same already-past
    /// instant every turn, and the "at most the observation rate" bound the
    /// clamp advertises did not hold under exactly the sustained-output
    /// workload that contends the lock.
    #[test]
    fn a_refused_sample_still_advances_the_observation_gate() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(250);
        let mut observer = StatusObserver::new(policy(), interval);

        // A Running session whose clocks then go stale: `owed_wake` returns
        // `last_activity + quiet_after`, which recedes into the past.
        let mut ev = evidence(blank(1));
        ev.foreground_job = Some(true);
        observer.observe(1, &ev, t0);
        // Movement is only inferable between two samples, and the candidate then
        // serves its dwell before publishing — so the third observation is what
        // makes `Running` the PUBLISHED phase (`owed_wake` reads the published
        // record, not the candidate).
        let moved_at = t0 + Duration::from_millis(10);
        ev.activity.content_seq = 2;
        observer.observe(1, &ev, moved_at);
        observer.observe(1, &ev, moved_at + DWELL);
        assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));

        // Long past the retirement instant, with the lock contended ever since.
        let now = t0 + QUIET + Duration::from_secs(30);
        assert!(
            observer.next_wake().is_some_and(|wake| wake < now),
            "the shape under test: an owed wake already deep in the past"
        );

        observer.note_skipped(1, now);
        // Every sweep ends in `note_swept`, which restores `next_due_any` to the
        // exact minimum (`note_skipped` only folds a lower bound, exactly as
        // `observe` does). Mirrored here so the O(1) gate is asserted in the
        // state production actually leaves it in.
        observer.note_swept(observer.swept_pool_epoch(), Vec::new());
        let wake = observer.next_wake().expect("the slot still owes a wake");
        assert!(
            wake >= now + interval,
            "a refused sample charges its interval, so the re-arm is in the future"
        );
        assert!(
            !observer.due(1, now),
            "and the gate that refused it is closed for that interval"
        );
        assert!(
            !observer.any_due(now),
            "the O(1) gate agrees, so the sweep itself stops running per turn"
        );
        assert!(
            observer.due(1, wake),
            "the armed wake is admissible when it fires"
        );

        // An unknown session is NOT charged: it holds no slot, so it owes no
        // deadline and cannot spin the loop, and inserting one here would bank
        // the pool epoch over a session that was never classified.
        observer.note_skipped(404, now);
        assert!(
            !observer.knows(404),
            "a refusal never invents a classification"
        );
    }

    /// The RFC §4 privacy rule as a table: the program's basename, an
    /// allow-listed subcommand at most, never an argument — and the wrappers,
    /// assignments and paths an agent's real command lines carry.
    #[test]
    fn command_detail_keeps_the_program_and_an_allowlisted_subcommand_only() {
        let cases: &[(&str, Option<&str>)] = &[
            ("claude --dangerously-skip-permissions", Some("claude")),
            // A prompt glyph a scrape left in front is not the program.
            ("$ sleep 30", Some("sleep")),
            ("❯ codex", Some("codex")),
            ("% ", None),
            ("codex", Some("codex")),
            ("targo --unverified test -p x", Some("targo test")),
            ("cargo build --release", Some("cargo build")),
            ("git status", Some("git status")),
            // `env -S`'s one string: the subcommand is read from its words,
            // from the closed list, and never an argument of it.
            (
                "env -S 'git status --porcelain /secret/repo'",
                Some("git status"),
            ),
            ("env -S 'git /secret/tok'", Some("git")),
            // A flag's VALUE is never a subcommand: the program stands alone.
            // The value may look exactly like a subcommand (a bare identifier);
            // only vocabulary membership tells them apart, and a directory, a
            // namespace, a context or a prefix is the very category the rule
            // keeps off the wire.
            ("git -C /secret/repo status", Some("git")),
            ("git -C aterm status", Some("git")),
            ("kubectl -n prod get pods", Some("kubectl")),
            ("docker --context customer-acme ps", Some("docker")),
            ("npm --prefix web run build", Some("npm")),
            // A word OUTSIDE the vocabulary is an argument even in the
            // subcommand slot.
            ("git customer-acme", Some("git")),
            ("cargo xtask release", Some("cargo")),
            ("ssh me@host", Some("ssh")),
            ("/usr/local/bin/python3 secret.py", Some("python3")),
            ("./run.sh --token abc", Some("run.sh")),
            // Quoting is honoured: a quoted path with a space is ONE word,
            // and a Windows launcher's `.exe` names the same program the bare
            // name does everywhere else (measured 2026-09-22: `Program`).
            (
                "\"C:\\Program Files\\Git\\usr\\bin\\less.exe\" README.md",
                Some("less"),
            ),
            // The audit's own line: pwsh's call operator (a glyph, skipped as
            // a prompt's is), then a SINGLE-quoted path with a space — and the
            // 8.3 short spelling of the same path, which never had a space.
            (
                "& 'C:\\Program Files\\Git\\usr\\bin\\less.exe' C:\\Windows\\win.ini",
                Some("less"),
            ),
            (
                "& C:\\PROGRA~1\\Git\\usr\\bin\\less.exe C:\\Windows\\win.ini",
                Some("less"),
            ),
            ("'/opt/my tools/claude' --resume", Some("claude")),
            ("C:\\Windows\\System32\\cmd.exe /c dir", Some("cmd")),
            ("C:\\tools\\build.CMD release", Some("build")),
            // An escaped space is part of the WORD, but a program's name is
            // never published with whitespace: it ends at the first one.
            ("my\\ tool --flag", Some("my")),
            ("\"git\" status", Some("git status")),
            // Only the executable extensions Windows resolves are dropped: a
            // dotted program NAME is not a path with an extension.
            ("python3.11 -m http.server", Some("python3.11")),
            ("/usr/bin/a.out", Some("a.out")),
            ("FOO=1 cmd --secret", Some("cmd")),
            ("FOO=1 BAR=two targo test", Some("targo test")),
            ("sudo -u root targo --unverified test", Some("targo test")),
            ("sudo apt install x", Some("apt")),
            ("env FOO=1 codex", Some("codex")),
            // A wrapper by full path is the same wrapper: basename first.
            ("/usr/bin/env FOO=1 codex", Some("codex")),
            (
                "/usr/bin/sudo -u root targo --unverified test",
                Some("targo test"),
            ),
            // A make/just target is user-named: only the conventional ones
            // are vocabulary.
            ("time make deploy-prod", Some("make")),
            ("make test", Some("make test")),
            ("just fmt", Some("just fmt")),
            ("aterm ctl ls", Some("aterm ctl")),
            ("atpkg install foo", Some("atpkg install")),
            ("nice -n 5 kubectl get pods -n prod", Some("kubectl get")),
            ("docker compose up", Some("docker compose")),
            ("'claude' 'resume'", Some("claude")),
            // Control removal must apply before wrapper/vocabulary matching,
            // even though ordinary words are now borrowed without a copy.
            ("clau\u{7f}de --private", Some("claude")),
            ("s\u{7f}udo -u user codex", Some("codex")),
            ("targo te\u{7f}st private", Some("targo test")),
            ("npm run build:prod", Some("npm run")),
            ("pnpm dlx create-foo", Some("pnpm dlx")),
            ("yarn build --token abc", Some("yarn build")),
            // A subcommand slot holding a path or an assignment is dropped.
            ("make ./target/x", Some("make")),
            // A COMPOUND line names the program of the segment that is RUNNING,
            // not the word it opens with: `cd x && claude` is the launch shape
            // the supervise-agent skill recommends, and it read `cd`. The last
            // of an `&&`/`;` chain; a trailing `|| …` runs only on failure, so
            // the operand before it; `exec` is a wrapper.
            ("cd ~/ay && claude", Some("claude")),
            ("cd ~/ay && exec claude", Some("claude")),
            ("exec claude --resume", Some("claude")),
            ("cd /tmp; codex", Some("codex")),
            ("make || echo failed", Some("make")),
            ("FOO=1 cd x && targo test", Some("targo test")),
            (
                "git pull && targo --unverified test -p aterm-gui",
                Some("targo test"),
            ),
            ("cd x && claude || echo failed", Some("claude")),
            ("cd x; claude;", Some("claude")),
            ("cd x &&", Some("cd")),
            ("(cd ~/ay && claude)", Some("claude")),
            ("cd x\nclaude", Some("claude")),
            // A pipeline is still named by its first command.
            ("cd x && cat log | grep err", Some("cat")),
            // An operator inside quotes or behind a backslash is an argument,
            // not a cut.
            ("claude -p \"fix a; then b\"", Some("claude")),
            ("echo 'x && y' && codex", Some("codex")),
            ("echo a \\; codex", Some("echo")),
            // A keyword opener stays the keyword: the body is not parsed, and
            // the `;` in `for …; do` is the keyword's grammar, not a list
            // operator (cutting there would answer `done`). `{` opens a group
            // the same way, so it keeps the first-word reading.
            ("for i in 1 2 3; do sleep 1; done", Some("for")),
            ("$ for i in 1 2 3; do sleep 1; done", Some("for")),
            ("while true; do claude; done", Some("while")),
            ("if [ -f x ]; then claude; fi", Some("if")),
            ("{ cd x; claude; }", Some("cd")),
            // The keyword need not open the LINE: behind a `cd x &&` or a
            // wrapper the first-word reading fell through to the running
            // segment, whose `;`-cut is the CLOSER — `done`, `fi` — and a
            // closer is never a program. The first segment whose program slot
            // holds an opener is read instead.
            ("cd x && for i in 1 2 3; do sleep 1; done", Some("for")),
            ("cd x && if true; then claude; fi", Some("if")),
            ("time for i in 1 2 3; do sleep 1; done", Some("for")),
            ("", None),
            ("   \t ", None),
            (";", None),
        ];
        // A keyword that CLOSES a compound command is never a program, so no
        // row may read one, whatever its `;`-cut would have answered.
        const KEYWORD_CLOSERS: [&str; 8] =
            ["done", "fi", "esac", "}", "then", "do", "else", "elif"];
        for (cmd, want) in cases {
            let got = command_detail(cmd);
            assert_eq!(got.as_deref(), *want, "{cmd:?}");
            assert!(
                !got.as_deref().is_some_and(|d| KEYWORD_CLOSERS.contains(&d)),
                "{cmd:?} reads a closer, which is never a program: {got:?}"
            );
        }
        // Every program in the table is exercised above, so a vocabulary
        // change cannot land without a row that shows what it does.
        for (program, vocabulary) in SUBCOMMANDS {
            assert!(
                cases
                    .iter()
                    .any(|(cmd, _)| cmd.split_whitespace().any(|w| w == *program)),
                "no table row exercises {program}"
            );
            assert!(
                !vocabulary.is_empty() && vocabulary.iter().all(|w| !w.starts_with('-')),
                "{program}: a vocabulary word is never a flag"
            );
        }
        // Bounded: a screen-scraped line can be long; the reply cannot.
        let long = format!("{} arg", "p".repeat(200));
        let detail = command_detail(&long).unwrap();
        assert_eq!(detail.chars().count(), 48);
        assert!(
            detail.capacity() <= 48 * 4,
            "long input capacity must not be retained"
        );
        let wide = format!("{} private", "終".repeat(80));
        assert_eq!(command_detail(&wide), Some("終".repeat(48)));
        // A program name is never reported with a subcommand it does not own.
        assert_eq!(command_detail("claude commit").as_deref(), Some("claude"));
    }

    /// The live E2E's D3 (2026-09-26): once the harness's live upgrade had
    /// relaunched Claude Code from the managed store — the line it types quotes
    /// every word, `'…/Library/Application Support/aterm/pkg/agents/claude'
    /// '--resume' …` — the tab's `status` read `detail=Application`. Words were
    /// cut at EVERY space, quoted or not, so the program slot held the first
    /// half of the path. A quoted or backslash-escaped space is part of its
    /// word, as the shell reads it, so the detail is the program's name.
    /// CONTROL: the same program at a path with no space reads as it always
    /// did, and a quoted argument with spaces is still never the detail.
    #[test]
    fn a_program_at_a_path_with_spaces_reads_its_name() {
        let store = "/Users//me/Library/Application Support/aterm/pkg/agents";
        let relaunch = format!(
            ". '/Users//me/.aterm/shell.d/00-atpkg.zsh'; rehash; '{store}/claude' \
             '--dangerously-skip-permissions' '--resume' 'b87ea568-277e-4ba7-bbd2-f0ab8bd14d1c'"
        );
        let cases: &[(String, &str)] = &[
            // The upgrade's relaunch line, verbatim in shape.
            (relaunch, "claude"),
            (format!("'{store}/claude' --resume x"), "claude"),
            (format!("\"{store}/claude\" --resume x"), "claude"),
            (
                format!("{} --resume x", store.replace(' ', "\\ ") + "/claude"),
                "claude",
            ),
            (format!("exec '{store}/claude'"), "claude"),
            // A quoted assignment with a space is ONE assignment, skipped.
            (format!("env 'FOO=a b' '{store}/codex' resume"), "codex"),
            // CONTROL: no space anywhere in the path.
            (
                "/Users//me/.local/share/claude/versions/2.1.281 --resume x".to_string(),
                "2.1.281",
            ),
            (
                "'/Users//me/.local/bin/claude' --resume x".to_string(),
                "claude",
            ),
            // CONTROL: an argument with spaces is an argument.
            (
                "claude -p 'fix the bug in Application Support'".to_string(),
                "claude",
            ),
        ];
        for (cmd, want) in cases {
            assert_eq!(command_detail(cmd).as_deref(), Some(*want), "{cmd:?}");
        }
        // Through the one producer the `status` record reads, by both roads: a
        // screen scrape of the typed line, and OSC 633;E's explicit line.
        let line = format!("'{store}/claude' '--resume' 'x'");
        let mut term = aterm_core::terminal::Terminal::new(24, 200);
        term.process(format!("\x1b]133;A\x07$ \x1b]133;B\x07{line}\n\x1b]133;C\x07").as_bytes());
        assert_eq!(
            executing_detail(&term).as_deref(),
            Some("claude"),
            "scraped"
        );
        term.process(b"\x1b]133;D;0\x07");
        term.process(
            format!("\x1b]133;A\x07$ \x1b]633;E;{line}\x07\x1b]133;B\x07{line}\n\x1b]133;C\x07")
                .as_bytes(),
        );
        assert_eq!(
            executing_detail(&term).as_deref(),
            Some("claude"),
            "explicit"
        );
    }

    /// The quote that lets a spaced path be ONE word (D3, above) must never
    /// carry an ARGUMENT into `detail=` — "never an argument" is the privacy
    /// rule, and splitting at every space kept it by construction. A line the
    /// POSIX reading gets wrong ran its program's word on to the end of the
    /// line, and `detail=` published whatever followed the word's last `/`:
    /// measured on the first D3 fix, `$'/usr/local/bin/don\'t' --token
    /// SECRET` read `'t' --token SECRET`, the harness's own fish quoting read
    /// `claude' '--resume' 'b87ea568-…` (the session id), and `don't --token
    /// SECRET` read the whole line. And ending a word at the first whitespace
    /// after the LAST quote that opened still leaked when the argument's
    /// opening quote closed the program's apostrophe: `don't '/secret/tok'`
    /// read `tok`, `/Users//o'neil/bin/claude --resume '/secret/tok'` read
    /// `tok`, `it's-a-script.sh --password 'x/hunter2'` read `hunter2`. So a
    /// word ends where EITHER reading (POSIX, fish) ends it, a reading that
    /// leaves a quote open ends the word at the word's own first whitespace,
    /// and the published name ends at its first whitespace. CONTROL: the
    /// managed copy's `Application Support` path, quoted every way a shell
    /// quotes it, still reads `claude`, and `don't "/secret/tok"` — whose
    /// double quotes sit inside the apostrophe's open quote, so the quote left
    /// open is the program's own — read `don't` under both rules.
    #[test]
    fn a_misquoted_line_never_publishes_an_argument() {
        let store = "Library/Application Support/aterm/pkg/agents/claude";
        let cases: &[(String, &str)] = &[
            // bash/zsh `$'…'`, where `\'` is a quote: POSIX reads the quote
            // after `t` as opening one that never closes. What is read is a
            // piece of the program's name (`don't`), never the argument.
            (r"$'/usr/local/bin/don\'t' --token SECRET".into(), "t"),
            // The harness's own fish quoting (`upgrade::quote`) of a home with
            // an apostrophe: POSIX runs the program's word into the session id.
            (
                r"'/Users//o\'neil/bin/claude' '--resume' 'b87ea568-277e-4ba7-bbd2-f0ab8bd14d1c'"
                    .into(),
                "claude",
            ),
            // …and a line POSIX misreads with every quote closed, whose last
            // `/` sits in an argument: no open quote to notice, only fish's
            // nearer end.
            (
                r"'/Users//o\'neil/bin/claude' '--resume' '/secret/tok en'".into(),
                "claude",
            ),
            // An apostrophe in a bare word opens a quote that never closes —
            // and the rest of the line may hold a `/` of its own.
            ("don't --token SECRET".into(), "don't"),
            ("don't /secret/tok".into(), "don't"),
            (
                "it's-a-script.sh --password hunter2".into(),
                "it's-a-script.sh",
            ),
            // …and the ARGUMENT's opening quote may close the program's
            // apostrophe, running the word through the argument, with the
            // line's last quote left open behind it. The word still ends at
            // its first whitespace, so nothing past the program is read.
            ("don't '/secret/tok'".into(), "don't"),
            (
                "it's-a-script.sh --password 'x/hunter2'".into(),
                "it's-a-script.sh",
            ),
            ("don't --token '/secret/tok'".into(), "don't"),
            (
                "don't '--resume' '/Users//me/secret-session'".into(),
                "don't",
            ),
            ("sudo don't '/secret/tok'".into(), "don't"),
            ("o'neil-tool --token '/secret/tok'".into(), "o'neil-tool"),
            (
                "/Users//o'neil/bin/claude --resume '/secret/tok'".into(),
                "claude",
            ),
            ("can't 'x' '/secret/tok'".into(), "can't"),
            ("it's 'a' b '/secret/tok'".into(), "it's"),
            // CONTROL: double quotes inside the apostrophe's open quote pair
            // nothing, so the quote left open is the program's own.
            ("don't \"/secret/tok\"".into(), "don't"),
            // `env -S` hands env ONE string that env splits into the program
            // and its arguments: the program is that string's first word,
            // never its basename (the third review read `server.js`, `tok`,
            // `notes.py`, `app.ts`).
            (
                "env -S \"node --inspect /Users//me/secret-project/server.js\"".into(),
                "node",
            ),
            ("env -S 'claude --resume /secret/tok'".into(), "claude"),
            (
                "/usr/bin/env -S \"python3 /Users//me/private/notes.py --token x\"".into(),
                "python3",
            ),
            (
                "env --split-string 'deno run --allow-read /Users//me/secret/app.ts'".into(),
                "deno",
            ),
            (
                "env --split-string='deno run --allow-read /Users//me/secret/app.ts'".into(),
                "deno",
            ),
            ("env -vS 'node /srv/secret/server.js'".into(), "node"),
            ("env -S'node /srv/secret/server.js'".into(), "node"),
            ("env -S '-i FOO=1 /usr/bin/node /srv/x.js'".into(), "node"),
            // CONTROL: `-u` takes a value, so `-uS` unsets `S` and is no split.
            ("env -uS /usr/bin/node /srv/x.js".into(), "node"),
            // A space in the program's OWN name is never published.
            (r"/Users//me/my\ tool --token SECRET".into(), "my"),
            ("\"/opt/my tools/run me\" --token SECRET".into(), "run"),
            // fish's quoting of the spaced store path under an apostrophe
            // home: the readings disagree, the nearer end wins. A directory's
            // name, never an argument (the cost of not knowing the dialect).
            (
                format!(r"'/Users//o\'neil/{store}' '--resume' 'SECRET'"),
                "Application",
            ),
            // CONTROL: the managed copy's path, as POSIX, fish (no apostrophe:
            // the same line), double quotes and backslashes write it.
            (format!("'/Users//me/{store}' '--resume' 'SECRET'"), "claude"),
            (
                format!(r"'/Users//o'\''neil/{store}' '--resume' 'SECRET'"),
                "claude",
            ),
            (format!("\"/Users//me/{store}\" --resume SECRET"), "claude"),
            (
                format!("/Users//me/{} --resume SECRET", store.replace(' ', r"\ ")),
                "claude",
            ),
        ];
        const ARGUMENTS: [&str; 7] = [
            "SECRET", "token", "hunter2", "b87ea568", "resume", "secret", "tok",
        ];
        for (cmd, want) in cases {
            let got = command_detail(cmd);
            assert_eq!(got.as_deref(), Some(*want), "{cmd:?}");
            let got = got.unwrap_or_default();
            assert!(
                !got.contains(char::is_whitespace),
                "{cmd:?}: a program's name is published without whitespace: {got:?}"
            );
            for argument in ARGUMENTS {
                assert!(
                    !got.contains(argument),
                    "{cmd:?}: {got:?} carries the argument {argument:?}"
                );
            }
        }
    }

    /// The allocation-free selection must keep the old group/pop semantics,
    /// especially blank groups and trailing OR alternatives. Quoting stays in
    /// the unchanged splitter and is exercised by the privacy table above.
    #[test]
    fn borrowed_running_segment_matches_the_previous_group_selection() {
        fn previous<'a>(cmdline: &'a str, segments: &[(ListOp, &'a str)]) -> &'a str {
            let mut groups: Vec<Vec<(ListOp, &str)>> = Vec::new();
            for &(op, text) in segments {
                if op == ListOp::Seq || groups.is_empty() {
                    groups.push(Vec::new());
                }
                groups.last_mut().unwrap().push((op, text));
            }
            let mut chain = groups
                .into_iter()
                .rev()
                .find(|group| group.iter().any(|(_, text)| !text.trim().is_empty()))
                .unwrap_or_default();
            while chain.len() > 1
                && chain
                    .last()
                    .is_some_and(|(op, text)| *op == ListOp::Or || text.trim().is_empty())
            {
                chain.pop();
            }
            chain.last().map_or(cmdline, |(_, text)| text)
        }

        let words = ["", " ", "first", "last"];
        let operators = [";", "\n", "&&", "||"];
        // Four operands and three operators: all 16,384 combinations include
        // empty first operands, entirely blank groups and mixed &&/|| tails.
        for case in 0..4usize.pow(7) {
            let mut choices = case;
            let mut cmdline = String::new();
            for segment in 0..4 {
                if segment != 0 {
                    cmdline.push_str(operators[choices % 4]);
                    choices /= 4;
                }
                cmdline.push_str(words[choices % 4]);
                choices /= 4;
            }
            let segments = split_list(&cmdline);
            assert_eq!(
                running_segment(&cmdline, &segments),
                previous(&cmdline, &segments),
                "{cmdline:?}",
            );
        }
    }

    /// `executing_detail` is the one producer: a block that is EXECUTING names
    /// its (sanitized) command; a prompt, an entering block and a completed
    /// block name nothing, whatever text they carry.
    #[test]
    fn executing_detail_reads_only_an_executing_block() {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        assert_eq!(executing_detail(&term), None, "no shell integration");
        term.process(b"\x1b]133;A\x07$ ");
        assert_eq!(executing_detail(&term), None, "prompt only");
        term.process(b"\x1b]133;B\x07targo --unverified test -p aterm-gui");
        assert_eq!(executing_detail(&term), None, "entering, not executing");
        term.process(b"\n\x1b]133;C\x07");
        assert_eq!(
            executing_detail(&term).as_deref(),
            Some("targo test"),
            "executing: screen-scraped command, sanitized"
        );
        term.process(b"running\n\x1b]133;D;0\x07");
        assert_eq!(executing_detail(&term), None, "complete");
        // OSC 633;E's explicit command line wins over the screen scrape.
        term.process(b"\x1b]133;A\x07$ \x1b]633;E;claude --dangerously-skip-permissions\x07\x1b]133;B\x07claude --dangerously-skip-permissions\n\x1b]133;C\x07");
        assert_eq!(executing_detail(&term).as_deref(), Some("claude"));
        term.process(b"\x1b]133;D;0\x07");
        // A WIDE prompt glyph before the 133;B mark: the cut is by display
        // column, so the program keeps its first letter.
        term.process(
            "]133;A🐱 $ ]133;Bcodex resume
]133;C"
                .as_bytes(),
        );
        assert_eq!(executing_detail(&term).as_deref(), Some("codex"));
    }

    /// The `status` record's `detail=`: the sanitized running command while the
    /// block executes, `-` otherwise; the engine's fact, so it is answered
    /// before the classifier's first publish, under a pin, and never by
    /// waiting on a contended terminal.
    #[test]
    fn the_status_record_details_the_executing_block_and_nothing_else() {
        let mut app = crate::App::headless_for_test();
        let term = app.pool.get(0).expect("session 0").term.clone();

        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" detail=- "), "nothing running: {record}");

        term.lock()
            .unwrap()
            .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 30\n\x1b]133;C\x07");
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" observed=false ") && record.contains(" detail=sleep "),
            "the engine's fact precedes the classifier's first publish: {record}"
        );

        let t0 = Instant::now();
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Executing);
        settle_observer(&mut app.session_status, 0, &ev, t0);
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" observed=true ") && record.contains(" detail=sleep "),
            "{record}"
        );
        assert!(
            !record.contains("30"),
            "arguments never reach the wire: {record}"
        );

        // A pin answers the subject from the leaf lock and keeps the detail.
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set("title", Some("deploy".to_string()))
            .expect("a short title is within cap");
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" subject=deploy subject_source=pin ")
                && record.contains(" detail=sleep "),
            "{record}"
        );

        // Contended: `-`, never a wait — and the pinned subject still answers.
        let held = term.lock().unwrap();
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" subject_source=pin ") && record.contains(" detail=- "),
            "a contended terminal is `-`, not a stall: {record}"
        );
        drop(held);

        term.lock().unwrap().process(b"\x1b]133;D;0\x07");
        let record = app.session_status_record(0).expect("live session");
        assert!(record.contains(" detail=- "), "complete: {record}");
    }

    /// The chrome already composes `running {detail}`; with the field populated
    /// it reads as the running program, and a blank detail stays plain `running`.
    #[test]
    fn chrome_renders_the_running_detail() {
        let status = |detail: Option<&str>| Status {
            phase: Phase::Running,
            last_outcome: Outcome::None,
            since: Instant::now(),
            detail: detail.map(str::to_string),
            confidence: Confidence::Strong,
            reasons: vec![Reason::ShellBlock],
            conflict: false,
        };
        assert_eq!(
            summary_text(&status(Some("targo test"))).as_deref(),
            Some("running targo test")
        );
        assert_eq!(summary_text(&status(Some(""))).as_deref(), Some("running"));
        assert_eq!(summary_text(&status(None)).as_deref(), Some("running"));
    }

    /// The chrome's `detail` was seeded `None` and never written, so every tab
    /// read plain `running` while the wire named the program. The sweep now
    /// folds `executing_detail` onto the published status through
    /// [`StatusObserver::set_detail`]: an Executing block publishes the
    /// sanitized program, and idle (prompt, complete, no integration) clears it.
    #[test]
    fn the_sweep_publishes_the_running_detail_onto_the_chrome_status() {
        let mut app = crate::App::headless_for_test();
        let term = app.pool.get(0).expect("session 0").term.clone();
        let t0 = Instant::now();

        // Idle: no executing block, so the published detail the chrome reads is
        // empty (plain `running` once running, never a stale command).
        app.observe_session_statuses(t0);
        assert_eq!(
            app.session_status.status(0).and_then(|s| s.detail.clone()),
            None,
            "idle publishes no detail"
        );

        // An executing block: the same sweep guard reads the command and folds
        // its sanitized basename onto the status `summary_text` turns into
        // `running <detail>`.
        term.lock()
            .unwrap()
            .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 30\n\x1b]133;C\x07");
        let t1 = t0 + Duration::from_secs(10);
        app.observe_session_statuses(t1);
        assert_eq!(
            app.session_status
                .status(0)
                .and_then(|s| s.detail.clone())
                .as_deref(),
            Some("sleep"),
            "the executing block's sanitized program reaches the chrome status"
        );

        // The block completes: back to idle, the detail clears — a done tab
        // never keeps naming the command that finished.
        term.lock().unwrap().process(b"\x1b]133;D;0\x07");
        let t2 = t1 + Duration::from_secs(10);
        app.observe_session_statuses(t2);
        assert_eq!(
            app.session_status.status(0).and_then(|s| s.detail.clone()),
            None,
            "a completed block clears the chrome detail"
        );
    }
}

/// THE SERVER-PUBLISHED AGENT VERDICT and PROGRAM IDENTITY, driven through the
/// real sweep (`App::observe_session_statuses`) on a headless App.
#[cfg(test)]
mod agent_verdict_tests {
    use std::time::{Duration, Instant};

    use super::{
        PROGRAM_NAME_CONFIRM, PROGRAM_NAMED_RECHECK_FLOOR, PROGRAM_RECHECK, SessionSlot, StatusFsm,
        StatusObserver, StatusPolicy,
    };
    use crate::presence::Cursor;
    use crate::{App, WindowId};

    /// The background resolver can answer after the usual one-interval
    /// follow-up has already gone quiet. Its completion wake restores one
    /// status read at the observation floor, including on a static screen.
    #[test]
    fn a_late_program_answer_owns_one_observation_wake() {
        let t0 = Instant::now();
        let floor = Duration::from_millis(250);
        let id = 7;
        let pgid = 42;
        let generation = crate::control::ScreenGen { epoch: 0, seq: 1 };
        let policy = StatusPolicy::default();
        let mut observer = StatusObserver::new(policy, floor);
        observer.sessions.insert(
            id,
            SessionSlot {
                fsm: StatusFsm::new(policy, t0),
                next_due: t0 + floor,
                revision: 1,
                lifecycle: None,
            },
        );
        observer.agent_observe(
            id,
            generation,
            Some(vec!["Claude Code composer".into()]),
            Cursor::Unknown,
            Some("claude".into()),
            pgid,
            false,
            t0,
        );
        observer.sessions.get_mut(&id).unwrap().next_due = t0 + floor * 2;
        observer.agent_observe(
            id,
            generation,
            None,
            Cursor::Unknown,
            Some("claude".into()),
            pgid,
            false,
            t0 + floor,
        );
        assert_eq!(observer.next_wake(), None, "the ordinary follow-up retired");

        observer.note_program_answered(id);
        observer.note_program_answered(id);
        assert_eq!(observer.next_wake(), Some(t0 + floor * 2));
        assert!(observer.agent_zone_wanted(
            id,
            generation,
            Cursor::Unknown,
            &Some("codex".into()),
            t0 + floor * 2,
        ));
        observer.agent_observe(
            id,
            generation,
            Some(vec!["Codex prompt".into()]),
            Cursor::Unknown,
            Some("codex".into()),
            pgid,
            false,
            t0 + floor * 2,
        );
        assert_eq!(observer.next_wake(), None);
        observer.retire(id);
        observer.note_program_answered(id);
        assert_eq!(observer.next_wake(), None);
    }

    /// Claude Code 2.1.280's rm circuit-breaker box, measured 2026-09-23 in a
    /// private headless aterm (120 columns; rows 1-24 of the capture).
    const RM_BOX: &[&str] = &[
        "",
        "  Get to finished work sooner with Opus 5.5. Switch anytime with /model.",
        "  1 more notice hidden",
        "",
        "\u{276f} Use the Bash tool to run exactly this one line, verbatim, as one call: S=$PWD/tmp; for p in a b; do set -- $p; rm -rf",
        "  $S/$1; done",
        "",
        "  Removing directories tmp/a and tmp/b",
        "  \u{23bf}  $ S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done",
        "",
        "\u{2500}",
        " Bash command",
        "",
        "   S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done",
        "   Remove directories tmp/a and tmp/b",
        "",
        " \u{2502} Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf $S/$1` (bind $1 and rewrite its $S as",
        " \u{2502} \"${S:?}\" or use a literal path)",
        "",
        " Do you want to proceed?",
        " \u{276f} 1. Yes",
        "   2. No",
        "",
        " Esc to cancel \u{00b7} Tab to amend",
    ];

    /// A busy Claude screen: a transcript row, the spinner, the composer
    /// between its two rules, the footer (the shape `aterm_phase` reads).
    fn busy_screen(secs: u32) -> Vec<String> {
        let rule = "\u{2500}".repeat(120);
        let mut rows = vec![String::new(); 24];
        rows[0] = "\u{23fa} Working on it.".into();
        rows[19] = format!("\u{2736} Deliberating\u{2026} ({secs}s \u{00b7} thinking)");
        rows[20] = rule.clone();
        rows[21] = "\u{276f} ".into();
        rows[22] = rule;
        rows[23] = "  \u{23f5}\u{23f5} bypass permissions on \u{00b7} esc to interrupt".into();
        rows
    }

    /// The rm box with its top rule at full width, and a transcript row above
    /// it that keeps TICKING (a live monitor line) — the audit's INT-1 shape.
    fn box_screen(tick: u32) -> Vec<String> {
        let mut rows: Vec<String> = RM_BOX.iter().map(|r| (*r).to_string()).collect();
        rows[10] = "\u{2500}".repeat(120);
        rows[0] = format!("\u{23fa} Monitor tick {tick}");
        rows
    }

    fn paint(app: &App, sid: u64, rows: &[String]) {
        let pooled = app.pool.get(sid).expect("pooled");
        let mut t = crate::term_lock(&pooled.term);
        if t.cols() != 120 {
            t.resize(24, 120);
        }
        let mut bytes = String::from("\x1b[H\x1b[2J");
        for (i, row) in rows.iter().enumerate() {
            bytes.push_str(&format!("\x1b[{};1H{row}", i + 1));
        }
        t.process(bytes.as_bytes());
    }

    fn published(app: &App, sid: u64) -> crate::session_timeline::AgentPublication {
        app.pool
            .get(sid)
            .expect("pooled")
            .ctx
            .timeline
            .lock()
            .unwrap()
            .agent()
            .clone()
    }

    fn app_with_stub() -> (App, u64) {
        let mut app = App::headless_for_test();
        let sid = app.next_session_id;
        app.push_stub_tab(WindowId(0), crate::stub_session(sid));
        (app, sid)
    }

    /// INT-1: a box raised while a row keeps ticking must reach `agent=` at
    /// the next sweep, although the shell status FSM's revision — the old
    /// classifier gate — never moves. The negative control is the revision
    /// itself: it stays put across the busy→prompt move, so a verdict gated on
    /// it (today's `level=quiet` with a box up) would never have been read.
    #[test]
    fn a_box_under_a_ticking_row_is_published_though_the_status_revision_never_moves() {
        let (mut app, sid) = app_with_stub();
        let t0 = Instant::now();
        let step = Duration::from_millis(300);
        let mut t = t0;
        for secs in 0..3 {
            paint(&app, sid, &busy_screen(secs));
            let _ = app.observe_session_statuses(t);
            t += step;
        }
        let busy = published(&app, sid);
        assert_eq!(busy.word, "busy", "the composer frame identified Claude");
        let revision = app.session_status.revision(sid);

        paint(&app, sid, &box_screen(1));
        let changed = app.observe_session_statuses(t);
        for id in &changed {
            app.refresh_presence_session(*id, false);
        }
        t += step;
        let prompt = published(&app, sid);
        assert_eq!(prompt.word, "prompt", "the box is read at the next sweep");
        assert_eq!(
            prompt.detail.as_deref(),
            Some("bash:not-read-only"),
            "the box's kind and the read-only verdict, never the command"
        );
        assert_eq!(prompt.rev, busy.rev + 1);
        assert!(changed.contains(&sid), "the caller refreshes presence");
        assert_eq!(
            app.session_status.revision(sid),
            revision,
            "NEGATIVE CONTROL: the status revision did not move, so the old \
             revision-gated classifier would still read busy"
        );
        let record = app.session_status_record(sid).expect("live");
        assert!(
            record.contains(" agent=prompt agent_detail=bash:not-read-only agent_rev="),
            "{record}"
        );
        assert!(record.contains(" level=attention story="), "{record}");
        assert!(record.contains(" why=prompt "), "{record}");
        // The move is on the timeline, so the events digest pushes it.
        let events: Vec<String> = app
            .pool
            .get(sid)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "agent-change")
            .map(|e| e.payload.clone())
            .collect();
        let stamp = prompt.stamp.expect("a published verdict carries its read");
        assert_eq!(
            events.last().map(String::as_str),
            Some(
                format!(
                    "prompt rev={} gen={} fp={:016x}",
                    prompt.rev, stamp.generation, stamp.fp
                )
                .as_str()
            )
        );

        // The row keeps ticking: the zone changed, so it is re-read — and the
        // verdict holds, so nothing is re-published.
        let calls = crate::presence::classifier_calls();
        paint(&app, sid, &box_screen(2));
        let _ = app.observe_session_statuses(t);
        t += step;
        assert_eq!(crate::presence::classifier_calls() - calls, 1);
        assert_eq!(
            published(&app, sid).rev,
            prompt.rev,
            "same verdict, same rev"
        );
        // An unchanged screen costs no classification at all.
        let _ = app.observe_session_statuses(t);
        t += step;
        let _ = app.observe_session_statuses(t);
        assert_eq!(
            crate::presence::classifier_calls() - calls,
            1,
            "no content move, no read"
        );
    }

    /// THE VERDICT'S STAMP (review major): `agent_gen=`/`agent_fp=` name the
    /// screen the verdict was read from, byte-equal to what `status gen=`/
    /// `hash=` printed for that screen. A screen that changes after the sweep
    /// moves `status gen=` but NOT `agent_gen=` — the NEGATIVE CONTROL, and the
    /// reason a press decided from the verdict fences on `agent_gen`: a fence
    /// on the fresh `status gen=` would hold over a box nothing classified.
    /// A re-read whose zone is unchanged re-confirms the verdict: the stamp
    /// moves, the rev does not.
    #[test]
    fn the_verdict_carries_the_stamp_of_the_screen_it_was_read_from() {
        let field = |record: &str, key: &str| {
            record
                .split(' ')
                .find_map(|kv| kv.strip_prefix(key))
                .map(str::to_string)
                .unwrap_or_else(|| panic!("{key} in {record}"))
        };
        let (mut app, sid) = app_with_stub();
        let mut t = Instant::now();
        let step = Duration::from_millis(300);
        paint(&app, sid, &busy_screen(0));
        let _ = app.observe_session_statuses(t);
        t += step;
        paint(&app, sid, &box_screen(1));
        let _ = app.observe_session_statuses(t);
        t += step;
        let record = app.session_status_record(sid).expect("live");
        assert!(record.contains(" agent=prompt "), "{record}");
        assert_eq!(
            field(&record, "agent_gen="),
            field(&record, "gen="),
            "{record}"
        );
        assert_eq!(
            field(&record, "agent_fp="),
            field(&record, "hash="),
            "{record}"
        );
        let rev = published(&app, sid).rev;

        // The screen moves with no sweep in between: `status` reads the new
        // screen, the verdict still names the one it was read from.
        paint(&app, sid, &box_screen(2));
        let record = app.session_status_record(sid).expect("live");
        assert_ne!(
            field(&record, "agent_gen="),
            field(&record, "gen="),
            "{record}"
        );
        assert_ne!(
            field(&record, "agent_fp="),
            field(&record, "hash="),
            "{record}"
        );

        // The sweep re-reads it (the ticking row, same verdict): the stamp
        // catches up, the rev stays.
        let _ = app.observe_session_statuses(t);
        let record = app.session_status_record(sid).expect("live");
        assert_eq!(
            field(&record, "agent_gen="),
            field(&record, "gen="),
            "{record}"
        );
        assert_eq!(
            field(&record, "agent_fp="),
            field(&record, "hash="),
            "{record}"
        );
        assert_eq!(published(&app, sid).rev, rev, "same verdict, same rev");
    }

    /// A zone re-read inside [`super::AGENT_MIN_INTERVAL`] is deferred, and
    /// the deferral arms a follow-up wake instead of being lost.
    #[test]
    fn the_rate_floor_defers_a_read_and_arms_a_followup() {
        let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
        let mut obs =
            super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
        let t0 = Instant::now();
        assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &None, t0));
        let rows = vec!["$ ls".to_string()];
        let _ = obs.agent_observe(1, g(1), Some(rows), Cursor::Unknown, None, -1, false, t0);
        assert!(
            !obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &None, t0),
            "nothing moved"
        );
        let t1 = t0 + Duration::from_millis(100);
        assert!(
            !obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &None, t1),
            "a read 100 ms after the last is deferred"
        );
        let _ = obs.agent_observe(1, g(2), None, Cursor::Unknown, None, -1, false, t1);
        assert!(obs.agents[&1].followup, "and a follow-up is armed");
        assert!(obs.agent_zone_wanted(
            1,
            g(2),
            Cursor::Unknown,
            &None,
            t0 + super::AGENT_MIN_INTERVAL
        ));
        // A resolution in flight keeps the follow-up armed over an unmoved
        // screen, so its answer is read one interval later.
        let t2 = t0 + super::AGENT_MIN_INTERVAL;
        let _ = obs.agent_observe(
            1,
            g(2),
            Some(vec!["$ ls".into()]),
            Cursor::Unknown,
            None,
            7,
            true,
            t2,
        );
        assert!(obs.agents[&1].followup);
        assert!(
            obs.agent_zone_wanted(
                1,
                g(2),
                Cursor::Unknown,
                &Some("sleep".into()),
                t2 + super::AGENT_MIN_INTERVAL
            ),
            "the resolved program re-reads an unchanged zone"
        );
    }

    /// THE FINAL FRAME OF A BURST THAT LANDED BETWEEN LOOKS (2026-09-24,
    /// live: a box answered and a static busy screen drawn still read
    /// `agent=prompt` after the host had cleared its badge; an exited agent's
    /// prompt left `program=claude` standing). A look that saw nothing move
    /// arms no follow-up, and output inside the interval runs no look: the
    /// output wake's [`super::StatusObserver::note_output`] owes the session a
    /// look one interval after the burst (never before its next due instant)
    /// — and so does a look the terminal lock refused
    /// ([`super::StatusObserver::note_skipped`]). NEGATIVE CONTROL: before the
    /// burst nothing is owed that soon.
    #[test]
    fn output_between_looks_is_owed_a_look_within_one_interval_of_it() {
        let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
        let interval = Duration::from_millis(250);
        let mut obs = super::StatusObserver::new(super::StatusPolicy::default(), interval);
        let ev = super::Evidence {
            pin: None,
            shell: None,
            lifecycle: None,
            foreground_job: None,
            activity: super::ActivitySample {
                alt_screen: false,
                content_seq: 1,
                last_input: None,
                last_output: None,
            },
        };
        let rows = vec!["$ ls".to_string()];
        let t0 = Instant::now();
        let _ = obs.observe(1, &ev, t0);
        assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &None, t0));
        let _ = obs.agent_observe(1, g(1), Some(rows), Cursor::Unknown, None, -1, false, t0);
        // A look at the same screen: nothing moved, nothing owed by it.
        let t1 = t0 + interval;
        let _ = obs.observe(1, &ev, t1);
        assert!(!obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &None, t1));
        let _ = obs.agent_observe(1, g(1), None, Cursor::Unknown, None, -1, false, t1);
        let due = obs.sessions[&1].next_due;
        assert!(
            obs.next_wake().is_none_or(|w| w > due),
            "NEGATIVE CONTROL: nothing owes a look at {due:?} yet"
        );
        // A burst lands 10 ms later: not due, so no look — but one is owed,
        // one interval after it.
        let burst = t1 + Duration::from_millis(10);
        obs.note_output(1, burst);
        let owed = burst + interval;
        assert!(
            owed >= due,
            "fixture: the burst's interval ends past the floor"
        );
        assert_eq!(obs.next_wake(), Some(owed), "a look once the burst paused");
        // That look reads the new frame, and owes nothing more once it holds.
        let t2 = owed;
        let _ = obs.observe(1, &ev, t2);
        assert!(obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &None, t2));
        let _ = obs.agent_observe(
            1,
            g(2),
            Some(vec!["$ ls".into(), "x".into()]),
            Cursor::Unknown,
            None,
            -1,
            false,
            t2,
        );
        let t3 = t2 + interval;
        let _ = obs.observe(1, &ev, t3);
        let _ = obs.agent_observe(1, g(2), None, Cursor::Unknown, None, -1, false, t3);
        let due = obs.sessions[&1].next_due;
        assert!(obs.next_wake().is_none_or(|w| w > due), "settled again");
        // A look the lock refused owes the next one as well.
        obs.note_skipped(1, t3);
        assert_eq!(obs.next_wake(), Some(obs.sessions[&1].next_due));
    }

    /// OUTPUT THAT KEEPS ARRIVING IS NOT A POLL (the spin gate's claude row,
    /// `tools/spin-conformance/spin_probe.sh`, after 2026-09-24's owed look
    /// landed: 39 timer wakes in 10 s against a quiet loop's 5). A program
    /// that repaints without moving the grid ~6x/s — a Claude Code spinner —
    /// wakes the loop by its own output, and each output wake's sweep looks
    /// whenever the session is due; an owed look pinned to the slot's next due
    /// instant fired between those wakes, 4x/s, forever. The owed look waits
    /// for the output to PAUSE: every burst pushes it one interval on, so no
    /// timer precedes the next burst, and the last burst is still read within
    /// one interval of it. RED before: the owed wake was the next due instant.
    #[test]
    fn output_that_keeps_arriving_owes_no_look_before_it_pauses() {
        let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
        let interval = Duration::from_millis(250);
        let gap = Duration::from_millis(167);
        let mut obs = super::StatusObserver::new(super::StatusPolicy::default(), interval);
        let ev = super::Evidence {
            pin: None,
            shell: None,
            lifecycle: None,
            foreground_job: None,
            activity: super::ActivitySample {
                alt_screen: false,
                content_seq: 1,
                last_input: None,
                last_output: None,
            },
        };
        // Settle first: the status FSM's first sample serves a dwell, and the
        // first look (a screen never read) owes its own follow-up. Past both,
        // the only deadline left to judge is the one output owes.
        let start = Instant::now();
        let _ = obs.observe(1, &ev, start);
        let _ = obs.agent_observe(
            1,
            g(1),
            Some(vec!["* Thinking".into()]),
            Cursor::Unknown,
            None,
            -1,
            false,
            start,
        );
        let t0 = start + Duration::from_secs(5);
        let _ = obs.observe(1, &ev, t0);
        let _ = obs.agent_observe(1, g(1), None, Cursor::Unknown, None, -1, false, t0);
        assert_eq!(
            obs.next_wake(),
            None,
            "fixture: a settled, unmoved session owes nothing"
        );
        // Twelve output wakes ~6/s over an unmoved screen, in the output
        // wake's order: `note_output`, then the sweep, which looks only when
        // the session is due (the rate gate) and answers for the burst.
        let mut at = t0;
        let mut looks = 0;
        for _ in 0..12 {
            at += gap;
            obs.note_output(1, at);
            if obs.sessions[&1].next_due <= at {
                let _ = obs.observe(1, &ev, at);
                let _ = obs.agent_observe(1, g(1), None, Cursor::Unknown, None, -1, false, at);
                looks += 1;
            }
            let next_burst = at + gap;
            assert!(
                obs.next_wake().is_none_or(|w| w > next_burst),
                "no timer fires before the next burst: {:?} vs {next_burst:?}",
                obs.next_wake()
            );
        }
        assert!(
            looks >= 5,
            "fixture: the output wakes' own sweeps looked ({looks})"
        );
        // The LAST burst lands between looks, and the output pauses: it is
        // read within one interval of it.
        let last = at + gap;
        obs.note_output(1, last);
        assert!(
            obs.sessions[&1].next_due > last,
            "fixture: not due at the burst"
        );
        assert_eq!(obs.next_wake(), Some(last + interval));
    }

    /// THE CHANGE GATE HASHES THE ZONE THE VERDICT READS (the review of
    /// 2026-09-26): the sweep re-derives the verdict only when its zone's
    /// hash moved, and that hash covered the grid's last 40 rows. Claude
    /// Code's inline renderer draws the REPL at the top of a tall pane, so a
    /// turn that ends ABOVE those rows — the spinner row over the prompt box
    /// replaced, every row from the box down unchanged — moved no hash, and
    /// the verdict stayed `busy`. The gate hashes the live zone
    /// (`presence::live_zone`): the idle REPL is classified and published.
    /// NEGATIVE CONTROL: the same screen again under a new generation moves
    /// no hash, and nothing is re-derived.
    #[test]
    fn a_change_above_the_last_40_rows_of_an_inline_repl_is_classified() {
        use aterm_phase::prompt::fixtures::{INLINE_REPL_READY, screen};
        let ready = screen(INLINE_REPL_READY);
        assert_eq!(ready.len(), 50);
        let top = ready
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("the top rule");
        let mut busy = ready.clone();
        busy[top - 1] = "✶ Deliberating… (3s · thinking)".to_string();
        assert!(top - 1 < ready.len() - 40, "above the grid's last 40 rows");
        assert_eq!(busy[ready.len() - 40..], ready[ready.len() - 40..]);
        let claude = Some("claude".to_string());
        let mut obs =
            super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
        let t0 = Instant::now();
        let mut look = |seq: u64, rows: &[String], after: u64| {
            let at = t0 + Duration::from_secs(after);
            let generation = crate::control::ScreenGen { epoch: 0, seq };
            assert!(obs.agent_zone_wanted(1, generation, Cursor::Unknown, &claude, at));
            obs.agent_observe(
                1,
                generation,
                Some(rows.to_vec()),
                Cursor::Unknown,
                claude.clone(),
                100,
                false,
                at,
            )
            .publish
            .map(|p| p.0)
        };
        assert_eq!(look(1, &busy, 0), Some("busy"));
        assert_eq!(look(2, &ready, 1), Some("idle"), "the turn's end");
        assert_eq!(look(3, &ready, 2), None, "the control: nothing moved");
    }

    /// THE CURSOR MOVING ALONE IS READ (2026-09-27): Claude Code's `idle` is
    /// the prompt box that holds the terminal's cursor, and the cursor moves
    /// without the content seq — a frame drawn whole, then the cursor stepped
    /// into its box. The gate reads the zone again when the cursor moved
    /// under an unchanged generation, and publishes `idle` once it is in the
    /// box. NEGATIVE CONTROL: the same generation and cursor again read
    /// nothing.
    #[test]
    fn a_cursor_that_steps_into_the_box_is_read() {
        use aterm_phase::prompt::fixtures::{INLINE_REPL_READY, cursor, screen};
        let ready = screen(INLINE_REPL_READY);
        let (caret, _) = cursor(INLINE_REPL_READY).expect("measured");
        let claude = Some("claude".to_string());
        let mut obs =
            super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
        let t0 = Instant::now();
        let generation = crate::control::ScreenGen { epoch: 0, seq: 7 };
        let mut look = |cursor: Cursor, after: u64| {
            let at = t0 + Duration::from_secs(after);
            obs.agent_zone_wanted(1, generation, cursor, &claude, at)
                .then(|| {
                    obs.agent_observe(
                        1,
                        generation,
                        Some(ready.clone()),
                        cursor,
                        claude.clone(),
                        100,
                        false,
                        at,
                    )
                    .publish
                    .map(|p| p.0)
                })
                .flatten()
        };
        let under = Cursor::At(Some(caret + 3));
        let in_box = Cursor::At(Some(caret));
        assert_eq!(look(under, 0), Some("unknown"), "the cursor under the box");
        assert_eq!(look(in_box, 1), Some("idle"), "the cursor stepped in");
        assert_eq!(look(in_box, 2), None, "the control: nothing moved");
    }

    /// Tier-1 for `ClaudeIdleAtComposer` (the live e2e's NEW-1, 2026-09-26,
    /// and its review's same-tab relaunch): over EVERY reachable state of the
    /// model, the shipping observer — one `agent_observe` of the screen the
    /// state names, under `program=claude`, with the terminal CURSOR measured
    /// on it — publishes the verdict the model's `Look` publishes there:
    /// `prompt` for the trust dialog, `idle` only for the NEW REPL drawn
    /// whole, neither for the shell's rows (before the dialog, after its
    /// press, or with no dialog) nor for the half-drawn REPL — in a new tab,
    /// and with the previous run's prompt box on the screen above the new
    /// launch line (`stale`). So `await agent idle` answers exactly where the
    /// model's `Type` is enabled, and the model proves a prompt typed there
    /// is never lost. The screens are the frames Claude Code 2.1.283 drew
    /// (aterm-phase's fixtures, each with its measured cursor; the one dialog
    /// with none measured is 2.1.280's, read with the cursor on none of its
    /// rows), each model screen looked at through EVERY measured frame of it:
    /// the fullscreen renderer (`LAUNCH_*`, `SHELL_MODE*`: `!` typed, the
    /// caret `!`, the REPL up and taking keys), the inline one in a new tab
    /// (`INLINE_*`: on the main grid under the launch line, 150x50, blank
    /// rows below — its whole REPL above the grid's last 40 rows), and the
    /// inline one relaunched in the same tab (`INLINE_RELAUNCH_*`). NEGATIVE
    /// CONTROLS: the reader before 2026-09-26 (`Buggy=1`) publishes idle on
    /// the shell's rows and the half-drawn REPL, the reader of 2026-09-26
    /// (`Buggy=2`, the box by its frame alone) on the relaunch's shell rows
    /// under the previous run's box — and the real observer disagrees with
    /// each there, and only there. RED: read by its frame alone (the cursor
    /// not given, the rule this branch had), every frame publishes what
    /// `Buggy=2` publishes: `idle` on that old box.
    #[test]
    fn claude_is_published_idle_exactly_where_the_model_types() {
        use aterm_phase::prompt::fixtures::{
            INLINE_RELAUNCH_BEFORE_REPL, INLINE_RELAUNCH_REPL_HALF_DRAWN,
            INLINE_RELAUNCH_REPL_READY, INLINE_RELAUNCH_TRUST, INLINE_REPL_HALF_DRAWN,
            INLINE_REPL_READY, INLINE_TRUST, LAUNCH_BEFORE_REPL, LAUNCH_REPL_HALF_DRAWN,
            LAUNCH_REPL_READY, SHELL_MODE, SHELL_MODE_DRAFT, TRUST, cursor, screen,
        };
        let model = aterm_spec::derive::claude_idle_at_composer_model();
        let old = aterm_spec::interp::with_buggy(&model, 1);
        let framed = aterm_spec::interp::with_buggy(&model, 2);
        // `screens[stale][screen]`: every measured frame of each.
        let screens: [[Vec<&str>; 4]; 2] = [
            [
                vec![LAUNCH_BEFORE_REPL],
                vec![TRUST, INLINE_TRUST],
                vec![LAUNCH_REPL_HALF_DRAWN, INLINE_REPL_HALF_DRAWN],
                vec![
                    LAUNCH_REPL_READY,
                    SHELL_MODE,
                    SHELL_MODE_DRAFT,
                    INLINE_REPL_READY,
                ],
            ],
            [
                vec![INLINE_RELAUNCH_BEFORE_REPL],
                vec![INLINE_RELAUNCH_TRUST],
                vec![INLINE_RELAUNCH_REPL_HALF_DRAWN],
                vec![INLINE_RELAUNCH_REPL_READY],
            ],
        ];
        let project = |word: &str| match word {
            "idle" => 1,
            "prompt" => 2,
            _ => 0,
        };
        let claude = Some("claude".to_string());
        let publish = |text: &str, cursor: Cursor| {
            let mut obs = super::StatusObserver::new(
                super::StatusPolicy::default(),
                Duration::from_millis(50),
            );
            let t0 = Instant::now();
            let generation = crate::control::ScreenGen { epoch: 0, seq: 1 };
            assert!(obs.agent_zone_wanted(1, generation, cursor, &claude, t0));
            let step = obs.agent_observe(
                1,
                generation,
                Some(screen(text)),
                cursor,
                claude.clone(),
                100,
                false,
                t0,
            );
            step.publish.expect("the zone was classified").0
        };
        let measured = |text: &str| Cursor::At(cursor(text).map(|(row, _)| row));
        // Every reachable state, breadth first.
        let mut seen = vec![model.init_state()];
        let mut next = 0;
        let mut looked = std::collections::BTreeSet::new();
        let mut disagreed = [
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
        ];
        while next < seen.len() {
            let state = seen[next].clone();
            next += 1;
            if model.action_enabled("Look", &state) {
                let at = usize::try_from(state["screen"]).expect("a screen");
                let stale = usize::try_from(state["stale"]).expect("stale");
                let modeled = model.successors("Look", &state).remove(0);
                for (frame, text) in screens[stale][at].iter().enumerate() {
                    let word = publish(text, measured(text));
                    assert_eq!(
                        project(word),
                        modeled["published"],
                        "stale {stale} screen {at} frame {frame}: the observer published `{word}`"
                    );
                    assert_eq!(
                        model.action_enabled("Type", &modeled),
                        word == "idle",
                        "stale {stale} screen {at} frame {frame}"
                    );
                    looked.insert((stale, at, frame));
                    for (bug, buggy) in [&old, &framed].into_iter().enumerate() {
                        let theirs = buggy.successors("Look", &state).remove(0)["published"];
                        if theirs != project(word) {
                            assert_eq!(theirs, 1, "Buggy={}: it said idle", bug + 1);
                            disagreed[bug].insert((stale, at));
                        }
                    }
                    // RED: the frame alone, the cursor not given.
                    let by_frame = publish(text, Cursor::Unknown);
                    let theirs = framed.successors("Look", &state).remove(0)["published"];
                    assert_eq!(
                        project(by_frame),
                        theirs,
                        "stale {stale} screen {at} frame {frame}: the frame rule is Buggy=2"
                    );
                }
            }
            for action in &model.actions {
                for s in model.successors(action.name, &state) {
                    if !seen.contains(&s) {
                        seen.push(s);
                    }
                }
            }
        }
        assert_eq!(
            looked.len(),
            screens.iter().flatten().map(Vec::len).sum::<usize>(),
            "every frame of every screen was looked at: {looked:?}"
        );
        assert_eq!(
            disagreed[0].iter().copied().collect::<Vec<_>>(),
            [(0, 0), (0, 2), (1, 0), (1, 2)],
            "NEGATIVE CONTROL: the old reader, refuted before the REPL"
        );
        assert_eq!(
            disagreed[1].iter().copied().collect::<Vec<_>>(),
            [(1, 0)],
            "NEGATIVE CONTROL: the box by its frame alone, refuted on the relaunch"
        );
    }

    /// THE DEPARTED AGENT'S LAST FRAME (2026-09-24): the look that first sees
    /// a new foreground group read `program=` from the timeline before this
    /// sweep's `note_foreground_group` forgot it — the name of the group that
    /// LEFT. Judged under it, an exited Claude Code's last frame published
    /// Claude's reader again, and the in-GUI host went on seeing the agent
    /// (the relaunch on exit missed exits that way). The zone of a moved
    /// group is judged under no name: `agent=-`, no reader. NEGATIVE CONTROL:
    /// the same rows in the SAME group are the agent's.
    #[test]
    fn a_group_that_left_is_never_judged_under_its_name() {
        let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
        let frame = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_OFFER);
        let mut exited = frame.clone();
        exited.push("$ ".to_string());
        let claude = Some("claude".to_string());
        let reader = |step: &super::AgentStep| step.publish.as_ref().map(|p| (p.0, p.3));
        for group_after in [200, 100] {
            let mut obs = super::StatusObserver::new(
                super::StatusPolicy::default(),
                Duration::from_millis(50),
            );
            let t0 = Instant::now();
            assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &claude, t0));
            let step = obs.agent_observe(
                1,
                g(1),
                Some(frame.clone()),
                Cursor::Unknown,
                claude.clone(),
                100,
                true,
                t0,
            );
            assert_eq!(
                reader(&step).and_then(|(_, r)| r),
                Some(aterm_phase::Program::Claude),
                "the agent, named"
            );
            let t1 = t0 + super::AGENT_MIN_INTERVAL;
            assert!(obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &claude, t1));
            let step = obs.agent_observe(
                1,
                g(2),
                Some(exited.clone()),
                Cursor::Unknown,
                claude.clone(),
                group_after,
                true,
                t1,
            );
            let got = reader(&step);
            if group_after == 100 {
                assert_eq!(
                    got.and_then(|(_, r)| r),
                    Some(aterm_phase::Program::Claude),
                    "NEGATIVE CONTROL: the same group is still the agent"
                );
            } else {
                assert_eq!(
                    got,
                    Some(("-", None)),
                    "the departed group's frame is no agent's"
                );
            }
        }
    }

    /// THE GATE HASHES THE ZONE THE VERDICT READS (the live run of
    /// 2026-09-26: Claude Code 2.1.283 in a fresh 149x62 pane). The pane
    /// idle at its top, then the folder-trust dialog drawn there: the last
    /// 40 rows of the SCREEN are blank on both, so a gate that hashed them
    /// passed over the dialog, and `agent=idle` stood while the hosted loop
    /// slept on it. The gate hashes [`crate::presence::live_zone`], the rows
    /// the verdict reads: the dialog is classified and published a trust
    /// prompt. NEGATIVE CONTROL: the dialog read again, nothing in the zone
    /// moved, is not classified again. The cursor is where Claude Code parks
    /// it: the idle prompt box's caret row, then the dialog's focused option
    /// (the measured row 17).
    #[test]
    fn a_dialog_above_a_blank_foot_passes_the_classifier_gate() {
        use aterm_phase::prompt::fixtures::{TRUST_FRESH_PANE, composer, rows, screen};
        let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
        let claude = Some("claude".to_string());
        let word = |step: &super::AgentStep| step.publish.as_ref().map(|p| (p.0, p.1.clone()));
        let mut idle = rows(&["\u{23fa} Done.", ""]);
        idle.extend(composer("  ? for shortcuts"));
        idle.resize(62, String::new());
        let trust = screen(TRUST_FRESH_PANE);
        assert_eq!(trust.len(), 62);
        assert_eq!(idle[22..], trust[22..], "the same blank foot");
        let mut obs =
            super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
        let t0 = Instant::now();
        let caret = Cursor::At(idle.iter().position(|r| r.starts_with('❯')));
        let focus = Cursor::At(trust.iter().position(|r| r == " ❯ No, exit"));
        assert_eq!(focus, Cursor::At(Some(17)), "the measured cursor");
        let step = obs.agent_observe(1, g(1), Some(idle), caret, claude.clone(), 100, false, t0);
        assert_eq!(word(&step), Some(("idle", None)));
        let t1 = t0 + super::AGENT_MIN_INTERVAL;
        let step = obs.agent_observe(
            1,
            g(2),
            Some(trust.clone()),
            focus,
            claude.clone(),
            100,
            false,
            t1,
        );
        assert!(step.reading_changed, "{:?}", word(&step));
        assert_eq!(word(&step), Some(("prompt", Some("trust".to_string()))));
        let t2 = t1 + super::AGENT_MIN_INTERVAL;
        let step = obs.agent_observe(1, g(3), Some(trust), focus, claude, 100, false, t2);
        assert_eq!(word(&step), None, "the zone did not move");
    }

    /// `history_lost=` (2026-09-26): the history lines this session's update
    /// handoffs could not carry, summed — what its adoption brought on its
    /// record plus every import that failed here — and `0` for a session that
    /// lost none. The update's total rides one band row as well
    /// (`App::settle_handoff_history`), never the log alone. NEGATIVE CONTROL:
    /// an update that carried every line posts no row.
    #[cfg(unix)] // App::settle_handoff_history is the unix handoff's
    #[test]
    fn the_status_record_counts_the_history_an_update_could_not_carry() {
        use crate::handoff_history::{ImportReport, record_reports};
        let (mut app, sid) = app_with_stub();
        let record = app.session_status_record(sid).expect("live");
        assert!(
            record.contains(" history_lost=0 integration_rev=- gen="),
            "{record}"
        );
        let whole = [ImportReport {
            session: sid,
            imported: 5000,
            dropped: 0,
            failed_lines: 0,
            failed: None,
            cleared: false,
        }];
        record_reports(&app.store, &whole);
        app.settle_handoff_history(&whole);
        assert!(
            !app.has_live_message("Couldn't carry all scrollback"),
            "nothing was lost: no row"
        );
        // The adoption's carried count, then an import that failed here.
        app.store.write().unwrap().add_history_lost(sid, 1200);
        let failed = [ImportReport {
            session: sid,
            imported: 0,
            dropped: 1200,
            failed_lines: 34,
            failed: Some("the sidecar arrived with a sha other than its stamp".into()),
            cleared: false,
        }];
        record_reports(&app.store, &failed);
        app.settle_handoff_history(&failed);
        let record = app.session_status_record(sid).expect("live");
        assert!(
            record.contains(" history_lost=1234 integration_rev=- gen="),
            "{record}"
        );
        assert!(app.has_live_message("Couldn't carry all scrollback"));
    }

    /// THE REGISTRY'S FROZEN MARK IS LOWERED BY A MEASUREMENT, NEVER RAISED
    /// (gap audit 2026-09-24: a tab healed by the live upgrade's relaunch line
    /// read `path=frozen` through seven handoffs, because nothing ever lowered
    /// the mark; review of 2026-09-25: a one-off `PATH=` override then RAISED
    /// it, and the band counted that tab "from before this update"). A live
    /// reading lowers it — so the handoff's carry and the managed record's
    /// count lose the tab. A frozen reading from one job, and then from a
    /// second, leaves an unmarked tab unmarked: one reads `unconfirmed`, two
    /// read `frozen` as measured. NEGATIVE CONTROL: with no reading, the
    /// carried mark stands through a sweep.
    #[test]
    fn a_measured_path_lowers_the_carried_frozen_mark_and_never_raises_it() {
        use crate::session_status::program::PathVerdict;
        let (mut app, sid) = app_with_stub();
        app.store.write().unwrap().mark_frozen_path(sid);
        let t0 = Instant::now();
        let _ = app.observe_session_statuses(t0);
        assert!(
            app.store.read().unwrap().has_frozen_path(sid),
            "no reading: the mark stands"
        );
        let record = app.session_status_record(sid).expect("live");
        assert!(record.contains(" path=frozen "), "{record}");
        assert!(record.contains(" path_evidence=carried "), "{record}");
        let measure = |app: &App, verdict: PathVerdict| {
            let pooled = app.pool.get(sid).unwrap();
            let mut tl = pooled.ctx.timeline.lock().unwrap();
            tl.note_foreground_group(4242);
            tl.set_program(4242, Some("claude".into()));
            tl.set_leader(4242, Some("foreign"), Some(verdict));
        };
        measure(&app, PathVerdict::Live);
        let _ = app.observe_session_statuses(t0 + Duration::from_millis(300));
        assert!(
            !app.store.read().unwrap().has_frozen_path(sid),
            "a live reading lowers the mark"
        );
        let record = app.session_status_record(sid).expect("live");
        assert!(record.contains(" path=live program=claude "), "{record}");
        assert!(record.contains(" path_evidence=measured:"), "{record}");
        assert!(record.contains(" copy=foreign upgrade=- "), "{record}");
        // One job's frozen reading (a `PATH=/usr/bin:/bin tool` override
        // reads exactly so) is not the shell's: not raised, and `path=` stays.
        measure(&app, PathVerdict::Frozen);
        let _ = app.observe_session_statuses(t0 + Duration::from_millis(600));
        assert!(
            !app.store.read().unwrap().has_frozen_path(sid),
            "one job's frozen reading raises nothing"
        );
        let record = app.session_status_record(sid).expect("live");
        assert!(record.contains(" path=live program=claude "), "{record}");
        assert!(record.contains(" path_evidence=unconfirmed:"), "{record}");
        // A SECOND job agreeing settles it as the shell's: `path=frozen`,
        // measured — and still no mark (it is not "from before this update").
        {
            let pooled = app.pool.get(sid).unwrap();
            let mut tl = pooled.ctx.timeline.lock().unwrap();
            tl.note_foreground_group(4343);
            tl.set_program(4343, Some("claude".into()));
            tl.set_leader(4343, Some("foreign"), Some(PathVerdict::Frozen));
        }
        let _ = app.observe_session_statuses(t0 + Duration::from_millis(900));
        assert!(!app.store.read().unwrap().has_frozen_path(sid));
        let record = app.session_status_record(sid).expect("live");
        assert!(record.contains(" path=frozen program=claude "), "{record}");
        assert!(record.contains(" path_evidence=measured:"), "{record}");
    }

    /// Tier-1 for `ProgramResolutionRetry`: a failed process-table lookup on a
    /// motionless Claude screen still owns a wake and another resolution.
    /// This drives the shipping observer's request bookkeeping, timer and
    /// guard; the model's old-code mutant is the negative control.
    #[test]
    fn an_unnamed_static_group_retries_promptly_then_backs_off_without_spinning() {
        let model = aterm_spec::derive::program_resolution_retry_model();
        let mut modeled = model.init_state();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut old = buggy.init_state();
        let t0 = Instant::now();
        let floor = Duration::from_millis(100);
        let id = 7;
        let pgid = 42;
        let generation = crate::control::ScreenGen { epoch: 0, seq: 1 };
        let policy = StatusPolicy::default();
        let mut observer = StatusObserver::new(policy, floor);
        observer.sessions.insert(
            id,
            SessionSlot {
                fsm: StatusFsm::new(policy, t0),
                next_due: t0 + floor,
                revision: 1,
                lifecycle: None,
            },
        );

        // Record the first group request before the zone read to exercise
        // the request helper independently. Production creates the watch
        // earlier via `agent_zone_wanted`; either order retains its deadline.
        assert!(observer.program_due(id, true, None, generation, t0));
        observer.note_program_request(id, pgid, t0);
        assert!(model.fire("Start", &mut modeled));
        assert!(buggy.fire("Start", &mut old));
        assert_eq!(observer.agents[&id].resolved_at, Some(t0));
        assert_eq!(
            i64::from(observer.agents[&id].resolve_attempts),
            modeled["attempts"]
        );
        assert!(!buggy.check_invariant("UnnamedGroupOwnsRetry", &old));
        observer.agent_observe(
            id,
            generation,
            Some(vec!["Claude Code approval box".into()]),
            Cursor::Unknown,
            None,
            pgid,
            true,
            t0,
        );
        assert_eq!(observer.next_wake(), Some(t0 + floor));

        // The one follow-up read sees the same frame. Its flag clears, but
        // the unresolved name keeps an independently bounded deadline.
        observer.sessions.get_mut(&id).unwrap().next_due = t0 + floor * 2;
        observer.agent_observe(
            id,
            generation,
            None,
            Cursor::Unknown,
            None,
            pgid,
            false,
            t0 + floor,
        );
        assert_eq!(observer.next_wake(), Some(t0 + Duration::from_millis(250)));
        assert!(!observer.program_due(
            id,
            false,
            None,
            generation,
            t0 + Duration::from_millis(249)
        ));
        let first_retry = t0 + Duration::from_millis(250);
        assert!(model.fire("Elapse", &mut modeled));
        assert!(model.fire("Decide", &mut modeled));
        assert!(buggy.fire("Elapse", &mut old));
        assert!(buggy.fire("Decide", &mut old));
        assert_eq!(modeled["retry_due"], 1);
        assert_eq!(old["retry_due"], 0, "old same-generation guard strands it");
        assert_eq!(
            observer.program_due(id, false, None, generation, first_retry),
            modeled["retry_due"] == 1
        );
        observer.note_program_request(id, pgid, first_retry);
        assert!(model.fire("Retry", &mut modeled));
        assert_eq!(
            i64::from(observer.agents[&id].resolve_attempts),
            modeled["attempts"]
        );

        // The second miss waits one second, then the third and every later
        // miss waits five seconds. Each resolution's single follow-up is
        // charged to the ordinary observation floor.
        observer.sessions.get_mut(&id).unwrap().next_due = first_retry + floor;
        observer.agent_observe(
            id,
            generation,
            None,
            Cursor::Unknown,
            None,
            pgid,
            true,
            first_retry,
        );
        assert_eq!(observer.next_wake(), Some(first_retry + floor));
        observer.sessions.get_mut(&id).unwrap().next_due = first_retry + floor * 2;
        observer.agent_observe(
            id,
            generation,
            None,
            Cursor::Unknown,
            None,
            pgid,
            false,
            first_retry + floor,
        );
        let second_retry = first_retry + Duration::from_secs(1);
        assert_eq!(observer.next_wake(), Some(second_retry));
        assert!(!observer.program_due(
            id,
            false,
            None,
            generation,
            second_retry - Duration::from_millis(1)
        ));
        assert!(model.fire("Elapse", &mut modeled));
        assert!(model.fire("Decide", &mut modeled));
        assert_eq!(
            observer.program_due(id, false, None, generation, second_retry),
            modeled["retry_due"] == 1
        );
        observer.note_program_request(id, pgid, second_retry);
        assert!(model.fire("Retry", &mut modeled));
        assert_eq!(observer.agents[&id].resolve_attempts, 3);
        observer.sessions.get_mut(&id).unwrap().next_due = second_retry + floor;
        observer.agent_observe(
            id,
            generation,
            None,
            Cursor::Unknown,
            None,
            pgid,
            true,
            second_retry,
        );
        observer.sessions.get_mut(&id).unwrap().next_due = second_retry + floor * 2;
        observer.agent_observe(
            id,
            generation,
            None,
            Cursor::Unknown,
            None,
            pgid,
            false,
            second_retry + floor,
        );
        assert_eq!(observer.next_wake(), Some(second_retry + PROGRAM_RECHECK));

        // A contended terminal can skip a deadline. The same past timer is
        // clamped to the charged next observation, never rearmed immediately.
        let late = second_retry + PROGRAM_RECHECK + Duration::from_secs(1);
        observer.note_skipped(id, late);
        assert_eq!(observer.next_wake(), Some(late + floor));

        // A later successful name on the same static frame identifies Claude
        // and disarms every unresolved retry.
        let named_at = late + floor;
        let named = observer.agent_observe(
            id,
            generation,
            Some(vec!["Claude Code approval box".into()]),
            Cursor::Unknown,
            Some("claude".into()),
            pgid,
            false,
            named_at,
        );
        assert_eq!(
            named.publish.and_then(|(_, _, _, reader)| reader),
            Some(aterm_phase::Program::Claude)
        );
        assert!(model.fire("Resolve", &mut modeled));
        assert_eq!(
            i64::from(observer.agents[&id].resolve_attempts),
            modeled["attempts"]
        );
        assert_eq!(observer.next_wake(), None);
        assert!(!observer.program_due(
            id,
            false,
            Some("claude"),
            generation,
            named_at + PROGRAM_RECHECK * 2
        ));

        // The same group can exec again. Once a name was known, its next
        // transient miss starts at the fast 250 ms retry instead of inheriting
        // the old five-second backoff. A departed or retired group owns none.
        let newer = crate::control::ScreenGen { epoch: 0, seq: 2 };
        let exec_at = named_at + PROGRAM_RECHECK * 2;
        assert!(observer.program_due(id, false, Some("claude"), newer, exec_at));
        observer.note_program_request(id, pgid, exec_at);
        observer.agent_observe(
            id,
            newer,
            Some(vec!["still named".into()]),
            Cursor::Unknown,
            Some("claude".into()),
            pgid,
            true,
            exec_at,
        );
        observer.agent_observe(
            id,
            newer,
            Some(vec!["unnamed".into()]),
            Cursor::Unknown,
            None,
            pgid,
            false,
            exec_at,
        );
        observer.sessions.get_mut(&id).unwrap().next_due = exec_at + floor;
        assert_eq!(
            observer.next_wake(),
            Some(exec_at + Duration::from_millis(250))
        );
        observer.agent_observe(
            id,
            newer,
            None,
            Cursor::Unknown,
            None,
            -1,
            false,
            exec_at + floor,
        );
        assert_eq!(observer.next_wake(), None);
        observer.retire(id);
        assert_eq!(observer.next_wake(), None);
    }

    /// Tier-1 for the named-shell side of `ProgramResolutionRetry`. A first
    /// lookup can catch `sh` before its same-PGID exec into Claude. One short
    /// confirmation may still see `sh`; a later single Claude frame must arm
    /// another bounded lookup even though that frame then stays still.
    #[test]
    fn a_named_shell_rechecks_once_after_a_later_static_claude_frame() {
        let model = aterm_spec::derive::program_resolution_retry_model();
        let mut modeled = model.init_state();
        let t0 = Instant::now();
        let floor = Duration::from_millis(100);
        let id = 8;
        let pgid = 43;
        let first = crate::control::ScreenGen { epoch: 0, seq: 1 };
        let later = crate::control::ScreenGen { epoch: 0, seq: 2 };
        let policy = StatusPolicy::default();
        let mut observer = StatusObserver::new(policy, floor);
        observer.sessions.insert(
            id,
            SessionSlot {
                fsm: StatusFsm::new(policy, t0),
                next_due: t0 + floor,
                revision: 1,
                lifecycle: None,
            },
        );

        observer.note_program_request(id, pgid, t0);
        assert!(model.fire("Start", &mut modeled));
        observer.agent_observe(
            id,
            first,
            Some(vec!["launching".into()]),
            Cursor::Unknown,
            Some("claude".into()),
            pgid,
            true,
            t0,
        );
        assert!(
            !observer.agents[&id].name_confirmed,
            "the group-change sweep's old Claude name is not this group's answer"
        );
        // The first later sweep sees the transient shell name, not the stale
        // name sampled before `note_foreground_group` on the launch sweep.
        let shell_at = t0 + floor;
        observer.agent_observe(
            id,
            first,
            Some(vec!["sh launching Claude".into()]),
            Cursor::Unknown,
            Some("sh".into()),
            pgid,
            false,
            shell_at,
        );
        assert!(model.fire("ResolveShell", &mut modeled));
        let first_confirm = shell_at + PROGRAM_NAME_CONFIRM;
        assert_eq!(modeled["confirm_armed"], 1);
        assert_eq!(observer.next_wake(), Some(first_confirm));
        assert!(!observer.program_due(
            id,
            false,
            Some("sh"),
            first,
            first_confirm - Duration::from_millis(1)
        ));
        assert!(model.fire("Elapse", &mut modeled));
        assert_eq!(
            observer.program_due(id, false, Some("sh"), first, first_confirm),
            model.action_enabled("ConfirmShell", &modeled)
        );
        observer.note_program_request(id, pgid, first_confirm);
        assert!(model.fire("ConfirmShell", &mut modeled));
        observer.agent_observe(
            id,
            first,
            None,
            Cursor::Unknown,
            Some("sh".into()),
            pgid,
            true,
            first_confirm,
        );
        observer.sessions.get_mut(&id).unwrap().next_due = first_confirm + floor;
        observer.agent_observe(
            id,
            first,
            None,
            Cursor::Unknown,
            Some("sh".into()),
            pgid,
            false,
            first_confirm + floor,
        );
        assert_eq!(modeled["confirm_armed"], 0);
        assert_eq!(observer.next_wake(), None, "stable sh stops polling");

        // Claude execs at t=3 s and draws once. At this point the 5 s
        // screen-moved guard is still closed; the deferred deadline is the
        // only way to name the now-static new program.
        let moved_at = t0 + Duration::from_secs(3);
        assert!(!observer.program_due(id, false, Some("sh"), later, moved_at));
        observer.agent_observe(
            id,
            later,
            Some(vec!["Claude Code approval box".into()]),
            Cursor::Unknown,
            Some("sh".into()),
            pgid,
            false,
            moved_at,
        );
        let before_move = modeled.clone();
        assert!(model.fire("MoveNamed", &mut modeled));
        let old_move = aterm_spec::interp::with_buggy(&model, 1)
            .successors("MoveNamed", &before_move)[0]
            .clone();
        assert_eq!(old_move["confirm_armed"], 0, "old guard loses this frame");
        observer.sessions.get_mut(&id).unwrap().next_due = moved_at + floor;
        assert_eq!(observer.next_wake(), Some(moved_at + floor));
        observer.sessions.get_mut(&id).unwrap().next_due = moved_at + floor * 2;
        observer.agent_observe(
            id,
            later,
            None,
            Cursor::Unknown,
            Some("sh".into()),
            pgid,
            false,
            moved_at + floor,
        );
        let later_confirm = first_confirm + PROGRAM_NAMED_RECHECK_FLOOR;
        assert_eq!(later_confirm, t0 + Duration::from_millis(4_600));
        assert_eq!(observer.next_wake(), Some(later_confirm));
        assert_eq!(modeled["confirm_armed"], 1);
        assert!(!observer.program_due(
            id,
            false,
            Some("sh"),
            later,
            later_confirm - Duration::from_millis(1)
        ));
        assert!(model.fire("Elapse", &mut modeled));
        assert_eq!(
            observer.program_due(id, false, Some("sh"), later, later_confirm),
            model.action_enabled("ConfirmShell", &modeled)
        );
        observer.note_program_request(id, pgid, later_confirm);
        assert!(model.fire("ConfirmShell", &mut modeled));
        observer.agent_observe(
            id,
            later,
            None,
            Cursor::Unknown,
            Some("sh".into()),
            pgid,
            true,
            later_confirm,
        );
        observer.sessions.get_mut(&id).unwrap().next_due = later_confirm + floor;
        assert_eq!(observer.next_wake(), Some(later_confirm + floor));
        let named = observer.agent_observe(
            id,
            later,
            Some(vec!["Claude Code approval box".into()]),
            Cursor::Unknown,
            Some("claude".into()),
            pgid,
            false,
            later_confirm + floor,
        );
        assert_eq!(
            named.publish.and_then(|(_, _, _, reader)| reader),
            Some(aterm_phase::Program::Claude)
        );
        assert!(model.fire("NameBecomesClaude", &mut modeled));
        assert_eq!(modeled["confirm_armed"], 0);
        assert_eq!(observer.next_wake(), None);

        // A later non-agent name can arm another deadline, but leaving the
        // foreground group cancels it even if the retired screen stays put.
        let third = crate::control::ScreenGen { epoch: 0, seq: 3 };
        observer.agent_observe(
            id,
            third,
            Some(vec!["sh again".into()]),
            Cursor::Unknown,
            Some("sh".into()),
            pgid,
            false,
            later_confirm + PROGRAM_RECHECK,
        );
        assert!(model.fire("AgentLeaves", &mut modeled));
        assert!(observer.agents[&id].deferred_name_recheck_at.is_some());
        assert_eq!(modeled["confirm_armed"], 1);
        observer.agent_observe(
            id,
            third,
            Some(vec!["prompt".into()]),
            Cursor::Unknown,
            None,
            -1,
            false,
            later_confirm + PROGRAM_RECHECK + floor,
        );
        assert!(model.fire("GroupLeaves", &mut modeled));
        assert_eq!(observer.agents[&id].deferred_name_recheck_at, None);
        assert_eq!(modeled["confirm_armed"], 0);
        assert_eq!(observer.next_wake(), None);
    }
    /// INT-4: a shell whose last line ends in `?` is not an agent — `agent=-`
    /// and no attention — while the SAME screen under `program=claude` is read
    /// as the question it is. Identity, not screen text, decides.
    #[test]
    fn a_shell_ending_in_a_question_mark_is_not_an_agent() {
        let (mut app, sid) = app_with_stub();
        let screen: Vec<String> = vec![
            "$ echo \"Checking whether the disk is full?\"; sleep 30".into(),
            "Checking whether the disk is full?".into(),
        ];
        paint(&app, sid, &screen);
        let t0 = Instant::now();
        let changed = app.observe_session_statuses(t0);
        for id in changed {
            app.refresh_presence_session(id, false);
        }
        assert_eq!(published(&app, sid).word, "-");
        let record = app.session_status_record(sid).expect("live");
        assert!(
            record.contains(" agent=- agent_detail=- agent_rev=0 "),
            "{record}"
        );
        assert!(!record.contains("level=attention"), "{record}");
        assert!(record.contains(" why=- "), "{record}");

        // The same screen, the foreground program named `claude`: an agent,
        // asking. (The stub has no PTY, so the program is published by hand
        // exactly as the resolver would.)
        {
            let pooled = app.pool.get(sid).unwrap();
            let mut tl = pooled.ctx.timeline.lock().unwrap();
            assert!(tl.note_foreground_group(4242));
            tl.set_program(4242, Some("claude".into()));
        }
        let changed = app.observe_session_statuses(t0 + Duration::from_millis(300));
        for id in changed {
            app.refresh_presence_session(id, false);
        }
        let p = published(&app, sid);
        assert_eq!(p.program.as_deref(), Some("claude"));
        assert_eq!(
            p.word, "question",
            "a program change re-reads an unchanged zone"
        );
        let record = app.session_status_record(sid).expect("live");
        assert!(
            record.contains(" program=claude agent=question "),
            "{record}"
        );
        assert!(record.contains(" why=question "), "{record}");
    }

    /// ACT-4: an adopted, checkpointed session whose handoff CARRIED its
    /// shell-integration nonce still takes its shell's marks — fed `633;E;claude
    /// --resume` then `133;C`, it reads `detail=claude` — and one whose
    /// handoff did not stays `integration=degraded` with its marks dropped
    /// (the requirement is never cleared as a fallback).
    #[test]
    fn an_adopted_session_with_its_carried_nonce_reads_its_command() {
        use aterm_core::terminal::{ShellIntegrationPosture, Terminal};
        let nonce = [0x3Cu8; 32];
        let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let mut outgoing = Terminal::new(24, 80);
        outgoing.authorize_shell_integration(nonce);
        outgoing.set_require_shell_integration_nonce(true);
        outgoing.process(format!("\x1b]133;A;id={hex}\x07$ ").as_bytes());
        let carried = outgoing.checkpoint_carry(0).expect("Ground");

        let feed = format!(
            "\x1b]133;D;0;id={hex}\x07\x1b]133;A;id={hex}\x07$ \x1b]633;E;claude --resume;id={hex}\x07\
             \x1b]133;B;id={hex}\x07claude --resume\n\x1b]133;C;id={hex}\x07"
        );
        let mut adopted = Terminal::new(24, 80);
        adopted.restore_checkpoint(&carried);
        crate::spawn::authorize_adopted_shell_nonce(&mut adopted, &carried, 1);
        assert_eq!(
            adopted.shell_integration_posture(),
            ShellIntegrationPosture::On
        );
        adopted.process(feed.as_bytes());
        assert_eq!(super::executing_detail(&adopted).as_deref(), Some("claude"));

        // NEGATIVE CONTROL: the same adopt without the carried nonce (a parent
        // that predates the carry) drops every mark and says so.
        let mut uncarried = carried.clone();
        uncarried.shell_integration_nonce = None;
        let mut blind = Terminal::new(24, 80);
        blind.restore_checkpoint(&uncarried);
        crate::spawn::authorize_adopted_shell_nonce(&mut blind, &uncarried, 2);
        assert_eq!(
            blind.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
        assert!(blind.is_require_shell_integration_nonce(), "never cleared");
        blind.process(feed.as_bytes());
        assert_eq!(super::executing_detail(&blind), None);
    }

    /// The herald's posts on this thread since the last call, drained.
    fn take_posts() -> Vec<crate::status_item::HeraldNotice> {
        crate::app_tabs::HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()))
    }

    /// One sweep plus the presence fan-out the event loop runs after it.
    fn sweep(app: &mut App, t: Instant) {
        for id in app.observe_session_statuses(t) {
            app.refresh_presence_session(id, false);
        }
    }

    fn looking_at(app: &App, sessions: &[u64]) {
        let mut set = app.notify_suppress.lock().unwrap();
        set.clear();
        set.extend(sessions.iter().copied());
    }

    /// THE E2E PROBE OF 2026-09-25 (W1): a trust dialog drawn at the top of
    /// a 45-row pane had its title above the live zone's 40 rows, so the
    /// server published `agent_detail=other` and the menu bar said "other
    /// approval" — and it said so from the raw verdict before the window's
    /// supervisor had taken the session's claim, then again from the
    /// supervisor's own escalation: two notifications for one box. The zone
    /// that cuts a box is read again whole (the verdict names `trust`), and a
    /// session the host supervises raises no verdict row, claim or not — its
    /// box is the host's to answer or escalate. NEGATIVE CONTROL: with no host
    /// supervising, the verdict's row is there.
    #[test]
    fn a_box_the_zone_cuts_is_read_whole_and_a_hosted_agent_raises_no_row() {
        let (mut app, sid) = app_with_stub();
        app.headless = false;
        let fixture = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::TRUST);
        let dialog = &fixture[8..=23];
        assert!(dialog[1].contains("Accessing workspace"), "{dialog:?}");
        {
            let pooled = app.pool.get(sid).expect("pooled");
            let mut t = crate::term_lock(&pooled.term);
            t.resize(45, 120);
            let mut bytes = String::from("\x1b[H\x1b[2J");
            for (i, row) in dialog.iter().enumerate() {
                bytes.push_str(&format!("\x1b[{};1H{row}", i + 1));
            }
            t.process(bytes.as_bytes());
        }
        let _ = take_posts();
        looking_at(&app, &[]);
        sweep(&mut app, Instant::now());
        let agent = published(&app, sid);
        assert_eq!(agent.word, "prompt", "{agent:?}");
        assert!(
            agent
                .detail
                .as_deref()
                .is_some_and(|d| d.starts_with("trust")),
            "{agent:?}"
        );
        let (hosted, bare) = {
            let store = app.store.read().unwrap();
            let handle = store.by_local(sid).expect("the session");
            (
                App::status_session_row(handle, true),
                App::status_session_row(handle, false),
            )
        };
        assert!(hosted.supervised, "{hosted:?}");
        assert_eq!(crate::status_item::escalation(&hosted), None);
        // NEGATIVE CONTROL: no host supervising — the verdict's row.
        assert!(!bare.supervised);
        assert!(crate::status_item::escalation(&bare).is_some());
    }

    /// ONE PLACE FOR ANDREW, end to end on the measured rm box: the sweep
    /// publishes `agent=prompt`, and the presence fan-out raises exactly one
    /// menu row naming the tab, the kind and the command, and exactly one
    /// notification. Re-classifying the same box (its ticking row moves the
    /// zone hash) posts nothing more; a new box while the human is looking
    /// at the tab posts nothing but still lists the row; a headless instance
    /// posts nothing at all.
    #[test]
    fn a_box_raises_one_menu_row_and_one_notification_per_transition() {
        let (mut app, sid) = app_with_stub();
        app.headless = false;
        {
            use crate::session_timeline::{MetaEdit, MetaField, write_session_meta};
            let ctx = app.pool.get(sid).expect("pooled").ctx.clone();
            write_session_meta(&ctx, MetaField::Title, MetaEdit::Set("build")).expect("title");
        }
        let _ = take_posts();
        looking_at(&app, &[]);
        let step = Duration::from_millis(300);
        let mut t = Instant::now();
        for secs in 0..2 {
            paint(&app, sid, &busy_screen(secs));
            sweep(&mut app, t);
            t += step;
        }
        assert!(take_posts().is_empty(), "a busy agent needs nobody");
        assert!(app.operator_fleet_glance().warnings.is_empty());

        paint(&app, sid, &box_screen(1));
        sweep(&mut app, t);
        t += step;
        let posts = take_posts();
        assert_eq!(posts.len(), 1, "exactly one notification: {posts:?}");
        assert_eq!(posts[0].session, sid);
        assert_eq!(posts[0].title, "aterm \u{b7} approval waiting");
        assert!(
            posts[0]
                .body
                .contains("bash S=$PWD/tmp; for p in a b; do set -- $p; rm -rf"),
            "the body names the command: {}",
            posts[0].body
        );
        let glance = app.operator_fleet_glance();
        assert_eq!(glance.warnings.len(), 1, "{:?}", glance.warnings);
        let (row_sid, label) = &glance.warnings[0];
        assert_eq!(*row_sid, sid, "a click on the row focuses this tab");
        assert!(
            label.starts_with("\u{26a0} build: bash S=$PWD/tmp;"),
            "`<tab title>: <kind> <command>`: {label}"
        );
        assert!(
            posts[0].body.starts_with("build: bash "),
            "{}",
            posts[0].body
        );
        let rev = published(&app, sid).rev;

        // The same box re-read: the ticking row moves the zone hash, so the
        // sweep classifies it again — the same verdict, the same rev.
        paint(&app, sid, &box_screen(2));
        sweep(&mut app, t);
        t += step;
        assert_eq!(
            published(&app, sid).rev,
            rev,
            "the same box is the same rev"
        );
        app.refresh_presence_session(sid, false);
        assert!(take_posts().is_empty(), "no second notification");
        assert_eq!(
            app.presence.herald.last_quiet(),
            Some(crate::status_item::HeraldQuiet::Same)
        );
        assert_eq!(app.operator_fleet_glance().warnings.len(), 1);

        // The box is answered, and a NEW one comes up while the human is
        // looking at this tab in the active app: listed, not announced.
        paint(&app, sid, &busy_screen(9));
        sweep(&mut app, t);
        t += step;
        assert!(app.operator_fleet_glance().warnings.is_empty(), "row gone");
        looking_at(&app, &[sid]);
        paint(&app, sid, &box_screen(3));
        sweep(&mut app, t);
        assert!(published(&app, sid).rev > rev, "a new transition");
        assert!(take_posts().is_empty(), "the human is looking at it");
        assert_eq!(
            app.presence.herald.last_quiet(),
            Some(crate::status_item::HeraldQuiet::Looking)
        );
        assert_eq!(app.operator_fleet_glance().warnings.len(), 1);

        // NEGATIVE CONTROL: the same first transition in a headless instance
        // reaches the published verdict and posts nothing.
        let (mut headless, hsid) = app_with_stub();
        assert!(headless.headless);
        looking_at(&headless, &[]);
        let mut t = Instant::now();
        paint(&headless, hsid, &busy_screen(0));
        sweep(&mut headless, t);
        t += step;
        paint(&headless, hsid, &box_screen(1));
        sweep(&mut headless, t);
        assert_eq!(published(&headless, hsid).word, "prompt");
        assert!(take_posts().is_empty(), "headless never notifies");
    }

    /// A SUPERVISOR THAT DIES (review major): its `ttl=` claim lapses with a
    /// box up and nothing writes — yet the human must hear. The presence
    /// timer wakes at the expiry, removes the claim with a `meta-change
    /// field=supervisor value=-` record (the `EVENT meta` push), and the box
    /// posts exactly one notice. NEGATIVE CONTROL: while the claim is live the
    /// same box posts nothing, and a tick before the expiry changes nothing.
    #[test]
    fn a_lapsed_supervisor_claim_hands_its_box_to_the_human() {
        let (mut app, sid) = app_with_stub();
        app.headless = false;
        let ctx = app.pool.get(sid).expect("pooled").ctx.clone();
        let _ = take_posts();
        looking_at(&app, &[]);
        // Long enough that the negative controls below — a presence refresh,
        // two debug-build paints and sweeps — finish inside it on a loaded
        // gate: at 120 ms they could outlast the claim and read a correct lapse
        // as a notice posted while supervised (the load-sensitive test audit
        // of 2026-09-27). The lapse is then waited for, not slept for.
        let ttl = Duration::from_secs(2);
        let now_us = crate::metrics::now_us();
        assert_eq!(
            crate::session_timeline::claim_supervisor(
                &ctx,
                "sup",
                None,
                Some(now_us + ttl.as_micros() as u64),
                now_us
            ),
            Ok(true)
        );
        // The `meta set` arm's `Wake::MetaChanged` refresh.
        app.refresh_presence_session(sid, false);
        let step = Duration::from_millis(300);
        let mut t = Instant::now();
        paint(&app, sid, &busy_screen(0));
        sweep(&mut app, t);
        t += step;
        paint(&app, sid, &box_screen(1));
        sweep(&mut app, t);
        assert_eq!(published(&app, sid).word, "prompt");
        assert!(take_posts().is_empty(), "NEGATIVE CONTROL: supervised");
        let deadline = app.presence_deadline(Instant::now()).expect("armed");
        assert!(deadline <= Instant::now() + ttl + Duration::from_millis(5));
        let _ = app.presence_tick(Instant::now());
        assert!(take_posts().is_empty(), "NEGATIVE CONTROL: not lapsed yet");

        let lapsed_by = Instant::now() + ttl + Duration::from_secs(10);
        let posts = loop {
            std::thread::sleep(Duration::from_millis(20));
            let _ = app.presence_tick(Instant::now());
            let posts = take_posts();
            if !posts.is_empty() {
                break posts;
            }
            assert!(
                Instant::now() < lapsed_by,
                "the lapsed claim never handed its box to the human"
            );
        };
        assert_eq!(posts.len(), 1, "exactly one notice: {posts:?}");
        assert_eq!(posts[0].session, sid);
        let meta_events: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.clone())
            .collect();
        assert_eq!(
            meta_events.last().map(String::as_str),
            Some("field=supervisor value=-"),
            "{meta_events:?}"
        );
        assert!(ctx.meta.lock().unwrap().supervisor_expiry().is_none());
        let _ = app.presence_tick(Instant::now());
        assert!(take_posts().is_empty(), "announced once");
    }
}

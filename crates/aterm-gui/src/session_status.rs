// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session status: the pure classifier ([`StatusFsm`]), the per-session observer that budgets
//! it ([`StatusObserver`]), and the App sweep that gathers its evidence under `try_lock`.
//!
//! The classifier holds no terminal handle, takes no lock and reads no clock: it maps
//! extracted evidence and its previous state to a [`Status`] at an injected instant, so its
//! rules are tested without a GUI, a PTY or timing. Two rules are load-bearing:
//!
//! Two rules from the RFC are load-bearing here and are asserted by tests:
//!
//! The activity primitive is the pair `(is_alternate_screen, content_seq)`: the two grids keep
//! independent counters, so an alt-screen transition is a resync, not activity.

use std::time::{Duration, Instant};

#[path = "session_program.rs"]
pub(crate) mod program;

/// Screen and cursor changes re-read one session's agent zone at most this often (4 Hz),
/// whatever the observation interval. A changed program keeps its own classification clock.
pub(crate) const AGENT_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// The whole-screen fingerprint and only the final `tail_rows` rows of a
/// screen's published text ([`crate::frame_memo::ScreenText`]): the fingerprint
/// is the text's `hash=`, exactly the `status hash=` of the same screen, and the
/// rows are copied out of the text with no lock held. A parity test spans
/// Unicode, resize and alternate-screen transitions.
pub(crate) fn screen_agent_frame(
    text: &crate::frame_memo::ScreenText,
    tail_rows: usize,
) -> (u64, Vec<String>) {
    let first = text.row_count().saturating_sub(tail_rows);
    let rows = text.rows().skip(first).map(str::to_string).collect();
    (text.fnv(), rows)
}

/// How often a foreground group whose screen keeps moving is re-named, to catch an `exec` in
/// place (`cd ~/ay && exec claude` keeps the process group).
pub(crate) const PROGRAM_RECHECK: Duration = Duration::from_secs(5);
/// One short confirmation after a newly named non-agent program: an interpreter can still be in
/// the same-pgid exec window before Claude.
const PROGRAM_NAME_CONFIRM: Duration = Duration::from_millis(500);
/// After the first confirmation, a later screen move owns one delayed recheck, at least this
/// long after the last request: under five seconds in all, with no polling of a stable screen.
const PROGRAM_NAMED_RECHECK_FLOOR: Duration = Duration::from_secs(4);

/// The retry delay for an unnamed foreground program: prompt at first, so a failed read does
/// not leave a Claude screen unnamed, then the ordinary five-second cadence.
fn unresolved_program_recheck(attempts: u8) -> Duration {
    match attempts {
        0 | 1 => Duration::from_millis(250),
        2 => Duration::from_secs(1),
        _ => PROGRAM_RECHECK,
    }
}

/// FNV-1a 64 over the classified rows and the cursor: the hash the agent verdict is re-derived
/// on, so a content move outside the zone costs no classification. The cursor is in it because
/// Claude Code's `idle` is the prompt box holding it ([`crate::presence::Cursor`]).
fn zone_hash(rows: &[String], cursor: crate::presence::Cursor) -> u64 {
    let mut h = aterm_hash::Fnv1a64::new();
    for row in rows {
        h.write(row.as_bytes());
        h.write(b"\n");
    }
    h.write(format!("{cursor:?}").as_bytes());
    h.write(b"\n");
    h.finish()
}

/// One session's agent-verdict watch: what the sweep last looked at, and the
/// reading it published.
#[derive(Debug, Default)]
struct AgentWatch {
    /// The screen generation the live zone was last read at: a screen switch can land the new
    /// grid on the old grid's content seq.
    seq_seen: Option<crate::control::ScreenGen>,
    /// The cursor the live zone was last read with: its moving alone warrants a read (Claude
    /// Code's `idle` is the prompt box that holds it).
    cursor_seen: Option<crate::presence::Cursor>,
    /// [`zone_hash`] of the rows last classified.
    zone_hash: Option<u64>,
    /// When the live zone was last read, hash changed or not: the [`AGENT_MIN_INTERVAL`] floor
    /// bounds terminal work.
    read_at: Option<Instant>,
    /// When the verdict was last classified; floors a new program's re-read.
    classified_at: Option<Instant>,
    /// The program the last classification ran under; a different one forces
    /// a re-read even over an unchanged zone.
    program_seen: Option<Option<String>>,
    /// Look once more at the next due observation: the content moved at this one (or the floor
    /// deferred a read), so a final frame drawn just after a look is read within one interval.
    followup: bool,
    /// The latest output wake ([`StatusObserver::note_output`]). A follow-up is owed only once
    /// output has been quiet for one interval: while it arrives, each wake's own sweep looks,
    /// and a timer would only add a 4 Hz poll during repaints that do not move the grid.
    output_at: Option<Instant>,
    /// The foreground group last seen (`-1` when unknowable).
    pgid: i32,
    /// When a program resolution was last requested for this session.
    resolved_at: Option<Instant>,
    /// Requests in this foreground group, capped at three: the unresolved retry runs at
    /// 250 ms, 1 s, then 5 s.
    resolve_attempts: u8,
    /// A confirmation deadline after a newly seen non-agent name or a later screen move: a
    /// transient `sh` can become Claude without another screen change.
    deferred_name_recheck_at: Option<Instant>,
    /// The short first confirmation ran; later screen moves use the named-program floor.
    name_confirmed: bool,
    /// The foreground group in which the agent's own screen identified it (Claude Code by its
    /// frame, Codex by its composer), and the agent: it stays identified until the group leaves
    /// the foreground, since a box can hide the composer.
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
    /// What the screen said of the turn, for the session log (phase 3B).
    pub(crate) turn: crate::presence::TurnFacts,
    /// The verdict to publish on the session's timeline (`word`, raw
    /// `detail`, a prompt's host-side subject, and the agent its reader
    /// read — by name or by screen), when the zone was classified this
    /// observation.
    pub(crate) publish: Option<AgentPublish>,
    /// What the same screen counts, published with the verdict for the
    /// survey (`SessionTimeline::set_gauges`).
    pub(crate) gauges: crate::session_timeline::AgentGauges,
}

/// Current activity of a session; success or failure lives in [`Outcome`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No usable evidence, or evidence conflicts. A normal condition.
    #[default]
    Unknown,
    /// Session created; nothing observed yet.
    Starting,
    /// Shell at a prompt with no foreground job.
    Idle,
    /// A foreground job exists and is producing output (or just started).
    Running,
    /// A foreground job exists but shows no recent display activity. Not a claim that anyone
    /// is being awaited.
    Quiet,
    /// The PTY has ended. Pairs with [`Status::last_outcome`].
    Exited,
}

impl Phase {
    /// The wire spelling, shared by the `status` verb and the timeline. Stable.
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

/// Result of the last completed unit of work. It survives phase changes until the next unit
/// starts, so a failed command stays visible while the session is `Idle`.
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

    /// The wire spelling; the payload rides `exit_code=`/`signal=`.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Success => "success",
            Self::Failure { .. } => "failure",
            Self::Signal { .. } => "signal",
        }
    }

    /// The `exit_code=`/`signal=` values beside [`Self::as_str`], `-` when
    /// unset — shared by the `status` record and its timeline event.
    fn wire_fields(self) -> (String, String) {
        match self {
            Self::Failure { exit_code } => (exit_code.to_string(), "-".to_string()),
            Self::Signal { signal } => ("-".to_string(), signal.to_string()),
            Self::None | Self::Success => ("-".to_string(), "-".to_string()),
        }
    }
}

/// Ordinal, not a probability. A `status` reader treats `heuristic`/`unknown` (or `conflict`)
/// as unsettled.
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

/// Why the classifier reached its conclusion, so a consumer can tell a fact from an inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    ShellBlock,
    LifecycleExit,
    ForegroundJob,
    ContentActivity,
    OutputActivity,
    Stall,
    NoEvidence,
    /// A macOS consent wall is possible: `fs_consent` is not `covered` and the
    /// shell-integration cwd is under a protected root. Not a claim that a dialog is showing;
    /// never emitted without a known cwd.
    ConsentPrompt,
}

impl Reason {
    /// The wire spellings.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
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

/// A shell-integration block that completed (OSC 133;D): [`Evidence::completed_block`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompletedBlock {
    pub(crate) id: u64,
    /// `None` when the shell reported no code; read as unknown, never success.
    pub(crate) exit_code: Option<i32>,
    /// When the command started (its OSC 133 C, wall ms), if stamped: the
    /// session log rebuilds a run the sweep never saw running from it.
    pub(crate) started_ms: Option<u64>,
    /// When it ended (its OSC 133 D, wall ms), if stamped.
    pub(crate) ended_ms: Option<u64>,
}

/// A held pane's carried exit, as its successor shows it ([`crate::App::note_carried_exit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CarriedExit {
    /// The person had seen the failure it marks the tab for.
    seen: bool,
    /// The completed block the held engine's checkpoint carries, which every
    /// later sweep reads again ([`completed_block`]).
    block: Option<CompletedBlock>,
}

/// The outcome a command's exit code reports: a missing code is unknown,
/// never success.
const fn exit_outcome(exit_code: Option<i32>) -> Outcome {
    match exit_code {
        None => Outcome::None,
        Some(0) => Outcome::Success,
        Some(code) => Outcome::Failure { exit_code: code },
    }
}

/// How the session's PTY ended, when known: produced at the `Wake::Exit` edge by
/// [`crate::App::note_session_exit`] from a non-blocking `waitpid` on the zombie.
/// `Exited { exit_code: None }` is an adopted session with no exit witness
/// (`spawn::adopted_exit_watch`), a master gone unreadable, or a child already reaped; never
/// success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Exited { exit_code: Option<i32> },
    Signalled { signal: u8 },
}

/// One sample of display activity; `content_seq` is meaningful only paired with `alt_screen`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActivitySample {
    pub(crate) alt_screen: bool,
    pub(crate) content_seq: u64,
    /// The most recent PTY output, from the session's activity atomic.
    pub(crate) last_output: Option<Instant>,
}

/// Everything the classifier may look at; the caller assembles it (the only part that locks).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Evidence {
    pub(crate) shell: Option<ShellEvidence>,
    /// The block whose result the shell is reporting ([`completed_block`]). Its exit code is
    /// the outcome a prompt reports, and its id tells two failing commands apart, so a second
    /// failure re-arms a seen mark ([`FailureMark`]). `None` while a command runs.
    pub(crate) completed_block: Option<CompletedBlock>,
    pub(crate) lifecycle: Option<Lifecycle>,
    /// `tcgetpgrp`-derived: is a foreground job other than the shell running? No process name.
    pub(crate) foreground_job: Option<bool>,
    pub(crate) activity: ActivitySample,
}

/// Tunables. Bounds are enforced by the config layer, not here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StatusPolicy {
    /// A foreground job with no display activity for this long becomes `Quiet`.
    pub(crate) quiet_after: Duration,
    /// How long a candidate phase must persist before it is published, so spinners cannot flap
    /// the badge or flood the timeline. Exact-confidence transitions bypass it.
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
    /// Contradictory evidence was present (not the expected background job at a prompt).
    pub(crate) conflict: bool,
}

impl Status {
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

/// Per-session classifier state: only what hysteresis and activity detection require.
#[derive(Clone, Debug)]
pub(crate) struct StatusFsm {
    policy: StatusPolicy,
    published: Status,
    /// Prior activity identity, for movement detection.
    prior: Option<(bool, u64)>,
    /// When display content last actually moved.
    last_movement: Option<Instant>,
    /// The freshest PTY-output stamp any observation carried, folded by max: `Running` is
    /// sustained by movement or output, so [`StatusFsm::owed_wake`] arms off both clocks.
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

    /// Fold the running-command `detail` onto the published status (the chrome's
    /// `running <detail>`, [`summary_text`]). Separate from [`Self::classify`], which decides
    /// the phase only. Returns whether it moved.
    fn set_detail(&mut self, detail: Option<String>) -> bool {
        if self.published.detail == detail {
            return false;
        }
        self.published.detail = detail;
        true
    }

    /// The instant at which this session could publish a transition that NO new
    /// output would cause. Two exist: a candidate still serving its dwell, and
    /// `Running` aging into `Quiet`. Everything else is edge-driven — an `Idle`
    /// pane cannot become anything until bytes arrive — so it returns `None`
    /// and costs the event loop nothing.
    ///
    /// Each arm is the earliest instant [`StatusFsm::classify`] could change its verdict.
    /// `Running` lives on movement or output, so it retires off the fresher clock: a client
    /// that repaints identical frames (a Claude Code TUI) keeps output fresh while movement goes
    /// stale, and arming off movement alone would put every wake in the past and spin.
    fn owed_wake(&self) -> Option<Instant> {
        if let Some((_, first_seen)) = &self.pending {
            return Some(*first_seen + self.policy.dwell);
        }
        if self.published.phase != Phase::Running {
            return None;
        }
        match (self.last_movement, self.last_output) {
            (Some(movement), Some(output)) => Some(movement.max(output) + self.policy.quiet_after),
            (Some(at), None) | (None, Some(at)) => Some(at + self.policy.quiet_after),
            (None, None) => None,
        }
    }

    /// Fold one observation in; returns `true` when the published status changed.
    pub(crate) fn observe(&mut self, evidence: &Evidence, now: Instant) -> bool {
        self.note_activity(&evidence.activity, now);
        let candidate = self.classify(evidence, now);
        self.apply(candidate, now)
    }

    /// Track movement, treating an alt-screen transition as a resync: the two grids have
    /// independent counters.
    fn note_activity(&mut self, sample: &ActivitySample, now: Instant) {
        let identity = (sample.alt_screen, sample.content_seq);
        match self.prior {
            // Movement is inferable only within one grid whose counter advanced.
            Some((prior_alt, prior_seq))
                if prior_alt == sample.alt_screen && sample.content_seq != prior_seq =>
            {
                self.last_movement = Some(now);
            }
            // First sample, or an alt-screen transition: adopt the identity, infer nothing.
            _ => {}
        }
        self.prior = Some(identity);
        // Fold the raw-output clock too (`owed_wake` arms off the fresher one), by max, so a
        // sample with no stamp never rolls it back.
        if let Some(at) = sample.last_output {
            self.last_output = Some(self.last_output.map_or(at, |prior| prior.max(at)));
        }
    }

    fn moved_recently(&self, now: Instant) -> bool {
        self.last_movement
            .is_some_and(|at| now.saturating_duration_since(at) < self.policy.quiet_after)
    }

    fn output_recently(&self, sample: &ActivitySample, now: Instant) -> bool {
        sample
            .last_output
            .is_some_and(|at| now.saturating_duration_since(at) < self.policy.quiet_after)
    }

    /// Which signal justified "something is happening": grid mutation or only PTY bytes.
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

        let moved = self.moved_recently(now) || self.output_recently(&evidence.activity, now);

        // 2. Shell integration, the strongest routine evidence, outranks screen movement: a
        //    background job printing at a prompt stays Idle.
        if let Some(shell) = evidence.shell {
            // A genuine contradiction: the shell claims to be executing while
            // the foreground process group is the shell itself.
            let conflict =
                matches!(shell, ShellEvidence::Executing) && evidence.foreground_job == Some(false);
            return match shell {
                // As in the foreground-job arm, output that mutates nothing reads
                // `output_activity`.
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
                // A prompt — displayed, or with a command line being typed at
                // it — is `Idle`, keystrokes or not: RFC §6 permits no
                // stronger claim for a shell with no foreground job.
                //
                // A PROMPT REPORTS THE BLOCK IT CLOSED. The precmd's 133;D and
                // the next prompt's 133;A arrive back to back, so the sweep
                // meets the finished command as the block behind the prompt,
                // never as a current `Complete` one ([`completed_block`]); the
                // prompt arm carries its result. A prompt that closed nothing
                // completed reports nothing, and the published outcome stands.
                ShellEvidence::Prompt | ShellEvidence::Entering => Candidate {
                    phase: Phase::Idle,
                    confidence: Confidence::Strong,
                    reasons: vec![Reason::ShellBlock],
                    conflict: false,
                    outcome: evidence.completed_block.map(|b| exit_outcome(b.exit_code)),
                },
                ShellEvidence::Complete { exit_code } => Candidate {
                    phase: Phase::Idle,
                    confidence: Confidence::Strong,
                    reasons: vec![Reason::ShellBlock],
                    conflict: false,
                    outcome: Some(exit_outcome(exit_code)),
                },
            };
        }

        // 3. No shell integration. A foreground-job Boolean still separates
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
            // 4. Nothing but the screen. Movement is weak evidence of work;
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

    /// Publish a candidate once it has held for the dwell interval; an exact-confidence one (a
    /// lifecycle exit) publishes at once.
    fn apply(&mut self, candidate: Candidate, now: Instant) -> bool {
        let outcome = candidate.outcome.unwrap_or(self.published.last_outcome);
        // A new unit of work clears the previous result. Keyed on leaving rest, not entering
        // `Running`: Idle -> Quiet -> Running (`sleep 30`) would otherwise keep the previous
        // command's failure marked for its whole run.
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

/// Map shell-integration block state to evidence: the one place block semantics are read, so
/// the classifier stays free of terminal types.
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

/// Prompt evidence that says nothing about what runs behind it YIELDS to a live
/// foreground job: `None` (so [`StatusFsm::classify`]'s foreground-job rung decides)
/// when the shell has never reported a command
/// ([`crate::quit_safety::shell_reports_commands`]) and the job oracle sees one;
/// `shell` unchanged otherwise.
///
/// THE DEFECT (measured 2026-09-27 on 0.95.0, on Windows). cmd.exe's integration is
/// its `%PROMPT%`: it marks each prompt (133;A/B) and never a command, so the block
/// the classifier reads is the `Entering` block of the prompt the command was typed
/// at, and it stays `Entering` until the next prompt whatever runs. With `ping -n
/// 30 127.0.0.1` running in a `--shell cmd` tab, `status` said `phase=idle` and the
/// tab read idle, while the close guard, reading the same job oracle
/// ([`crate::quit_safety::foreground_busy`]), refused the close as a running job.
/// Any prompt-only integration on unix reads the same way while `tcgetpgrp` names a
/// job.
///
/// A shell that HAS reported commands keeps its marks' authority: its prompt is
/// its word that nothing holds the foreground, and a child it started in the
/// background (`Start-Process notepad`, `sleep 60 &`) is not the tab's work (the
/// `tail -f &` rule rung 2 exists for). A shell's first prompt, before any command,
/// cannot yet tell the two apart, and reads as the close guard reads it: a job is a
/// job. Only the prompt arms yield; an executing or completed block already says
/// what it is.
pub(crate) fn prompt_yields_to_job(
    shell: Option<ShellEvidence>,
    reports_commands: bool,
    foreground_job: Option<bool>,
) -> Option<ShellEvidence> {
    match shell {
        Some(ShellEvidence::Prompt | ShellEvidence::Entering)
            if !reports_commands && foreground_job == Some(true) =>
        {
            None
        }
        other => other,
    }
}

/// The block whose result the shell is reporting ([`Evidence::completed_block`]): the current
/// block once complete (133;D read, next 133;A not yet), or, at a prompt, the block that prompt
/// closed if it completed.
///
/// The second form is the one a sweep meets. Every aterm integration writes the
/// finished command's `133;D` and the next prompt's `133;A` back to back from
/// one hook (the zsh and bash precmd, fish's prompt event, the pwsh prompt
/// function), so the reader processes both in one batch, and the A archives
/// the completed block (`shell_prompt_start`) before any sweep can read it as
/// current. Reading the current block alone therefore never saw a
/// result: a failed command published `outcome=none` while `subscribe … events`
/// pushed its `block-complete … exit=1` (review finding, 2026-09-27).
///
/// Only the block the prompt closed, never an older one: a prompt that closed a
/// block with no 133;D (an empty Enter, an interrupted or `exec`-replaced
/// shell) reports nothing, so an older command's result cannot come back over
/// work that never reported one.
///
/// A scrollback erase after the prompt (Cmd-K, or a later ED 3) can take the
/// closed block with its rows, and the archive with it, before a sweep reads
/// it. The engine's copy of the block the last D completed
/// ([`aterm_core::terminal::Terminal::last_finished_block`]) answers then, when
/// it is the block the prompt closed (the id before the prompt's own), so the
/// result reported is that command's, never the one published before it.
pub(crate) fn completed_block(term: &aterm_core::terminal::Terminal) -> Option<CompletedBlock> {
    use aterm_types::BlockState;

    let current = term.current_block();
    let block = match current.map(|current| current.state) {
        Some(BlockState::Complete) => current?,
        // At a prompt (or with no current block, where `shell_evidence` reads
        // this same archived block): the newest archived block.
        Some(BlockState::PromptOnly | BlockState::EnteringCommand) | None => {
            match term.last_completed_block() {
                Some(archived) if archived.is_complete() => archived,
                Some(_) => return None,
                // Nothing archived is held: an erase took it, if this prompt
                // closed one.
                None => {
                    let closed = current?.id.checked_sub(1)?;
                    term.last_finished_block()
                        .filter(|finished| finished.id == closed)?
                }
            }
        }
        // A command is running: it reports when it completes.
        Some(_) => return None,
    };
    Some(CompletedBlock::of(block))
}

impl CompletedBlock {
    /// `block`'s id, exit code and C/D stamps.
    pub(crate) fn of(block: &aterm_types::OutputBlock) -> CompletedBlock {
        CompletedBlock {
            id: block.id,
            exit_code: block.exit_code,
            started_ms: block.command_exec_start_time_ms,
            ended_ms: block.command_end_time_ms,
        }
    }
}

/// THE SESSION LOG's reading of `term`'s blocks
/// ([`crate::session_log::BlockRead`]): the blocks from `from` on (the open
/// `cmd` run's block, else the one past the newest the log handled; `None`:
/// all), OLDEST first, at most [`BLOCK_READ_MAX`] that ran a command — the
/// block executing now, each one that finished (with its stamps and exit
/// code, the [`CompletedBlock`] [`completed_block`] reports) and each one a
/// prompt closed with no OSC 133 D. A block that finished before `since_wall`
/// (the log's session opened here: another process's) is passed over without
/// counting. Blocks past the cap wait for the next sweep (`more`, which owes
/// one, only while a block that may yet be a fact is left), so a burst of
/// commands (`true; false` typed together, a hundred lines fed at once) is
/// logged late, never dropped; block ids past `from` the terminal no longer
/// holds are counted once (`dropped`) and passed (`through`), and the
/// started-command count with the held blocks that reached C (`started`,
/// `started_held`) says whether a command went with them. `running_detail`
/// is the executing block's [`executing_detail`], read by the caller from the
/// same guard; a finished block's is read here once, as the log takes each
/// block once, and reduced by the same [`command_detail`]: a fact names its
/// program, never its line. Field reads
/// (and a finished block's command row) under the guard the sweep already
/// holds, and only of the blocks from `from` on: the walk goes newest first
/// and stops at the cursor, so the guard pays for what is new, never for the
/// retained history (up to a thousand blocks).
pub(crate) fn block_read(
    term: &aterm_core::terminal::Terminal,
    from: Option<u64>,
    since_wall: Option<u64>,
    running_detail: Option<String>,
) -> crate::session_log::BlockRead {
    use aterm_types::BlockState;

    use crate::session_log::{BlockEnd, BlockFact, BlockRead};
    let current = term.current_block().map(|b| b.id);
    let mut read = BlockRead {
        from,
        started: Some(term.started_command_seq()),
        ..BlockRead::default()
    };
    // The blocks from the cursor on, newest first to the cursor, then turned
    // oldest first.
    let mut past: Vec<&aterm_types::OutputBlock> = term
        .all_blocks()
        .rev()
        .take_while(|b| from.is_none_or(|f| b.id >= f))
        .collect();
    past.reverse();
    // A block that reached C: one accepted C each.
    let ran = |b: &aterm_types::OutputBlock| {
        matches!(b.state, BlockState::Executing | BlockState::Complete)
    };
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    read.started_held = count(past.iter().filter(|b| ran(b)).count());
    read.started_at_from = past.first().is_some_and(|b| Some(b.id) == from && ran(b));
    // Block ids rise by one per prompt and the terminal holds a contiguous
    // run of them: an oldest held id past the cursor means its history
    // dropped the ids between, which the read passes.
    if let (Some(f), Some(oldest)) = (from, past.first().map(|b| b.id))
        && oldest > f
    {
        read.dropped = oldest - f;
        read.through = Some(oldest - 1);
    }
    for (i, block) in past.iter().copied().enumerate() {
        // The prompt that closed it: the next block's.
        let closed_at = past.get(i + 1).and_then(|next| next.prompt_time_ms);
        let end = match block.state {
            BlockState::Executing if Some(block.id) == current => Some(BlockEnd::Running),
            BlockState::Complete => Some(BlockEnd::Done(CompletedBlock::of(block))),
            // Archived while still executing: a prompt closed it with no D (an
            // interrupted, replaced or crashed command).
            BlockState::Executing => Some(BlockEnd::Unended {
                ended_ms: closed_at,
            }),
            // A prompt, or a command being typed: nothing ran.
            _ => None,
        };
        // One that finished before the log opened here is another process's.
        let end = end.filter(|end| {
            let ended = match *end {
                BlockEnd::Running => None,
                BlockEnd::Done(done) => done.ended_ms,
                BlockEnd::Unended { ended_ms } => ended_ms,
            };
            !ended.zip(since_wall).is_some_and(|(e, t)| e < t)
        });
        // The cap stops the walk only at a block that would be a fact: the
        // rest (blocks that ran nothing, another process's) are passed now.
        if end.is_some() && read.facts.len() == BLOCK_READ_MAX {
            read.more = true;
            read.started_unread = count(past.iter().skip(i).filter(|b| ran(b)).count());
            break;
        }
        // The prompt in front that has run nothing yet is where the next
        // command runs (it keeps its id from OSC 133 A to D): never passed.
        let settled = Some(block.id) != current
            || matches!(block.state, BlockState::Executing | BlockState::Complete);
        if settled {
            read.through = Some(block.id);
        }
        let Some(end) = end else {
            continue;
        };
        // Its program, never its line: a block the sweep first sees already
        // finished (the usual case for `echo`, `export KEY=…`) is reduced by
        // the same [`command_detail`] the running one's caller applied, so no
        // later word of a command reaches the log, its journal or the bus.
        let detail = match end {
            BlockEnd::Running => running_detail.clone(),
            _ => block_command_line(term, block).and_then(|line| command_detail(&line)),
        };
        read.facts.push(BlockFact {
            id: block.id,
            started_ms: block.command_exec_start_time_ms,
            end,
            detail,
        });
    }
    read
}

/// The most blocks that ran a command one [`block_read`] reports: far more
/// than a person finishes between two sweeps. A burst past it (lines fed or
/// typed ahead, an agent's quick `send`s) is read on at the next sweep, which
/// the cut read owes.
pub(crate) const BLOCK_READ_MAX: usize = 32;

/// The running command of `term`'s current shell-integration block, reduced by
/// [`command_detail`]; `None` unless a block is `Executing` and names a command. The one
/// producer of `Status.detail`; `status` and `sessions` read it under a `try_lock`.
pub(crate) fn executing_detail(term: &aterm_core::terminal::Terminal) -> Option<String> {
    command_detail(&executing_command_line(term)?)
}

/// The WHOLE command line of `term`'s current shell-integration block while
/// it is `Executing` — what [`executing_detail`] reduces to a word, and what
/// the survey quotes for a shell (`cmd=`, redacted and capped there). `None`
/// unless a block is `Executing` and names a command.
pub(crate) fn executing_command_line(term: &aterm_core::terminal::Terminal) -> Option<String> {
    let block = term.current_block()?;
    if block.state != aterm_types::BlockState::Executing {
        return None;
    }
    block_command_line(term, block)
}

/// `block`'s WHOLE command line, arguments and all: the shell's own OSC
/// 633;E line, else its command row scraped from the screen past the prompt.
/// Never stored as it is: the session log reduces it by [`command_detail`]
/// ([`block_read`]), and the survey redacts and caps it.
fn block_command_line(
    term: &aterm_core::terminal::Terminal,
    block: &aterm_types::OutputBlock,
) -> Option<String> {
    let explicit = block
        .commandline
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    match explicit {
        // OSC 633;E: the shell said the command line itself.
        Some(explicit) => Some(explicit.to_owned()),
        // The screen scrape reads WHOLE rows, so the command's first row still
        // carries the prompt in front of it (`$ sleep 30`); cut at the column
        // the shell marked the command start (OSC 133;B) — by display column,
        // not char count, so a wide prompt glyph does not eat the program's
        // first letter.
        None => {
            let scraped = term.block_command(block)?;
            let skip = usize::from(block.command_start_col.unwrap_or(0));
            let from = aterm_grapheme::column_to_char_index(&scraped, skip);
            Some(scraped.chars().skip(from).collect())
        }
    }
}

/// Wrappers unwrapped one level; `exec claude` is `claude` running.
const WRAPPERS: [&str; 7] = ["sudo", "env", "time", "nice", "command", "nohup", "exec"];
/// Wrapper flags that consume the following word (`sudo -u me`, `nice -n 5`, `exec -a argv0`).
const WRAPPER_FLAGS_WITH_VALUE: [&str; 4] = ["-u", "-n", "-g", "-a"];
/// Shell keywords that open a compound command, which is not parsed: the segment reads as the
/// keyword, and its `;`s are its grammar, not list operators.
const KEYWORD_OPENERS: [&str; 6] = ["for", "while", "if", "until", "case", "{"];
/// The punctuation the shell's quoting or a list cut leaves attached to a word
/// (`'claude'`, `(for`, `claude)`): trimmed before any word is read.
const WORD_TRIM: [char; 8] = ['\'', '"', '`', ';', '(', ')', '&', '|'];

/// `env`'s split-string flag (`-S`, a cluster with it after value-less letters like `-vS`, or
/// `--split-string`) hands env one string it splits into the program and its arguments
/// (`env -S 'claude --resume /secret/tok'` runs `claude`), so the program is that string's
/// first word. `Some(Some(s))`: the string is attached (`-Snode …`, `--split-string=…`);
/// `Some(None)`: it is the next word; `None`: not the flag.
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

/// The program a word in a program slot names: its basename, less a Windows executable
/// extension.
///
/// The basename follows the last `/` or `\` (save a `\` escaping a space) and ends at its first
/// whitespace, so `"/opt/My App/My App"` is `My` and no misreading of the line can carry an
/// argument out behind the name. `PATHEXT`'s `.exe`/`.com`/`.bat`/`.cmd` are dropped
/// case-insensitively (`less.exe` is `less`, as on every other host); only those, so
/// `python3.11` stays whole.
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

/// The raw word in a segment's program slot: past env assignments, a scrape's prompt glyph
/// (`$`, `❯`: no letter or digit; `{` is the one keyword with none) and one wrapper with its
/// flags, so `time for …` reads `for`. Used to tell whether a segment opens a compound command.
fn leading_word(segment: &str) -> Option<&str> {
    let mut words = shell_words(segment)
        .map(|w| w.trim_matches(WORD_TRIM))
        .filter(|w| !is_assignment(w) && (*w == "{" || w.chars().any(char::is_alphanumeric)))
        .peekable();
    let first = words.next()?;
    // Compared by [`basename`], so a Windows launcher's `C:\…\env.exe` is the wrapper `env`.
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

/// `segment` cut into words where the shell cuts them: at whitespace outside quotes, a quoted
/// stretch or a backslash-escaped character belonging to its word. So
/// `'/Users//me/Library/Application Support/aterm/pkg/agents/claude' --resume x` (the harness's
/// relaunch line) is a path and two arguments. Words are borrowed slices, quotes and
/// backslashes included.
///
/// A word ends at the nearer end of two readings ([`word_end`]): POSIX (a backslash in single
/// quotes is literal) and fish (`\'` in single quotes does not close them, as in the harness's
/// fish line `'/Users//o\'neil/…/claude'`). Backticks and `$(…)` are not tracked.
fn shell_words(segment: &str) -> impl Iterator<Item = &str> {
    let mut rest = 0;
    std::iter::from_fn(move || {
        let start = rest + segment[rest..].find(|c: char| !c.is_whitespace())?;
        let end = word_end(segment, start, false).min(word_end(segment, start, true));
        rest = end;
        Some(&segment[start..end])
    })
}

/// Where the word opening at byte `start` of `segment` ends under one quoting dialect:
/// `escape_in_single` is fish's (and `$'…'`'s), where a backslash inside single quotes
/// escapes, else POSIX's. The end is the first whitespace outside quotes, or the segment's
/// end; if a quote is still open there, the first whitespace after `start` at all, since the
/// open quote may be an argument's.
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
        // PowerShell escapes with a backtick (`& C:\Program` Files\…\less.exe x`). The line does
        // not say which shell typed it, so the escape is honoured only unquoted, directly before
        // whitespace, inside a word that is already a Windows path ([`windows_path_so_far`]);
        // a POSIX line (`claude` /secret/tok`) still ends the word at the space.
        if c == '`'
            && quote.is_none()
            && chars.clone().next().is_some_and(|(_, n)| n.is_whitespace())
            && windows_path_so_far(&segment[start..at])
        {
            chars.next();
        }
    }
    if quote.is_none() {
        return segment.len();
    }
    // A quote is still open, so this reading mispaired the quotes, perhaps the program's
    // apostrophe with an argument's opening quote (`don't '/secret/tok'`). The word's first
    // whitespace is the one end no mispairing can move past the program's own text.
    segment[start..]
        .find(char::is_whitespace)
        .map_or(segment.len(), |offset| start + offset)
}

/// Whether the start of a word, `head`, is already a Windows path (a drive `C:` or a `\`), the
/// only place a backtick before a space is PowerShell's escape ([`word_end`]).
fn windows_path_so_far(head: &str) -> bool {
    let mut chars = head.chars();
    let drive = matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    );
    drive || head.contains('\\')
}

/// The most a `detail=` may say about a command line: the program's basename, plus its first
/// subcommand when that word is in the program's closed vocabulary ([`SUBCOMMANDS`]), never an
/// argument. A command line carries tokens, hostnames and paths; an agent reading `status`
/// must learn who lives in a session (`claude`, `targo test`) without learning what it was
/// told. Empty input is `None`.
///
/// - Only the first non-flag word is a subcommand candidate, and only membership lets it
///   through: `git -C aterm status` is `git`, `kubectl -n prod get` is `kubectl`.
/// - Env assignments are skipped and one wrapper (`sudo`, `env`, `time`, `nice`, `exec`, …,
///   matched by basename) is unwrapped: `FOO=1 sudo -u me targo --unverified test` is
///   `targo test`. `env -S '<program> <args>'` reads the program from the split string
///   ([`env_split_string`]).
/// - A word is the shell's ([`shell_words`]): a quoted or escaped space belongs to it, so
///   `'/Users//me/Library/Application Support/aterm/pkg/agents/claude' --resume x` is `claude`.
///   The name ends at its first whitespace, and a word ends wherever a POSIX or fish reading
///   of the quotes ends it (at its first whitespace if a quote stays open), so a misquoted
///   line still never publishes argument text.
/// - The name drops a Windows executable extension ([`basename`]): `"C:\…\less.exe"` is `less`.
/// - A compound line names the segment that is running ([`split_list`], [`running_segment`]):
///   `cd ~/ay && claude` is `claude`, `cd /tmp; codex` is `codex`, `make || echo failed` is
///   `make`. A pipeline's `|` and a background `&` do not cut, and parentheses are not tracked.
/// - A shell keyword in a segment's program slot ([`leading_word`]) wins:
///   `for i in 1 2 3; do sleep 1; done` is `for` (also `if`, `while`, `case`, `until`, `{`),
///   since cutting its `;`s would answer a closer such as `done`. The first segment holding an
///   opener is read.
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
/// The retired Smart Titles summarizer's `short_command` was the precedent the
/// RFC names, but not this transform: it hid every program outside its own
/// allow-list as "a command", which is exactly the answer F5 found insufficient.
pub(crate) fn command_detail(cmdline: &str) -> Option<String> {
    /// The bound on the reply: enough for `kubectl port-forward`.
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
    // Which segment to read: the first whose program slot holds a keyword opener, else the
    // running one. Runs on every due observation, so the line is parsed once and words stay
    // borrowed.
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

    // Skip env assignments (`FOO=1 cmd`) and prompt glyphs a scrape left in front (`$`, `❯`:
    // no letter or digit).
    while words
        .peek()
        .is_some_and(|w| is_assignment(w) || !w.chars().any(char::is_alphanumeric))
    {
        words.next();
    }
    // The program slot, wrapper included, is compared by [`basename`], so `/usr/bin/env FOO=1
    // codex` unwraps like `env FOO=1 codex`.
    let mut program_word = words.next()?;
    // `env -S '<program> <args>'`: the one string env splits ([`env_split_string`]).
    let mut split: Option<String> = None;
    if WRAPPERS.contains(&basename(&program_word)?) {
        let env = basename(&program_word) == Some("env");
        // Unwrap one level: the wrapper's flags (with a value-taking flag's value), then
        // `env`'s assignments.
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
    // Past its program, the split string's words stand in for the subcommand; env's options
    // and assignments may open it (`-S '-i FOO=1 node x'`).
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
    // Bound before allocating: a truncated owned token would keep its whole capacity.
    let program_end = program
        .char_indices()
        .nth(MAX_CHARS)
        .map_or(program.len(), |(end, _)| end);
    let mut detail = program[..program_end].to_string();
    if let Some((_, vocabulary)) = SUBCOMMANDS.iter().find(|(p, _)| *p == program) {
        // The first word after the program not starting with `-`, only when the vocabulary
        // owns it. Flags are skipped but their values are not, so `git -C aterm status` reads
        // `git` and `git -C status log` reads `git status`: wrong either way, never an argument.
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
    /// `&&`: the right side runs only after the left succeeded.
    And,
    /// `||`: the right side runs only if the left failed.
    Or,
}

/// `cmdline` cut at its top-level list operators (`;` and newline, `&&`, `||`), each segment
/// paired with the operator before it (the first with `Seq`). Quotes, backticks and backslash
/// escapes are honoured (`claude -p "a; b"` is one segment); a single `|` or `&` stays inside
/// its segment and `(`/`$(` nesting is not tracked. Every cut is an ASCII byte, so slices
/// stay on char boundaries.
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
            // Inside quotes only the matching quote ends them; a backslash escapes the next
            // byte except inside single quotes.
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

/// The segment of a command line that is running, which [`command_detail`] reads: the last
/// `;`-group with content, and within it the last `&&` operand once trailing `|| …`
/// alternatives (which run only on failure) are dropped.
fn running_segment<'a>(cmdline: &'a str, segments: &[(ListOp, &'a str)]) -> &'a str {
    // The last nonblank operand ends the last nonblank `;` group.
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
    // `a && b || c`: `c` runs only if `a && b` failed, so drop trailing `||` and blank operands.
    while chain.len() > 1
        && chain
            .last()
            .is_some_and(|(op, text)| *op == ListOp::Or || text.trim().is_empty())
    {
        chain = &chain[..chain.len() - 1];
    }
    chain.last().map_or(cmdline, |(_, text)| text)
}

/// The programs whose first subcommand a `detail=` may name, each with the closed vocabulary
/// it is checked against: a word outside it is an argument, whatever it looks like
/// (`make deploy-prod` reads `make`). A new entry changes the wire, so the table test names one
/// row per program.
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

/// `cargo`'s subcommands, shared with `targo`.
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

/// The subcommands the three node package managers share; a `run` script name is an argument.
const NODE_SUBCOMMANDS: &[&str] = &[
    "run", "install", "test", "build", "start", "dev", "ci", "add", "remove", "exec", "publish",
    "init", "link", "update", "outdated", "audit", "lint", "format", "dlx", "create", "why",
    "list",
];

/// The `make`/`just` targets common enough to be vocabulary; anything else is the user's word.
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

/// STATUS_CONTROL_C_EXIT (`0xC000013A`) as a signed exit code: what a Windows console
/// program stopped by Ctrl+C exits with (`ping -t` in pwsh or cmd), and what the pwsh
/// integration reports for a line Ctrl+C stopped. No Unix shell reports it.
pub(crate) const WINDOWS_CONTROL_C_EXIT: i32 = -1_073_741_510;

/// Whether a command's exit code says Ctrl+C stopped it, for chrome's words: the
/// Windows code anywhere, and on Windows also 130 (128 + SIGINT), what Git Bash reports
/// for the same Ctrl+C, so pwsh, cmd and Git Bash tabs read alike. The wire keeps the
/// code (`status exit_code=`, `blocks`). macOS and Linux show 130 as before.
pub(crate) const fn interrupted_exit(exit_code: i32) -> bool {
    exit_code == WINDOWS_CONTROL_C_EXIT || (cfg!(windows) && exit_code == 130)
}

/// Short display text for chrome; `None` for phases not worth showing.
pub(crate) fn summary_text(status: &Status) -> Option<String> {
    // A guess reads as what was observed, never as a conclusion: below `Strong`, a moving grid
    // is `screen changing` and bytes over an unchanged grid `output arriving`.
    let text = match (status.phase, status.last_outcome) {
        (Phase::Unknown, _) => return None,
        (Phase::Starting, _) => "starting".to_string(),
        (Phase::Running, _) if status.confidence < Confidence::Strong => {
            if status.reasons.contains(&Reason::ContentActivity) {
                "screen changing".to_string()
            } else {
                "output arriving".to_string()
            }
        }
        (Phase::Running, _) => match &status.detail {
            Some(detail) if !detail.is_empty() => format!("running {detail}"),
            _ => "running".to_string(),
        },
        (Phase::Quiet, _) => "no output lately".to_string(),
        (Phase::Idle, Outcome::Failure { exit_code }) if interrupted_exit(exit_code) => {
            "interrupted".to_string()
        }
        (Phase::Idle, Outcome::Failure { exit_code }) => format!("failed (exit {exit_code})"),
        (Phase::Idle, Outcome::Signal { signal }) => format!("killed (signal {signal})"),
        (Phase::Idle, Outcome::Success) => "done".to_string(),
        (Phase::Idle, Outcome::None) => "ready".to_string(),
        (Phase::Exited, Outcome::Failure { exit_code }) if interrupted_exit(exit_code) => {
            "exited (interrupted)".to_string()
        }
        (Phase::Exited, Outcome::Failure { exit_code }) => format!("exited (exit {exit_code})"),
        (Phase::Exited, Outcome::Signal { signal }) => format!("exited (signal {signal})"),
        (Phase::Exited, _) => "exited".to_string(),
    };
    Some(text)
}

/// An identified agent's published phase as chrome's activity line, in the band's words
/// (`busy`, `bash approval`, `logged out`); `None` for an unknown phase, leaving
/// [`summary_text`].
pub(crate) fn agent_activity_text(phase: &crate::presence::AgentPhase) -> Option<String> {
    use crate::presence::AgentPhase as P;
    Some(match phase {
        P::Prompt { detail } => crate::presence::prompt_band_word(detail.as_deref()),
        P::Unknown => return None,
        other => other.band_word().to_string(),
    })
}

/// Map one session's status onto the tab's independent indicator bits. `dirty` (unsaved editor
/// state) is never claimed. A split tab folds these bits per leaf
/// ([`crate::tab_model::aggregate_presentations`]), so an `Exited` pane cannot hide a
/// sibling's failure.
#[must_use]
pub(crate) fn status_indicators(status: &Status) -> crate::tab_model::TabIndicators {
    crate::tab_model::TabIndicators {
        dirty: false,
        // Quiet is not idle: it means a known foreground job printed nothing for
        // `quiet_after`, and darkening the dot mid-link-step would read as finished.
        busy: matches!(status.phase, Phase::Running | Phase::Quiet),
        // A native leaf's own mark: the status path never writes it, so the classifier's bit
        // below is recomputed rather than latched.
        attention: false,
        // The outcome outlives its phase ([`Outcome`]), so a failed command keeps its tab
        // marked while the shell sits at `Idle`. Only a failure raises attention: there is no
        // local evidence for "waiting for you", and inventing it from silence would mark half
        // the window.
        status_attention: status.last_outcome.is_failure(),
    }
}

/// The timeline kind of a published status transition, pushed on `subscribe …
/// events` as `EVENT <local> status …` (`subscribe::timeline_wire_kind`).
pub(crate) const STATUS_CHANGE: &str = "status-change";

/// `status`'s ` conpty=…` field: the console host a Windows session runs in
/// (`aterm_pty::conpty_host`), `-` for a key no pseudoconsole is behind (a stub
/// or replay session). Empty off Windows (`windows` false), where no session
/// has one, so the record there is the one every earlier build wrote.
fn conpty_status_field(windows: bool, host: Option<aterm_pty::ConsoleHostKind>) -> &'static str {
    if !windows {
        return "";
    }
    match host {
        Some(aterm_pty::ConsoleHostKind::OpenConsole) => " conpty=openconsole",
        Some(aterm_pty::ConsoleHostKind::Inbox) => " conpty=inbox",
        None => " conpty=-",
    }
}

/// `status`'s `needs=<word|-> needs_n=<n|->`: the published need's word, then
/// how many needs a harness's ask names ([`crate::needs::HarnessAsk::count`],
/// Autopilot v5 §12.2.1), `-` where the published need is none or not a
/// harness's. One value, so `needs_n=` rides `needs=` wherever it is printed.
pub(crate) fn needs_fields(verdict: &crate::needs::Verdict) -> String {
    let n = verdict
        .harness_ask()
        .map_or_else(|| "-".to_string(), |ask| ask.count().to_string());
    format!("{} needs_n={n}", verdict.needs_word())
}

/// THE WORKER'S NEEDS YOU AS ITS LOOP TOLD THE WINDOW (development builds,
/// `docs/DESIGN-harness-autopilot-2026-10-15.md` §12.2.3 item 3, slice 2c):
/// ` needs_told=<n>` where a reader reads a set of `n` needs beside the
/// needs owner's entry that stands (`entry`): the set told last, or the
/// outgoing one while its own entry still stands; ` needs_told=stale` where
/// the set told last was told for another entry and none held describes the
/// one standing (a request the connection lost, or the entry moved by
/// someone else: no reader reads it); and ` needs_told=-` where none was
/// told last (none, or the take-down) and none is read. The seam a headless
/// lab reads the store through; additive, read by key.
#[cfg(any(test, debug_assertions))]
pub(crate) fn needs_told_field(told: &crate::needs::ToldNeeds, entry: Option<&str>) -> String {
    match (told.words_for(entry), told.current()) {
        (Some(needs), _) => format!(" needs_told={}", needs.len()),
        (None, Some(_)) => " needs_told=stale".to_string(),
        (None, None) => " needs_told=-".to_string(),
    }
}

/// A `status-change` event's payload, in the `status` record's spellings:
/// `phase=<p> outcome=<o> exit_code=<n|-> signal=<n|-> confidence=<c>`.
pub(crate) fn status_event_payload(status: &Status) -> String {
    let (exit_code, signal) = status.last_outcome.wire_fields();
    format!(
        "phase={} outcome={} exit_code={exit_code} signal={signal} confidence={}",
        status.phase.as_str(),
        status.last_outcome.as_str(),
        status.confidence.as_str(),
    )
}

/// A published status transition as the session log's typed `status` mark
/// takes it (phase 3B, plan §3.2): the same facts as
/// [`status_event_payload`], never parsed back from it.
pub(crate) fn status_facts(status: &Status) -> crate::session_log::observed::StatusFacts {
    let out = match status.last_outcome {
        Outcome::None => None,
        Outcome::Success => Some(("ok", None)),
        Outcome::Failure { exit_code } => Some(("fail", Some(i64::from(exit_code)))),
        Outcome::Signal { signal } => Some(("signal", Some(i64::from(signal)))),
    };
    let conf = match status.confidence {
        Confidence::Unknown => aterm_slog::Conf::Unknown,
        Confidence::Heuristic => aterm_slog::Conf::Heuristic,
        Confidence::Strong => aterm_slog::Conf::Strong,
        Confidence::Exact => aterm_slog::Conf::Exact,
    };
    crate::session_log::observed::StatusFacts {
        phase: status.phase.as_str(),
        out,
        conf,
    }
}

/// Owns one [`StatusFsm`] per live session and enforces the observation budget:
/// at most one classification per session per `min_interval`, so an output flood
/// cannot turn into a classification flood.
#[derive(Debug)]
pub(crate) struct StatusObserver {
    policy: StatusPolicy,
    min_interval: Duration,
    /// Last applied `tab_status_badge`, held so a flip is detectable (it gates chrome only).
    badge: bool,
    sessions: std::collections::HashMap<u64, SessionSlot>,
    /// A lower bound on the minimum of `sessions`' deadlines, so the sweep's gate is O(1);
    /// `None` keeps the gate open. [`StatusObserver::observe`] only folds new deadlines in, which
    /// can leave it early (one extra scan, never a missed classification);
    /// [`StatusObserver::note_swept`] makes it exact.
    next_due_any: Option<Instant>,
    /// `SessionPool::insert_epoch` at the last completed sweep. While it matches,
    /// `next_due_any` covers the whole pool; a mismatch means a new session may await its
    /// first classification, so the gate opens.
    swept_pool_epoch: u64,
    /// Pooled sessions the last full sweep could not classify (their `try_lock` lost to the PTY
    /// reader); each output wake retries just these ([`StatusObserver::note_swept`]).
    unclassified: Vec<u64>,
    /// The agent verdict's watch, per session.
    agents: std::collections::HashMap<u64, AgentWatch>,
    /// Monotonic across sessions and clears: a changed reading's sequence number, so presence
    /// folds only newer readings.
    agent_seq: u64,
    /// [`Self::agent_seq`] at the last [`Self::clear`], answered for an unwatched session so a
    /// reading from before the clear is retired.
    agent_cleared_at: u64,
    /// Names a foreground group's program off the event loop.
    programs: program::ProgramResolver,
    /// The input watch: sessions with input their program has not read, and each published
    /// stall ([`crate::input_stall::InputWatches`]). [`Self::clear`] leaves it alone: a
    /// published stall is also an attention entry only its own probe may clear.
    pub(crate) inputs: crate::input_stall::InputWatches,
    /// When each Claude Code session's footer facts were last asked for (at most every
    /// [`crate::claude_footer::RECHECK`]).
    footer_asked: std::collections::HashMap<u64, FooterAsk>,
    /// When each codex-alab session's rollout was last asked to be read on
    /// ([`Self::request_rollout_if_due`]), at most every
    /// [`crate::codex_rollout_tap::RECHECK`].
    rollout_asked: std::collections::HashMap<u64, Instant>,
    /// THE SESSION LOG's settle wakes ([`crate::session_log::Wakes`]): the
    /// sessions whose turn tracker waits on a resting word, and when to tick
    /// it. Not part of the `tab_status` subsystem either: [`Self::clear`]
    /// leaves it, and a tick finds its own way out.
    pub(crate) slog_wakes: crate::session_log::Wakes,
    /// THE SESSION LOG's instance-wide memory sweep
    /// ([`crate::App::sweep_session_log_memory`]): its cap and what it last
    /// read of each log.
    pub(crate) slog_memory: crate::session_log::MemorySweep,
    /// THE PROGRAM'S OWN STATE (OSC 7501), per session: which report shows
    /// ([`crate::program_state::ProgramStateGate`]). A claim: shown, and
    /// each move a `program` log mark (6F-4), never a verdict; not part of
    /// the `tab_status` subsystem either.
    program_states: std::collections::HashMap<u64, crate::program_state::ProgramStateGate>,
}

#[derive(Clone, Copy, Debug)]
struct FooterAsk {
    at: Instant,
    pgid: i32,
    stopped: bool,
    /// The Claude Code drawer image the ask was made under
    /// (`SessionTimeline::claude_drawer_token`), where one was known: a new
    /// one at an exec edge is asked for at once.
    image: Option<u64>,
}

#[derive(Debug)]
struct SessionSlot {
    fsm: StatusFsm,
    next_due: Instant,
    /// Bumped when the published status changes, so chrome skips unchanged frames.
    revision: u64,
    /// How this session's PTY ended, sticky: a `--hold` pane keeps being swept, and its failing
    /// `tcgetpgrp` must not read as an idle shell.
    lifecycle: Option<Lifecycle>,
    /// Whether the person has seen the failure this session's tab is marked for.
    failure: FailureMark,
    /// The `(phase, outcome)` the last `status-change` timeline event carried
    /// ([`StatusObserver::take_status_transition`]); `None` before the first.
    evented: Option<(Phase, Outcome)>,
}

impl SessionSlot {
    fn new(policy: StatusPolicy, now: Instant) -> Self {
        Self {
            fsm: StatusFsm::new(policy, now),
            next_due: now,
            revision: 1,
            lifecycle: None,
            failure: FailureMark::default(),
            evented: None,
        }
    }
}

/// Whether a failure mark has been seen. A failed command marks its tab
/// ([`status_indicators`]); once the person looks (the focused view of a focused window) the
/// mark goes quiet, while the failure stays on the record (`status outcome=`, the tooltip).
///
/// A generation compare, so a later failure re-arms the mark. A new failure is the outcome
/// becoming a failure, or the shell completing a different block while it stays one: two
/// failing commands inside one interval publish no status change, and only the completed block
/// ([`Evidence::completed_block`]) tells them apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FailureMark {
    /// The published outcome is a failure.
    failing: bool,
    /// Bumped per new failure.
    generation: u64,
    /// The completed block the current failure was read from, when shell
    /// integration named one.
    block: Option<u64>,
    /// The generation the person has seen.
    seen: u64,
}

impl FailureMark {
    /// Fold the published outcome in after an observation.
    fn note(&mut self, failing: bool, completed_block: Option<u64>) {
        if failing {
            let another_block = completed_block.is_some() && completed_block != self.block;
            if !self.failing || another_block {
                self.generation = self.generation.saturating_add(1);
            }
            if completed_block.is_some() {
                self.block = completed_block;
            }
        } else {
            self.block = None;
        }
        self.failing = failing;
    }

    /// A failure the person has not seen yet: what marks the tab.
    const fn unseen(&self) -> bool {
        self.failing && self.generation > self.seen
    }

    /// The person looked. Returns whether that quieted a mark.
    fn see(&mut self) -> bool {
        let quieted = self.unseen();
        self.seen = self.generation;
        quieted
    }
}

impl StatusObserver {
    pub(crate) fn new(policy: StatusPolicy, min_interval: Duration) -> Self {
        Self {
            policy,
            min_interval,
            // `tab_status_badge`'s default, so the first reconfigure reports no flip.
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
            rollout_asked: std::collections::HashMap::new(),
            slog_wakes: crate::session_log::Wakes::default(),
            slog_memory: crate::session_log::MemorySweep::default(),
            program_states: std::collections::HashMap::new(),
        }
    }

    /// Ask for `session`'s footer facts when Claude Code is the program in
    /// front and the last ask is at least [`crate::claude_footer::RECHECK`]
    /// old — or, on an `edged` visit, when the group's published image is a
    /// store or Native Claude Code drawer the last ask was not made under
    /// (card engine §3.10): the `exec` into it keeps the group and the
    /// program's name, so nothing else would ask before `RECHECK`, and an
    /// ask made before it may have fallen in atpkg's `/bin/sh` shim, whose
    /// environment macOS hides, so it could not find the Claude dir whose
    /// registry the resolver watches. The answer arrives on the resolver
    /// thread; a change wakes the loop.
    #[expect(
        clippy::too_many_arguments,
        reason = "the sweep's one call site hands over what it already sampled"
    )]
    pub(crate) fn request_footer_if_due(
        &mut self,
        session: u64,
        timeline: &std::sync::Arc<std::sync::Mutex<crate::session_timeline::SessionTimeline>>,
        program: Option<&str>,
        pgid: i32,
        now: Instant,
        wake: &crate::claude_footer::LogWake,
        edged: bool,
    ) {
        if !crate::claude_footer::drawn_for(program) || pgid <= 0 {
            // A held pane can outlive Claude with no valid foreground group, and the resolver
            // names no replacement to stop: stop the group last asked for (or the timeline's
            // still-named Claude group), once, not every interval.
            let prior = self.footer_asked.get(&session).copied();
            if prior.is_some_and(|ask| ask.stopped)
                || (prior.is_none() && !(pgid <= 0 && crate::claude_footer::drawn_for(program)))
            {
                return;
            }
            let current = timeline.lock().unwrap_or_else(|p| p.into_inner());
            // Hold the leaf lock through the stop: a newer Claude answer already landed is kept,
            // and one landing next sends its request after this stop.
            if pgid > 0
                && current.agent().program_pgid == pgid
                && crate::claude_footer::drawn_for(current.agent().program.as_deref())
            {
                return;
            }
            let old = prior.map(|ask| ask.pgid).or_else(|| {
                (pgid <= 0 && crate::claude_footer::drawn_for(program))
                    .then_some(current.agent().program_pgid)
            });
            if let Some(old) = old.filter(|old| *old > 0) {
                self.footer_asked.insert(
                    session,
                    FooterAsk {
                        at: now,
                        pgid: old,
                        stopped: true,
                        image: None,
                    },
                );
                crate::claude_footer::stop(session, old);
            }
            return;
        }
        // Only an edge can publish a new image, so only an edged visit looks.
        let image = if edged {
            timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .claude_drawer_token(pgid)
        } else {
            None
        };
        let prior = self.footer_asked.get(&session).copied();
        let due = prior.is_none_or(|ask| {
            ask.stopped
                || ask.pgid != pgid
                || now.saturating_duration_since(ask.at) >= crate::claude_footer::RECHECK
                || image.is_some_and(|token| ask.image != Some(token))
        });
        if due {
            // The sampled program can be stale: hold the leaf lock through the send, so a stale
            // sample cannot restart a watch a worker has already stopped.
            let current = timeline.lock().unwrap_or_else(|p| p.into_inner());
            if current.agent().program_pgid != pgid
                || !crate::claude_footer::drawn_for(current.agent().program.as_deref())
            {
                return;
            }
            // An ask off an edge keeps the image the last ask of this group
            // was made under, so the next edge of that image asks nothing.
            let image = image.or_else(|| {
                prior
                    .filter(|ask| ask.pgid == pgid && !ask.stopped)
                    .and_then(|ask| ask.image)
            });
            self.footer_asked.insert(
                session,
                FooterAsk {
                    at: now,
                    pgid,
                    stopped: false,
                    image,
                },
            );
            crate::claude_footer::request_footer(session, timeline, pgid, wake);
        }
    }

    /// THE CODEX ROLLOUT TAP (session log 5G-3; ruling T15): ask for
    /// `session`'s rollout to be read on into its log, at most every
    /// [`crate::codex_rollout_tap::RECHECK`], while `program` in front is the
    /// one a proven self-claim speaks for and that claim's copy names a
    /// thread ([`crate::self_claim::rollout_target`]); otherwise its tap is
    /// dropped once, and `status rollout=` says `stopped`. The read runs on
    /// the tap's own thread ([`crate::codex_rollout_tap::follow_ask`]).
    pub(crate) fn request_rollout_if_due(
        &mut self,
        session: u64,
        ctx: &std::sync::Arc<crate::SessionCtx>,
        program: Option<&str>,
        now: Instant,
        wake: &crate::claude_footer::LogWake,
    ) {
        use crate::codex_rollout_tap::{due, follow_ask};
        let target = crate::self_claim::fronts_claimant(program)
            .then(|| crate::self_claim::rollout_target(ctx))
            .flatten();
        follow_ask(
            due(&mut self.rollout_asked, session, target, now),
            session,
            ctx,
            wake,
        );
    }

    /// Session `session`'s published input stall, if any.
    pub(crate) fn input_stall(&self, session: u64) -> Option<&crate::input_stall::InputStallFact> {
        self.inputs.published(session)
    }

    /// Whether `session`'s input is watched (its output wakes probe it, at most every
    /// `input_stall::PROBE_MIN_GAP`).
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

    /// Could any known session be past its deadline now? The O(1) half of the sweep's gate (the
    /// other is the pool epoch). `None` answers yes.
    pub(crate) fn any_due(&self, now: Instant) -> bool {
        self.next_due_any.is_none_or(|due| now >= due)
    }

    /// A sweep walked the whole pool: make `next_due_any` exact, bank `pool_epoch`, and record
    /// the pooled sessions it left slot-less. O(slots), at the classification rate only.
    ///
    /// A session whose `try_lock` lost to the PTY reader has no slot, so `next_due_any` cannot
    /// speak for it while a pool scan would classify it on the next wake. Recording it keeps the
    /// gate open for exactly that session until it is classified, so the gate stays equivalent
    /// to the scan it replaces.
    pub(crate) fn note_swept(&mut self, pool_epoch: u64, unclassified: Vec<u64>) {
        self.swept_pool_epoch = pool_epoch;
        self.unclassified = unclassified;
        self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
    }

    /// Whether a sweep left a pooled session slot-less ([`StatusObserver::note_swept`]).
    pub(crate) fn has_unclassified(&self) -> bool {
        !self.unclassified.is_empty()
    }

    /// Hand the unclassified ids to a retry pass, which returns the ones it still could not
    /// classify through [`Self::restore_unclassified`].
    pub(crate) fn take_unclassified(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.unclassified)
    }

    /// See [`Self::take_unclassified`].
    pub(crate) fn restore_unclassified(&mut self, ids: Vec<u64>) {
        self.unclassified = ids;
    }

    /// Whether this observer holds a slot for `session`, i.e. has ever classified it.
    pub(crate) fn knows(&self, session: u64) -> bool {
        self.sessions.contains_key(&session)
    }

    /// See [`StatusObserver::note_swept`].
    pub(crate) fn swept_pool_epoch(&self) -> u64 {
        self.swept_pool_epoch
    }

    /// Whether this session is allowed a classification now; checked before taking the
    /// terminal lock.
    pub(crate) fn due(&self, session: u64, now: Instant) -> bool {
        self.sessions
            .get(&session)
            .is_none_or(|slot| now >= slot.next_due)
    }

    /// Charge a session's observation interval for a sample that could NOT be
    /// taken — the sweep reached it and refused, rather than never reaching it.
    ///
    /// Only a known session is charged: a slot-less one has no deadline to spin on, and a slot
    /// here would bank the pool epoch over a session never classified.
    pub(crate) fn note_skipped(&mut self, session: u64, now: Instant) {
        let Some(slot) = self.sessions.get_mut(&session) else {
            return;
        };
        let due = now + self.min_interval;
        slot.next_due = due;
        // The lock was held by the PTY reader, so output arrived: owe a look.
        self.note_output(session, now);
        // Same lower-bound fold as `observe`.
        self.next_due_any = Some(self.next_due_any.map_or(due, |min| min.min(due)));
    }

    /// Fold one observation in; returns `true` when the published status changed. A recorded
    /// exit overrides the evidence's `lifecycle`: no later sample of a dead pane un-ends it.
    pub(crate) fn observe(&mut self, session: u64, evidence: &Evidence, now: Instant) -> bool {
        let policy = self.policy;
        let slot = self
            .sessions
            .entry(session)
            .or_insert_with(|| SessionSlot::new(policy, now));
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
        slot.failure.note(
            slot.fsm.status().last_outcome.is_failure(),
            evidence.completed_block.map(|b| b.id),
        );
        // Fold, never recompute: a lower bound keeps the gate O(1); `note_swept` restores
        // exactness once per sweep.
        let due = now + self.min_interval;
        self.next_due_any = Some(self.next_due_any.map_or(due, |min| min.min(due)));
        changed
    }

    /// Record that this session's PTY ended, and classify it at once; returns whether the
    /// published status changed. Called from the `Wake::Exit` edge: the exit status is only
    /// collectable before teardown.
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
            .or_insert_with(|| SessionSlot::new(policy, now))
            .lifecycle = Some(lifecycle);
        // Exit is `Confidence::Exact`: it bypasses dwell and publishes on this call.
        self.observe(session, evidence, now)
    }

    /// Remember the badge switch; returns whether it moved (the caller refolds every tab).
    pub(crate) fn set_badge(&mut self, badge: bool) -> bool {
        std::mem::replace(&mut self.badge, badge) != badge
    }

    /// Earliest instant at which some session owes a time-driven transition, for the event
    /// loop's wait deadline: without it a build that finishes silently would show busy forever.
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        // The agent follow-up: a session whose content moved at its last look, or whose output
        // landed between looks, is looked at once more after the output pauses
        // (`output_at + min_interval`, never before `next_due`), so an agent's last frame is
        // read within one interval of it.
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
                // A missing argv[0] can be transient (a group just exec'd), and an unchanged
                // screen requests nothing, so this deadline is the only route to a retry.
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
                // Clamp to `next_due`: `due()` refuses the slot until then, so an earlier
                // deadline could only fire, be refused and re-arm. This bounds any such bug to
                // the observation rate rather than an event-loop-rate spin.
                slot.fsm.owed_wake().map(|owed| owed.max(slot.next_due))
            })
            .min();
        match (owed, agent_wake) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The resolver finished after this session's ordinary follow-up: arm one more observation
    /// at the slot's floor. A level, so a burst of completions owns one wake; a retired session
    /// is a no-op.
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

    /// This session's indicator bits ([`status_indicators`]), a seen failure kept off the tab
    /// ([`FailureMark`]); `None` before the first publication.
    pub(crate) fn indicators(&self, session: u64) -> Option<crate::tab_model::TabIndicators> {
        let slot = self.sessions.get(&session)?;
        let mut bits = status_indicators(slot.fsm.status());
        bits.status_attention &= slot.failure.unseen();
        Some(bits)
    }

    /// The person is looking at `session`: its current failure is seen. Returns whether a tab
    /// mark went quiet.
    pub(crate) fn note_seen(&mut self, session: u64) -> bool {
        self.sessions
            .get_mut(&session)
            .is_some_and(|slot| slot.failure.see())
    }

    /// Whether `session`'s published outcome is a failure the person has
    /// already SEEN ([`FailureMark`]): what an update carries for a held pane
    /// (`session_store::HeldPaneCarry::failure_seen`), whose successor shows
    /// the same exit again (round eight of the update audit, U34).
    #[cfg(unix)]
    pub(crate) fn failure_seen(&self, session: u64) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|slot| slot.failure.failing && !slot.failure.unseen())
    }

    /// The payload of a `status-change` timeline event, when the published
    /// PHASE or OUTCOME moved since the last one (RFC §7: one timeline event per
    /// transition that survived dwell). Reason, confidence and detail moves are
    /// not events: they change without dwell (`content_activity` ↔
    /// `output_activity` under one `Running`), and carrying them would flood the
    /// bounded timeline the dwell exists to protect. Consumes the change.
    #[cfg(test)]
    pub(crate) fn take_status_event(&mut self, session: u64) -> Option<String> {
        self.take_status_transition(session)
            .map(|(payload, _)| payload)
    }

    /// The `status-change` payload (the tests' `take_status_event`) with the
    /// same transition's typed facts, for the session log's `status` mark
    /// (one take, so the two never disagree).
    pub(crate) fn take_status_transition(
        &mut self,
        session: u64,
    ) -> Option<(String, crate::session_log::observed::StatusFacts)> {
        let slot = self.sessions.get_mut(&session)?;
        let status = slot.fsm.status();
        let key = (status.phase, status.last_outcome);
        if slot.evented == Some(key) {
            return None;
        }
        slot.evented = Some(key);
        Some((status_event_payload(status), status_facts(status)))
    }

    /// The published agent reading presence folds, with its sequence number (`None`: not an
    /// identified agent). A session never classified answers the sequence of the last
    /// [`Self::clear`] (`0` before any), which retires an older reading.
    pub(crate) fn agent_reading(
        &self,
        session: u64,
    ) -> (u64, Option<crate::presence::AgentReading>) {
        match self.agents.get(&session) {
            Some(w) if w.reading_seq > 0 => (w.reading_seq, w.reading.clone()),
            _ => (self.agent_cleared_at, None),
        }
    }

    /// The program [`Self::agent_reading`]'s verdict was classified under (the
    /// `status program=` of its read), or `None` with no reading, or one whose
    /// group has since moved.
    pub(crate) fn agent_reading_program(&self, session: u64) -> Option<&str> {
        self.agents
            .get(&session)
            .filter(|w| w.reading_seq > 0)
            .and_then(|w| w.program_seen.as_ref())
            .and_then(Option::as_deref)
    }

    /// Name `program` as the one the standing reading was classified under,
    /// leaving the reading itself: a test's stand-in for a reading of a
    /// program whose screen no reader reads today (Codex's lane is retired), so
    /// a rule keyed on the reading's program can be shown to pass.
    #[cfg(test)]
    pub(crate) fn stand_in_reading_program(&mut self, session: u64, program: &str) {
        if let Some(w) = self.agents.get_mut(&session) {
            w.program_seen = Some(Some(program.to_owned()));
        }
    }

    /// [`Self::agent_reading`]'s sequence number, without cloning the reading.
    pub(crate) fn agent_reading_seq(&self, session: u64) -> u64 {
        match self.agents.get(&session) {
            Some(w) if w.reading_seq > 0 => w.reading_seq,
            _ => self.agent_cleared_at,
        }
    }

    /// Output arrived for `session` (the output wake, before its sweep): its agent verdict owes
    /// a look once the output pauses. A sweep that cannot look now (not due, gated, contended)
    /// leaves this armed, so the final frame of a burst is read within one interval of it. One
    /// deadline, pushed on by each burst, fires after the last ([`Self::next_wake`]).
    pub(crate) fn note_output(&mut self, session: u64, now: Instant) {
        if let Some(w) = self.agents.get_mut(&session) {
            w.followup = true;
            w.output_at = Some(now);
        }
    }

    /// `session` owes ONE more observation at its ordinary deadline (the
    /// session log's block read stopped at its cap with blocks left): the
    /// follow-up [`Self::next_wake`] arms at the slot's next due instant,
    /// without moving the output stamp a burst's final-frame look waits on,
    /// so a group handback's immediate look ([`Self::agent_observe`]) stays
    /// immediate. A level, like [`Self::note_program_answered`].
    pub(crate) fn owe_look(&mut self, session: u64) {
        if self.sessions.contains_key(&session)
            && let Some(w) = self.agents.get_mut(&session)
        {
            w.followup = true;
        }
    }

    /// Whether this observation should read the live zone (the caller holds the terminal
    /// guard): the generation, the cursor or the published program moved since the last read,
    /// and the [`AGENT_MIN_INTERVAL`] floor allows it. A deferred read arms a follow-up.
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
        let program_moved = w.program_seen.as_ref() != Some(program);
        let moved =
            w.seq_seen != Some(generation) || w.cursor_seen != Some(cursor) || program_moved;
        if !moved {
            return false;
        }
        // A new program is floored by the last classification, so its transition stays
        // prompt; screen and cursor moves are floored by the last read.
        let last = if program_moved {
            w.classified_at
        } else {
            w.read_at
        };
        let floor = last.is_none_or(|at| now.saturating_duration_since(at) >= AGENT_MIN_INTERVAL);
        if !floor {
            w.followup = true;
        }
        floor
    }

    /// One observation's agent step, after the terminal guard is released: `rows` is the live
    /// zone when [`Self::agent_zone_wanted`] asked for it, `cursor` read under the same guard,
    /// `pgid` the foreground group, `resolving` whether a resolution was just requested.
    /// Classifies ([`crate::presence::agent_verdict`]) only when the zone's hash or the program
    /// changed.
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
        // The group moved since the last look: `program` (read before this sweep's
        // `note_foreground_group`) names the group that left, so the zone is judged under no
        // name, never under the departed agent's. A first look (`-1`) takes the name as read.
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
            // A later exec in this group can make the next lookup miss: a known name starts a
            // fresh retry episode.
            w.resolve_attempts = 0;
            // The group-change sweep can still carry the old group's program, so only a later
            // observation arms a confirmation; after one, a new screen move arms one more,
            // never faster than the four-second floor.
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
                // A name that identifies an agent needs no confirmation lookup.
                w.deferred_name_recheck_at = None;
                if !group_moved && !resolving {
                    w.name_confirmed = true;
                }
            }
        } else {
            // With no name, the bounded unknown-name retry owns the group.
            w.deferred_name_recheck_at = None;
            w.name_confirmed = false;
        }
        let Some(rows) = rows else {
            // Not read (unchanged, or deferred by the floor): look again next interval if a
            // moved screen or program, or an unread resolution, is still owed. A handback is to
            // a real group; a move to no group (`-1`) hands no frame back and owes no wake.
            let handback = group_moved && pgid > 0;
            w.followup = resolving || program_moved || screen_moved || handback;
            if group_moved {
                // The read gate ran before the foreground-group syscall, so a recent
                // unchanged-zone read can suppress the frame that hands an exited agent back to
                // the shell: drop the old program verdict and, when the classification floor
                // allows, owe one immediate follow-up.
                w.program_seen = None;
                if handback
                    && w.classified_at
                        .is_none_or(|at| now.saturating_duration_since(at) >= AGENT_MIN_INTERVAL)
                    && let Some(slot) = self.sessions.get_mut(&session)
                {
                    // A group handback, not the final-frame follow-up that waits for output to
                    // pause.
                    w.output_at = None;
                    slot.next_due = now;
                    self.next_due_any = Some(self.next_due_any.map_or(now, |due| due.min(now)));
                }
            }
            return AgentStep::default();
        };
        // Charge every successful read, not only a changed verdict: a repaint can move the
        // generation without the zone hash, and a 50 ms interval would read at 20 Hz, not 4 Hz.
        w.read_at = Some(now);
        let moved = screen_moved;
        w.seq_seen = Some(generation);
        w.cursor_seen = Some(cursor);
        w.followup = moved || resolving;
        // The zone the verdict reads ([`crate::presence::live_zone`]) and the cursor's row.
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
        // What the screen counts, for the survey: the context left the
        // reading carries and, for Claude Code, its background shells.
        let gauges = crate::session_timeline::AgentGauges {
            ctx_pct: w.reading.as_ref().and_then(|r| r.context_pct),
            shells: match verdict.program() {
                Some(aterm_phase::Program::Claude) => aterm_phase::shells_running(&rows),
                // Only Claude Code's footer is read for its shells; the
                // other agents' screens draw no count measured yet.
                Some(
                    aterm_phase::Program::Codex
                    | aterm_phase::Program::Agy
                    | aterm_phase::Program::Muse
                    | aterm_phase::Program::Gemini
                    | aterm_phase::Program::Generic,
                )
                | None => None,
            },
        };
        AgentStep {
            reading_changed,
            turn: verdict.turn(),
            publish: Some((verdict.word(), verdict.detail(), subject, verdict.program())),
            gauges,
        }
    }

    /// Whether `session`'s foreground program should be (re-)named now: its
    /// group just changed (`group_changed`), it has never been asked for while
    /// unnamed (`first_unnamed`: a restored session whose foreground group was
    /// adopted before the first status sweep), an unknown name has reached its
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
            let first_unnamed = program.is_none() && w.resolved_at.is_none();
            let confirmation_due = program.is_some_and(|p| aterm_phase::program_of(p).is_none())
                && w.pgid > 0
                && w.deferred_name_recheck_at.is_some_and(|at| now >= at);
            first_unnamed
                || confirmation_due
                || w.resolved_at.is_some_and(|at| {
                    let age = now.saturating_duration_since(at);
                    (program.is_none()
                        && w.pgid > 0
                        && age >= unresolved_program_recheck(w.resolve_attempts))
                        || (w.seq_seen != Some(generation) && age >= PROGRAM_RECHECK)
                })
        })
    }

    /// Charge one resolver request and advance its bounded retry episode.
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
            // A confirmation was requested; another waits for screen movement and the
            // named-program floor.
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
        proxy: Option<&aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
    ) {
        self.note_program_request(session, pgid, now);
        self.programs.request(session, timeline, pgid, shell, proxy);
    }

    /// The session log's wake the resolver's footer reads end with
    /// ([`crate::claude_footer::LogWake`]): set once a sweep, before it asks.
    pub(crate) fn set_log_wake(&mut self, wake: &crate::claude_footer::LogWake) {
        self.programs.set_log_wake(wake);
    }

    pub(crate) fn revision(&self, session: u64) -> u64 {
        self.sessions.get(&session).map_or(0, |slot| slot.revision)
    }

    /// Fold the running-command `detail`, read under the sweep's own guard, onto `session`'s
    /// published status, so the chrome's `running <detail>` and `status detail=` agree. No slot
    /// yet is a no-op. Bumps the revision so chrome recomposes when only the program changed;
    /// returns whether it moved.
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

    /// Fold one sweep's OSC 7501 reading for `session`
    /// ([`crate::program_state::ProgramStateGate::observe`]). Whether what it
    /// shows changed.
    pub(crate) fn observe_program_state(
        &mut self,
        session: u64,
        report: aterm_core::terminal::program_state::ProgramStateReport,
        honoured: bool,
        group_moved: bool,
        now: Instant,
    ) -> bool {
        self.program_states
            .entry(session)
            .or_default()
            .observe(report, honoured, group_moved, now)
    }

    /// The program's own state report `session` shows, and since when
    /// (`status program_state=`, the needs border's hover).
    pub(crate) fn program_state(
        &self,
        session: u64,
    ) -> Option<(aterm_core::terminal::program_state::ProgramState, Instant)> {
        self.program_states
            .get(&session)
            .and_then(crate::program_state::ProgramStateGate::shown)
    }

    /// How many turn ends the program in front of `session` has reported
    /// (OSC 7501 `working` → `done`|`idle`, as shown): presence's story point
    /// for it, and the tab's `•` where nobody watched. Display only.
    pub(crate) fn program_turn_ends(&self, session: u64) -> u64 {
        self.program_states
            .get(&session)
            .map_or(0, crate::program_state::ProgramStateGate::turn_ends)
    }

    /// How many turns the program in front of `session` has begun (OSC 7501
    /// `working` after no turn in progress, as shown): what tells the band
    /// that a turn end and a ledger settle are the same turn. Display only.
    pub(crate) fn program_turn_starts(&self, session: u64) -> u64 {
        self.program_states
            .get(&session)
            .map_or(0, crate::program_state::ProgramStateGate::turn_starts)
    }

    /// The engine's count of the turns `session`'s program began (OSC 7501,
    /// `ProgramStateReport::turns`), as the last sweep read it: what a proven
    /// self-claim's landing reopens past (`App::reopen_self_claim_landing`).
    /// `0` before a sweep read it.
    pub(crate) fn program_turns(&self, session: u64) -> u64 {
        self.program_states
            .get(&session)
            .map_or(0, crate::program_state::ProgramStateGate::turns)
    }

    /// When the program in front of `session` began its latest counted turn (as
    /// shown), `None` before any: a ledger settle pairs with that turn only when
    /// it began no earlier than the ledger turn opened. Display only.
    pub(crate) fn program_turn_began(&self, session: u64) -> Option<Instant> {
        self.program_states
            .get(&session)
            .and_then(crate::program_state::ProgramStateGate::turn_started)
    }

    /// Drop a retired session's state.
    pub(crate) fn retire(&mut self, session: u64) {
        self.agents.remove(&session);
        self.program_states.remove(&session);
        let _ = self.inputs.forget(session);
        self.programs.retire(session);
        // The footer resolver's watch and usage fold too.
        crate::claude_footer::stop_session(session);
        self.footer_asked.remove(&session);
        // And its rollout tap.
        crate::codex_rollout_tap::retire_session(session);
        self.rollout_asked.remove(&session);
        if self.sessions.remove(&session).is_some() {
            // The removed slot may have been the minimum; retirement is rare, so recompute.
            self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
        }
    }

    /// Adopt a new policy at runtime; returns whether anything moved. Every live slot is
    /// rewritten, since each [`StatusFsm`] holds its own copy of the policy, and `next_due` is
    /// clamped down so a shortened interval takes effect at once.
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
        // Every deadline just moved down, so the gate's bound follows.
        self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
        true
    }

    /// Forget every session's classifier state, when `tab_status` is turned off.
    pub(crate) fn clear(&mut self) -> bool {
        let had = !self.sessions.is_empty();
        self.sessions.clear();
        self.programs.clear_pending();
        self.footer_asked.clear();
        self.rollout_asked.clear();
        // The agent readings describe the same stopped subsystem: retire them
        // (presence folds the post-clear sequence as "no reading").
        self.agents.clear();
        self.agent_seq += 1;
        self.agent_cleared_at = self.agent_seq;
        // No slots, no known deadline: the gate is open for the re-enabled subsystem.
        self.next_due_any = None;
        self.unclassified.clear();
        had
    }
}

impl crate::App {
    /// Classify every live session that is due, gathering evidence under `try_lock` only.
    /// Returns the sessions whose published status or agent reading changed; the caller
    /// refreshes their chrome and presence.
    ///
    /// On the output path: a session not due costs a map lookup and never touches its lock, so
    /// an output flood is no classification flood. Contention is a skip, delaying a transition
    /// by at most the interval. `tab_status = false` returns before touching the pool.
    pub(crate) fn observe_session_statuses(&mut self, now: std::time::Instant) -> Vec<u64> {
        if !self.config.tab_status_or_default() {
            // The edged inbox's pushes do not stop with the sweep (the PTY
            // readers know no config): emptied here, so a session's id is
            // never kept past the wake it rode.
            drop(self.session_factory.edge_inbox.take());
            return Vec::new();
        }
        // The O(1) deadline gate. This runs at the PTY reader's batch rate (thousands of times
        // a second, at 120 sessions ~360k map probes/s if it folded the pool), so three terms
        // decide whether to sweep: `any_due` covers every known session; the pool epoch covers
        // a new session with no slot, due at once; and the newcomers a sweep could not classify
        // because the reader held their terminal are recorded (`StatusObserver::note_swept`)
        // and retried alone, rather than re-walking the pool on every wake.
        let pool_epoch = self.pool.insert_epoch();
        let retry_only = pool_epoch == self.session_status.swept_pool_epoch()
            && !self.session_status.any_due(now);
        // THE EDGED INBOX (card engine §3.5): the sessions whose PTY reader
        // published a holder image since the last sweep, taken first and
        // visited ahead of everything else, each whether or not it is due, so
        // a drawer's program is named on the wake after its edge rather than
        // at its next observation. A visit the PTY reader's hold refuses goes
        // back in the inbox for the next wake and charges nothing itself: the
        // pool walk reaches the session again, and charges a due one's
        // refusal as it charges any, so its owed instant never stays past.
        let edged = self.session_factory.edge_inbox.take();
        if retry_only && !self.session_status.has_unclassified() && edged.is_empty() {
            return Vec::new();
        }
        // A sweep that visits sessions, timed for `metrics`' `status_sweeps`
        // (card engine §8): the gate's O(1) answer above is none.
        let _timed = crate::metrics::SweepTimer::start();
        let retry = if retry_only {
            self.session_status.take_unclassified()
        } else {
            Vec::new()
        };
        let pool = &self.pool;
        // `retry` is empty unless `retry_only`, so after the edged sessions
        // the chain is either the retry ids' live sessions or the whole pool,
        // in the same order. Each is paired with whether it is an edged visit.
        let sessions = edged
            .iter()
            .filter_map(|id| pool.get(*id))
            .map(|session| (session, true))
            .chain(
                retry
                    .iter()
                    .filter_map(|id| pool.get(*id))
                    .chain((!retry_only).then(|| pool.iter()).into_iter().flatten())
                    .map(|session| (session, false)),
            );
        let mut changed = Vec::new();
        // The proven claims this sweep cleared: their chrome is told once the
        // pool is no longer borrowed (below).
        let mut claims_cleared = Vec::new();
        // The sessions whose citation hovers a new naming of the front owes a
        // re-ask (`App::reask_renamed_hovers`), after the loop.
        let mut hovers_renamed = Vec::new();
        let mut clauses_moved: Vec<u64> = Vec::new();
        // The sessions whose program this sweep newly showed `done` (OSC
        // 7501): an `fx celebrate on=done` arm bound to one fires after the
        // loop (`App::note_program_done`).
        let mut program_done = Vec::new();
        // The sessions whose agent word moved or whose program began a turn
        // since the last sweep read it: a proven self-claim's landing reopens
        // at a turn anyone begins after it (`App::reopen_self_claim_landing`,
        // FOREVER LAND §10.4), after the loop.
        let mut turns_moved = Vec::new();
        // What a footer read the sweep starts wakes when it moves a session's
        // log (session log Phase 3B, `claude_footer::LogWake`).
        let footer_wake = self.footer_log_wake();
        self.session_status.set_log_wake(&footer_wake);
        // The sweep never changes pool membership: borrow each due session
        // directly instead of collecting ids, looking them up again, and
        // cloning its terminal Arc on every observation interval.
        for (session, edged_visit) in sessions {
            #[cfg(test)]
            crate::work_counts::status_probe();
            let id = session.id;
            // An edged session is visited first, due or not, and the pool
            // walk takes it as it takes any other: a visit observed it, which
            // leaves it not due, and a refused one leaves a due session due,
            // to be tried and charged there (`note_skipped`). Skipped here,
            // a due slot refused on its edged visit kept its past `next_due`,
            // re-arming the loop every turn the reader's hold won.
            if !edged_visit && !self.session_status.due(id, now) {
                continue;
            }
            let (master, pid) = (session.master, session.pid);
            // PTY-output age from the session's activity atomic: set for every consumed burst
            // and never cleared by presentation, so a hidden pane's output still ages.
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
            //
            // An edged visit refused is owed on the next wake (the inbox) and
            // charges nothing: the pool walk below decides that, as for any
            // session.
            let Ok(guard) = session.term.try_lock() else {
                if edged_visit {
                    self.session_factory.edge_inbox.push(id);
                } else {
                    self.session_status.note_skipped(id, now);
                }
                continue;
            };
            // Field reads and a block peek, then the lock is released: the PTY reader must not
            // wait behind a syscall. `detail=` comes from this same guard, so the chrome's
            // `running <detail>` and the wire's `status detail=` share one producer.
            let shell = shell_evidence(&guard);
            let reports_commands = crate::quit_safety::shell_reports_commands(&guard);
            // `status integration=degraded`, from the SAME guard: the lost
            // key the tab's notice speaks for (`integration_notice`). Read
            // once, with the engine's other session-log facts (phase 3B:
            // notifications, bells, progress), written once it is released.
            let engine = crate::session_log::observed::EngineRead::of(&guard);
            let integration_degraded =
                engine.integration == aterm_core::terminal::ShellIntegrationPosture::Degraded;
            let completed_block = completed_block(&guard);
            let alt_screen = guard.is_alternate_screen();
            let content_seq = guard.content_seq();
            let generation = crate::control::screen_gen(&guard);
            // The resize ledger's probe: four integer loads.
            let resize_probe = crate::resize_ledger::EngineProbe::sample(&guard);
            let detail = executing_detail(&guard);
            // The cwd report the engine holds, witnessed below under the group
            // this sweep finds in front (`SessionTimeline::witness_cwd_report`),
            // with the group in front read under this SAME hold: one `tcgetpgrp`
            // ioctl, the reader's wait no longer than it.
            //
            // ONE READ PER LOOK: the job evidence and the program identity
            // below take this same answer. On macOS the ioctl on a PTY master
            // costs ~51k instructions (2.5 us), and a look asked it three
            // times: at 40 idle tabs, every sweep a new tab's wakes and owed
            // looks run observed all of them, and those reads were 86% of the
            // main thread's sweep and most of its per-tab cost of a spawn.
            let cwd_report = guard.reported_cwd_seq();
            let pgid = if master >= 0 {
                (self.session_factory.fg_probe)(master)
            } else {
                -1
            };
            // THE PROGRAM'S OWN STATE (OSC 7501): one copy under this guard,
            // gated below once the foreground move is known.
            let program_report = guard.program_state();
            // THE SESSION LOG's block read, from this SAME guard: every block
            // past the log's cursor (the timeline is a strict leaf, taken here
            // as for `program` below), written to the log once the guard is
            // released.
            let (from, since_wall) = {
                let timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                (
                    timeline.slog().block_cursor(),
                    timeline.slog().opened_wall(),
                )
            };
            let blocks = block_read(&guard, from, since_wall, detail.clone());
            // THE AGENT VERDICT's one read of the screen, under this SAME
            // guard: the live zone's rows, only when the content moved since
            // the last read (and at most 4 Hz). Classified after the guard is
            // released — the PTY reader never waits on `aterm_phase`.
            // The published program is read here, under the guard (the
            // timeline is a strict leaf: nothing holding it takes a terminal),
            // so the zone is judged under the program it was read with. With
            // it, the agent in front as the host knows it: by its name, or a
            // runtime like `node` by its screen (`harness_host::agent_of`).
            let (program, front_agent) = {
                let timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                (
                    timeline.agent().program.clone(),
                    crate::harness_host::agent_of(timeline.agent()),
                )
            };
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
            // The text is built under the guard and published to the session's
            // slot once the guard is gone, so the `status`, `text` and `turn`
            // reads of this screen that follow are served it. No look first: a
            // contended session costs one `try_lock` and nothing else.
            let read = self
                .session_status
                .agent_zone_wanted(id, generation, cursor, &program, now)
                .then(|| std::sync::Arc::new(crate::frame_memo::ScreenText::of(&guard)));
            drop(guard);
            let (zone, stamp) = match read {
                Some(text) => {
                    // The whole screen: the verdict reads its live zone (the
                    // last CLASSIFY_ROWS of the content, `presence::live_zone`),
                    // and the whole for a box the zone cuts
                    // (`presence::agent_verdict`).
                    let (fp, zone) = screen_agent_frame(&text, usize::MAX);
                    session.ctx.frame.publish_text(text);
                    let stamp = crate::session_timeline::AgentStamp { generation, fp };
                    (Some(zone), Some(stamp))
                }
                None => (None, None),
            };
            let last_output = match latest_output_ns {
                0 => None,
                ns => self
                    .lat_epoch
                    .checked_add(std::time::Duration::from_nanos(ns)),
            };
            // The keystroke stamp of the window this session is FOCUSED in —
            // the only session a human is typing into; a background tab's key
            // is `None`. Keystrokes are job evidence: a shell cannot start a
            // foreground child without consuming input.
            let last_input = self.windows.values().find_map(|ws| {
                ws.front_terminal()
                    .is_some_and(|t| t.session == id)
                    .then_some(ws.last_key_at)
                    .flatten()
            });
            // Foreground-job PRESENCE only: `tcgetpgrp` gives a Boolean, not a
            // process name (the name is not available on the session path at
            // all), so the classifier is deliberately built to need only this.
            //
            // Routed through `App::job_probe` rather than calling
            // `foreground_is_job` directly. On unix that is the group read
            // under the hold above, the per-fd `tcgetpgrp` this always was.
            // On WINDOWS there is no `tcgetpgrp`, so the answer comes from a
            // system-wide process-table walk (~3 ms measured) — and calling it
            // here, once per due session per sweep, is what made an idle
            // EIGHT-tab window cost ~4x an idle one-tab window at the same two
            // frames a second: eight walks every 250 ms, ~100 ms of CPU per
            // idle second, none of it visible to the scheduler's deadline
            // counters because it rides the per-turn sweep rather than any
            // armed wake. `JobProbe` keeps the verdict and its freshness rule
            // and removes only the duplication: one capture per sweep for every
            // due session, and one per `quit_safety::JOB_PROBE_MAX_AGE` while
            // no session's evidence moves.
            let foreground_job = (master >= 0 && pid > 0).then(|| {
                self.job_probe.is_job(
                    id,
                    pgid,
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
            // A prompt-only integration (cmd.exe's) never says a command runs, so
            // its prompt yields to a live job ([`prompt_yields_to_job`]).
            let shell = prompt_yields_to_job(shell, reports_commands, foreground_job);
            let evidence = Evidence {
                shell,
                completed_block,
                // The exit arrives as an event (`note_session_exit`), sticky on the slot.
                lifecycle: None,
                foreground_job,
                activity: ActivitySample {
                    alt_screen,
                    content_seq,
                    last_output,
                },
            };
            // The running command is a separate fact from the phase; either moving refolds the
            // tab, so `running targo test` -> `running claude` under `Running` still repaints.
            let status_changed = self.session_status.observe(id, &evidence, now);
            let detail_changed = self.session_status.set_detail(id, detail);
            // A failure published while the person is looking at this session (focused view of
            // a focused window) is seen at once and never marks the tab (`FailureMark`).
            let seen = self.windows.values().any(|ws| {
                crate::os_window_focused(ws) && ws.front_terminal().is_some_and(|t| t.session == id)
            }) && self.session_status.note_seen(id);
            // One timeline event per published transition; dwell bounds them, so a spinner
            // cannot flood the ring.
            let status_event = status_changed
                .then(|| self.session_status.take_status_transition(id))
                .flatten();
            if let Some((payload, facts)) = status_event.as_ref() {
                let mut timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                timeline.record(STATUS_CHANGE, payload.clone());
                // THE SESSION LOG's typed `status` mark, in the same hold.
                let at = timeline.slog().now();
                timeline.slog_mut().observe_status(*facts, at);
            }
            // PROGRAM IDENTITY + AGENT VERDICT. The foreground group is the
            // one read under the hold above (the job probe took the same
            // answer); only a CHANGED group costs a resolution, and that runs
            // off this thread. The verdict is published on the session's
            // timeline — the store `status`, `sessions`, `await agent` and the
            // events digest all read — and a move records `agent-change`,
            // which wakes this session's watchers at once. Read through the
            // factory's injected probe (`tcgetpgrp` in shipping), so a test
            // puts a group in front with no process.
            // THE SESSION LOG's shell blocks, then a failure the person saw
            // (`seen` above), then the foreground move, in that order
            // (`SessionTimeline::sweep_slog`): a command the shell reported
            // ended closes at its own D, and with it the program and agent
            // runs that ran under it, so no child closes after its parent at
            // the time this sweep noticed.
            // The settle wake read under the same hold: a turn a seamless
            // update carried resting is owed its tick before any verdict
            // moves.
            let (previous_group, wake) = {
                let mut timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                timeline.witness_cwd_report(cwd_report, pgid);
                let previous = timeline.sweep_slog(&blocks, seen, pid, pgid);
                // The engine's marks (phase 3B), on what is open once the
                // blocks and the foreground move were written.
                let at = timeline.slog().now();
                timeline.slog_mut().observe_engine(&engine, at);
                // A program state its run's bucket held, and a shown tuple
                // the session's did, is written once a token frees (6F-4,
                // 6F-9; U44).
                timeline.slog_mut().program_tick(at);
                timeline.slog_mut().shown_tick(at);
                (previous, timeline.slog_wake())
            };
            self.session_status.slog_wakes.note(id, wake);
            let group_changed = previous_group.is_some();
            // THE PROVEN CLAIM LEAVES WITH ITS PROCESS (the self-claim
            // contract, §2): a proven claim whose agent no longer leads the
            // group in front, or has exited, is cleared as the group moves
            // (`meta-change field=supervisor value=-`), so aterm's own lane
            // and the agent's menu rows come back. Leaf locks, after the
            // timeline's is let go; the process is looked at outside them.
            if group_changed
                && crate::session_timeline::release_proven_if_gone(
                    &session.ctx,
                    pgid,
                    crate::metrics::now_us(),
                )
            {
                claims_cleared.push(id);
            }
            // THE PROGRAM'S OWN STATE (OSC 7501, the native harness contract
            // §6): shown only while the program in front names `codex` (or a
            // runtime that hosts one, npm's `node`), and only a report newer
            // than the sweep before the group moved.
            // The PTY does not attribute unseen output to a writer. A claim:
            // no verdict reads it; each move is a row and a `program` log mark.
            // Beside it, the engine's count of the turns the reports began:
            // one that moved since the last sweep's reading is a turn begun,
            // seen running or not (a turn that began and ended between two
            // sweeps leaves the same report standing).
            let turn_begun = program_report.turns != self.session_status.program_turns(id);
            let program_state_moved = self.session_status.observe_program_state(
                id,
                program_report,
                crate::program_state::honoured_for(program.as_deref()),
                group_changed,
                now,
            );
            if turn_begun {
                turns_moved.push(id);
            }
            // The disk tick's roster reads it on the host's thread: a vendor
            // `codex` under `node` is named by its report alone.
            if program_state_moved {
                let shown = self
                    .session_status
                    .program_state(id)
                    .map(|(state, _)| state);
                // THE PROGRAM'S TURN LEDGER: every honoured move is one
                // `program-state` row on the session's timeline and, in the
                // same hold, one `program` mark in its log, coalesced past
                // the run's bucket (`SessionTimeline::note_program_state`).
                // A leaf lock; nothing reads the row for a decision.
                session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .note_program_state(shown);
                if shown.is_some_and(|state| {
                    state.state == aterm_core::terminal::program_state::ProgramStateWord::Done
                }) {
                    program_done.push(id);
                }
                crate::harness_host::note_codex_report(
                    session.ctx.self_id.as_str(),
                    shown.is_some(),
                );
            }
            if let Some(previous) = previous_group {
                crate::claude_footer::stop(id, previous);
                // A program started or ended: the crash journal names the leaves that run one, and
                // a silent job start wakes nothing else it would capture on.
                self.crash_journal.note_activity();
            }
            // An edged visit owes a naming as a moved group does: its edge may
            // be an `exec` in place, which moves no group (card engine §3.5).
            let resolve = pgid > 0
                && self.session_status.program_due(
                    id,
                    group_changed || edged_visit,
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
            // A live PATH measurement lowers the registry's frozen mark, so `path=`, the
            // handoff carry and the managed-current count read the measured fact. Never raised:
            // one job's exec environment (a `PATH=` override reads frozen) cannot mean "adopted
            // from before this update". Owed once per live reading.
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
            // The Claude Code footer facts, re-read at most every RECHECK while it is in front
            // (`crate::claude_footer`).
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
                &footer_wake,
                edged_visit,
            );
            // THE CODEX ROLLOUT TAP: a codex-alab's own rollout read on into
            // its session log while it is in front (`crate::codex_rollout_tap`).
            self.session_status.request_rollout_if_due(
                id,
                &session.ctx,
                if group_changed {
                    None
                } else {
                    program.as_deref()
                },
                now,
                &footer_wake,
            );
            // While integration is degraded, what gives it back (`integration_notice`): the
            // tab's re-key channel, the harness's move of the agent in front (only when it will
            // type that move's line), or a new tab. Only a moved reading is said, after the loop.
            let heal = integration_degraded.then(|| {
                let channel = self
                    .store
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .has_rekey_channel(id);
                let moves = self.harness.as_ref().is_some_and(|host| {
                    host.rekeys_agents() && front_agent.is_some_and(|agent| host.admits(agent))
                });
                // A Codex is relaunched only on a line aterm's copy of
                // codex-alab's own record validated.
                let codex_line = crate::self_claim::copy_of_tab(session.ctx.self_id.as_str())
                    .is_some_and(|copy| copy.resume.is_ok());
                crate::integration_notice::heal_for(channel, front_agent, moves, codex_line)
            });
            let _ = self.integration_notices.observe(id, heal, now);
            let step = self
                .session_status
                .agent_observe(id, generation, zone, cursor, program, pgid, resolve, now);
            // A block read cut at its cap owes the rest one more look at the
            // slot's next due instant (after `agent_observe`, which sets the
            // follow-up from what it read; a handback's immediate look stands).
            if blocks.more {
                self.session_status.owe_look(id);
            }
            // The grid the read was taken at, under its guard: a re-grid's
            // text is no move of the program's (`AgentPublication::text_moves`).
            let grid = (resize_probe.lifetime, resize_probe.ordinal);
            let moved = match (step.publish, stamp) {
                (Some((word, detail, subject, reader)), Some(stamp)) => {
                    let (moved, wake) = {
                        let mut timeline = session
                            .ctx
                            .timeline
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        // THE SESSION LOG (phase 3B): a turn a person
                        // stopped closes `out=interrupted` (told before
                        // the verdict that settles it); the context left
                        // and the goal are marked on the run it published.
                        if step.turn.interrupted {
                            timeline.slog_mut().agent_interrupted();
                        }
                        timeline.set_gauges(step.gauges);
                        timeline.note_grid(grid, now);
                        let moved = timeline.publish_agent(word, detail, subject, reader, stamp);
                        let at = timeline.slog().now();
                        timeline.slog_mut().observe_screen_facts(
                            step.turn.ctx_pct,
                            step.turn.goal,
                            at,
                        );
                        (moved, timeline.slog_wake())
                    };
                    // The session log's settle wake, owed by the loop
                    // (`session_log::Wakes`): one entry while a turn rests.
                    self.session_status.slog_wakes.note(id, wake);
                    moved
                }
                // Read, and the zone was unchanged: the verdict stands on this
                // newer screen.
                (None, Some(stamp)) => {
                    let mut timeline = session
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    timeline.note_grid(grid, now);
                    timeline.note_agent_stamp(stamp);
                    false
                }
                _ => false,
            };
            // THE A/B CLOCK (Settings ▸ Harness): a moved word closes the
            // stretch the last one opened, charged to the arm then in force.
            // At the word's own instant: no clock is read here.
            if moved {
                let arm = self.harness_ab_arm(id);
                let glass = self.harness_ab_glass(id);
                self.harness_screen.note_phase(id, &session.ctx, arm, glass);
                turns_moved.push(id);
            }
            // THE RESIZE LEDGER, evaluated on this sweep so a `render` verdict
            // transition lands on the timeline (and wakes the digest) without
            // waiting for a reader. The SELF-AUDIT: an engine past the ledger
            // (or another engine) took a resize no path booked, so its journal
            // is copied under a second `try_lock` taken after the timeline is
            // released (never nested, never waited on) and backfilled as
            // `site=-`.
            let now_ms = crate::turn_ledger::now_ms();
            // The session log's owed wake is taken here too, after every
            // append this pass made (its blocks, a failure seen, the verdict):
            // an owed one rings the session's watchers (phase 3A,
            // `SessionTimeline::slog_take_wake`).
            let (behind, log_moved) = {
                let mut timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let _ = timeline.evaluate_resizes(resize_probe, now_ms);
                (
                    timeline.resizes().behind(&resize_probe),
                    timeline.slog_take_wake(),
                )
            };
            let journal = if behind {
                session
                    .term
                    .try_lock()
                    .ok()
                    .map(|t| crate::resize_ledger::JournalCopy::take(&t))
            } else {
                None
            };
            // A render mark the backfill writes comes after the wake was
            // taken above: it takes its own (addendum A22).
            let backfill_logged = journal.is_some_and(|copy| {
                let mut timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let _ = timeline.ingest_resizes(&copy, None, now_ms);
                timeline.slog_take_wake()
            });
            // The sweep's moves reach the tab's clause (phase 3B; addendum
            // A12 item 4), refreshed below once the pool is let go.
            if (log_moved || backfill_logged) && self.clause_moved(id) {
                clauses_moved.push(id);
            }
            if status_changed
                || detail_changed
                || seen
                || step.reading_changed
                || program_state_moved
            {
                changed.push(id);
            }
            // A CITATION'S HOVER answered under another naming of the group in
            // front (`HoverMemo::renamed`): re-asked once the pool is let go
            // (below). Only a window hovering this session's citation pays the
            // leaf lock.
            if self.windows.values().any(|ws| {
                ws.hover_memo
                    .as_ref()
                    .is_some_and(|memo| memo.answered_citation_of(id))
            }) {
                let timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let named = (
                    timeline.agent().program_pgid,
                    timeline.agent().program.as_deref(),
                );
                if self.windows.values().any(|ws| {
                    ws.hover_memo
                        .as_ref()
                        .is_some_and(|memo| memo.renamed(id, named))
                }) {
                    hovers_renamed.push(id);
                }
            }
        }
        for id in hovers_renamed {
            self.reask_renamed_hovers(id);
        }
        for id in clauses_moved {
            self.refresh_session_status_chrome(id);
        }
        for id in program_done {
            self.note_program_done(id, now);
        }
        for id in turns_moved {
            self.reopen_self_claim_landing(id);
        }
        // THE CARD'S ARTS, AHEAD (card engine §4.3): a session whose PTY
        // reader published a card drawer's image is a card about to be
        // drawn; each window showing it has the card's arts for its cell
        // height queued for residency now. The first admitted ask may still
        // build during drawing; this queue does not promise a prior upload.
        for &id in &edged {
            self.prefetch_card_arts_for(id);
        }
        // A cleared proven claim moves what a released one moves (the
        // `Wake::MetaChanged` arm); its `meta-change` row rang the session's
        // watchers, so it is pushed as `EVENT meta`.
        for id in claims_cleared {
            self.on_meta_changed(id);
        }
        // Said before either exit: a newcomer retry can move a reading too. One word per sweep.
        if self.integration_notices.owes() {
            self.say_integration();
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
        // A full pass makes the gate's bound exact: O(slots), only when something was due.
        // Sessions a lost `try_lock` left slot-less are recorded for the next wake's retry
        // (`StatusObserver::note_swept`).
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
    /// Called at the top of the `Wake::Exit` arm while the child is still an unreaped zombie,
    /// the only window in which its status can be collected (`aterm_pty` reaps on the teardown
    /// thread and discards it). This is the exit path's first look
    /// ([`crate::app_tabs::ExitLook::First`]). An uncollectable status (adopted session, master
    /// gone unreadable, child already reaped) becomes `Outcome::None`, never a made-up success.
    ///
    /// It matters under `--hold` or a shell that failed at start, which keep the pane: without
    /// it the next sweep reads the failing `tcgetpgrp` as no job and shows the dead pane `Idle`.
    pub(crate) fn note_session_exit(&mut self, session: u64) {
        self.note_exit_of(session, None);
    }

    /// A held pane's carried exit: the exit an update's successor shows again for a pane
    /// `--hold` kept open, classified as [`Self::note_session_exit`] does but not news. Its
    /// failure mark stays as seen as the person left it (`seen`,
    /// `session_store::HeldPaneCarry::failure_seen`), and no `status-change` is recorded or
    /// pushed: the old process recorded it.
    ///
    /// `block` is the completed block the held engine's checkpoint carries ([`completed_block`]):
    /// every successor sweep reads that block again, so the mark names it from the start
    /// rather than taking it for a new failed command and re-arming.
    pub(crate) fn note_carried_exit(
        &mut self,
        session: u64,
        seen: bool,
        block: Option<CompletedBlock>,
    ) {
        self.note_exit_of(session, Some(CarriedExit { seen, block }));
    }

    /// [`Self::note_session_exit`], or a carried exit
    /// ([`Self::note_carried_exit`]).
    fn note_exit_of(&mut self, session: u64, carried: Option<CarriedExit>) {
        if !self.config.tab_status_or_default() {
            return;
        }
        let Some(timeline) = self
            .pool
            .get(session)
            .map(|pooled| pooled.ctx.timeline.clone())
        else {
            return;
        };
        // `exit_status` reaps the zombie and latches that on the session, so teardown cannot
        // later `killpg` a reissued pid (under `--hold` the session outlives this by minutes).
        let collected = self.exit_status(session, crate::app_tabs::ExitLook::First);
        let lifecycle = match collected {
            Some(aterm_pty::ChildExit::Code(code)) => Lifecycle::Exited {
                exit_code: Some(code),
            },
            Some(aterm_pty::ChildExit::Signal(signal)) => Lifecycle::Signalled { signal },
            None => Lifecycle::Exited { exit_code: None },
        };
        // A lifecycle exit short-circuits classification, so no terminal lock is taken here.
        // A carried exit names its checkpoint's block, which only [`FailureMark::note`] reads.
        let evidence = Evidence {
            shell: None,
            completed_block: carried.and_then(|carried| carried.block),
            lifecycle: Some(lifecycle),
            foreground_job: None,
            activity: ActivitySample {
                alt_screen: false,
                content_seq: 0,
                last_output: None,
            },
        };
        let moved =
            self.session_status
                .note_exit(session, lifecycle, &evidence, std::time::Instant::now());
        // A carried exit's event is taken and dropped, so no later sweep records it.
        let event = moved
            .then(|| self.session_status.take_status_transition(session))
            .flatten()
            .filter(|_| carried.is_none());
        if let Some((payload, facts)) = event {
            let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.record(STATUS_CHANGE, payload);
            // THE SESSION LOG's typed `status` mark, in the same hold, and
            // its owed wake taken there (`slog_take_wake` rings the watchers).
            let at = timeline.slog().now();
            timeline.slog_mut().observe_status(facts, at);
            let _ = timeline.slog_take_wake();
        }
        let quieted =
            carried.is_some_and(|carried| carried.seen) && self.session_status.note_seen(session);
        if quieted {
            // THE SESSION LOG: the failure the carry says was seen is seen here,
            // and its readers woken in the same hold (ruling R9).
            let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
            let at = timeline.slog().now();
            timeline.slog_mut().failure_seen(at);
            let _ = timeline.slog_take_wake();
        }
        if moved || quieted {
            self.refresh_session_status_chrome(session);
        }
    }

    /// The person is looking at `session` in `wid` when it is that window's focused view and
    /// the window has keyboard focus: its failure mark is seen ([`FailureMark`]). Called on a
    /// tab or pane switch (`sync_active_session`) and window focus (`on_focus`); the sweep covers
    /// a failure published while the session is already in front.
    pub(crate) fn note_session_seen(&mut self, wid: crate::WindowId, session: u64) {
        let looking = self
            .windows
            .get(&wid)
            .map(Box::as_ref)
            .is_some_and(crate::os_window_focused)
            && self.focused_session_id(wid) == Some(session);
        if looking && self.session_status.note_seen(session) {
            // THE SESSION LOG: `mark - seen` lifts the failure's need; its
            // owed wake rings the session's watchers in the same hold.
            if let Some(s) = self.pool.get(session) {
                let mut timeline = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                let at = timeline.slog().now();
                timeline.slog_mut().failure_seen(at);
                let _ = timeline.slog_take_wake();
            }
            self.refresh_session_status_chrome(session);
        }
    }

    /// Adopt a new `tab_status*` generation. Called from the one config
    /// publication point, after `self.config` is live.
    pub(crate) fn reconfigure_session_status(&mut self) {
        let now = std::time::Instant::now();
        // Turning the master switch off retires what is on screen: a tab must not keep a phase
        // from a classifier that no longer runs.
        let moved = if self.config.tab_status_or_default() {
            self.session_status.reconfigure(
                self.config.tab_status_policy(),
                self.config.tab_status_observe_interval(),
                now,
            )
        } else {
            // The Claude Code footer rides this sweep: with it off, forget the facts, or the
            // next Claude in a tab is painted with the last one's (`crate::claude_footer`).
            // `clear` cancels the program resolver first, so an in-flight identification
            // cannot enqueue a watch after its stop.
            let cleared = self.session_status.clear();
            let mut footers = false;
            for session in self.pool.iter() {
                crate::claude_footer::stop_session(session.id);
                crate::codex_rollout_tap::stop_session(session.id, &session.ctx.timeline);
                footers |= session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear_claude_footer();
            }
            cleared || footers
        };
        // The badge switch is no policy, so `reconfigure` cannot see it move; track it here or
        // toggling it leaves every badge as it was.
        let badge = self.config.tab_status_badge_or_default();
        let badge_moved = self.session_status.set_badge(badge);
        if !moved && !badge_moved {
            return;
        }
        // Every window: the badge is how a background tab reports itself.
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

    /// Short status text for this session's chrome, or `None`. An identified agent's published
    /// phase (`idle`, `bash approval`) wins over the shell's, except after an exit.
    pub(crate) fn session_status_text(&self, session: u64) -> Option<String> {
        let status = self.session_status.status(session);
        if status.is_none_or(|s| s.phase != Phase::Exited)
            && let (_, Some(reading)) = self.session_status.agent_reading(session)
            && let Some(text) = agent_activity_text(&reading.phase)
        {
            return Some(text);
        }
        status.and_then(summary_text)
    }

    /// Moves with the shell status and with the published agent reading, so
    /// chrome recomposes when either line would read differently.
    pub(crate) fn session_status_revision(&self, session: u64) -> u64 {
        self.session_status
            .revision(session)
            .wrapping_add(self.session_status.agent_reading_seq(session))
    }

    /// This session's contribution to its tab's indicator bits; nothing before anything is
    /// published. `tab_status_badge = false` contributes nothing either but keeps classifying,
    /// so `status` and the tooltip still read the record.
    pub(crate) fn session_status_indicators(
        &self,
        session: u64,
    ) -> crate::tab_model::TabIndicators {
        if !self.config.tab_status_badge_or_default() {
            return crate::tab_model::TabIndicators::default();
        }
        self.session_status.indicators(session).unwrap_or_default()
    }

    /// Fan one session's published status change out to the chrome that shows it. Label and
    /// tooltip come from the focused view alone; the indicator bits fold every leaf, so a
    /// background pane of a split schedules its window too. Both refresh in one pass per window,
    /// since `refresh_window_tabs` takes every tab's terminal lock on the output path.
    pub(crate) fn refresh_session_status_chrome(&mut self, session: u64) {
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
        // A closed tab leaves the lost-integration row: restated without it, or withdrawn when
        // it was the last (never a `✓`: closing is no fix).
        self.forget_integration_notice(session);
        // Without this the job oracle's map would keep one entry per closed tab.
        self.job_probe.forget(session);
        // Its A/B stretch is settled, and Settings ▸ Harness loses its row.
        self.retire_harness_ab(session);
        self.harness_screen_moved();
    }

    /// The bridge's roster-round status read: one event-loop wake for the whole roster. Each
    /// row carries the registry's sid and launch nonce beside the revision, hold, detail and
    /// agent fields, so a reused local id is never taken for the session sampled earlier; a
    /// vanished session has no row (the bridge falls back to `@sid status`). It skips `status`'s
    /// subject, screen hash and consent projection, which N times in one turn would stall
    /// typing, and a contended timeline only defers that row's agent field.
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
            // The registry may change while the status is read: never stamp a new occupant of
            // the local id with the old key.
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
    ///
    /// And one the record does not carry, `hold_origin=` (`person`, `fleet`,
    /// `local`, comma-joined in that order, or `-`), from the same read of the
    /// same leaf: whose holds stand — several, when the fleet halted a session
    /// the owner had held or the person took over. The bridge converges the
    /// FLEET slot with the bus and must never lift the owner's LOCAL one or the
    /// PERSON'S, which `hold=` alone cannot tell apart from it. (A bridge from before the token,
    /// or from before two slots meeting `fleet,local`, reads the row as
    /// malformed and retries that session's `status`.)
    fn fabric_status_fields(&self, session: u64) -> Option<String> {
        let pooled = self.pool.get(session)?;
        let holds = pooled.ctx.fabric.holds();
        let (hold, hold_origin) = (u8::from(holds.is_held()), holds.origins());
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
            // Contended: hold/detail still go out, and `agent_deferred=1` makes the bridge keep
            // its last agent verdict (an older bridge retries the full status read).
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        Some(match agent {
            Some(word) => format!(
                "sid={session} revision={revision} hold={hold} hold_origin={hold_origin} \
                 detail={detail} agent={word}"
            ),
            None => format!(
                "sid={session} revision={revision} hold={hold} hold_origin={hold_origin} \
                 detail={detail} agent_deferred=1"
            ),
        })
    }

    /// Project one session's subject and status onto the `status` verb's reply body; the
    /// caller adds `OK ` and the newline.
    ///
    /// Runs on the event loop: the `ctx.meta` leaf lock first (released), then one `try_lock`
    /// on the terminal for the subject rungs and `detail=`. Contention answers
    /// `subject_source=unavailable` / `detail=-` rather than a lower rung, so a poller can tell
    /// "changed" from "could not look". A pin answers the subject without the terminal.
    ///
    /// No `window=` here: membership lives on the main thread, and `status` is what drivers
    /// poll; `sessions` and `dims` name the window.
    pub(crate) fn session_status_record(&self, session: u64) -> Result<String, String> {
        let Some(pooled) = self.pool.get(session) else {
            return Err(format!("no such session {session}"));
        };
        let enabled = self.config.tab_status_or_default();
        // `hold=` is this session's standing halt (why `send` answers `ERR halted`), and
        // `fabric=` the instance's bridge state; the tail `fabric_rtt_ms=` /
        // `fabric_link_age_ms=` is the bridge's last broker ack and its age. Leaf reads.
        let hold = u8::from(pooled.ctx.fabric.hold().is_some());
        // `hold_advisory=`: `fabric-lost` when a lost bridge left this session unheld (nothing
        // refused) and no newer bridge or hold stands; `-` otherwise.
        let hold_advisory = pooled
            .ctx
            .fabric
            .hold_advisory()
            .map_or_else(|| "-".to_string(), |h| h.reason);
        // `editing=<n>` (amacs §6.7), just before the `gen=`/`seq=`/`hash=` stamp:
        // how many open `edit` tickets hold this session, whose typing verbs then
        // answer `ERR busy editing` ([`crate::control_edit::hold`]). Its leaf lock
        // is taken here, before any other this record takes.
        let editing = crate::control_edit::hold::count(session);
        let fabric = crate::fabric::fabric_state();
        let fabric_tail = crate::fabric::fabric_status_tail();
        // `fleet=` (2026-10-15), after the fabric's tail: the INSTANCE's fleet
        // listener (`crate::fleet_listen`), one leaf-mutex read.
        let fleet = crate::fleet_listen::fleet_status_word();
        let pin = {
            let meta = pooled.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            meta.presentation_value("title")
        };
        // `human_seq=` is read before the terminal guard that stamps `gen=`/`seq=`/`hash=`, as
        // `control_query::cmd_text_json_looked` does: read after, a gesture landing between the
        // two would pair an older `gen=` with a count that already holds it, and a write fenced
        // on both would go through behind the person's key.
        let human_seq = pooled.ctx.human_input.seq();
        // The session's published text, looked at BEFORE the terminal guard (the
        // two never nest): `hash=` below is served from it when it is this
        // screen's, and built under the guard when it is not.
        let mut look = pooled.ctx.frame.look_frame();
        let term = match pooled.term.try_lock() {
            Ok(t) => Some(t),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        // `launch=<why|none> launch_seq=<n|->` (2026-10-16), after `human_seq=`:
        // where the person count stood when the shell last signed a command
        // start, judged against THIS record's count ([`crate::launch_witness`]).
        // Its leaf lock is taken after the terminal guard, never before it.
        let launch = pooled.ctx.launch.status_fields(human_seq);
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
            // Rungs 2 and 4. Rung 5 (shell name) is not resolvable here, so an empty
            // terminal answers `-`.
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
        // Rung 3: what is running, from the same guard; `-` when nothing runs or the lock was
        // contended. The roster's `detail=` comes from the same producer.
        let detail = term.as_deref().and_then(executing_detail);
        // THE SETTLED SCREEN'S STAMP, read under the SAME guard as `detail=` so
        // the two cannot describe different instants. `seq=` is the terminal's
        // `content_seq` and `hash=` is FNV-1a-64 of the UNTRIMMED visible screen
        // — byte for byte the pair `turn` returns and `history` keeps for a turn
        // id, so a report built from this screen can be matched against the
        // ledger rather than believed (a manager's fold of a report). A session
        // whose terminal could not be locked answers `-`/`-`: the poll never
        // waits on the guard for a field it can say it does not have.
        let stamp = term
            .as_deref()
            .map(|t| (t.content_seq(), look.text(t).fnv()));
        // THE RESIZE LEDGER's probe (`render=`), from the same guard: four
        // integer loads. `None` when the lock was contended, and `render=-`.
        let resize_probe = term
            .as_deref()
            .map(crate::resize_ledger::EngineProbe::sample);
        let (seq, hash) = stamp.map_or_else(
            || ("-".to_string(), "-".to_string()),
            |(seq, hash)| (seq.to_string(), format!("{hash:016x}")),
        );
        // `gen=<epoch>.<seq>`: the generation a fenced press names (`key if-gen=`); `seq=`
        // alone repeats after an alternate-screen re-entry ([`crate::control::ScreenGen`]).
        let generation = term.as_deref().map_or_else(
            || "-".to_string(),
            |t| crate::control::screen_gen(t).to_string(),
        );
        // `integration=<on|off|degraded>`: whether OSC 133/633 marks reach the engine.
        // `degraded` means a nonce is required and none is in use (an adopted shell, or one
        // waiting to re-key, `shell_rekey`), so `detail=` is dark and `program=` names it.
        let integration = term
            .as_deref()
            .map_or("-", |t| t.shell_integration_posture().as_str());
        // The integration body revision and posture `integration_rev=` is read from.
        let body = term.as_deref().map(|t| {
            (
                t.shell_integration_rev().map(str::to_owned),
                t.shell_integration_posture(),
            )
        });
        // `os_appearance= theme_side= reported_scheme= scheme_from= bg_dark=`
        // (APPEAR-10), between the agent verdict and `integration=`: the OS
        // reading and the split side it picked, then what THIS session reports
        // to DSR 996 / mode 2031, whether that is the theme's baseline or the
        // background its program set, and whether its background is dark, from
        // the same guard. A check reads them agree after an OS flip.
        let appearance_probe = term.as_deref().map(|t| {
            (
                t.color_scheme(),
                t.color_scheme_source(),
                t.default_background(),
            )
        });
        drop(term);
        let appearance = self.appearance_status_fields(
            appearance_probe.map(|(reported, _, _)| reported),
            Some(crate::app_colorscheme::scheme_from_word(
                appearance_probe.map(|(_, source, _)| source),
            )),
            appearance_probe.map(|(_, _, bg)| bg),
        );
        // `session_theme= theme_origin=` right after: the theme this session
        // asks for of its own and who chose it, `instance`
        // when it draws the instance's ([`crate::session_theme`]).
        let appearance = format!("{appearance} {}", self.session_theme_fields(session));
        // ` conpty=<openconsole|inbox|->` (Windows only, right before
        // `integration=`): the console host this session runs in, the tab's own
        // fact (a fallback put this tab on conhost, not the process). Absent off
        // Windows, so those records are byte-identical.
        let conpty = conpty_status_field(cfg!(windows), aterm_pty::conpty_host(pooled.master));
        // `supervisor=<holder|->`: the live supervisor claim (`meta set
        // supervisor`), so a poll shows that something is answering this
        // session's prompts. A leaf lock, taken after the terminal guard —
        // and in the same read, the session's own Autopilot word.
        //
        // `supervisor_proven=<0|1>` beside it (2026-10-16): whether that claim
        // is a PROVEN self-claim ([`crate::self_claim`]): the kernel proved the
        // claim is the foreground agent's own, never the holder's spelling.
        let (supervisor, word, land_at, claimed_here, proven) = {
            let meta = pooled.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let now = crate::metrics::now_us();
            let live = meta.live_supervisor(now);
            let proven = meta.supervisor_proven(now);
            (
                format!(
                    "{} supervisor_proven={}",
                    live.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode),
                    u8::from(proven),
                ),
                meta.autopilot_word(),
                meta.land_at,
                live == Some(crate::autopilot::own_holder()),
                proven,
            )
        };
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
        // `disk= disk_free_mb= disk_room_mb= disk_age_ms=` (2026-10-17), right
        // after the input fields: the volume holding this session's working
        // directory, as the disk watch last measured it — from the disk
        // board under its leaf lock, no I/O ([`crate::disk_board`]). Facts
        // about the machine: the presence `level=` does not move for them.
        //
        // `job=<verdict|none|-> job_quiet_ms=<ms|->` (2026-10-20, system
        // awareness §11.2), right after the disk fields: the session's worst
        // live job as the stuck engine last judged it, and how long it has
        // been quiet — from the snapshot the loop holds, no lock, no I/O
        // ([`crate::App::status_job_fields`]).
        //
        // `syspolicyd= syspolicyd_workers= syspolicyd_full_ms=` (2026-10-21), after the
        // job fields: whether macOS's program check still answers, as its watch last
        // judged it ([`crate::program_check_watch`]). `stuck`: no new program starts on the
        // Mac until it restarts. A machine fact, from the watch's board; no I/O.
        let input = format!(
            "{input} {} {} {}",
            crate::disk_board::status_fields(session, reported_cwd.as_deref()),
            self.status_job_fields(session),
            crate::program_check_watch::status_fields()
        );
        // THE PROGRAM AND THE AGENT VERDICT (`program= agent= agent_detail=
        // agent_rev= agent_since_ms=`): the sweep's publication, read from the
        // session timeline — the same store the `sessions` row, `await agent`
        // and `EVENT agent` read, so no two surfaces disagree.
        //
        // `render=<ok|displaced|desync-risk|unverified|unjudged|-> resizes=<n> flaps=<n>`
        // (2026-09-28), evaluated in the same leaf lock and printed just before
        // the `gen=`/`seq=`/`hash=` stamp: whether the app has drawn over
        // content a resize displaced ([`crate::resize_ledger`]), how many
        // resizes the session took, and how many net-zero flaps.
        //
        // `last_act=<act|->` (2026-10-16), from the same leaf lock: the last act
        // an agent that supervises itself recorded (`meta set harness_act`,
        // the timeline's `harness-act` rows).
        //
        // THE SESSION LOG's fields (`log_seq= log_ep= run= turn= step= need=
        // need_since_ms=`, phase 3A), from the same leaf lock and printed just
        // before the stamp: the reader's head and epoch (a `since=` every read
        // takes), the open path, and the most urgent observed need
        // ([`crate::session_log::SessionLog::status_fields`]), and after them
        // `rollout=<word|-> rollout_asks=<n>` (B5c): what the codex-alab
        // rollout tap last answered for the session and how many asks it
        // answered, each counted after its records were applied
        // ([`crate::session_log::transcript::TapAnswers`]).
        //
        // `task_state=<working|done|exited|stalled|-> task_rev=<n>` (2026-10-15),
        // from the same leaf lock and printed after `pause=`: the session's
        // task record ([`crate::task_record`]) — what a manager polls a worker
        // for, `-`/`0` for a session with no task. Named apart from `task=`
        // (below), the task's face under Land, which is another fact: the
        // record is the task a worker was spawned with and how it ended; the
        // face is how its loop's landing stands for this window's host.
        let (
            agent,
            resize,
            task_record,
            (supervised_agent, group),
            last_act,
            face,
            (pause, held),
            slog,
        ) = {
            let mut timeline = pooled
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let slog = timeline.slog();
            let slog = format!(
                "{} {}",
                slog.status_fields(slog.now().wall_ms),
                slog.rollout_fields()
            );
            (
                timeline.agent().wire_fields(),
                timeline.resize_status_fields(resize_probe, crate::turn_ledger::now_ms()),
                crate::task_record::status_tokens(timeline.task()),
                (
                    crate::harness_host::agent_of(timeline.agent()),
                    timeline.agent().program_pgid,
                ),
                timeline.last_act().unwrap_or("-").to_string(),
                timeline.task_face().map(|(face, _)| face),
                (timeline.pause(), timeline.land_held()),
                slog,
            )
        };
        // The agent the host admits (never a vendor `codex`), read after the
        // timeline's leaf lock.
        let supervised_agent =
            crate::harness_host::admitted_as(&pooled.ctx, supervised_agent, group);
        // `autopilot=<off|cruise|land|->` (2026-10-15), right after
        // `supervisor=` and its `supervisor_proven=`: the session's EFFECTIVE Autopilot while it runs an
        // agent the harness supervises — its own word, else `[harness]
        // autopilot`, and `off` under the master switch or with no host — and
        // `-` for any other session ([`crate::autopilot::wire_field`]).
        let autopilot =
            crate::autopilot::wire_field(self.harness_posture(), word, supervised_agent);
        // `program_state=<state>[:<kind>]|-` (2026-10-16), right after the
        // agent verdict's fields and apart from them: what a `codex` in front
        // reports about itself (OSC 7501, [`crate::program_state`]), for
        // display. Never `agent=`, never an `await`'s, never a decision's.
        let program_state = crate::program_state::wire(self.session_status.program_state(session));
        // `land_at=<unix s|-> task=<open|landed|theirs|->`
        // (2026-10-16, `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.5,
        // §9.4.1), from the same two reads and printed just before the
        // `gen=`/`seq=`/`hash=` stamp, which stays last: for an agent session,
        // the instant its timed Cruise lands and its task's face as its loop
        // last told this window's host — `task=` only while that host holds
        // the live claim, or beside a proven self-claim whose own acts tell
        // the face (FOREVER LAND §10.4) — and `-` otherwise.
        let land_at =
            crate::autopilot::land_at_field(land_at.filter(|_| supervised_agent.is_some()));
        let task = crate::autopilot::task_field(face, claimed_here || proven, supervised_agent);
        // `pause=<unix s|->` (2026-10-17, design §10.5.1), right after
        // `task=` and under its rule: the unix second the loop's pause after
        // short turns ends, as it last told this window's host.
        let pause = crate::autopilot::pause_field(pause, claimed_here, supervised_agent);
        // `land_held=<legs|-> land_nudges=<n|-> land_since=<unix s|->`
        // (Autopilot v5 §12.3.1, slice 3c), right after `pause=` and under
        // its rule: why Land still holds, as its loop last told this
        // window's host, only while the session runs under Land with its
        // task open ([`land_fields`]).
        let under_land = supervised_agent.is_some_and(|agent| {
            crate::autopilot::AutopilotView::of(self.harness_posture().for_agent(Some(agent)), word)
                .mode()
                == Some(aterm_agent::supervise::Autopilot::Land)
        }) && !matches!(
            face,
            Some(
                aterm_agent::harness::acts::TaskFace::Landed
                    | aterm_agent::harness::acts::TaskFace::Theirs
            )
        );
        let land = land_fields(held.filter(|_| claimed_here && under_land));
        // `end=<unix s|-> end_seen=<0|1|-> end_cheered=<0|1|->` (development
        // builds only, `docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.2),
        // right after `pause=` and under `task=`'s rule: the task's END on
        // the glass, when it was told, whether someone has looked at it and
        // whether its cheer is spent ([`end_fields`]). One map lookup, and
        // the timeline's leaf lock only where an end stands. A release build
        // prints none of it.
        #[cfg(any(test, debug_assertions))]
        let end = self.end_status_fields(
            session,
            &pooled.ctx,
            (claimed_here || proven) && supervised_agent.is_some(),
        );
        #[cfg(not(any(test, debug_assertions)))]
        let end = "";
        // `human_ms=<ms|->`, just after `supervisor=`: how long ago a PERSON
        // last had a hand on this session through a window
        // ([`crate::human_input`]; control verbs never stamp it, save a
        // development build's `key|send as=person`), `-` for
        // never — what a supervisor keeps its hands off for `[harness]
        // human_grace_s`, and waits on before it keys a dialog a person may
        // be navigating. One atomic load.
        //
        // `human_seq=<n>` beside it: how many person gestures have reached the
        // session — the value a write fenced `if-human=<n>` holds a person's
        // keystroke since this read to ([`crate::human_input`]). The count was
        // read FIRST, before the screen (above), the age here after it
        // (`HumanInputStamp::seq`): read the other way round, a gesture landing
        // between the two loads gave an old, quiet age beside a count that
        // already held it — and a key fenced on that count went through.
        let human_ms = pooled.ctx.human_input.wire(crate::metrics::now_us());
        // `attribution=` and `fs_consent=`, from the cwd the terminal guard produced.
        let consent = self.session_consent(session, reported_cwd.as_deref());
        let opt = |value: Option<&str>| {
            value.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode)
        };
        // THE ROUND-19 TAIL (design §5): `hand= hand_pid= level= story=`, additive after
        // the fabric fields. Read from the presence slot the wakes keep current
        // — plain field reads, no lock, no classifier — so the poll pays nothing
        // it did not already pay.
        let presence = self.presence_status_tail(session);
        // `needs=<word|-> hold_by=<person|local|bridge|->` (2026-10-16; the
        // person's is Take Over, design §4), after the
        // presence tail: the published need verdict (`crate::needs`), what the
        // menu bar, the survey and every reader read — one leaf-lock read, no
        // classifier. `hold=` keeps its `0|1` (the harness's halt loop reads it).
        // `needs_n=<n|->` rides `needs=` (Autopilot v5 §12.2.1): how many
        // needs a harness's ask names (its first and its ` (+N more)`), `-`
        // where none is the published need.
        //
        // `needs_told=<n|stale|->` (development builds only, Autopilot v5
        // §12.2.3 item 3, slice 2c) rides it too: the worker's NEEDS YOU set
        // as its loop told this window's host, whole ([`needs_told_field`]).
        // The needs owner's entry is copied under the meta's leaf lock first,
        // released before the timeline's; a release build reads neither.
        #[cfg(any(test, debug_assertions))]
        let needs_entry = pooled
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .attention_owners
            .get(crate::harness_host::NEEDS_ATTENTION_OWNER)
            .map(str::to_string);
        let (needs, hold_by) = {
            let timeline = pooled
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let need = timeline.need();
            #[cfg(any(test, debug_assertions))]
            let needs = format!(
                "{}{}",
                needs_fields(need),
                needs_told_field(&timeline.harness_needs(), needs_entry.as_deref())
            );
            #[cfg(not(any(test, debug_assertions)))]
            let needs = needs_fields(need);
            (needs, need.hold_word())
        };
        // `typed_by=<pct|-> typed_ms=<ms|->` (S9), right after `hold_by=`: who
        // last typed into it over the socket, named here, and how long ago.
        let typed = crate::typed_by::wire(
            self.typed_by(&pooled.ctx).as_ref(),
            aterm_time::unix_millis().unwrap_or(0),
        );
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
        // `born=<cold|spare-parked|spare-warming|spare-ready>` (2026-10-16): how
        // this tab's shell started — a cold spawn, or a warm spare it adopted
        // (`crate::spare`), with the spare's age then.
        let born = pooled.ctx.witness.born_fields();
        // `journal=<live|off>` (crash restore, 2026-10-17), after `history_lost=`:
        // whether this session writes a crash-restore region, which its registration
        // hands the keeper beside the master (a keeper that had not answered yet is
        // told when it does; `aterm keeper status journals=` counts what one holds),
        // with a `journal_off=` reason when it does not, and while it does, its tail and
        // history ring's lag, whether memory pressure shed its ring
        // (`journal_ring=<live|shedding|shed>`), the RAM its region costs (`journal_bytes=`)
        // and the turn records its control slot does not hold (`journal_ledger_lag=`: not
        // carried yet, or shed to fit);
        // then, on a session the keeper recovered, `restore=<exact|screen|meta>` and what
        // the restore could not reproduce (`crash_restore::RestoreReport::status_words`).
        // Counts only: no verb reads a region.
        let journal = pooled.ctx.crash_journal.status_words();
        let integration_rev = crate::shell_body::status_word(
            body.as_ref()
                .map(|(rev, posture)| (rev.as_deref(), *posture)),
            integration_frozen,
        );
        // A measured PATH reading wins over the carried mark (`path_evidence=` says which);
        // `copy=` and `upgrade=` ride after `supervisor=` ([`SessionTimeline::owner_columns`]).
        let (path, owner) = pooled
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .owner_columns(mark, crate::upgrade_host::now_s());
        // `bells=<n> bells_sounded=<n> last_bell=<ms>/<sound>/<program>[,…]|-`
        // (2026-10-22), after `editing=` and before the stamp: the bells this
        // session's programs rang, how many played the alert sound, and the
        // last few — how long ago, what became of the sound, the program in
        // front ([`crate::bell_ledger`]). A fact for later; one leaf lock.
        let bells = pooled
            .ctx
            .bells
            .wire(aterm_time::unix_millis().unwrap_or(0));
        let Some(status) = self.session_status.status(session) else {
            // Never classified; distinct from `phase=unknown`, a classification with no usable
            // evidence. `detail=` is the engine's fact, so it is answered here too.
            return Ok(format!(
                "schema=1 sid={session} subject={} subject_source={subject_source} observed=false \
                 phase=unknown since_ms=- outcome=none exit_code=- signal=- detail={} \
                 confidence=unknown reasons=- attribution={} fs_consent={} conflict=false \
                 revision=0 enabled={enabled} hold={hold} hold_advisory={hold_advisory} \
                 fabric={fabric} {fabric_tail} fleet={fleet} \
                 {presence} needs={needs} hold_by={hold_by} {typed} path={path} {agent} program_state={program_state} last_act={last_act} {appearance}{conpty} integration={integration} \
                 {input} supervisor={supervisor} autopilot={autopilot} human_ms={human_ms} \
                 human_seq={human_seq} {launch} {owner} \
                 history_lost={history_lost} {journal} integration_rev={integration_rev} {born} {resize} \
                 land_at={land_at} task={task} pause={pause}{land}{end} {task_record} {slog} editing={editing} {bells} gen={generation} seq={seq} hash={hash}",
                opt(subject.as_deref()),
                opt(detail.as_deref()),
                consent.attribution.as_str(),
                consent.fs_consent.as_str(),
            ));
        };
        // `Status::since` is an `Instant`, so the reply carries an age.
        let since_ms = std::time::Instant::now()
            .saturating_duration_since(status.since)
            .as_millis();
        let (exit_code, signal) = status.last_outcome.wire_fields();
        // `consent_at_risk` rides the reasons list: the FSM has no consent input.
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
             conflict={} revision={} enabled={enabled} hold={hold} hold_advisory={hold_advisory} \
             fabric={fabric} {fabric_tail} fleet={fleet} \
             {presence} needs={needs} hold_by={hold_by} {typed} path={path} {agent} program_state={program_state} last_act={last_act} {appearance}{conpty} integration={integration} \
             {input} supervisor={supervisor} autopilot={autopilot} human_ms={human_ms} \
             human_seq={human_seq} {launch} {owner} \
             history_lost={history_lost} {journal} integration_rev={integration_rev} {born} {resize} \
             land_at={land_at} task={task} pause={pause}{land}{end} {task_record} {slog} editing={editing} {bells} gen={generation} seq={seq} hash={hash}",
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

    /// `status`'s end fields for `session` ([`end_fields`]): the end its
    /// glass holds, where this window tells its task's face (`told`), at the
    /// instant the timeline holds for that face (`crate::app_tabs::end_wall_ms`,
    /// the Done for you row's own instant).
    #[cfg(any(test, debug_assertions))]
    fn end_status_fields(&self, session: u64, ctx: &crate::SessionCtx, told: bool) -> String {
        let end = self
            .harness_glass
            .get(&session)
            .and_then(crate::harness_glass::SessionGlass::end)
            .filter(|_| told)
            .map(|end| {
                let at = ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .task_face_at(end.face.task_face());
                (end, crate::app_tabs::end_wall_ms(end, at))
            });
        end_fields(end)
    }
}

/// WHY LAND STILL HOLDS AS `status` PRINTS IT
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.3.1, slice 3c): `
/// land_held=<legs> land_nudges=<n> land_since=<unix s>` for `held`, the
/// hold the session's loop last told this window's host — its legs in the
/// owner's order (`needs,repo,open,defense,cut`, `-` before a land act's
/// yield is read: Land's instruction typed, nothing judged since), the
/// nudges typed since the act that began it, and that act's second (`-`
/// where its ledger could not say) — and all three `-` where none stands:
/// outside a landing, where this host holds no claim, beside a proven
/// self-claim (the caller's rule). Additive, read by key.
pub(crate) fn land_fields(held: Option<aterm_agent::harness::acts::LandHeld>) -> String {
    let Some(held) = held else {
        return " land_held=- land_nudges=- land_since=-".to_string();
    };
    let legs: Vec<&str> = if held.read {
        held.legs_held().map(|leg| leg.word()).collect()
    } else {
        Vec::new()
    };
    format!(
        " land_held={} land_nudges={} land_since={}",
        if legs.is_empty() {
            "-".to_string()
        } else {
            legs.join(",")
        },
        held.nudges,
        held.since_ms
            .map_or_else(|| "-".to_string(), |ms| ms.div_euclid(1000).to_string())
    )
}

/// THE END ON THE GLASS AS `status` PRINTS IT (development builds,
/// `docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.2): ` end=<unix s>
/// end_seen=<0|1> end_cheered=<0|1>` for `end`, an end that stands, told at
/// unix ms `at_ms` (`end=-` where its instant is not known, `0`), and `end=-
/// end_seen=- end_cheered=-` where none does. Seen and cheered are two facts
/// ([`crate::harness_glass::EndGlass::seen`] and `cheered`): the strip's
/// looking rule moves the one, the first frame that shows the pane the
/// other, so a headless end can be cheered and never seen. `end_cheered=1`
/// is that the end's cheer is spent: a pet cheered it, or a seamless update
/// carried it in cheered (`SessionGlass::carry_end`), so no pet replays it.
/// Additive, read by key.
#[cfg(any(test, debug_assertions))]
pub(crate) fn end_fields(end: Option<(crate::harness_glass::EndGlass, u64)>) -> String {
    match end {
        None => " end=- end_seen=- end_cheered=-".to_string(),
        Some((end, at_ms)) => {
            let at = if at_ms == 0 {
                "-".to_string()
            } else {
                (at_ms / 1000).to_string()
            };
            format!(
                " end={at} end_seen={} end_cheered={}",
                u8::from(!end.unseen()),
                u8::from(!end.uncheered())
            )
        }
    }
}

#[cfg(all(test, windows))]
mod idle_cost_tests {
    use std::time::{Duration, Instant};

    /// A pid that certainly owns no child process, so the classifier's verdict
    /// is stable however the test binary's own process tree happens to look.
    const CHILDLESS_PID: i32 = 0x7FFF_FFF0;

    /// Stage `count` pooled sessions that look like ConPTY sessions to the observer: a
    /// non-negative master (Windows keys start at `0x4000_0000`) and a live-looking pid, which
    /// arm the `foreground_job` evidence the headless stubs (`-1`/`-1`) skip.
    fn stage_pty_like_sessions(app: &mut crate::App, count: u64) {
        for i in 0..count {
            let mut session = crate::stub_session(1000 + i);
            session.master = 0x4000_0000 + i32::try_from(i).expect("small fixture");
            session.pid = CHILDLESS_PID;
            app.pool.insert(session);
        }
    }

    /// Run `secs` of simulated idle event loop (four sweeps a second, as a blinking cursor
    /// wakes it) and return how many process-table captures the job probe took.
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

    /// On Windows the foreground-job probe is a system-wide process-table walk (~3.1 ms), not a
    /// `tcgetpgrp`: an idle window's captures must not scale with its tab count, and stay
    /// within one per `JOB_PROBE_MAX_AGE`.
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

    /// A session whose grid moves gets a fresh capture, so a job start is never answered from
    /// a stale process table.
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

        // Move one staged session's grid, as a starting job does, and sweep inside the ceiling.
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
#[path = "session_status_tests.rs"]
mod tests;

/// THE EDGED INBOX (card engine §3.5), driven through the real sweep: an
/// edged session is named on the next wake, due or not, and a contended
/// visit is retried with no interval charged.
#[cfg(test)]
#[path = "session_status_edge_inbox_tests.rs"]
mod edge_inbox_tests;

/// The published agent verdict and program identity, driven through the real sweep
/// (`App::observe_session_statuses`) on a headless App.
#[cfg(test)]
#[path = "session_status_agent_verdict_tests.rs"]
mod agent_verdict_tests;

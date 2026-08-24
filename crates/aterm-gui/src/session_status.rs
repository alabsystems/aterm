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
            pending: None,
        }
    }

    pub(crate) fn status(&self) -> &Status {
        &self.published
    }

    /// The instant at which this session could publish a transition that NO new
    /// output would cause. Three exist: a candidate still serving its dwell,
    /// `Running` aging into `Quiet`, and the LIVE typing `Idle` settling into
    /// plain `Idle` — all keyed off the same movement clock. Everything else is
    /// edge-driven — a settled `Idle` pane cannot become anything until bytes
    /// arrive — so it returns `None` and costs the event loop nothing.
    fn owed_wake(&self) -> Option<Instant> {
        if let Some((_, first_seen)) = &self.pending {
            return Some(*first_seen + self.policy.dwell);
        }
        let aging = self.published.phase == Phase::Running || self.published.typing_live();
        aging
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
                ShellEvidence::Executing => Candidate {
                    phase: if moved { Phase::Running } else { Phase::Quiet },
                    confidence: Confidence::Strong,
                    reasons: if moved {
                        vec![Reason::ShellBlock, Reason::ContentActivity]
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
                ShellEvidence::Entering if self.typed_recently(&evidence.activity, now) => Candidate {
                    phase: Phase::Idle,
                    confidence: Confidence::Strong,
                    reasons: vec![Reason::ShellBlock, Reason::ContentActivity],
                    conflict: false,
                    outcome: None,
                },
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
        }
    }

    /// Could ANY known session be past its deadline right now? The O(1) half of
    /// the sweep's gate (the other half is the pool epoch — see
    /// `swept_pool_epoch`). `None` (nothing known) answers YES, so an observer
    /// that has never seen a session never gates anything out.
    pub(crate) fn any_due(&self, now: Instant) -> bool {
        self.next_due_any.is_none_or(|due| now >= due)
    }

    /// A sweep just walked the whole pool: make `next_due_any` EXACT again, and
    /// bank `pool_epoch` IF the sweep left every pooled session with a slot.
    /// O(slots), on the classification path only — which is 4/s at the default
    /// interval, not the burst rate the gate spares.
    ///
    /// `None` MEANS "DO NOT BANK", and that is the load-bearing case. A sweep
    /// can leave a session unclassified: the classify path takes the session's
    /// terminal with `try_lock` and SKIPS the session when the PTY reader holds
    /// it — which, under exactly the output flood this gate exists to survive,
    /// is not rare. A session skipped that way still has no slot, so `due()`
    /// would report it due and the whole-pool scan WOULD classify it, while
    /// `next_due_any` (a min over slots that do not include it) would not. If
    /// this banked the epoch anyway, the gate would close over a session it has
    /// never seen and that session's first status could wait out another
    /// session's whole interval. Leaving the epoch stale keeps the gate open
    /// until the newcomer really is classified, which costs one scan per wake
    /// in a rare case and makes the gate EXACTLY equivalent to the scan it
    /// replaces — the only property that makes it safe to ship.
    pub(crate) fn note_swept(&mut self, pool_epoch: Option<u64>) {
        if let Some(epoch) = pool_epoch {
            self.swept_pool_epoch = epoch;
        }
        self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
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
        self.sessions
            .values()
            .filter_map(|slot| slot.fsm.owed_wake())
            .min()
    }

    pub(crate) fn status(&self, session: u64) -> Option<&Status> {
        self.sessions.get(&session).map(|slot| slot.fsm.status())
    }

    pub(crate) fn revision(&self, session: u64) -> u64 {
        self.sessions.get(&session).map_or(0, |slot| slot.revision)
    }

    /// Drop a retired session's state. Without this the map would grow for the
    /// process lifetime as tabs open and close.
    pub(crate) fn retire(&mut self, session: u64) {
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
        // No slots ⇒ no known deadline ⇒ the gate is open again, which is what
        // a re-enabled subsystem needs (every session is unclassified).
        self.next_due_any = None;
        had
    }
}

impl crate::App {
    /// Classify every live session that is due, gathering evidence under
    /// `try_lock` only. Returns the sessions whose published status changed.
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
        let pool_epoch = self.pool.insert_epoch();
        let gated = pool_epoch == self.session_status.swept_pool_epoch()
            && !self.session_status.any_due(now);
        if gated {
            return Vec::new();
        }
        let due: Vec<(u64, i32, i32)> = self
            .pool
            .iter()
            .filter(|session| self.session_status.due(session.id, now))
            .map(|session| (session.id, session.master, session.pid))
            .collect();
        let mut changed = Vec::new();
        for (id, master, pid) in due {
            let Some(session) = self.pool.get(id) else {
                continue;
            };
            let term = session.term.clone();
            // Real PTY-output age from the session's existing activity atomic —
            // overwritten for EVERY consumed burst and never cleared by
            // presentation, so a hidden pane's output still ages honestly.
            let last_output = match session
                .latest_output_activity_ns
                .load(std::sync::atomic::Ordering::Relaxed)
            {
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
            let foreground_job = (master >= 0 && pid > 0).then(|| {
                crate::quit_safety::foreground_is_job(
                    crate::quit_safety::foreground_pgrp(master),
                    pid,
                )
            });
            let Ok(guard) = term.try_lock() else {
                continue;
            };
            let evidence = Evidence {
                pin: None,
                shell: shell_evidence(&guard),
                // The exit fact arrives as an EVENT (`note_session_exit`) and is
                // held sticky on the slot, so the sweep never has to discover it.
                lifecycle: None,
                foreground_job,
                activity: ActivitySample {
                    alt_screen: guard.is_alternate_screen(),
                    content_seq: guard.content_seq(),
                    last_output,
                    last_input,
                },
            };
            drop(guard);
            if self.session_status.observe(id, &evidence, now) {
                changed.push(id);
            }
        }
        // This pass walked the whole pool, so the gate's bound can be made
        // exact. O(slots), once per sweep — and a sweep only happens when
        // something WAS due, i.e. at the classification rate (~4/s at the
        // default interval), never at the burst rate.
        //
        // The EPOCH is banked only if every pooled session now has a slot. A
        // `try_lock` that lost to the PTY reader leaves a session unclassified
        // and slot-less, and the scan this gate replaces would have retried it
        // on the very next wake; banking the epoch over it would instead defer
        // its first status by another session's interval. See
        // `StatusObserver::note_swept`.
        let fully_classified = self
            .pool
            .iter()
            .all(|session| self.session_status.knows(session.id));
        self.session_status
            .note_swept(fully_classified.then_some(pool_epoch));
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
            self.session_status.clear()
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
    }

    /// Project one session's SUBJECT + STATUS onto the `status` verb's reply
    /// body (RFC §8). The caller adds the `OK ` prefix and the newline.
    ///
    /// Runs on the event loop, so the Subject ladder uses the same discipline as
    /// tab titles: the `ctx.meta` LEAF lock first — a pin returns without the
    /// terminal lock ever being attempted — then `try_lock`, with contention
    /// reported as `subject_source=unavailable` rather than silently answered
    /// from a lower rung. A driver polling under load must be able to tell "the
    /// title changed" from "I could not look".
    pub(crate) fn session_status_record(&self, session: u64) -> Result<String, String> {
        let Some(pooled) = self.pool.get(session) else {
            return Err(format!("no such session {session}"));
        };
        let enabled = self.config.tab_status_or_default();
        let pin = {
            let meta = pooled.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            meta.presentation_value("title")
        };
        let (subject, subject_source) = match pin {
            Some(title) if !title.is_empty() => (Some(title), "pin"),
            // Rungs 2 and 4 (RFC §4). Rung 3 (command-derived) has no producer
            // and rung 5 (shell name) is not resolvable here, so an empty
            // terminal answers `-`/`unavailable` instead of inventing one.
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
                match pooled.term.try_lock() {
                    Ok(t) => rungs(&t),
                    Err(std::sync::TryLockError::Poisoned(p)) => rungs(&p.into_inner()),
                    Err(std::sync::TryLockError::WouldBlock) => (None, "unavailable"),
                }
            }
        };
        let opt = |value: Option<&str>| {
            value.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode)
        };
        let Some(status) = self.session_status.status(session) else {
            // Never classified. Distinct from `phase=unknown`, which IS a
            // classification ("evidence was looked for and none was usable").
            return Ok(format!(
                "schema=1 sid={session} subject={} subject_source={subject_source} observed=false \
                 phase=unknown since_ms=- outcome=none exit_code=- signal=- detail=- \
                 confidence=unknown reasons=- conflict=false revision=0 enabled={enabled}",
                opt(subject.as_deref())
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
        let reasons = if status.reasons.is_empty() {
            "-".to_string()
        } else {
            status
                .reasons
                .iter()
                .map(|reason| reason.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        Ok(format!(
            "schema=1 sid={session} subject={} subject_source={subject_source} observed=true \
             phase={} since_ms={since_ms} outcome={} exit_code={exit_code} signal={signal} \
             detail={} confidence={} reasons={reasons} conflict={} revision={} enabled={enabled}",
            opt(subject.as_deref()),
            status.phase.as_str(),
            status.last_outcome.as_str(),
            opt(status.detail.as_deref()),
            status.confidence.as_str(),
            status.conflict,
            self.session_status.revision(session),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Duration = Duration::from_millis(5_000);
    const DWELL: Duration = Duration::from_millis(750);

    fn policy() -> StatusPolicy {
        StatusPolicy {
            quiet_after: QUIET,
            dwell: DWELL,
        }
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

    /// The `status` verb's reply, field by field. `schema=` leads so a consumer
    /// can reject an unknown MAJOR from the first token.
    #[test]
    fn the_status_record_is_versioned_and_separates_unobserved_from_unknown() {
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
        assert!(record.ends_with(" enabled=false"), "{record}");

        assert!(
            app.session_status_record(9999).is_err(),
            "an unknown session is an error, not a blank record"
        );
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
        assert!(fsm.observe(&ev, t1), "the live marker publishes immediately");
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
        let owed = observer.next_wake().expect("a live typing pane owes a wake");
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
        assert!(!app.session_status.status(0).expect("published").settled_idle());

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
        assert!(app.session_status.status(0).expect("published").settled_idle());
        app.refresh_session_status_chrome(0);
        assert_eq!(
            app.title_summaries.activity(0, &app.config),
            Some("Ready"),
            "an abandoned prompt decays to the prompt-state description"
        );
    }
}

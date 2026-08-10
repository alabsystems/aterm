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
//! * **`waiting_input` is never inferred from silence.** A quiet foreground job
//!   may be CPU-bound, sleeping, blocked on the network, in a pager, or at a
//!   REPL. Those are [`Phase::Quiet`] with [`Confidence::Heuristic`], which is
//!   honest; claiming a human is being awaited is not.
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
    /// Known to be awaiting input. Only ever set from the narrow sources in
    /// [`Evidence::waiting_input_signal`].
    WaitingInput,
    /// The PTY has ended. Pairs with [`Status::last_outcome`].
    Exited,
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
/// Not yet produced by the observer: `Wake::Exit` carries session and window
/// identity but no exit status, and a session is normally removed on exit.
/// Carrying the status is the prerequisite named in RFC §5, evidence row 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
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
}

/// Everything the classifier is allowed to look at. Assembling this is the
/// caller's job (and the only part that touches a lock).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Evidence {
    /// A phase pinned explicitly by the user. Outranks everything.
    pub(crate) pin: Option<Phase>,
    /// Set only by the narrow sources permitted by RFC §6 — never inferred.
    pub(crate) waiting_input_signal: bool,
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
    /// output would cause. Two exist: a candidate still serving its dwell, and
    /// `Running` aging into `Quiet`. Everything else is edge-driven — an `Idle`
    /// pane cannot become anything until bytes arrive — so it returns `None`
    /// and costs the event loop nothing.
    fn owed_wake(&self) -> Option<Instant> {
        if let Some((_, first_seen)) = &self.pending {
            return Some(*first_seen + self.policy.dwell);
        }
        (self.published.phase == Phase::Running)
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

        // 3. Waiting-for-input is only ever SIGNALLED, never inferred.
        if evidence.waiting_input_signal {
            return Candidate {
                phase: Phase::WaitingInput,
                confidence: Confidence::Strong,
                reasons: vec![Reason::ShellBlock],
                conflict: false,
                outcome: None,
            };
        }

        let moved = self.moved_recently(now) || self.output_recently(&evidence.activity, now);

        // 4. Shell integration: the strongest routine evidence. It OUTRANKS raw
        //    screen movement, so a background job printing at a prompt stays
        //    Idle rather than masquerading as the foreground task.
        if let Some(shell) = evidence.shell {
            // A genuine contradiction: the shell claims to be executing while
            // the foreground process group is the shell itself.
            let conflict = matches!(shell, ShellEvidence::Executing)
                && evidence.foreground_job == Some(false);
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

        // 5. No shell integration. A foreground-job Boolean still separates
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
            // 6. Nothing but the screen. Movement is weak evidence of work;
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
        let outcome = candidate
            .outcome
            .unwrap_or(self.published.last_outcome);
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
        (Phase::WaitingInput, _) => "waiting for input".to_string(),
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
/// into one — a highest-attention phase would let a `WaitingInput` pane hide a
/// `Running` sibling, and an exited pane hide both, which is exactly what
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
        // The outcome outlives the phase that produced it (see [`Outcome`]), so a
        // command that failed keeps its tab marked while the shell honestly sits
        // back at `Idle` — the tab is how a user learns about a pane they are
        // not looking at.
        attention: status.last_outcome.is_failure() || status.phase == Phase::WaitingInput,
    }
}

/// Owns one [`StatusFsm`] per live session and enforces the observation budget:
/// at most one classification per session per `min_interval`, so an output flood
/// cannot turn into a classification flood.
#[derive(Debug)]
pub(crate) struct StatusObserver {
    policy: StatusPolicy,
    min_interval: Duration,
    sessions: std::collections::HashMap<u64, SessionSlot>,
}

#[derive(Debug)]
struct SessionSlot {
    fsm: StatusFsm,
    next_due: Instant,
    /// Bumped only when the PUBLISHED status changes, so chrome can skip
    /// recomposition on an unchanged frame.
    revision: u64,
}

impl StatusObserver {
    pub(crate) fn new(policy: StatusPolicy, min_interval: Duration) -> Self {
        Self {
            policy,
            min_interval,
            sessions: std::collections::HashMap::new(),
        }
    }

    /// Whether this session is allowed a classification now. Callers check this
    /// BEFORE taking the terminal lock, so a rate-limited session costs nothing.
    pub(crate) fn due(&self, session: u64, now: Instant) -> bool {
        self.sessions
            .get(&session)
            .is_none_or(|slot| now >= slot.next_due)
    }

    /// Fold one observation in. Returns `true` when the published status changed.
    pub(crate) fn observe(&mut self, session: u64, evidence: &Evidence, now: Instant) -> bool {
        let policy = self.policy;
        let slot = self.sessions.entry(session).or_insert_with(|| SessionSlot {
            fsm: StatusFsm::new(policy, now),
            next_due: now,
            revision: 1,
        });
        slot.next_due = now + self.min_interval;
        let changed = slot.fsm.observe(evidence, now);
        if changed {
            slot.revision = slot.revision.saturating_add(1);
        }
        changed
    }

    /// Earliest instant at which SOME session owes a time-driven transition, for
    /// the event loop's wait deadline. Without this the classifier is purely
    /// edge-driven: a build that finishes and then prints nothing leaves its tab
    /// showing busy forever, because the observation that would retire it never
    /// runs.
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
        self.sessions.remove(&session);
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
    pub(crate) fn observe_session_statuses(&mut self, now: std::time::Instant) -> Vec<u64> {
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
            // Foreground-job PRESENCE only: `tcgetpgrp` gives a Boolean, not a
            // process name (the name is not available on the session path at
            // all), so the classifier is deliberately built to need only this.
            let foreground_job = (master >= 0 && pid > 0).then(|| {
                crate::quit_safety::foreground_is_job(crate::quit_safety::foreground_pgrp(master), pid)
            });
            let Ok(guard) = term.try_lock() else {
                continue;
            };
            let evidence = Evidence {
                pin: None,
                waiting_input_signal: false,
                shell: shell_evidence(&guard),
                lifecycle: None,
                foreground_job,
                activity: ActivitySample {
                    alt_screen: guard.is_alternate_screen(),
                    content_seq: guard.content_seq(),
                    last_output,
                },
            };
            drop(guard);
            if self.session_status.observe(id, &evidence, now) {
                changed.push(id);
            }
        }
        changed
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
    pub(crate) fn session_status_indicators(
        &self,
        session: u64,
    ) -> crate::tab_model::TabIndicators {
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
            last_output: None,
        }
    }

    fn evidence(activity: ActivitySample) -> Evidence {
        Evidence {
            pin: None,
            waiting_input_signal: false,
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

    #[test]
    fn silence_never_becomes_waiting_input() {
        // A quiet foreground job may be CPU-bound, sleeping, or in a pager.
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(7));
        ev.foreground_job = Some(true);

        settle(&mut fsm, &ev, t0 + QUIET * 2);

        assert_eq!(fsm.status().phase, Phase::Quiet);
        assert_eq!(fsm.status().confidence, Confidence::Heuristic);
        assert_ne!(fsm.status().phase, Phase::WaitingInput);
    }

    #[test]
    fn waiting_input_requires_an_explicit_signal() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.waiting_input_signal = true;

        settle(&mut fsm, &ev, t0);

        assert_eq!(fsm.status().phase, Phase::WaitingInput);
        assert_eq!(fsm.status().confidence, Confidence::Strong);
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
            last_output: None,
        });
        ev.foreground_job = Some(true);
        fsm.observe(&ev, t0);

        ev.activity = ActivitySample {
            alt_screen: true,
            content_seq: 3,
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
        ev.shell = Some(ShellEvidence::Complete {
            exit_code: Some(1),
        });

        settle(&mut fsm, &ev, t0);

        assert_eq!(fsm.status().phase, Phase::Idle, "finished work is not a phase");
        assert_eq!(
            fsm.status().last_outcome,
            Outcome::Failure { exit_code: 1 }
        );
        assert!(fsm.status().last_outcome.is_failure());
    }

    #[test]
    fn a_new_unit_of_work_clears_the_previous_outcome() {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete {
            exit_code: Some(2),
        });
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

        assert!(fsm.status().conflict, "contradictory evidence must be visible");
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
            (Phase::WaitingInput, Outcome::None, (false, true)),
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
                    attention,
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
                attention: true,
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
            }
        );
        assert!(
            app.tab_strip_metadata(wid).first().expect("one tab").busy,
            "the bits both strip renderers read must carry it"
        );
    }
}

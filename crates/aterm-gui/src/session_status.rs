// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session activity/status classifier ([`StatusFsm`]), budgeted [`StatusObserver`], and `try_lock` sweep.

use std::time::{Duration, Instant};

#[path = "session_program.rs"]
pub(crate) mod program;

pub(crate) const AGENT_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// Computes the whole-screen FNV-1a hash and extracts the final `tail_rows` of published screen text.
pub(crate) fn screen_agent_frame(
    text: &crate::frame_memo::ScreenText,
    tail_rows: usize,
) -> (u64, Vec<String>) {
    let first = text.row_count().saturating_sub(tail_rows);
    let rows = text.rows().skip(first).map(str::to_string).collect();
    (text.fnv(), rows)
}

pub(crate) const PROGRAM_RECHECK: Duration = Duration::from_secs(5);
const PROGRAM_NAME_CONFIRM: Duration = Duration::from_millis(500);
const PROGRAM_NAMED_RECHECK_FLOOR: Duration = Duration::from_secs(4);

const fn unresolved_program_recheck(attempts: u8) -> Duration {
    match attempts {
        0 | 1 => Duration::from_millis(250),
        2 => Duration::from_secs(1),
        _ => PROGRAM_RECHECK,
    }
}

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

#[derive(Debug, Default)]
struct AgentWatch {
    seq_seen: Option<crate::control::ScreenGen>,
    cursor_seen: Option<crate::presence::Cursor>,
    zone_hash: Option<u64>,
    read_at: Option<Instant>,
    classified_at: Option<Instant>,
    program_seen: Option<Option<String>>,
    followup: bool,
    output_at: Option<Instant>,
    pgid: i32,
    resolved_at: Option<Instant>,
    resolve_attempts: u8,
    deferred_name_recheck_at: Option<Instant>,
    name_confirmed: bool,
    frame: Option<(i32, aterm_phase::Program)>,
    reading: Option<crate::presence::AgentReading>,
    reading_seq: u64,
}

/// Published agent status tuple: `(word, detail, prompt_subject, program_name)`.
pub(crate) type AgentPublish = (
    &'static str,
    Option<String>,
    Option<String>,
    Option<aterm_phase::Program>,
);

/// Decision returned by a single agent screen observation step.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentStep {
    pub(crate) reading_changed: bool,
    pub(crate) turn: crate::presence::TurnFacts,
    pub(crate) publish: Option<AgentPublish>,
    pub(crate) gauges: crate::session_timeline::AgentGauges,
}

/// High-level activity phase of a terminal session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Unknown,
    Starting,
    Idle,
    Running,
    Quiet,
    Exited,
}

impl Phase {
    /// Stable wire string for `status` responses and timeline events.
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

/// Outcome of the last completed command or unit of work in a session.
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
    /// Returns whether the outcome represents a non-zero exit or uncaught signal.
    pub(crate) const fn is_failure(self) -> bool {
        matches!(self, Self::Failure { .. } | Self::Signal { .. })
    }

    /// Stable wire string for `status` responses.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Success => "success",
            Self::Failure { .. } => "failure",
            Self::Signal { .. } => "signal",
        }
    }

    fn wire_fields(self) -> (String, String) {
        match self {
            Self::Failure { exit_code } => (exit_code.to_string(), "-".to_string()),
            Self::Signal { signal } => ("-".to_string(), signal.to_string()),
            Self::None | Self::Success => ("-".to_string(), "-".to_string()),
        }
    }
}

/// Confidence level of a classified session [`Status`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Confidence {
    #[default]
    Unknown,
    Heuristic,
    Strong,
    Exact,
}

impl Confidence {
    /// Stable wire string for `status` responses.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Heuristic => "heuristic",
            Self::Strong => "strong",
            Self::Exact => "exact",
        }
    }
}

/// Signal source that determined the classified [`Status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    ShellBlock,
    LifecycleExit,
    ForegroundJob,
    ContentActivity,
    OutputActivity,
    Stall,
    NoEvidence,
    ConsentPrompt,
}

impl Reason {
    /// Stable wire string for `status` responses.
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

/// Shell-integration (OSC 133) prompt or command-execution state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellEvidence {
    Prompt,
    Entering,
    Executing,
    Complete { exit_code: Option<i32> },
}

/// Completed shell-integration command block (OSC 133;D).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompletedBlock {
    pub(crate) id: u64,
    pub(crate) exit_code: Option<i32>,
    pub(crate) started_ms: Option<u64>,
    pub(crate) ended_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CarriedExit {
    seen: bool,
    block: Option<CompletedBlock>,
}

const fn exit_outcome(exit_code: Option<i32>) -> Outcome {
    match exit_code {
        None => Outcome::None,
        Some(0) => Outcome::Success,
        Some(code) => Outcome::Failure { exit_code: code },
    }
}

/// Terminal PTY exit state captured at `Wake::Exit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Exited { exit_code: Option<i32> },
    Signalled { signal: u8 },
}

/// Snapshot of terminal display sequence counter and alt-screen state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActivitySample {
    pub(crate) alt_screen: bool,
    pub(crate) content_seq: u64,
    pub(crate) last_output: Option<Instant>,
}

/// Lock-gathered terminal and process evidence passed to [`StatusFsm::observe`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Evidence {
    pub(crate) shell: Option<ShellEvidence>,
    pub(crate) completed_block: Option<CompletedBlock>,
    pub(crate) lifecycle: Option<Lifecycle>,
    pub(crate) foreground_job: Option<bool>,
    pub(crate) activity: ActivitySample,
}

/// Hysteresis and quiescence timing thresholds for [`StatusFsm`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct StatusPolicy {
    pub(crate) quiet_after: Duration,
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

/// Published classification result for a single session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) phase: Phase,
    pub(crate) last_outcome: Outcome,
    pub(crate) since: Instant,
    pub(crate) detail: Option<String>,
    pub(crate) confidence: Confidence,
    pub(crate) reasons: Vec<Reason>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    phase: Phase,
    confidence: Confidence,
    reasons: Vec<Reason>,
    conflict: bool,
    outcome: Option<Outcome>,
}

/// Pure per-session state machine mapping [`Evidence`] snapshots to a [`Status`].
#[derive(Clone, Debug)]
pub(crate) struct StatusFsm {
    policy: StatusPolicy,
    published: Status,
    prior: Option<(bool, u64)>,
    last_movement: Option<Instant>,
    last_output: Option<Instant>,
    pending: Option<(Candidate, Instant)>,
}

impl StatusFsm {
    /// Initializes a [`StatusFsm`] in `Idle` at `now`.
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

    /// Returns the currently published [`Status`].
    pub(crate) const fn status(&self) -> &Status {
        &self.published
    }

    fn set_detail(&mut self, detail: Option<String>) -> bool {
        if self.published.detail == detail {
            return false;
        }
        self.published.detail = detail;
        true
    }

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

    /// Folds an [`Evidence`] snapshot at `now` and returns whether the published [`Status`] changed.
    pub(crate) fn observe(&mut self, evidence: &Evidence, now: Instant) -> bool {
        self.note_activity(&evidence.activity, now);
        let candidate = self.classify(evidence, now);
        self.apply(candidate, now)
    }

    fn note_activity(&mut self, sample: &ActivitySample, now: Instant) {
        let identity = (sample.alt_screen, sample.content_seq);
        match self.prior {
            Some((prior_alt, prior_seq))
                if prior_alt == sample.alt_screen && sample.content_seq != prior_seq =>
            {
                self.last_movement = Some(now);
            }
            _ => {}
        }
        self.prior = Some(identity);
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

        let moved = self.moved_recently(now)
            || (self.published.phase == Phase::Running
                && self.output_recently(&evidence.activity, now));

        if let Some(shell) = evidence.shell {
            let conflict =
                matches!(shell, ShellEvidence::Executing) && evidence.foreground_job == Some(false);
            return match shell {
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

    fn apply(&mut self, candidate: Candidate, now: Instant) -> bool {
        let outcome = candidate.outcome.unwrap_or(self.published.last_outcome);
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

/// Extracts [`ShellEvidence`] from `term`'s latest OSC 133 block.
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

/// Drops stale `AtPrompt` shell evidence when an unintegrated shell has a live foreground job.
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

/// Extracts the latest [`CompletedBlock`] from `term`'s OSC 133 block list.
pub(crate) fn completed_block(term: &aterm_core::terminal::Terminal) -> Option<CompletedBlock> {
    use aterm_types::BlockState;

    let current = term.current_block();
    let block = match current.map(|current| current.state) {
        Some(BlockState::Complete) => current?,
        Some(BlockState::PromptOnly | BlockState::EnteringCommand) | None => {
            match term.last_completed_block() {
                Some(archived) if archived.is_complete() => archived,
                Some(_) => return None,
                None => {
                    let closed = current?.id.checked_sub(1)?;
                    term.last_finished_block()
                        .filter(|finished| finished.id == closed)?
                }
            }
        }
        Some(_) => return None,
    };
    Some(CompletedBlock::of(block))
}

impl CompletedBlock {
    /// Converts an [`aterm_types::OutputBlock`] into a [`CompletedBlock`].
    pub(crate) const fn of(block: &aterm_types::OutputBlock) -> CompletedBlock {
        CompletedBlock {
            id: block.id,
            exit_code: block.exit_code,
            started_ms: block.command_exec_start_time_ms,
            ended_ms: block.command_end_time_ms,
        }
    }
}

/// Reads completed and executing shell-integration blocks from `term` for the session log.
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
    let mut past: Vec<&aterm_types::OutputBlock> = term
        .all_blocks()
        .rev()
        .take_while(|b| from.is_none_or(|f| b.id >= f))
        .collect();
    past.reverse();
    let ran = |b: &aterm_types::OutputBlock| {
        matches!(b.state, BlockState::Executing | BlockState::Complete)
    };
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    read.started_held = count(past.iter().filter(|b| ran(b)).count());
    read.started_at_from = past.first().is_some_and(|b| Some(b.id) == from && ran(b));
    if let (Some(f), Some(oldest)) = (from, past.first().map(|b| b.id))
        && oldest > f
    {
        read.dropped = oldest - f;
        read.through = Some(oldest - 1);
    }
    for (i, block) in past.iter().copied().enumerate() {
        let closed_at = past.get(i + 1).and_then(|next| next.prompt_time_ms);
        let end = match block.state {
            BlockState::Executing if Some(block.id) == current => Some(BlockEnd::Running),
            BlockState::Complete => Some(BlockEnd::Done(CompletedBlock::of(block))),
            BlockState::Executing => Some(BlockEnd::Unended {
                ended_ms: closed_at,
            }),
            _ => None,
        };
        let end = end.filter(|end| {
            let ended = match *end {
                BlockEnd::Running => None,
                BlockEnd::Done(done) => done.ended_ms,
                BlockEnd::Unended { ended_ms } => ended_ms,
            };
            !ended.zip(since_wall).is_some_and(|(e, t)| e < t)
        });
        if end.is_some() && read.facts.len() == BLOCK_READ_MAX {
            read.more = true;
            read.started_unread = count(past.iter().skip(i).filter(|b| ran(b)).count());
            break;
        }
        let settled = Some(block.id) != current
            || matches!(block.state, BlockState::Executing | BlockState::Complete);
        if settled {
            read.through = Some(block.id);
        }
        let Some(end) = end else {
            continue;
        };
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

pub(crate) const BLOCK_READ_MAX: usize = 32;

/// Extracts the sanitized [`command_detail`] for `term`'s currently executing block.
pub(crate) fn executing_detail(term: &aterm_core::terminal::Terminal) -> Option<String> {
    command_detail(&executing_command_line(term)?)
}

/// Extracts the full command line of `term`'s currently executing block.
pub(crate) fn executing_command_line(term: &aterm_core::terminal::Terminal) -> Option<String> {
    let block = term.current_block()?;
    if block.state != aterm_types::BlockState::Executing {
        return None;
    }
    block_command_line(term, block)
}

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
        Some(explicit) => Some(explicit.to_owned()),
        None => {
            let scraped = term.block_command(block)?;
            let skip = usize::from(block.command_start_col.unwrap_or(0));
            let from = aterm_grapheme::column_to_char_index(&scraped, skip);
            Some(scraped.chars().skip(from).collect())
        }
    }
}

/// How a wrapper reads one of its own flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Takes {
    /// No value (`sudo -n`).
    Nothing,
    /// A value, attached (`-uroot`, `--user=root`) or the next word (`-u root`).
    Value,
    /// sudo's `-h`: a host, attached or the next word unless that word is a flag; without one it
    /// is help, which runs no program.
    HostOrHelp,
    /// Only an attached value (`--preserve-env[=list]`), never the next word.
    Attached,
    /// env's `-S`: a value whose words env reads as more of its own arguments.
    Split,
}

/// One wrapper's own grammar: the flags it reads before the program it runs. A flag missing from
/// its tables, including one that runs no named program (`sudo -l`, `command -v`), keeps only the
/// wrapper's name, never a later word.
struct Wrapper {
    name: &'static str,
    short: &'static [(char, Takes)],
    long: &'static [(&'static str, Takes)],
    /// `NAME=value` words may sit between its flags and the program (sudo, env, the shell's `time`).
    assignments: bool,
    /// nice's old `-N` adjustment.
    numeric: bool,
    /// env's lone `-`, the old spelling of `-i`.
    lone_dash: bool,
}

const WRAPPERS: [Wrapper; 7] = [
    Wrapper {
        name: "sudo",
        short: &[
            ('u', Takes::Value),
            ('g', Takes::Value),
            ('h', Takes::HostOrHelp),
            ('p', Takes::Value),
            ('C', Takes::Value),
            ('D', Takes::Value),
            ('r', Takes::Value),
            ('t', Takes::Value),
            ('T', Takes::Value),
            ('R', Takes::Value),
            ('U', Takes::Value),
            ('A', Takes::Nothing),
            ('B', Takes::Nothing),
            ('b', Takes::Nothing),
            ('E', Takes::Nothing),
            ('H', Takes::Nothing),
            ('k', Takes::Nothing),
            ('n', Takes::Nothing),
            ('N', Takes::Nothing),
            ('P', Takes::Nothing),
            ('S', Takes::Nothing),
            ('s', Takes::Nothing),
            ('i', Takes::Nothing),
        ],
        long: &[
            ("user", Takes::Value),
            ("group", Takes::Value),
            ("host", Takes::Value),
            ("prompt", Takes::Value),
            ("close-from", Takes::Value),
            ("chdir", Takes::Value),
            ("role", Takes::Value),
            ("type", Takes::Value),
            ("command-timeout", Takes::Value),
            ("chroot", Takes::Value),
            ("other-user", Takes::Value),
            ("askpass", Takes::Nothing),
            ("bell", Takes::Nothing),
            ("background", Takes::Nothing),
            ("preserve-env", Takes::Attached),
            ("set-home", Takes::Nothing),
            ("reset-timestamp", Takes::Nothing),
            ("non-interactive", Takes::Nothing),
            ("no-update", Takes::Nothing),
            ("preserve-groups", Takes::Nothing),
            ("stdin", Takes::Nothing),
            ("shell", Takes::Nothing),
            ("login", Takes::Nothing),
        ],
        assignments: true,
        numeric: false,
        lone_dash: false,
    },
    Wrapper {
        name: "env",
        short: &[
            ('u', Takes::Value),
            ('C', Takes::Value),
            ('P', Takes::Value),
            ('S', Takes::Split),
            ('i', Takes::Nothing),
            ('0', Takes::Nothing),
            ('v', Takes::Nothing),
        ],
        long: &[
            ("unset", Takes::Value),
            ("chdir", Takes::Value),
            ("split-string", Takes::Split),
            ("ignore-environment", Takes::Nothing),
            ("null", Takes::Nothing),
        ],
        assignments: true,
        numeric: false,
        lone_dash: true,
    },
    Wrapper {
        name: "time",
        short: &[
            ('o', Takes::Value),
            ('f', Takes::Value),
            ('p', Takes::Nothing),
            ('a', Takes::Nothing),
            ('l', Takes::Nothing),
            ('v', Takes::Nothing),
            ('q', Takes::Nothing),
        ],
        long: &[
            ("output", Takes::Value),
            ("format", Takes::Value),
            ("append", Takes::Nothing),
            ("portability", Takes::Nothing),
            ("verbose", Takes::Nothing),
            ("quiet", Takes::Nothing),
        ],
        assignments: true,
        numeric: false,
        lone_dash: false,
    },
    Wrapper {
        name: "nice",
        short: &[('n', Takes::Value)],
        long: &[("adjustment", Takes::Value)],
        assignments: false,
        numeric: true,
        lone_dash: false,
    },
    Wrapper {
        name: "command",
        short: &[('p', Takes::Nothing)],
        long: &[],
        assignments: false,
        numeric: false,
        lone_dash: false,
    },
    Wrapper {
        name: "nohup",
        short: &[],
        long: &[],
        assignments: false,
        numeric: false,
        lone_dash: false,
    },
    Wrapper {
        name: "exec",
        short: &[
            ('a', Takes::Value),
            ('c', Takes::Nothing),
            ('l', Takes::Nothing),
        ],
        long: &[],
        assignments: false,
        numeric: false,
        lone_dash: false,
    },
];
const KEYWORD_OPENERS: [&str; 6] = ["for", "while", "if", "until", "case", "{"];
const WORD_TRIM: [char; 8] = ['\'', '"', '`', ';', '(', ')', '&', '|'];

fn wrapper_named(name: &str) -> Option<&'static Wrapper> {
    WRAPPERS.iter().find(|wrapper| wrapper.name == name)
}

/// What a wrapper's grammar read before the program it runs.
enum Unwrap<W> {
    /// This word names the program.
    Program(W),
    /// env's `-S` value, from this byte of this word: env reads its words before the rest.
    Split(W, usize),
    /// A flag the grammar does not know, one that runs no named program, or no program at all.
    Closed,
}

/// What one flag word read.
enum Flag<W> {
    Read,
    /// `--`: the flags end.
    End,
    /// env's `-S` value: in the next word when it carries one, else in the flag's own word from
    /// this byte.
    Split(Option<W>, usize),
    Closed,
}

/// Reads `wrapper`'s flags and assignments off `words` up to the word naming the program it runs.
fn unwrap<W: AsRef<str>>(wrapper: &Wrapper, words: &mut impl Iterator<Item = W>) -> Unwrap<W> {
    let mut flags = true;
    while let Some(word) = words.next() {
        let text = word.as_ref();
        if flags && text.starts_with('-') {
            match read_flag(wrapper, text, words) {
                Flag::Read => continue,
                Flag::End => {
                    flags = false;
                    continue;
                }
                Flag::Split(Some(value), from) => return Unwrap::Split(value, from),
                Flag::Split(None, from) => return Unwrap::Split(word, from),
                Flag::Closed => return Unwrap::Closed,
            }
        }
        if is_assignment(text) {
            if !wrapper.assignments {
                return Unwrap::Closed;
            }
            flags = false;
            continue;
        }
        if text.starts_with('-') || basename(text).is_none() {
            return Unwrap::Closed;
        }
        return Unwrap::Program(word);
    }
    Unwrap::Closed
}

fn read_flag<W: AsRef<str>>(
    wrapper: &Wrapper,
    flag: &str,
    words: &mut impl Iterator<Item = W>,
) -> Flag<W> {
    if flag == "--" {
        return Flag::End;
    }
    if flag == "-" {
        return if wrapper.lone_dash {
            Flag::Read
        } else {
            Flag::Closed
        };
    }
    if wrapper.numeric && old_adjustment(flag) {
        return Flag::Read;
    }
    if let Some(long) = flag.strip_prefix("--") {
        let (name, attached) = match long.find('=') {
            Some(eq) => (&long[..eq], Some(2 + eq + 1)),
            None => (long, None),
        };
        return match wrapper.long.iter().find(|(known, _)| *known == name) {
            Some(&(_, takes)) => flag_value(takes, attached, words),
            None => Flag::Closed,
        };
    }
    for (at, c) in flag.char_indices().skip(1) {
        let Some(&(_, takes)) = wrapper.short.iter().find(|(known, _)| *known == c) else {
            return Flag::Closed;
        };
        if takes != Takes::Nothing {
            let rest = at + c.len_utf8();
            return flag_value(takes, (rest < flag.len()).then_some(rest), words);
        }
    }
    Flag::Read
}

/// Reads the value of a flag that `takes` one; `attached` is where it starts in the flag's word.
fn flag_value<W: AsRef<str>>(
    takes: Takes,
    attached: Option<usize>,
    words: &mut impl Iterator<Item = W>,
) -> Flag<W> {
    match (takes, attached) {
        (Takes::Nothing, None)
        | (Takes::Attached, _)
        | (Takes::Value | Takes::HostOrHelp, Some(_)) => Flag::Read,
        (Takes::Nothing, Some(_)) => Flag::Closed,
        (Takes::Split, Some(from)) => Flag::Split(None, from),
        (Takes::Value, None) => words.next().map_or(Flag::Closed, |_| Flag::Read),
        (Takes::HostOrHelp, None) => match words.next() {
            Some(host) if !host.as_ref().starts_with('-') => Flag::Read,
            _ => Flag::Closed,
        },
        (Takes::Split, None) => words
            .next()
            .map_or(Flag::Closed, |value| Flag::Split(Some(value), 0)),
    }
}

/// nice's old `-N`, `--N` and `-+N` adjustment.
fn old_adjustment(flag: &str) -> bool {
    let number = flag[1..].strip_prefix(['-', '+']).unwrap_or(&flag[1..]);
    !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())
}

/// One word of a command line: as typed (quotes and all; `None` for a word read out of an env
/// `-S` value) and as read (the shell's quote and operator characters trimmed off its ends, and
/// any control character dropped).
struct Word<'a> {
    raw: Option<&'a str>,
    text: std::borrow::Cow<'a, str>,
}

impl<'a> Word<'a> {
    fn typed(raw: &'a str) -> Self {
        Word {
            raw: Some(raw),
            text: clean(raw),
        }
    }
}

impl AsRef<str> for Word<'_> {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

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

/// `segment`'s words as the program it runs reads them. The shell takes each redirection
/// (`>log`, `2> err`, `<<<word`, `&>x`, the `>log` of `make>log`) and its target out of the words
/// before any program sees them, so they are left out. A word that holds a command or process
/// substitution (`$(..)`, a backtick, `<(..)`) ends the words: what it expands to, and so where
/// every later word falls, is not known from the line.
fn argv(segment: &str) -> impl Iterator<Item = Word<'_>> {
    let mut words = shell_words(segment);
    std::iter::from_fn(move || {
        loop {
            let word = words.next()?;
            if expands(word) {
                return None;
            }
            let start = match redirection(word) {
                None => return Some(Word::typed(word)),
                Some(Redirect::Attached(start)) => start,
                Some(Redirect::Next(start)) => match words.next() {
                    Some(target) if expands(target) => return None,
                    _ => start,
                },
                Some(Redirect::Unread) => return None,
            };
            if start > 0 {
                return Some(Word::typed(&word[..start]));
            }
        }
    })
    .fuse()
}

/// The characters of one shell word the shell itself reads, with the quote each is under (`None`
/// bare, `Some('"')` in double quotes). Quote characters, what single or `$'..'` quotes hold,
/// what a backslash escapes and what a pwsh backtick escapes inside a Windows path are left out:
/// none of them is a redirection or a substitution.
fn shell_chars(raw: &str) -> impl Iterator<Item = (usize, char, Option<char>)> + '_ {
    let mut quote: Option<char> = None;
    let mut chars = raw.char_indices();
    std::iter::from_fn(move || {
        loop {
            let (at, c) = chars.next()?;
            match (quote, c) {
                (Some('\'' | '$'), '\'') | (Some('"'), '"') => quote = None,
                (Some('$' | '"') | None, '\\') => {
                    chars.next();
                }
                (Some('\'' | '$'), _) => {}
                (None, '\'' | '"') => quote = Some(c),
                (None, '$') if raw[at + 1..].starts_with('\'') => {
                    chars.next();
                    quote = Some('$');
                }
                (None, '`') if windows_path_so_far(&raw[..at]) => {
                    chars.next();
                }
                _ => return Some((at, c, quote)),
            }
        }
    })
}

/// Whether `raw` holds a command or process substitution: `$(..)`, a backtick, `<(..)`, `>(..)`.
fn expands(raw: &str) -> bool {
    shell_chars(raw).any(|(at, c, quote)| {
        let opens = raw[at + c.len_utf8()..].starts_with('(');
        c == '`' || (c == '$' && opens) || (quote.is_none() && matches!(c, '<' | '>') && opens)
    })
}

/// A redirection in one shell word.
enum Redirect {
    /// From this byte (its fd number, `{name}` or `&` included), with its target in the word.
    Attached(usize),
    /// From this byte, with its target the next word.
    Next(usize),
    /// `>!` with its target apart: zsh reads the next word as the target, bash a file named `!`.
    Unread,
}

fn redirection(raw: &str) -> Option<Redirect> {
    let (op, ..) =
        shell_chars(raw).find(|&(_, c, quote)| quote.is_none() && matches!(c, '<' | '>'))?;
    let head = &raw[..op];
    let fd = !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit());
    let named = head
        .strip_prefix('{')
        .and_then(|head| head.strip_suffix('}'))
        .is_some_and(|name| {
            !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        });
    let start = if fd || named {
        0
    } else {
        head.strip_suffix('&').map_or(op, str::len)
    };
    let mut end = raw[op..]
        .find(|c| !matches!(c, '<' | '>' | '&' | '|' | '!'))
        .map_or(raw.len(), |len| op + len);
    if &raw[op..end] == "<<" && raw[end..].starts_with('-') {
        end += 1;
    }
    Some(if end < raw.len() {
        Redirect::Attached(start)
    } else if raw[op..end].contains('!') {
        Redirect::Unread
    } else {
        Redirect::Next(start)
    })
}

/// `raw` as the shell hands it on, its quotes and escapes taken off; `None` when a quote is left
/// open or a backslash ends it.
fn shell_unquote(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut quote: Option<char> = None;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, '\\') => out.push(chars.next()?),
            (Some('"'), '\\') => {
                let next = chars.next()?;
                if !matches!(next, '$' | '`' | '"' | '\\') {
                    out.push('\\');
                }
                out.push(next);
            }
            _ => out.push(c),
        }
    }
    quote.is_none().then_some(out)
}

/// The words env's `-S` reads, from byte `from` of `word` as read: what was typed there, with
/// the shell's quotes and escapes taken off. `None` when that is not known, or when it holds a
/// quote, a backslash, a `$` or a `#`, which env reads by its own rules (a quoted word, an
/// escape, a variable, a comment), so its words are not the ones a split on spaces finds.
fn split_value(word: &Word<'_>, from: usize) -> Option<String> {
    let typed: String = shell_unquote(word.raw?)?
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let value = typed.strip_prefix(word.text.get(..from)?)?;
    (!value.contains(['\'', '"', '\\', '$', '#'])).then(|| value.to_owned())
}

/// A segment's words, with an env `-S` value's words read before the rest.
struct Spliced<I> {
    split: Option<(String, usize)>,
    rest: I,
}

impl<'a, I: Iterator<Item = Word<'a>>> Iterator for Spliced<I> {
    type Item = Word<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((value, at)) = &mut self.split {
            if let Some(skip) = value[*at..].find(|c: char| !c.is_whitespace()) {
                let start = *at + skip;
                let end = value[start..]
                    .find(char::is_whitespace)
                    .map_or(value.len(), |len| start + len);
                *at = end;
                return Some(Word {
                    raw: None,
                    text: value[start..end].trim_matches(WORD_TRIM).to_owned().into(),
                });
            }
            self.split = None;
        }
        self.rest.next()
    }
}

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

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

fn leading_word(segment: &str) -> Option<&str> {
    let mut words = shell_words(segment)
        .map(|w| w.trim_matches(WORD_TRIM))
        .filter(|w| !is_assignment(w) && (*w == "{" || w.chars().any(char::is_alphanumeric)));
    let mut first = words.next()?;
    while let Some(wrapper) = wrapper_named(basename(first).unwrap_or(first)) {
        match unwrap(wrapper, &mut words) {
            Unwrap::Program(word) => first = word,
            Unwrap::Split(..) | Unwrap::Closed => break,
        }
    }
    Some(first)
}

fn shell_words(segment: &str) -> impl Iterator<Item = &str> {
    let mut rest = 0;
    std::iter::from_fn(move || {
        let start = rest + segment[rest..].find(|c: char| !c.is_whitespace())?;
        let end = word_end(segment, start, false).min(word_end(segment, start, true));
        rest = end;
        Some(&segment[start..end])
    })
}

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
    segment[start..]
        .find(char::is_whitespace)
        .map_or(segment.len(), |offset| start + offset)
}

fn windows_path_so_far(head: &str) -> bool {
    let mut chars = head.chars();
    let drive = matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    );
    drive || head.contains('\\')
}

/// Reduces a shell command line to a safe `program [subcommand]` summary without flags or paths.
pub(crate) fn command_detail(cmdline: &str) -> Option<String> {
    const MAX_CHARS: usize = 48;

    let segments = split_list(cmdline);
    let segment = segments
        .iter()
        .map(|(_, text)| *text)
        .find(|text| leading_word(text).is_some_and(|w| KEYWORD_OPENERS.contains(&w)))
        .unwrap_or_else(|| running_segment(cmdline, &segments));
    let mut words = Spliced {
        split: None,
        rest: argv(segment),
    };
    let mut program_word =
        words.find(|w| !is_assignment(&w.text) && w.text.chars().any(char::is_alphanumeric))?;
    while let Some(wrapper) = basename(&program_word.text).and_then(wrapper_named) {
        match unwrap(wrapper, &mut words) {
            Unwrap::Program(word) => program_word = word,
            Unwrap::Split(value, from) if words.split.is_none() => {
                let Some(value) = split_value(&value, from) else {
                    return Some(wrapper.name.to_owned());
                };
                words.split = Some((value, 0));
            }
            Unwrap::Split(..) | Unwrap::Closed => return Some(wrapper.name.to_owned()),
        }
    }
    let program = basename(&program_word.text)?;
    let program_end = program
        .char_indices()
        .nth(MAX_CHARS)
        .map_or(program.len(), |(end, _)| end);
    let mut detail = program[..program_end].to_string();
    if let Some((_, vocabulary)) = SUBCOMMANDS.iter().find(|(p, _)| *p == program) {
        let sub = words.find(|w| !w.text.is_empty() && !w.text.starts_with('-'));
        if let Some(sub) = sub.filter(|s| vocabulary.contains(&&*s.text)) {
            detail.push(' ');
            detail.push_str(&sub.text);
        }
    }
    if let Some((end, _)) = detail.char_indices().nth(MAX_CHARS) {
        detail.truncate(end);
    }
    Some(detail)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ListOp {
    Seq,
    And,
    Or,
}

fn split_list(cmdline: &str) -> Vec<(ListOp, &str)> {
    let bytes = cmdline.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut op = ListOp::Seq;
    let mut quote: Option<u8> = None;
    // A here-document's lines, after the line that opens it, are its text and its delimiter,
    // never a command.
    let mut heredoc = false;
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
            b'<' if bytes.get(i + 1) == Some(&b'<') => {
                let here_string = bytes.get(i + 2) == Some(&b'<');
                heredoc |= !here_string;
                i += if here_string { 3 } else { 2 };
                continue;
            }
            b'\n' if heredoc => {
                segments.push((op, &cmdline[start..i]));
                return segments;
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

fn running_segment<'a>(cmdline: &'a str, segments: &[(ListOp, &'a str)]) -> &'a str {
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
    while chain.len() > 1
        && chain
            .last()
            .is_some_and(|(op, text)| *op == ListOp::Or || text.trim().is_empty())
    {
        chain = &chain[..chain.len() - 1];
    }
    chain.last().map_or(cmdline, |(_, text)| text)
}

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

const NODE_SUBCOMMANDS: &[&str] = &[
    "run", "install", "test", "build", "start", "dev", "ci", "add", "remove", "exec", "publish",
    "init", "link", "update", "outdated", "audit", "lint", "format", "dlx", "create", "why",
    "list",
];

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

pub(crate) const WINDOWS_CONTROL_C_EXIT: i32 = -1_073_741_510;

/// Returns whether `exit_code` indicates a Ctrl+C interrupt (`130` on Unix or `STATUS_CONTROL_C_EXIT` on Windows).
pub(crate) const fn interrupted_exit(exit_code: i32) -> bool {
    exit_code == WINDOWS_CONTROL_C_EXIT || (cfg!(windows) && exit_code == 130)
}

/// Formats short tab-chrome summary text for `status`, or `None` when idle.
pub(crate) fn summary_text(status: &Status) -> Option<String> {
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

/// Formats an identified agent's published phase as tab-chrome activity text.
pub(crate) fn agent_activity_text(phase: &crate::presence::AgentPhase) -> Option<String> {
    use crate::presence::AgentPhase as P;
    Some(match phase {
        P::Prompt { detail } => crate::presence::prompt_band_word(detail.as_deref()),
        P::Unknown => return None,
        other => other.band_word().to_string(),
    })
}

/// Maps a session's [`Status`] to its tab indicator bits.
#[must_use]
pub(crate) const fn status_indicators(status: &Status) -> crate::tab_model::TabIndicators {
    crate::tab_model::TabIndicators {
        dirty: false,
        busy: matches!(status.phase, Phase::Running | Phase::Quiet),
        attention: false,
        status_attention: status.last_outcome.is_failure(),
    }
}

pub(crate) const STATUS_CHANGE: &str = "status-change";

const fn conpty_status_field(
    windows: bool,
    host: Option<aterm_pty::ConsoleHostKind>,
) -> &'static str {
    if !windows {
        return "";
    }
    match host {
        Some(aterm_pty::ConsoleHostKind::OpenConsole) => " conpty=openconsole",
        Some(aterm_pty::ConsoleHostKind::Inbox) => " conpty=inbox",
        None => " conpty=-",
    }
}

/// Formats `needs=<word|-> needs_n=<n|->` fields for `status` output.
pub(crate) fn needs_fields(verdict: &crate::needs::Verdict) -> String {
    let n = verdict
        .harness_ask()
        .map_or_else(|| "-".to_string(), |ask| ask.count().to_string());
    format!("{} needs_n={n}", verdict.needs_word())
}

/// Formats the `needs_told=` development diagnostic field for `status` output.
#[cfg(any(test, debug_assertions))]
pub(crate) fn needs_told_field(told: &crate::needs::ToldNeeds, entry: Option<&str>) -> String {
    match (told.words_for(entry), told.current()) {
        (Some(needs), _) => format!(" needs_told={}", needs.len()),
        (None, Some(_)) => " needs_told=stale".to_string(),
        (None, None) => " needs_told=-".to_string(),
    }
}

/// A test's reading of `status` in its `status-change` row's words (the view prints the row from the mark, [`status_facts`]).
#[cfg(test)]
pub(crate) fn status_event_payload(status: &Status) -> String {
    let (exit_code, signal) = status.last_outcome.wire_fields();
    format!(
        "phase={} outcome={} exit_code={exit_code} signal={signal} confidence={}",
        status.phase.as_str(),
        status.last_outcome.as_str(),
        status.confidence.as_str(),
    )
}

/// Projects `status` into typed [`crate::session_log::observed::StatusFacts`] for the session log.
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

/// Per-session status classifier table enforcing observation intervals and scheduling wakes.
#[derive(Debug)]
pub(crate) struct StatusObserver {
    policy: StatusPolicy,
    min_interval: Duration,
    badge: bool,
    sessions: std::collections::HashMap<u64, SessionSlot>,
    next_due_any: Option<Instant>,
    swept_pool_epoch: u64,
    unclassified: Vec<u64>,
    agents: std::collections::HashMap<u64, AgentWatch>,
    agent_seq: u64,
    agent_cleared_at: u64,
    programs: program::ProgramResolver,
    pub(crate) inputs: crate::input_stall::InputWatches,
    footer_asked: std::collections::HashMap<u64, FooterAsk>,
    rollout_asked: std::collections::HashMap<u64, crate::codex_rollout_tap::RolloutAsk>,
    pub(crate) slog_wakes: crate::session_log::Wakes,
    pub(crate) slog_memory: crate::session_log::MemorySweep,
    program_states: std::collections::HashMap<u64, crate::program_state::ProgramStateGate>,
}

#[derive(Clone, Copy, Debug)]
struct FooterAsk {
    at: Instant,
    pgid: i32,
    stopped: bool,
    image: Option<u64>,
}

#[derive(Debug)]
struct SessionSlot {
    fsm: StatusFsm,
    next_due: Instant,
    revision: u64,
    lifecycle: Option<Lifecycle>,
    failure: FailureMark,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FailureMark {
    failing: bool,
    generation: u64,
    block: Option<u64>,
    seen: u64,
}

impl FailureMark {
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

    const fn unseen(&self) -> bool {
        self.failing && self.generation > self.seen
    }

    const fn see(&mut self) -> bool {
        let quieted = self.unseen();
        self.seen = self.generation;
        quieted
    }
}

impl StatusObserver {
    /// Creates a [`StatusObserver`] with `policy` and minimum per-session observation interval.
    pub(crate) fn new(policy: StatusPolicy, min_interval: Duration) -> Self {
        Self {
            policy,
            min_interval,
            badge: true,
            sessions: std::collections::HashMap::new(),
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

    /// Schedules an off-thread Claude footer refresh for `session` when due.
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
            let prior = self.footer_asked.get(&session).copied();
            if prior.is_some_and(|ask| ask.stopped)
                || (prior.is_none() && !(pgid <= 0 && crate::claude_footer::drawn_for(program)))
            {
                return;
            }
            let current = timeline.lock().unwrap_or_else(|p| p.into_inner());
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
            let current = timeline.lock().unwrap_or_else(|p| p.into_inner());
            if current.agent().program_pgid != pgid
                || !crate::claude_footer::drawn_for(current.agent().program.as_deref())
            {
                return;
            }
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

    /// Schedules an off-thread Codex rollout log tap read for `session` when due.
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

    /// Returns the published [`crate::input_stall::InputStallFact`] for `session`, if any.
    pub(crate) fn input_stall(&self, session: u64) -> Option<&crate::input_stall::InputStallFact> {
        self.inputs.published(session)
    }

    /// Returns whether input-stall probing is currently armed for `session`.
    pub(crate) fn input_armed(&self, session: u64) -> bool {
        self.inputs.armed(session)
    }

    /// Clears input-stall tracking for `session` and returns any previously published stall.
    pub(crate) fn forget_input(
        &mut self,
        session: u64,
    ) -> Option<crate::input_stall::InputStallFact> {
        self.inputs.forget(session)
    }

    /// Returns the earliest input-watch probe deadline across all sessions.
    pub(crate) fn next_input_wake(&self) -> Option<Instant> {
        self.inputs.next_wake()
    }

    /// Returns whether any tracked session may be due for classification at `now`.
    pub(crate) fn any_due(&self, now: Instant) -> bool {
        self.next_due_any.is_none_or(|due| now >= due)
    }

    /// Updates the next-due watermark and records `pool_epoch` and any unclassified session IDs after a sweep.
    pub(crate) fn note_swept(&mut self, pool_epoch: u64, unclassified: Vec<u64>) {
        self.swept_pool_epoch = pool_epoch;
        self.unclassified = unclassified;
        self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
    }

    /// Returns whether the last sweep left any pooled session unclassified.
    pub(crate) const fn has_unclassified(&self) -> bool {
        !self.unclassified.is_empty()
    }

    /// Takes the list of unclassified session IDs for a retry pass.
    pub(crate) fn take_unclassified(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.unclassified)
    }

    /// Restores session IDs that remained unclassified after a retry pass.
    pub(crate) fn restore_unclassified(&mut self, ids: Vec<u64>) {
        self.unclassified = ids;
    }

    /// Returns whether `session` has an initialized classifier slot.
    pub(crate) fn knows(&self, session: u64) -> bool {
        self.sessions.contains_key(&session)
    }

    /// Returns the session-pool epoch last observed by [`Self::note_swept`].
    pub(crate) const fn swept_pool_epoch(&self) -> u64 {
        self.swept_pool_epoch
    }

    /// Returns whether `session` is eligible for classification at `now`.
    pub(crate) fn due(&self, session: u64, now: Instant) -> bool {
        self.sessions
            .get(&session)
            .is_none_or(|slot| now >= slot.next_due)
    }

    /// Advances `session`'s next-allowed observation time when a sweep skips it due to lock contention.
    pub(crate) fn note_skipped(&mut self, session: u64, now: Instant) {
        let Some(slot) = self.sessions.get_mut(&session) else {
            return;
        };
        let due = now + self.min_interval;
        slot.next_due = due;
        self.note_output(session, now);
        self.next_due_any = Some(self.next_due_any.map_or(due, |min| min.min(due)));
    }

    /// Folds `evidence` into `session`'s [`StatusFsm`] at `now` and returns whether its status changed.
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
        let due = now + self.min_interval;
        self.next_due_any = Some(self.next_due_any.map_or(due, |min| min.min(due)));
        changed
    }

    /// Records that `session`'s PTY exited with `how` and immediately classifies it as `Exited`.
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
        self.observe(session, evidence, now)
    }

    /// Updates the `tab_status_badge` setting and returns whether it changed.
    pub(crate) const fn set_badge(&mut self, badge: bool) -> bool {
        std::mem::replace(&mut self.badge, badge) != badge
    }

    /// Returns the earliest instant when any session requires a time-driven status transition.
    pub(crate) fn next_wake(&self) -> Option<Instant> {
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
        let rollout_wake = self
            .rollout_asked
            .iter()
            .filter(|(_, ask)| ask.owed)
            .filter_map(|(id, ask)| {
                let slot = self.sessions.get(id)?;
                ask.at
                    .checked_add(crate::codex_rollout_tap::RECHECK)
                    .map(|at| at.max(slot.next_due))
            })
            .min();
        let owed = self
            .sessions
            .values()
            .filter_map(|slot| {
                // Clamp to `next_due`.
                slot.fsm.owed_wake().map(|owed| owed.max(slot.next_due))
            })
            .min();
        [owed, agent_wake, rollout_wake].into_iter().flatten().min()
    }

    /// Schedules a follow-up observation for `session` after the off-thread program resolver completes.
    pub(crate) fn note_program_answered(&mut self, session: u64) {
        if self.sessions.contains_key(&session)
            && let Some(watch) = self.agents.get_mut(&session)
        {
            watch.followup = true;
        }
    }

    /// Returns the published [`Status`] for `session`, if classified.
    pub(crate) fn status(&self, session: u64) -> Option<&Status> {
        self.sessions.get(&session).map(|slot| slot.fsm.status())
    }

    /// Returns tab indicator bits for `session`, suppressing failure badges already seen by the user.
    pub(crate) fn indicators(&self, session: u64) -> Option<crate::tab_model::TabIndicators> {
        let slot = self.sessions.get(&session)?;
        let mut bits = status_indicators(slot.fsm.status());
        bits.status_attention &= slot.failure.unseen();
        Some(bits)
    }

    /// Marks `session`'s current failure outcome as seen by the user and returns whether its badge cleared.
    pub(crate) fn note_seen(&mut self, session: u64) -> bool {
        self.sessions
            .get_mut(&session)
            .is_some_and(|slot| slot.failure.see())
    }

    /// Returns whether `session`'s current failure outcome has already been seen by the user.
    #[cfg(unix)]
    pub(crate) fn failure_seen(&self, session: u64) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|slot| slot.failure.failing && !slot.failure.unseen())
    }

    /// Takes a pending `status-change` for `session` if its phase or outcome changed, read in its row's words ([`status_event_payload`]).
    #[cfg(test)]
    pub(crate) fn take_status_event(&mut self, session: u64) -> Option<String> {
        self.take_status_transition(session)?;
        self.status(session).map(status_event_payload)
    }

    /// Takes a pending `status-change`'s typed [`crate::session_log::observed::StatusFacts`] for `session` (its mark is the row).
    pub(crate) fn take_status_transition(
        &mut self,
        session: u64,
    ) -> Option<crate::session_log::observed::StatusFacts> {
        let slot = self.sessions.get_mut(&session)?;
        let status = slot.fsm.status();
        let key = (status.phase, status.last_outcome);
        if slot.evented == Some(key) {
            return None;
        }
        slot.evented = Some(key);
        Some(status_facts(status))
    }

    /// Returns `session`'s published agent reading and sequence number.
    pub(crate) fn agent_reading(
        &self,
        session: u64,
    ) -> (u64, Option<crate::presence::AgentReading>) {
        match self.agents.get(&session) {
            Some(w) if w.reading_seq > 0 => (w.reading_seq, w.reading.clone()),
            _ => (self.agent_cleared_at, None),
        }
    }

    /// Returns the program name under which `session`'s standing agent reading was classified.
    pub(crate) fn agent_reading_program(&self, session: u64) -> Option<&str> {
        self.agents
            .get(&session)
            .filter(|w| w.reading_seq > 0)
            .and_then(|w| w.program_seen.as_ref())
            .and_then(Option::as_deref)
    }

    /// Overrides the program name recorded for `session`'s standing agent reading in tests.
    #[cfg(test)]
    pub(crate) fn stand_in_reading_program(&mut self, session: u64, program: &str) {
        if let Some(w) = self.agents.get_mut(&session) {
            w.program_seen = Some(Some(program.to_owned()));
        }
    }

    /// Returns the sequence number of `session`'s published agent reading.
    pub(crate) fn agent_reading_seq(&self, session: u64) -> u64 {
        match self.agents.get(&session) {
            Some(w) if w.reading_seq > 0 => w.reading_seq,
            _ => self.agent_cleared_at,
        }
    }

    /// Marks `session` as owing an agent screen read after new PTY output arrives.
    pub(crate) fn note_output(&mut self, session: u64, now: Instant) {
        if let Some(w) = self.agents.get_mut(&session) {
            w.followup = true;
            w.output_at = Some(now);
        }
    }

    /// Schedules one additional observation for `session` at its next ordinary interval.
    pub(crate) fn owe_look(&mut self, session: u64) {
        if self.sessions.contains_key(&session)
            && let Some(w) = self.agents.get_mut(&session)
        {
            w.followup = true;
        }
    }

    /// Returns whether the current sweep should read `session`'s terminal screen rows for agent classification.
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

    #[allow(
        clippy::too_many_arguments,
        reason = "one observation's facts, each read under a different lock or \
                  syscall by the sweep; a struct would only rename the list"
    )]
    /// Runs one agent screen-classification step for `session`.
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
            w.resolve_attempts = 0;
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
                w.deferred_name_recheck_at = None;
                if !group_moved && !resolving {
                    w.name_confirmed = true;
                }
            }
        } else {
            w.deferred_name_recheck_at = None;
            w.name_confirmed = false;
        }
        let Some(rows) = rows else {
            let handback = group_moved && pgid > 0;
            w.followup = resolving || program_moved || screen_moved || handback;
            if group_moved {
                w.program_seen = None;
                if handback
                    && w.classified_at
                        .is_none_or(|at| now.saturating_duration_since(at) >= AGENT_MIN_INTERVAL)
                    && let Some(slot) = self.sessions.get_mut(&session)
                {
                    w.output_at = None;
                    slot.next_due = now;
                    self.next_due_any = Some(self.next_due_any.map_or(now, |due| due.min(now)));
                }
            }
            return AgentStep::default();
        };
        w.read_at = Some(now);
        let moved = screen_moved;
        w.seq_seen = Some(generation);
        w.cursor_seen = Some(cursor);
        w.followup = moved || resolving;
        let hash = zone_hash(crate::presence::live_zone(&rows), cursor);
        if w.zone_hash == Some(hash) && !program_moved {
            return AgentStep::default();
        }
        w.zone_hash = Some(hash);
        w.program_seen = Some(program.clone());
        w.classified_at = Some(now);
        let known = w.frame.filter(|(g, _)| *g == pgid).map(|(_, p)| p);
        let pending = program.is_none() && pgid > 0;
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
        let subject = verdict.subject().map(str::to_string);
        let gauges = crate::session_timeline::AgentGauges {
            ctx_pct: w.reading.as_ref().and_then(|r| r.context_pct),
            shells: match verdict.program() {
                Some(aterm_phase::Program::Claude) => aterm_phase::shells_running(&rows),
                Some(
                    aterm_phase::Program::Codex
                    | aterm_phase::Program::Grok
                    | aterm_phase::Program::Kimi
                    | aterm_phase::Program::Agy
                    | aterm_phase::Program::Muse
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

    /// Returns whether `session`'s foreground process group leader should be resolved now.
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
            w.name_confirmed = true;
        }
        w.resolved_at = Some(now);
        w.resolve_attempts = w.resolve_attempts.saturating_add(1).min(3);
    }

    /// Requests an off-thread process-name lookup for `pgid` into `timeline`.
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

    /// Sets the session-log wake callback used by off-thread Claude footer reads.
    pub(crate) fn set_log_wake(&mut self, wake: &crate::claude_footer::LogWake) {
        self.programs.set_log_wake(wake);
    }

    /// Returns the status revision counter for `session`.
    pub(crate) fn revision(&self, session: u64) -> u64 {
        self.sessions.get(&session).map_or(0, |slot| slot.revision)
    }

    /// Updates `session`'s running-command `detail` and returns whether it changed.
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

    /// Folds an OSC 7501 program-state reading for `session` and returns whether it changed.
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

    /// Returns the active OSC 7501 `(state, since)` reading for `session`, if any.
    pub(crate) fn program_state(
        &self,
        session: u64,
    ) -> Option<(aterm_core::terminal::program_state::ProgramState, Instant)> {
        self.program_states
            .get(&session)
            .and_then(crate::program_state::ProgramStateGate::shown)
    }

    /// `session`'s `status program_state= program_state_ms=` fields, from its gate alone.
    pub(crate) fn program_state_fields(&self, session: u64) -> String {
        crate::program_state::status_fields(self.program_states.get(&session))
    }

    /// Returns the number of OSC 7501 turn completions observed for `session`'s current foreground program.
    pub(crate) fn program_turn_ends(&self, session: u64) -> u64 {
        self.program_states
            .get(&session)
            .map_or(0, crate::program_state::ProgramStateGate::turn_ends)
    }

    /// Returns the number of OSC 7501 turn starts observed for `session`'s current foreground program.
    pub(crate) fn program_turn_starts(&self, session: u64) -> u64 {
        self.program_states
            .get(&session)
            .map_or(0, crate::program_state::ProgramStateGate::turn_starts)
    }

    /// Returns the engine's raw OSC 7501 turn count for `session`.
    pub(crate) fn program_turns(&self, session: u64) -> u64 {
        self.program_states
            .get(&session)
            .map_or(0, crate::program_state::ProgramStateGate::turns)
    }

    /// Returns the `Instant` when `session`'s foreground program began its latest counted turn.
    pub(crate) fn program_turn_began(&self, session: u64) -> Option<Instant> {
        self.program_states
            .get(&session)
            .and_then(crate::program_state::ProgramStateGate::turn_started)
    }

    /// Removes all classifier and agent-watch state for a retired `session`.
    pub(crate) fn retire(&mut self, session: u64) {
        self.agents.remove(&session);
        self.program_states.remove(&session);
        let _ = self.inputs.forget(session);
        self.programs.retire(session);
        crate::claude_footer::stop_session(session);
        self.footer_asked.remove(&session);
        crate::codex_rollout_tap::retire_session(session);
        self.rollout_asked.remove(&session);
        if self.sessions.remove(&session).is_some() {
            self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
        }
    }

    /// Applies a new [`StatusPolicy`] and minimum interval across all live sessions.
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
        self.next_due_any = self.sessions.values().map(|slot| slot.next_due).min();
        true
    }

    /// Clears all per-session classifier state when `tab_status` is disabled.
    pub(crate) fn clear(&mut self) -> bool {
        let had = !self.sessions.is_empty();
        self.sessions.clear();
        self.programs.clear_pending();
        self.footer_asked.clear();
        self.rollout_asked.clear();
        self.agents.clear();
        self.agent_seq += 1;
        self.agent_cleared_at = self.agent_seq;
        self.next_due_any = None;
        self.unclassified.clear();
        had
    }
}

impl crate::App {
    /// Disabled projection cannot keep a box marker or later revive its pending receipt.
    fn clear_box_banner_receipts(&mut self) {
        for session in self.pool.iter() {
            session.ctx.box_banner.shown(None);
        }
        for carry in self.status_carry.values_mut() {
            carry.box_receipt = None;
        }
        for adopted in &mut self.seamless_adopt {
            adopted.box_receipt = None;
        }
    }

    /// Sweeps all due sessions under `try_lock`, returning the IDs whose published status or chrome changed.
    pub(crate) fn observe_session_statuses(&mut self, now: std::time::Instant) -> Vec<u64> {
        if !self.config.tab_status_or_default() {
            self.clear_box_banner_receipts();
            drop(self.session_factory.edge_inbox.take());
            return Vec::new();
        }
        let pool_epoch = self.pool.insert_epoch();
        let retry_only = pool_epoch == self.session_status.swept_pool_epoch()
            && !self.session_status.any_due(now);
        let edged = self.session_factory.edge_inbox.take();
        if retry_only && !self.session_status.has_unclassified() && edged.is_empty() {
            return Vec::new();
        }
        let _timed = crate::metrics::SweepTimer::start();
        let retry = if retry_only {
            self.session_status.take_unclassified()
        } else {
            Vec::new()
        };
        let pool = &self.pool;
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
        let mut claims_cleared = Vec::new();
        let mut hovers_renamed = Vec::new();
        let mut clauses_moved: Vec<u64> = Vec::new();
        let mut program_done = Vec::new();
        let mut turns_moved = Vec::new();
        let mut visited = Vec::new();
        let footer_wake = self.footer_log_wake();
        self.session_status.set_log_wake(&footer_wake);
        for (session, edged_visit) in sessions {
            #[cfg(test)]
            crate::work_counts::status_probe();
            let id = session.id;
            if !edged_visit && !self.session_status.due(id, now) {
                continue;
            }
            visited.push(id);
            let (master, pid) = (session.master, session.pid);
            let latest_output_ns = session
                .latest_output_activity_ns
                .load(std::sync::atomic::Ordering::Relaxed);
            let Ok(guard) = session.term.try_lock() else {
                if edged_visit {
                    self.session_factory.edge_inbox.push(id);
                } else {
                    self.session_status.note_skipped(id, now);
                }
                continue;
            };
            let shell = shell_evidence(&guard);
            let reports_commands = crate::quit_safety::shell_reports_commands(&guard);
            let engine = crate::session_log::observed::EngineRead::of(&guard);
            let integration_degraded =
                engine.integration == aterm_core::terminal::ShellIntegrationPosture::Degraded;
            let completed_block = completed_block(&guard);
            let alt_screen = guard.is_alternate_screen();
            let content_seq = guard.content_seq();
            let generation = crate::control::screen_gen(&guard);
            let resize_probe = crate::resize_ledger::EngineProbe::sample(&guard);
            let detail = executing_detail(&guard);
            let cwd_report = guard.reported_cwd_seq();
            let pgid = if master >= 0 {
                (self.session_factory.fg_probe)(master)
            } else {
                -1
            };
            let program_report = guard.program_state();
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
            let cursor = crate::presence::Cursor::At(Some(usize::from(guard.cursor().row)));
            let read = (!guard.frame_unfinished_at(now)
                && self
                    .session_status
                    .agent_zone_wanted(id, generation, cursor, &program, now))
            .then(|| std::sync::Arc::new(crate::frame_memo::ScreenText::of(&guard)));
            drop(guard);
            let (zone, stamp) = match read {
                Some(text) => {
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
            let last_input = self.windows.values().find_map(|ws| {
                ws.front_terminal()
                    .is_some_and(|t| t.session == id)
                    .then_some(ws.last_key_at)
                    .flatten()
            });
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
            let shell = prompt_yields_to_job(shell, reports_commands, foreground_job);
            let evidence = Evidence {
                shell,
                completed_block,
                lifecycle: None,
                foreground_job,
                activity: ActivitySample {
                    alt_screen,
                    content_seq,
                    last_output,
                },
            };
            let status_changed = self.session_status.observe(id, &evidence, now);
            let detail_changed = self.session_status.set_detail(id, detail);
            let seen = self.windows.values().any(|ws| {
                crate::os_window_focused(ws) && ws.front_terminal().is_some_and(|t| t.session == id)
            }) && self.session_status.note_seen(id);
            let status_event = status_changed
                .then(|| self.session_status.take_status_transition(id))
                .flatten();
            if let Some(facts) = status_event {
                let mut timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let at = timeline.slog().now();
                timeline.slog_mut().observe_status(facts, at);
                timeline.record(STATUS_CHANGE);
            }
            let (previous_group, wake, program_honoured) = {
                let mut timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                timeline.witness_cwd_report(cwd_report, pgid);
                let previous = timeline.sweep_slog(&blocks, seen, pid, pgid);
                let at = timeline.slog().now();
                timeline.slog_mut().observe_engine(&engine, at);
                timeline.slog_mut().program_tick(at);
                timeline.slog_mut().shown_tick(at);
                // The claim is judged by the program in front once this sweep's foreground move
                // is noted (unnamed right after one), never by the name read before it, and only
                // where the report names that program.
                let honoured = crate::program_state::honoured_for(
                    timeline.agent().program.as_deref(),
                    program_report.state.map(|s| s.app),
                );
                (previous, timeline.slog_wake(), honoured)
            };
            self.session_status.slog_wakes.note(id, wake);
            let group_changed = previous_group.is_some();
            if group_changed
                && crate::session_timeline::release_proven_if_gone(
                    &session.ctx,
                    pgid,
                    crate::metrics::now_us(),
                )
            {
                claims_cleared.push(id);
            }
            let turn_begun = program_report.turns != self.session_status.program_turns(id);
            let program_state_moved = self.session_status.observe_program_state(
                id,
                program_report,
                program_honoured,
                group_changed,
                now,
            );
            if turn_begun {
                turns_moved.push(id);
            }
            let shown = self
                .session_status
                .program_state(id)
                .map(|(state, _)| state);
            // An inherited marker can precede this gate's first projection. Keep its
            // custody synchronized even when the projection is already None.
            session.ctx.box_banner.shown(
                shown
                    .filter(|state| {
                        state.state
                            == aterm_core::terminal::program_state::ProgramStateWord::Blocked
                    })
                    .map(|_| program_report.began),
            );
            if program_state_moved {
                session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .note_program_state(shown, program_report.began);
                if shown.is_some_and(|state| {
                    state.state == aterm_core::terminal::program_state::ProgramStateWord::Done
                }) {
                    program_done.push(id);
                }
                crate::harness_host::note_codex_report(
                    session.ctx.self_id.as_str(),
                    shown.map(|state| state.app),
                );
            }
            if let Some(previous) = previous_group {
                crate::claude_footer::stop(id, previous);
                self.crash_journal.note_activity();
            }
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
            let heal = integration_degraded.then(|| {
                let channel = self
                    .store
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .has_rekey_channel(id);
                let moves = self.harness.as_ref().is_some_and(|host| {
                    host.rekeys_agents() && front_agent.is_some_and(|agent| host.admits(agent))
                });
                let codex_line = crate::self_claim::copy_of_tab(session.ctx.self_id.as_str())
                    .is_some_and(|copy| copy.resume.is_ok());
                crate::integration_notice::heal_for(channel, front_agent, moves, codex_line)
            });
            let _ = self.integration_notices.observe(id, heal, now);
            let step = self
                .session_status
                .agent_observe(id, generation, zone, cursor, program, pgid, resolve, now);
            if blocks.more {
                self.session_status.owe_look(id);
            }
            let grid = (resize_probe.lifetime, resize_probe.ordinal);
            let moved = match (step.publish, stamp) {
                (Some((word, detail, subject, reader)), Some(stamp)) => {
                    let (moved, wake) = {
                        let mut timeline = session
                            .ctx
                            .timeline
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
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
                    self.session_status.slog_wakes.note(id, wake);
                    moved
                }
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
            if moved {
                let arm = self.harness_ab_arm(id);
                let glass = self.harness_ab_glass(id);
                self.harness_screen.note_phase(id, &session.ctx, arm, glass);
                turns_moved.push(id);
            }
            let now_ms = crate::turn_ledger::now_ms();
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
            let backfill_logged = journal.is_some_and(|copy| {
                let mut timeline = session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let _ = timeline.ingest_resizes(&copy, None, now_ms);
                timeline.slog_take_wake()
            });
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
        for id in visited {
            self.reconcile_self_claim_task(id);
        }
        for &id in &edged {
            self.prefetch_card_arts_for(id);
        }
        for id in claims_cleared {
            self.on_meta_changed(id);
        }
        if self.integration_notices.owes() {
            self.say_integration();
        }
        if retry_only {
            let still: Vec<u64> = retry
                .into_iter()
                .filter(|id| self.pool.get(*id).is_some() && !self.session_status.knows(*id))
                .collect();
            self.session_status.restore_unclassified(still);
            return changed;
        }
        let unclassified: Vec<u64> = self
            .pool
            .iter()
            .map(|session| session.id)
            .filter(|id| !self.session_status.knows(*id))
            .collect();
        self.session_status.note_swept(pool_epoch, unclassified);
        changed
    }

    /// Records `session`'s PTY exit and publishes `Exited` immediately.
    pub(crate) fn note_session_exit(&mut self, session: u64) {
        self.note_exit_of(session, None);
    }

    /// Re-applies a carried exit status onto a `--hold` pane adopted across a seamless update.
    pub(crate) fn note_carried_exit(
        &mut self,
        session: u64,
        seen: bool,
        block: Option<CompletedBlock>,
    ) {
        self.note_exit_of(session, Some(CarriedExit { seen, block }));
    }

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
        let collected = self.exit_status(session, crate::app_tabs::ExitLook::First);
        let lifecycle = match collected {
            Some(aterm_pty::ChildExit::Code(code)) => Lifecycle::Exited {
                exit_code: Some(code),
            },
            Some(aterm_pty::ChildExit::Signal(signal)) => Lifecycle::Signalled { signal },
            None => Lifecycle::Exited { exit_code: None },
        };
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
        let event = moved
            .then(|| self.session_status.take_status_transition(session))
            .flatten()
            .filter(|_| carried.is_none());
        if let Some(facts) = event {
            let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
            let at = timeline.slog().now();
            timeline.slog_mut().observe_status(facts, at);
            timeline.record(STATUS_CHANGE);
            let _ = timeline.slog_take_wake();
        }
        let quieted =
            carried.is_some_and(|carried| carried.seen) && self.session_status.note_seen(session);
        if quieted {
            let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
            let at = timeline.slog().now();
            timeline.slog_mut().failure_seen(at);
            let _ = timeline.slog_take_wake();
        }
        if moved || quieted {
            self.refresh_session_status_chrome(session);
        }
    }

    /// Marks `session` as seen when it is the focused view of an active, unoccluded window `wid`.
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

    /// Reconfigures `status_observer` after `self.config` updates `tab_status*` settings.
    pub(crate) fn reconfigure_session_status(&mut self) {
        let now = std::time::Instant::now();
        self.session_factory.tab_status = self.config.tab_status_or_default();
        let moved = if self.config.tab_status_or_default() {
            self.session_status.reconfigure(
                self.config.tab_status_policy(),
                self.config.tab_status_observe_interval(),
                now,
            )
        } else {
            self.clear_box_banner_receipts();
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
        let badge = self.config.tab_status_badge_or_default();
        let badge_moved = self.session_status.set_badge(badge);
        if !moved && !badge_moved {
            return;
        }
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

    /// Returns the short status text displayed in `session`'s tab chrome, if any.
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

    /// Returns a combined revision counter covering both shell status and agent reading for `session`.
    pub(crate) fn session_status_revision(&self, session: u64) -> u64 {
        self.session_status
            .revision(session)
            .wrapping_add(self.session_status.agent_reading_seq(session))
    }

    /// Returns `session`'s contribution to its tab's indicator bits.
    pub(crate) fn session_status_indicators(
        &self,
        session: u64,
    ) -> crate::tab_model::TabIndicators {
        if !self.config.tab_status_badge_or_default() {
            return crate::tab_model::TabIndicators::default();
        }
        self.session_status.indicators(session).unwrap_or_default()
    }

    /// Refreshes tab chrome, window title, and status bar after `session`'s status changes.
    pub(crate) fn refresh_session_status_chrome(&mut self, session: u64) {
        let mut windows = self.windows_with_focused_session(session);
        for (wid, tab_id) in self.tabs_viewing_session(session) {
            if self.refresh_tab_status_indicators(wid, tab_id) && !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        self.refresh_tab_chrome_windows(windows);
    }

    /// Retires `session` from `status_observer` and clears its presence and needs state.
    pub(crate) fn retire_session_status(&mut self, session: u64) {
        self.session_status.retire(session);
        self.forget_integration_notice(session);
        self.job_probe.forget(session);
        self.retire_harness_ab(session);
        self.harness_screen_moved();
    }

    /// Builds the multi-line `sessions` status record for all live sessions.
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

    /// Formats the `status` wire record for `session` (without the leading `OK `).
    pub(crate) fn session_status_record(&self, session: u64) -> Result<String, String> {
        let Some(pooled) = self.pool.get(session) else {
            return Err(format!("no such session {session}"));
        };
        let enabled = self.config.tab_status_or_default();
        let hold = u8::from(pooled.ctx.fabric.hold().is_some());
        let hold_advisory = pooled
            .ctx
            .fabric
            .hold_advisory()
            .map_or_else(|| "-".to_string(), |h| h.reason);
        let editing = crate::control_edit::hold::count(session);
        let fabric = crate::fabric::fabric_state();
        let fabric_tail = crate::fabric::fabric_status_tail();
        let fleet = crate::fleet_listen::fleet_status_word();
        let pin = {
            let meta = pooled.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            meta.presentation_value("title")
        };
        let human_seq = pooled.ctx.human_input.seq();
        let mut look = pooled.ctx.frame.look_frame();
        let term = match pooled.term.try_lock() {
            Ok(t) => Some(t),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        let launch = pooled.ctx.launch.status_fields(human_seq);
        let reported_cwd = {
            use crate::cwd_native::ReportedCwd as _;
            term.as_deref().and_then(|t| {
                t.native_working_directory()
                    .map(std::borrow::Cow::into_owned)
            })
        };
        let (subject, subject_source) = match pin {
            Some(title) if !title.is_empty() => (Some(title), "pin"),
            _ => {
                let rungs = |t: &aterm_core::terminal::Terminal| {
                    use crate::cwd_native::ReportedCwd as _;
                    let osc = t.title().to_string();
                    if osc.is_empty() {
                        (
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
        let detail = term.as_deref().and_then(executing_detail);
        let stamp = term
            .as_deref()
            .filter(|t| !crate::frame_unfinished_now(t))
            .map(|t| (t.content_seq(), look.text(t).fnv()));
        let resize_probe = term
            .as_deref()
            .map(crate::resize_ledger::EngineProbe::sample);
        let (seq, hash) = stamp.map_or_else(
            || ("-".to_string(), "-".to_string()),
            |(seq, hash)| (seq.to_string(), format!("{hash:016x}")),
        );
        let generation = term.as_deref().map_or_else(
            || "-".to_string(),
            |t| crate::control::screen_gen(t).to_string(),
        );
        let integration = term
            .as_deref()
            .map_or("-", |t| t.shell_integration_posture().as_str());
        let body = term.as_deref().map(|t| {
            (
                t.shell_integration_rev().map(str::to_owned),
                t.shell_integration_posture(),
            )
        });
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
        let appearance = format!("{appearance} {}", self.session_theme_fields(session));
        let conpty = conpty_status_field(cfg!(windows), aterm_pty::conpty_host(pooled.master));
        let (supervisor, word, land_at, claimed_here, proven) = {
            let meta = pooled.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let now = crate::metrics::now_us();
            let live = meta.live_supervisor(now);
            let proven = meta.supervisor_proven(now);
            (
                format!(
                    "{} supervisor_proven={}",
                    live.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode),
                    u8::from(meta.claim_proven(now)),
                ),
                meta.autopilot_word(),
                meta.land_at,
                live == Some(crate::autopilot::own_holder()),
                proven,
            )
        };
        let input = crate::input_stall::status_input(
            &pooled.ctx.sink,
            self.session_status.input_stall(session),
            self.session_status.input_armed(session),
        );
        let occupancy_sid = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .map(|h| h.sid.as_str().to_string());
        let input = format!(
            "{input} {} {} {} {} {}",
            crate::disk_board::status_fields(session, reported_cwd.as_deref()),
            self.status_job_fields(session),
            crate::program_check_watch::status_fields(),
            crate::occupancy_host::status_fields(occupancy_sid.as_deref()),
            crate::occupancy_wakes::status_fields(occupancy_sid.as_deref())
        );
        let (
            agent,
            resize,
            task_record,
            (supervised_agent, group),
            last_act,
            face,
            (pause, held),
            slog,
            goal,
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
                goal_field(timeline.goal()),
            )
        };
        let supervised_agent =
            crate::harness_host::admitted_as(&pooled.ctx, supervised_agent, group);
        let autopilot =
            crate::autopilot::wire_field(self.harness_posture(), word, supervised_agent);
        let program_state = self.session_status.program_state_fields(session);
        let land_at =
            crate::autopilot::land_at_field(land_at.filter(|_| supervised_agent.is_some()));
        let task = crate::autopilot::task_field(face, claimed_here || proven, supervised_agent);
        let pause = crate::autopilot::pause_field(pause, claimed_here, supervised_agent);
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
        let land = format!(
            "{land}{}",
            stop_field(self.shown_stop(session).filter(|_| claimed_here))
        );
        #[cfg(any(test, debug_assertions))]
        let end = self.end_status_fields(
            session,
            &pooled.ctx,
            (claimed_here || proven) && supervised_agent.is_some(),
        );
        #[cfg(not(any(test, debug_assertions)))]
        let end = "";
        let human_ms = pooled.ctx.human_input.wire(crate::metrics::now_us());
        let consent = self.session_consent(session, reported_cwd.as_deref());
        let opt = |value: Option<&str>| {
            value.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode)
        };
        let presence = self.presence_status_tail(session);
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
        let typed = crate::typed_by::wire(
            self.typed_by(&pooled.ctx).as_ref(),
            aterm_time::unix_millis().unwrap_or(0),
        );
        let (mark, history_lost, integration_frozen) = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            (
                store.has_frozen_path(session),
                store.history_lost(session),
                store.is_integration_frozen(session),
            )
        };
        let born = pooled.ctx.witness.born_fields();
        let journal = pooled.ctx.crash_journal.status_words();
        let integration_rev = crate::shell_body::status_word(
            body.as_ref()
                .map(|(rev, posture)| (rev.as_deref(), *posture)),
            integration_frozen,
        );
        let (path, owner) = pooled
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .owner_columns(mark, crate::upgrade_host::now_s());
        let bells = pooled
            .ctx
            .bells
            .wire(aterm_time::unix_millis().unwrap_or(0));
        let Some(status) = self.session_status.status(session) else {
            return Ok(format!(
                "schema=1 sid={session} subject={} subject_source={subject_source} observed=false \
                 phase=unknown since_ms=- outcome=none exit_code=- signal=- detail={} \
                 confidence=unknown reasons=- attribution={} fs_consent={} conflict=false \
                 revision=0 enabled={enabled} hold={hold} hold_advisory={hold_advisory} \
                 fabric={fabric} {fabric_tail} fleet={fleet} \
                 {presence} needs={needs} hold_by={hold_by} {typed} path={path} {agent} {program_state} last_act={last_act} {appearance}{conpty} integration={integration} \
                 {input} supervisor={supervisor} autopilot={autopilot} human_ms={human_ms} \
                 human_seq={human_seq} {launch} {owner} \
                 history_lost={history_lost} {journal} integration_rev={integration_rev} {born} {resize} \
                 land_at={land_at} task={task} pause={pause}{land}{end} goal={goal} {task_record} {slog} editing={editing} {bells} gen={generation} seq={seq} hash={hash}",
                opt(subject.as_deref()),
                opt(detail.as_deref()),
                consent.attribution.as_str(),
                consent.fs_consent.as_str(),
            ));
        };
        let since_ms = std::time::Instant::now()
            .saturating_duration_since(status.since)
            .as_millis();
        let (exit_code, signal) = status.last_outcome.wire_fields();
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
             {presence} needs={needs} hold_by={hold_by} {typed} path={path} {agent} {program_state} last_act={last_act} {appearance}{conpty} integration={integration} \
             {input} supervisor={supervisor} autopilot={autopilot} human_ms={human_ms} \
             human_seq={human_seq} {launch} {owner} \
             history_lost={history_lost} {journal} integration_rev={integration_rev} {born} {resize} \
             land_at={land_at} task={task} pause={pause}{land}{end} goal={goal} {task_record} {slog} editing={editing} {bells} gen={generation} seq={seq} hash={hash}",
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

/// `status`'s `goal=`: the agent's active goal percent-encoded ([`crate::session_timeline::SessionTimeline::goal`]), `-` while it has none.
pub(crate) fn goal_field(goal: Option<&str>) -> String {
    goal.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode)
}

/// Formats the `land_held` and `land_detail` fields for `status` output.
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

/// Formats the `held` field for `status` output (§14.3): the arm that stopped the typing
/// (`same-reply`, `no-work`), `-` where aterm types on. A reader that finds no `held=` (a build
/// before it) reads it as unknown, never as `-`.
pub(crate) fn stop_field(stopped: Option<aterm_agent::harness::acts::HeldBy>) -> String {
    format!(
        " held={}",
        stopped.map_or("-", aterm_agent::harness::acts::HeldBy::word)
    )
}

/// Formats the development-build `end_glass` fields for `status` output.
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

    const CHILDLESS_PID: i32 = 0x7FFF_FFF0;

    fn stage_pty_like_sessions(app: &mut crate::App, count: u64) {
        for i in 0..count {
            let mut session = crate::stub_session(1000 + i);
            session.master = 0x4000_0000 + i32::try_from(i).expect("small fixture");
            session.pid = CHILDLESS_PID;
            app.pool.insert(session);
        }
    }

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

    #[test]
    fn a_session_whose_grid_moves_buys_a_fresh_capture() {
        let mut app = crate::App::headless_for_test();
        stage_pty_like_sessions(&mut app, 3);
        let t0 = Instant::now();
        let _ = app.observe_session_statuses(t0);
        let settled = app.job_probe.capture_count();
        assert!(settled >= 1, "the first sweep captures");

        let t1 = t0 + Duration::from_millis(500);
        let _ = app.observe_session_statuses(t1);
        assert_eq!(
            app.job_probe.capture_count(),
            settled,
            "a settled sweep must buy nothing"
        );

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

#[cfg(test)]
#[path = "session_status_edge_inbox_tests.rs"]
mod edge_inbox_tests;

#[cfg(test)]
#[path = "session_status_agent_verdict_tests.rs"]
mod agent_verdict_tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor approval event loop driving `approval` policy decisions against `Fx`.
use super::super::policy::WorkerEnv;
use super::super::policy::approval::RULE_RM_BREAKER_WARN;
use super::super::policy::approval::{
    AnswerTarget, ApprovalCtx, Choice, DeclineStep, LimitTry, QuestionPriority,
    RULE_AUTOCOMPACT_OFFER, RULE_LIMIT_RESET, RULE_LIMIT_RETRY, RULE_LIMIT_STOP, RULE_LIMIT_WAIT,
    RULE_TRUST_ANY, RULE_TRUST_DIALOG, decide, keeps_trying_its_limit, limit_no_reset_reason,
    limit_stopped_reason, proof_would_lift, unvoiced_tools,
};
use super::super::policy::box_call;
use super::super::policy::box_facts::{BoxFacts, QuestionFacts};
use super::super::policy::login::PickerGate;
use super::super::policy::question::{begun, fall_back_to, is_question_rule};
use super::super::policy::turn_end::{Seat, reads_conversation, screen_alone};
use super::*;
use crate::harness::upgrade_drive::{LiveTab, roster_rows, unique_tab_for_group};
use aterm_phase::{QuestionDialog, QuestionFocus, QuestionForm};

#[cfg(test)]
thread_local! {
    /// A test's planted panic in the approval policy (this thread's).
    pub(crate) static PANIC_AT_DECIDE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// One answer since the last review point.
#[derive(Debug, Clone)]
pub(super) struct Approved {
    command: String,
    question: Option<String>,
    declined: Option<String>,
}

enum Act {
    Approve { guard: String, choice: Choice },
    Decline { refusal: usize, text: String },
}

const REASKED_AFTER: usize = 2;

/// The pause before a re-asked question answered `answered` times is answered again.
pub(super) fn reask_pause(answered: usize) -> Duration {
    let n = u32::try_from(answered.saturating_sub(REASKED_AFTER)).unwrap_or(u32::MAX);
    crate::supervise::ladder::doubling(REFUSAL_PAUSE, n, PRESS_RETRY_MAX)
}

const MAX_UNSETTLED_WAITS: u32 = 3;

const QUESTION_STANDIN_QUIET: Duration = Duration::from_secs(10);

/// What the loop remembers of the question dialog it is answering.
#[derive(Debug, Default)]
pub(super) struct QuestionMemory {
    pub(super) keyed: Option<String>,
    pub(super) pending: Option<QuestionPending>,
    pub(super) standin: Option<String>,
    pub(super) waiting_said: Option<String>,
    pub(super) toggled: std::collections::BTreeMap<String, std::collections::BTreeSet<u8>>,
    pub(super) reasked: Option<(String, usize)>,
    pub(super) answered: Vec<String>,
    pub(super) theirs: bool,
    pub(super) held_as: Option<String>,
    pub(super) typed: std::collections::BTreeMap<String, QuestionPriority>,
    pub(super) typed_person: std::collections::BTreeMap<String, u64>,
    pub(super) entered: std::collections::BTreeSet<String>,
    pub(super) mode: Option<QuestionPriority>,
    pub(super) deciding: Option<QuestionPriority>,
    pub(super) by_text: bool,
    pub(super) fell_back: std::collections::BTreeSet<String>,
    pub(super) note_entered: Option<String>,
}

/// THE DECISION ON A BOX READ WHOLE FROM THE HISTORY (`box_head`).
pub(crate) fn whole_box_decision(
    whole: Decision,
    rows: &[String],
    screen: &[String],
    cut: impl FnOnce() -> Decision,
) -> Decision {
    let shown = rows.len().saturating_sub(screen.len());
    match whole {
        Decision::Approve {
            rule_id,
            guard,
            choice: choice @ Choice::Digit(_),
            subject,
            unproven,
        } => {
            let guard = if rows[shown..]
                .iter()
                .any(|r| super::super::policy::row_guard(r) == guard)
            {
                guard
            } else {
                match screen.iter().find(|r| !r.trim().is_empty()) {
                    Some(top) => super::super::policy::row_guard(top),
                    None => return cut(),
                }
            };
            Decision::Approve {
                rule_id,
                guard,
                choice,
                subject,
                unproven,
            }
        }
        escalate @ Decision::Escalate { .. } => escalate,
        Decision::Approve { .. } | Decision::Decline { .. } => cut(),
    }
}

impl QuestionMemory {
    /// A person gave the session input while this dialog is up (R1).
    pub(super) fn person_gave_input(&mut self) {
        if self.pending.is_some() || !self.answered.is_empty() || !self.typed.is_empty() {
            self.theirs = true;
        }
    }
}

/// What the loop remembers of the autocompact offer's stepper on the offer now up ([`Session::stepper_check`]).
#[derive(Debug, Default)]
pub(super) struct StepperTrack {
    offer: Option<String>,
    deciding: Option<(&'static str, u32)>,
    last: Option<(&'static str, u32)>,
    keys: usize,
}

/// The most stepper keys one offer takes.
pub(super) const MAX_STEPPER_KEYS: usize = aterm_phase::autocompact::LADDER.len() + 1;

/// What a question write was ([`QuestionPending::kind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingKind {
    Move,
    Enter,
    Text,
}

/// What the loop remembers of the decline it is carrying out ([`Session::press_decline`]).
#[derive(Debug, Default)]
pub(super) struct DeclineMemory {
    pub(super) pending: Option<DeclinePending>,
    pub(super) again: Option<(String, usize)>,
}

/// A decline keystroke written, as the box stood when it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclinePending {
    pub(super) key: String,
    pub(super) step: DeclineStep,
    pub(super) unseen: u32,
}

/// A question key written, as the box and its focus stood when it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QuestionPending {
    pub(super) identity: String,
    pub(super) focus: QuestionFocus,
    pub(super) kind: PendingKind,
    pub(super) waited: bool,
    pub(super) unsettled: u32,
    pub(super) retries: u32,
    pub(super) backed_off: bool,
}

fn text_unfenced(caps: &Caps, screen: &Screen) -> Option<String> {
    let mut missing: Vec<&str> = Vec::new();
    if caps.send_gen != Some(true) {
        missing.push("send if-gen=");
    }
    if screen.human_seq.is_none() {
        missing.push("the person count (human_seq)");
    }
    if screen.human == HumanInput::Unknown {
        missing.push("the person stamp (human_ms)");
    }
    (!missing.is_empty()).then(|| {
        format!(
            "this window cannot fence a typed answer: no {}",
            missing.join(", no ")
        )
    })
}

/// How a session's question is answered ([`Session::question_policy`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QuestionPolicy {
    pub(super) answer: Option<QuestionPriority>,
    pub(super) by_session: bool,
    pub(super) why: Option<String>,
}

impl QuestionPolicy {
    /// The global policy alone, no session's word read.
    pub(super) fn global(opts: &SuperviseOpts) -> Self {
        Self {
            answer: opts
                .policy
                .answer_questions
                .then_some(opts.policy.question_priority),
            by_session: false,
            why: None,
        }
    }

    /// Why the question is the person's, in the words an escalation names, when the session's word handed it over; `None` otherwise.
    pub(super) fn session_reason(&self) -> Option<String> {
        (self.by_session && self.answer.is_none()).then(|| {
            self.why.clone().unwrap_or_else(|| {
                "this session's `questions` word is ask: a question is the person's to answer"
                    .to_string()
            })
        })
    }
}

fn session_word(word: &str) -> QuestionPolicy {
    if word == "ask" {
        return QuestionPolicy {
            answer: None,
            by_session: true,
            why: None,
        };
    }
    match QuestionPriority::parse(word) {
        Some(p) => QuestionPolicy {
            answer: Some(p),
            by_session: true,
            why: None,
        },
        None => QuestionPolicy {
            answer: None,
            by_session: true,
            why: Some(format!(
                "this session's `questions` word {word} is not one this supervisor knows: a \
                 question is the person's to answer"
            )),
        },
    }
}

enum Hold {
    Again,
    HandOver(String),
    BackOff(String),
}

fn meta_word(stdout: &str, key: &str) -> Option<String> {
    let line = stdout.lines().find(|l| crate::is_ok_reply(l))?;
    let prefix = format!("{key}=");
    let raw = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))?;
    (raw != "-" && !raw.is_empty()).then(|| super::super::report::pct_decode(raw))
}

fn meta_names(stdout: &str, key: &str) -> bool {
    let prefix = format!("{key}=");
    stdout
        .lines()
        .find(|l| crate::is_ok_reply(l))
        .is_some_and(|line| line.split_whitespace().any(|w| w.starts_with(&prefix)))
}

/// Session autopilot mode and source as last read from `status` (`AutopilotWord`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AutopilotWord {
    kept: bool,
    word: Option<Autopilot>,
    fresh: bool,
    read: bool,
    answered: bool,
}

impl AutopilotWord {
    /// A decision begins: the word last read is no longer this decision's own.
    pub(super) const fn begin(&mut self) {
        self.fresh = false;
    }

    fn note(&mut self, meta: &str) {
        self.kept = meta_names(meta, "autopilot");
        self.word = meta_word(meta, "autopilot")
            .as_deref()
            .and_then(Autopilot::parse);
        self.fresh = true;
        self.read = true;
        self.answered = true;
    }

    /// Whether a decision should read the word.
    pub(super) const fn worth_a_read(&self) -> bool {
        self.kept || !self.read
    }

    /// Whether a bare `meta` reply was ever read ([`Self::read`]).
    pub(super) const fn read(&self) -> bool {
        self.read
    }

    /// The session's own word as the last reply read named it ([`Self::word`]).
    pub(super) const fn word(&self) -> Option<Autopilot> {
        self.word
    }

    /// Whether the host ever answered a bare `meta`, read or refused.
    pub(super) const fn answered(&self) -> bool {
        self.answered
    }

    const fn refused(&mut self) {
        self.fresh = true;
        self.answered = true;
    }
}

/// A question's identity for the first-key wait and the re-ask cap.
pub(super) fn question_key(dialog: &QuestionDialog, rows: &[String]) -> String {
    let form = match &dialog.form {
        QuestionForm::Single { .. } => "single",
        QuestionForm::Multi { .. } => "multi",
        QuestionForm::Preview { .. } => "preview",
        QuestionForm::Review { .. } => "review",
        QuestionForm::Unknown { .. } => "unknown",
    };
    match dialog.question() {
        Some(q) => format!("{form}\n{}", q.text),
        None => format!("{form}\n{}", box_identity(rows).unwrap_or_default()),
    }
}

const fn answers(dialog: &QuestionDialog, target: AnswerTarget) -> bool {
    !matches!(
        (&dialog.form, target),
        (QuestionForm::Multi { .. }, AnswerTarget::Option(_))
    )
}

fn dialog_rows(rows: &[String]) -> Option<String> {
    let v2 = aterm_phase::parse_prompt_v2(rows)?;
    let (top, bottom) = v2.span;
    let top = top.saturating_sub(1);
    let bottom = bottom.min(rows.len().checked_sub(1)?);
    (top <= bottom).then(|| {
        rows[top..=bottom]
            .iter()
            .map(|r| r.trim_end())
            .collect::<Vec<_>>()
            .join(" ⏎ ")
    })
}

fn note_dialog(review: &mut dyn Review, seq: u64, rule_id: &str, rows: &[String]) {
    if let Some(drawn) = dialog_rows(rows) {
        review.note(&format!("DIALOG seq={seq} rule={rule_id} {drawn}"));
    }
}

/// What "the same box" is after a press ([`Session::box_left`]).
pub(super) fn box_identity(rows: &[String]) -> Option<String> {
    let v2 = aterm_phase::parse_prompt_v2(rows)?;
    Some(identity_of(&v2, rows))
}

/// [`box_identity`] of the box `v2` read on `rows`.
fn identity_of(v2: &aterm_phase::PromptV2, rows: &[String]) -> String {
    let mut key = format!("{}\n", v2.kind.name());
    for (_, row) in box_rows(v2, rows) {
        key.push_str(row);
        key.push('\n');
    }
    key
}

/// Margin before Claude Code's auto-deny when an untouched hold ends so the policy's answer lands first.
pub(super) const DENY_MARGIN: Duration = Duration::from_secs(15);

const LEAVE_NEAR_DENY: Duration = Duration::from_secs(3);

fn leave_wait(now: Instant, deny: Option<Instant>) -> Duration {
    let Some(deny) = deny else {
        return WAIT_STEP;
    };
    let to_margin = deny
        .checked_sub(DENY_MARGIN)
        .map_or(Duration::ZERO, |end| end.saturating_duration_since(now));
    to_margin.max(LEAVE_NEAR_DENY).min(WAIT_STEP)
}

const TICK_SLACK: Duration = Duration::from_secs(3);

/// Recheck interval for a box whose auto-deny countdown has not yet been seen ticking.
pub(super) const TICK_LOOK: Duration = Duration::from_secs(1);

/// Auto-deny countdown read from a box, trusted only once seen ticking in step with the loop's clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Countdown {
    identity: String,
    left: Duration,
    deny_at: Instant,
    since: Instant,
    asked: Instant,
    ticking: bool,
    ended_said: bool,
}

/// Parses the auto-deny countdown on `rows` from a read requested at `asked`.
pub(super) fn countdown_of(rows: &[String], asked: Instant) -> Option<Countdown> {
    if !rows.iter().any(|r| r.contains('⚠')) {
        return None;
    }
    let v2 = aterm_phase::parse_prompt_v2(rows)?;
    let left = v2.auto_deny_left()?;
    Some(Countdown {
        identity: identity_of(&v2, rows),
        left,
        deny_at: asked.checked_add(left)?,
        since: asked,
        asked,
        ticking: false,
        ended_said: false,
    })
}

impl Countdown {
    /// Advances this read's countdown from `before`, marking it ticking when `left` drops in step with the clock.
    pub(super) fn after(mut self, before: Option<&Self>) -> Self {
        let Some(b) = before.filter(|b| self.left <= b.left) else {
            return self;
        };
        if self.left == b.left {
            self.deny_at = b.deny_at;
            self.since = b.since;
            self.ticking = b.ticking && self.asked.saturating_duration_since(b.since) <= TICK_SLACK;
        } else {
            let apart = self
                .deny_at
                .saturating_duration_since(b.deny_at)
                .max(b.deny_at.saturating_duration_since(self.deny_at));
            self.ticking = apart <= TICK_SLACK;
            if !self.ticking {
                return self;
            }
        }
        self.ended_said = b.ended_said;
        self
    }

    /// Returns when Claude Code denies the box, or `None` until the countdown is seen ticking.
    pub(super) fn deny(&self) -> Option<Instant> {
        self.ticking.then_some(self.deny_at)
    }
}

/// Auto-deny deadline state for the box on a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DenyClock {
    None,
    Unseen,
    At { deny: Instant },
}

/// Contiguous box episode tracking whether a person or another driver touched the session while a box was up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Episode {
    baseline: Option<u64>,
    by: Option<&'static str>,
}

const BY_DRIVER: &str = "another driver's hand was on the session while it was up";

impl Episode {
    fn touch(&mut self, by: &'static str) {
        self.by.get_or_insert(by);
    }

    fn read(&mut self, screen: &Screen) {
        let by = match (self.baseline, screen.human_seq, screen.human) {
            (None, _, _) | (_, None, _) | (_, _, HumanInput::Unknown) => {
                "whether a person touched it since it appeared cannot be told"
            }
            (Some(then), Some(now), _) if now > then => {
                "a person's gesture was counted on the session since it appeared"
            }
            (Some(then), Some(now), _) if now < then => {
                "the session's person count started over since it appeared, so whether a \
                 person touched it cannot be told"
            }
            _ => return,
        };
        self.touch(by);
    }
}

/// The rows the box `v2` on `rows` is made of, with their indexes.
pub(super) fn box_rows<'a>(
    v2: &aterm_phase::PromptV2,
    rows: &'a [String],
) -> Vec<(usize, &'a str)> {
    let (top, bottom) = v2.span;
    let countdown = v2.auto_deny.as_deref();
    let mut out = Vec::new();
    let mut rest: Option<&str> = None;
    for (i, r) in rows
        .iter()
        .enumerate()
        .take(bottom.saturating_add(1))
        .skip(top)
    {
        let t = r.trim();
        if t.is_empty() {
            continue;
        }
        let t = t.strip_prefix('❯').map_or(t, str::trim_start);
        let piece = t.trim_start_matches('⚠').trim();
        let left = match (rest, countdown) {
            (Some(left), _) => Some(left),
            (None, Some(c)) if t.starts_with('⚠') => Some(c),
            _ => None,
        };
        if let Some(left) = left.filter(|l| !piece.is_empty() && l.starts_with(piece)) {
            rest = Some(left[piece.len()..].trim_start()).filter(|l| !l.is_empty());
            continue;
        }
        rest = None;
        out.push((i, t));
    }
    out
}

/// What one `status` reply says for THE LAUNCH, each field as the server wrote it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LaunchStatus {
    pub(super) person_seq: Option<u64>,
    pub(super) input: Option<String>,
    pub(super) program: Option<String>,
}

impl LaunchStatus {
    /// The [`LaunchStatus`] of one `status` reply.
    pub(super) fn of(stdout: &str) -> Self {
        let field = |f: &str| super::escalate::status_field(stdout, f);
        Self {
            person_seq: (field("launch") == Some("person"))
                .then(|| field("launch_seq").and_then(|n| n.parse().ok()))
                .flatten(),
            input: field("input").map(str::to_string),
            program: field("program").map(str::to_string),
        }
    }
}

/// What THE LAUNCH makes of a box a person's grace would hold.
pub(super) enum LaunchCall {
    Held,
    Again,
    Answer(crate::supervise::screen::LaunchWitness),
}

/// The label of the option the focus is on in the box on `rows`, if any.
pub(super) fn focused_label(program: Option<&str>, rows: &[String]) -> Option<String> {
    aterm_phase::read(program, rows, None)
        .prompt
        .and_then(|q| q.focused().map(|o| o.label.clone()))
}

fn shown_subject(subject: &str, command: &str) -> String {
    if subject.trim().is_empty() {
        command.to_string()
    } else {
        subject
            .split('\n')
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn told_by_role(act: &Act, p: &aterm_phase::PromptV2, what: &str) -> Option<(String, bool)> {
    if aterm_phase::muse::is_review(p) {
        return None;
    }
    let note = match act {
        Act::Approve { choice, .. } => match choice {
            Choice::Digit(_) | Choice::Focus { .. } => false,
            Choice::Note { .. } => true,
            Choice::Answer(_) | Choice::FocusKey { .. } => return None,
        },
        Act::Decline { .. } => return None,
    };
    let asked = one_line(&p.description);
    let told = match what
        .strip_prefix(p.title.as_str())
        .and_then(|rest| rest.strip_prefix(" → "))
    {
        Some(answer) if !asked.trim().is_empty() => format!("{} → {answer}", asked.trim()),
        _ => what.to_string(),
    };
    Some((told, note))
}

#[derive(Debug, Clone, Copy)]
struct Reads {
    git: bool,
    bash: bool,
    asks: bool,
}

fn needs_cwd(p: &aterm_phase::PromptV2) -> bool {
    p.kind == PromptKind::Trust || p.rm_breaker().is_some() || may_run_git(p)
}

fn may_run_git(p: &aterm_phase::PromptV2) -> bool {
    p.kind == PromptKind::Bash && p.command_rows.iter().any(|r| r.contains("git"))
}

/// The session's `s-…` id and its PTY's foreground process group, off a `who` reply's rows ([`roster_rows`]).
pub(super) fn session_group(
    rows: &str,
    stable: Option<&str>,
    local: Option<&str>,
) -> Result<(String, u32), String> {
    let rows = roster_rows(rows).ok_or("`who` answered no roster this check can read")?;
    let sid = match (stable, local) {
        (Some(stable), _) => stable.to_string(),
        (None, Some(local)) => rows
            .iter()
            .find(|(id, _)| id.to_string() == local)
            .map(|(_, tab)| tab.sid.clone())
            .ok_or_else(|| format!("session {local} is not on the instance's roster (`who`)"))?,
        (None, None) => return Err("the session's id is unknown (`status` named none)".to_string()),
    };
    let tabs: Vec<LiveTab> = rows.into_iter().map(|(_, tab)| tab).collect();
    let group = tabs
        .iter()
        .find(|tab| tab.sid == sid)
        .ok_or_else(|| format!("session {sid} is not on the instance's roster (`who`)"))?
        .fgpgid
        .ok_or_else(|| format!("{sid}'s foreground process group is unknown"))?;
    if unique_tab_for_group(&tabs, group) != Some(sid.as_str()) {
        return Err(format!(
            "{sid}'s foreground process group {group} is another session's too"
        ));
    }
    let group = u32::try_from(group)
        .map_err(|_| format!("{sid}'s foreground process group {group} is out of range"))?;
    Ok((sid, group))
}

const PROGRAM_SETTLE: Duration = Duration::from_millis(2000);

impl<C: Ctl> Session<'_, C> {
    /// The session's foreground program as `status program=` names it.
    pub(super) fn foreground_program(&mut self) -> Result<Option<String>, Fail> {
        let r = self.call(&["status"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            self.person = None;
            self.launch_status = LaunchStatus::default();
            self.status_sid = None;
            self.halted = false;
            return Ok(None);
        }
        let field = |f: &str| super::escalate::status_field(&r.stdout, f).map(str::to_string);
        self.launch_status = LaunchStatus::of(&r.stdout);
        self.halted = field("hold").as_deref() == Some("1");
        self.status_sid = field("sid").filter(|s| s != "-");
        self.person = field("human_ms")
            .and_then(|ms| ms.parse::<u64>().ok())
            .and_then(|ms| self.fx.mono().checked_sub(Duration::from_millis(ms)));
        self.note_hand(&r.stdout);
        let program = field("program");
        let named_agent = program
            .as_deref()
            .and_then(aterm_phase::program_of)
            .is_some();
        Ok(if field("agent").is_some() && !named_agent {
            None
        } else {
            program
        })
    }

    /// Another driver's hand on `status` holds the loop as a person's key does and touches the box
    /// episode.
    pub(super) fn note_hand(&mut self, status: &str) {
        self.driver = crate::harness::relaunch::driver_hand(status);
        if self.driver {
            self.person = Some(self.fx.mono());
            self.episode_driven();
        }
    }

    fn box_facts(&mut self, fresh_cwd: bool, reads: Reads) -> Result<BoxFacts, Fail> {
        let Reads { git, bash, asks } = reads;
        if fresh_cwd {
            let r = self.call(&["meta"])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
            }
            if r.ok() {
                self.note_meta(&r.stdout);
            } else {
                self.cwd = None;
                self.autopilot.refused();
            }
        }
        let cwd = self.cwd.clone();
        let mut facts = BoxFacts {
            program: self.program.clone(),
            cwd: cwd.clone(),
            trusted_cwd: self.trusted_cwd.clone(),
            worker_agent: self.worker_agent,
            bypass: self.footer == Some(crate::harness::lights::Mode::Bypass),
            limit_reset_passed: self.reset_passed_here(),
            ..BoxFacts::default()
        };
        if git || bash || asks {
            let probe = self.fx.policy_probe();
            let source = self.setup.approval_env.worker.clone();
            let session = matches!(source, WorkerSource::Session);
            let worker = match source {
                WorkerSource::Session => self.worker_env(),
                WorkerSource::Fixed(worker) => worker,
            };
            if git && let (Ok(w), Some(cwd)) = (&worker, cwd.as_deref()) {
                facts.shell_cwds = probe.shell_cwds(w, cwd);
            }
            if bash && session {
                facts.shell = Some(match (&worker, cwd.as_deref()) {
                    (Ok(w), Some(cwd)) => probe.shell_startup(w, cwd),
                    (Err(why), _) => Err(format!("the worker's environment is unknown: {why}")),
                    (Ok(_), None) => Err("the session's cwd is unknown (meta cwd=-)".to_string()),
                });
            }
            if asks {
                facts.owner_asks = Some(match (&worker, cwd.as_deref()) {
                    (Ok(w), Some(cwd)) => probe.owner_asks(w, cwd),
                    (Err(why), _) => Err(format!("its environment is unknown: {why}")),
                    (Ok(_), None) => Err("its directory is unknown (meta cwd=-)".to_string()),
                });
            }
            facts.worker = Some(worker);
        }
        Ok(facts)
    }

    fn approval_ctx(
        &self,
        facts: &BoxFacts,
        opts: &SuperviseOpts,
        allow: &[String],
    ) -> ApprovalCtx {
        ApprovalCtx::from(
            &opts.policy,
            &self.setup.approval_env,
            allow,
            facts,
            self.fx.policy_probe(),
        )
    }

    /// The worker's environment ([`WorkerEnv`]).
    pub(crate) fn worker_env(&mut self) -> Result<WorkerEnv, String> {
        let stable = self
            .setup
            .sid
            .as_deref()
            .and_then(|s| s.strip_prefix('@'))
            .filter(|s| s.starts_with("s-"))
            .map(str::to_string);
        let r = match stable.as_deref() {
            Some(sid) => self.ctl.call_at_host_of(sid, &["who"]),
            None => self.ctl.call(&["who"]),
        }
        .map_err(|e| format!("`who` failed: {e}"))?;
        if !r.ok() {
            return Err(format!("`who` refused: {}", r.stderr.trim()));
        }
        let (sid, group) = session_group(&r.stdout, stable.as_deref(), self.status_sid.as_deref())?;
        self.fx.policy_probe().worker_proc(group, &sid)
    }

    fn box_call(
        &mut self,
        p: &PromptV2,
        opts: &SuperviseOpts,
        home: Option<&Path>,
    ) -> Option<String> {
        if !box_call::box_may_prove(p) {
            return None;
        }
        let host = opts.idle_host.as_deref()?;
        let mut pending = box_call::pending_calls(&host.transcript_tail()?);
        for _ in 0..box_call::LAG_TRIES {
            if !box_call::lagging(p, &pending, &self.calls_spent) {
                break;
            }
            self.fx.sleep(self.setup.box_call_step);
            pending = box_call::pending_calls(&host.transcript_tail()?);
        }
        let call = box_call::drew(p, &pending, &self.calls_spent, home)?;
        self.box_proved = Some(call.id);
        Some(call.tool)
    }

    /// The box on the screen has left.
    pub(super) fn box_gone(&mut self) {
        self.box_first_seen = None;
        self.spend_proved();
    }

    /// A read with no box on it and the composer back (`composer`: its frame on the screen, which the autocompact offer replaces).
    pub(super) fn offer_gone(&mut self, composer: bool) {
        if composer {
            self.stepper = StepperTrack::default();
        }
    }

    fn spend_proved(&mut self) {
        if let Some(id) = self.box_proved.take() {
            self.calls_spent.push(id);
            let over = self.calls_spent.len().saturating_sub(SPENT_CALLS);
            self.calls_spent.drain(..over);
        }
    }

    /// The decision on the box on `screen`.
    pub(super) fn decide_box(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        allow: &[String],
    ) -> Result<Decision, Fail> {
        self.question_held = false;
        self.question.deciding = None;
        self.reset_fallback = None;
        self.decided_on = None;
        let limit_reset = opts.policy.limit_reset && !self.reset_passed_here();
        let picker = matches!(
            aterm_phase::login_screen(&screen.rows),
            Some(aterm_phase::LoginScreen::Picker(_))
        );
        if picker {
            let now = self.fx.mono();
            self.turn_end.take_up_login(
                aterm_phase::login_at_wall(&screen.rows).as_ref(),
                false,
                now,
            );
        }
        let typed = picker && self.turn_end.login_typed_at().is_some();
        let shared = typed
            .then(|| opts.idle_host.as_deref().and_then(|h| h.login_shared()))
            .flatten();
        let renewed = typed
            && opts.idle_host.as_deref().is_some_and(|h| {
                super::super::policy::login::renewal_followed(
                    h.login_superseded(),
                    &opts.policy,
                    h.can_restart(),
                )
            });
        let gate = super::super::policy::login::picker_gate(typed, shared.as_ref(), renewed);
        let login_carry = gate == PickerGate::Carried;
        let login_back = gate == PickerGate::Back;
        let login_renewed = gate == PickerGate::Renewed;
        let own_rule = |limit_reset: bool| {
            aterm_phase::read(None, &screen.rows, None)
                .prompt
                .is_some_and(|p| {
                    p.kind == PromptKind::Question
                        || p.kind == PromptKind::McpServer
                        || login_carry
                        || login_back
                        || login_renewed
                        || p.kind == PromptKind::AutocompactOffer
                            && opts.policy.autocompact_offer.is_some()
                        || p.kind == PromptKind::UsageLimit
                            && (opts.policy.limit_wait
                                || limit_reset
                                    && aterm_phase::reset_row(&p, &opts.policy.limit_reset_kinds)
                                        .is_some())
                })
        };
        let owners = || Decision::Escalate {
            reason: "approve = \"none\": every box is the owner's".to_string(),
        };
        if opts.policy.approve == Approve::None && !own_rule(limit_reset) {
            return Ok(owners());
        }
        if let Some(holder) = self.claim.watching_behind() {
            return Ok(Decision::Escalate {
                reason: format!("another supervisor ({holder}) answers this session"),
            });
        }
        self.program = self.foreground_program()?;
        let mut reading = aterm_phase::read(
            self.program.as_deref(),
            &screen.rows,
            Some(screen.cursor_col),
        );
        if reading.prompt.is_none() && aterm_phase::read(None, &screen.rows, None).prompt.is_some()
        {
            let settle = PROGRAM_SETTLE.as_millis().to_string();
            let r = self.call(&["await", "agent", "prompt", "timeout", &settle])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!(
                    "await agent failed: {}",
                    r.stderr.trim()
                )));
            }
            if r.ok() {
                self.program = self.foreground_program()?;
                reading = aterm_phase::read(
                    self.program.as_deref(),
                    &screen.rows,
                    Some(screen.cursor_col),
                );
            }
        }
        let joined = self.box_from_history(screen, &reading)?;
        let rows: &[String] = joined.as_ref().map_or(&screen.rows, |(rows, _)| rows);
        let cut = joined
            .as_ref()
            .map(|(_, whole)| std::mem::replace(&mut reading, whole.clone()));
        let git = reading.prompt.as_ref().is_some_and(may_run_git);
        let bash = reading
            .prompt
            .as_ref()
            .is_some_and(|p| p.kind == PromptKind::Bash);
        let asks = match reading.program {
            Program::Claude => reading
                .prompt
                .as_ref()
                .is_some_and(|p| !unvoiced_tools(p).is_empty()),
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        let fresh_cwd = reading.prompt.as_ref().is_some_and(needs_cwd)
            || asks
            || bash && matches!(self.setup.approval_env.worker, WorkerSource::Session);
        let mut facts = self.box_facts(fresh_cwd, Reads { git, bash, asks })?;
        facts.screen_cols = screen
            .cols
            .unwrap_or_else(|| super::super::policy::question::widest_row(&screen.rows));
        facts.screen_rows = screen.pane_rows.unwrap_or(screen.first + screen.rows.len());
        facts.login_carry = login_carry;
        facts.login_back = login_back;
        facts.login_renewed = login_renewed;
        facts.limit_retry = reading
            .prompt
            .as_ref()
            .is_some_and(|p| p.kind == PromptKind::UsageLimit)
            && self.limit_retry_due(rows, opts) == RetryDue::Due;
        if login_carry {
            facts.login_method = opts.idle_host.as_deref().and_then(|h| h.login_method());
        }
        let mut said_by_session = None;
        let mut fallback: Option<(QuestionPriority, String)> = None;
        let mut latchable: Option<(String, QuestionDialog)> = None;
        if let Some(prompt) = reading
            .prompt
            .as_ref()
            .filter(|p| p.kind == PromptKind::Question)
        {
            let policy = self.question_policy(opts)?;
            self.question_held = policy.answer.is_none();
            let drawn = box_identity(&screen.rows);
            if self
                .question
                .held_as
                .as_ref()
                .is_some_and(|was| Some(was) != drawn.as_ref())
            {
                self.question.theirs = true;
            }
            self.question.held_as = if self.question_held { drawn } else { None };
            let mut question = QuestionFacts {
                answer: policy.answer.is_some(),
                priority: QuestionPriority::default(),
                own: None,
                fell_back: false,
            };
            said_by_session = policy.session_reason();
            if let Some(word) = policy.answer {
                question.priority = word;
                self.question.deciding = Some(word);
                let this = prompt
                    .question_dialog
                    .as_ref()
                    .map(|d| (question_key(d, &screen.rows), d.focus()));
                let key = this.as_ref().map(|(k, _)| k.clone()).or_else(|| {
                    prompt.question_dialog.is_none().then(|| {
                        let entered = self.question.note_entered.clone().filter(|k| {
                            aterm_phase::muse::is_review(prompt)
                                && k.lines().next() == Some(prompt.title.as_str())
                        });
                        entered.unwrap_or_else(|| {
                            super::super::policy::question::roles_question_key(prompt)
                        })
                    })
                });
                question.own = key
                    .as_ref()
                    .and_then(|k| self.question.typed.get(k).copied());
                question.fell_back = key
                    .as_ref()
                    .is_some_and(|k| self.question.fell_back.contains(k));
                if let Some(latched) = self.question.mode.filter(|m| *m != word) {
                    return Ok(Decision::Escalate {
                        reason: format!(
                            "the question policy was {} at the supervisor's first key on this \
                             dialog and is {} now: a person answers it",
                            latched.word(),
                            word.word()
                        ),
                    });
                }
                if let (Some((_, focus)), Some(pending)) = (&this, &self.question.pending)
                    && pending.kind == PendingKind::Text
                    && pending.focus == *focus
                    && box_identity(&screen.rows).as_deref() == Some(pending.identity.as_str())
                {
                    return Ok(Decision::Escalate {
                        reason: "the dialog did not take the policy text: a text is never written \
                                 twice"
                            .to_string(),
                    });
                }
                if word.text().is_some() {
                    let _ = self.gen_fence_known()?;
                    let _ = self.send_fence_known()?;
                    if let Some(why) = text_unfenced(&self.caps, screen) {
                        question.priority = QuestionPriority::Recommended;
                        fallback = Some((word, why));
                    } else if let (Some((key, _)), Some(dialog)) =
                        (this, prompt.question_dialog.clone())
                    {
                        latchable = Some((key, dialog));
                    }
                }
            }
            facts.question = Some(question);
        }
        let mut ctx = self.approval_ctx(&facts, opts, allow);
        if asks
            && let Some(p) = reading.prompt.as_ref()
            && proof_would_lift(&reading, rows, &ctx)
        {
            facts.box_call = self.box_call(p, opts, ctx.home.as_deref());
            ctx = self.approval_ctx(&facts, opts, allow);
        }
        let decide_here = |ctx: &ApprovalCtx| {
            let decision = decide(&reading, rows, ctx);
            match &cut {
                Some(cut) => whole_box_decision(decision, rows, &screen.rows, || {
                    decide(cut, &screen.rows, ctx)
                }),
                None => decision,
            }
        };
        let decision = decide_here(&ctx);
        self.decided_on = Some(facts);
        let owners_box = opts.policy.approve == Approve::None && !own_rule(false);
        if matches!(&decision, Decision::Approve { rule_id, .. } if *rule_id == RULE_LIMIT_RESET) {
            self.reset_fallback = Some(if owners_box {
                owners()
            } else {
                ctx.limit_reset = false;
                decide_here(&ctx)
            });
        } else if owners_box {
            return Ok(owners());
        }
        if let (
            Some((key, dialog)),
            Decision::Approve {
                choice: Choice::Answer(answer),
                ..
            },
        ) = (latchable, &decision)
            && super::super::policy::question::falls_back(&dialog, answer)
        {
            self.question.fell_back.insert(key);
        }
        let decision = if screen.human_seq.is_none()
            && matches!(
                &decision,
                Decision::Approve { rule_id, .. }
                    if *rule_id == RULE_AUTOCOMPACT_OFFER
                        || *rule_id == super::super::policy::login::RULE_LOGIN_METHOD
            ) {
            Decision::Escalate {
                reason: "the box's read carries no person count to fence its keys".to_string(),
            }
        } else {
            decision
        };
        let decision = self.stepper_check(&reading, rows, opts.policy.autocompact_offer, decision);
        let decision = if is_muse(&reading.program)
            && screen.human_seq.is_none()
            && matches!(
                &decision,
                Decision::Approve {
                    choice: Choice::Focus { .. },
                    ..
                }
            ) {
            Decision::Escalate {
                reason: "the Muse read carries no person count to fence its focus keys".to_string(),
            }
        } else {
            decision
        };
        Ok(match decision {
            Decision::Escalate { .. } if said_by_session.is_some() => Decision::Escalate {
                reason: said_by_session.unwrap_or_default(),
            },
            decision @ Decision::Approve { rule_id, .. }
                if is_question_rule(rule_id) && fallback.is_some() =>
            {
                let (word, why) = fallback.unwrap_or_default();
                fall_back_to(decision, word, &why)
            }
            decision => decision,
        })
    }

    /// A question dialog the loop's own keys answered, done (the worker works again) with no person taking part.
    pub(super) fn tell_answered(
        &mut self,
        done: &QuestionMemory,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let said = done.answered.join(" | ");
        let latched = done.mode.unwrap_or_default();
        let rule = latched.rule_id();
        let word = if done.by_text {
            latched.word()
        } else {
            QuestionPriority::Recommended.word()
        };
        let line = format!(
            "CHOSE seq={seq} rule={rule} policy={word} {}",
            clip(&one_line(&said))
        );
        review.say(&line).map_err(Fail::Hard)?;
        if self.tell_own {
            let _ = self.call(&["story", "chose", word]);
        }
        Ok(())
    }

    /// The question dialog in hand is done (the worker works again, read busy, or sits at its composer with no dialog up).
    pub(super) fn question_done(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        let done = std::mem::take(&mut self.question);
        let held_to_end = std::mem::take(&mut self.question_held);
        if !done.answered.is_empty() && !done.theirs && !held_to_end {
            self.tell_answered(&done, seq, review)?;
        }
        Ok(())
    }

    fn asked_again(&self, screen: &Screen, p: &aterm_phase::PromptV2) -> bool {
        let Some(dialog) = p.question_dialog.as_ref() else {
            return false;
        };
        let pristine = match &dialog.form {
            QuestionForm::Single { free_text, .. } | QuestionForm::Multi { free_text, .. } => {
                free_text.pristine
            }
            _ => false,
        };
        if !pristine || screen.human_seq.is_none() {
            return false;
        }
        let key = question_key(dialog, &screen.rows);
        self.question.entered.contains(&key)
            && self.question.typed_person.get(&key).copied() == screen.human_seq
    }

    /// Whether this session's question dialog is answered by some priority.
    pub(super) fn question_policy_answers(&mut self, opts: &SuperviseOpts) -> Result<bool, Fail> {
        Ok(self.question_policy(opts)?.answer.is_some())
    }

    /// The one resolver of how this session's questions are answered (its dialogs, and the priority words of those it asks in prose).
    pub(super) fn question_policy(&mut self, opts: &SuperviseOpts) -> Result<QuestionPolicy, Fail> {
        let global = QuestionPolicy::global(opts);
        let r = self.call(&["meta"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            self.autopilot.refused();
            return Ok(global);
        }
        self.note_meta(&r.stdout);
        Ok(match meta_word(&r.stdout, "questions") {
            None => global,
            Some(word) => session_word(&word),
        })
    }

    /// What a bare `meta` reply says of the session that the loop keeps.
    pub(super) fn note_meta(&mut self, meta: &str) {
        self.note_cwd(meta);
        self.autopilot.note(meta);
    }

    /// The session's Autopilot as the loop last read it.
    pub(super) fn autopilot(&self, opts: &SuperviseOpts) -> Autopilot {
        opts.policy.autopilot_for(self.autopilot.word)
    }

    /// [`Self::autopilot`] for a decision about to act.
    pub(super) fn autopilot_read(&mut self, opts: &SuperviseOpts) -> Result<Autopilot, Fail> {
        if self.autopilot.worth_a_read() && !self.autopilot.fresh {
            self.read_autopilot_word()?;
        }
        Ok(self.autopilot(opts))
    }

    /// A busy-read action is a new decision too.
    pub(super) fn autopilot_allows_busy(&mut self, opts: &SuperviseOpts) -> Result<bool, Fail> {
        self.autopilot.begin();
        let known =
            !self.autopilot.worth_a_read() || self.read_autopilot_word()? || !self.autopilot.kept;
        Ok(known && self.autopilot(opts) != Autopilot::Off)
    }

    fn read_autopilot_word(&mut self) -> Result<bool, Fail> {
        let r = self.call(&["meta"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
        }
        if r.ok() {
            self.note_meta(&r.stdout);
        } else {
            self.autopilot.refused();
        }
        Ok(r.ok())
    }

    /// One row of the approval ledger.
    pub(super) fn ledger_row(
        &mut self,
        rule_id: &str,
        outcome: approvals::Outcome,
        command: &str,
        reason: &str,
        box_seq: u64,
    ) {
        let policy_act = outcome == approvals::Outcome::Typed && !approvals::is_fact_rule(rule_id);
        if policy_act {
            self.left_keys = std::mem::take(&mut self.left_keys)
                .iter()
                .filter_map(|k| approvals::left_after_typed(k, rule_id, command))
                .collect();
        }
        let (count, captured_boot) = if policy_act {
            self.fx.asleep_for_ledger()
        } else {
            (None, None)
        };
        let entry = approvals::Row {
            rule_id,
            outcome,
            command,
            reason,
            box_seq,
        };
        let named_track = self.turn_end.has_limit_track();
        let scoped_wait = outcome == approvals::Outcome::Waiting
            && matches!(
                rule_id,
                approvals::RULE_WALL_WAIT | approvals::RULE_POINT | approvals::RULE_TRY_ANSWERED
            );
        let conversation = if policy_act && named_track {
            Some(
                self.wall_verdict
                    .as_ref()
                    .filter(|v| v.seq == box_seq)
                    .and_then(|v| v.conversation.clone())
                    .unwrap_or(super::super::policy::turn_end::Conversation::Unknown),
            )
        } else if scoped_wait {
            self.wall_memory
                .as_ref()
                .map(|m| m.conversation.clone())
                .or_else(|| self.turn_end.limit_conversation().cloned())
        } else {
            None
        };
        let program = (scoped_wait && rule_id == approvals::RULE_WALL_WAIT)
            .then(|| self.limit_met_under())
            .flatten();
        let at_ms = self.fx.stamp_ms();
        let row = match conversation {
            Some(c) => self.ledger.write_scoped_counted(
                at_ms,
                &entry,
                count.as_ref(),
                captured_boot.as_deref(),
                &c,
                program,
                &mut std::io::stderr(),
            ),
            None => self.ledger.write_counted_with_boot_text(
                at_ms,
                &entry,
                count.as_ref(),
                captured_boot.as_deref(),
                &mut std::io::stderr(),
            ),
        };
        if let Some(row) = row {
            self.fx
                .emit(super::super::tape::doors::LEDGER, || row.into_bytes());
        }
        if policy_act {
            self.wall_closed_by_act();
            self.turn_end.ledgered(rule_id, at_ms);
        }
        if let Some(host) = &self.stall_host
            && let Some(act) = crate::harness::acts::sort(rule_id, outcome, command).act
        {
            host.act_taken(act);
        }
        if policy_act {
            self.tell_hold();
        }
        if let Some(mark) = crate::harness::acts::log_mark(rule_id, outcome, command) {
            self.act_marks.queue(mark);
        }
        if let Some(words) = crate::harness::acts::loop_label(rule_id, outcome) {
            self.act_marks
                .queue(crate::harness::acts::label_mark(words));
        }
    }

    fn stepper_check(
        &mut self,
        reading: &aterm_phase::Reading,
        rows: &[String],
        target: Option<u32>,
        decision: Decision,
    ) -> Decision {
        let track = &mut self.stepper;
        track.deciding = None;
        let Some((prompt, offer)) = reading
            .prompt
            .as_ref()
            .and_then(|p| p.autocompact_offer().map(|o| (p, o)))
        else {
            *track = StepperTrack::default();
            return decision;
        };
        let title = aterm_phase::prompt::title_row(prompt, rows).unwrap_or_default();
        let this = format!("{}\n{}", title.trim(), prompt.options[offer.keep].label);
        if track.offer.as_deref() != Some(this.as_str()) {
            *track = StepperTrack {
                offer: Some(this),
                ..StepperTrack::default()
            };
        }
        let Decision::Approve {
            choice: Choice::FocusKey { key, .. },
            rule_id,
            ..
        } = &decision
        else {
            return decision;
        };
        if *rule_id != RULE_AUTOCOMPACT_OFFER {
            return decision;
        }
        let key = *key;
        let ended = match track.last {
            Some((last, _)) if last != key => Some("the stepper turned back"),
            Some((_, at)) if at == offer.tokens => Some("the stepper did not move past it"),
            _ if track.keys >= MAX_STEPPER_KEYS => Some("the stepper never reached it"),
            _ => None,
        };
        if let Some(why) = ended {
            let target = target.map_or_else(|| "?".to_string(), |t| format!("{t}k"));
            return Decision::Escalate {
                reason: format!(
                    "an autocompact offer, and autocompact_offer = {target} is not among the \
                     sizes this offer steps through ({why} at {})",
                    aterm_phase::autocompact::size_words(offer.tokens)
                ),
            };
        }
        track.deciding = Some((key, offer.tokens));
        decision
    }

    const fn stepper_pressed(&mut self) {
        if let Some(pressed) = self.stepper.deciding.take() {
            self.stepper.last = Some(pressed);
            self.stepper.keys += 1;
        }
    }

    fn box_young(&mut self, turn: &Turn, kind: PromptKind) -> Option<Duration> {
        let key = self.review_key(turn);
        let first = match &self.box_first_seen {
            Some((k, at)) if *k == key => *at,
            _ => {
                let now = self.fx.mono();
                self.box_first_seen = Some((key, now));
                self.reset_passed = None;
                self.reset_again = 0;
                self.spend_proved();
                now
            }
        };
        let settle = if kind == PromptKind::AutocompactOffer {
            self.setup.box_settle.max(self.setup.autocompact_settle)
        } else {
            self.setup.box_settle
        };
        let wait = settle.saturating_sub(self.fx.since(first));
        (!wait.is_zero()).then_some(wait)
    }

    /// Whether a person holds the act this look was about to take (`act`, for the journal).
    pub(super) fn held_for_person(
        &mut self,
        seq: u64,
        act: &str,
        fresh: bool,
        clock: DenyClock,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if fresh {
            self.program = self.foreground_program()?;
        }
        let Some(grace_ends) = self.hands_off_until(opts) else {
            return Ok(false);
        };
        let who = if self.driver {
            "another driver's hand is on the session"
        } else {
            "a person is typing"
        };
        let (until, why) = match (clock, self.touched()) {
            (DenyClock::None, _) => (grace_ends, String::new()),
            (DenyClock::Unseen, Some(by)) => (grace_ends, format!("; {by}: held as ever")),
            (DenyClock::At { deny, .. }, Some(by)) => (
                grace_ends,
                format!(
                    "; {by}: held as ever, and the vendor denies it at {}",
                    self.utc_at(deny)
                ),
            ),
            (DenyClock::Unseen, None) => (
                grace_ends.min(self.fx.mono() + TICK_LOOK),
                "; a countdown on it is read again to see it tick".to_string(),
            ),
            (DenyClock::At { deny }, None) => {
                let now = self.fx.mono();
                let cap = deny.checked_sub(DENY_MARGIN).unwrap_or(now);
                if now >= cap {
                    if let Some(c) = self.countdown.as_mut().filter(|c| !c.ended_said) {
                        c.ended_said = true;
                        review.note(&format!(
                            "HOLD-ENDED seq={seq} {who}, but nobody has touched the box and the \
                             vendor denies it at {}: the policy's answer goes now ({act})",
                            self.utc_at(deny)
                        ));
                    }
                    return Ok(false);
                }
                (
                    grace_ends.min(cap),
                    format!("; the vendor denies it at {}", self.utc_at(deny)),
                )
            }
        };
        review.note(&format!(
            "HELD seq={seq} {who}: hands off until {} ({act}{why})",
            self.utc_at(until)
        ));
        let until = self
            .setup
            .person_hold_wait
            .map_or(until, |wait| until.min(self.fx.mono() + wait));
        self.set_due(until, "a person is typing");
        self.held = true;
        Ok(true)
    }

    pub(super) fn utc_at(&self, at: Instant) -> String {
        utc_stamp(
            u64::try_from(self.now_unix()).unwrap_or(0)
                + at.saturating_duration_since(self.fx.mono()).as_secs(),
        )
    }

    /// When Claude Code auto-denies the box on `rows`, once its countdown is seen ticking.
    pub(super) fn deny_at(&self, rows: &[String]) -> Option<Instant> {
        #[cfg(test)]
        if self.deny_cap_off {
            return None;
        }
        let identity = box_identity(rows)?;
        self.deny_of(self.countdown_of_box(&identity)?)
    }

    fn deny_of(&self, c: &Countdown) -> Option<Instant> {
        let deny = c.deny()?;
        #[cfg(test)]
        if let Some(read) = self.read_clock_now() {
            let now = self.fx.mono();
            return Some(match deny.checked_duration_since(read) {
                Some(ahead) => now + ahead,
                None => now.checked_sub(read.duration_since(deny)).unwrap_or(now),
            });
        }
        Some(deny)
    }

    fn countdown_of_box(&self, identity: &str) -> Option<&Countdown> {
        self.countdown.as_ref().filter(|c| c.identity == identity)
    }

    /// Keeps the countdown on `rows` (asked for at `asked`); a read with none forgets it.
    pub(super) fn keep_countdown(&mut self, rows: &[String], asked: Instant) {
        let Some(read) = countdown_of(rows, asked) else {
            self.countdown = None;
            return;
        };
        self.countdown = Some(read.after(self.countdown.as_ref()));
    }

    /// Keeps the box episode `screen` reads, its baseline `before` (the prior read's person count);
    /// a read with no box ends it.
    pub(super) fn keep_episode(&mut self, screen: &Screen, before: Option<Option<u64>>) {
        #[cfg(test)]
        {
            let shown = box_identity(&screen.rows);
            if self.episode_per_box && self.last_box.is_some() && shown != self.last_box {
                self.episode = None;
            }
            self.last_box = shown;
        }
        if aterm_phase::parse_prompt_v2(&screen.rows).is_none() {
            self.episode = None;
            return;
        }
        let episode = self.episode.get_or_insert_with(|| Episode {
            baseline: before.flatten(),
            by: None,
        });
        episode.read(screen);
    }

    pub(super) fn episode_driven(&mut self) {
        if let Some(e) = self.episode.as_mut() {
            e.touch(BY_DRIVER);
        }
    }

    pub(super) fn touched(&self) -> Option<&'static str> {
        #[cfg(test)]
        if self.touch_off {
            return None;
        }
        self.episode.as_ref().and_then(|e| e.by)
    }

    /// The one person count every key of an answer names: an untouched episode's baseline, else
    /// `seen`'s.
    pub(super) const fn person_fence(&self, seen: &Screen) -> Option<u64> {
        match &self.episode {
            Some(e) if e.by.is_none() => e.baseline,
            _ => seen.human_seq,
        }
    }

    pub(super) fn deny_clock(&self, screen: &Screen) -> DenyClock {
        #[cfg(test)]
        if self.deny_cap_off {
            return DenyClock::None;
        }
        let Some(identity) = box_identity(&screen.rows) else {
            return DenyClock::None;
        };
        let Some(c) = self.countdown_of_box(&identity) else {
            return DenyClock::None;
        };
        match self.deny_of(c) {
            Some(deny) => DenyClock::At { deny },
            None => DenyClock::Unseen,
        }
    }

    /// Whether nobody touched the box's episode and its ticking countdown is within [`DENY_MARGIN`]
    /// of the deny.
    pub(super) fn hold_over(&self, screen: &Screen) -> bool {
        self.touched().is_none()
            && matches!(
                self.deny_clock(screen),
                DenyClock::At { deny, .. } if self.fx.mono() + DENY_MARGIN >= deny
            )
    }

    /// The quiet a retry waits for, and how long: [`SETTLE_MS`] within [`DENY_MARGIN`] of a ticking
    /// deny, where two seconds of quiet never come.
    pub(super) fn still_wait(&self, screen: &Screen) -> (&'static str, Duration) {
        let DenyClock::At { deny, .. } = self.deny_clock(screen) else {
            return (IDLE_MS, WAIT_STEP);
        };
        let now = self.fx.mono();
        match deny.checked_sub(DENY_MARGIN).filter(|end| *end > now) {
            Some(end) => (IDLE_MS, WAIT_STEP.min(end - now)),
            None => (
                SETTLE_MS,
                WAIT_STEP.min(deny.saturating_duration_since(now)),
            ),
        }
    }

    fn box_grace(&self, opts: &SuperviseOpts, screen: &Screen) -> Duration {
        if self.hold_over(screen) {
            Duration::ZERO
        } else {
            Duration::from_secs(u64::from(opts.policy.human_grace_s))
        }
    }

    /// Until when the loop keeps its hands off the session.
    pub(super) fn hands_off_until(&self, opts: &SuperviseOpts) -> Option<Instant> {
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        self.person
            .and_then(|typed| typed.checked_add(grace))
            .filter(|until| *until > self.fx.mono())
    }

    fn carry_unheld(&self) -> bool {
        self.turn_end
            .login_typed_at()
            .is_some_and(|typed| !self.driver && self.person.is_none_or(|key| key <= typed))
    }

    /// THE LAUNCH.
    pub(super) fn launch_call(
        &mut self,
        screen: &Screen,
        p: &PromptV2,
        decision: &Decision,
        opts: &SuperviseOpts,
    ) -> Result<LaunchCall, Fail> {
        let trusts = matches!(
            decision,
            Decision::Approve {
                rule_id,
                choice: Choice::Focus { .. },
                ..
            } if matches!(*rule_id, RULE_TRUST_ANY | RULE_TRUST_DIALOG)
        );
        if !trusts || p.kind != PromptKind::Trust || self.driver {
            return Ok(LaunchCall::Held);
        }
        let (Some(n), Some(witness)) = (screen.human_seq, screen.launch.as_ref()) else {
            return Ok(LaunchCall::Held);
        };
        let agent = aterm_phase::program_of(&witness.word);
        let status = &self.launch_status;
        let named = status.program.as_deref().and_then(aterm_phase::program_of);
        let box_key = box_identity(&screen.rows).unwrap_or_else(|| p.command.clone());
        if witness.seq != n
            || agent.is_none()
            || agent != Some(self.reader(&screen.rows).program())
            || named != agent
            || status.person_seq != Some(n)
            || self
                .launch_used
                .as_ref()
                .is_some_and(|(id, used)| *id == witness.id && *used != box_key)
        {
            return Ok(LaunchCall::Held);
        }
        match status.input.as_deref() {
            Some("clear") => {}
            Some("pending" | "typeahead") => return Ok(LaunchCall::Again),
            _ => return Ok(LaunchCall::Held),
        }
        if self.autopilot_read(opts)? == Autopilot::Off {
            return Ok(LaunchCall::Held);
        }
        self.launch_used = Some((witness.id, box_key));
        Ok(LaunchCall::Answer(witness.clone()))
    }

    /// One turn under the approval policy.
    pub(super) fn auto_read(
        &mut self,
        turn: Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        approved: &mut Vec<Approved>,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let box_on = (turn.phase == Phase::Prompt)
            .then(|| self.reader(&turn.screen.rows).prompt(&turn.screen.rows))
            .flatten();
        let Some(p) = box_on else {
            return Ok(Step::Review(turn));
        };
        let seq = turn.screen.seq;
        if let Some(wait) = self.box_young(&turn, p.kind) {
            self.wait(&["seq", &seq.to_string()], wait)?;
            return Ok(Step::Again { settle: false });
        }
        if self.asked_again(&turn.screen, &p) {
            self.question_done(seq, review)?;
        }
        if self.limit_retry_hold(&turn, &p, opts, review)? {
            return Ok(Step::Review(turn));
        }
        self.autopilot.begin();
        let decision = self.decide_box(&turn.screen, opts, allow)?;
        #[cfg(test)]
        if PANIC_AT_DECIDE.with(std::cell::Cell::get) {
            panic!("a planted panic in the policy, at the decision on seq={seq}");
        }
        self.fx.decide(
            if p.kind == PromptKind::Question {
                super::super::tape::doors::DECIDE_QUESTION
            } else {
                super::super::tape::doors::DECIDE_APPROVAL
            },
            &turn.screen,
            decision.rule_id(),
            |_| format!("{decision:?}"),
            self.decided_on.as_ref(),
        );
        if self.stall_in_hand(review) {
            return Ok(Step::Again { settle: false });
        }
        if review.unattended() && self.autopilot_read(opts)? == Autopilot::Off {
            self.park_while_off(seq, deadline, opts, review)?;
            return Ok(Step::Again { settle: false });
        }
        let carried_pick = matches!(
            &decision,
            Decision::Approve { rule_id, .. }
                if *rule_id == super::super::policy::login::RULE_LOGIN_METHOD
        ) && self.carry_unheld();
        let answers = matches!(
            decision,
            Decision::Approve { .. } | Decision::Decline { .. }
        ) && !carried_pick;
        let launch = if answers && self.hands_off_until(opts).is_some() {
            self.launch_call(&turn.screen, &p, &decision, opts)?
        } else {
            LaunchCall::Held
        };
        match launch {
            LaunchCall::Answer(w) => review.note(&format!(
                "LAUNCH seq={seq} id={} word={} the person's last gesture was the Enter that \
                 started it, none since (count {}): answered, not held",
                w.id, w.word, w.seq
            )),
            LaunchCall::Again => {
                self.wait(&["seq", &seq.to_string()], PROGRAM_SETTLE)?;
                return Ok(Step::Again { settle: false });
            }
            LaunchCall::Held if answers => {
                if p.kind == PromptKind::Question && self.hands_off_until(opts).is_some() {
                    self.question.person_gave_input();
                }
                let clock = self.deny_clock(&turn.screen);
                if self.held_for_person(seq, "the box's answer", false, clock, opts, review)? {
                    return Ok(Step::Review(turn));
                }
            }
            LaunchCall::Held => {}
        }
        self.auto_act(turn, p, decision, opts, approved, deadline, review)
    }

    #[allow(clippy::too_many_arguments)]
    fn auto_act(
        &mut self,
        turn: Turn,
        p: aterm_phase::PromptV2,
        decision: Decision,
        opts: &SuperviseOpts,
        approved: &mut Vec<Approved>,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let seq = turn.screen.seq;
        let (rule_id, act, subject, unproven) = match decision {
            Decision::Escalate { reason } => {
                self.ledger_row("-", approvals::Outcome::Escalated, &p.command, &reason, seq);
                if p.kind == PromptKind::Question {
                    note_dialog(review, seq, "-", &turn.screen.rows);
                    if !self.question_held {
                        self.question.theirs = true;
                    }
                }
                self.box_reason = Some((self.review_key(&turn), reason));
                return Ok(Step::Review(turn));
            }
            Decision::Approve {
                rule_id,
                guard,
                choice,
                subject,
                unproven,
            } => (rule_id, Act::Approve { guard, choice }, subject, unproven),
            Decision::Decline {
                rule_id,
                refusal,
                text,
                subject,
            } => (rule_id, Act::Decline { refusal, text }, subject, None),
        };
        let what = shown_subject(&subject, &p.command);
        let (question, toggle) = match &act {
            Act::Approve {
                choice: Choice::Answer(answer),
                ..
            } => {
                let Some(dialog) =
                    aterm_phase::parse_prompt_v2(&turn.screen.rows).and_then(|v| v.question_dialog)
                else {
                    let why = "the question dialog was not read again from the same rows";
                    self.question.theirs = true;
                    self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, why, seq);
                    self.box_reason = Some((self.review_key(&turn), why.to_string()));
                    return Ok(Step::Review(turn));
                };
                let identity = box_identity(&turn.screen.rows).unwrap_or_else(|| what.clone());
                let answering = answer
                    .enter_target()
                    .is_some_and(|target| answers(&dialog, target));
                let this_question = question_key(&dialog, &turn.screen.rows);
                let grace = self.box_grace(opts, &turn.screen);
                let text_word = self.question.deciding.is_some_and(|w| w.text().is_some());
                let reask = (answering || text_word).then_some(this_question.as_str());
                match self.question_hold(
                    &turn, &dialog, &identity, reask, approved, grace, rule_id, review,
                )? {
                    None => {}
                    Some(Hold::Again) => return Ok(Step::Again { settle: true }),
                    Some(Hold::BackOff(why)) => {
                        return self.box_back_off(&turn, &what, &why, review);
                    }
                    Some(Hold::HandOver(why)) => {
                        self.question.theirs = true;
                        self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, &why, seq);
                        note_dialog(review, seq, rule_id, &turn.screen.rows);
                        self.box_reason = Some((self.review_key(&turn), why));
                        return Ok(Step::Review(turn));
                    }
                }
                let toggle = match (&dialog.form, answer.enter_target()) {
                    (QuestionForm::Multi { .. }, Some(AnswerTarget::Option(n))) => {
                        Some((this_question.clone(), n))
                    }
                    _ => None,
                };
                (answering.then_some(this_question), toggle)
            }
            _ => (None, None),
        };
        let answer_named = question.is_some();
        let declined = match &act {
            Act::Decline { .. } => {
                let key = super::press::decline_box(&p, &turn.screen.rows);
                if let Some(pause) = self.decline_again(&key, approved) {
                    review.note(&format!(
                        "WAITING seq={seq} rule={rule_id} the same box came back after its \
                         declines since the last review point (the worker has not acted on its \
                         reason yet): declined again in {} ms",
                        pause.as_millis()
                    ));
                    self.wait(&["seq", &seq.to_string()], pause)?;
                    return Ok(Step::Again { settle: true });
                }
                Some(key)
            }
            Act::Approve { .. } => None,
        };
        let repeats = approved
            .iter()
            .filter(|a| a.declined.is_none() && a.command == p.command)
            .count();
        if opts.policy.approve != Approve::All
            && !is_question_rule(rule_id)
            && declined.is_none()
            && repeats >= MAX_APPROVALS_OF_ONE_COMMAND
        {
            let why = "the same prompt came back after two approvals";
            self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, why, seq);
            self.box_reason = Some((self.review_key(&turn), why.to_string()));
            return Ok(Step::Review(turn));
        }
        let reset = rule_id == RULE_LIMIT_RESET;
        if reset {
            match self.reset_take(&turn, opts, review)? {
                super::reset_loop::ResetAsked::Take => {}
                super::reset_loop::ResetAsked::Again => return Ok(Step::Again { settle: false }),
                super::reset_loop::ResetAsked::Pass => {
                    let plain = self
                        .reset_fallback
                        .take()
                        .unwrap_or_else(|| Decision::Escalate {
                            reason: "a usage-limit dialog whose reset was passed over".to_string(),
                        });
                    return self.auto_act(turn, p, plain, opts, approved, deadline, review);
                }
            }
        }
        let unproven = unproven.map(|u| format!("unproven: {u}"));
        let over = match (&act, &self.reset_taken) {
            (Act::Decline { text, .. }, _) => text.clone(),
            (Act::Approve { .. }, Some((go, _))) if reset => {
                format!("taken: wall until {}", go.until)
            }
            (Act::Approve { .. }, _) => unproven.clone().unwrap_or_default(),
        };
        let grace = self.box_grace(opts, &turn.screen);
        let past_hold = self.hold_over(&turn.screen) && self.hands_off_until(opts).is_some();
        let person = past_hold.then(|| self.person_fence(&turn.screen)).flatten();
        let pressing = match &act {
            Act::Approve {
                guard,
                choice: Choice::Digit(n),
            } => self.press_one_guarded(
                guard,
                &n.to_string(),
                &turn.screen,
                super::super::policy::KeyAct::Approve,
            )?,
            Act::Approve {
                guard,
                choice: Choice::Focus { steps, label },
            } => {
                let person = turn
                    .screen
                    .human_seq
                    .filter(|_| {
                        reset
                            || p.kind == PromptKind::Trust
                            || p.kind == PromptKind::AutocompactOffer
                            || rule_id == super::super::policy::login::RULE_LOGIN_METHOD
                            || is_muse(&self.reader(&turn.screen.rows).program())
                    })
                    .or(person);
                self.press_focused(
                    &p,
                    *steps,
                    label,
                    "enter",
                    guard,
                    &turn.screen,
                    person,
                    Some(super::super::policy::KeyAct::Approve),
                )?
            }
            Act::Approve {
                guard,
                choice: Choice::FocusKey { steps, label, key },
            } => {
                let person = turn
                    .screen
                    .human_seq
                    .filter(|_| p.kind == PromptKind::AutocompactOffer)
                    .or(person);
                self.press_focused(&p, *steps, label, key, guard, &turn.screen, person, None)?
            }
            Act::Approve {
                choice: Choice::Answer(answer),
                ..
            } => self.press_question(answer, &turn.screen, grace)?,
            Act::Approve {
                choice: Choice::Note { text },
                ..
            } => self.press_codex_note(*text, &turn.screen, grace)?,
            Act::Decline { refusal, text } => {
                self.press_decline(*refusal, text, &turn.screen, grace)?
            }
        };
        let read_back = self.read_back.take();
        let this = Approved {
            command: if reset {
                what.clone()
            } else {
                p.command.clone()
            },
            question,
            declined,
        };
        let press = match pressing {
            Pressing::Done(press) => press,
            Pressing::Unconfirmed { why, retry } => {
                if retry && self.retries_presses(opts, review, rule_id) {
                    return self.press_missed(&turn, &what, &why, opts, review);
                }
                if reset {
                    self.reset_pass(
                        seq,
                        &format!("the row was not pressed ({why})"),
                        opts,
                        review,
                    );
                    return Ok(Step::Again { settle: false });
                }
                let why = if this.declined.is_some() {
                    format!("the decline could not be carried out: {why}")
                } else {
                    why
                };
                self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, &why, seq);
                if is_question_rule(rule_id) {
                    self.question.theirs = true;
                    note_dialog(review, seq, rule_id, &turn.screen.rows);
                }
                self.box_reason = Some((self.review_key(&turn), why));
                return Ok(Step::Review(turn));
            }
            Pressing::Halted { why } => {
                self.ledger_row(rule_id, approvals::Outcome::Deferred, &what, &why, seq);
                self.park_on_hold(&why, seq, review)?;
                return Ok(Step::Again { settle: false });
            }
            Pressing::Refused { why } => {
                self.ledger_row(rule_id, approvals::Outcome::Deferred, &what, &why, seq);
                let anchor = match &act {
                    Act::Approve { guard, .. } => Some(guard.as_str()),
                    Act::Decline { .. } => None,
                };
                self.back_off(&why, seq, anchor, review)?;
                return Ok(Step::Again { settle: false });
            }
            Pressing::Lost { why } => {
                if reset && self.focus_key_tried {
                    self.reset_row_lost(&what, seq);
                }
                return Err(Fail::Lost(why));
            }
        };
        self.refusal_pause = REFUSAL_PAUSE;
        match press {
            Press::Pressed { seq: at } if is_question_rule(rule_id) => {
                self.changed_streak = 0;
                let submit = matches!(
                    &act,
                    Act::Approve {
                        choice: Choice::Answer(answer),
                        ..
                    } if answer.enter_target() == Some(AnswerTarget::ReviewSubmit)
                );
                if answer_named && !submit && self.question.answered.last() != Some(&what) {
                    self.question.answered.push(what.clone());
                }
                if let Some((told, note)) = told_by_role(&act, &p, &what)
                    && self.question.answered.last() != Some(&told)
                {
                    self.question.answered.push(told);
                    self.question.by_text |= note;
                }
                if answer_named
                    && !submit
                    && let Some(q) = this
                        .question
                        .as_ref()
                        .filter(|q| self.question.typed.contains_key(*q))
                {
                    self.question.by_text = true;
                    self.question.entered.insert(q.clone());
                }
                if let Some((q, n)) = toggle {
                    self.question.toggled.entry(q).or_default().insert(n);
                }
                approved.push(this);
                self.ledger_row(rule_id, approvals::Outcome::Approved, &what, &over, seq);
                review.approved(at, &what)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                note_dialog(review, seq, rule_id, &turn.screen.rows);
                self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
                Ok(Step::Again { settle: true })
            }
            Press::Pressed { seq: at } if reset => {
                self.changed_streak = 0;
                self.press_retry = None;
                self.reset_row_taken();
                approved.push(this);
                self.answered(&act, rule_id, &what, &over, (at, seq), review)?;
                self.box_gone();
                self.wait(&["seq", &at.to_string()], self.setup.reset_waits.step)?;
                self.reset_run(None, deadline, opts, review)?;
                Ok(Step::Again { settle: true })
            }
            Press::Typed { seq: at } => {
                self.changed_streak = 0;
                let word = match &act {
                    Act::Approve {
                        choice: Choice::Answer(answer),
                        ..
                    } => answer.typed().unwrap_or_default(),
                    _ => QuestionPriority::default(),
                }
                .word();
                approved.push(this);
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Approved,
                    &what,
                    &format!(
                        "typed the {word} policy text into the free-text row: the worker decides"
                    ),
                    seq,
                );
                review.approved(at, &what)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                note_dialog(review, seq, rule_id, &turn.screen.rows);
                match read_back
                    .map(|(back, still)| (self.turn_of(back), still))
                    .filter(|(next, _)| next.phase == Phase::Prompt)
                {
                    Some((next, still)) => {
                        let since = self
                            .fx
                            .mono()
                            .checked_sub(still)
                            .unwrap_or_else(|| self.fx.mono());
                        self.box_first_seen = Some((self.review_key(&next), since));
                        Ok(Step::Decide(next.screen))
                    }
                    None => Ok(Step::Again { settle: true }),
                }
            }
            Press::Yielded { seq: at } => {
                if is_question_rule(rule_id) {
                    self.question.theirs = true;
                }
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "a person gave the session input while the answer was keyed: decided again \
                     after their quiet",
                    at,
                );
                Ok(Step::Again { settle: true })
            }
            Press::Unseen { seq: at } => {
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the dialog has not shown the key's effect yet",
                    at,
                );
                Ok(Step::Again { settle: true })
            }
            Press::Pressed { seq: at } => {
                self.changed_streak = 0;
                if matches!(
                    &act,
                    Act::Approve {
                        choice: Choice::FocusKey { .. },
                        ..
                    }
                ) {
                    self.stepper_pressed();
                }
                if rule_id == RULE_LIMIT_RETRY {
                    self.limit_retry_pressed();
                }
                approved.push(this);
                self.answered(&act, rule_id, &what, &over, (at, seq), review)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                let identity = box_identity(&turn.screen.rows).unwrap_or_default();
                let deny = self.deny_at(&turn.screen.rows);
                let screen = match self.box_left(at, &identity, deny, deadline)? {
                    Past::Moved => {
                        self.press_retry = None;
                        return Ok(Step::Again { settle: true });
                    }
                    Past::Still(Some(screen)) => screen,
                    Past::Still(None) => return Ok(Step::Review(turn)),
                };
                let why = "the box did not change after the press";
                let still = self.turn_of(screen);
                if self.retries_presses(opts, review, rule_id) {
                    return self.press_missed(&still, &what, why, opts, review);
                }
                Ok(Step::Review(still))
            }
            Press::Changed { seq: at } => {
                self.changed_streak += 1;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the screen moved past the judged read",
                    at,
                );
                if self.changed_streak >= MAX_CHANGED_PRESSES {
                    self.changed_streak = 0;
                    let why = format!(
                        "{MAX_CHANGED_PRESSES} fenced presses in a row did not land (the screen \
                         moved, or the key was not taken)"
                    );
                    if self.retries_presses(opts, review, rule_id) {
                        return self.press_missed(&turn, &what, &why, opts, review);
                    }
                    self.box_reason = Some((self.review_key(&turn), why));
                    return Ok(Step::Review(turn));
                }
                Ok(Step::Again {
                    settle: is_question_rule(rule_id),
                })
            }
            Press::Skipped { seq: at } => {
                self.changed_streak = 0;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the guarded row was not on the screen",
                    at,
                );
                if at == seq {
                    let why = "aterm could not find the box's row";
                    if self.retries_presses(opts, review, rule_id) {
                        return self.press_missed(&turn, &what, why, opts, review);
                    }
                    self.box_reason = Some((self.review_key(&turn), why.to_string()));
                    return Ok(Step::Review(turn));
                }
                Ok(Step::Again { settle: false })
            }
        }
    }
}

const PRESS_RETRY_MAX: Duration = Duration::from_secs(60);
/// How long a box whose presses keep missing is tried before the session is badged.
pub(super) const PRESS_BADGE_AFTER: Duration = Duration::from_secs(120);

/// Gemini CLI usage-limit dialog retry state ([`RULE_LIMIT_RETRY`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LimitRetry {
    program: Program,
    reset: Option<aterm_phase::gemini::LimitReset>,
    seen_unix: i64,
    at_unix: Option<i64>,
    zone_read: bool,
    tries: u32,
    looks: u32,
    last_unix: Option<i64>,
    resumed: bool,
    badge: Option<String>,
    model: Option<String>,
    away: bool,
    relaunch: Option<bool>,
}

/// Last observed Gemini CLI usage-limit dialog read ([`Session::note_limit_dialog`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LimitDialogRead {
    program: Program,
    reset: Option<aterm_phase::gemini::LimitReset>,
    seen_unix: i64,
    last_unix: i64,
}

#[cfg(test)]
impl LimitRetry {
    /// Returns the placed reset timestamp ([`Self::at_unix`]).
    pub(super) fn placed_unix(&self) -> Option<i64> {
        self.at_unix
    }
}

/// Gemini CLI usage-limit dialog answered `Stop`, or its agent relaunched, while the loop waited on
/// it ([`Session::limit_stopped`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoppedLimit {
    pub(super) wall: aterm_phase::Wall,
    pub(super) reset_unix: i64,
}

/// Initial back-off after a `Keep trying` when the dialog returns, doubled on each retry.
pub(super) const LIMIT_RETRY_BACKOFF_S: i64 = 120;

/// Maximum retry back-off ([`LimitRetry`]).
pub(super) const LIMIT_RETRY_BACKOFF_MAX_S: i64 = 1800;

/// Maximum wait before the first `Keep trying` or between looks in the machine's zone ([`LimitRetry`]).
pub(super) const LIMIT_RETRY_RELOOK_S: i64 = LIMIT_RETRY_BACKOFF_MAX_S;

/// Window after `Keep trying` during which a busy read counts as that try in flight ([`Session::limit_retry_busy`]).
pub(super) const LIMIT_RETRY_ANSWER_S: i64 = 300;

/// Window after the last usage-limit dialog read during which a busy frame may precede `Stop`'s error ([`Session::note_limit_dialog`]).
pub(super) const LIMIT_DIALOG_FRAME_S: i64 = 10;

const LIMIT_RETRY_AHEAD_MAX_S: i64 = 23 * 3600;

const LIMIT_RETRY_MINUTE_S: i64 = 60;

/// When Gemini CLI's usage-limit dialog is due its `Keep trying`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RetryDue {
    Not,
    Due,
    Hold {
        until_unix: i64,
        reset: Option<String>,
    },
}

/// A box whose press did not land, being tried again at full power.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PressRetry {
    key: String,
    tries: u32,
    since: Instant,
    badged: bool,
}

impl<C: Ctl> Session<'_, C> {
    fn retries_presses(&self, opts: &SuperviseOpts, review: &dyn Review, rule_id: &str) -> bool {
        review.unattended()
            && (opts.policy.approve == Approve::All
                || is_question_rule(rule_id)
                || rule_id == RULE_LIMIT_WAIT
                || rule_id == RULE_LIMIT_RETRY
                || rule_id == RULE_LIMIT_STOP
                || rule_id == RULE_LIMIT_RESET
                || rule_id == RULE_AUTOCOMPACT_OFFER
                || rule_id == super::super::policy::login::RULE_LOGIN_METHOD)
    }

    fn press_missed(
        &mut self,
        turn: &Turn,
        what: &str,
        why: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let why = format!("the press did not land ({why})");
        if self.reset_taken.is_some() {
            return self.reset_press_missed(turn, &why, opts, review);
        }
        self.box_back_off(turn, what, &why, review)
    }

    fn reset_press_missed(
        &mut self,
        turn: &Turn,
        why: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let key = box_identity(&turn.screen.rows).unwrap_or_default();
        let (now, seq) = (self.fx.mono(), turn.screen.seq);
        let (tries, since) = match self.press_retry.as_mut().filter(|r| r.key == key) {
            Some(r) => {
                r.tries = r.tries.saturating_add(1);
                (r.tries, r.since)
            }
            None => {
                self.press_retry = Some(PressRetry {
                    key,
                    tries: 1,
                    since: now,
                    badged: false,
                });
                (1, now)
            }
        };
        if now.duration_since(since) >= self.setup.press_badge_after {
            self.press_retry = None;
            self.reset_pass(seq, why, opts, review);
            return Ok(Step::Again { settle: false });
        }
        let pause = crate::supervise::ladder::doubling(REFUSAL_PAUSE, tries - 1, PRESS_RETRY_MAX);
        review.note(&format!(
            "WAITING seq={seq} rule={RULE_LIMIT_RESET} {why} (try {tries}): read and tried again \
             in {} ms",
            pause.as_millis()
        ));
        self.wait(&["seq", &seq.to_string()], pause)?;
        Ok(Step::Again { settle: false })
    }

    /// WHEN GEMINI CLI'S USAGE-LIMIT DIALOG ON `rows` IS DUE ITS `Keep trying` ([`RULE_LIMIT_RETRY`]).
    pub(super) fn limit_retry_due(&mut self, rows: &[String], opts: &SuperviseOpts) -> RetryDue {
        let program = self.reader(rows).program();
        if !opts.policy.limit_wait || !keeps_trying_its_limit(program) {
            self.limit_retry = None;
            return RetryDue::Not;
        }
        let dialog = aterm_phase::gemini::prompt(rows)
            .is_some_and(|p| p.kind == PromptKind::UsageLimit && !p.head_off_screen);
        if !dialog {
            self.limit_retry = None;
            return RetryDue::Not;
        }
        let reset = aterm_phase::gemini::limit_reset(rows);
        let now = self.now_unix();
        if self
            .limit_retry
            .as_ref()
            .is_none_or(|r| r.program != program || r.reset != reset)
        {
            let carried = self
                .limit_retry
                .take()
                .filter(|r| r.program == program && r.tries > 0);
            let mut retry = self.limit_retry_open(program, reset, now);
            if let Some(r) = carried {
                retry.tries = r.tries;
                retry.looks = r.looks;
                retry.last_unix = r.last_unix;
                retry.resumed = r.resumed;
                retry.badge = r.badge;
                retry.model = r.model;
            }
            self.limit_retry = Some(retry);
        }
        let Some(retry) = self.limit_retry.as_ref() else {
            return RetryDue::Not;
        };
        let Some(until_unix) = self.limit_retry_until(retry) else {
            return RetryDue::Not;
        };
        if now >= until_unix {
            RetryDue::Due
        } else {
            RetryDue::Hold {
                until_unix,
                reset: retry
                    .at_unix
                    .and(retry.reset.as_ref())
                    .map(ToString::to_string),
            }
        }
    }

    fn limit_retry_open(
        &mut self,
        program: Program,
        reset: Option<aterm_phase::gemini::LimitReset>,
        seen_unix: i64,
    ) -> LimitRetry {
        let spec = reset
            .as_ref()
            .and_then(|r| Some((r.offset_s(), limit::parse_reset(&r.clock)?)));
        let (at_unix, zone_read) = match spec {
            Some((offset, spec)) => {
                let zone = offset.map(i64::from);
                let offset = zone.unwrap_or_else(|| self.local_offset_s());
                let at = limit::reset_in_zone(&spec, seen_unix, LIMIT_RETRY_AHEAD_MAX_S, &|_| {
                    Some(offset)
                });
                (at, zone.is_some())
            }
            None => (None, false),
        };
        LimitRetry {
            program,
            reset,
            seen_unix,
            at_unix,
            zone_read,
            tries: 0,
            looks: 0,
            last_unix: None,
            resumed: false,
            badge: None,
            model: None,
            away: false,
            relaunch: None,
        }
    }

    /// Records or clears the usage-limit dialog read on `rows` ([`LimitDialogRead`]).
    pub(super) fn note_limit_dialog(&mut self, program: Program, rows: &[String], busy: bool) {
        let now = self.now_unix();
        let past_frame =
            |d: &LimitDialogRead| now.saturating_sub(d.last_unix) > LIMIT_DIALOG_FRAME_S;
        if !keeps_trying_its_limit(program)
            || (busy && self.limit_dialog_read.as_ref().is_some_and(past_frame))
        {
            self.limit_dialog_read = None;
            return;
        }
        let Some(p) = aterm_phase::gemini::prompt(rows) else {
            return;
        };
        if p.kind != PromptKind::UsageLimit {
            self.limit_dialog_read = None;
            return;
        }
        let reset = aterm_phase::gemini::limit_reset(rows);
        match self.limit_dialog_read.as_mut() {
            Some(d) if d.program == program && d.reset == reset => d.last_unix = now,
            _ => {
                self.limit_dialog_read = Some(LimitDialogRead {
                    program,
                    reset,
                    seen_unix: now,
                    last_unix: now,
                });
            }
        }
    }

    /// Opens a [`LimitRetry`] episode from [`Self::note_limit_dialog`] when `Stop`'s error is reached before any point decided the dialog.
    pub(super) fn limit_stopped_adopt(
        &mut self,
        reading: &aterm_phase::Reading,
        opts: &SuperviseOpts,
    ) {
        if self.limit_retry.is_some()
            || !opts.policy.limit_wait
            || !reading
                .wall
                .as_ref()
                .is_some_and(aterm_phase::gemini::stopped_error)
        {
            return;
        }
        let Some(read) = self.limit_dialog_read.take() else {
            return;
        };
        let retry = self.limit_retry_open(read.program, read.reset, read.seen_unix);
        self.limit_retry = Some(retry);
    }

    fn limit_retry_until(&self, retry: &LimitRetry) -> Option<i64> {
        let secs = |d: Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
        let until_unix = match retry.at_unix {
            Some(at_unix) => {
                let past_reset = self.limit_retry_past_reset(at_unix);
                let climbed = retry.tries.saturating_sub(retry.looks);
                let backed_off = match (retry.last_unix, climbed) {
                    (Some(last), 1..) => {
                        last.saturating_add(secs(crate::supervise::ladder::doubling(
                            Duration::from_secs(LIMIT_RETRY_BACKOFF_S.unsigned_abs()),
                            climbed - 1,
                            Duration::from_secs(LIMIT_RETRY_BACKOFF_MAX_S.unsigned_abs()),
                        )))
                    }
                    _ => i64::MIN,
                };
                let look = (retry.tries == 0 || !retry.zone_read).then(|| {
                    retry
                        .last_unix
                        .unwrap_or(retry.seen_unix)
                        .saturating_add(LIMIT_RETRY_RELOOK_S)
                });
                look.map_or(past_reset, |look| past_reset.min(look))
                    .max(backed_off)
            }
            None => {
                let rung = usize::try_from(retry.tries).unwrap_or(usize::MAX);
                let pause =
                    crate::supervise::ladder::Ladder(&self.turn_end.timing.entitlement_backoff)
                        .at(rung)?;
                retry
                    .last_unix
                    .unwrap_or(retry.seen_unix)
                    .saturating_add(secs(pause))
            }
        };
        Some(until_unix)
    }

    /// Returns the [`StoppedLimit`] when Gemini CLI's usage-limit dialog was answered `Stop` or its agent relaunched while held.
    pub(super) fn limit_stopped(
        &self,
        reading: &aterm_phase::Reading,
        rows: &[String],
    ) -> Option<StoppedLimit> {
        let retry = self.limit_retry.as_ref()?;
        if !keeps_trying_its_limit(retry.program) {
            return None;
        }
        let stopped = reading
            .wall
            .as_ref()
            .filter(|w| aterm_phase::gemini::stopped_error(w));
        let relaunched = stopped.is_none()
            && retry.away
            && retry.relaunch.is_some()
            && reading.program == retry.program
            && reading.prompt.is_none()
            && !matches!(reading.phase, Phase::Busy | Phase::Prompt);
        if stopped.is_none() && !relaunched {
            return None;
        }
        let moved = retry
            .model
            .as_ref()
            .zip(aterm_phase::gemini::footer_model(rows))
            .is_some_and(|(was, now)| *was != now)
            || (relaunched && retry.relaunch == Some(false));
        let due = if moved {
            self.now_unix()
        } else {
            self.limit_retry_until(retry)?
        };
        let reset_unix = due.saturating_sub(self.limit_retry_grace_s());
        let title = aterm_phase::anchor("gemini.usage.title");
        let how = if relaunched {
            "the agent was relaunched while its dialog stood"
        } else {
            "its dialog was answered Stop"
        };
        let placed = retry.at_unix.and(retry.reset.as_ref());
        let message = match (placed, retry.reset.as_ref()) {
            (Some(reset), _) => format!(
                "{title} the session's model; {how}. {}{reset}.",
                aterm_phase::anchor("gemini.usage.resets"),
            ),
            (None, Some(drawn)) => format!(
                "{title} the session's model; {how}. It names a reset the loop cannot place ({}).",
                crate::harness::one_line(&drawn.to_string(), 24)
            ),
            (None, None) => format!("{title} the session's model; {how}. It names no reset."),
        };
        let (row, placement) = match stopped {
            Some(w) => (w.row, w.placement),
            None => (
                rows.iter()
                    .rposition(|r| !r.trim().is_empty())
                    .unwrap_or_default(),
                aterm_phase::Placement::ErrorRow,
            ),
        };
        Some(StoppedLimit {
            wall: aterm_phase::Wall {
                kind: aterm_phase::WallKind::UsageSession,
                message,
                reset: placed.map(ToString::to_string),
                row,
                placement,
                under_call: false,
            },
            reset_unix,
        })
    }

    /// Keeps the [`LimitRetry`] episode across the agent's absence and judges the relaunch's seat on its return.
    pub(super) fn limit_retry_away(
        &mut self,
        program: Program,
        rows: &[String],
        opts: &SuperviseOpts,
    ) -> bool {
        let Some(retry) = self
            .limit_retry
            .as_ref()
            .filter(|r| keeps_trying_its_limit(r.program))
        else {
            return false;
        };
        if !reads_conversation(program) && !screen_alone(program) {
            if let Some(retry) = self.limit_retry.as_mut() {
                retry.away = true;
                retry.relaunch = None;
            }
            return true;
        }
        if !retry.away || program != retry.program {
            return false;
        }
        if retry.relaunch.is_none() {
            let here = Seat::of(program, rows);
            let vouched = self.turn_end.seat_vouched(program, &here)
                || opts.idle_host.as_ref().is_some_and(|h| h.relaunch_owed());
            if let Some(retry) = self.limit_retry.as_mut() {
                retry.relaunch = Some(vouched);
            }
        }
        true
    }

    /// Returns whether the [`LimitRetry`] episode is currently marked away.
    pub(super) fn limit_retry_is_away(&self) -> bool {
        self.limit_retry.as_ref().is_some_and(|r| r.away)
    }

    /// Clears the away state once the turn-end policy's limit track takes over after a relaunch.
    pub(super) fn limit_retry_back(&mut self) {
        if self.limit_ladder_kept()
            && let Some(retry) = self.limit_retry.as_mut()
        {
            retry.away = false;
            retry.relaunch = None;
        }
    }

    /// Records the footer model when `Stop`'s error is first read under a [`LimitRetry`] episode.
    pub(super) fn limit_stopped_seat(&mut self, reading: &aterm_phase::Reading, rows: &[String]) {
        let Some(retry) = self.limit_retry.as_mut().filter(|r| r.model.is_none()) else {
            return;
        };
        if keeps_trying_its_limit(retry.program)
            && reading
                .wall
                .as_ref()
                .is_some_and(aterm_phase::gemini::stopped_error)
        {
            retry.model = aterm_phase::gemini::footer_model(rows);
        }
    }

    /// Returns the badge reason at `Stop`'s error when the usage-limit dialog named no placeable reset and no badge is up yet.
    pub(super) fn limit_stopped_badge(&self, point: &Turn, key: &str) -> Option<String> {
        let retry = self.limit_retry.as_ref()?;
        if retry.at_unix.is_some()
            || !keeps_trying_its_limit(retry.program)
            || self.box_ask.as_ref().is_some_and(|b| b.key == key)
            || !aterm_phase::gemini::wall(&point.screen.rows)
                .is_some_and(|w| aterm_phase::gemini::stopped_error(&w))
        {
            return None;
        }
        Some(limit_stopped_reason(
            retry.reset.as_ref().map(ToString::to_string).as_deref(),
        ))
    }

    /// Records that the episode's badge was raised at the point keyed `key`.
    pub(super) fn limit_badge_raised(&mut self, key: &str) {
        if let Some(retry) = self.limit_retry.as_mut() {
            retry.badge = Some(key.to_string());
        }
    }

    /// Carries the episode's badge to `turn` (keyed `key`) while the usage limit still stands.
    pub(super) fn limit_badge_carry(&mut self, turn: &Turn, key: &str) -> bool {
        let rows = &turn.screen.rows;
        let program = self.reader(rows).program();
        let (Some(retry), Some(ask)) = (self.limit_retry.as_mut(), self.box_ask.as_mut()) else {
            return false;
        };
        if retry.badge.as_deref() != Some(ask.key.as_str())
            || !keeps_trying_its_limit(retry.program)
        {
            return false;
        }
        let its_limit = aterm_phase::gemini::prompt(rows)
            .is_some_and(|p| p.kind == PromptKind::UsageLimit)
            || aterm_phase::gemini::wall(rows)
                .is_some_and(|w| aterm_phase::gemini::stopped_error(&w));
        let elsewhere =
            program == retry.program || (!reads_conversation(program) && !screen_alone(program));
        if !its_limit && !elsewhere {
            return false;
        }
        ask.key = key.to_string();
        retry.badge = Some(key.to_string());
        self.limit_badge_away = (!its_limit).then(|| key.to_string());
        true
    }

    /// Returns whether a badge carried off its limit screen should now be cleared because its episode ended.
    pub(super) fn limit_badge_off_ended(&mut self) -> bool {
        let Some(key) = self.limit_badge_away.as_deref() else {
            return false;
        };
        let up = self.box_ask.as_ref().is_some_and(|b| b.key == key);
        let stands = self
            .limit_retry
            .as_ref()
            .is_some_and(|r| r.badge.as_deref() == Some(key));
        if !up || !stands {
            self.limit_badge_away = None;
        }
        up && !stands
    }

    /// Records a resume typed at `Stop`'s error or while the ladder is kept across a relaunch.
    pub(super) fn limit_resume_typed(&mut self, rows: &[String]) {
        if aterm_phase::gemini::wall(rows).is_some_and(|w| aterm_phase::gemini::stopped_error(&w))
            || self.limit_ladder_kept()
        {
            self.limit_retry_pressed();
            if let Some(retry) = self.limit_retry.as_mut() {
                retry.resumed = true;
            }
        }
    }

    /// Returns whether the [`LimitRetry`] episode and the turn-end policy's limit track both stand for the same program.
    pub(super) fn limit_ladder_kept(&self) -> bool {
        self.limit_retry
            .as_ref()
            .is_some_and(|r| keeps_trying_its_limit(r.program))
            && self.turn_end.has_limit_track()
            && self
                .limit_retry
                .as_ref()
                .is_some_and(|r| self.turn_end.limit_program() == Some(r.program))
    }

    fn limit_retry_past_reset(&self, at_unix: i64) -> i64 {
        at_unix
            .saturating_add(LIMIT_RETRY_MINUTE_S)
            .saturating_add(self.limit_retry_grace_s())
    }

    fn limit_retry_grace_s(&self) -> i64 {
        i64::try_from(self.turn_end.timing.grace_for(Program::Gemini).as_secs()).unwrap_or(i64::MAX)
    }

    fn limit_retry_hold(
        &mut self,
        turn: &Turn,
        p: &aterm_phase::PromptV2,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if p.kind != PromptKind::UsageLimit || !review.unattended() {
            return Ok(false);
        }
        let RetryDue::Hold { until_unix, reset } = self.limit_retry_due(&turn.screen.rows, opts)
        else {
            return Ok(false);
        };
        let now = self.now_unix();
        let wait = Duration::from_secs(u64::try_from(until_unix - now).unwrap_or(0));
        let until = self.fx.mono() + wait;
        let (tries, resumed) = self
            .limit_retry
            .as_ref()
            .map_or((0, false), |r| (r.tries, r.resumed));
        let unplaced = self
            .limit_retry
            .as_ref()
            .and_then(|r| r.at_unix.is_none().then_some(r.reset.as_ref()).flatten())
            .map(ToString::to_string);
        let retry = aterm_phase::anchor("gemini.usage.retry");
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        let tries_by = LimitTry::of(&labels).unwrap_or(LimitTry::KeepTrying);
        let pressed = tries_by.pressed();
        let (why, how) = match (&reset, &unplaced) {
            (Some(reset), _) => (
                format!("Gemini CLI's usage limit resets at {reset}"),
                format!("{pressed} past it"),
            ),
            (None, Some(drawn)) => (
                format!(
                    "Gemini CLI's usage limit names a reset the loop cannot place ({})",
                    crate::harness::one_line(drawn, 24)
                ),
                format!("{pressed} on the back-off ladder"),
            ),
            (None, None) => (
                "Gemini CLI's usage limit names no reset".to_string(),
                format!("{pressed} on the back-off ladder"),
            ),
        };
        let told = self
            .turn_end_due
            .as_ref()
            .is_some_and(|d| d.why == why && (d.unix_ms.div_euclid(1000) - until_unix).abs() <= 1);
        if !told {
            let back = match (tries, resumed) {
                (0, _) => String::new(),
                (n, true) => format!(" (drawn again after its typed resume, try {n})"),
                (n, false) => format!(" (drawn again after its {retry}, try {n})"),
            };
            let said = format!("{why}: {how}{back}");
            let stamp = utc_stamp(u64::try_from(until_unix).unwrap_or(0));
            let seq = turn.screen.seq;
            review.note(&format!(
                "WAITING seq={seq} until={stamp} rule={RULE_LIMIT_RETRY} {said}"
            ));
            self.ledger_row(
                RULE_LIMIT_RETRY,
                approvals::Outcome::Waiting,
                &p.command,
                &format!("until {stamp}: {said}"),
                seq,
            );
        }
        if reset.is_none() {
            let key = self.review_key(turn);
            if self.box_ask.as_ref().is_none_or(|b| b.key != key) {
                let reason = limit_no_reset_reason(unplaced.as_deref(), tries_by);
                self.ledger_row(
                    "-",
                    approvals::Outcome::Escalated,
                    &p.command,
                    &reason,
                    turn.screen.seq,
                );
                self.escalate_point(turn, Some(&reason), review)?;
            }
            if let Some(retry) = self.limit_retry.as_mut() {
                retry.badge = Some(key);
            }
        }
        self.set_due(until, why);
        self.held = true;
        Ok(true)
    }

    /// A `Keep trying` of Gemini CLI's usage-limit dialog landed.
    pub(super) fn limit_retry_pressed(&mut self) {
        let now = self.now_unix();
        let early = self
            .limit_retry
            .as_ref()
            .and_then(|r| r.at_unix)
            .is_some_and(|at| now < self.limit_retry_past_reset(at));
        if let Some(retry) = self.limit_retry.as_mut() {
            retry.tries = retry.tries.saturating_add(1);
            retry.looks = retry.looks.saturating_add(u32::from(early));
            retry.last_unix = Some(now);
            retry.resumed = false;
        }
        self.limit_dialog_read = None;
    }

    /// Handles a busy read while a [`LimitRetry`] episode stands.
    pub(super) fn limit_retry_busy(&mut self) -> bool {
        let now = self.now_unix();
        let Some(retry) = self.limit_retry.as_ref() else {
            return false;
        };
        let in_flight = retry.away
            || retry
                .last_unix
                .is_some_and(|last| now.saturating_sub(last) < LIMIT_RETRY_ANSWER_S);
        if !in_flight {
            self.limit_retry = None;
            return false;
        }
        retry
            .badge
            .as_ref()
            .is_some_and(|k| self.box_ask.as_ref().is_some_and(|b| b.key == *k))
    }

    fn box_back_off(
        &mut self,
        turn: &Turn,
        what: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let key = box_identity(&turn.screen.rows).unwrap_or_default();
        let now = self.fx.mono();
        if self.press_retry.as_ref().is_none_or(|r| r.key != key) {
            self.press_retry = Some(PressRetry {
                key,
                tries: 0,
                since: now,
                badged: false,
            });
        }
        let Some(r) = self.press_retry.as_mut() else {
            return Ok(Step::Again { settle: false });
        };
        r.tries = r.tries.saturating_add(1);
        let pause = crate::supervise::ladder::doubling(REFUSAL_PAUSE, r.tries - 1, PRESS_RETRY_MAX);
        let badge = !r.badged && now.duration_since(r.since) >= self.setup.press_badge_after;
        r.badged |= badge;
        let tries = r.tries;
        let seq = turn.screen.seq;
        review.note(&format!(
            "WAITING seq={seq} {why} (try {tries}): read and tried again in {} ms: {}",
            pause.as_millis(),
            clip(what)
        ));
        if badge && review.unattended() {
            let reason = format!("{why}; still trying ({tries} tries)");
            self.escalate_point(turn, Some(&reason), review)?;
        }
        self.wait(&["seq", &seq.to_string()], pause)?;
        Ok(Step::Again { settle: false })
    }

    #[allow(clippy::too_many_arguments)]
    fn question_hold(
        &mut self,
        turn: &Turn,
        dialog: &QuestionDialog,
        identity: &str,
        answering: Option<&str>,
        approved: &[Approved],
        grace: Duration,
        rule_id: &str,
        review: &mut dyn Review,
    ) -> Result<Option<Hold>, Fail> {
        let seq = turn.screen.seq;
        let quiet_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX);
        let over = self.hold_over(&turn.screen);
        match turn.screen.human {
            _ if over => {}
            HumanInput::Never | HumanInput::NeverFor(_) => {}
            HumanInput::Ago(ms) if ms >= quiet_ms => {}
            HumanInput::Ago(ms) => {
                self.question.person_gave_input();
                self.note_question_wait(
                    review,
                    seq,
                    rule_id,
                    identity,
                    &format!(
                        "a person gave the session input {ms} ms ago: the question waits for {} \
                         s of their quiet",
                        grace.as_secs()
                    ),
                );
                let left = Duration::from_millis(quiet_ms - ms);
                self.wait(&["seq", &seq.to_string()], left.min(WAIT_STEP))?;
                return Ok(Some(Hold::Again));
            }
            HumanInput::Unknown if self.question.standin.as_deref() == Some(identity) => {}
            HumanInput::Unknown => {
                self.note_question_wait(
                    review,
                    seq,
                    rule_id,
                    identity,
                    &format!(
                        "this host does not stamp a person's input: the question waits for the \
                         screen to hold still {} s",
                        QUESTION_STANDIN_QUIET.as_secs()
                    ),
                );
                let ms = QUESTION_STANDIN_QUIET.as_millis().to_string();
                match self.wait(&["idle", &ms], QUESTION_STANDIN_QUIET + SETTLE_CAP)? {
                    Wait::Latched | Wait::Unsupported => {
                        self.question.standin = Some(identity.to_string());
                    }
                    Wait::TimedOut => {}
                }
                return Ok(Some(Hold::Again));
            }
        }
        let focus = dialog.focus();
        let unchanged = self
            .question
            .pending
            .clone()
            .filter(|p| p.identity == identity && p.focus == focus);
        let toggling = match &unchanged {
            Some(QuestionPending {
                kind: PendingKind::Enter,
                focus: QuestionFocus::Option(n),
                ..
            }) => Some(*n),
            _ => None,
        };
        let unattended = review.unattended();
        let retrying = unchanged.is_some();
        match unchanged {
            Some(QuestionPending {
                waited: false,
                unsettled,
                ..
            }) => {
                if unsettled == 0 {
                    review.note(&format!(
                        "WAITING seq={seq} rule={rule_id} the dialog has not shown the last \
                         key's effect: read again once the screen holds still"
                    ));
                }
                let (quiet, step) = self.still_wait(&turn.screen);
                let settled = matches!(self.wait(&["idle", quiet], step)?, Wait::Latched);
                if let Some(p) = self.question.pending.as_mut() {
                    if settled {
                        p.waited = true;
                    } else {
                        p.unsettled += 1;
                        if p.unsettled >= MAX_UNSETTLED_WAITS {
                            if unattended {
                                p.unsettled = 0;
                                return Ok(Some(Hold::BackOff(
                                    "the screen has not held still for the key to be sent again"
                                        .to_string(),
                                )));
                            }
                            self.question.pending = None;
                            return Ok(Some(Hold::HandOver(
                                "the dialog did not take the key, and the screen never held \
                                 still after it for the key to be sent once more"
                                    .to_string(),
                            )));
                        }
                    }
                }
                return Ok(Some(Hold::Again));
            }
            Some(QuestionPending {
                retries,
                backed_off,
                ..
            }) if retries > 0 => {
                if !unattended {
                    self.question.pending = None;
                    return Ok(Some(Hold::HandOver(
                        "the dialog did not take the key: nothing changed after it, nor after it \
                         was sent once more"
                            .to_string(),
                    )));
                }
                if !backed_off {
                    if let Some(p) = self.question.pending.as_mut() {
                        p.backed_off = true;
                    }
                    return Ok(Some(Hold::BackOff(format!(
                        "the dialog took none of the {} keys sent",
                        retries.saturating_add(1)
                    ))));
                }
            }
            Some(_) => {}
            None => {
                if let Some(p) = self.question.pending.take()
                    && self
                        .press_retry
                        .as_ref()
                        .is_some_and(|r| r.key == p.identity)
                {
                    self.press_retry = None;
                }
            }
        }
        if let QuestionForm::Multi { options, .. } = &dialog.form {
            let mine = self
                .question
                .toggled
                .get(&question_key(dialog, &turn.screen.rows));
            let made = |n: u8| mine.is_some_and(|m| m.contains(&n));
            if let Some(o) = options
                .iter()
                .find(|o| o.checked == Some(true) && !made(o.n))
            {
                return Ok(Some(Hold::HandOver(begun(&format!(
                    "option {} is checked, and the supervisor did not check it",
                    o.n
                )))));
            }
            if let Some(o) = options
                .iter()
                .find(|o| made(o.n) && o.checked != Some(true) && toggling != Some(o.n))
            {
                return Ok(Some(Hold::HandOver(begun(&format!(
                    "option {}, which the supervisor checked, reads unchecked",
                    o.n
                )))));
            }
        }
        if let Some(q) = answering.filter(|_| !retrying) {
            let answered = approved
                .iter()
                .filter(|a| a.question.as_deref() == Some(q))
                .count();
            let waited = self
                .question
                .reasked
                .as_ref()
                .is_some_and(|(k, n)| k == q && *n == answered);
            if answered >= REASKED_AFTER && !waited {
                self.question.reasked = Some((q.to_string(), answered));
                let pause = reask_pause(answered);
                review.note(&format!(
                    "WAITING seq={seq} rule={rule_id} the same question came back after \
                     {answered} answers since the last review point (the model is re-asking \
                     it): answered again in {} ms",
                    pause.as_millis()
                ));
                self.wait(&["seq", &seq.to_string()], pause)?;
                return Ok(Some(Hold::Again));
            }
        }
        Ok(None)
    }

    fn answered(
        &mut self,
        act: &Act,
        rule_id: &str,
        shown: &str,
        reason: &str,
        (at, seq): (u64, u64),
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        match act {
            Act::Approve { .. } => {
                self.ledger_row(rule_id, approvals::Outcome::Approved, shown, reason, seq);
                review.approved(at, shown)?;
            }
            Act::Decline { text, .. } => {
                self.ledger_row(rule_id, approvals::Outcome::Declined, shown, reason, seq);
                review.note(&format!(
                    "DECLINED seq={at} rule={rule_id} {} => {text}",
                    clip(shown)
                ));
            }
        }
        Ok(())
    }

    fn decline_again(&mut self, key: &str, approved: &[Approved]) -> Option<Duration> {
        let declines = approved
            .iter()
            .filter(|a| a.declined.as_deref() == Some(key))
            .count();
        let waited = self
            .decline
            .again
            .as_ref()
            .is_some_and(|(k, n)| k == key && *n == declines);
        if declines < REASKED_AFTER || waited {
            return None;
        }
        self.decline.again = Some((key.to_string(), declines));
        Some(reask_pause(declines))
    }

    fn note_question_wait(
        &mut self,
        review: &mut dyn Review,
        seq: u64,
        rule_id: &str,
        identity: &str,
        why: &str,
    ) {
        if self.question.waiting_said.as_deref() != Some(identity) {
            self.question.waiting_said = Some(identity.to_string());
            review.note(&format!("WAITING seq={seq} rule={rule_id} {why}"));
        }
    }

    fn box_left(
        &mut self,
        at: u64,
        identity: &str,
        deny: Option<Instant>,
        deadline: Instant,
    ) -> Result<Past, Fail> {
        let pressed = self.fx.mono();
        let mut until = pressed + leave_wait(pressed, deny);
        let mut since = at;
        let mut by = deadline;
        loop {
            let step = until.saturating_duration_since(pressed);
            let (now, moved) = match self.moved_past(since, by, step)? {
                Past::Moved => (self.screen()?, true),
                Past::Still(Some(now)) => (now, false),
                Past::Still(None) if self.fx.mono() < deadline => (self.screen()?, false),
                still => return Ok(still),
            };
            if box_identity(&now.rows).as_deref() != Some(identity) {
                self.box_gone();
                return Ok(Past::Moved);
            }
            until = pressed + leave_wait(pressed, self.deny_at(&now.rows));
            if !moved || self.fx.mono() >= until {
                return Ok(Past::Still(Some(now)));
            }
            since = now.seq;
            by = deadline.min(until);
        }
    }
}

fn note_unproven(review: &mut dyn Review, seq: u64, rule_id: &str, unproven: Option<&str>) {
    if let Some(u) = unproven {
        review.note(&format!("UNPROVEN seq={seq} rule={rule_id} {u}"));
        if rule_id == RULE_RM_BREAKER_WARN {
            let _ = review.say(&format!("WARN seq={seq} rule={rule_id} {u}"));
        }
    }
}

const fn is_muse(program: &Program) -> bool {
    match program {
        Program::Muse => true,
        Program::Claude | Program::Codex | Program::Agy | Program::Gemini | Program::Generic => {
            false
        }
    }
}

#[cfg(test)]
mod act_tests {
    use super::*;
    use crate::harness::acts::{Act, TaskFace};
    use crate::supervise::approvals::Outcome;
    use crate::supervise::policy::question::RULE_ANSWER_RECOMMENDED;
    use crate::supervise::policy::turn_end::{
        RULE_ANSWER, RULE_COMPACT, RULE_CONTINUE, RULE_LAND, RULE_LAND_NUDGE, RULE_LOGIN,
    };
    use std::sync::Mutex;

    struct Silent;

    impl Ctl for Silent {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            Err(format!("no server for {args:?}"))
        }
    }

    #[derive(Debug, Default)]
    struct Recorder {
        acts: Mutex<Vec<Act>>,
    }

    impl IdleHost for Recorder {
        fn wants(&self) -> bool {
            false
        }
        fn at_idle(&self) -> Option<HostStep> {
            None
        }
        fn owns_turn_end(&self) -> bool {
            false
        }
        fn act_taken(&self, act: Act) {
            self.acts.lock().unwrap().push(act);
        }
        fn task_face(&self, face: TaskFace) {
            let _ = face;
        }
    }

    const ROWS: [(&str, Outcome, &str); 15] = [
        ("allow-once@v1", Outcome::Approved, "cargo test"),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
        (RULE_ANSWER, Outcome::Typed, "Decide for yourself."),
        (RULE_CONTINUE, Outcome::Typed, "keep going"),
        (RULE_COMPACT, Outcome::Typed, "/compact"),
        (RULE_COMPACT, Outcome::Typed, "Carry on where you left off."),
        (RULE_LAND, Outcome::Typed, "[aterm harness] land"),
        (
            RULE_LAND_NUDGE,
            Outcome::Typed,
            "[aterm harness] still landing",
        ),
        ("decline@v1", Outcome::Declined, "rm -rf /"),
        (RULE_LOGIN, Outcome::Typed, "/login"),
        ("read-only@classify-v9", Outcome::Skipped, "ls"),
        ("-", Outcome::Escalated, ""),
        ("wall-wait@v1", Outcome::Waiting, ""),
    ];

    fn write_rows(dir: &std::path::Path, host: Option<Arc<dyn IdleHost>>) -> Vec<Option<Act>> {
        let path = dir.join("s-1.jsonl");
        let mut ctl = Silent;
        let mut s = Session::new(&mut ctl, Some("s-1".to_string()));
        s.set_approval_ledger(Some(path.clone()));
        s.ledger = approvals::Ledger::open(Some(&path), Some("s-1"), &mut std::io::sink());
        s.stall_host = host;
        for (i, (rule, outcome, command)) in ROWS.into_iter().enumerate() {
            s.ledger_row(rule, outcome, command, "", i as u64);
        }
        let body = std::fs::read_to_string(&path).expect("the ledger");
        body.lines()
            .map(|line| {
                let v: aterm_json::Value = aterm_json::from_str(line).expect("a row");
                let text = |k: &str| {
                    v.get(k)
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                let outcome = Outcome::from_word(&text("decision")).expect("its decision");
                crate::harness::acts::sort(&text("rule_id"), outcome, &text("command")).act
            })
            .collect()
    }

    #[test]
    fn the_ledgers_writer_tells_its_host_one_act_per_act_row() {
        let dir = crate::supervise::ScratchDir::new("acts", "told");
        let host = Arc::new(Recorder::default());
        let read = write_rows(&dir, Some(Arc::clone(&host) as Arc<dyn IdleHost>));
        assert_eq!(read.len(), ROWS.len(), "every row written");
        let told = host.acts.lock().unwrap().clone();
        let acts: Vec<Act> = read.into_iter().flatten().collect();
        assert_eq!(told, acts);
        assert_eq!(
            told,
            [
                Act::Approved,
                Act::Answered,
                Act::Answered,
                Act::Answered,
                Act::Answered,
                Act::Continued,
                Act::Resumed,
                Act::Landing,
                Act::Nudged,
                Act::Declined,
            ]
        );
        let bare = crate::supervise::ScratchDir::new("acts", "bare");
        assert_eq!(write_rows(&bare, None).len(), ROWS.len());
        assert_eq!(host.acts.lock().unwrap().len(), told.len());
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor approval event loop driving `approval` policy decisions against `Fx`.
use super::super::policy::WorkerEnv;
use super::super::policy::approval::RULE_RM_BREAKER_WARN;
use super::super::policy::approval::{
    AnswerTarget, ApprovalCtx, Choice, DeclineStep, QuestionPriority, RULE_AUTO_MODE_SETUP,
    RULE_AUTOCOMPACT_OFFER, RULE_LIMIT_RESET, RULE_LIMIT_WAIT, RULE_TRUST_ANY, RULE_TRUST_DIALOG,
    decide, proof_would_lift, unvoiced_tools,
};
use super::super::policy::box_call;
use super::super::policy::box_facts::{BoxFacts, QuestionFacts};
use super::super::policy::login::PickerGate;
use super::super::policy::question::{begun, fall_back_to, is_question_rule};
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

/// THE MOST QUIET A PERSON'S INPUT BUYS BEFORE THE LOOP ANSWERS A CHOICE DIALOG (a question, or
/// any other dialog that grants and spends nothing: [`Dialog::of`]): their grace (`[harness] human_grace_s`,
/// 120 s by default, any length the owner sets), but never more than this. A keystroke this recent
/// is a person answering now, and their keys are never raced; one older is no reason to leave the
/// dialog to them (owner, 2026-10-24: "THE HARNESS MUST DO THIS STOP ASKING THE USER"), so it is
/// answered without them.
pub(super) const QUESTION_PERSON_GRACE: Duration = Duration::from_secs(15);

/// The quiet a person's input buys before the loop acts: `[harness] human_grace_s`.
fn person_grace(opts: &SuperviseOpts) -> Duration {
    Duration::from_secs(u64::from(opts.policy.human_grace_s))
}

/// What the loop is about to answer, as far as a person's grace is concerned. The loop answers
/// only under Cruise or Land (under Off it parks), so this is Autopilot's rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog {
    /// A choice that grants nothing and spends nothing (a question, a plan-mode switch, a goal, a
    /// model or effort switch, the autocompact offer, the auto-mode setup offer):
    /// [`QUESTION_PERSON_GRACE`] at most.
    Choice,
    /// A permission box, which grants a command, a tool, a file, the network, a folder, an app or
    /// a server its reach: [`QUESTION_PERSON_GRACE`] at most while nobody has touched it since it
    /// appeared, the person's whole grace once a person or another driver has
    /// ([`Session::answer_grace`]).
    Permission,
    /// The usage-limit menu, whose answer can spend a reset or credits; a held message, another
    /// session's text, which becomes the worker's prompt; or a box not recognized: the person's
    /// whole grace.
    Grant,
}

impl Dialog {
    /// Which `kind` is. Every kind is named, so a new one is placed here by hand. An unrecognized
    /// box ([`PromptKind::Other`]), a held message and the usage-limit menu (under `usage_credits`
    /// its answer spends, which stays at a person's pace) keep the whole grace.
    const fn of(kind: PromptKind) -> Self {
        match kind {
            PromptKind::Question
            | PromptKind::PlanEnter
            | PromptKind::PlanExit
            | PromptKind::GoalProposal
            | PromptKind::ModelSwitch
            | PromptKind::AutocompactOffer
            | PromptKind::AutoModeSetup => Self::Choice,
            PromptKind::Bash
            | PromptKind::Edit
            | PromptKind::Write
            | PromptKind::Overwrite
            | PromptKind::Read
            | PromptKind::Workflow
            | PromptKind::Trust
            | PromptKind::PowerShell
            | PromptKind::Fetch
            | PromptKind::NetworkRequest
            | PromptKind::Browser
            | PromptKind::Skill
            | PromptKind::Monitor
            | PromptKind::Tool
            | PromptKind::ComputerUse
            | PromptKind::ReadOutsideSetting
            | PromptKind::McpServer => Self::Permission,
            PromptKind::UsageLimit | PromptKind::HeldMessage | PromptKind::Other => Self::Grant,
        }
    }
}

/// The quiet a person's input buys before the loop answers `dialog` ([`QUESTION_PERSON_GRACE`]),
/// a permission box counted as touched ([`Session::answer_grace`] reads its episode).
fn dialog_grace(opts: &SuperviseOpts, dialog: Dialog) -> Duration {
    match dialog {
        Dialog::Choice => person_grace(opts).min(QUESTION_PERSON_GRACE),
        Dialog::Permission | Dialog::Grant => person_grace(opts),
    }
}

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
    /// The content seq at which a key written to this dialog was last unread input, as written
    /// or as `status` read it, and when ([`Session::question_key_undrawn`]).
    pub(super) unread_at: Option<(u64, Instant)>,
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
    /// The stored word a wait at rest last woke on (`OK autopilot <word>`), until a read.
    reported: Option<Option<Autopilot>>,
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
        self.reported = None;
        self.fresh = true;
        self.read = true;
        self.answered = true;
    }

    /// A wait at rest woke on `word` as the session's stored word ([`Self::latch`]).
    pub(super) const fn reported(&mut self, word: Option<Autopilot>) {
        self.reported = Some(word);
    }

    /// The stored word a wait at rest carries (`-` unset): the one a wait last woke on, else the
    /// one last read; `None` while `meta` names none.
    pub(super) fn latch(&self) -> Option<&'static str> {
        self.kept.then(|| {
            self.reported
                .unwrap_or(self.word)
                .map_or("-", Autopilot::word)
        })
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
        let roster = typed
            && opts.policy.logins
            && opts
                .idle_host
                .as_deref()
                .is_some_and(|h| h.login_label().is_some());
        let back = self.turn_end.login_back_from(shared.as_ref());
        let gate = super::super::policy::login::picker_gate(typed, back, roster, renewed);
        let login_carry = gate == PickerGate::Carried;
        let login_back = gate == PickerGate::Back;
        let login_roster = gate == PickerGate::Roster;
        let login_renewed = gate == PickerGate::Renewed;
        let own_rule = |limit_reset: bool| {
            aterm_phase::read(None, &screen.rows, None)
                .prompt
                .is_some_and(|p| {
                    p.kind == PromptKind::Question
                        || p.kind == PromptKind::McpServer
                        || p.kind == PromptKind::AutoModeSetup
                        || login_carry
                        || login_back
                        || login_roster
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
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
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
        facts.login_roster = login_roster;
        facts.login_renewed = login_renewed;
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
                        || *rule_id == super::super::policy::login::RULE_LOGIN_BACK_OUT
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
        self.autopilot_under(&opts.policy)
    }

    /// [`Self::autopilot`] under the policy `cfg`.
    pub(super) fn autopilot_under(
        &self,
        cfg: &super::super::config::SupervisorConfig,
    ) -> Autopilot {
        cfg.autopilot_for(self.autopilot.word)
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
        Ok(self
            .autopilot_at_busy(&opts.policy)?
            .is_some_and(|mode| mode != Autopilot::Off))
    }

    /// The session's Autopilot under `cfg`, read afresh for an act at a busy screen (a new
    /// decision, [`Self::autopilot_allows_busy`]); `None` where the session keeps a word the
    /// read could not get.
    pub(super) fn autopilot_at_busy(
        &mut self,
        cfg: &super::super::config::SupervisorConfig,
    ) -> Result<Option<Autopilot>, Fail> {
        self.autopilot.begin();
        let known =
            !self.autopilot.worth_a_read() || self.read_autopilot_word()? || !self.autopilot.kept;
        Ok(known.then(|| cfg.autopilot_for(self.autopilot.word)))
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
        let settle = if matches!(
            kind,
            PromptKind::AutocompactOffer | PromptKind::AutoModeSetup
        ) {
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
        let grace = person_grace(opts);
        self.held_for_person_within(seq, act, fresh, clock, grace, review)
    }

    /// [`Self::held_for_person`] with the person's input buying `grace` of quiet
    /// ([`dialog_grace`] at a box or a question).
    fn held_for_person_within(
        &mut self,
        seq: u64,
        act: &str,
        fresh: bool,
        clock: DenyClock,
        grace: Duration,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if fresh {
            self.program = self.foreground_program()?;
        }
        let Some(grace_ends) = self.hands_off_for(grace) else {
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

    fn box_grace(&self, opts: &SuperviseOpts, screen: &Screen, dialog: Dialog) -> Duration {
        if self.hold_over(screen) {
            Duration::ZERO
        } else {
            self.answer_grace(opts, dialog)
        }
    }

    /// The quiet a person's input buys before the loop answers `dialog` on this read. A permission
    /// box nobody has touched since it appeared is held by input older than it no longer than a
    /// choice dialog is ([`QUESTION_PERSON_GRACE`]): that input answered something else (owner,
    /// 2026-10-25, of a subagent's `bash -c` rm box held 114 s for a keystroke 6 s older than it:
    /// "the claude harness needs to immediately auto-approve these messages!"). A box a person or
    /// another driver touched, or whose touch cannot be told, keeps the whole grace, as ever.
    fn answer_grace(&self, opts: &SuperviseOpts, dialog: Dialog) -> Duration {
        #[cfg(test)]
        if self.bound_off {
            return dialog_grace(opts, dialog);
        }
        let untouched = self.episode.is_some() && self.touched().is_none();
        match dialog {
            Dialog::Permission if untouched => person_grace(opts).min(QUESTION_PERSON_GRACE),
            Dialog::Choice | Dialog::Permission | Dialog::Grant => dialog_grace(opts, dialog),
        }
    }

    /// Until when the loop keeps its hands off the session.
    pub(super) fn hands_off_until(&self, opts: &SuperviseOpts) -> Option<Instant> {
        self.hands_off_for(person_grace(opts))
    }

    /// Until when a person's last input keeps the loop's hands off, their input buying `grace`.
    fn hands_off_for(&self, grace: Duration) -> Option<Instant> {
        self.person
            .and_then(|typed| typed.checked_add(grace))
            .filter(|until| *until > self.fx.mono())
    }

    /// THE PICKER OF THE `/login` THIS LOOP BACKED OUT OF, STILL DRAWN WHILE ITS LAST ESC MAY
    /// STILL BE CLOSING IT ([`crate::supervise::policy::turn_end::TurnEndState::login_closing`]):
    /// held, the point handed over with no one asked, and decided again once that pause is over
    /// (each pause twice the last, at most `BACK_OUT_PAUSE_MAX`), when the picker still drawn gets
    /// the Esc again — never a second Esc inside the pause.
    fn back_out_held(&mut self, seq: u64, review: &mut dyn Review) -> bool {
        let Some(until) = self.turn_end.login_closing(self.fx.mono()) else {
            return false;
        };
        let why = "the /login dialog aterm backed out of to close";
        if self.turn_end_due.as_ref().is_none_or(|due| due.at != until) {
            review.note(&format!(
                "WAITING seq={seq} rule={} {why}: its Esc again at {}",
                super::super::policy::login::RULE_LOGIN_BACK_OUT,
                self.utc_at(until)
            ));
        }
        self.set_due(until, why);
        self.held = true;
        true
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
        if matches!(
            &decision,
            Decision::Approve { rule_id, .. }
                if *rule_id == super::super::policy::login::RULE_LOGIN_BACK_OUT
        ) && self.back_out_held(seq, review)
        {
            return Ok(Step::Review(turn));
        }
        let carried_pick = matches!(
            &decision,
            Decision::Approve { rule_id, .. }
                if *rule_id == super::super::policy::login::RULE_LOGIN_METHOD
                    || *rule_id == super::super::policy::login::RULE_LOGIN_BACK_OUT
        ) && self.carry_unheld();
        let answers = matches!(
            decision,
            Decision::Approve { .. } | Decision::Decline { .. }
        ) && !carried_pick;
        let dialog = Dialog::of(p.kind);
        let grace = self.answer_grace(opts, dialog);
        let launch = if answers && self.hands_off_for(grace).is_some() {
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
                if p.kind == PromptKind::Question && self.hands_off_for(grace).is_some() {
                    self.question.person_gave_input();
                }
                let clock = self.deny_clock(&turn.screen);
                let act = "the box's answer";
                if self.held_for_person_within(seq, act, false, clock, grace, review)? {
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
                let asked = Dialog::Choice;
                let grace = self.box_grace(opts, &turn.screen, asked);
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
        let dialog = Dialog::of(p.kind);
        let grace = self.box_grace(opts, &turn.screen, dialog);
        let past_hold = (self.hold_over(&turn.screen)
            || self.answer_grace(opts, dialog) < dialog_grace(opts, dialog))
            && self.hands_off_for(dialog_grace(opts, dialog)).is_some();
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
                    .filter(|_| {
                        p.kind == PromptKind::AutocompactOffer
                            || rule_id == super::super::policy::login::RULE_LOGIN_BACK_OUT
                    })
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
                if rule_id == super::super::policy::login::RULE_LOGIN_BACK_OUT {
                    let now = self.fx.mono();
                    self.turn_end.login_backed_out_at(now);
                }
                approved.push(this);
                self.answered(&act, rule_id, &what, &over, (at, seq), review)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                let identity = box_identity(&turn.screen.rows).unwrap_or_default();
                let deny = self.deny_at(&turn.screen.rows);
                let screen = match self.box_left(at, &identity, deny, deadline)? {
                    Past::Moved | Past::Word(_) => {
                        self.press_retry = None;
                        return Ok(Step::Again { settle: true });
                    }
                    Past::Still(Some(screen)) => screen,
                    Past::Still(None) => return Ok(Step::Review(turn)),
                };
                let why = "the box did not change after the press";
                let still = self.turn_of(screen);
                if rule_id == super::super::policy::login::RULE_LOGIN_BACK_OUT {
                    // Its pause, not the press retries' ladder, spaces the next Esc: one already
                    // over by now is due at once.
                    return Ok(if self.back_out_held(still.screen.seq, review) {
                        Step::Review(still)
                    } else {
                        Step::Again { settle: false }
                    });
                }
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
                || rule_id == RULE_LIMIT_RESET
                || rule_id == RULE_AUTOCOMPACT_OFFER
                || rule_id == RULE_AUTO_MODE_SETUP
                || rule_id == super::super::policy::login::RULE_LOGIN_METHOD
                || rule_id == super::super::policy::login::RULE_LOGIN_BACK_OUT)
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
                    Wait::Latched | Wait::Unsupported | Wait::Word(_) => {
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
        if unchanged.is_some()
            && let Some((why, left)) = self.question_key_undrawn(seq)
        {
            self.note_question_wait(
                review,
                seq,
                rule_id,
                identity,
                &format!("{why}: nothing sent again until the screen moves"),
            );
            self.wait(&["seq", &seq.to_string()], left.min(WAIT_STEP))?;
            return Ok(Some(Hold::Again));
        }
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
                self.question.unread_at = None;
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

    /// A QUESTION KEY THE WORKER HAS NOT DRAWN IS NEVER SENT AGAIN ON THE SCREEN IT WAS WRITTEN
    /// TO ([`super::turn_end_loop::UNDRAWN_INPUT_HANG`]). A key sent once more because the dialog
    /// still looks as it was keyed is right for a dialog that dropped the key, and wrong for a
    /// starved worker that has the key queued, or read it and has not painted it yet: the second
    /// copy lands on the next screen (a tab's question answered unread, a focus moved twice). A
    /// key is unread input as it is written ([`Session::question_written`]) and whenever `status`
    /// reads the input unread (`input=pending|typeahead`); until the screen moves past the
    /// content seq where it was last unread, nothing is sent again. A screen that has not moved
    /// [`super::turn_end_loop::UNDRAWN_INPUT_HANG`] after that ends the hold, a hang detector,
    /// and the dialog's re-send goes. Returns why the key is held, and how long the hold has
    /// left.
    pub(super) fn question_key_undrawn(&mut self, seq: u64) -> Option<(String, Duration)> {
        let now = self.fx.mono();
        let hang = super::turn_end_loop::UNDRAWN_INPUT_HANG;
        if let Some(word) = self
            .launch_status
            .input
            .as_deref()
            .filter(|w| matches!(*w, "pending" | "typeahead"))
        {
            let why = format!("the worker has not read the last key written to it (input={word})");
            self.question.unread_at = Some((seq, now));
            return Some((why, hang));
        }
        match self.question.unread_at {
            Some((at, since)) if at == seq && now.saturating_duration_since(since) < hang => Some((
                "the worker has not drawn the last key written to it (the screen has not moved \
                 since the key was written or seen unread)"
                    .to_string(),
                hang.saturating_sub(now.saturating_duration_since(since)),
            )),
            _ => {
                self.question.unread_at = None;
                None
            }
        }
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
            let (now, moved) = match self.moved_past(since, by, step, false)? {
                Past::Moved | Past::Word(_) => (self.screen()?, true),
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
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Generic => false,
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
        ("read-only@classify-v10", Outcome::Skipped, "ls"),
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

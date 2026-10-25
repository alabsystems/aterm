// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure supervisor tool-approval policy decider: [`decide`] judges a box against an
//! [`ApprovalCtx`] into a [`Decision`].
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_phase::prompt::{
    Opt, PromptKind, PromptV2, RmBreaker, RmBreakerKind, RmCommand, Role, Select, asks, cut_short,
    kept_answer, rm_breaker_of, settles_later,
};
use aterm_phase::{Phase, Program, Reading, anchor};

use super::super::config::{Approve, SupervisorConfig};
use super::a_or_an;
use super::box_command;
use super::claude_settings::{AskKind, OwnerAsk};
use super::git_config::{GitView, WorkerEnv, git_reads, hazard};
use super::guard::row_guard;
use super::probe::{PolicyProbe, WorkerWrites};
use super::rm_breaker::{RmScope, ScratchRoot, abs_components, clip, resolve_rm_line};
use super::shell_startup::ShellStartup;
use super::trace::{NoTrace, Outcome, Trace, note};
use crate::harness::lights::Mode;
use crate::supervise::classify::{
    brace_expansion, classify_command_with, classify_except_rm, command_words, glob_match,
    path_operands, plain_zsh_line, program, runs, unquoted_glob,
};

/// Rule identifier for the read-only Bash classifier approval rule.
pub const RULE_READ_ONLY: &str = "read-only@classify-v9";
/// [`RULE_READ_ONLY`]'s proof over a Bash box whose one note is Claude Code's no-rule note
/// (`This command requires approval`).
pub const RULE_NO_RULE_READ_ONLY: &str = "no-rule-read-only@v4";
/// The rm circuit-breaker rule's id; it does not run [`proof_gaps_clear`], whose assignments the
/// rm resolver follows.
pub const RULE_RM_BREAKER: &str = "rm-breaker@v8";
/// The rm circuit breaker's too-many box ([`RmBreakerKind::TooManySubstitutions`]) over a
/// command that names no rm.
pub const RULE_BREAKER_READ_ONLY: &str = "breaker-read-only@v9";
/// The rm circuit breaker the safe rules could not prove, approved with a warning under
/// `approve = "safe"` ([`rm_breaker_warn`]).
pub const RULE_RM_BREAKER_WARN: &str = "rm-breaker-warn@v1";
/// The Read-outside-cwd rule's id.
pub const RULE_READ_OUTSIDE_CWD: &str = "read-outside-cwd@v2";
/// The folder-trust dialog rule's id.
pub const RULE_TRUST_DIALOG: &str = "trust-dialog@v1";
/// Full power: a permission box's one-shot allow.
pub const RULE_ALLOW_ONCE: &str = "allow-once@v1";
/// Full power: the folder-trust dialog, any folder.
pub const RULE_TRUST_ANY: &str = "trust-any@v1";
/// Full power.
pub const RULE_PLAN: &str = "plan-approve@v2";
/// Full power.
pub const RULE_NO_SPEND: &str = "no-spend@v1";
/// Full power under `[harness] usage_credits`.
pub const RULE_CREDITS_CONSENT: &str = "credits-consent@v1";
/// Full power with `[harness] usage_credits` off, the default.
pub const RULE_CREDITS_DECLINE: &str = "credits-decline@v1";
/// Full power.
pub const RULE_DECLINE: &str = "decline@v1";
/// Full power.
pub const RULE_MODEL_SWITCH: &str = "model-switch@v1";
/// A box taller than the screen in a bypass session, declined with what its foot note flags
/// ([`tall_box`]).
pub const RULE_TALL_BOX: &str = "tall-box@v1";
/// Full power.
pub const RULE_MODEL_CONFIRM: &str = "model-confirm@v1";
/// The usage-limit dialog's rule ([`limit_wait`]).
pub const RULE_LIMIT_WAIT: &str = "limit-wait@v1";
/// The usage-limit dialog's grant row ([`limit_reset`]).
pub const RULE_LIMIT_RESET: &str = "limit-reset@v1";
/// Claude Code's autocompact offer ([`autocompact_offer`]).
pub const RULE_AUTOCOMPACT_OFFER: &str = "autocompact-offer@v1";
/// Claude Code's auto-mode environment offer ([`auto_mode_setup`]).
pub const RULE_AUTO_MODE_SETUP: &str = "auto-mode-setup@v1";

/// A question dialog answered under the priority word `long-term` ([`QuestionPriority::LongTerm`]).
pub const RULE_ANSWER_LONG_TERM: &str = "answer-long-term@v1";
/// The same under `short-term` ([`super::question::SHORT_TERM_TEXT`]).
pub const RULE_ANSWER_SHORT_TERM: &str = "answer-short-term@v1";
/// A prose question answered under `long-term` with its policy text instead of
/// `[harness] answer_text`.
pub const RULE_PROSE_LONG_TERM: &str = "prose-long-term@v1";
/// The same under `short-term`.
pub const RULE_PROSE_SHORT_TERM: &str = "prose-short-term@v1";

/// How a question is answered once [`ApprovalCtx::answer_questions`] says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum QuestionPriority {
    #[default]
    Recommended,
    LongTerm,
    ShortTerm,
}

impl QuestionPriority {
    /// Every priority, in [`Self::WORDS`]' order.
    pub const ALL: [Self; 3] = [Self::Recommended, Self::LongTerm, Self::ShortTerm];
    /// The words the config key and the session word spell them with, in [`Self::ALL`]'s order.
    pub const WORDS: [&'static str; 3] = ["recommended", "long-term", "short-term"];

    /// The priority `word` names, or `None` for any other text.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        let word = word.trim();
        Self::ALL
            .into_iter()
            .find(|p| p.word().eq_ignore_ascii_case(word))
    }

    /// Its word: `recommended`, `long-term`, `short-term`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Recommended => Self::WORDS[0],
            Self::LongTerm => Self::WORDS[1],
            Self::ShortTerm => Self::WORDS[2],
        }
    }

    /// The rule a question dialog answered under it is ledgered as.
    #[must_use]
    pub const fn rule_id(self) -> &'static str {
        match self {
            Self::Recommended => super::question::RULE_ANSWER_RECOMMENDED,
            Self::LongTerm => RULE_ANSWER_LONG_TERM,
            Self::ShortTerm => RULE_ANSWER_SHORT_TERM,
        }
    }

    /// The rule a prose question answered under it is ledgered as.
    #[must_use]
    pub const fn prose_rule_id(self) -> &'static str {
        match self {
            Self::Recommended => super::turn_end::RULE_ANSWER,
            Self::LongTerm => RULE_PROSE_LONG_TERM,
            Self::ShortTerm => RULE_PROSE_SHORT_TERM,
        }
    }

    /// The fixed policy text written into a free-text row (and used for prose).
    #[must_use]
    pub const fn text(self) -> Option<&'static str> {
        match self {
            Self::Recommended => None,
            Self::LongTerm => Some(super::question::LONG_TERM_TEXT),
            Self::ShortTerm => Some(super::question::SHORT_TERM_TEXT),
        }
    }
}

/// What every decline's text opens with ([`decline_text`]).
pub const DECLINE_PREFIX: &str = "aterm harness (not the user): ";

/// The refusal a decline amends.
pub const REFUSAL_LABEL: &str = "No";

const DECLINE_WHY_CHARS: usize = 160;

/// How the approving option is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Digit(u8),
    Focus {
        steps: i32,
        label: String,
    },
    Answer(Answer),
    FocusKey {
        steps: i32,
        label: String,
        key: &'static str,
    },
    Note {
        text: QuestionPriority,
    },
}

/// How Claude Code's question dialog is answered ([`Choice::Answer`]; Codex's question takes a
/// digit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    FocusEnter { steps: i32, target: AnswerTarget },
    Type { steps: i32, text: QuestionPriority },
}

impl Answer {
    /// Where the answer's Enter goes.
    #[must_use]
    pub const fn enter_target(&self) -> Option<AnswerTarget> {
        match self {
            Self::FocusEnter { target, .. } => Some(*target),
            Self::Type { .. } => None,
        }
    }

    /// The priority whose policy text the answer writes.
    #[must_use]
    pub const fn typed(&self) -> Option<QuestionPriority> {
        match self {
            Self::FocusEnter { .. } => None,
            Self::Type { text, .. } => Some(*text),
        }
    }

    /// The rows the focus is moved first (down when positive).
    #[must_use]
    pub const fn steps(&self) -> i32 {
        match self {
            Self::FocusEnter { steps, .. } | Self::Type { steps, .. } => *steps,
        }
    }
}

/// Where a question answer's Enter goes ([`Answer::FocusEnter`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerTarget {
    Option(u8),
    Button,
    ReviewSubmit,
    OwnText(QuestionPriority),
}

/// What to do with one box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Approve {
        rule_id: &'static str,
        choice: Choice,
        guard: String,
        subject: String,
        unproven: Option<String>,
    },
    Decline {
        rule_id: &'static str,
        refusal: usize,
        text: String,
        subject: String,
    },
    Escalate {
        reason: String,
    },
}

impl Decision {
    pub(super) fn escalate(reason: impl Into<String>) -> Self {
        Self::Escalate {
            reason: reason.into(),
        }
    }

    /// The rule that approved or declined (`None` for an escalation).
    #[must_use]
    pub const fn rule_id(&self) -> Option<&'static str> {
        match self {
            Self::Approve { rule_id, .. } | Self::Decline { rule_id, .. } => Some(rule_id),
            Self::Escalate { .. } => None,
        }
    }
}

/// A path a Read box may not be approved for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRule {
    Under(PathBuf),
    Component(String),
    Basename(String),
}

/// The default secrets list.
pub fn default_secrets(home: Option<&Path>) -> Vec<SecretRule> {
    let mut out: Vec<SecretRule> = [
        ".ssh",
        ".aws",
        ".gnupg",
        ".kube",
        ".docker",
        ".password-store",
        "Keychains",
        "gcloud",
        ".azure",
    ]
    .iter()
    .map(|c| SecretRule::Component(c.to_string()))
    .collect();
    out.extend(
        [
            "*.pem",
            "*.key",
            "*.p12",
            "*.pfx",
            "*.token",
            ".env",
            ".env.*",
            ".envrc",
            "id_rsa*",
            "id_dsa*",
            "id_ecdsa*",
            "id_ed25519*",
            ".netrc",
            ".pgpass",
            ".git-credentials",
            ".credentials.json",
            "credentials*",
            "*_history",
            ".npmrc",
            ".pypirc",
            ".vault-token",
        ]
        .iter()
        .map(|g| SecretRule::Basename(g.to_string())),
    );
    if let Some(h) = home {
        // Joined with `/`, which [`path_readings`] reads in a Unix home and a
        // Windows one (`C:\Users\name/.secrets`) alike.
        let h = h.to_string_lossy();
        let h = h.trim_end_matches('/');
        for sub in [
            ".config/gh",
            ".claude.json",
            "Library",
            ".cargo/credentials",
            ".secrets",
            ".config/aterm/secrets",
        ] {
            out.push(SecretRule::Under(PathBuf::from(format!("{h}/{sub}"))));
        }
    }
    out
}

/// Roots from config text (`[harness] trust_roots`).
pub fn roots_from_config(specs: &[String], home: Option<&Path>) -> Vec<ScratchRoot> {
    let mut out = Vec::new();
    for spec in specs {
        let spec = spec.trim().trim_end_matches('/');
        let full = if spec == "~" {
            match home {
                Some(h) => h.to_string_lossy().to_string(),
                None => continue,
            }
        } else if let Some(rest) = spec.strip_prefix("~/") {
            match home {
                Some(h) => format!("{}/{rest}", h.to_string_lossy().trim_end_matches('/')),
                None => continue,
            }
        } else {
            spec.to_string()
        };
        let (parent, last) = match full.rsplit_once('/') {
            Some((p, l)) => (if p.is_empty() { "/" } else { p }, l),
            None => continue,
        };
        if parent.contains(['*', '?', '[']) || last.contains(['?', '[']) {
            continue;
        }
        let root = match last.find('*') {
            None => ScratchRoot::dir(Path::new(&full)),
            Some(at) if at + 1 == last.len() && at > 0 => {
                ScratchRoot::glob_under(Path::new(parent), last)
            }
            Some(_) => None,
        };
        out.extend(root);
    }
    out
}

/// Everything [`decide`] knows besides the box.
#[derive(Debug, Clone)]
pub struct ApprovalCtx {
    pub cwd: PathBuf,
    pub cwd_known: bool,
    pub home: Option<PathBuf>,
    pub bypass_mode: bool,
    pub approve: Approve,
    pub answer_questions: bool,
    pub model_fallback: bool,
    pub limit_wait: bool,
    pub limit_reset: bool,
    pub reset_kinds: Vec<aterm_phase::ResetKind>,
    pub usage_credits: bool,
    pub autocompact_offer: Option<u32>,
    pub login_carry: bool,
    /// The picker is the `/login` aterm typed over a login that came back in another tab: the
    /// loop backs out of it ([`super::login::PickerGate::Back`]).
    pub login_back: bool,
    /// The picker is the `/login` aterm typed over a roster row's login, which aterm's sign-in
    /// ladder signs in: the loop backs out of it ([`super::login::PickerGate::Roster`]).
    pub login_roster: bool,
    /// The picker is the `/login` aterm typed over a login a renewal the tab follows replaced:
    /// the loop backs out of it ([`super::login::PickerGate::Renewed`]).
    pub login_renewed: bool,
    pub login_method: Option<aterm_phase::LoginMethod>,
    pub trust_roots: Vec<ScratchRoot>,
    pub trusted_cwd: Option<PathBuf>,
    pub worker_agent: Option<Program>,
    pub read_roots: Vec<ScratchRoot>,
    pub scratch_roots: Vec<ScratchRoot>,
    pub rm_breaker_warn: bool,
    pub secrets: Vec<SecretRule>,
    pub python_allow: Vec<String>,
    pub question_priority: QuestionPriority,
    pub question_own: Option<QuestionPriority>,
    pub screen_cols: u16,
    pub screen_rows: usize,
    pub question_fell_back: bool,
    pub worker: Result<WorkerEnv, String>,
    pub shell_cwds: Vec<PathBuf>,
    pub shell: Result<ShellStartup, String>,
    pub owner_asks: Result<Vec<OwnerAsk>, String>,
    pub probe: Arc<dyn PolicyProbe>,
    pub box_call: Option<String>,
}

impl ApprovalCtx {
    /// Defaults for a session in `cwd` owned by `uid`.
    pub fn new(
        cwd: PathBuf,
        home: Option<PathBuf>,
        uid: u32,
        tmpdir: Option<PathBuf>,
        probe: Arc<dyn PolicyProbe>,
    ) -> Self {
        let claude_tmp = PathBuf::from(format!("/private/tmp/claude-{uid}"));
        let mut scratch_roots: Vec<ScratchRoot> = Vec::new();
        scratch_roots.extend(ScratchRoot::dir(&claude_tmp));
        if let Some(t) = tmpdir.as_deref()
            && let Some(comps) = abs_components(&t.to_string_lossy())
        {
            let holds = |p: Option<&Path>| {
                p.and_then(|p| abs_components(&p.to_string_lossy()))
                    .is_some_and(|q| q.starts_with(&comps))
            };
            if comps.len() >= 2 && !holds(Some(&cwd)) && !holds(home.as_deref()) {
                scratch_roots.extend(ScratchRoot::dir(t));
            }
        }
        scratch_roots.extend(ScratchRoot::children_of(Path::new("/tmp")));
        scratch_roots.extend(ScratchRoot::children_of(Path::new("/private/tmp")));
        scratch_roots.extend(ScratchRoot::glob_under(&cwd, "target*"));
        let defaults = SupervisorConfig::default();
        let mut ctx = Self {
            secrets: default_secrets(home.as_deref()),
            cwd,
            cwd_known: true,
            home,
            bypass_mode: false,
            approve: defaults.approve,
            answer_questions: defaults.answer_questions,
            model_fallback: defaults.model_fallback.is_some(),
            limit_wait: defaults.limit_wait,
            limit_reset: defaults.limit_reset,
            reset_kinds: defaults.limit_reset_kinds,
            usage_credits: defaults.usage_credits,
            autocompact_offer: defaults.autocompact_offer,
            login_carry: false,
            login_back: false,
            login_roster: false,
            login_renewed: false,
            login_method: None,
            trust_roots: Vec::new(),
            trusted_cwd: None,
            worker_agent: None,
            read_roots: Vec::new(),
            scratch_roots,
            rm_breaker_warn: !defaults.rm_breaker_ask,
            python_allow: Vec::new(),
            question_priority: defaults.question_priority,
            question_own: None,
            screen_cols: 0,
            screen_rows: 0,
            question_fell_back: false,
            worker: worker_unread(),
            shell_cwds: Vec::new(),
            shell: shell_unread(),
            owner_asks: asks_unread(),
            probe,
            box_call: None,
        };
        ctx.set_trust_roots(&defaults.trust_roots, uid);
        ctx
    }

    /// The trust roots from `[harness] trust_roots` ([`roots_from_config`]), and the Read roots
    /// rebuilt around them.
    pub fn set_trust_roots(&mut self, specs: &[String], uid: u32) {
        let specs: Vec<String> = specs.iter().map(|s| own_claude_tmp(s, uid)).collect();
        self.trust_roots = roots_from_config(&specs, self.home.as_deref());
        self.read_roots = self.trust_roots.clone();
        for sys in ["/usr", "/etc", "/private/etc", "/opt/homebrew"] {
            self.read_roots.extend(ScratchRoot::dir(Path::new(sys)));
        }
    }
}

fn worker_unread() -> Result<WorkerEnv, String> {
    #[cfg(test)]
    {
        Ok(WorkerEnv::hermetic(None))
    }
    #[cfg(not(test))]
    {
        Err("the worker's environment was not read".to_string())
    }
}

fn shell_unread() -> Result<ShellStartup, String> {
    #[cfg(test)]
    {
        Ok(ShellStartup::default())
    }
    #[cfg(not(test))]
    {
        Err("the Bash tool's shell startup was not read".to_string())
    }
}

fn asks_unread() -> Result<Vec<OwnerAsk>, String> {
    #[cfg(test)]
    {
        Ok(Vec::new())
    }
    #[cfg(not(test))]
    {
        Err("they were not read".to_string())
    }
}

fn shell_models_hold(
    readings: &[String],
    runs_on: Option<&str>,
    ctx: &ApprovalCtx,
) -> Result<(), String> {
    if let Some(machine) = runs_on {
        return Err(format!(
            "a command on {machine}: the shell startup it runs under is that machine's"
        ));
    }
    let state = ctx.shell.as_ref().map_err(|why| {
        format!("the Bash tool's shell may not start as this check reads a line: {why}")
    })?;
    for line in readings {
        state.admits(line)?;
    }
    Ok(())
}

fn own_claude_tmp(spec: &str, uid: u32) -> String {
    let t = spec.trim().trim_end_matches('/');
    for base in ["/private/tmp", "/tmp"] {
        if t == format!("{base}/claude-*") {
            return format!("{base}/claude-{uid}");
        }
    }
    spec.to_string()
}

/// The permission mode the footer under the composer names.
pub fn footer_mode(rows: &[String]) -> Option<Mode> {
    let tail = rows.iter().rev().filter(|r| !r.trim().is_empty()).take(3);
    for r in tail {
        if let Some(mode) = crate::harness::footer::pill_indicator(r).and_then(Mode::of_indicator) {
            return Some(mode);
        }
        if r.contains(aterm_phase::anchor!("footer.shortcuts")) {
            return Some(Mode::Manual);
        }
    }
    None
}

/// Decide the box `reading` found on screen `rows` under [`ApprovalCtx::approve`].
pub fn decide(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    decide_traced(reading, rows, ctx, &mut NoTrace)
}

/// [`decide`], each check it consults written to `trace` as it is consulted ([`super::trace`]).
pub fn decide_traced(
    reading: &Reading,
    rows: &[String],
    ctx: &ApprovalCtx,
    trace: &mut dyn Trace,
) -> Decision {
    let decided = decide_box(reading, rows, ctx, trace);
    let before = trace.on().then(|| decided.clone());
    let decision = never_spends(reading, rows, decided);
    if before.as_ref().is_some_and(|before| before != &decision) {
        note(trace, "never-spends", None, || {
            Outcome::Overruled(said(&decision))
        });
    }
    match reading
        .prompt
        .as_ref()
        .and_then(|p| kept_or_bought(p, &decision))
    {
        Some(why) => {
            note(trace, "press-gate", None, || {
                Outcome::Overruled(why.clone())
            });
            Decision::escalate(why)
        }
        None => decision,
    }
}

fn said(d: &Decision) -> String {
    match d {
        Decision::Approve {
            choice,
            subject,
            unproven,
            ..
        } => {
            let unproven = unproven
                .as_deref()
                .map_or_else(String::new, |u| format!(" (unproven: {u})"));
            format!("approve {choice:?}: {subject}{unproven}")
        }
        Decision::Decline { text, subject, .. } => format!("decline {subject}: {text}"),
        Decision::Escalate { reason } => format!("escalate: {reason}"),
    }
}

fn decided_at(trace: &mut dyn Trace, check: &'static str, d: Decision) -> Decision {
    note(trace, check, d.rule_id(), || Outcome::Decided(said(&d)));
    d
}

/// Whether `o`'s answer is kept for every later ask.
pub(crate) fn kept_for_good(o: &Opt) -> bool {
    o.role == Role::Persist || o.kept || settles_later(&o.label) || kept_answer(&o.label)
}

fn kept_or_bought(p: &PromptV2, decision: &Decision) -> Option<String> {
    let Decision::Approve {
        rule_id, choice, ..
    } = decision
    else {
        return None;
    };
    if p.kind == PromptKind::Question || *rule_id == RULE_LIMIT_RESET {
        return None;
    }
    let o = match choice {
        Choice::Digit(n) => p.options.iter().find(|o| o.n == Some(*n)),
        Choice::FocusKey { key: "esc", .. } => None,
        Choice::Focus { label, .. } | Choice::FocusKey { label, .. } => {
            p.options.iter().find(|o| o.label == *label)
        }
        Choice::Answer(_) | Choice::Note { .. } => None,
    }?;
    let what = if kept_for_good(o) {
        "keeps its answer for every later ask"
    } else if buys(o) {
        "buys something"
    } else {
        return None;
    };
    Some(format!(
        "`{}` {what}: no automatic press chooses a standing answer or a purchase",
        o.label
    ))
}

fn never_spends(reading: &Reading, rows: &[String], decided: Decision) -> Decision {
    let Decision::Approve {
        rule_id, choice, ..
    } = &decided
    else {
        return decided;
    };
    if *rule_id == RULE_LIMIT_RESET {
        return decided;
    }
    let options = reading.prompt.as_ref().map(|p| p.options.as_slice());
    let chosen = match choice {
        Choice::Digit(n) => options
            .and_then(|all| all.iter().find(|o| o.n == Some(*n)))
            .map(|o| o.label.as_str()),
        Choice::FocusKey { key: "esc", .. } => None,
        Choice::Focus { label, .. } | Choice::FocusKey { label, .. } => Some(label.as_str()),
        Choice::Answer(answer) => reading
            .prompt
            .as_ref()
            .and_then(|p| p.question_dialog.as_ref())
            .and_then(|dialog| match answer.enter_target()? {
                AnswerTarget::Option(n) => super::question::option_label(dialog, n),
                AnswerTarget::Button => match &dialog.form {
                    aterm_phase::QuestionForm::Multi { options, .. } => options
                        .iter()
                        .find(|o| o.checked == Some(true) && spends(&o.label))
                        .map(|o| o.label.as_str()),
                    _ => None,
                },
                AnswerTarget::ReviewSubmit | AnswerTarget::OwnText(_) => None,
            }),
        Choice::Note { .. } => None,
    };
    if let Some(label) = chosen.filter(|label| spends(label)) {
        return Decision::escalate(format!(
            "{rule_id} chose `{label}`, which spends a limit reset: never pressed by that rule"
        ));
    }
    let keeps = |label: &str| {
        options
            .and_then(|all| all.iter().find(|o| o.label == label))
            .is_some_and(keeps_reset)
    };
    match reading.prompt.as_ref() {
        Some(p) if confirms_reset(p, rows) && !chosen.is_some_and(keeps) => {
            Decision::escalate(format!(
                "{rule_id} chose `{}` on a limit reset's confirmation: nothing but its refusal \
                 is pressed by that rule",
                chosen.unwrap_or("an answer of its own")
            ))
        }
        _ => decided,
    }
}

fn confirms_reset(p: &PromptV2, rows: &[String]) -> bool {
    !matches!(
        aterm_phase::reset_confirm(p, rows),
        aterm_phase::ConfirmRead::Not
    )
}

fn keeps_reset(opt: &Opt) -> bool {
    (opt.role == Role::Deny || waits(opt)) && !buys(opt)
}

const fn draws_claude_boxes(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => false,
    }
}

fn decide_box(
    reading: &Reading,
    rows: &[String],
    ctx: &ApprovalCtx,
    trace: &mut dyn Trace,
) -> Decision {
    // A program the supervisor does not host (`aterm_phase::Program::supervisable`) is read,
    // never answered: every box of its is a person's, whatever its reader names in it.
    if !reading.program.supervisable() {
        return decided_at(
            trace,
            "supervisable",
            Decision::escalate(format!(
                "{} is read, not supervised: its boxes are a person's",
                reading.program.product()
            )),
        );
    }
    note(trace, "supervisable", None, || Outcome::Passed);
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::UsageLimit
    {
        let read = reading.phase == Phase::Prompt && reading.phase_authoritative;
        let decision = if !draws_claude_boxes(reading.program) || !read {
            Decision::escalate("no usage-limit dialog the reader vouches for")
        } else {
            limit_reset(prompt, rows, ctx).unwrap_or_else(|| limit_wait(prompt, rows, ctx))
        };
        return decided_at(trace, "usage-limit", decision);
    }
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::AutocompactOffer
    {
        let decision = if !draws_claude_boxes(reading.program)
            || reading.phase != Phase::Prompt
            || !reading.phase_authoritative
        {
            Decision::escalate("no autocompact offer the reader vouches for")
        } else {
            autocompact_offer(prompt, rows, ctx)
        };
        return decided_at(trace, "autocompact-offer", decision);
    }
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::AutoModeSetup
    {
        let decision = if !draws_claude_boxes(reading.program)
            || reading.phase != Phase::Prompt
            || !reading.phase_authoritative
        {
            Decision::escalate("no auto-mode setup offer the reader vouches for")
        } else {
            auto_mode_setup(prompt, rows)
        };
        return decided_at(trace, "auto-mode-setup", decision);
    }
    if let Some(prompt) = reading.prompt.as_ref()
        && draws_claude_boxes(reading.program)
        && let Some(screen) = aterm_phase::login_screen(rows)
    {
        let decision = match screen {
            _ if reading.phase != Phase::Prompt || !reading.phase_authoritative => {
                Decision::escalate("no login dialog the reader vouches for")
            }
            aterm_phase::LoginScreen::Picker(picker) if ctx.login_carry => {
                super::login::pick_method(prompt, &picker, ctx.login_method)
            }
            aterm_phase::LoginScreen::Picker(picker)
                if ctx.login_back || ctx.login_roster || ctx.login_renewed =>
            {
                super::login::back_out_of_picker(prompt, &picker)
            }
            aterm_phase::LoginScreen::Picker(_) => {
                Decision::escalate("a /login you opened: its sign-in method is yours to pick")
            }
            _ => Decision::escalate(
                "Claude Code's Console sign-in menu: its API key option is yours to choose",
            ),
        };
        return decided_at(trace, "login-method", decision);
    }
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::Question
    {
        let decision = if reading.phase != Phase::Prompt || !reading.phase_authoritative {
            Decision::escalate("no box the reader vouches for")
        } else if ctx.answer_questions {
            let seen = super::question::Pane::of(rows);
            let pane = super::question::Pane {
                cols: if ctx.screen_cols > 0 {
                    ctx.screen_cols
                } else {
                    seen.cols
                },
                rows: if ctx.screen_rows > 0 {
                    ctx.screen_rows
                } else {
                    seen.rows
                },
            };
            super::question::answer_question(
                prompt,
                rows,
                ctx.question_priority,
                ctx.question_own,
                ctx.question_fell_back,
                pane,
            )
        } else {
            Decision::escalate("answer_questions is off: a question is the person's to answer")
        };
        return decided_at(trace, "question", decision);
    }
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::McpServer
    {
        return decided_at(
            trace,
            "mcp-server",
            Decision::escalate(mcp_server_held(prompt)),
        );
    }
    if let Some(review) = reading.prompt.as_ref().and_then(|p| p.owner_review()) {
        return decided_at(
            trace,
            "owner-review",
            Decision::escalate(format!(
                "the owner's Claude Code settings send this box to a person ({}): {}",
                review.kind.words(),
                review.text
            )),
        );
    }
    note(trace, "owner-review", None, || Outcome::Passed);
    if let Some(why) = settings_ask(reading, rows, ctx) {
        return decided_at(trace, "settings-ask", Decision::escalate(why));
    }
    note(trace, "settings-ask", None, || Outcome::Passed);
    if let Some(why) = forced_read(reading) {
        return decided_at(trace, "forced-read", Decision::escalate(why));
    }
    note(trace, "forced-read", None, || Outcome::Passed);
    if let Some(why) = head_unread(reading) {
        let decision = match ctx.approve {
            Approve::All | Approve::Safe => tall_box(reading, rows, ctx, why),
            Approve::None => Decision::escalate(why),
        };
        return decided_at(trace, "head-unread", decision);
    }
    note(trace, "head-unread", None, || Outcome::Passed);
    match ctx.approve {
        Approve::None => decided_at(
            trace,
            "approve-none",
            Decision::escalate("approve = \"none\": every box is the owner's"),
        ),
        Approve::Safe => match decide_safe(reading, rows, ctx) {
            Decision::Escalate { reason } if ctx.rm_breaker_warn => {
                note(trace, "safe-rules", None, || {
                    Outcome::Unproven(reason.clone())
                });
                decided_at(
                    trace,
                    "rm-breaker-warn",
                    rm_breaker_warn(reading, rows, reason),
                )
            }
            decided => decided_at(trace, "safe-rules", decided),
        },
        Approve::All => match decide_safe(reading, rows, ctx) {
            answered @ (Decision::Approve { .. } | Decision::Decline { .. }) => {
                decided_at(trace, "safe-rules", answered)
            }
            Decision::Escalate { reason } => {
                note(trace, "safe-rules", None, || {
                    Outcome::Unproven(reason.clone())
                });
                decided_at(trace, "full-power", full_power(reading, rows, ctx, reason))
            }
        },
    }
}

/// Every Claude Code tool a box `p` may be a call of, where Claude Code draws no ask reason on a
/// box of its kind (a file box).
#[must_use]
pub fn unvoiced_tools(p: &PromptV2) -> &'static [&'static str] {
    let title = p.title.trim_start();
    match p.kind {
        PromptKind::Write | PromptKind::Overwrite => &["Write"],
        PromptKind::Read => &["Read", "Glob", "Grep"],
        PromptKind::Edit if title.starts_with(aterm_phase::anchor!("box.edit")) => {
            &["Edit", "Bash"]
        }
        PromptKind::Edit if title.starts_with(aterm_phase::anchor!("box.edit_notebook")) => {
            &["NotebookEdit"]
        }
        PromptKind::Edit => &["Edit", "Write", "NotebookEdit", "Bash"],
        _ => &[],
    }
}

/// Whether a transcript candidate could change the actual decision.
#[must_use]
pub fn proof_would_lift(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> bool {
    let Some(p) = reading.prompt.as_ref() else {
        return false;
    };
    let unnamed = ApprovalCtx {
        box_call: None,
        ..ctx.clone()
    };
    settings_ask(reading, rows, &unnamed).is_some()
        && unvoiced_tools(p)
            .iter()
            .filter(|t| super::box_call::PROVABLE.contains(t))
            .any(|t| {
                let proved = ApprovalCtx {
                    box_call: Some((*t).to_string()),
                    ..ctx.clone()
                };
                !matches!(decide(reading, rows, &proved), Decision::Escalate { .. })
            })
}

fn settings_ask(reading: &Reading, _rows: &[String], ctx: &ApprovalCtx) -> Option<String> {
    let p = reading.prompt.as_ref()?;
    let tools = unvoiced_tools(p);
    if !draws_claude_boxes(reading.program) || tools.is_empty() {
        return None;
    }
    let asks = match &ctx.owner_asks {
        Ok(asks) => asks,
        Err(why) => {
            return Some(format!(
                "{} box the agent's Claude Code settings may send to a person, and they are \
                 unknown: {why}",
                a_or_an(p.kind.name())
            ));
        }
    };
    if let Some(a) = asks
        .iter()
        .find(|a| tools.iter().any(|tool| a.asks_every(tool)))
    {
        return Some(format!(
            "the owner's Claude Code settings send this box to a person (an ask rule): `{}` in \
             {}",
            a.text, a.source
        ));
    }
    if !tools.contains(&"Bash") {
        return None;
    }
    asks.iter().find(|a| a.may_ask("Bash")).map(|a| {
        let (what, named) = match a.kind {
            AskKind::Rule { .. } => ("a Bash ask rule", format!("`{}`", a.text)),
            AskKind::Hook { .. } => (
                "a PreToolUse hook that can match Bash",
                format!("matcher `{}`", a.text),
            ),
        };
        format!(
            "the owner's Claude Code settings may send this box to a person ({what}; Claude \
             Code draws a Bash `sed -i` as this box with no reason): {named} in {}",
            a.source
        )
    })
}

fn forced_read(reading: &Reading) -> Option<String> {
    let p = reading.prompt.as_ref()?;
    let root_level = p.path.as_deref().is_some_and(|path| {
        path.strip_prefix('/')
            .is_some_and(|rest| !rest.contains('/'))
    });
    if !draws_claude_boxes(reading.program)
        || p.kind != PromptKind::Read
        || p.runs_on().is_some()
        || root_level
        || p.options
            .iter()
            .any(|o| o.label.contains(aterm_phase::anchor!("box.read_grant")))
    {
        return None;
    }
    let path = p.path.as_deref().unwrap_or("?");
    if read_readings(path).any(vendor_distrusts) {
        return Some(format!(
            "a Read box with no folder grant (Claude Code asks about this name itself): {path}"
        ));
    }
    let folder = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let has_private = ["/tmp", "/var", "/etc", "/private"].iter().any(|top| {
        folder
            .strip_prefix(top)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    });
    let sure = if has_private {
        GRANT_FOLDER_SURE
    } else {
        GRANT_TEXT_MAX
    };
    if folder.chars().count() > sure {
        return Some(format!(
            "a Read box with no folder grant (an ask rule, hook or plugin sent it, or its folder \
             is too long for one): {path}"
        ));
    }
    Some(format!(
        "a Read box with no folder grant (an ask rule, hook or plugin sent it): {path}"
    ))
}

const GRANT_TEXT_MAX: usize = 160;

const GRANT_FOLDER_SURE: usize = (GRANT_TEXT_MAX - " and /private".len()) / 2;

fn vendor_distrusts(path: &str) -> bool {
    let tilde_digit = path
        .as_bytes()
        .windows(2)
        .any(|w| w[0] == b'~' && w[1].is_ascii_digit());
    let odd_component = path.split('/').any(|c| {
        c == "..."
            || (!matches!(c, "" | "." | "..")
                && (c.ends_with('.') || c.ends_with(char::is_whitespace)))
    });
    let device = path.rsplit_once('.').is_some_and(|(_, ext)| {
        let ext = ext.to_ascii_uppercase();
        matches!(ext.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ext
                .strip_prefix("COM")
                .or_else(|| ext.strip_prefix("LPT"))
                .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
    });
    tilde_digit || odd_component || device
}

/// What full power's ledger subject puts between a box and the option it chose.
pub const SUBJECT_OPTION: &str = " => ";

/// Audit reason recorded when an `rm` circuit-breaker warning box is approved under `safe`.
pub const WARNED_UNDER_SAFE: &str = "approved with this warning under approve = \"safe\"";

fn rm_breaker_warn(reading: &Reading, rows: &[String], safe: String) -> Decision {
    if !draws_claude_boxes(reading.program)
        || reading.phase != Phase::Prompt
        || !reading.phase_authoritative
    {
        return Decision::escalate(safe);
    }
    let Some(p) = reading
        .prompt
        .as_ref()
        .filter(|p| p.kind == PromptKind::Bash)
    else {
        return Decision::escalate(safe);
    };
    let Some(breaker) = p.rm_breaker() else {
        return Decision::escalate(safe);
    };
    if !warnable_breaker(&breaker) || p.runs_on().is_some() {
        return Decision::escalate(safe);
    }
    let once: Vec<&Opt> = p.options.iter().filter(|o| o.role == Role::Once).collect();
    let [yes] = once.as_slice() else {
        return Decision::escalate(safe);
    };
    if buys(yes) || p.options.len() != 2 || p.with_role(Role::Deny).is_none() {
        return Decision::escalate(safe);
    }
    let why = format!(
        "{} \u{2014} {WARNED_UNDER_SAFE} ([harness] rm_breaker_ask = true hands it to a \
         person)",
        name_the_breaker(&breaker, safe.clone())
    );
    match answer(p, rows, Ok((RULE_RM_BREAKER_WARN, *yes)), safe) {
        Decision::Approve {
            rule_id,
            choice,
            guard,
            subject,
            unproven: _,
        } => Decision::Approve {
            rule_id,
            choice,
            guard,
            subject,
            unproven: Some(why),
        },
        other => other,
    }
}

fn head_unread(reading: &Reading) -> Option<String> {
    let p = reading.prompt.as_ref()?;
    let why = if p.head_off_screen {
        "a box taller than the pane"
    } else if p.kind == PromptKind::Other && asks(&p.title) {
        "its first row is its question"
    } else {
        return None;
    };
    Some(format!(
        "the box's title row is not on the screen ({why}): {}; {UNREAD}",
        p.title
    ))
}

const UNREAD: &str = "nothing is pressed on a box not read whole: it is decided again once the \
                      pane shows it whole";

const TALL_WHY: &str =
    "it is taller than the screen, so the harness cannot read all of it to check it";

const TALL_RM_PATHS: &str = "its paths written as literal absolute paths (a command run on its \
                             own keeps none of this one's shell variables)";

fn tall_box(reading: &Reading, rows: &[String], ctx: &ApprovalCtx, why: String) -> Decision {
    let handed_over = |not: Option<String>| {
        Decision::escalate(match not {
            Some(not) => format!("{why}; not declined: {not}"),
            None => why.clone(),
        })
    };
    let Some(prompt) = reading.prompt.as_ref() else {
        return handed_over(None);
    };
    if !draws_claude_boxes(reading.program)
        || reading.phase != Phase::Prompt
        || !reading.phase_authoritative
        || !ctx.bypass_mode
        || prompt.kind == PromptKind::Question
    {
        return handed_over(None);
    }
    let Some(note) = prompt
        .foot_note(rows)
        .filter(|n| rm_breaker_of(n).is_some())
    else {
        return handed_over(None);
    };
    if ctx.scratch_roots.is_empty() {
        return handed_over(Some("the rm rule has no scratch root".to_string()));
    }
    if let Err(not) = yes_no_to_proceed(prompt, rows) {
        return handed_over(Some(not));
    }
    let breaker = rm_breaker_of(&note);
    let in_script = breaker.as_ref().is_some_and(|b| {
        matches!(
            b.kind,
            RmBreakerKind::ShellScriptUnchecked | RmBreakerKind::ShellScriptValue
        )
    });
    let fix = if in_script {
        format!(
            "Run the removal on its own as a short command, outside any shell -c script, \
             {TALL_RM_PATHS}"
        )
    } else if breaker
        .as_ref()
        .is_some_and(|b| b.kind == RmBreakerKind::NotRuledOut)
    {
        format!(
            "Split it into shorter, plainer commands, run one at a time, any removal among them \
             on its own, {TALL_RM_PATHS}"
        )
    } else if breaker.as_ref().is_some_and(breaker_not_ruled_out) {
        format!(
            "Split it into shorter commands with fewer substitutions, run one at a time, any \
             removal among them on its own, {TALL_RM_PATHS}"
        )
    } else {
        format!(
            "Run {} on its own as a short command, separate from the rest, {TALL_RM_PATHS}",
            quoted_invocation(&note).map_or_else(
                || "the removal".to_string(),
                |inv| format!("`{}`", clip(inv, DECLINE_WHY_CHARS))
            )
        )
    };
    let text = decline_text("this command", TALL_WHY, &fix);
    let subject = format!("a box taller than the screen, its note: {note}");
    decline(RULE_TALL_BOX, prompt, text, subject).unwrap_or_else(|not| handed_over(Some(not)))
}

fn yes_no_to_proceed(prompt: &PromptV2, rows: &[String]) -> Result<(), String> {
    let proceed = aterm_phase::anchor!("prompt.proceed");
    if !prompt
        .question
        .and_then(|q| rows.get(q))
        .is_some_and(|r| r.trim() == proceed)
    {
        return Err(format!("its foot does not ask `{proceed}`"));
    }
    let roles: Vec<Role> = prompt.options.iter().map(|o| o.role).collect();
    if roles != [Role::Once, Role::Deny] {
        return Err(format!(
            "its options are not a one-shot allow and a refusal ({})",
            roles.iter().map(|r| r.name()).collect::<Vec<_>>().join(",")
        ));
    }
    Ok(())
}

fn quoted_invocation(note: &str) -> Option<&str> {
    let (_, rest) = note.split_once('`')?;
    let (inv, after) = rest.split_once('`')?;
    (!inv.trim().is_empty() && !after.contains('`')).then_some(inv.trim())
}

fn decide_safe(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if !draws_claude_boxes(reading.program) {
        if let Some(prompt) = reading.prompt.as_ref()
            && prompt.kind == PromptKind::Trust
            && reading.phase == Phase::Prompt
            && reading.phase_authoritative
            && reading.program.supervisable()
        {
            return spawned_trust(reading.program, prompt, rows, ctx);
        }
        return Decision::escalate(format!(
            "{} session's box: the safe rules judge Claude Code's boxes",
            a_or_an(reading.program.name())
        ));
    }
    if reading.phase != Phase::Prompt || !reading.phase_authoritative {
        return Decision::escalate("no box the reader vouches for");
    }
    let Some(prompt) = reading.prompt.as_ref() else {
        return Decision::escalate("no box on the screen");
    };
    match prompt.kind {
        PromptKind::Bash => bash(prompt, rows, ctx),
        PromptKind::Read => read_box(prompt, rows, ctx),
        PromptKind::Trust => trust_dialog(prompt, rows, ctx),
        kind => Decision::escalate(format!(
            "{} box: no rule approves this kind",
            a_or_an(kind.name())
        )),
    }
}

/// Read `rows` with the reader for `program` ([`aterm_phase::read`]) and [`decide`] the box on
/// them.
pub fn decide_screen(
    program: Option<&str>,
    rows: &[String],
    ctx: &ApprovalCtx,
) -> Option<Decision> {
    let reading = aterm_phase::read(program, rows, None);
    reading.prompt.as_ref()?;
    Some(decide(&reading, rows, ctx))
}

fn row_of(rows: &[String], prompt: &PromptV2, text: &str) -> Option<usize> {
    let (top, bottom) = prompt.span;
    (top + 1..bottom.min(rows.len())).find(|&k| {
        let t = rows[k].trim();
        t.strip_prefix('│').map_or(t, str::trim) == text
    })
}

fn file_path_row(rows: &[String], prompt: &PromptV2, path: &str) -> Option<usize> {
    row_of(rows, prompt, path)
        .or_else(|| {
            row_of(
                rows,
                prompt,
                &format!("{}{path})", aterm_phase::anchor!("box.read_call")),
            )
        })
        .or_else(|| {
            row_of(
                rows,
                prompt,
                &format!("{}{path}", aterm_phase::anchor!("muse.box.write_path")),
            )
        })
}

fn once_choice(prompt: &PromptV2) -> Result<Choice, String> {
    if prompt.select != Select::Digits {
        return Err("an unnumbered box".to_string());
    }
    if amend_input_open(prompt) {
        return Err(OPEN_INPUT.to_string());
    }
    let once: Vec<_> = prompt
        .options
        .iter()
        .filter(|o| o.role == Role::Once)
        .collect();
    let [opt] = once.as_slice() else {
        return Err(format!(
            "no single one-shot allow among the options ({}): only `Yes` is pressed, never a \
             session, persist or mode-switch grant",
            prompt
                .options
                .iter()
                .map(|o| o.role.name())
                .collect::<Vec<_>>()
                .join(",")
        ));
    };
    if !prompt.options.iter().any(|o| o.role == Role::Deny) {
        return Err("the box offers no `No`".to_string());
    }
    opt.n
        .map(Choice::Digit)
        .ok_or_else(|| "the one-shot allow has no number".to_string())
}

const OPEN_INPUT: &str =
    "the focus is on an open `No, and …` input: a digit would be typed into it";

fn amend_input_open(prompt: &PromptV2) -> bool {
    let amended = format!("{REFUSAL_LABEL}, ");
    let footer_alone = prompt
        .footer
        .as_deref()
        .is_some_and(|f| aterm_phase::footer_keys(f) == [aterm_phase::anchor!("prompt.cancel")]);
    footer_alone
        && prompt
            .options
            .iter()
            .any(|o| o.focused && o.role == Role::Deny && o.label.starts_with(&amended))
}

fn focus_steps(from: usize, to: usize) -> i32 {
    i32::try_from(to).unwrap_or(0) - i32::try_from(from).unwrap_or(0)
}

fn is_option_row(t: &str) -> bool {
    let t = t.strip_prefix('❯').map_or(t, str::trim_start);
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    (1..=2).contains(&digits) && t[digits..].starts_with(". ")
}

fn no_option_above_the_question(prompt: &PromptV2, rows: &[String]) -> Result<(), String> {
    let (top, bottom) = prompt.span;
    let bottom = bottom.min(rows.len());
    let Some(q) = (top + 1..bottom).rev().find(|&k| asks(&rows[k])) else {
        return Err("a box with no question row".to_string());
    };
    if (top + 1..q).any(|k| is_option_row(rows[k].trim())) {
        return Err("an option-shaped row above the box's question".to_string());
    }
    Ok(())
}

fn box_width(prompt: &PromptV2, rows: &[String]) -> Option<usize> {
    let rule = rows.get(prompt.span.0.checked_sub(1)?)?;
    let t = rule.trim();
    (!t.is_empty() && t.chars().all(|c| c == '─')).then(|| rule.trim_end().chars().count())
}

fn cells_are_chars(row: &str) -> bool {
    row.bytes().all(|b| (0x20..0x7f).contains(&b))
}

fn wrap_margin(width: usize) -> usize {
    (width / 4).max(8)
}

fn command_readings(prompt: &PromptV2, rows: &[String], rm_breaker: bool) -> (Vec<String>, String) {
    let all = prompt.readings();
    let n = prompt.command_rows.len();
    let shown = n.saturating_sub(prompt.description_rows).max(1);
    let shown_text =
        prompt.command_rows[..shown.min(n)].join(if prompt.gutter { "\n" } else { " " });
    let measurable = rm_breaker
        && !prompt.gutter
        && n > 1
        && prompt.command_rows.iter().all(|r| cells_are_chars(r));
    let Some(width) = box_width(prompt, rows).filter(|_| measurable) else {
        return (all, shown_text);
    };
    let margin = wrap_margin(width);
    let mut extent = n;
    for k in 0..n - 1 {
        let Some(at) = row_of(rows, prompt, &prompt.command_rows[k]) else {
            return (all, shown_text);
        };
        if !cells_are_chars(&rows[at]) {
            return (all, shown_text);
        }
        let ends = rows[at].trim_end().len();
        let next_word = prompt.command_rows[k + 1]
            .split_whitespace()
            .next()
            .map_or(0, str::len);
        if ends + 1 + next_word + margin <= width {
            extent = k + 1;
            break;
        }
    }
    let head = prompt.command_rows[..extent].join(" ");
    let mut out = vec![prompt.command_rows[..1].join(" ")];
    if !out.contains(&head) {
        out.push(head.clone());
    }
    (out, head)
}

fn bash(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    // Every header form aterm-phase reads: an origin, `(unsandboxed)`,
    // `(runs on <m>)`. An unreadable suffix is no Bash header.
    if prompt.base_title().as_deref() != Some(aterm_phase::anchor!("box.bash")) {
        return Decision::escalate(format!(
            "the box header is not a Bash header: {}",
            prompt.title
        ));
    }
    let Some(first) = prompt.command_rows.first() else {
        return Decision::escalate("a Bash box with no command row");
    };
    let Some(first_row) = row_of(rows, prompt, first) else {
        return Decision::escalate("the command's first row is not on the screen");
    };
    if let Err(why) = no_option_above_the_question(prompt, rows) {
        return Decision::escalate(why);
    }
    let breaker = prompt.rm_breaker();
    if let Some(b) = breaker.as_ref().filter(|b| breaker_not_ruled_out(b)) {
        return breaker_read_only(prompt, rows, b, &rows[first_row], ctx);
    }
    if let Some(Err(why)) = breaker.as_ref().map(resolvable_breaker) {
        return Decision::escalate(why);
    }
    let rm_breaker = breaker.is_some();
    let (readings, subject) = command_readings(prompt, rows, rm_breaker);
    let guard = row_guard(&rows[first_row]);
    if rm_breaker {
        // The scratch-root proof is this machine's, so an rm running on another
        // machine is not proven by it.
        if let Some(machine) = prompt.runs_on() {
            return Decision::escalate(format!(
                "the rm circuit breaker on {machine}: its targets resolve on that machine, \
                 not this one"
            ));
        }
        if !ctx.bypass_mode {
            return Decision::escalate("the rm circuit breaker outside a bypass session");
        }
        if prompt.options.len() != 2 {
            return Decision::escalate("an rm circuit breaker with other than Yes/No");
        }
        if !ctx.cwd_known {
            return Decision::escalate(
                "the rm circuit breaker with the session's cwd unknown (meta cwd=-)",
            );
        }
        let scope = RmScope {
            cwd: &ctx.cwd,
            home: ctx.home.as_deref(),
            roots: &ctx.scratch_roots,
            probe: &*ctx.probe,
        };
        let mut targets = Vec::new();
        for line in &readings {
            match resolve_rm_line(line, &scope) {
                Ok(t) => targets = t,
                Err(why) => {
                    return Decision::escalate(format!(
                        "rm circuit breaker ({}): {why}",
                        reading_name(line)
                    ));
                }
            }
            let rest = classify_except_rm(line, &ctx.python_allow);
            if !rest.read_only {
                return Decision::escalate(format!(
                    "rm circuit breaker ({}): the rest of the line is not read-only: {}",
                    reading_name(line),
                    rest.reason
                ));
            }
        }
        if let Err(why) = network_free(&readings).and_then(|()| brace_free(&readings)) {
            return Decision::escalate(format!("rm circuit breaker: {why}"));
        }
        if let Err(why) = blanks_shown(&readings) {
            return Decision::escalate(format!("rm circuit breaker: {why}"));
        }
        if let Err(why) = shell_models_hold(&readings, None, ctx) {
            return Decision::escalate(format!("rm circuit breaker: {why}"));
        }
        if let Err(why) = git_reads_clear(&readings, None, ctx) {
            return Decision::escalate(format!("rm circuit breaker: {why}"));
        }
        if let Err(why) = secrets_free(&readings, &ctx.secrets, ctx.home.as_deref()) {
            return Decision::escalate(format!("rm circuit breaker: {why}"));
        }
        return match once_choice(prompt) {
            Ok(choice) => Decision::Approve {
                rule_id: RULE_RM_BREAKER,
                choice,
                guard,
                subject: format!("{subject} => {}", targets.join(" ")),
                unproven: None,
            },
            Err(why) => Decision::escalate(why),
        };
    }
    let rule_id = match prompt.notes.as_slice() {
        [] => RULE_READ_ONLY,
        [note] if note == aterm_phase::anchor!("box.note_no_rule") => RULE_NO_RULE_READ_ONLY,
        [note, ..] => {
            return Decision::escalate(format!("the box carries a vendor note: {note}"));
        }
    };
    if ctx.bypass_mode {
        return Decision::escalate("a box in a bypass session is a vendor circuit breaker");
    }
    let runs_on = prompt.runs_on();
    let proven = readings_proven(&readings, &ctx.python_allow)
        .and_then(|()| shell_models_hold(&readings, runs_on.as_deref(), ctx))
        .and_then(|()| plain_under_zsh(&readings, ctx))
        .and_then(|()| git_reads_clear(&readings, runs_on.as_deref(), ctx))
        .and_then(|()| secrets_free(&readings, &ctx.secrets, ctx.home.as_deref()))
        .and_then(|()| once_choice(prompt));
    match proven {
        Ok(choice) => Decision::Approve {
            rule_id,
            choice,
            guard,
            subject,
            unproven: None,
        },
        Err(why) if rule_id == RULE_NO_RULE_READ_ONLY => Decision::escalate(format!(
            "{why} (the box's vendor note: {})",
            aterm_phase::anchor!("box.note_no_rule")
        )),
        Err(why) => Decision::escalate(why),
    }
}

const GLOB_SAFE: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "stat",
    "grep",
    "egrep",
    "fgrep",
    "diff",
    "du",
    "df",
    "echo",
    "strings",
    "hexdump",
    "od",
    "nl",
    "cut",
    "column",
    "tr",
    "tac",
    "rev",
    "cmp",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "sha256sum",
    "sha1sum",
    "md5sum",
];

/// `Ok` when no reading holds a form the classifier cannot vet: an assignment, `env`, `export` or
/// `xargs` on the walk to a program, a loop variable, or an unquoted glob beside a program off
/// [`GLOB_SAFE`].
fn proof_gaps_clear(readings: &[String]) -> Result<(), String> {
    let assignment = |w: &str| {
        w.split_once('=').is_some_and(|(name, _)| {
            name.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    };
    for line in readings {
        let name = reading_name(line);
        let segments =
            command_words(line).map_err(|why| format!("not read-only ({name}): {why}"))?;
        let glob = unquoted_glob(line);
        for seg in &segments {
            let run = runs(seg).map_err(|why| format!("not read-only ({name}): {why}"))?;
            let at = run.as_ref().map_or(seg.len(), |r| r.at);
            if let Some(w) = seg[..at].iter().find(|w| assignment(w)) {
                return Err(format!(
                    "assigns a variable ({name}): {w}, which the classifier vets only against \
                     its list of hazards"
                ));
            }
            let walked = &seg[..seg.len().min(at + 1)];
            if let Some(p) = walked
                .iter()
                .map(|w| program(w))
                .find(|p| matches!(*p, "env" | "export" | "xargs"))
            {
                return Err(format!(
                    "runs {p} ({name}), whose assignments or files the read-only proof does not \
                     vet"
                ));
            }
            if let Some(p) = seg
                .get(at)
                .map(|w| program(w))
                .filter(|p| matches!(*p, "for" | "select"))
            {
                return Err(format!(
                    "assigns a {p} loop's variable ({name}), which the classifier vets only \
                     against its list of hazards"
                ));
            }
            if let Some(g) = glob {
                let off = seg
                    .get(at)
                    .map(|w| program(w))
                    .filter(|p| GLOB_SAFE.contains(p));
                if off.is_none() {
                    return Err(format!(
                        "an unquoted glob `{g}` ({name}) beside {}, which may take its words \
                         for other operands or options",
                        seg.get(at).map_or("no command", |w| program(w))
                    ));
                }
            }
        }
    }
    Ok(())
}

fn git_reads_clear(
    readings: &[String],
    runs_on: Option<&str>,
    ctx: &ApprovalCtx,
) -> Result<(), String> {
    let mut seen: Vec<(PathBuf, GitView)> = Vec::new();
    let mut cwds = vec![ctx.cwd.clone()];
    cwds.extend(ctx.shell_cwds.iter().filter(|d| **d != ctx.cwd).cloned());
    for line in readings {
        let Some(reads) = git_reads(line, &cwds, ctx.home.as_deref())? else {
            continue;
        };
        if let Some(machine) = runs_on {
            return Err(format!(
                "a git read on {machine}: the configuration it loads is that machine's"
            ));
        }
        if !ctx.cwd_known {
            return Err("a git read with the session's cwd unknown (meta cwd=-)".to_string());
        }
        if let Some(why) = reads.remote.clone() {
            return Err(why);
        }
        let worker = ctx
            .worker
            .as_ref()
            .map_err(|why| format!("a git read whose environment is unknown: {why}"))?;
        ctx.probe.settings_env(worker, &ctx.cwd)?;
        let writes = WorkerWrites::of(ctx);
        for dir in &reads.dirs {
            if !seen.iter().any(|(d, _)| d == dir) {
                seen.push((dir.clone(), ctx.probe.git_view(worker, dir, &writes)?));
            }
            let view = &seen.iter().find(|(d, _)| d == dir).expect("just seen").1;
            if let Some(why) = hazard(view, &reads) {
                return Err(why);
            }
        }
    }
    Ok(())
}

fn reading_name(line: &str) -> &'static str {
    if line.contains('\n') {
        "newline reading"
    } else {
        "space reading"
    }
}

fn plain_under_zsh(readings: &[String], ctx: &ApprovalCtx) -> Result<(), String> {
    if !ctx.shell.as_ref().is_ok_and(ShellStartup::may_run_zsh) {
        return Ok(());
    }
    for line in readings {
        plain_zsh_line(line)
            .map_err(|why| format!("not a plain line under zsh ({}): {why}", reading_name(line)))?;
    }
    Ok(())
}

/// `Ok` when every reading of a Bash box's command classifies read-only, reads no socket, and
/// holds no brace expansion.
pub(crate) fn read_only_every_reading(
    readings: &[String],
    python_allow: &[String],
) -> Result<(), String> {
    if readings.is_empty() {
        return Err("a Bash box with no command".to_string());
    }
    network_free(readings).and_then(|()| brace_free(readings))?;
    for line in readings {
        let v = classify_command_with(line, python_allow);
        if !v.read_only {
            return Err(format!(
                "not read-only ({}): {}",
                reading_name(line),
                v.reason
            ));
        }
    }
    Ok(())
}

fn network_free(readings: &[String]) -> Result<(), String> {
    let socket = |t: &str| ["/dev/tcp/", "/dev/udp/"].iter().any(|p| t.starts_with(p));
    let inet = |t: &str| {
        ["/inet/", "/inet4/", "/inet6/"]
            .iter()
            .any(|p| t.contains(p))
    };
    for line in readings {
        let segments = command_words(line)
            .map_err(|why| format!("not read-only ({}): {why}", reading_name(line)))?;
        for seg in &segments {
            for (k, word) in seg.iter().enumerate() {
                if inet(word) {
                    return Err(format!(
                        "names a gawk network file ({}): {word}",
                        reading_name(line)
                    ));
                }
                let op = word.trim_start_matches(|c: char| c.is_ascii_digit());
                let Some(rest) = op.strip_prefix('<') else {
                    continue;
                };
                if rest.starts_with('<') {
                    continue;
                }
                let rest = rest.strip_prefix('&').unwrap_or(rest);
                let Some(target) = Some(rest)
                    .filter(|r| !r.is_empty())
                    .or_else(|| seg.get(k + 1).map(String::as_str))
                else {
                    continue;
                };
                if socket(target) || target.contains(['$', '`']) {
                    return Err(format!(
                        "an input redirect from {target} ({}) may open a network connection",
                        reading_name(line)
                    ));
                }
            }
        }
    }
    Ok(())
}

fn brace_free(readings: &[String]) -> Result<(), String> {
    for line in readings {
        if let Some(braces) = brace_expansion(line) {
            return Err(format!(
                "a brace expansion ({}): {braces}, which bash makes into words the classifier \
                 never reads",
                reading_name(line)
            ));
        }
    }
    Ok(())
}

/// `Ok` when every reading of a Bash box's command classifies read-only, reads no socket, holds
/// no brace expansion, turns on no blank the screen cannot show, and holds no proof gap.
pub(crate) fn readings_proven(readings: &[String], python_allow: &[String]) -> Result<(), String> {
    read_only_every_reading(readings, python_allow)?;
    blanks_shown(readings)?;
    proof_gaps_clear(readings)
}

/// [`readings_proven`], then the default secret list under this process's `HOME`: the advisory
/// word for a Bash box.
///
/// # Errors
///
/// The first reading that is not read-only, else the first that holds a
/// form the shell may read otherwise than drawn or the classifier cannot
/// vet, else the first that reads a secret, named.
pub fn readings_read_only(readings: &[String], python_allow: &[String]) -> Result<(), String> {
    readings_proven(readings, python_allow)?;
    let home = advisory_home();
    secrets_free(readings, &default_secrets(home.as_deref()), home.as_deref())
}

/// `Ok` unless `line` reads a path the default secret list names under `home`:
/// `aterm drive classify`'s secret half.
///
/// # Errors
///
/// `a read of a secret (<rule>) (space reading): <path>`, or the
/// classifier's refusal of a line it cannot read.
pub fn command_reads_no_secret(line: &str, home: Option<&Path>) -> Result<(), String> {
    secrets_free(&[line.to_string()], &default_secrets(home), home)
}

/// The home the advisory word resolves `~` and the secret list against.
fn advisory_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn blanks_shown(readings: &[String]) -> Result<(), String> {
    readings.iter().try_for_each(|line| {
        box_command::blanks_read_as_drawn(line)
            .map_err(|why| format!("not read as drawn ({}): {why}", reading_name(line)))
    })
}

const NESTED_UNPROVEN: &str = "the vendor draws this only over a shell keyword right after an \
                               assignment or a redirect (bash runs a program by that name) with \
                               `${ …; }` function substitutions, neither of which aterm's \
                               classifier proves a read";

const NOT_RULED_OUT_UNPROVEN: &str = "aterm proves a read past the breaker only on its too-many \
                                      note, never on a reason it has no reading of";

fn breaker_read_only(
    prompt: &PromptV2,
    rows: &[String],
    b: &RmBreaker,
    guard_row: &str,
    ctx: &ApprovalCtx,
) -> Decision {
    let label = rm_breaker_label(b);
    let refused = |why: String| Decision::escalate(format!("{label}: {why}"));
    match b.kind {
        RmBreakerKind::TooManySubstitutions => {}
        RmBreakerKind::NestedUnanalyzed => return refused(NESTED_UNPROVEN.to_string()),
        _ => return refused(NOT_RULED_OUT_UNPROVEN.to_string()),
    }
    if let Err(why) = yes_no_to_proceed(prompt, rows) {
        return refused(why);
    }
    let (readings, subject) = command_readings(prompt, rows, false);
    if let Some(line) = readings.iter().find(|l| names_a_removal(l)) {
        return refused(format!(
            "the command names rm or rmdir ({}), the removal the breaker could not rule out",
            reading_name(line)
        ));
    }
    let runs_on = prompt.runs_on();
    let proven = readings_proven(&readings, &ctx.python_allow)
        .and_then(|()| shell_models_hold(&readings, runs_on.as_deref(), ctx))
        .and_then(|()| plain_under_zsh(&readings, ctx))
        .and_then(|()| git_reads_clear(&readings, runs_on.as_deref(), ctx))
        .and_then(|()| secrets_free(&readings, &ctx.secrets, ctx.home.as_deref()));
    if let Err(why) = proven {
        return refused(why);
    }
    match once_choice(prompt) {
        Ok(choice) => Decision::Approve {
            rule_id: RULE_BREAKER_READ_ONLY,
            choice,
            guard: row_guard(guard_row),
            subject,
            unproven: None,
        },
        Err(why) => refused(why),
    }
}

fn names_a_removal(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    lower.match_indices("rm").any(|(at, _)| {
        let mut end = at + 2;
        if lower[end..].starts_with("dir") {
            end += 3;
        }
        let before = at == 0 || !word(bytes[at - 1]);
        before && bytes.get(end).is_none_or(|c| !word(*c))
    })
}

/// The breakers [`rm_breaker_warn`] answers under `approve = "safe"`: an rm on a possibly-empty
/// variable read whole ([`resolvable_breaker`]), and an rm inside a `-c` script the vendor could not
/// check or whose target is a value known only when it runs. A note that names its target as a
/// critical path, the working directory or an ancestor, a drive root or a mounted drive, and every
/// `not ruled out` reason, stays with a person under `safe`.
fn warnable_breaker(b: &RmBreaker) -> bool {
    resolvable_breaker(b).is_ok()
        || (b.command == RmCommand::Rm
            && matches!(
                b.kind,
                RmBreakerKind::ShellScriptUnchecked | RmBreakerKind::ShellScriptValue
            ))
}

fn resolvable_breaker(b: &RmBreaker) -> Result<(), String> {
    if b.command != RmCommand::Rm || !matches!(b.kind, RmBreakerKind::EmptyVariable { .. }) {
        return Err(format!(
            "{}: the rm-breaker rule resolves only an rm on a possibly-empty variable",
            rm_breaker_label(b)
        ));
    }
    if !b.target.trim_end().ends_with(')')
        || b.target.matches('(').count() != b.target.matches(')').count()
    {
        return Err(format!(
            "{}: its note is not read whole",
            rm_breaker_label(b)
        ));
    }
    Ok(())
}

/// The vendor's rm/rmdir breaker as a reason names it, in the note's own words
/// (`the rm circuit breaker (statically-unresolvable target)`).
pub fn rm_breaker_label(b: &RmBreaker) -> String {
    let id = match b.kind {
        RmBreakerKind::EmptyVariable {
            in_substitution: false,
        } => "box.rm_breaker.empty_var",
        RmBreakerKind::EmptyVariable {
            in_substitution: true,
        } => "box.rm_breaker.empty_var_in_substitution",
        RmBreakerKind::Unresolvable => "box.rm_breaker.unresolvable",
        RmBreakerKind::CriticalPath => "box.rm_breaker.critical_path",
        RmBreakerKind::WorkingDirectory => "box.rm_breaker.working_dir",
        RmBreakerKind::DriveRoot => "box.rm_breaker.drive_root",
        RmBreakerKind::MountedDrivePath => "box.rm_breaker.mounted_drive",
        RmBreakerKind::TooManySubstitutions => "box.rm_breaker.too_many",
        RmBreakerKind::NestedUnanalyzed => "box.rm_breaker.nested_unanalyzed",
        RmBreakerKind::NotRuledOut => "",
        RmBreakerKind::ShellScriptUnchecked => "box.rm_breaker.shell_unchecked",
        RmBreakerKind::ShellScriptValue => "box.rm_breaker.shell_value",
    };
    let why = if id.is_empty() {
        b.target.trim()
    } else {
        anchor(id)
            .trim()
            .trim_start_matches("on ")
            .trim_end_matches([':', '('])
            .trim()
    };
    let command = match b.command {
        RmCommand::Rm => "rm",
        RmCommand::Rmdir => "rmdir",
    };
    if breaker_not_ruled_out(b) {
        let not_ruled_out = aterm_phase::anchor!("box.rm_breaker.not_ruled_out").trim();
        return format!(
            "the {command} circuit breaker ({not_ruled_out}: {why}; the command need not name \
             {command})"
        );
    }
    format!("the {command} circuit breaker ({why})")
}

const fn breaker_not_ruled_out(b: &RmBreaker) -> bool {
    matches!(
        b.kind,
        RmBreakerKind::TooManySubstitutions
            | RmBreakerKind::NestedUnanalyzed
            | RmBreakerKind::NotRuledOut
    )
}

fn name_the_breaker(b: &RmBreaker, reason: String) -> String {
    let label = rm_breaker_label(b);
    if reason.contains(&label) {
        return reason;
    }
    let rest = ["the ", "an ", ""].iter().find_map(|article| {
        ["rm", "rmdir"].iter().find_map(|command| {
            reason
                .strip_prefix(&format!("{article}{command} circuit breaker"))
                .filter(|rest| rest.is_empty() || rest.starts_with([' ', ':']))
        })
    });
    match rest {
        Some(rest) => format!("{label}{rest}"),
        None => format!("{label}: {reason}"),
    }
}

/// What a decline tells the worker ([`Decision::Decline`]).
#[must_use]
pub fn decline_text(what: &str, why: &str, fix: &str) -> String {
    let why = clip(why.trim().trim_end_matches('.'), DECLINE_WHY_CHARS);
    format!("{DECLINE_PREFIX}{what} was not run: {why}. {fix}.")
        .chars()
        .map(|c| {
            if c.is_control() || (c.is_whitespace() && c != ' ') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// The decline's one constructor.
pub fn decline(
    rule_id: &'static str,
    prompt: &PromptV2,
    text: String,
    subject: String,
) -> Result<Decision, String> {
    let refusal = refusal_of(prompt, &text)?;
    Ok(Decision::Decline {
        rule_id,
        refusal,
        text,
        subject,
    })
}

/// `text` with all whitespace dropped, so wrapped rows read back as what was written.
pub(crate) fn squashed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn amend_open_label() -> String {
    format!(
        "{REFUSAL_LABEL}, {}",
        aterm_phase::anchor!("prompt.amend_no")
    )
}

fn amended_label(text: &str) -> String {
    format!("{REFUSAL_LABEL}, {text}")
}

fn offers_amend(prompt: &PromptV2) -> bool {
    let hint = aterm_phase::anchor!("prompt.amend");
    prompt
        .footer
        .as_deref()
        .is_some_and(|f| f.split(" · ").any(|h| h == hint))
}

fn refusal_of(prompt: &PromptV2, text: &str) -> Result<usize, String> {
    let open = amend_open_label();
    let typed = squashed(&amended_label(text));
    let hits: Vec<usize> = prompt
        .options
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            o.role == Role::Deny
                && (o.label == REFUSAL_LABEL
                    || (o.focused && (o.label == open || squashed(&o.label) == typed)))
        })
        .map(|(k, _)| k)
        .collect();
    let [k] = hits[..] else {
        return Err(if hits.is_empty() {
            "the box has no `No` to amend".to_string()
        } else {
            "the box has more than one `No`".to_string()
        });
    };
    if prompt.options[k].label == REFUSAL_LABEL && !offers_amend(prompt) {
        return Err(format!(
            "the box does not offer `{}`",
            aterm_phase::anchor!("prompt.amend")
        ));
    }
    Ok(k)
}

/// The next keystroke of a decline with the guard of the row showing the state it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclineStep {
    Move { down: bool, guard: String },
    Amend { guard: String },
    Type { guard: String },
    Submit { guard: String },
}

/// A decline's next keystroke on a fresh read.
pub fn decline_step(
    prompt: &PromptV2,
    rows: &[String],
    refusal: usize,
    text: &str,
) -> Result<DeclineStep, String> {
    let Some(o) = prompt.options.get(refusal).filter(|o| o.role == Role::Deny) else {
        return Err(format!("the box has no refusal at option {}", refusal + 1));
    };
    let guard_of = |row: usize| {
        rows.get(row)
            .map(|r| row_guard(r))
            .ok_or_else(|| "an option's row is not on the screen".to_string())
    };
    if !o.focused {
        if o.label != REFUSAL_LABEL {
            return Err(format!(
                "the refusal reads `{}` with the focus elsewhere",
                clip(&o.label, 60)
            ));
        }
        let Some(at) = prompt.options.iter().position(|x| x.focused) else {
            return Err("no option has the focus".to_string());
        };
        return Ok(DeclineStep::Move {
            down: refusal > at,
            guard: guard_of(prompt.options[at].row)?,
        });
    }
    let guard = guard_of(o.row)?;
    if o.label == REFUSAL_LABEL {
        if !offers_amend(prompt) {
            return Err(format!(
                "the focused `No` is not offered `{}`",
                aterm_phase::anchor!("prompt.amend")
            ));
        }
        return Ok(DeclineStep::Amend { guard });
    }
    if o.label == amend_open_label() {
        return Ok(DeclineStep::Type { guard });
    }
    if squashed(&o.label) == squashed(&amended_label(text)) {
        return Ok(DeclineStep::Submit { guard });
    }
    Err(format!(
        "the refusal's input holds `{}`, not the decline's text",
        clip(&o.label, 60)
    ))
}

fn read_box(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if !prompt.base_title().as_deref().is_some_and(is_read_title) {
        return Decision::escalate(format!(
            "the box header is not a Read header: {}",
            prompt.title
        ));
    }
    if let Some(machine) = prompt.runs_on() {
        return Decision::escalate(format!(
            "a Read on {machine}: its path is not on this machine"
        ));
    }
    let Some(path) = prompt.path.as_deref() else {
        return Decision::escalate("a Read box with no path");
    };
    let Some(path_row) = file_path_row(rows, prompt, path) else {
        return Decision::escalate("the Read path's row is not on the screen");
    };
    let choice = match once_choice(prompt) {
        Ok(c) => c,
        Err(why) => return Decision::escalate(why),
    };
    if !path
        .split(" · ")
        .all(|part| part.bytes().all(|b| (0x20..0x7f).contains(&b)))
    {
        return Decision::escalate(format!("a Read of a path outside plain ASCII: {path}"));
    }
    let mut lexical = Vec::new();
    for reading in read_readings(path) {
        match read_path_lexical(reading, ctx) {
            Ok(comps) => lexical.push((reading, comps)),
            Err(why) => return Decision::escalate(why),
        }
    }
    for (reading, comps) in &lexical {
        if let Err(why) = read_path_on_disk(reading, comps, ctx) {
            return Decision::escalate(why);
        }
    }
    Decision::Approve {
        rule_id: RULE_READ_OUTSIDE_CWD,
        choice,
        guard: row_guard(&rows[path_row]),
        subject: path.to_string(),
        unproven: None,
    }
}

fn read_readings(call: &str) -> impl Iterator<Item = &str> {
    std::iter::once(call).chain(call.match_indices(" · ").map(|(at, _)| &call[..at]))
}

fn is_read_title(base: &str) -> bool {
    base.strip_prefix(aterm_phase::anchor!("box.read"))
        .is_some_and(|rest| matches!(rest, "" | "s" | "(s)"))
}

fn resolve_abs(path: &str, home: Option<&Path>) -> Result<Vec<String>, String> {
    if path.contains(['*', '?', '[', '{']) {
        return Err(format!("a pattern: {path}"));
    }
    let full = if let Some(rest) = path.strip_prefix("~/") {
        let home = home.ok_or_else(|| format!("{path} under ~ with no home to resolve it"))?;
        let home = home.to_string_lossy();
        if !home.starts_with('/') {
            return Err(format!(
                "{path} under a home that is not a `/` path ({home})"
            ));
        }
        format!("{}/{rest}", home.trim_end_matches('/'))
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        return Err(format!("a path that is not absolute: {path}"));
    };
    abs_components(&full).ok_or_else(|| format!("a path with `..`: {path}"))
}

fn read_path_lexical(path: &str, ctx: &ApprovalCtx) -> Result<Vec<String>, String> {
    // The components below drop an empty one (`a//b`, `a/./b`, a trailing `/`),
    // so the path judged would not be the one drawn. Claude Code collapses those
    // in a real path, so one on the box means the screen drew a name as nothing
    // (zero-width characters only): a person reads it.
    if path.contains("//") || path.contains("/./") || path.ends_with('/') {
        return Err(format!(
            "a Read of a path with an empty component, a name the box may have drawn as nothing: {path}"
        ));
    }
    let comps = resolve_abs(path, ctx.home.as_deref()).map_err(|e| format!("a Read of {e}"))?;
    read_comps_allowed(&comps, path, ctx)?;
    Ok(comps)
}

fn read_path_on_disk(path: &str, comps: &[String], ctx: &ApprovalCtx) -> Result<(), String> {
    let real = ctx
        .probe
        .real_components(comps)
        .ok_or_else(|| format!("a Read whose symlinks cannot be resolved: {path}"))?;
    if real != comps {
        read_comps_allowed(&real, &format!("{path} (-> /{})", real.join("/")), ctx)?;
    }
    if let Some(twin) = ctx.probe.hidden_twin_in(comps, ctx.home.as_deref())? {
        return Err(format!(
            "a Read of {path}: {twin:?} differs from it only by characters the box may not \
             draw (and by case, which the default volume ignores), so the box may not show \
             which one is read"
        ));
    }
    Ok(())
}

fn read_comps_allowed(comps: &[String], shown: &str, ctx: &ApprovalCtx) -> Result<(), String> {
    let path = shown;
    if !ctx.read_roots.iter().any(|r| r.holds(comps)) {
        return Err(format!("a Read outside every allowed root: {path}"));
    }
    if let Some(rule) = secret_hit(comps, &ctx.secrets) {
        return Err(format!("a Read of a secret ({rule:?}): {path}"));
    }
    Ok(())
}

/// Every component list a path's text may name, for the secrets check (a hit on any one
/// refuses, so each reading past the plain `/` split only adds refusals):
/// - split at `/` alone (a Unix name may hold a `\`), and at `\` too (Windows' separator, which
///   Git Bash's MSYS layer also takes);
/// - a drive path (`C:\a`, `C:/a`) read as Git Bash spells it (`c`, `a`), so a Windows path's
///   spellings and a rule under a Windows home (`C:\Users\name/.secrets`) share components;
/// - a rooted path's `..` kept as a component (a symlink before it may lead elsewhere, as `cat`
///   resolves `..` on disk) and also taken lexically, for when none does.
fn path_readings(path: &str) -> Vec<Vec<String>> {
    let (drive, rest) = match path.split_at_checked(2) {
        Some((d, rest))
            if d.ends_with(':')
                && d.as_bytes()[0].is_ascii_alphabetic()
                && rest.starts_with(['/', '\\']) =>
        {
            (Some(d[..1].to_ascii_lowercase()), rest)
        }
        _ => (None, path),
    };
    let floor = usize::from(drive.is_some());
    let mut out = Vec::new();
    for seps in [&['/'][..], &['/', '\\']] {
        if seps.len() == 2 && !rest.contains('\\') {
            break;
        }
        let literal: Vec<String> = drive
            .iter()
            .cloned()
            .chain(
                rest.split(seps)
                    .filter(|c| !c.is_empty() && *c != ".")
                    .map(str::to_string),
            )
            .collect();
        let lexical = ((drive.is_some() || rest.starts_with(seps))
            && literal.iter().any(|c| c == ".."))
        .then(|| {
            let mut comps: Vec<String> = Vec::new();
            for c in &literal {
                if c != ".." {
                    comps.push(c.clone());
                } else if comps.len() > floor {
                    comps.pop();
                }
            }
            comps
        });
        out.push(literal);
        out.extend(lexical);
    }
    out
}

fn secret_hit<'a>(comps: &[String], secrets: &'a [SecretRule]) -> Option<&'a SecretRule> {
    secrets.iter().find(|rule| match rule {
        SecretRule::Under(dir) => path_readings(&dir.to_string_lossy()).iter().any(|d| {
            comps.len() >= d.len() && d.iter().zip(comps).all(|(a, b)| a.eq_ignore_ascii_case(b))
        }),
        SecretRule::Component(name) => comps.iter().any(|c| c.eq_ignore_ascii_case(name)),
        SecretRule::Basename(glob) => comps
            .last()
            .is_some_and(|c| glob_match(&glob.to_ascii_lowercase(), &c.to_ascii_lowercase())),
    })
}

/// The texts an operand may name, each read by [`path_readings`]: the word itself, a `~` lead
/// resolved to the home, a word led by an expansion (`$HOME/…`, `$USERPROFILE\…`) also read as
/// under the home at either separator, and a root-relative `\…` also read on a drive home's
/// drive, as Windows resolves it on the current one.
fn operand_comps(op: &str, home: Option<&Path>) -> Vec<Vec<String>> {
    let home_text = home.map(|h| h.to_string_lossy().trim_end_matches('/').to_string());
    let mut texts = Vec::new();
    match (op.strip_prefix('~'), &home_text) {
        (Some(""), Some(h)) => texts.push(h.clone()),
        (Some(rest), Some(h)) if rest.starts_with('/') => texts.push(format!("{h}{rest}")),
        _ => texts.push(op.to_string()),
    }
    if let Some(h) = &home_text {
        for sep in ['/', '\\'] {
            if let Some((lead, rest)) = op.split_once(sep)
                && lead.starts_with('$')
            {
                texts.push(format!("{h}/{rest}"));
            }
        }
        if op.starts_with('\\')
            && !op.starts_with(r"\\")
            && let Some(drive) = h
                .get(..2)
                .filter(|d| d.ends_with(':') && d.as_bytes()[0].is_ascii_alphabetic())
        {
            texts.push(format!("{drive}{op}"));
        }
    }
    texts.iter().flat_map(|t| path_readings(t)).collect()
}

fn secrets_free(
    readings: &[String],
    secrets: &[SecretRule],
    home: Option<&Path>,
) -> Result<(), String> {
    if secrets.is_empty() {
        return Ok(());
    }
    for line in readings {
        let operands = path_operands(line)
            .map_err(|why| format!("not read-only ({}): {why}", reading_name(line)))?;
        for op in &operands {
            let hit = operand_comps(op, home)
                .iter()
                .find_map(|comps| secret_hit(comps, secrets));
            if let Some(rule) = hit {
                return Err(format!(
                    "a read of a secret ({rule:?}) ({}): {op}",
                    reading_name(line)
                ));
            }
        }
    }
    Ok(())
}

fn trust_dialog(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    let Some(path) = prompt.path.as_deref() else {
        return Decision::escalate("a folder-trust dialog with no folder");
    };
    let comps = match resolve_abs(path, ctx.home.as_deref()) {
        Ok(c) => c,
        Err(why) => return Decision::escalate(format!("a folder-trust dialog for {why}")),
    };
    if !ctx.cwd_known {
        return Decision::escalate(
            "a folder-trust dialog with the session's cwd unknown (meta cwd=-)",
        );
    }
    let cwd = abs_components(&ctx.cwd.to_string_lossy());
    if cwd.as_deref() != Some(comps.as_slice()) {
        return Decision::escalate(format!(
            "a folder-trust dialog for {path}, not the session's cwd {}",
            ctx.cwd.display()
        ));
    }
    let spawned = ctx
        .trusted_cwd
        .as_deref()
        .and_then(|dir| abs_components(&dir.to_string_lossy()))
        .is_some_and(|dir| dir == comps);
    if !spawned && !ctx.trust_roots.iter().any(|r| r.holds(&comps)) {
        return Decision::escalate(format!(
            "a folder-trust dialog for {path}, under no trust root"
        ));
    }
    let roles: Vec<Role> = prompt.options.iter().map(|o| o.role).collect();
    if roles.len() != 2 || !roles.contains(&Role::Trust) || !roles.contains(&Role::Exit) {
        return Decision::escalate(format!(
            "a folder-trust dialog whose options are not exactly an exit and a trust ({})",
            prompt
                .options
                .iter()
                .map(|o| format!("`{}` {}", o.label, o.role.name()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let Some(target) = prompt.options.iter().position(|o| o.role == Role::Trust) else {
        return Decision::escalate("a folder-trust dialog with no trust option");
    };
    let Some(path_row) = rows
        .iter()
        .enumerate()
        .take(prompt.span.1.min(rows.len()))
        .skip(prompt.span.0 + 1)
        .find(|(_, r)| {
            let t = r.trim();
            t.starts_with('/') || t.starts_with('~')
        })
        .map(|(k, _)| k)
    else {
        return Decision::escalate("the folder's row is not on the screen");
    };
    let choice = match prompt.select {
        Select::Digits => match prompt.options[target].n {
            Some(n) => Choice::Digit(n),
            None => return Decision::escalate("a numbered dialog with an unnumbered option"),
        },
        Select::ArrowsEnter => {
            let Some(at) = prompt.options.iter().position(|o| o.focused) else {
                return Decision::escalate("a folder-trust dialog with no focused option");
            };
            Choice::Focus {
                steps: focus_steps(at, target),
                label: prompt.options[target].label.clone(),
            }
        }
    };
    Decision::Approve {
        rule_id: RULE_TRUST_DIALOG,
        choice,
        guard: row_guard(&rows[path_row]),
        subject: path.to_string(),
        unproven: None,
    }
}

fn spawned_trust(
    program: Program,
    prompt: &PromptV2,
    rows: &[String],
    ctx: &ApprovalCtx,
) -> Decision {
    let product = program.product();
    if !ctx.cwd_known {
        return Decision::escalate(format!(
            "{product}'s folder-trust dialog with the session's cwd unknown (meta cwd=-)"
        ));
    }
    let Some(trusted) = ctx
        .trusted_cwd
        .as_deref()
        .and_then(|dir| abs_components(&dir.to_string_lossy()))
    else {
        return Decision::escalate(format!(
            "{product}'s folder-trust dialog: no folder its starter trusted for this session"
        ));
    };
    if abs_components(&ctx.cwd.to_string_lossy()).as_deref() != Some(trusted.as_slice()) {
        return Decision::escalate(format!(
            "{product}'s folder-trust dialog: the session's cwd {} is not the folder its \
             starter trusted",
            ctx.cwd.display()
        ));
    }
    let trust: Vec<usize> = (0..prompt.options.len())
        .filter(|&k| prompt.options[k].role == Role::Trust)
        .collect();
    let [target] = trust.as_slice() else {
        return Decision::escalate(format!(
            "{product}'s folder-trust dialog with no single trust option"
        ));
    };
    let target = *target;
    if let Some(o) = prompt
        .options
        .iter()
        .find(|o| !matches!(o.role, Role::Trust | Role::Exit | Role::Deny))
    {
        return Decision::escalate(format!(
            "{product}'s folder-trust dialog with an option that neither trusts this folder nor \
             exits (`{}` {})",
            o.label,
            o.role.name()
        ));
    }
    let (top, bottom) = prompt.span;
    let span = (top..=bottom.min(rows.len().saturating_sub(1))).filter(|&k| k < rows.len());
    let Some(path) = prompt.path.as_deref() else {
        return Decision::escalate(format!("{product}'s folder-trust dialog with no folder"));
    };
    let comps = match resolve_abs(path, ctx.home.as_deref()) {
        Ok(c) => c,
        Err(why) => {
            return Decision::escalate(format!("{product}'s folder-trust dialog for {why}"));
        }
    };
    if comps != trusted {
        return Decision::escalate(format!(
            "{product}'s folder-trust dialog for {path}, not the folder its starter trusted"
        ));
    }
    let Some(row) = span.clone().find(|&k| rows[k].trim_end().ends_with(path)) else {
        return Decision::escalate(format!(
            "{product}'s folder-trust dialog: the folder's row is not on the screen"
        ));
    };
    let choice = match prompt.select {
        Select::Digits => match prompt.options[target].n {
            Some(n) => Choice::Digit(n),
            None => return Decision::escalate("a numbered dialog with an unnumbered option"),
        },
        Select::ArrowsEnter => {
            let Some(at) = prompt.options.iter().position(|o| o.focused) else {
                return Decision::escalate(format!(
                    "{product}'s folder-trust dialog with no focused option"
                ));
            };
            Choice::Focus {
                steps: focus_steps(at, target),
                label: prompt.options[target].label.clone(),
            }
        }
    };
    Decision::Approve {
        rule_id: RULE_TRUST_DIALOG,
        choice,
        guard: row_guard(&rows[row]),
        subject: path.to_string(),
        unproven: None,
    }
}

const BACKSTOP_ROWS: usize = 10;

fn backstop_warnings(body: &[String]) -> Vec<String> {
    let leading_spaces = |r: &str| r.chars().take_while(|c| c.is_whitespace()).count();
    let mut out = Vec::new();
    let mut k = 0;
    while k < body.len() {
        let row = &body[k];
        k += 1;
        let Some(head) = row.trim().strip_prefix('⚠') else {
            continue;
        };
        let col = leading_spaces(row);
        let mut listed = Vec::new();
        while k < body.len() && !body[k].trim().is_empty() && leading_spaces(&body[k]) > col {
            listed.push(body[k].trim());
            k += 1;
        }
        let mut warning = head.trim().to_string();
        if !listed.is_empty() {
            let more = listed.len().saturating_sub(BACKSTOP_ROWS);
            warning.push(' ');
            warning.push_str(&listed[..listed.len() - more].join(", "));
            if more > 0 {
                warning.push_str(&format!(", +{more} more"));
            }
        }
        out.push(warning);
    }
    out
}

const PURCHASE: &[&str] = &[
    "re-enable",
    "turn on usage credits",
    "switch to usage credits",
    "usage credits",
    "buy",
    "purchase",
    "pay",
    "add funds",
    "upgrade",
    "subscribe",
    "adjust monthly limit",
    "adjust monthly spend limit",
    "set limit",
    "set to unlimited",
    "set monthly spend limit",
];

const RESET_SPEND: &[&str] = &["use my reset", "use your reset", "reset your session limit"];

fn opens_with(label: &str, words: &[&str]) -> bool {
    let l = label.trim().to_lowercase();
    let l = l
        .strip_prefix("yes,")
        .or_else(|| l.strip_prefix("yes "))
        .map_or(l.as_str(), str::trim_start);
    words.iter().any(|w| {
        l.strip_prefix(w)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
    })
}

fn spends(label: &str) -> bool {
    aterm_phase::spends_reset(label)
        || opens_with(label, RESET_SPEND)
        || aterm_phase::uses_a_reset(label)
}

/// Whether choosing `opt` would buy something.
#[must_use]
pub fn buys(opt: &Opt) -> bool {
    spends(&opt.label) || opens_with(&opt.label, PURCHASE)
}

fn waits(opt: &Opt) -> bool {
    let l = opt.label.to_lowercase();
    ["stop and wait", "wait", "no, keep", "stay on", "stay "]
        .iter()
        .any(|w| l.starts_with(w))
}

fn full_power(reading: &Reading, rows: &[String], ctx: &ApprovalCtx, safe: String) -> Decision {
    if reading.phase != Phase::Prompt || !reading.phase_authoritative {
        return Decision::escalate(safe);
    }
    let Some(p) = reading.prompt.as_ref() else {
        return Decision::escalate(safe);
    };
    if let Some(why) = unmeasured_box(reading.program, p) {
        return Decision::escalate(format!("{why} (the safe rules: {safe})"));
    }
    if p.kind == PromptKind::Other
        && let Some(decision) = credits_decline(reading.program, p, rows, ctx, &safe)
    {
        return decision;
    }
    let pick = match p.kind {
        PromptKind::Question => Err("a question is answered by its own rule".to_string()),
        PromptKind::PlanEnter | PromptKind::PlanExit => plan_pick(p),
        PromptKind::Trust => p
            .with_role(Role::Trust)
            .map(|o| (RULE_TRUST_ANY, o))
            .ok_or_else(|| "a folder-trust dialog with no trust option".to_string()),
        PromptKind::HeldMessage => match p
            .options
            .iter()
            .filter(|o| o.role != Role::Deny)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [o] if p.options.iter().any(|o| o.role == Role::Deny) => Ok((RULE_ALLOW_ONCE, *o)),
            _ => Err("a held message whose delivery is not its one other option".to_string()),
        },
        PromptKind::ReadOutsideSetting => match p.with_role(Role::Once) {
            Some(o) => Ok((RULE_ALLOW_ONCE, o)),
            None => p
                .options
                .iter()
                .rev()
                .find(|o| o.role == Role::Deny)
                .map(|o| (RULE_DECLINE, o))
                .ok_or_else(|| "the read-outside setting with no `No`".to_string()),
        },
        PromptKind::ModelSwitch => model_confirm(p, rows),
        PromptKind::Other => other_dialog(reading.program, p, rows, ctx),
        _ => allow_once(p),
    };
    answer(p, rows, pick, safe)
}

fn unmeasured_box(program: Program, p: &PromptV2) -> Option<String> {
    match program {
        Program::Muse => match p.kind {
            PromptKind::Tool => Some(format!(
                "Muse Code's tool-action box for `{}`: an MCP tool's call reaches past the \
                 measured shell, file and network boxes",
                p.command
            )),
            PromptKind::Other if p.title == aterm_phase::anchor!("muse.box.network") => {
                Some(format!(
                    "Muse Code's network box of a command: what it reaches past its sandbox (a \
                 host, a Unix socket) is a person's to allow{}",
                    if p.command.is_empty() {
                        String::new()
                    } else {
                        format!(" (`{}`)", p.command)
                    }
                ))
            }
            _ => None,
        },
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Generic => None,
    }
}

fn model_confirm<'p>(p: &'p PromptV2, rows: &[String]) -> Result<(&'static str, &'p Opt), String> {
    let head_end = p
        .options
        .first()
        .map_or(p.span.1, |o| o.row)
        .min(rows.len());
    let subtitle = rows
        .get(p.span.0 + 1..head_end)
        .unwrap_or_default()
        .iter()
        .map(|r| r.trim())
        .take_while(|r| !r.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let hook = aterm_phase::anchor!("model_switch.hook");
    if subtitle.contains(hook) {
        return Err(format!(
            "the person's own PreModelSwitch hook asked for this confirmation (`{hook}`)"
        ));
    }
    if subtitle != aterm_phase::anchor!("model_switch.cost") {
        return Err(format!(
            "a model-switch confirmation whose subtitle is not the cost warning: `{subtitle}`"
        ));
    }
    match p
        .options
        .iter()
        .filter(|o| o.role == Role::Once)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [o] if !buys(o) => Ok((RULE_MODEL_CONFIRM, *o)),
        _ => Err(format!(
            "a model-switch confirmation without exactly one `{}` ({})",
            aterm_phase::anchor!("model_switch.yes"),
            p.options
                .iter()
                .map(|o| format!("{}:{}", o.role.name(), o.label))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn answer(
    p: &PromptV2,
    rows: &[String],
    pick: Result<(&'static str, &Opt), String>,
    safe: String,
) -> Decision {
    let (rule_id, pick) = match pick {
        Ok(pick) => pick,
        Err(why) => return Decision::escalate(format!("{why} (the safe rules: {safe})")),
    };
    let (choice, label) = match choice_of(p, pick) {
        Ok(c) => (c, pick.label.clone()),
        Err(why) => return Decision::escalate(why),
    };
    let what = if p.command.is_empty() {
        p.title.as_str()
    } else {
        p.command.as_str()
    };
    let unproven = match p.rm_breaker() {
        Some(b) => name_the_breaker(&b, safe),
        None => safe,
    };
    let default_no = p.select == Select::ArrowsEnter
        && p.options.first().is_some_and(|o| o.role == Role::Deny)
        && rule_id == RULE_ALLOW_ONCE;
    let unproven = if default_no
        || rule_id == RULE_ALLOW_ONCE && p.focused().is_some_and(|o| o.role == Role::Deny)
    {
        format!("{unproven}; vendor default was No")
    } else {
        unproven
    };
    let what = match backstop_of(p, rows) {
        Some(warnings) => {
            format!("{what} (backstop: {warnings}; trusting makes these apply without asking)")
        }
        None => what.to_string(),
    };
    let subject = if matches!(rule_id, RULE_ALLOW_ONCE | RULE_TRUST_ANY) {
        what
    } else {
        format!("{what}{SUBJECT_OPTION}{label}")
    };
    Decision::Approve {
        rule_id,
        choice,
        guard: row_guard(&rows[judged_row(p, rows)]),
        subject,
        unproven: Some(unproven),
    }
}

fn backstop_of(p: &PromptV2, rows: &[String]) -> Option<String> {
    let backstop = p.kind == PromptKind::Trust
        && p.options
            .iter()
            .any(|o| o.role == Role::Deny && o.label == aterm_phase::anchor!("trust.no_backstop"));
    backstop.then(|| {
        let body = &rows[(p.span.0 + 1).min(rows.len())..p.span.1.min(rows.len())];
        backstop_warnings(body).join("; ")
    })
}

const CONSENT: &[&str] = &["continue with"];

fn consents(opt: &Opt) -> bool {
    opens_with(&opt.label, CONSENT) && !kept_for_good(opt) && !cut_short(&opt.label)
}

const CREDITS: &[&str] = &["usage credit", "extra usage"];

fn credits_consent(p: &PromptV2, rows: &[String], opt: &Opt) -> bool {
    let names = |text: &str| {
        let text = text.to_lowercase();
        CREDITS.iter().any(|w| text.contains(w))
    };
    let end = p.span.1.saturating_add(1).min(rows.len());
    let frame = |w: &&str| w.chars().all(|c| ('\u{2500}'..='\u{257f}').contains(&c));
    let body = rows
        .get(p.span.0.min(end)..end)
        .unwrap_or_default()
        .iter()
        .flat_map(|r| r.split_whitespace())
        .filter(|w| !frame(w))
        .collect::<Vec<_>>()
        .join(" ");
    names(&opt.label) || names(&p.title) || names(&body)
}

fn credits_decline(
    program: Program,
    p: &PromptV2,
    rows: &[String],
    ctx: &ApprovalCtx,
    safe: &str,
) -> Option<Decision> {
    if ctx.usage_credits || !draws_claude_boxes(program) {
        return None;
    }
    let o = p.options.iter().find(|o| consents(o) && !buys(o))?;
    if !credits_consent(p, rows, o) {
        return None;
    }
    let esc = p.footer.as_deref().is_some_and(|footer| {
        aterm_phase::prompt::footer_keys(footer)
            .iter()
            .any(|key| key.eq_ignore_ascii_case(aterm_phase::anchor!("prompt.cancel")))
    });
    let title = aterm_phase::prompt::title_row(p, rows);
    Some(match (esc, title, p.focused()) {
        (true, Some(title), Some(focused)) => Decision::Approve {
            rule_id: RULE_CREDITS_DECLINE,
            choice: Choice::FocusKey {
                steps: 0,
                label: focused.label.clone(),
                key: "esc",
            },
            guard: row_guard(title),
            subject: format!("{}{SUBJECT_OPTION}Esc, `{}` not taken", p.title, o.label),
            unproven: Some(safe.to_string()),
        },
        _ => Decision::escalate(format!(
            "{}; the box reads no `Esc to cancel` to decline it with (the safe rules: {safe})",
            credits_held(o)
        )),
    })
}

fn credits_held(o: &Opt) -> String {
    let what = if o.label.to_lowercase().contains("usage credit") {
        "would carry the session onto the credits already on at the account (it saves nothing)"
    } else {
        "would carry the session onto usage credits, and Claude Code keeps that consent for good \
         for that model and account"
    };
    format!(
        "continuing on usage credits is off (`[harness] usage_credits`): `{}` {what}",
        o.label
    )
}

fn mcp_server_held(p: &PromptV2) -> String {
    format!(
        "every answer, Esc too, is saved for good in .claude/settings.local.json (`claude mcp \
         reset-project-choices` undoes it): Claude Code's MCP server dialog (`{}`) is no rule's \
         to answer, at any level",
        p.title
    )
}

fn other_dialog<'p>(
    program: Program,
    p: &'p PromptV2,
    rows: &[String],
    ctx: &ApprovalCtx,
) -> Result<(&'static str, &'p Opt), String> {
    if draws_claude_boxes(program)
        && let Some(o) = p.options.iter().find(|o| consents(o) && !buys(o))
    {
        if !credits_consent(p, rows, o) {
            return Ok((RULE_ALLOW_ONCE, o));
        }
        if ctx.usage_credits {
            return Ok((RULE_CREDITS_CONSENT, o));
        }
        return Err(credits_held(o));
    }
    if let Some(o) = refusal_pause_switch(program, p) {
        return fallback_switch(o, ctx);
    }
    if p.options.iter().any(buys)
        && let Some(o) = p
            .options
            .iter()
            .find(|o| waits(o) && !buys(o) && !switches_model(o))
    {
        return Ok((RULE_NO_SPEND, o));
    }
    p.with_role(Role::Deny)
        .filter(|o| !switches_model(o))
        .map(|o| (RULE_DECLINE, o))
        .ok_or_else(|| {
            let switch = p
                .options
                .iter()
                .find(|o| switches_model(o))
                .map(|o| format!("its `{}` is no switch aterm makes; ", o.label))
                .unwrap_or_default();
            format!(
                "a dialog of no kind aterm reads, {switch}with no consent, nothing that waits and \
                 no refusal among its options ({})",
                p.options
                    .iter()
                    .map(|o| format!("{}:{}", o.role.name(), o.label))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn switches_model(opt: &Opt) -> bool {
    opt.label.trim_start().starts_with("Switch to ") && !buys(opt)
}

fn refusal_pause_switch(program: Program, p: &PromptV2) -> Option<&Opt> {
    if !draws_claude_boxes(program)
        || p.head_off_screen
        || p.title.trim() != aterm_phase::anchor!("paused.title")
    {
        return None;
    }
    match p.options.as_slice() {
        [switch, edit]
            if switches_model(switch)
                && edit
                    .label
                    .trim_start()
                    .starts_with(aterm_phase::anchor!("paused.edit")) =>
        {
            Some(switch)
        }
        _ => None,
    }
}

fn fallback_switch<'p>(o: &'p Opt, ctx: &ApprovalCtx) -> Result<(&'static str, &'p Opt), String> {
    if ctx.model_fallback {
        Ok((RULE_MODEL_SWITCH, o))
    } else {
        Err(format!(
            "a model switch (`{}`): model_fallback is off",
            o.label
        ))
    }
}

const BROAD_PLAN: &[&str] = &[
    "auto mode",
    "accept edits",
    "auto-accept",
    "bypass permissions",
    "clear context",
];

fn plan_pick(p: &PromptV2) -> Result<(&'static str, &Opt), String> {
    let yes = |o: &&Opt| o.affirms() && !buys(o);
    let broad = |o: &Opt| {
        let l = o.label.to_lowercase();
        BROAD_PLAN.iter().any(|w| l.contains(w))
    };
    if let Some(o) = p
        .options
        .iter()
        .filter(yes)
        .find(|o| o.role == Role::Once && !broad(o))
        .or_else(|| p.options.iter().filter(yes).find(|o| !broad(o)))
    {
        return Ok((RULE_PLAN, o));
    }
    p.with_role(Role::Deny)
        .map(|o| (RULE_DECLINE, o))
        .ok_or_else(|| {
            format!(
                "a plan approval whose every yes grants a standing mode, and no `No` ({})",
                p.options
                    .iter()
                    .map(|o| o.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn allow_once(p: &PromptV2) -> Result<(&'static str, &Opt), String> {
    let once: Vec<&Opt> = p.options.iter().filter(|o| o.role == Role::Once).collect();
    if let [o] = once.as_slice()
        && !buys(o)
    {
        return Ok((RULE_ALLOW_ONCE, o));
    }
    let sound = p
        .options
        .iter()
        .any(|o| o.role != Role::Other && !o.role.standing());
    if sound
        && let Some(o) = p
            .options
            .iter()
            .find(|o| o.affirms() && !o.role.standing() && !buys(o))
    {
        return Ok((RULE_ALLOW_ONCE, o));
    }
    if p.options.iter().any(buys)
        && let Some(o) = p.options.iter().find(|o| waits(o) && !buys(o))
    {
        return Ok((RULE_NO_SPEND, o));
    }
    p.with_role(Role::Deny)
        .map(|o| (RULE_DECLINE, o))
        .ok_or_else(|| {
            format!(
                "no one-shot allow, nothing that waits and no `No` among the options ({})",
                p.options
                    .iter()
                    .map(|o| format!("{}:{}", o.role.name(), o.label))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn choice_of(p: &PromptV2, o: &Opt) -> Result<Choice, String> {
    match p.select {
        Select::Digits if amend_input_open(p) => Err(OPEN_INPUT.to_string()),
        Select::Digits => {
            o.n.map(Choice::Digit)
                .ok_or_else(|| format!("the option `{}` has no number", o.label))
        }
        Select::ArrowsEnter => {
            let at = p
                .options
                .iter()
                .position(|q| q.focused)
                .ok_or_else(|| "an unnumbered box with no focused option".to_string())?;
            let to = p
                .options
                .iter()
                .position(|q| std::ptr::eq(q, o))
                .unwrap_or(at);
            Ok(Choice::Focus {
                steps: i32::try_from(to).unwrap_or(0) - i32::try_from(at).unwrap_or(0),
                label: o.label.clone(),
            })
        }
    }
}

fn unbar(row: &str) -> Option<&str> {
    row.strip_prefix('│')
        .and_then(|t| t.strip_suffix('│'))
        .map(str::trim)
}

fn judged_row(p: &PromptV2, rows: &[String]) -> usize {
    let own = match p.kind {
        PromptKind::Bash | PromptKind::PowerShell => p.command_rows.first().and_then(|c| {
            let (top, bottom) = p.span;
            row_of(rows, p, c)
                .or_else(|| {
                    (top + 1..bottom.min(rows.len())).find(|&k| {
                        rows[k]
                            .trim()
                            .strip_suffix(c.as_str())
                            .is_some_and(|glyph| !glyph.is_empty() && glyph.trim_end() == "$")
                    })
                })
                .or_else(|| {
                    (top + 1..bottom.min(rows.len()))
                        .find(|&k| unbar(rows[k].trim()).and_then(unbar) == Some(c.as_str()))
                })
        }),
        PromptKind::Trust => (p.span.0 + 1..p.span.1.min(rows.len())).find(|&k| {
            let t = rows[k].trim();
            t.starts_with('/') || t.starts_with('~')
        }),
        PromptKind::NetworkRequest => row_of(
            rows,
            p,
            &format!("{}{}", aterm_phase::anchor!("muse.box.full_url"), p.command),
        ),
        _ => p
            .path
            .as_deref()
            .and_then(|path| file_path_row(rows, p, path))
            .or(p.question.filter(|&q| q < rows.len() && p.path.is_some())),
    };
    own.unwrap_or(p.span.0)
}

fn limit_reset(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Option<Decision> {
    if !ctx.limit_reset {
        return None;
    }
    let row = aterm_phase::reset_row(prompt, &ctx.reset_kinds)?;
    let title = aterm_phase::anchor!("limit.title");
    if prompt.title != title {
        return None;
    }
    let at = prompt.options.iter().position(|o| o.focused)?;
    let title_row = aterm_phase::prompt::title_row(prompt, rows)?;
    Some(Decision::Approve {
        rule_id: RULE_LIMIT_RESET,
        choice: Choice::Focus {
            steps: focus_steps(at, row.index),
            label: row.label.clone(),
        },
        guard: row_guard(title_row),
        subject: row.label,
        unproven: None,
    })
}

fn limit_wait(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if !ctx.limit_wait {
        return Decision::escalate("a usage-limit dialog (limit_wait is off)");
    }
    let title = aterm_phase::anchor!("limit.title");
    if prompt.title != title {
        return Decision::escalate(format!(
            "a usage-limit dialog whose title is not `{title}`: {}",
            prompt.title
        ));
    }
    let waits: Vec<usize> = (0..prompt.options.len())
        .filter(|&k| prompt.options[k].role == Role::AutoResume)
        .collect();
    let [target] = waits.as_slice() else {
        return Decision::escalate(format!(
            "a usage-limit dialog with {} `{} …` row{} ({}): only that row is chosen, never \
             stop, usage credits, an upgrade or a reset claim",
            if waits.is_empty() {
                "no"
            } else {
                "more than one"
            },
            aterm_phase::anchor!("limit.wait"),
            if waits.is_empty() { "" } else { "s" },
            prompt
                .options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    };
    if buys(&prompt.options[*target]) {
        return Decision::escalate("a usage-limit dialog whose wait row reads as a purchase");
    }
    let Some(at) = prompt.options.iter().position(|o| o.focused) else {
        return Decision::escalate("a usage-limit dialog with no focused option");
    };
    let Some(title_row) = aterm_phase::prompt::title_row(prompt, rows) else {
        return Decision::escalate("the usage-limit dialog's title is not on the screen");
    };
    let label = prompt.options[*target].label.clone();
    Decision::Approve {
        rule_id: RULE_LIMIT_WAIT,
        choice: Choice::Focus {
            steps: focus_steps(at, *target),
            label: label.clone(),
        },
        guard: row_guard(title_row),
        subject: label,
        unproven: None,
    }
}

fn autocompact_offer(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    let Some(target) = ctx.autocompact_offer else {
        return Decision::escalate("an autocompact offer (autocompact_offer is off)");
    };
    if !aterm_phase::autocompact::LADDER.contains(&target) {
        return Decision::escalate(format!(
            "an autocompact offer, and autocompact_offer = {target}k is no size it offers"
        ));
    }
    let Some(offer) = prompt.autocompact_offer() else {
        return Decision::escalate("an autocompact offer not read whole");
    };
    let Some(title_row) = aterm_phase::prompt::title_row(prompt, rows) else {
        return Decision::escalate("the autocompact offer's title is not on the screen");
    };
    let at = match offer.focused {
        aterm_phase::OfferFocus::Accept => offer.accept,
        aterm_phase::OfferFocus::Keep => offer.keep,
    };
    let steps = focus_steps(at, offer.accept);
    let label = prompt.options[offer.accept].label.clone();
    let target_tokens = target.saturating_mul(1000);
    let (choice, subject) = if offer.tokens == target_tokens {
        (
            Choice::Focus {
                steps,
                label: label.clone(),
            },
            format!("{label} (autocompact at {target}k)"),
        )
    } else {
        let key = if offer.tokens < target_tokens {
            "right"
        } else {
            "left"
        };
        (
            Choice::FocusKey {
                steps,
                label: label.clone(),
                key,
            },
            format!(
                "{label}: one step {key}, from {} toward {target}k",
                aterm_phase::autocompact::size_words(offer.tokens)
            ),
        )
    };
    Decision::Approve {
        rule_id: RULE_AUTOCOMPACT_OFFER,
        choice,
        guard: row_guard(title_row),
        subject,
        unproven: None,
    }
}

/// Claude Code's `Teach auto mode about your environment?` offer (2.1.310), answered `Not now` at
/// every level: it puts the offer off a week and changes nothing else. `Don't show again` would
/// turn the offer off for the person for good, and `Yes` starts a minute-long scan of the project
/// and the person's sessions that nobody asked for.
fn auto_mode_setup(prompt: &PromptV2, rows: &[String]) -> Decision {
    let Some(later) = prompt.auto_mode_later() else {
        return Decision::escalate("an auto-mode setup offer not read whole");
    };
    let (Some(n), Select::Digits) = (later.n, prompt.select) else {
        return Decision::escalate("an auto-mode setup offer whose `Not now` takes no digit");
    };
    let Some(title_row) = aterm_phase::prompt::title_row(prompt, rows) else {
        return Decision::escalate("the auto-mode setup offer's title is not on the screen");
    };
    Decision::Approve {
        rule_id: RULE_AUTO_MODE_SETUP,
        choice: Choice::Digit(n),
        guard: row_guard(title_row),
        subject: format!("{}{SUBJECT_OPTION}{}", prompt.title, later.label),
        unproven: None,
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
pub(crate) mod tests;

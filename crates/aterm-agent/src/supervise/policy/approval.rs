// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Approval policy: turn a parsed agent dialog into a guarded [`Decision`].
//! [`decide`] is the entrypoint; [`decide_traced`] runs the same checks while
//! recording why they answered or refused. The caller supplies the [`Reading`],
//! screen rows and [`ApprovalCtx`]. The loop executes keys; this module only
//! chooses a guarded action, amended refusal or escalation.
//!
//! The decision order is implemented in [`decide_box`]:
//!
//! * A program outside `Program::supervisable` is read but never answered.
//!   Decisions require the agent reader's authoritative prompt phase.
//! * Usage-limit dialogs use [`limit_reset`] when explicitly enabled, else
//!   [`limit_wait`]. The grant pick ([`aterm_phase::reset_row`]: the one
//!   reset on offer, of a kind the step takes, or the first kind of the
//!   owner's `limit_reset_kinds` on offer, never a Fable reset) is not
//!   spending authority: the loop asks the host before its first key and
//!   fences the confirmation separately ([`super::reset`]); success is
//!   recorded only for the claimed raw wall.
//! * Usage-credits consent uses [`credits_consent`] and the default-off
//!   `usage_credits` grant. Without it, [`credits_decline`] cancels only a
//!   recognized consent; purchases and re-enabling account credits stay refused.
//!   An enabled model consent may persist for that model and account.
//! * [`autocompact_offer`] uses its own configured target. It is an explicit
//!   standing-setting choice, independent of the permission level.
//! * An owned login carry may choose the existing sign-in method
//!   ([`super::login`]); a person's login or Console choice stays theirs.
//! * Questions use [`super::question::answer_question`] and their own enable
//!   switch/priority, independently of `approve`. Their cancel and checked
//!   options are part of the structured question, not permission roles.
//! * MCP server choices, owner ask rules/hooks and forced review are held
//!   before the permission level. Unread/cut heads are held too, except the
//!   narrowly guarded removal-breaker refusal in [`tall_box`].
//!
//! Ordinary permission levels then apply:
//!
//! * [`Approve::None`] escalates ordinary permission boxes; it does not turn
//!   off the separately configured dialog policies above.
//! * [`Approve::Safe`] uses [`decide_safe`]: Claude Code command reads
//!   ([`bash`]), bounded scratch removals ([`super::rm_breaker`]), allowed
//!   non-secret reads ([`read_box`]) and cwd/trust-root grants ([`trust_dialog`]).
//!   Command ambiguity, shell startup and effective git configuration are
//!   checked, not inferred from an apparently read-only command. An enabled
//!   [`rm_breaker_warn`] can still approve a removal with an unproven warning.
//! * [`Approve::All`] (the default) tries those rules first, then
//!   [`full_power`]. It prefers a one-shot allow or a non-persistent refusal,
//!   with explicit plan/model/consent rules. It still respects reader and
//!   per-vendor capability limits ([`unmeasured_box`]); an unread or
//!   unsupported dialog is not automatically safe merely because All is set.
//!   The failed safe-rule reason accompanies an unproven approval in its ledger.
//!
//! Every decision then passes two final checks:
//!
//! * [`never_spends`] reserves limit-reset spending for [`RULE_LIMIT_RESET`].
//!   Other answers inspect the actual option labels, including the selected
//!   or checked options of structured questions. A reset confirmation permits
//!   only its refusal outside the reset step, even if its yes is reworded.
//! * [`kept_or_bought`] rejects ordinary picks that purchase or keep a standing
//!   answer ([`buys`], [`kept_for_good`]); questions have their own equivalent
//!   checks. Folder trust and the configured autocompact target are deliberate
//!   standing choices, not permission grants inferred from an unknown label.
//!
//! An approval carries the judged row's [`row_guard`]. The surrounding
//! `supervise::approval_loop` adds fresh-read, generation and person fences,
//! and waits for the answered box to leave before pressing it again. A
//! [`Decision::Decline`] instead carries feedback through
//! [`decline_step`]; its refusal-input semantics are documented there.
//!
//! Filesystem, settings, shell and git checks use the supplied worker/probe
//! context. A path observation does not constrain a worker's later file open.
//! Dialog grammar and capture evidence live in `aterm-phase`; local
//! `approval_tests.rs` exercises this policy, including the four recorded
//! `fixtures/cap-decline-*.txt` states. Contracts map to those fixtures in
//! `crates/aterm-agent/SPECS.md`. The private vendor lab is `aterm-lab`;
//! source templates and composed fixtures do not imply a live capture.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_phase::prompt::{
    Opt, PromptKind, PromptV2, RmBreaker, RmBreakerKind, RmCommand, Role, Select, asks, cut_short,
    kept_answer, rm_breaker_of, settles_later,
};
use aterm_phase::{Phase, Program, Reading, anchor};

use super::super::config::{Approve, SupervisorConfig};
use super::a_or_an;
use super::claude_settings::{AskKind, OwnerAsk};
use super::git_config::{GitView, WorkerEnv, git_reads, hazard};
use super::guard::row_guard;
use super::probe::{PolicyProbe, WorkerWrites};
use super::rm_breaker::{RmScope, ScratchRoot, abs_components, clip, resolve_rm_line};
use super::shell_startup::ShellStartup;
use super::trace::{NoTrace, Outcome, Trace, note};
use crate::harness::lights::Mode;
use crate::supervise::classify::{
    classify_command_with, classify_except_rm, glob_match, plain_zsh_line,
};

/// The read-only Bash rule's id (the classifier generation it trusts).
pub const RULE_READ_ONLY: &str = "read-only@classify-v3";
/// The rm circuit-breaker rule's id.
pub const RULE_RM_BREAKER: &str = "rm-breaker@v3";
/// The rm circuit breaker the safe rules could not prove, approved with a
/// warning under `approve = "safe"` ([`rm_breaker_warn`]): the one-shot allow
/// pressed, the failed proof kept as `unproven` and said as one `WARN` line
/// (owner, 2026-09-29).
/// `[harness] rm_breaker_ask = true` ([`ApprovalCtx::rm_breaker_warn`] off)
/// hands it to a person.
pub const RULE_RM_BREAKER_WARN: &str = "rm-breaker-warn@v1";
/// The Read-outside-cwd rule's id.
pub const RULE_READ_OUTSIDE_CWD: &str = "read-outside-cwd@v2";
/// The folder-trust dialog rule's id.
pub const RULE_TRUST_DIALOG: &str = "trust-dialog@v1";
/// Full power: a permission box's one-shot allow.
pub const RULE_ALLOW_ONCE: &str = "allow-once@v1";
/// Full power: the folder-trust dialog, any folder.
pub const RULE_TRUST_ANY: &str = "trust-any@v1";
/// Full power: plan mode's approval (or its entry), its yes that grants no
/// standing mode ([`plan_pick`]).
pub const RULE_PLAN: &str = "plan-approve@v2";
/// Full power: a box whose every yes would buy something, answered with the
/// option that waits.
pub const RULE_NO_SPEND: &str = "no-spend@v1";
/// Full power under `[harness] usage_credits`: a consent onto usage credits
/// ([`credits_consent`]), its yes — kept for good per model and account where
/// it names a model; the owner opted into that.
pub const RULE_CREDITS_CONSENT: &str = "credits-consent@v1";
/// Full power with `[harness] usage_credits` off, the default: a consent onto
/// usage credits answered with Esc, the dialog's cancel, which saves nothing
/// ([`credits_decline`]).
pub const RULE_CREDITS_DECLINE: &str = "credits-decline@v1";
/// Full power: a box with no yes it may take (a standing grant, a purchase)
/// and nothing that waits, answered with the refusal that settles nothing.
pub const RULE_DECLINE: &str = "decline@v1";
/// Full power: Claude Code's model-refusal pause (`Session paused`) alone, its
/// `Switch to <model>` while `[harness] model_fallback` is set
/// ([`ApprovalCtx::model_fallback`], [`refusal_pause_switch`]).
pub const RULE_MODEL_SWITCH: &str = "model-switch@v1";
/// A box taller than the screen in a bypass session, DECLINED with what its
/// foot note flags ([`tall_box`]): it never approves.
pub const RULE_TALL_BOX: &str = "tall-box@v1";
/// Full power: the model-switch confirmation's `Yes, switch to <m>` — the
/// `/model` or `/effort` change the person typed ([`model_confirm`]).
pub const RULE_MODEL_CONFIRM: &str = "model-confirm@v1";
/// The usage-limit dialog's rule ([`limit_wait`]): its `Wait here, then
/// continue automatically …` row, at every level, under
/// [`ApprovalCtx::limit_wait`].
pub const RULE_LIMIT_WAIT: &str = "limit-wait@v1";
/// The usage-limit dialog's grant row ([`limit_reset`]): the one reset on
/// offer, of a kind the step takes, or the first kind of the owner's list on
/// offer ([`ApprovalCtx::reset_kinds`]), `Use a <5-hour|weekly|full> reset
/// (<n> left, until <day>)`, at every level, only under
/// [`ApprovalCtx::limit_reset`]. The one
/// rule that may choose an option that spends a reset ([`never_spends`]).
/// The ledger's Enter on its confirmation names that kind's yes (`Yes, use
/// my weekly reset`; an older build's `Yes, use my reset`).
pub const RULE_LIMIT_RESET: &str = "limit-reset@v1";
/// Claude Code's autocompact offer ([`autocompact_offer`]): `Autocompact at ‹
/// <size> › tokens` at the size `[harness] autocompact_offer` names, at every
/// level, under [`ApprovalCtx::autocompact_offer`].
pub const RULE_AUTOCOMPACT_OFFER: &str = "autocompact-offer@v1";

/// A question dialog answered under the priority word `long-term`
/// ([`QuestionPriority::LongTerm`]): the worker decides by
/// [`super::question::LONG_TERM_TEXT`], typed into the free-text row.
pub const RULE_ANSWER_LONG_TERM: &str = "answer-long-term@v1";
/// The same under `short-term` ([`super::question::SHORT_TERM_TEXT`]).
pub const RULE_ANSWER_SHORT_TERM: &str = "answer-short-term@v1";
/// A prose question answered under `long-term` with its policy text instead of
/// `[harness] answer_text` (the turn-end counterpart of
/// [`RULE_ANSWER_LONG_TERM`]).
pub const RULE_PROSE_LONG_TERM: &str = "prose-long-term@v1";
/// The same under `short-term`.
pub const RULE_PROSE_SHORT_TERM: &str = "prose-short-term@v1";

/// How a question is answered, once answered at all ([`ApprovalCtx::answer_questions`]
/// decides whether): `[harness] question_priority`, or a session's `questions`
/// word (`aterm ctl @<sid> meta set questions …`) over it.
///
/// * [`Self::Recommended`] (default): a dialog's recommended option by Enter
///   ([`super::question::RULE_ANSWER_RECOMMENDED`]); a prose question with
///   `[harness] answer_text` (`answer@v1`).
/// * [`Self::LongTerm`] / [`Self::ShortTerm`]: the supervisor writes the word's
///   fixed policy text ([`Self::text`]) into a dialog's free-text row and
///   presses Enter, so the worker decides by that policy
///   ([`RULE_ANSWER_LONG_TERM`], [`RULE_ANSWER_SHORT_TERM`]); prose questions
///   get the same text ([`RULE_PROSE_LONG_TERM`], [`RULE_PROSE_SHORT_TERM`]).
///   The text is a constant, never from config or screen. A tab without a
///   free-text row, or where the text would not fit, gets its recommended
///   option and says so (`policy/question.rs`, `choose_text`); a question
///   naming an irreversible act is answered as under `recommended`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum QuestionPriority {
    /// The recommended option(s).
    #[default]
    Recommended,
    /// The worker decides for long-term correctness and total value.
    LongTerm,
    /// The worker decides for the current deliverable, soonest.
    ShortTerm,
}

impl QuestionPriority {
    /// Every priority, in [`Self::WORDS`]' order.
    pub const ALL: [Self; 3] = [Self::Recommended, Self::LongTerm, Self::ShortTerm];
    /// The words the config key and the session word spell them with, in
    /// [`Self::ALL`]'s order (the Settings row lists them so).
    pub const WORDS: [&'static str; 3] = ["recommended", "long-term", "short-term"];

    /// The priority `word` names (trimmed, ASCII case-folded), or `None` for
    /// any other text (`ask` is a session's word for not answering).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        let word = word.trim();
        Self::ALL
            .into_iter()
            .find(|p| p.word().eq_ignore_ascii_case(word))
    }

    /// Its word: `recommended`, `long-term`, `short-term`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Recommended => Self::WORDS[0],
            Self::LongTerm => Self::WORDS[1],
            Self::ShortTerm => Self::WORDS[2],
        }
    }

    /// The rule a question dialog answered under it is ledgered as.
    #[must_use]
    pub fn rule_id(self) -> &'static str {
        match self {
            Self::Recommended => super::question::RULE_ANSWER_RECOMMENDED,
            Self::LongTerm => RULE_ANSWER_LONG_TERM,
            Self::ShortTerm => RULE_ANSWER_SHORT_TERM,
        }
    }

    /// The rule a prose question answered under it is ledgered as: `answer@v1`
    /// under [`Self::Recommended`], else the word's own.
    #[must_use]
    pub fn prose_rule_id(self) -> &'static str {
        match self {
            Self::Recommended => super::turn_end::RULE_ANSWER,
            Self::LongTerm => RULE_PROSE_LONG_TERM,
            Self::ShortTerm => RULE_PROSE_SHORT_TERM,
        }
    }

    /// The fixed policy text written into a free-text row (and used for prose);
    /// `None` for [`Self::Recommended`].
    #[must_use]
    pub fn text(self) -> Option<&'static str> {
        match self {
            Self::Recommended => None,
            Self::LongTerm => Some(super::question::LONG_TERM_TEXT),
            Self::ShortTerm => Some(super::question::SHORT_TERM_TEXT),
        }
    }
}

/// What every decline's text opens with ([`decline_text`]): Claude Code presents
/// the text as the user's words, so it names who said them.
pub const DECLINE_PREFIX: &str = "aterm harness (not the user): ";

/// The refusal a decline amends: the Bash box's `No`, whose input is
/// opened by [`decline_step`].
pub const REFUSAL_LABEL: &str = "No";

/// Maximum characters of a reason or quoted command in a decline's text, which
/// is typed and read back before Enter.
const DECLINE_WHY_CHARS: usize = 160;

/// How the approving option is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Press this digit: a numbered box ([`Select::Digits`]).
    Digit(u8),
    /// Move the focus `steps` options (down when positive), then press Enter
    /// once a fresh read shows it on `label`: for unnumbered dialogs
    /// ([`Select::ArrowsEnter`]) and the usage-limit wait row, never chosen by
    /// digit ([`limit_wait`]). `steps` 0: already focused.
    Focus { steps: i32, label: String },
    /// A question dialog's answer ([`QuestionPriority::rule_id`]): every key
    /// under the screen-generation fence, never a digit.
    Answer(Answer),
    /// Move the focus as [`Self::Focus`], then press `key` (not Enter) once a
    /// fresh read shows it on `label`: the autocompact offer's `left` or
    /// `right` toward the owner's size.
    FocusKey {
        steps: i32,
        label: String,
        key: &'static str,
    },
    /// A question read by its answers' roles (Codex's, Muse Code's) answered
    /// under a text priority: `text`'s fixed words written as the note of
    /// `None of the above` — the focus moved there with arrows, Tab, the
    /// text, Enter once a read shows it — one keystroke at a time, each
    /// decided on a fresh read ([`super::question::note_step`]). Never a
    /// digit, never Esc.
    Note { text: QuestionPriority },
}

/// How a question dialog is answered ([`Choice::Answer`]): move the focus, then
/// Enter; under a text priority the policy text is first written into the
/// free-text row ([`Self::Type`], a decision of its own). A digit is never sent:
/// one landing late does harm (answers the next tab, cancels on the review
/// tab, leaves a draft in the composer), while a late Enter lands harmlessly.
/// The policy text holds no digit or newline.
///
/// Read it through [`Self::enter_target`], [`Self::typed`] and [`Self::steps`],
/// not by destructuring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Move the focus `steps` rows in the dialog's focus order (options,
    /// free-text row, multi-select button, chat row; the review's two rows),
    /// one key at a time, each seen landing, then Enter once a fresh read shows
    /// the focus on `target`, guarded on the focused row. Usually 0.
    FocusEnter { steps: i32, target: AnswerTarget },
    /// Move the focus `steps` rows onto the pristine free-text row, then write
    /// the priority's fixed text ([`QuestionPriority::text`]); never Enter,
    /// which is a later decision on a read showing exactly that text
    /// ([`AnswerTarget::OwnText`]).
    Type { steps: i32, text: QuestionPriority },
}

impl Answer {
    /// Where the answer's Enter goes; `None` for [`Self::Type`], which
    /// presses none.
    #[must_use]
    pub fn enter_target(&self) -> Option<AnswerTarget> {
        match self {
            Self::FocusEnter { target, .. } => Some(*target),
            Self::Type { .. } => None,
        }
    }

    /// The priority whose policy text the answer writes; `None` for
    /// [`Self::FocusEnter`], which writes none.
    #[must_use]
    pub fn typed(&self) -> Option<QuestionPriority> {
        match self {
            Self::FocusEnter { .. } => None,
            Self::Type { text, .. } => Some(*text),
        }
    }

    /// The rows the focus is moved first (down when positive).
    #[must_use]
    pub fn steps(&self) -> i32 {
        match self {
            Self::FocusEnter { steps, .. } | Self::Type { steps, .. } => *steps,
        }
    }
}

/// Where a question answer's Enter goes ([`Answer::FocusEnter`]): never the
/// free-text row except [`Self::OwnText`], never the chat row or the review's
/// cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerTarget {
    /// The model's own option `n` (single-select and preview: answers it;
    /// multi-select: toggles its checkbox).
    Option(u8),
    /// The multi-select button (`Next`, or `Submit` on the last question).
    Button,
    /// The review tab's `1. Submit answers`.
    ReviewSubmit,
    /// A single-select question's free-text row holding exactly this priority's
    /// text, which the supervisor wrote ([`Answer::Type`]): Enter submits it.
    /// Proving it is the supervisor's own is the loop's (`approval_loop.rs`). A
    /// multi-select's text is never entered here (Enter unticks it); its button
    /// is.
    OwnText(QuestionPriority),
}

/// What to do with one box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Choose `choice` under `guard`.
    Approve {
        /// Which rule approved (a safe rule, one of full power's,
        /// [`RULE_LIMIT_WAIT`], or a question's:
        /// [`super::question::is_question_rule`]).
        rule_id: &'static str,
        /// The option, and how it is chosen.
        choice: Choice,
        /// The anchored `key if=` pattern of the judged row
        /// ([`super::guard::row_guard`]).
        guard: String,
        /// What was judged, as the ledger and the journal's `APPROVED` line
        /// name it: the command or path (rm rule: ` => ` resolved targets;
        /// trust backstop: warnings in parentheses; full power: the box's
        /// subject and ` => ` the option chosen).
        subject: String,
        /// Under full power, why the safe rules did not prove the box (the
        /// breaker kind first, and `vendor default was No` where it applies):
        /// the ledger's and journal's `unproven:`. `None` when proven.
        unproven: Option<String>,
    },
    /// Refuse the box with feedback: `No` amended with `text`, never the
    /// bare `No`. Built only by
    /// [`decline`]; the loop carries it out one keystroke at a time via
    /// [`decline_step`].
    Decline {
        /// The rule that declined.
        rule_id: &'static str,
        /// The refusal's index in the box's options ([`refusal_of`]).
        refusal: usize,
        /// What the worker is told ([`decline_text`]).
        text: String,
        /// What was judged: what the ledger row and the journal's `DECLINED`
        /// line name.
        subject: String,
    },
    /// Hand the box to a person, with the reason.
    Escalate {
        /// Why no rule approved or declined it.
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
    pub fn rule_id(&self) -> Option<&'static str> {
        match self {
            Self::Approve { rule_id, .. } | Self::Decline { rule_id, .. } => Some(rule_id),
            Self::Escalate { .. } => None,
        }
    }
}

/// A path a Read box may not be approved for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRule {
    /// This absolute directory (or file) and everything under it.
    Under(PathBuf),
    /// Any path with a component of exactly this name (`.ssh` anywhere).
    Component(String),
    /// Any path whose last component matches this glob (`*.pem`, `.env*`).
    Basename(String),
}

/// The default secrets list: key and credential stores, key and token file
/// shapes (including aterm's `*.token`), shell histories, and under `home`
/// all of `~/Library` and tool credential directories. Checked on top of the
/// allow-list ([`ApprovalCtx::read_roots`]).
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
        for sub in [
            ".config/gh",
            ".claude.json",
            "Library",
            ".cargo/credentials",
        ] {
            out.push(SecretRule::Under(h.join(sub)));
        }
    }
    out
}

/// Roots from config text (`[harness] trust_roots`): `~/` is `home`, a trailing
/// `*` matches any suffix of the last component (`~/src/app*`), resolved
/// lexically. A spec not absolute after `~`, holding `..`, or globbing
/// elsewhere is dropped, never widened.
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
    /// The session's working directory as the server reports it (`meta cwd=`,
    /// OSC 7): where the agent was launched, not the Bash tool's cwd.
    pub cwd: PathBuf,
    /// Whether [`Self::cwd`] was reported (`meta cwd=-` and an unread meta
    /// are not): the trust dialog and the rm breaker need it.
    pub cwd_known: bool,
    /// The user's home directory (`~` in a Read path; the rm deny table).
    pub home: Option<PathBuf>,
    /// Whether the session runs in bypass mode, from its footer as last read
    /// ([`footer_mode`]); the box covers the footer while up.
    pub bypass_mode: bool,
    /// What may be answered (`[harness] approve`).
    pub approve: Approve,
    /// Whether a question dialog is answered
    /// ([`super::question::answer_question`], `[harness] answer_questions`),
    /// regardless of [`Self::approve`].
    pub answer_questions: bool,
    /// Whether full power may switch model on Claude Code's model-refusal pause
    /// ([`RULE_MODEL_SWITCH`]): `[harness] model_fallback` set.
    pub model_fallback: bool,
    /// Whether the usage-limit dialog's wait row is chosen ([`RULE_LIMIT_WAIT`],
    /// `[harness] limit_wait`), regardless of [`Self::approve`].
    pub limit_wait: bool,
    /// Whether the usage-limit dialog's grant row is chosen
    /// ([`RULE_LIMIT_RESET`], `[harness] limit_reset`), regardless of
    /// [`Self::approve`] and [`Self::limit_wait`]. Off unless the loop sets
    /// it: with it off the dialog is the wait row's, as it was before a reset
    /// could be taken.
    pub limit_reset: bool,
    /// The kinds of reset [`Self::limit_reset`] may spend, the owner's first
    /// first (`[harness] limit_reset_kinds`); empty, the dialog's one reset
    /// on offer ([`aterm_phase::reset_row`]).
    pub reset_kinds: Vec<aterm_phase::ResetKind>,
    /// Whether full power takes a consent that carries the session onto
    /// usage credits ([`credits_consent`], `[harness] usage_credits`). Off,
    /// the default: such a box is cancelled with Esc where it reads one
    /// ([`credits_decline`]), else goes to a person. A consent that names no
    /// credits is outside it.
    pub usage_credits: bool,
    /// The size Claude Code's autocompact offer is answered with, in
    /// kilo-tokens ([`RULE_AUTOCOMPACT_OFFER`], `[harness]
    /// autocompact_offer`), regardless of [`Self::approve`]; `None`: the
    /// offer is a person's.
    pub autocompact_offer: Option<u32>,
    /// THE `/login` METHOD PICKER IS THE TURN-END POLICY'S CARRY: the loop
    /// typed the `/login` it belongs to into a lost login
    /// (`crate::supervise::policy::turn_end::TurnEndState::login_typed_at`).
    /// `false`: a person's own `/login`, whose picker is theirs whatever
    /// [`Self::login_method`] says.
    pub login_carry: bool,
    /// THE `/login` METHOD PICKER OF THE LOOP'S OWN `/login`, ONCE THE LOGIN
    /// CAME BACK IN ANOTHER TAB ([`super::login::PickerGate::Back`]): nothing
    /// is left to sign in, so it is answered by nobody — a method chosen
    /// would open a browser sign-in nobody needs — and the owner closes it.
    pub login_back: bool,
    /// THE `/login` METHOD PICKER OF THE LOOP'S OWN `/login`, ONCE A RENEWAL
    /// THE TAB FOLLOWS BY ITSELF REPLACED THE LOGIN IT SIGNS IN
    /// ([`super::login::PickerGate::Renewed`]): answered by nobody — a method
    /// chosen would open a browser sign-in that brings back the replaced
    /// login — and the owner closes it.
    pub login_renewed: bool,
    /// THE METHOD THIS SESSION'S LOGIN ALREADY IS (`crate::supervise::IdleHost::login_method`),
    /// which the picker of the loop's own `/login` ([`Self::login_carry`]) is
    /// answered with ([`super::login::pick_method`],
    /// [`super::login::RULE_LOGIN_METHOD`]), regardless of [`Self::approve`]:
    /// the picker is no permission. `None`: nothing says, and the picker is
    /// the owner's.
    pub login_method: Option<aterm_phase::LoginMethod>,
    /// Where a folder-trust dialog's folder may be trusted
    /// ([`RULE_TRUST_DIALOG`]).
    pub trust_roots: Vec<ScratchRoot>,
    /// The ONE folder this session's own starter trusted for it: the `cwd=`
    /// an Owner's `spawn agent=claude` named (`meta`'s `trusted_cwd=`), where
    /// a folder-trust dialog for exactly that folder is trusted as if under a
    /// trust root — never a folder beneath it, and no other session's
    /// ([`RULE_TRUST_DIALOG`]). `None` for every session no such spawn started.
    pub trusted_cwd: Option<PathBuf>,
    /// Where a Read box may read ([`RULE_READ_OUTSIDE_CWD`]): the trust
    /// roots and the system roots ([`Self::new`]).
    pub read_roots: Vec<ScratchRoot>,
    /// Where an rm operand may point ([`RULE_RM_BREAKER`]). Empty withholds the
    /// rule, and no rm is proven ([`RmScope::roots`]).
    pub scratch_roots: Vec<ScratchRoot>,
    /// Under `approve = "safe"`, whether an unprovable `rm` breaker on a
    /// possibly-empty variable is approved with a warning
    /// ([`RULE_RM_BREAKER_WARN`], default `true`) or handed to a person
    /// (`[harness] rm_breaker_ask = true`, or legacy `rm_breaker = false`).
    pub rm_breaker_warn: bool,
    /// What a Read box may not be approved for, under an allowed root.
    pub secrets: Vec<SecretRule>,
    /// The `python3 <script>` globs the classifier treats as reads (none by
    /// default).
    pub python_allow: Vec<String>,
    /// How a question dialog is answered when [`Self::answer_questions`] is on:
    /// the session's `questions` word, else `[harness] question_priority`
    /// (default [`QuestionPriority::Recommended`]).
    pub question_priority: QuestionPriority,
    /// The priority whose text the loop wrote into this question's free-text row
    /// and remembers writing; `None` when none
    /// (or forgotten after a restart or handover). The same text with `None`
    /// here is a person's.
    pub question_own: Option<QuestionPriority>,
    /// The pane's width in cells (`text --json` `dims.cols`, else the widest
    /// row); `0` makes [`decide`] use the widest row. Policy text is typed only
    /// where its wrap fits (`policy/question.rs`, `text_fits`).
    pub screen_cols: u16,
    /// The pane's height in rows (`text --json` `dims.rows`, else the grid row
    /// past the last read), never the `tail=` count; `0` makes [`decide`] use
    /// the count of `rows`.
    pub screen_rows: usize,
    /// The loop already fell back to this tab's recommended option under a text
    /// priority (`policy/question.rs`, `falls_back`) and latched it, so the fit
    /// is not re-estimated.
    pub question_fell_back: bool,
    /// The worker's environment ([`WorkerEnv`]) for reading git configuration
    /// ([`super::git_config`]), or why it is unreadable (every git read then
    /// escalates). Read by the loop only for a box that may run git
    /// ([`worker_unread`]).
    pub worker: Result<WorkerEnv, String>,
    /// Where the session's Bash tool may stand beyond [`Self::cwd`], from
    /// Claude Code's transcripts ([`crate::harness::footer::shell_cwds`]); git
    /// reads are checked there too. Empty until the loop reads it.
    pub shell_cwds: Vec<PathBuf>,
    /// What the worker's Bash shell starts with beyond the modelled defaults
    /// ([`super::shell_startup::read`]): aliases and functions failing lines
    /// that name them, or the option, variable or file failing every line, or
    /// why it is unreadable. Read by the loop only for a Bash box
    /// ([`shell_unread`]).
    pub shell: Result<ShellStartup, String>,
    /// The ask rules and PreToolUse hooks of the agent's own Claude Code
    /// settings ([`super::claude_settings::owner_asks`]), or why they could
    /// not be read: what holds a box Claude Code draws no ask reason on
    /// ([`unvoiced_tools`], [`settings_ask`]). Read by the loop only for such
    /// a box ([`asks_unread`]).
    pub owner_asks: Result<Vec<OwnerAsk>, String>,
    /// How the policy looks at the machine while it decides
    /// ([`super::probe`]): where a Read's symlinks lead and whether a folder
    /// on its way holds a hidden twin of a name, where an `rm` operand's links
    /// lead, the configuration a git read would load and the settings' `env`
    /// blocks. [`super::probe::LiveProbe`] looks at the disk as [`decide`] always did; a run
    /// recorded to a harness tape hands in a probe that tapes each derived
    /// answer, so a decision that turned on the disk holds what it turned on.
    pub probe: Arc<dyn PolicyProbe>,
    /// A transcript candidate for the box, without current-mount identity.
    /// The transcript may lag a visible Bash box with an older Edit pending,
    /// and screen rows do not close that gap. This never removes a possible
    /// tool's owner hold ([`settings_ask`]).
    pub box_call: Option<String>,
}

impl ApprovalCtx {
    /// Defaults for a session in `cwd` owned by `uid`: the switches and trust
    /// roots of [`SupervisorConfig::default`] (full power, questions answered,
    /// the usage-limit wait chosen, no limit reset taken and no trust root).
    /// Scratch roots: `/private/tmp/claude-<uid>/`, `tmpdir` (a root for literal
    /// paths, when at least two levels deep and holding neither `cwd` nor
    /// `home`), any `/tmp/<x>/` and `/private/tmp/<x>/`, and `<cwd>/target*`.
    /// [`Self::set_trust_roots`] replaces the trust roots. Read roots: trust
    /// roots plus `/usr`, `/etc`, `/private/etc`, `/opt/homebrew`.
    /// [`default_secrets`], cwd known, not bypass, recommended answers, pane
    /// size unknown.
    ///
    /// `probe` is how its decisions look at the machine, and it has no
    /// default: the loop hands in its door's (`Fx::policy_probe`, which tapes
    /// each derived answer when the run records), a test the live one. A
    /// context that defaulted to [`super::probe::LiveProbe`] would decide past the door
    /// whenever a caller forgot to replace it, and a tape would hold none of
    /// what those decisions turned on.
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
            login_renewed: false,
            login_method: None,
            trust_roots: Vec::new(),
            trusted_cwd: None,
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

    /// The trust roots from `[harness] trust_roots` ([`roots_from_config`]), and
    /// the Read roots rebuilt around them. A `claude-*` spec under
    /// `/private/tmp` or `/tmp` names only this uid's `claude-<uid>`. With no
    /// spec there is no trust root and the Read roots are the system's alone:
    /// no fact about a session or the vendor makes a folder safe to trust, so
    /// the folders to trust are a person's to name.
    pub fn set_trust_roots(&mut self, specs: &[String], uid: u32) {
        let specs: Vec<String> = specs.iter().map(|s| own_claude_tmp(s, uid)).collect();
        self.trust_roots = roots_from_config(&specs, self.home.as_deref());
        self.read_roots = self.trust_roots.clone();
        for sys in ["/usr", "/etc", "/private/etc", "/opt/homebrew"] {
            self.read_roots.extend(ScratchRoot::dir(Path::new(sys)));
        }
    }
}

/// [`ApprovalCtx::worker`] before the loop reads the environment: nothing, so a
/// git read escalates; in this crate's unit tests a hermetic stand-in
/// ([`WorkerEnv::hermetic`]).
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

/// [`ApprovalCtx::shell`] before the loop reads shell startup: nothing, so no
/// Bash line is proven; in this crate's unit tests the modelled defaults.
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

/// [`ApprovalCtx::owner_asks`] before the loop reads the agent's settings:
/// unknown, so a box an ask rule could have sent unread is held; in this
/// crate's unit tests no rule.
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

/// `Ok` when the worker's Bash shell starts as the rm rule and classifier model
/// it ([`ApprovalCtx::shell`], [`ShellStartup::admits`]); `Err` names what
/// breaks that, or why it is unknown. A box running on another machine has
/// startup this one cannot read.
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

/// `spec`, with a `claude-*` glob under `/private/tmp` or `/tmp` narrowed to
/// `uid`'s own `claude-<uid>` (Claude Code's per-user temp root).
fn own_claude_tmp(spec: &str, uid: u32) -> String {
    let t = spec.trim().trim_end_matches('/');
    for base in ["/private/tmp", "/tmp"] {
        if t == format!("{base}/claude-*") {
            return format!("{base}/claude-{uid}");
        }
    }
    spec.to_string()
}

/// The permission mode the footer under the composer names, read from its
/// pill (`harness::footer::pill_indicator`), or [`Mode::Manual`] for
/// `? for shortcuts` and no pill; `None` when no footer is drawn (as while a
/// box is up), so a caller keeps its last answer.
pub fn footer_mode(rows: &[String]) -> Option<Mode> {
    let tail = rows.iter().rev().filter(|r| !r.trim().is_empty()).take(3);
    for r in tail {
        if let Some(mode) = crate::harness::footer::pill_indicator(r).and_then(Mode::of_indicator) {
            return Some(mode);
        }
        if r.contains(aterm_phase::anchor("footer.shortcuts")) {
            return Some(Mode::Manual);
        }
    }
    None
}

/// Decide the box `reading` found on screen `rows` under
/// [`ApprovalCtx::approve`]: the safe rules, then under [`Approve::All`] full
/// power's answer to what they leave. `rows` are needed to guard the press on
/// the judged row and cross-check the rows around the box. Whatever is
/// decided spends no limit reset, but by the reset's own rule
/// ([`never_spends`]).
pub fn decide(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    decide_traced(reading, rows, ctx, &mut NoTrace)
}

/// [`decide`], each check it consults written to `trace` as it is consulted
/// ([`super::trace`]): the gates that hand a box to a person, the level's
/// rules, and the two checks every decision passes last ([`never_spends`],
/// THE PRESS GATE), each named by the rule that decided where one did. The
/// same function: `decide` is this one with no trace, so a trace tells of
/// the very decision the loop makes. The loop uses `NoTrace`; its reader is
/// `aterm drive replay --why` (`tape::explain`), which decides a recorded
/// box again from its mark's facts with a trace kept.
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

/// `d` as a trace states it: the act and what it was judged on, or the
/// reason it went to a person.
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

/// `d`, decided at `check`, written to `trace` (named by its rule where it
/// has one) and handed back.
fn decided_at(trace: &mut dyn Trace, check: &'static str, d: Decision) -> Decision {
    note(trace, check, d.rule_id(), || Outcome::Decided(said(&d)));
    d
}

/// Whether `o`'s answer is kept for every later ask: its role
/// ([`Role::Persist`]), its label ([`settles_later`], [`kept_answer`]) or the
/// rows it is drawn on ([`Opt::kept`]: a run of its block's rows that takes
/// in one of its own spells a kept answer whole, or, ending the block as a
/// frame half drawn may, its first two words or more), so an option whose
/// role the reader withheld (options read unsound, two cursors mid-move), or
/// whose label a narrow pane's wrap left joined too far or not far enough,
/// is still read so. The [`Opt::kept`] term is defence in depth: read sound,
/// the reader already reads such an option [`Role::Persist`]. The press
/// gate's test, the reset step's for a box it may Escape, and [`consents`]'s.
pub(crate) fn kept_for_good(o: &Opt) -> bool {
    o.role == Role::Persist || o.kept || settles_later(&o.label) || kept_answer(&o.label)
}

/// Why the option `decision` presses on `p`
/// may not be pressed, whatever rule chose it — its answer is kept for every
/// later ask ([`kept_for_good`]) or it buys something ([`buys`]). A
/// question's options are the question rule's to judge
/// ([`super::question`]), descriptions and all, and the grant row is the
/// reset's own rule's ([`RULE_LIMIT_RESET`], answered for by
/// [`never_spends`]).
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
        // Esc chooses no option: its label is only where the focus must be.
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

/// NO RULE BUT THE RESET'S OWN CHOOSES AN OPTION THAT SPENDS A LIMIT RESET
/// ([`spends`]), at any level: a check every decision of [`decide`] passes,
/// whichever rule made it (THE PRESS GATE is the other). An approval whose chosen option — the
/// one its digit numbers, or the one its focus is moved onto — spends a reset
/// is handed to a person instead, unless the rule is [`RULE_LIMIT_RESET`]
/// (decided only under [`ApprovalCtx::limit_reset`], for the grant row read
/// by its exact template). The picks pass such an option over by themselves
/// ([`buys`]); this holds for the ones that take an option by its role or its
/// place. Structured question answers also check the chosen option, or the
/// checked options submitted by a multi-select button, before any key is sent.
///
/// AND ON A RESET'S CONFIRMATION NOTHING BUT ITS REFUSAL IS CHOSEN, whatever
/// its yes is labelled ([`confirms_reset`]): the server may reword that
/// label, so there the check goes by the BOX and not by the option — an
/// approval of anything but an option that refuses or waits, a question's
/// answer among them, is handed to a person.
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

/// Whether the box `p`, read from `rows`, is a limit reset's confirmation
/// by ANY part left of it — its title, its yes, its no, a row of its body
/// ([`aterm_phase::reset_confirm`] answers anything but
/// [`aterm_phase::ConfirmRead::Not`]): the one reading the loop's own step
/// and every rule here share. The server may reword any text of that box and
/// a short pane cuts its title off, so its yes cannot be told by its label
/// alone ([`spends`]).
fn confirms_reset(p: &PromptV2, rows: &[String]) -> bool {
    !matches!(
        aterm_phase::reset_confirm(p, rows),
        aterm_phase::ConfirmRead::Not
    )
}

/// Whether choosing `opt` on a reset's confirmation KEEPS the reset: it
/// refuses or waits, and buys nothing.
fn keeps_reset(opt: &Opt) -> bool {
    (opt.role == Role::Deny || waits(opt)) && !buys(opt)
}

/// Whether `program` draws Claude Code's boxes and dialogs, the ones the safe
/// rules, the owner's Claude Code settings and the dialog rules here were
/// measured on. Any other program's box is escalated or handed over.
fn draws_claude_boxes(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => false,
    }
}

/// Why a usage-limit dialog no rule here answers goes to a person: Gemini
/// CLI's (`Usage limit reached for <model>.` over `1. Keep trying` and `2.
/// Stop`, measured on 0.46.0) offers no wait — `Keep trying` sends the turn
/// again into a quota that resets hours later, so pressed by a supervisor it
/// would be pressed again at every return, and `Stop` ends the turn — and
/// names its own `/stats model`; any other program's is not one the reader
/// vouches for. Short: an escalation's reason is cut after 120 cells, and
/// the dialog's title is already its label.
fn foreign_limit(program: Program) -> &'static str {
    match program {
        Program::Gemini => {
            "Gemini CLI's usage limit: neither Keep trying nor Stop is pressed; /stats model in \
             this tab says which limit holds it"
        }
        Program::Claude | Program::Codex | Program::Agy | Program::Muse | Program::Generic => {
            "no usage-limit dialog the reader vouches for"
        }
    }
}

/// [`decide`], before its last checks ([`never_spends`], THE PRESS GATE),
/// each check it consults written to `trace` ([`decide_traced`]).
fn decide_box(
    reading: &Reading,
    rows: &[String],
    ctx: &ApprovalCtx,
    trace: &mut dyn Trace,
) -> Decision {
    // A program the supervisor does not host
    // (`aterm_phase::Program::supervisable`: the generic reader's, and
    // Codex's, whose screen aterm reads no more) is read, never answered: every box of its is a person's,
    // whatever its reader names in it.
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
    // The usage-limit dialog is decided first by its own rules, before
    // questions and levels, so full power never sees it: the grant row where
    // the owner turned limit resets on, else the wait row.
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::UsageLimit
    {
        let decision = if !draws_claude_boxes(reading.program)
            || reading.phase != Phase::Prompt
            || !reading.phase_authoritative
        {
            Decision::escalate(foreign_limit(reading.program))
        } else {
            limit_reset(prompt, rows, ctx).unwrap_or_else(|| limit_wait(prompt, rows, ctx))
        };
        return decided_at(trace, "usage-limit", decision);
    }
    // Claude Code's autocompact offer is no permission either: answered by
    // its own rule under its own key, before questions and levels.
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
    // Claude Code's `/login` dialog is no permission either: the method
    // picker of the loop's own `/login` gets the method the session's login
    // already is, else it is the owner's; a person's own `/login` is theirs
    // whole; and Console's own menu (whose second option mints an API key)
    // is always the owner's — before questions and levels, so full power
    // never presses any option of it ([`super::login`]).
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
            aterm_phase::LoginScreen::Picker(_) if ctx.login_back => Decision::escalate(
                "the login came back in another tab: press Esc in the tab to close this sign-in",
            ),
            aterm_phase::LoginScreen::Picker(_) if ctx.login_renewed => Decision::escalate(
                "the login was renewed: press Esc in the tab to close this sign-in",
            ),
            aterm_phase::LoginScreen::Picker(_) => {
                Decision::escalate("a /login you opened: its sign-in method is yours to pick")
            }
            _ => Decision::escalate(
                "Claude Code's Console sign-in menu: its API key option is yours to choose",
            ),
        };
        return decided_at(trace, "login-method", decision);
    }
    // A question is no permission: answered under its own
    // key, before any level is consulted.
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
    // Claude Code's MCP server dialog is a person's at every level, before
    // any is consulted: each of its answers, Esc's too, is saved for good.
    // The one rule that holds it: no level's rules ([`decide_safe`],
    // [`full_power`]) ever see it.
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::McpServer
    {
        return decided_at(
            trace,
            "mcp-server",
            Decision::escalate(mcp_server_held(prompt)),
        );
    }
    // The user's own Claude Code configuration (an ask rule or hook) sends this
    // box to a person: no level presses it — read off the box's reason where
    // Claude Code draws one, else off the agent's own settings (a bare-tool
    // ask rule for a file tool; for a box a Bash `sed -i` is drawn as, any
    // Bash ask rule or Bash hook, retaining every possible tool even where
    // the screen and transcript both suggest Edit),
    // else, for an in-project Read, off the missing folder grant. Built-in
    // safety checks stay the levels' to answer.
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
    // A box not read whole waits for its head, except the narrowly guarded
    // tall-box removal refusal below.
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

/// EVERY Claude Code tool a box `p` may be a call of, where Claude Code draws
/// no ask reason on a box of its kind: a file box. Under `ask: ["Write",
/// "Edit"]` and `ask: ["Read"]` its Write, Edit and Read boxes are the boxes an
/// unruled call draws (driven 2026-10-06 on 2.1.292 and 2026-10-09 on 2.1.295;
/// an ask rule's Read outside the project carries the grant row Claude Code's
/// own does), so a bare-tool ask rule for one of them, read from the agent's
/// own settings, is what sends the box to a person ([`settings_ask`]). Each
/// bare rule is its own tool's (driven 2026-10-09 on 2.1.295 in accept-edits
/// mode, where an unruled call draws no box: `Edit` drew none for a Write or
/// a NotebookEdit, `Write` none for an Edit).
///
/// A box is no tool's name, so this lists every tool Claude Code draws it
/// for, and a bare rule for ANY of them holds it (fail-closed: the box
/// cannot say which drew it). From its file-box builder (2.1.295 and
/// 2.1.299 bundles): `Create file`, `Write file` and `Overwrite file` are
/// the Write tool's; `Edit notebook` NotebookEdit's; `Edit file` the Edit
/// tool's AND the Bash tool's, which previews a `sed -i` as that box with no
/// reason (`_simulatedSedEdit`: captured under `ask: ["Bash"]`,
/// `claude-2.1.295-box-bash-sed-edit-ask-rule.txt`, row for row an Edit's);
/// `Read file` every read-only file tool's (Read, Glob, Grep); and the IDE
/// diff, which replaces the title of any file-writing box, any of those that
/// write. Empty for every other kind: Claude Code draws the reason on a Bash,
/// Fetch or Tool box ([`PromptV2::owner_review`]). A box this lists `Bash`
/// for is one a Bash `sed -i` may be drawn as, which no reason on it can
/// tell from a rule's or a hook's ask ([`settings_ask`], DECIDED
/// 2026-10-15). Rows above the box and a transcript candidate do not bind
/// the current mount to a tool, so every possible tool's hold survives.
#[must_use]
pub fn unvoiced_tools(p: &PromptV2) -> &'static [&'static str] {
    let title = p.title.trim_start();
    match p.kind {
        PromptKind::Write | PromptKind::Overwrite => &["Write"],
        PromptKind::Read => &["Read", "Glob", "Grep"],
        PromptKind::Edit if title.starts_with(anchor("box.edit")) => &["Edit", "Bash"],
        PromptKind::Edit if title.starts_with(anchor("box.edit_notebook")) => &["NotebookEdit"],
        PromptKind::Edit => &["Edit", "Write", "NotebookEdit", "Bash"],
        _ => &[],
    }
}

/// Whether a transcript candidate could change the actual decision. Owner
/// holds always retain [`unvoiced_tools`], so no candidate can lift one.
/// The loop therefore does not read a candidate merely to bypass such a hold.
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

/// Why the owner's own Claude Code settings hold the box `reading` found (on
/// `rows`), at every level, where Claude Code draws no reason to read it by
/// ([`unvoiced_tools`]), from what the agent's settings chain carries
/// ([`ApprovalCtx::owner_asks`]), or those settings unread, where no rule can
/// be ruled out. A Codex box is not Claude Code's.
///
/// * A BARE-TOOL ask rule (`Write`, `Edit`, `Bash`, `Read`…, no specifier)
///   for any tool Claude Code draws the box for: every call of the tool is
///   the person's, so no matcher needs evaluating (DECIDED 2026-10-09).
/// * For a box a Bash `sed -i` is drawn as (`Edit file`, the IDE diff: the
///   box lists `Bash`), ANY Bash ask rule (`Bash(sed:*)`) and any PreToolUse
///   hook whose matcher can match Bash
///   ([`super::claude_settings::hook_may_match`]): Claude Code draws that box
///   with no reason, so neither the rule's specifier nor the hook's answer
///   can be read off it, and in auto mode the box is there only because one
///   of them asked (DECIDED 2026-10-15, fail-closed toward the owner's rules;
///   ATERM-KI-1015-1 F1). The conservative over-hold remains: a transcript
///   may retain an older Edit of the same file while the current Bash call
///   is not written yet, and a queued Edit's childless row can stand over
///   that Bash box while its Waiting row is off-screen. Neither observation
///   establishes current-mount identity, even where the two agree.
///
/// Any other rule with a specifier (`Edit(src/**)`, `Read(~/.ssh/**)`) and
/// a hook on another tool are Claude Code's to match and hold nothing here
/// (`docs/measured/real-claude-lab-2026-10-15.md`, the 2026-10-06 ruling).
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

/// A Read box Claude Code offers no folder grant for was sent by the owner's
/// ask rule or hook. Claude Code draws `Yes, allow reading from <dir> during
/// this session` on every Read box for a folder outside its working set (its
/// directory and any `--add-dir`: MEASURED on 2.1.292 and 2.1.284, in default,
/// accept-edits, plan, auto and bypass mode, for `.env`, `~/.ssh`,
/// `/etc/passwd`, `/dev/null` and a symlink that leads out) and asks about
/// nothing inside it, so a box with none is for a path inside, and only the
/// owner's rule or hook puts one in front of a person. It draws no reason for a
/// file tool, so [`PromptV2::owner_review`] cannot see it: held at every level
/// like the reviews it cannot read. The tell asks for no working directory (the
/// shell's last prompt is not where Claude was launched after `cd x && claude`,
/// in a worktree, or after the harness's own relaunch), reads no path, and does
/// not wait for the box to read whole: Claude Code's renderer garbles the `No`
/// row at some pane widths (`3. Nossion`), and a box with no `No` is no reason
/// to press it.
///
/// Its misses: an owner's Read OUTSIDE the working set carries the grant like
/// Claude Code's own (held where the rule is a bare `Read` the agent's
/// settings carry, [`settings_ask`]; a hook's, or a rule with a specifier, is
/// not seen); a Read on another machine (`runs on`), whose grant the vendor
/// withholds, is left to the proof that it is not local. Its false
/// holds, rare (none of the 808 Reads in the owner's own transcripts): the
/// names Claude Code distrusts as Windows names (`x~1.txt`, `paper.aux`,
/// `file.~1~`, a trailing dot, a `...` component), a folder whose grant text
/// would pass 160 characters (the vendor draws none: a deep `/var/folders/…`
/// path, whose label doubles with its `/private` spelling), and a request a
/// plugin or a remote agent made. Root-level paths (`/foo`) draw boxes of
/// Claude Code's own with no grant too, and are left out.
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
            .any(|o| o.label.contains(anchor("box.read_grant")))
    {
        return None;
    }
    // The reason leads with the hold, so the status line's clip keeps it, and
    // names the false hold the path itself shows.
    let path = p.path.as_deref().unwrap_or("?");
    if read_readings(path).any(vendor_distrusts) {
        return Some(format!(
            "a Read box with no folder grant (Claude Code asks about this name itself): {path}"
        ));
    }
    // The grant's text is the folder, joined with its `/private` spelling where
    // it has one; past 160 characters Claude Code draws none.
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

/// The longest grant text Claude Code draws.
const GRANT_TEXT_MAX: usize = 160;

/// The longest folder whose grant text, doubled with its `/private` spelling
/// (`<dir> and /private<dir>`), stays within [`GRANT_TEXT_MAX`].
const GRANT_FOLDER_SURE: usize = (GRANT_TEXT_MAX - " and /private".len()) / 2;

/// A Read path Claude Code asks about itself, as a name it distrusts on Windows
/// (its `rvt()`, driven on 2.1.284 to 2.1.292): `~` and a digit anywhere
/// (`x~1.txt`), a component ending in `.` or white space, a `...` component, or
/// a path ending in a device name's extension (`paper.aux`).
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

/// What full power's ledger subject puts between a box and the option it
/// chose, for every rule but a one-shot allow ([`RULE_ALLOW_ONCE`]) and the
/// trust dialog's ([`RULE_TRUST_ANY`]), whose subject is the box alone: `<the
/// box's command or title> => <the option's label>` (`… => No, keep
/// default`, `… => Stop and wait for limit to reset`). Settings ▸ Harness's
/// DECIDED FOR YOU reads it back (`crate::harness::stats`) to say which
/// option was pressed, so the two cannot drift.
pub const SUBJECT_OPTION: &str = " => ";

/// What an rm circuit breaker's warned box approved under `approve =
/// "safe"` says in its ledger row's reason (after `unproven:` and the
/// breaker's name): Settings ▸ Harness's DECIDED FOR YOU reads it back
/// (`crate::harness::stats`) to name the switch that pressed it.
pub const WARNED_UNDER_SAFE: &str = "approved with this warning under approve = \"safe\"";

/// Under `approve = "safe"`, the rm breaker the safe rules left (`safe`: their
/// reason), approved with a warning ([`RULE_RM_BREAKER_WARN`]) only in the form
/// the rm rule was written for: a vouched Claude Code Bash box whose note is one
/// `rm` on a possibly-empty variable ([`RmBreakerKind::EmptyVariable`],
/// [`resolvable_breaker`]) with one plain one-shot `Yes` and a `No`. Every
/// other breaker kind, another machine's box, or other options keep the
/// escalation (owner, 2026-09-29).
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
    if resolvable_breaker(&breaker).is_err() || p.runs_on().is_some() {
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

/// Why the box's title is not what the reader found, when it is not: a box
/// taller than the pane, or a box of no kind whose first row is its own
/// question (the walk up stopped inside its body). Such a box is not read
/// whole, and nothing is pressed on it ([`decide`]).
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

/// What a box not read whole waits for, at every level (under `approve =
/// "none"` too, so it promises no answer).
const UNREAD: &str = "nothing is pressed on a box not read whole: it is decided again once the \
                      pane shows it whole";

/// What a declined box taller than the screen is told of itself.
const TALL_WHY: &str =
    "it is taller than the screen, so the harness cannot read all of it to check it";

/// How a declined tall box's flagged removal is to be rerun: its quoted
/// invocation leans on variables the surrounding line binds, and Claude Code's
/// Bash tool keeps no shell state between calls, so run alone it could expand
/// to `rm -rf /`. Paths go in literally, and the rm rule then judges the short
/// box whole.
const TALL_RM_PATHS: &str = "its paths written as literal absolute paths (a command run on its \
                             own keeps none of this one's shell variables)";

/// The answer, at every level that answers at all, to a box whose head is off
/// the screen (`why`, [`head_unread`]): nothing is chosen among options read
/// without their box. In a bypass session, a foot note flagging a removal
/// ([`PromptV2::foot_note`]) on a `Yes`/`No` proceed box
/// ([`yes_no_to_proceed`]), with the rm rule's roots present, is declined
/// ([`decline`], [`RULE_TALL_BOX`]) — the plain `No` with a reason, which
/// settles nothing and whatever the cut title — telling the worker to rerun
/// the quoted part alone with literal paths ([`TALL_RM_PATHS`]). Everything
/// else is handed over with `why`.
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
    let fix = format!(
        "Run {} on its own as a short command, separate from the rest, {TALL_RM_PATHS}",
        quoted_invocation(&note).map_or_else(
            || "the removal".to_string(),
            |inv| format!("`{}`", clip(inv, DECLINE_WHY_CHARS))
        )
    );
    let text = decline_text("this command", TALL_WHY, &fix);
    let subject = format!("a box taller than the screen, its note: {note}");
    decline(RULE_TALL_BOX, prompt, text, subject).unwrap_or_else(|not| handed_over(Some(not)))
}

/// `Ok` when a box's foot is a permission box's: `Do you want to proceed?`
/// (anchor `prompt.proceed`) with exactly a one-shot allow then a refusal
/// (the refusal's input may be open or filled by a decline).
fn yes_no_to_proceed(prompt: &PromptV2, rows: &[String]) -> Result<(), String> {
    let proceed = anchor("prompt.proceed");
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

/// The invocation a removal note quotes between backticks (`… in \`rm -rf
/// $S/$1\` (…)`), when it quotes exactly one.
fn quoted_invocation(note: &str) -> Option<&str> {
    let (_, rest) = note.split_once('`')?;
    let (inv, after) = rest.split_once('`')?;
    (!inv.trim().is_empty() && !after.contains('`')).then_some(inv.trim())
}

/// The safe rules alone ([`Approve::Safe`]): the rule for the box's kind,
/// or an escalation. They judge Claude Code's boxes alone.
fn decide_safe(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if !draws_claude_boxes(reading.program) {
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

/// Read `rows` with the reader for `program` ([`aterm_phase::read`]) and
/// [`decide`] the box on them; `None` when that reader sees no box.
pub fn decide_screen(
    program: Option<&str>,
    rows: &[String],
    ctx: &ApprovalCtx,
) -> Option<Decision> {
    let reading = aterm_phase::read(program, rows, None);
    reading.prompt.as_ref()?;
    Some(decide(&reading, rows, ctx))
}

/// The screen row a box's content row is drawn on: the first row of the
/// box's span whose text, a `│` bar and the indent stripped, is `text`.
fn row_of(rows: &[String], prompt: &PromptV2, text: &str) -> Option<usize> {
    let (top, bottom) = prompt.span;
    (top + 1..bottom.min(rows.len())).find(|&k| {
        let t = rows[k].trim();
        t.strip_prefix('│').map_or(t, str::trim) == text
    })
}

/// The screen row of a file box's `path`: the bare path (2.1.282), a Read
/// box's call (2.1.284 and later), `Read(<path>)`, or Muse Code's file box's
/// `write path: <path>` (1.4.10). ONE row: a call that wraps names no row,
/// and nothing is pressed under it.
fn file_path_row(rows: &[String], prompt: &PromptV2, path: &str) -> Option<usize> {
    row_of(rows, prompt, path)
        .or_else(|| row_of(rows, prompt, &format!("{}{path})", anchor("box.read_call"))))
        .or_else(|| {
            row_of(
                rows,
                prompt,
                &format!("{}{path}", anchor("muse.box.write_path")),
            )
        })
}

/// The one plain one-shot allow of a numbered box, what the safe rules press:
/// exactly one numbered [`Role::Once`] with a [`Role::Deny`] beside it, never
/// while an amend input is open ([`amend_input_open`]).
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

/// Why no digit is pressed while [`amend_input_open`].
const OPEN_INPUT: &str =
    "the focus is on an open `No, and …` input: a digit would be typed into it";

/// Whether the focus is on the refusal's open amend input: a `No, …` label (the
/// placeholder or typed text) over a footer reading `Esc to cancel` alone.
/// Claude Code sends every key but the arrows to that input, so a digit would
/// be typed into it. A fixed refusal label (the footerless Fetch and network
/// boxes' `No, and tell Claude … (esc)`, Chrome's `Deny (esc)`) is no input.
fn amend_input_open(prompt: &PromptV2) -> bool {
    let amended = format!("{REFUSAL_LABEL}, ");
    // The keys of the box's own footer, less the vendor's note about
    // background agents (2.1.291), which an agent's box keeps drawn while the
    // input hides `Tab to amend`.
    let footer_alone = prompt
        .footer
        .as_deref()
        .is_some_and(|f| aterm_phase::footer_keys(f) == [anchor("prompt.cancel")]);
    footer_alone
        && prompt
            .options
            .iter()
            .any(|o| o.focused && o.role == Role::Deny && o.label.starts_with(&amended))
}

/// How many options the focus moves from option `from` to option `to`.
fn focus_steps(from: usize, to: usize) -> i32 {
    i32::try_from(to).unwrap_or(0) - i32::try_from(from).unwrap_or(0)
}

/// `❯ 1. Yes` / `2. No`: a number, a dot, a space.
fn is_option_row(t: &str) -> bool {
    let t = t.strip_prefix('❯').map_or(t, str::trim_start);
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    (1..=2).contains(&digits) && t[digits..].starts_with(". ")
}

/// No option-shaped row above the box's question: options are the rows under
/// `Do you want …`, and a `1. Yes` among command rows is command text or a
/// forgery (a second line of defence behind aterm-phase).
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

/// The box's content width in cells, from its top rule of `─` above the title;
/// `None` when it has none.
fn box_width(prompt: &PromptV2, rows: &[String]) -> Option<usize> {
    let rule = rows.get(prompt.span.0.checked_sub(1)?)?;
    let t = rule.trim();
    (!t.is_empty() && t.chars().all(|c| c == '─')).then(|| rule.trim_end().chars().count())
}

/// Whether a row's width in cells is its character count: printable ASCII only.
/// The server's row text omits wide characters' continuation cells, and emoji
/// width depends on the font, so any other row is never measured (no
/// narrowing, every reading).
fn cells_are_chars(row: &str) -> bool {
    row.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// How far inside the box's width a row plus the next row's first word must end
/// before the row is taken as not wrapped: a quarter of the width, well inside
/// any right edge the box can have.
fn wrap_margin(width: usize) -> usize {
    (width / 4).max(8)
}

/// The readings of a Bash box's command to judge ([`PromptV2::readings`]),
/// narrowed by geometry only for the rm breaker (`rm_breaker`) and only where
/// the screen proves it. Without bars the command is one shell line, so a later
/// row is its tail only if it is a wrap; a row ending with room to spare for the
/// next word ([`wrap_margin`]) ends the command, and rows under it are judged
/// as description. Barred boxes, boxes without a top rule, and non-ASCII rows
/// ([`cells_are_chars`]) are judged under every reading.
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
    if prompt.base_title().as_deref() != Some(anchor("box.bash")) {
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
        // After the line's own proof: a refused line is refused on where it
        // points, and a proven one still needs the assumed shell startup.
        if let Err(why) = shell_models_hold(&readings, None, ctx) {
            return Decision::escalate(format!("rm circuit breaker: {why}"));
        }
        if let Err(why) = git_reads_clear(&readings, None, ctx) {
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
    if let Some(note) = prompt.notes.first() {
        return Decision::escalate(format!("the box carries a vendor note: {note}"));
    }
    if ctx.bypass_mode {
        return Decision::escalate("a box in a bypass session is a vendor circuit breaker");
    }
    if let Err(why) = read_only_every_reading(&readings, &ctx.python_allow) {
        return Decision::escalate(why);
    }
    if let Err(why) = shell_models_hold(&readings, prompt.runs_on().as_deref(), ctx) {
        return Decision::escalate(why);
    }
    if let Err(why) = plain_under_zsh(&readings, ctx) {
        return Decision::escalate(why);
    }
    if let Err(why) = git_reads_clear(&readings, prompt.runs_on().as_deref(), ctx) {
        return Decision::escalate(why);
    }
    match once_choice(prompt) {
        Ok(choice) => Decision::Approve {
            rule_id: RULE_READ_ONLY,
            choice,
            guard,
            subject,
            unproven: None,
        },
        Err(why) => Decision::escalate(why),
    }
}

/// `Ok` when no git read on any reading of a read-only line would load
/// configuration that runs a program ([`super::git_config`]); `Err` names the
/// key, hook, or why the check failed. A box on another machine cannot be
/// checked.
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
        // Both looks at the machine go through the context's probe: the
        // settings' `env` blocks, and the configuration each directory's git
        // loads — projected to what `hazard` reads, so the view judged here is
        // the very one a harness tape holds (`GitView::project`).
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

/// How a reason names a reading: by the join it has.
fn reading_name(line: &str) -> &'static str {
    if line.contains('\n') {
        "newline reading"
    } else {
        "space reading"
    }
}

/// `Ok` unless the Bash tool may run the line under zsh
/// ([`ShellStartup::may_run_zsh`]) and a reading of it is not plain
/// ([`plain_zsh_line`]): under zsh the read-only rule proves only a line
/// written where the shell expands nothing. A startup not read is
/// [`shell_models_hold`]'s refusal, made before this.
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

/// `Ok` when every reading of a Bash box's command ([`command_readings`])
/// classifies read-only; `Err` names the reading and the reason.
pub(crate) fn read_only_every_reading(
    readings: &[String],
    python_allow: &[String],
) -> Result<(), String> {
    if readings.is_empty() {
        return Err("a Bash box with no command".to_string());
    }
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

/// `Ok` when [`RULE_RM_BREAKER`]'s resolver may judge this breaker: an `rm` on a
/// possibly-empty variable ([`RmBreakerKind::EmptyVariable`]) whose note is
/// read whole (one note, one `Dangerous`, its advice's parentheses closed).
/// `Err` names the kind and why not.
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
/// (`the rm circuit breaker (statically-unresolvable target)`). The words are
/// aterm-phase's anchors.
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
        RmBreakerKind::TooManySubstitutions => "box.rm_breaker.too_many",
    };
    let why = anchor(id)
        .trim()
        .trim_start_matches("on ")
        .trim_end_matches([':', '('])
        .trim();
    let command = match b.command {
        RmCommand::Rm => "rm",
        RmCommand::Rmdir => "rmdir",
    };
    format!("the {command} circuit breaker ({why})")
}

/// A breaker box's escalation `reason` with its kind named once: unchanged if
/// it already carries [`rm_breaker_label`]; the label in place of a leading
/// "the/an rm circuit breaker" (rmdir alike); else the label and a colon in
/// front.
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

/// What a decline tells the worker ([`Decision::Decline`]): [`DECLINE_PREFIX`],
/// that `what` was not run and why (cut at [`DECLINE_WHY_CHARS`]), and `fix`.
/// One printable line: control characters, tabs (would close the input) and
/// newlines (would submit) become spaces.
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

/// The decline's one constructor: `text` typed into the box's refusal
/// ([`refusal_of`]) under `rule_id`, naming `subject`, or `Err` when the box
/// offers no refusal to amend (the caller escalates). A box with a decline
/// already under way is the same decline, so a stopped loop resumes it.
///
/// Claude Code's bare `No` aborts the worker's turn and asks the person what
/// to do next. Nonempty amended feedback lets the worker continue in the same
/// turn; a digit on `No`, or Enter on its empty input, is not that feedback.
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

/// `text` with all whitespace dropped, so wrapped rows read back as what was
/// written (an option label here, a composer draft in `turn_end_loop.rs`).
pub(crate) fn squashed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The refusal as drawn with its input open and empty: `No, and tell Claude
/// what to do differently`.
fn amend_open_label() -> String {
    format!("{REFUSAL_LABEL}, {}", anchor("prompt.amend_no"))
}

/// The refusal with `text` typed into its input: `No, <text>`.
fn amended_label(text: &str) -> String {
    format!("{REFUSAL_LABEL}, {text}")
}

/// Whether the box's footer offers `Tab to amend` (Claude Code draws it
/// while the focused option is an allow, or a refusal whose input is shut).
fn offers_amend(prompt: &PromptV2) -> bool {
    let hint = anchor("prompt.amend");
    prompt
        .footer
        .as_deref()
        .is_some_and(|f| f.split(" · ").any(|h| h == hint))
}

/// The option a decline amends: the one [`Role::Deny`] plain `No`
/// ([`REFUSAL_LABEL`]) on a footer offering `Tab to amend`, or during a decline
/// the focused refusal whose input is open ([`amend_open_label`]) or holds
/// `text` ([`amended_label`]).
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
            anchor("prompt.amend")
        ));
    }
    Ok(k)
}

/// The next keystroke of a decline ([`decline_step`]) with the guard of the row
/// showing the state it answers ([`super::guard::row_guard`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclineStep {
    /// `down` (or `up`) toward the refusal, guarded on the focused row.
    Move { down: bool, guard: String },
    /// `tab` on the focused `No`: its input opens.
    Amend { guard: String },
    /// The text, typed into the open, empty input.
    Type { guard: String },
    /// `enter` on the input that shows the text: the refusal, with it.
    Submit { guard: String },
}

/// A decline's next keystroke on a fresh read: focus elsewhere, move onto the
/// refusal; on the shut `No` (footer offers `Tab to amend`), Tab; on the open
/// empty input, the text; on the input showing exactly the text, Enter. `Err`
/// for anything else, where no keystroke is sound.
///
/// Tab toggles the focused option's input; it is not an idempotent open.
/// The loop must keep one key in flight and wait for the drawn state before
/// the next step, or a second Tab can close the input underneath the text.
/// The recorded `cap-decline-*.txt` states and their guarded transition test
/// in `approval_tests.rs` retain the evidence for this sequence.
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
                anchor("prompt.amend")
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
        // A Bash box's description row (`Read file /etc/hosts`) is not a
        // Read header, and neither is a ` · ` suffix the grammar cannot read.
        return Decision::escalate(format!(
            "the box header is not a Read header: {}",
            prompt.title
        ));
    }
    // The proof is local (symlinks on this machine, secrets under this
    // `$HOME`), so a Read running on another machine is not proven.
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
    // The box draws the path as text. A character it draws as another (a
    // newline, a tab, an escape: U+FFFD) or that this screen drops (a
    // zero-width joiner) makes the name judged here a name that is not there,
    // and a link under the real one is never followed: only plain ASCII is
    // proven, a person reads the rest (the ` · ` of a view is the one exception).
    if !path
        .split(" · ")
        .all(|part| part.bytes().all(|b| (0x20..0x7f).contains(&b)))
    {
        return Decision::escalate(format!("a Read of a path outside plain ASCII: {path}"));
    }
    // The view the model asked for rides inside the call (`Read(<path> · lines
    // 1-5)`), and the screen cannot tell a path with a view from a name that
    // ends like one (a link named `k · lines 1-5` is a file Claude Code reads):
    // every path the text can mean is judged, and the press waits on all.
    // Every reading's text first, then the disk: a reading that names a secret
    // says so whatever a sibling reading's folders let this user look at.
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

/// Every path a Read call's text can mean: the whole of it, and each prefix cut
/// at a ` · ` (Claude Code draws the view the model asked for after one).
fn read_readings(call: &str) -> impl Iterator<Item = &str> {
    std::iter::once(call).chain(call.match_indices(" · ").map(|(at, _)| &call[..at]))
}

/// A Read box's base title ([`PromptV2::base_title`]): `Read file`, `Read
/// files` or `Read file(s)`.
fn is_read_title(base: &str) -> bool {
    base.strip_prefix(anchor("box.read"))
        .is_some_and(|rest| matches!(rest, "" | "s" | "(s)"))
}

/// `path` resolved to its absolute components: `~/…` against `home`, no
/// glob, no `..`.
fn resolve_abs(path: &str, home: Option<&Path>) -> Result<Vec<String>, String> {
    if path.contains(['*', '?', '[', '{']) {
        return Err(format!("a pattern: {path}"));
    }
    let full = if let Some(rest) = path.strip_prefix("~/") {
        let home = home.ok_or_else(|| format!("{path} under ~ with no home to resolve it"))?;
        let home = home.to_string_lossy();
        // A Windows home (`C:\Users\…`) is no `/` path these rules read.
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

/// A Read box's path, as text: absolute or `~/…`, one plain path (no glob, no
/// `..`), under an [`ApprovalCtx::read_roots`] root and outside every
/// [`SecretRule`]. Its components, for [`read_path_on_disk`].
fn read_path_lexical(path: &str, ctx: &ApprovalCtx) -> Result<Vec<String>, String> {
    // The components below drop an empty one (`a//b`, `a/./b`, a trailing `/`),
    // so the path judged would not be the one drawn. Claude Code collapses those
    // in a real path before it draws the box: one the box shows stands where the
    // screen drew a folder or file name as NOTHING (a name of zero-width
    // characters only), and the press waits for a person.
    if path.contains("//") || path.contains("/./") || path.ends_with('/') {
        return Err(format!(
            "a Read of a path with an empty component, a name the box may have drawn as nothing: {path}"
        ));
    }
    let comps = resolve_abs(path, ctx.home.as_deref()).map_err(|e| format!("a Read of {e}"))?;
    read_comps_allowed(&comps, path, ctx)?;
    Ok(comps)
}

/// A Read box's path ([`read_path_lexical`]'s `comps`), on this machine: where
/// its symlinks lead passes the same checks, and no name beside one on its way
/// differs from it only by characters the box may not draw. Both looks at the
/// disk go through the context's probe ([`ApprovalCtx::probe`]:
/// [`PolicyProbe::real_components`], [`PolicyProbe::hidden_twin`]).
fn read_path_on_disk(path: &str, comps: &[String], ctx: &ApprovalCtx) -> Result<(), String> {
    // Apply the same root/secret checks to the symlink target observed now.
    // The worker can change it before its later file open.
    let real = ctx
        .probe
        .real_components(comps)
        .ok_or_else(|| format!("a Read whose symlinks cannot be resolved: {path}"))?;
    if real != comps {
        read_comps_allowed(&real, &format!("{path} (-> /{})", real.join("/")), ctx)?;
    }
    // The scan lists folders, so it runs for a path every cheaper test passed:
    // one that is held lists nothing.
    if let Some(twin) = ctx.probe.hidden_twin(comps)? {
        return Err(format!(
            "a Read of {path}: {twin:?} differs from it only by characters the box may not \
             draw (and by case, which the default volume ignores), so the box may not show \
             which one is read"
        ));
    }
    Ok(())
}

/// The root and secret checks of [`read_path_lexical`] on one spelling of
/// the path (`shown` in the reason).
fn read_comps_allowed(comps: &[String], shown: &str, ctx: &ApprovalCtx) -> Result<(), String> {
    let path = shown;
    if !ctx.read_roots.iter().any(|r| r.holds(comps)) {
        return Err(format!("a Read outside every allowed root: {path}"));
    }
    for rule in &ctx.secrets {
        let hit = match rule {
            SecretRule::Under(dir) => abs_components(&dir.to_string_lossy()).is_some_and(|d| {
                comps.len() >= d.len()
                    && d.iter().zip(comps).all(|(a, b)| a.eq_ignore_ascii_case(b))
            }),
            SecretRule::Component(name) => comps.iter().any(|c| c.eq_ignore_ascii_case(name)),
            SecretRule::Basename(glob) => comps
                .last()
                .is_some_and(|c| glob_match(&glob.to_ascii_lowercase(), &c.to_ascii_lowercase())),
        };
        if hit {
            return Err(format!("a Read of a secret ({rule:?}): {path}"));
        }
    }
    Ok(())
}

/// The folder-trust dialog ([`RULE_TRUST_DIALOG`]): its folder is the session's
/// launch directory under a trust root (or the one folder the session's
/// starter trusted for it, [`ApprovalCtx::trusted_cwd`]), its options exactly
/// an exit and a trust (Claude Code's `No, exit` and `Yes, I trust this
/// folder`), focus on one of them. The choice moves the focus to the trust
/// option (or presses its digit if numbered).
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
    // The dialog's path block loses a space its wrap fell on (aterm-phase),
    // so the folder is compared with the session's own directory, exactly.
    let cwd = abs_components(&ctx.cwd.to_string_lossy());
    if cwd.as_deref() != Some(comps.as_slice()) {
        return Decision::escalate(format!(
            "a folder-trust dialog for {path}, not the session's cwd {}",
            ctx.cwd.display()
        ));
    }
    // The session's own starter's folder, exactly (`ApprovalCtx::trusted_cwd`).
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

/// The most rows of one backstop warning's list [`backstop_warnings`]
/// names; the rest are counted.
const BACKSTOP_ROWS: usize = 10;

/// The trust backstop's `⚠` warnings in `body`, each with its listed rows (tool
/// permissions, directories, headersHelper), which the dialog's Yes makes apply
/// without asking in later sessions. The ledger subject names what was
/// accepted.
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

/// The words a purchase opens with ([`buys`]), lowercased, after a leading
/// `yes, `: buying, adding funds, upgrading, spend limits, re-enabling usage
/// credits (drawn only when the account holder turned them off), and switching
/// onto usage credits (`Switch to usage credits`, which would otherwise read as
/// a model switch, [`switches_model`]). A consent to continue with credits
/// already on buys nothing ([`credits_consent`] is its own key's). Claude Code
/// 2.1.304 adds two (READ in its bundle; neither captured on a screen): the
/// spend-limit nudge's `Adjust monthly spend limit: $N`, whose Enter on a
/// changed value raises the account's spend limit for good and carries the
/// session on on credits, and the promotional buy screen's `Pay $X · Get $Y`.
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

/// The words spending a limit reset opens with ([`spends`]), lowercased,
/// after a leading `yes, `: wider than the vendor's own three labels, so a
/// row whose tail or whose `now` the server rewords still reads as a spend.
const RESET_SPEND: &[&str] = &["use my reset", "use your reset", "reset your session limit"];

/// Whether `label`, past a leading `yes, `, opens with one of `words` as a
/// whole word, case ignored — so a label naming a path (`billing/upgrade.rs`)
/// opens with none.
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

/// WHETHER CHOOSING AN OPTION LABELLED `label` SPENDS A LIMIT RESET — the ONE
/// rule every pick and [`decide`]'s last check ask ([`buys`],
/// [`never_spends`]): Claude Code 2.1.304's grant rows of every kind (`Use a
/// weekly reset (…)`, the Fable one too) and its `Reset your session limit
/// now` row on the usage-limit dialog, and each kind's `Yes, use my <kind>`
/// on the confirmation a grant row opens, by the reader's own anchors
/// ([`aterm_phase::spends_reset`]) and by their wide words
/// ([`aterm_phase::uses_a_reset`]: `use` and a determiner, then a head that
/// names a reset, so a kind the server adds or renames, however many words
/// its name takes, still reads as a spend); and the
/// 2.1.286–2.1.303 words the reader no longer reads (`Use your reset now
/// (…)`, `Yes, use my reset`) by [`RESET_SPEND`], so an older build still
/// running spends nothing either. READ in the 2.1.286, 2.1.287 and 2.1.304
/// bundles; no such box has been captured on a screen.
///
/// A reset is a counted allowance with a deadline, and the weekly one claims
/// with no confirmation at all. A confirmation whose title row is off the
/// screen would fall to [`allow_once`], whose first plain yes is `Yes, use my
/// weekly reset`; so such a box is pressed by no rule (it is not read whole,
/// [`head_unread`]), a box that offers one gets the option that waits (`No,
/// keep it`, [`RULE_NO_SPEND`]), and the grant row is chosen by
/// [`RULE_LIMIT_RESET`] alone.
fn spends(label: &str) -> bool {
    aterm_phase::spends_reset(label)
        || opens_with(label, RESET_SPEND)
        || aterm_phase::uses_a_reset(label)
}

/// Whether choosing `opt` would buy something: its label spends a limit
/// reset ([`spends`]), or, past a leading `yes, `, opens with one of
/// [`PURCHASE`]'s words as a whole word.
#[must_use]
pub fn buys(opt: &Opt) -> bool {
    spends(&opt.label) || opens_with(&opt.label, PURCHASE)
}

/// Whether `opt` waits rather than goes on: `Stop and wait for limit to reset`,
/// `No, keep my current model`, `Stay on …`.
fn waits(opt: &Opt) -> bool {
    let l = opt.label.to_lowercase();
    ["stop and wait", "wait", "no, keep", "stay on", "stay "]
        .iter()
        .any(|w| l.starts_with(w))
}

/// Full power's answer ([`Approve::All`]) to a box the safe rules left (`safe`:
/// their reason, kept in an escalation and in `unproven`).
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
        // [`decide`] answers a question by its own rule first.
        PromptKind::Question => Err("a question is answered by its own rule".to_string()),
        PromptKind::PlanEnter | PromptKind::PlanExit => plan_pick(p),
        PromptKind::Trust => p
            .with_role(Role::Trust)
            .map(|o| (RULE_TRUST_ANY, o))
            .ok_or_else(|| "a folder-trust dialog with no trust option".to_string()),
        // A held cross-session message: its one delivery, the one option
        // that does not refuse (`Deliver this message to Claude`).
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
        // The read-outside dialog: its one-shot allow (2.1.287's `Yes, but
        // ask again next time`), else the last `No` (`No, ask again next
        // time`); each settles nothing.
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

/// Why full power leaves a box of `program` to a person where its picks for
/// the kind would answer it: a program whose boxes were measured kind by kind.
/// Gemini CLI's (0.46.0) are answered only as measured — its shell, write and
/// edit boxes by their one [`Role::Once`] (`Allow once`), its folder-trust
/// dialog by its [`Role::Trust`] (`Trust folder`) — so a box of another kind
/// (an MCP tool's, a web fetch's: `Allow execution of [<tool>]?`), or one
/// with no single one-shot allow, is never declined or allowed by the general
/// picks. Muse Code's tool-action box (1.4.10, measured outside the lab's
/// jail: `tool action: mcp__<server>__<tool>`, a [`PromptKind::Tool`] whose
/// `Yes, proceed` the general picks would press) is an MCP tool's call, and
/// goes to a person as Gemini CLI's MCP box does: the tool reaches past the
/// shell, file and network boxes whose answers were measured. So does its
/// network box of a command, a box of no kind (no `full URL:` row: a
/// sandboxed `pip install`'s host, measured on 1.4.10, and the Unix-socket
/// box, `Allow once` · `Reject`, measured outside the lab's jail): its reason
/// says what the box is within the 120 cells a person's attention keeps of
/// it, the command after (the attention's label names it too), where the
/// general path's would say the box offers no refusal although its reader
/// reads its Esc as one. `None` for every other program, and for those boxes.
fn unmeasured_box(program: Program, p: &PromptV2) -> Option<String> {
    match program {
        Program::Gemini => {
            let once = p.options.iter().filter(|o| o.role == Role::Once).count() == 1;
            match p.kind {
                PromptKind::Bash | PromptKind::Write | PromptKind::Edit if once => None,
                PromptKind::Trust => None,
                kind => Some(format!(
                    "{} box of Gemini CLI's with no measured answer: its one-shot `Allow \
                     once` on a shell, write or edit box, or its folder trust",
                    a_or_an(kind.name())
                )),
            }
        }
        Program::Muse => match p.kind {
            PromptKind::Tool => Some(format!(
                "Muse Code's tool-action box for `{}`: an MCP tool's call reaches past the \
                 measured shell, file and network boxes",
                p.command
            )),
            PromptKind::Other if p.title == anchor("muse.box.network") => Some(format!(
                "Muse Code's network box of a command: what it reaches past its sandbox (a \
                 host, a Unix socket) is a person's to allow{}",
                if p.command.is_empty() {
                    String::new()
                } else {
                    format!(" (`{}`)", p.command)
                }
            )),
            _ => None,
        },
        Program::Claude | Program::Codex | Program::Agy | Program::Generic => None,
    }
}

/// Full power's answer to the model-switch confirmation
/// ([`PromptKind::ModelSwitch`]): Claude Code asks before a person's `/model` or
/// `/effort` change whether to pay an uncached re-read, so `Yes, switch to <m>`
/// carries out what they asked ([`RULE_MODEL_CONFIRM`]). Pressed only when the
/// subtitle (rows under the title to the first blank, joined) is exactly the
/// cost warning (anchor `model_switch.cost`); a hook's confirmation (anchor
/// `model_switch.hook`) or a reworded subtitle is handed over.
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
    let hook = anchor("model_switch.hook");
    if subtitle.contains(hook) {
        return Err(format!(
            "the person's own PreModelSwitch hook asked for this confirmation (`{hook}`)"
        ));
    }
    if subtitle != anchor("model_switch.cost") {
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
            anchor("model_switch.yes"),
            p.options
                .iter()
                .map(|o| format!("{}:{}", o.role.name(), o.label))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The press full power makes of `pick` on `p`: its choice, the judged row's
/// guard, the subject and `unproven` (`safe`: why the safe rules did not prove
/// it).
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
    // An unnumbered box the vendor opened on its refusal (`No` listed first):
    // pressed anyway and said so, read from option order since the press
    // moves the focus.
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
    // A trust backstop's Yes makes the listed grants apply in later sessions,
    // so the subject names every listed row.
    let what = match backstop_of(p, rows) {
        Some(warnings) => {
            format!("{what} (backstop: {warnings}; trusting makes these apply without asking)")
        }
        None => what.to_string(),
    };
    // A one-shot allow names what it allowed (hashed as the command); other
    // answers name the option chosen.
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

/// A trust dialog's backstop warnings, joined (`None` for other boxes): the
/// option `No, continue without these permissions` beside the trust option.
fn backstop_of(p: &PromptV2, rows: &[String]) -> Option<String> {
    let backstop = p.kind == PromptKind::Trust
        && p.options
            .iter()
            .any(|o| o.role == Role::Deny && o.label == anchor("trust.no_backstop"));
    backstop.then(|| {
        let body = &rows[(p.span.0 + 1).min(rows.len())..p.span.1.min(rows.len())];
        backstop_warnings(body).join("; ")
    })
}

/// The words a limit's consent to go on opens with, lowercased (`Continue with
/// …`, drawn while credits are on). Re-enabling credits is a purchase.
const CONSENT: &[&str] = &["continue with"];

/// Whether `opt` is a consent to go on ([`CONSENT`]): its label opens so, as
/// a whole word ([`opens_with`]), and its answer is not kept for good
/// ([`kept_for_good`], the press gate's test, its rows' term defence in
/// depth). `Continue without …` is no
/// consent but a refusal that goes on (aterm-phase's role table reads it
/// [`Role::Deny`]): read by the bare prefix it was one, and full power
/// pressed 2.1.301's `Continue without using this MCP server` under
/// [`RULE_ALLOW_ONCE`], which disabled the server for good (measured
/// 2026-10-16). Nor is the Chrome setup's `Continue with browser tools`
/// (2.1.301 bundle, est()): it turns the browser tools on for every later
/// session, so its panel is declined with `Continue without browser tools`
/// (in the layout 2.1.302 draws, each option's description beside its
/// label).
/// Nor is a label Claude Code cut short ([`cut_short`]: `Continue with
/// browser…`, the Chrome setup's in a narrow pane), whose cut end may keep
/// it.
fn consents(opt: &Opt) -> bool {
    opens_with(&opt.label, CONSENT) && !kept_for_good(opt) && !cut_short(&opt.label)
}

/// The words that name usage credits ([`credits_consent`]), lowercased:
/// Claude Code's `usage credits`, and `extra usage`, its older name for them.
const CREDITS: &[&str] = &["usage credit", "extra usage"];

/// Whether the consent `opt` on `p` ([`consents`]) carries the session onto
/// USAGE CREDITS: its label, the box's title, or the box's own rows
/// (`p.span`, title to footer, wrapped rows joined) name them ([`CREDITS`]),
/// case ignored. Claude Code 2.1.304's credit consents always do (READ in its
/// bundle; no such box captured on a screen): the model consent dialog's
/// title `<M> now uses usage credits`, or under `You've reached your Fable
/// limit` its body `You've used your included Fable usage for this week.
/// Continuing on <M> uses usage credits…` (`<M> runs on usage credits…`
/// otherwise) over its `Continue with <M>`; the Usage credits dialog's
/// `Continue with usage credits`. A consent that names none (a refill's
/// `Continue with a refill`, the new-model offer's `Continue with <newer>`) is
/// none: `[harness] usage_credits` leaves it as it was.
fn credits_consent(p: &PromptV2, rows: &[String], opt: &Opt) -> bool {
    let names = |text: &str| {
        let text = text.to_lowercase();
        CREDITS.iter().any(|w| text.contains(w))
    };
    let end = p.span.1.saturating_add(1).min(rows.len());
    // A box's bar (`│`) and rule glyphs are no words: dropped, so a wrap
    // between `usage` and `credits` inside a framed box still reads whole.
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

/// THE CREDITS CONSENT CANCELLED (`[harness] usage_credits` off, the
/// default; the owner, 2026-10-19): a consent onto usage credits on Claude
/// Code's screen ([`consents`], [`credits_consent`]) is answered with Esc
/// ([`RULE_CREDITS_DECLINE`]) where the box's own footer reads `Esc to
/// cancel` (anchor `prompt.cancel`) and its title row is on the screen. Esc
/// is the dialog's cancel and saves nothing (READ in the 2.1.304 bundle: the
/// model consent dialog's ends `cancelled`, its consent unstored; the Usage
/// credits dialog's closes it); none of its rows is pressed, since `Switch to
/// <model> and continue` and `Not now` may save the person's default model.
/// The key goes on a fresh read with the focus where it was judged
/// ([`Choice::FocusKey`], 0 steps), so it chooses nothing; the press gate
/// reads no option of it. Where no Esc is read, or no option is focused, the
/// box is a person's ([`credits_held`]). `None`: no such consent, the key on,
/// or another program's screen.
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
            .any(|key| key.eq_ignore_ascii_case(anchor("prompt.cancel")))
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

/// Why a consent onto usage credits is not taken with the key off, by its
/// kind: a model's (`Continue with <M>`), which Claude Code keeps for good
/// per model and account, or the Usage credits dialog's (`Continue with
/// usage credits`), which saves nothing and carries the session onto the
/// credits already on at the account.
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

/// Why no rule answers Claude Code's MCP server dialog (aterm-phase's
/// `McpServer`), at any level: every answer it offers, Esc's too, is saved
/// for good in the project's `.claude/settings.local.json` — a standing
/// grant or a standing refusal — and no rule gives one but the folder-trust
/// dialog's and the owner's autocompact choice ([`trust_dialog`],
/// [`autocompact_offer`]). Claude Code itself leaves it to a person in
/// its own background sessions. What a person must know comes first, in the
/// 120 cells the owner's badge keeps of a reason (`escalate::fit_reason`).
fn mcp_server_held(p: &PromptV2) -> String {
    format!(
        "every answer, Esc too, is saved for good in .claude/settings.local.json (`claude mcp \
         reset-project-choices` undoes it): Claude Code's MCP server dialog (`{}`) is no rule's \
         to answer, at any level",
        p.title
    )
}

/// A dialog of no kind aterm-phase names ([`PromptKind::Other`]), `rows` the
/// screen it was read from. On Claude Code's screen a consent is accepted
/// ([`consents`]: never an answer kept for good; on no other program's
/// screen, [`draws_claude_boxes`]) — one onto usage credits
/// ([`credits_consent`]) only under `[harness] usage_credits`
/// ([`ApprovalCtx::usage_credits`], [`RULE_CREDITS_CONSENT`]), and with
/// that off its box is cancelled before this ([`credits_decline`]) or a
/// person's, none of its rows pressed (module header, the consent bullet); a
/// box whose every yes buys gets the option that
/// waits; the model-refusal pause gets its switch while `model_fallback` is set
/// ([`refusal_pause_switch`], [`RULE_MODEL_SWITCH`]); any other setup or config
/// dialog is declined with its refusal ([`RULE_DECLINE`]), since its yes
/// settles something for good. No other `Switch to …` is chosen here. Else a
/// person's.
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

/// Whether choosing `opt` switches the model: a `Switch to <model>` label with
/// no purchase in it.
fn switches_model(opt: &Opt) -> bool {
    opt.label.trim_start().starts_with("Switch to ") && !buys(opt)
}

/// Claude Code's model-refusal pause and nothing else: its switch when the box
/// is program Claude, head read, titled `Session paused` (anchor
/// `paused.title`), options exactly `Switch to <model>` and `Edit prompt and
/// retry…` (anchor `paused.edit`).
fn refusal_pause_switch(program: Program, p: &PromptV2) -> Option<&Opt> {
    if !draws_claude_boxes(program) || p.head_off_screen || p.title.trim() != anchor("paused.title")
    {
        return None;
    }
    match p.options.as_slice() {
        [switch, edit]
            if switches_model(switch)
                && edit.label.trim_start().starts_with(anchor("paused.edit")) =>
        {
            Some(switch)
        }
        _ => None,
    }
}

/// The refusal pause's switch `o`, pressed while `model_fallback` is set
/// ([`RULE_MODEL_SWITCH`]).
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

/// Words of a plan yes that grants a standing mode or wipes the conversation,
/// lowercased.
const BROAD_PLAN: &[&str] = &[
    "auto mode",
    "accept edits",
    "auto-accept",
    "bypass permissions",
    "clear context",
];

/// Plan mode's box: the yes that grants no standing mode, a [`Role::Once`]
/// (`Yes, implement this plan`) or else a yes naming none of [`BROAD_PLAN`]
/// (`Yes, manually approve edits`, `Yes, enter plan mode`). Claude Code lists
/// its broadest yes first, so the first yes is never assumed. Without one the
/// plan is declined (`No, keep planning`) and the turn-end policy continues.
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

/// A permission box's one-shot allow: its one [`Role::Once`]; else, on a sound
/// parse that reads a role a press is chosen by, its first yes granting
/// nothing standing ([`Role::standing`]); else, on a box that would buy, the
/// option that waits; else its `No` ([`RULE_DECLINE`]).
fn allow_once(p: &PromptV2) -> Result<(&'static str, &Opt), String> {
    let once: Vec<&Opt> = p.options.iter().filter(|o| o.role == Role::Once).collect();
    if let [o] = once.as_slice()
        && !buys(o)
    {
        return Ok((RULE_ALLOW_ONCE, o));
    }
    // Roles only on a sound parse (aterm-phase), and only the standing ones
    // where an unnumbered option under another opens with no capital: there
    // the labels are unproven, so no yes is taken by its label.
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
    // Waiting instead of buying is harmless whatever the parse.
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

/// How `o` is chosen on `p`: its digit — never while the focus is on an
/// open amend input ([`amend_input_open`]) — or the focus moved to it.
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

/// `│ text │` → `text`, trimmed.
fn unbar(row: &str) -> Option<&str> {
    row.strip_prefix('│')
        .and_then(|t| t.strip_suffix('│'))
        .map(str::trim)
}

/// The row a full-power press is guarded on: a Bash or PowerShell box's first
/// command row (bare, behind a prompt glyph like Codex's `$ `, or in a box
/// drawn inside the box, as Gemini CLI's), a file box's path (the question
/// for `Edit notebook`, the IDE diff and a box that names its file only in
/// its title), the trust dialog's folder, else the box's first row.
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
                // A command in a box drawn inside the box (Gemini CLI's
                // `│ │ <command> │ │`).
                .or_else(|| {
                    (top + 1..bottom.min(rows.len()))
                        .find(|&k| unbar(rows[k].trim()).and_then(unbar) == Some(c.as_str()))
                })
        }),
        PromptKind::Trust => (p.span.0 + 1..p.span.1.min(rows.len())).find(|&k| {
            let t = rows[k].trim();
            t.starts_with('/') || t.starts_with('~')
        }),
        // Muse Code's network box: its `full URL: <url>` row, where the URL
        // fits on it (1.4.10); else its title, as a wrapped path's box is.
        PromptKind::NetworkRequest => row_of(
            rows,
            p,
            &format!("{}{}", anchor("muse.box.full_url"), p.command),
        ),
        // A file box's path row — or, for `Edit notebook` and the IDE diff,
        // whose file only the question names, the question row.
        _ => p
            .path
            .as_deref()
            .and_then(|path| file_path_row(rows, p, path))
            .or(p.question.filter(|&q| q < rows.len() && p.path.is_some())),
    };
    own.unwrap_or(p.span.0)
}

/// The usage-limit options dialog's GRANT ROW ([`RULE_LIMIT_RESET`]), decided
/// in front of the wait row: `Some` only where `[harness] limit_reset` is on
/// ([`ApprovalCtx::limit_reset`]), the dialog's row taken is the ONE option
/// that spends a reset, of a kind the step takes (5-hour, weekly, full; never
/// a Fable row) — or, under the owner's `limit_reset_kinds`
/// ([`ApprovalCtx::reset_kinds`]), the row of the first listed kind on offer
/// — read by its whole template ([`aterm_phase::reset_row`]: never by its
/// place; several on offer, with no list, are the owner's to choose), its title
/// is the dialog's and on the screen, and an option has the focus. The row
/// is then reached and confirmed as the wait row is — arrows, and Enter once
/// a fresh read shows the focus on that very label and that label still the
/// row the rule takes (`press_focused`), under the title row's guard, never
/// a digit. `None` — the wait row's rule, exactly as with the key off — in
/// every other case. What follows the row is the loop's.
fn limit_reset(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Option<Decision> {
    if !ctx.limit_reset {
        return None;
    }
    let row = aterm_phase::reset_row(prompt, &ctx.reset_kinds)?;
    let title = anchor("limit.title");
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

/// The usage-limit options dialog ([`RULE_LIMIT_WAIT`]): its wait row, by
/// label, reached with arrows and confirmed with Enter once a fresh read shows
/// the focus on it (`press_focused`), never a digit or Esc, guarded on the
/// title row. A menu with no or several wait rows, no focus, or the rule off
/// is a person's.
fn limit_wait(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if !ctx.limit_wait {
        return Decision::escalate("a usage-limit dialog (limit_wait is off)");
    }
    let title = anchor("limit.title");
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
            anchor("limit.wait"),
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
    // The row its title opens on, its line read across the rows a narrow
    // pane wraps it onto (`What do you want to` over `do?` at 26 columns).
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

/// Claude Code's autocompact offer ([`RULE_AUTOCOMPACT_OFFER`];
/// aterm-phase's "THE AUTOCOMPACT OFFER"): its accept row, read whole
/// ([`aterm_phase::Offer`]), at [`ApprovalCtx::autocompact_offer`]'s size.
/// One keystroke's worth of decision per fresh read, so every key meets the
/// dialog as judged:
///
/// * the size is the target → [`Choice::Focus`] onto the accept row, whose
///   Enter `press_focused` sends only once a fresh read shows the focus there
///   (`steps` 0 when it is already);
/// * the size is not → [`Choice::FocusKey`] with `right` (or `left`): the
///   focus moved onto the accept row first, then ONE step of the stepper, and
///   the dialog is decided again from the read after it — never an Enter on
///   a size that is not the target. The loop bounds the stepper across
///   reads (`approval_loop::StepperTrack`): the server's size list need not
///   hold the target, and a key that turns back, does not move the size, or
///   exceeds the longest list hands the offer to a person.
///
/// Never the keep row, never Esc (Claude Code counts it toward the offer's
/// ask limit), never a printable key (the dialog counts one as a stray and
/// disables itself). A dialog not read whole, a size or target off the
/// ladder, or the key off is a person's. The arrows are guarded on the title
/// row; the Enter or step on the focused row itself.
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
    // The target is a size of the ladder in kilo-tokens; the offer's size is
    // the server's, in tokens as drawn (`350.5k`).
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

#[cfg(test)]
#[path = "approval_tests.rs"]
pub(crate) mod tests;

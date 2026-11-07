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
use super::git_config::{GitView, WorkerEnv, cd_operand, cdpath_searched, git_reads, hazard};
use super::guard::row_guard;
use super::probe::{Below, PolicyProbe, WorkerWrites};
use super::rm_breaker::{RmScope, ScratchRoot, abs_components, clip, resolve_rm_line};
use super::shell_startup::ShellStartup;
use super::trace::{NoTrace, Outcome, Trace, note};
use crate::harness::disk::fence;
use crate::harness::lights::Mode;
use crate::supervise::classify::{
    QUOTED_SUBSTITUTION, SUBSTITUTION, brace_expansion, classify_command_except,
    classify_command_with, classify_except_rm, command_words, command_words_globbed,
    expanded_parameters, git_grep_operands, git_log_operands, git_subcommand, git_tree_path,
    glob_match, grammar_names_operand, path_operands_except, path_operands_of, plain_zsh_line,
    program, runs, unquoted_glob, unredirected,
};

/// Rule identifier for the read-only Bash classifier approval rule.
pub const RULE_READ_ONLY: &str = "read-only@classify-v13";
/// [`RULE_READ_ONLY`]'s proof over a Bash box whose one note is Claude Code's no-rule note
/// (`This command requires approval`).
pub const RULE_NO_RULE_READ_ONLY: &str = "no-rule-read-only@v8";
/// The rm circuit-breaker rule's id; it does not run [`proof_gaps_clear`], whose assignments the
/// rm resolver follows.
pub const RULE_RM_BREAKER: &str = "rm-breaker@v11";
/// The rm circuit breaker's too-many box ([`RmBreakerKind::TooManySubstitutions`]) over a
/// command that names no rm.
pub const RULE_BREAKER_READ_ONLY: &str = "breaker-read-only@v13";
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
/// Claude Code's `Claude in Chrome` intro ([`chrome_intro`]).
pub const RULE_CHROME_INTRO: &str = "chrome-intro@v1";
/// Preserve the conversation selected by the resume path.
pub const RULE_CONVERSATION_RESUME: &str = "conversation-resume@v1";
/// A disk-free worker's fence ([`ApprovalCtx::disk_worker`], design
/// `docs/DESIGN-disk-free-ask-2026-10-24.md` §8.4): a Bash box whose every command is a read or
/// `aterm harness disk` ([`crate::harness::disk::fence::runs_harness_disk`]) is approved under
/// it, and every other permission box the worker raises is declined under it, with
/// [`DISK_WORKER_DECLINE`] or, for such a Bash box that a proof did not hold for, why not.
pub const RULE_DISK_WORKER: &str = "disk-worker@v5";

/// What [`RULE_DISK_WORKER`] types into a disk-free worker's refusal of a command that is not a
/// read nor `aterm harness disk` or a System State read, and of every other tool's box.
pub const DISK_WORKER_DECLINE: &str = "aterm's disk-free worker removes only through `aterm \
                                       harness disk free`; keep it and name it in your report";

/// Why [`RULE_DISK_WORKER`] declines a disk-free worker's read or `aterm harness disk` while no
/// shell snapshot stands ([`super::shell_startup::unwritten`]): Claude Code writes one only once
/// its Bash tool has run a command in a session, so a worker's first box, in a config folder where
/// no running session ran one, cannot be proven.
pub const DISK_WORKER_SHELL_UNREAD: &str = "aterm cannot read your Bash tool's shell startup \
                                            yet: Claude Code writes its shell snapshot only once \
                                            its Bash tool has run a command";

/// What [`RULE_DISK_WORKER`]'s refusal then tells the worker to do ([`DISK_WORKER_SHELL_UNREAD`]):
/// `pwd` is a read Claude Code runs without a box, and running it writes the snapshot. The
/// refusal keeps no count, and a `pwd` writes none when Claude Code's snapshot fails (its login
/// shell outruns Claude Code's 10 s, or the write fails on a full volume), so it also says what to
/// do once `pwd` has not helped.
pub const DISK_WORKER_FIRST_BASH: &str = "Run `pwd` alone first, which Claude Code runs without \
                                          asking, then run this command again; if you already \
                                          ran `pwd`, keep it and name it in your report";

/// Why [`RULE_DISK_WORKER`] declines a disk-free worker's Bash box of reads and `aterm harness
/// disk` alone that Claude Code drew on more than one gutter row (`│`): a row it wrapped reads the
/// same as a line of its own, and every reading of the rows must pass, so the rows read as lines
/// make a second command that is a path.
pub const DISK_WORKER_WRAPPED: &str = "Claude Code drew it on more than one row, and aterm cannot \
                                       tell a row Claude Code wrapped from a second command";

/// What [`RULE_DISK_WORKER`]'s refusal then tells the worker to do ([`DISK_WORKER_WRAPPED`]).
pub const DISK_WORKER_ONE_ROW: &str = "Run it again short enough for one row: one path per \
                                       command, with your home written as `~`";

/// The rules that approve a box as a read, whose approvals a disk-free worker's fence keeps
/// ([`disk_fence`]).
const READ_RULES: &[&str] = &[
    RULE_READ_ONLY,
    RULE_NO_RULE_READ_ONLY,
    RULE_BREAKER_READ_ONLY,
    RULE_READ_OUTSIDE_CWD,
];

/// A credit-claim worker's fence ([`ApprovalCtx::claim_worker`],
/// [`crate::harness::credit_offers`]): its questions are declined on the dialog's own Esc, never
/// handed to a person, whatever `answer_questions` says ([`CLAIM_WORKER_QUESTION`]); a Claude in
/// Chrome action keeps the rules' approval only where it presses the box's
/// one-shot allow (never the site-wide grant for the session) on a page
/// [`crate::harness::credit_offers::claim_page`] admits; a Claude in Chrome tool's own Tool box
/// ([`crate::harness::credit_offers::CHROME_TOOL`]) is decided as for every session; every other
/// tool box is declined ([`CLAIM_WORKER_TOOLS`]), a Bash box among them: the worker starts with
/// no built-in tool but `ToolSearch` ([`crate::harness::credit_offers::CLAIM_TOOLS`]). A box it
/// declines that has no `No` to amend (Claude in Chrome's own) is refused on its own Esc
/// ([`CLAIM_WORKER_ESC`]), never handed to a person. Under `approve = "none"` every permission
/// box stays the owner's.
pub const RULE_CLAIM_WORKER: &str = "claim-worker@v1";

/// Why [`RULE_CLAIM_WORKER`] declines a claim worker's box of a tool other than Claude in
/// Chrome's.
pub const CLAIM_WORKER_TOOLS: &str = "aterm's credit-claim worker uses only its Claude in Chrome \
                                      tools, on the vendor's claim pages";

/// What [`RULE_CLAIM_WORKER`]'s refusal tells the worker to do, of a tool's box and a page off
/// its claim alike: the claim task's own ending where it cannot go on
/// ([`crate::harness::credit_offers::claim_task`]), a failed conclusion, never a defense.
pub const CLAIM_WORKER_FIX: &str = "Go on without it on the claim page, or end with one reply \
                                    whose first line is `Could not finish: NOT CLAIMED:` and \
                                    why, with no `Nothing left:` line";

/// A question dialog answered under the priority word `long-term` ([`QuestionPriority::LongTerm`]).
pub const RULE_ANSWER_LONG_TERM: &str = "answer-long-term@v1";
/// The same under `short-term` ([`super::question::SHORT_TERM_TEXT`]).
pub const RULE_ANSWER_SHORT_TERM: &str = "answer-short-term@v1";
/// A question dialog answered under Autopilot Land ([`QuestionPriority::Land`]).
pub const RULE_ANSWER_LAND: &str = "answer-land@v1";
/// A prose question answered under `long-term` with its policy text instead of
/// `[harness] answer_text`.
pub const RULE_PROSE_LONG_TERM: &str = "prose-long-term@v1";
/// The same under `short-term`.
pub const RULE_PROSE_SHORT_TERM: &str = "prose-short-term@v1";

/// A priority a person sets: what the `[harness] question_priority` key and a session's
/// `questions` word spell. It is [`QuestionPriority`] less Land's, which no person sets, so no
/// configuration carries Land's priority: a dialog takes Land's from the approval loop (the
/// session's Autopilot) or from a box's facts ([`QuestionPriority::of_fact`]), and a prose
/// question under Land gets the turn-end policy's own Land answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum PriorityWord {
    #[default]
    Recommended,
    LongTerm,
    ShortTerm,
}

impl PriorityWord {
    /// Every word, in [`Self::WORDS`]' order.
    pub const ALL: [Self; 3] = [Self::Recommended, Self::LongTerm, Self::ShortTerm];
    /// How the config key and the session word spell them, in [`Self::ALL`]'s order.
    pub const WORDS: [&'static str; 3] = ["recommended", "long-term", "short-term"];

    /// The word `word` names, trimmed and with its ASCII case folded, or `None` for any other
    /// text (`land` included: no person sets it).
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

    /// The rule a prose question answered under it is ledgered as.
    #[must_use]
    pub const fn prose_rule_id(self) -> &'static str {
        match self {
            Self::Recommended => super::turn_end::RULE_ANSWER,
            Self::LongTerm => RULE_PROSE_LONG_TERM,
            Self::ShortTerm => RULE_PROSE_SHORT_TERM,
        }
    }

    /// Its policy text, which a prose question is answered with and a dialog's free-text row is
    /// written with: its [`QuestionPriority`]'s.
    #[must_use]
    pub fn text(self) -> Option<&'static str> {
        QuestionPriority::from(self).text()
    }
}

impl From<PriorityWord> for QuestionPriority {
    fn from(word: PriorityWord) -> Self {
        match word {
            PriorityWord::Recommended => Self::Recommended,
            PriorityWord::LongTerm => Self::LongTerm,
            PriorityWord::ShortTerm => Self::ShortTerm,
        }
    }
}

/// How a question dialog is answered once [`ApprovalCtx::answer_questions`] says it is: under a
/// person's [`PriorityWord`] or under Land's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum QuestionPriority {
    #[default]
    Recommended,
    LongTerm,
    ShortTerm,
    /// Autopilot Land's: never a word a person sets; the approval loop takes it for a session
    /// whose Autopilot is Land.
    Land,
}

impl QuestionPriority {
    /// Every priority: each [`PriorityWord`]'s, in [`PriorityWord::ALL`]'s order, and
    /// [`Self::Land`].
    pub const EVERY: [Self; 4] = [
        Self::Recommended,
        Self::LongTerm,
        Self::ShortTerm,
        Self::Land,
    ];

    /// The priority a box's facts or a tape name by its [`Self::word`], `land` included.
    #[must_use]
    pub fn of_fact(word: &str) -> Option<Self> {
        Self::EVERY.into_iter().find(|p| p.word() == word)
    }

    /// Its word: its [`PriorityWord`]'s, or `land`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Recommended => PriorityWord::Recommended.word(),
            Self::LongTerm => PriorityWord::LongTerm.word(),
            Self::ShortTerm => PriorityWord::ShortTerm.word(),
            Self::Land => "land",
        }
    }

    /// The rule a question dialog answered under it is ledgered as.
    #[must_use]
    pub const fn rule_id(self) -> &'static str {
        match self {
            Self::Recommended => super::question::RULE_ANSWER_RECOMMENDED,
            Self::LongTerm => RULE_ANSWER_LONG_TERM,
            Self::ShortTerm => RULE_ANSWER_SHORT_TERM,
            Self::Land => RULE_ANSWER_LAND,
        }
    }

    /// The fixed policy text written into a free-text row; a [`PriorityWord`]'s answers a prose
    /// question too ([`PriorityWord::text`]).
    #[must_use]
    pub const fn text(self) -> Option<&'static str> {
        match self {
            Self::Recommended => None,
            Self::LongTerm => Some(super::question::LONG_TERM_TEXT),
            Self::ShortTerm => Some(super::question::SHORT_TERM_TEXT),
            Self::Land => Some(super::question::LAND_TEXT),
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
    /// The session is aterm's disk-free worker: its task record's trusted folder is the worker
    /// folder ([`crate::harness::disk::fence::is_worker_folder`]), so its boxes are decided under
    /// [`RULE_DISK_WORKER`]'s fence.
    pub disk_worker: bool,
    /// The session is aterm's credit-claim worker: its task record's trusted folder is the claim
    /// folder ([`crate::harness::credit_offers::is_claim_folder`]), so its boxes are decided under
    /// [`RULE_CLAIM_WORKER`]'s fence.
    pub claim_worker: bool,
    pub worker_agent: Option<Program>,
    pub read_roots: Vec<ScratchRoot>,
    pub scratch_roots: Vec<ScratchRoot>,
    pub rm_breaker_warn: bool,
    pub secrets: Vec<SecretRule>,
    pub python_allow: Vec<String>,
    pub question_priority: QuestionPriority,
    pub question_own: Option<QuestionPriority>,
    /// A person gave input since the loop's first policy text on this dialog.
    pub question_person: bool,
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
            disk_worker: false,
            claim_worker: false,
            worker_agent: None,
            read_roots: Vec::new(),
            scratch_roots,
            rm_breaker_warn: !defaults.rm_breaker_ask,
            python_allow: Vec::new(),
            question_priority: defaults.question_priority.into(),
            question_own: None,
            question_person: false,
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

    /// The directories the session's command may start in: its cwd, then where its Bash tool
    /// stands ([`Self::shell_cwds`]).
    fn shell_dirs(&self) -> Vec<PathBuf> {
        let mut cwds = vec![self.cwd.clone()];
        cwds.extend(self.shell_cwds.iter().filter(|d| **d != self.cwd).cloned());
        cwds
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
    let o = chosen_option(p, choice)?;
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

/// The option of `p` that `choice` presses: by its digit, or the label the focus moves to; `None`
/// for an Esc, a question's answer or a typed note.
fn chosen_option<'p>(p: &'p PromptV2, choice: &Choice) -> Option<&'p Opt> {
    match choice {
        Choice::Digit(n) => p.options.iter().find(|o| o.n == Some(*n)),
        Choice::FocusKey { key: "esc", .. } => None,
        Choice::Focus { label, .. } | Choice::FocusKey { label, .. } => {
            p.options.iter().find(|o| o.label == *label)
        }
        Choice::Answer(_) | Choice::Note { .. } => None,
    }
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
    let decided = decide_unfenced(reading, rows, ctx, trace);
    let (check, fenced) = if ctx.disk_worker {
        (
            "disk-worker",
            disk_fence(reading, rows, ctx, decided.clone()),
        )
    } else if ctx.claim_worker {
        (
            "claim-worker",
            claim_fence(reading, rows, ctx, decided.clone()),
        )
    } else {
        return decided;
    };
    if fenced != decided {
        note(trace, check, fenced.rule_id(), || {
            Outcome::Overruled(said(&fenced))
        });
    }
    fenced
}

/// [`RULE_CLAIM_WORKER`]'s fence over `decided`, what the rules made of a credit-claim worker's
/// box. A question is declined, never answered and never handed to a person, whatever
/// `answer_questions` and `approve` say ([`claim_question`]): the worker's task asks none, and
/// nobody chose for the person what one asks.
/// Of a tool box ([`fenced`]) under `approve = "none"`, the owner's decision stands. A Claude in
/// Chrome action ([`PromptKind::Browser`]) keeps an approval only where it presses the box's
/// one-shot allow on a page [`crate::harness::credit_offers::claim_page`] admits
/// ([`claim_action`]); a refusal pressed or a hand-off the rules made stands. A Claude in Chrome
/// tool's Tool box ([`crate::harness::credit_offers::CHROME_TOOL`]: its reads and navigation; its
/// site actions raise the browser box) is decided as for every session. Every other tool box is
/// declined on its `No`. A box the fence declines that has no `No` to amend (Claude in Chrome's
/// own has `Deny (esc)` and no amend) is refused on its own Esc ([`claim_esc`]), so no person is
/// asked and the worker goes on from its task; only one the reader does not vouch for, or one
/// with neither a `No` nor an Esc that refuses, is handed to a person, as nothing is pressed on
/// it ([`disk_fence`]'s rule).
fn claim_fence(
    reading: &Reading,
    rows: &[String],
    ctx: &ApprovalCtx,
    decided: Decision,
) -> Decision {
    use crate::harness::credit_offers;
    let Some(p) = reading.prompt.as_ref() else {
        return decided;
    };
    if p.kind == PromptKind::Question {
        return claim_question(reading, p, rows, decided);
    }
    if !fenced(p.kind) || ctx.approve == Approve::None {
        return decided;
    }
    let refused = |what: &str, why: &str| {
        let not_declined = |not: &str| {
            Decision::escalate(format!(
                "a credit-claim worker's box, which its fence ({RULE_CLAIM_WORKER}) declines \
                 ({why}), was not declined: {not} ({})",
                said(&decided)
            ))
        };
        if !draws_claude_boxes(reading.program)
            || reading.phase != Phase::Prompt
            || !reading.phase_authoritative
        {
            return not_declined("no box the reader vouches for");
        }
        let shown = if p.command.is_empty() {
            p.title.as_str()
        } else {
            p.command.as_str()
        };
        let subject = format!("{} box: {shown}", p.kind.name());
        decline(
            RULE_CLAIM_WORKER,
            p,
            decline_text(what, why, CLAIM_WORKER_FIX),
            subject.clone(),
        )
        .or_else(|no| {
            claim_esc(
                p,
                rows,
                &format!("{subject}{SUBJECT_OPTION}{CLAIM_WORKER_ESC}"),
            )
            .map_err(|esc| format!("{no}, and {esc}"))
        })
        .unwrap_or_else(|not| not_declined(&not))
    };
    match p.kind {
        PromptKind::Browser => {
            let Decision::Approve { choice, .. } = &decided else {
                return decided;
            };
            match claim_action(p, choice) {
                Ok(()) => decided,
                Err(why) => refused("this Chrome action", &why),
            }
        }
        PromptKind::Tool if p.command.starts_with(credit_offers::CHROME_TOOL) => decided,
        _ => refused("this tool call", CLAIM_WORKER_TOOLS),
    }
}

/// What [`RULE_CLAIM_WORKER`]'s refusal of a claim worker's box with no `No` to amend says it
/// pressed: Claude in Chrome's own box (`Deny (esc)`), whose refusal takes no words.
pub const CLAIM_WORKER_ESC: &str = "Esc, the box's own refusal: aterm's credit-claim worker \
                                    does not take this action";

/// [`RULE_CLAIM_WORKER`]'s press of `p`'s own `Esc` refusal ([`aterm_phase::prompt::CancelEffect::Reject`]),
/// on its focused row, guarded by that row, for a box or dialog the fence declines that has no
/// `No` to amend: a question, or Claude in Chrome's box (`Deny (esc)`). It types nothing.
/// `Err`: the box reads no `Esc` that refuses, or no focused row is on the rows read, and nothing
/// is pressed.
fn claim_esc(p: &PromptV2, rows: &[String], subject: &str) -> Result<Decision, String> {
    if !p
        .cancel
        .as_ref()
        .is_some_and(|c| c.effect == aterm_phase::prompt::CancelEffect::Reject)
    {
        return Err("it reads no `Esc` that refuses it".to_string());
    }
    let Some((focused, row)) = p
        .focused()
        .and_then(|o| rows.get(o.row).map(|row| (o, row)))
    else {
        return Err("no focused row is on the rows read".to_string());
    };
    Ok(Decision::Approve {
        rule_id: RULE_CLAIM_WORKER,
        choice: Choice::FocusKey {
            steps: 0,
            label: focused.label.clone(),
            key: "esc",
        },
        guard: row_guard(row),
        subject: subject.to_string(),
        unproven: None,
    })
}

/// What [`RULE_CLAIM_WORKER`]'s decline of a claim worker's question says it pressed.
pub const CLAIM_WORKER_QUESTION: &str = "Esc, the whole dialog declined with nothing chosen: \
                                         aterm's credit-claim worker asks no question";

/// [`RULE_CLAIM_WORKER`]'s decline of a claim worker's question `p`, whatever the rules made of
/// it (`decided`): the dialog's own `Esc to cancel`, which declines the whole dialog with nothing
/// chosen (measured, `docs/measured/claude-code-dialogs-2026-09-24.md`), pressed on the focused
/// row. The worker's task asks no question and says how it ends, so the worker goes on from its
/// task and Autopilot Land's next act, and no person is asked. A question dialog has no `No` to
/// type a refusal into, and only a policy's own fixed text is ever written into one, so the
/// decline types nothing ([`claim_esc`]). Where the reader does not vouch for the dialog, or it
/// reads no `Esc` that cancels or no focused row, nothing is pressed and it is handed over, as an
/// unread box is.
fn claim_question(reading: &Reading, p: &PromptV2, rows: &[String], decided: Decision) -> Decision {
    let not_declined = |not: &str| {
        Decision::escalate(format!(
            "a credit-claim worker's question, which its fence ({RULE_CLAIM_WORKER}) declines, \
             was not declined: {not} ({})",
            said(&decided)
        ))
    };
    if !draws_claude_boxes(reading.program)
        || reading.phase != Phase::Prompt
        || !reading.phase_authoritative
    {
        return not_declined("no box the reader vouches for");
    }
    claim_esc(
        p,
        rows,
        &format!("{}{SUBJECT_OPTION}{CLAIM_WORKER_QUESTION}", p.title),
    )
    .unwrap_or_else(|not| not_declined(&not))
}

/// Why the Claude in Chrome box `p`, approved with `choice`, is no action of a claim worker's
/// [`RULE_CLAIM_WORKER`] keeps; `Ok` where the choice presses `p`'s refusal (a decline the rules
/// made), or its one-shot allow on a page [`crate::harness::credit_offers::claim_page`] admits
/// whose title names a vendor host, where it names one (`… wants to click on <host>`).
fn claim_action(p: &PromptV2, choice: &Choice) -> Result<(), String> {
    use crate::harness::credit_offers;
    let Some(o) = chosen_option(p, choice) else {
        return Err("its press is no option of the box".to_owned());
    };
    if o.role == Role::Deny {
        return Ok(());
    }
    if o.role != Role::Once {
        return Err(format!(
            "`{}` is not the box's one-shot allow",
            clip(&o.label, 60)
        ));
    }
    credit_offers::claim_page(&p.command)?;
    let title = p.base_title().unwrap_or_else(|| p.title.clone());
    match title.rsplit_once(" on ") {
        Some((_, host)) if !credit_offers::vendor_host(host.trim()) => Err(format!(
            "its box names {} as the site",
            clip(host.trim(), 60)
        )),
        _ => Ok(()),
    }
}

/// The tool boxes an agent raises for a call it makes: the boxes a disk-free worker's fence
/// decides ([`disk_fence`]). A question, the folder-trust dialog, and the dialogs about the
/// session (a usage limit, a model switch, the autocompact and auto-mode offers, the Claude in
/// Chrome intro, an MCP server, a held message, one of no kind aterm reads) are left to their own
/// rules.
const fn fenced(kind: PromptKind) -> bool {
    match kind {
        PromptKind::Bash
        | PromptKind::PowerShell
        | PromptKind::Edit
        | PromptKind::Write
        | PromptKind::Overwrite
        | PromptKind::Read
        | PromptKind::Workflow
        | PromptKind::Fetch
        | PromptKind::NetworkRequest
        | PromptKind::Browser
        | PromptKind::Skill
        | PromptKind::Monitor
        | PromptKind::Tool
        | PromptKind::PlanEnter
        | PromptKind::PlanExit
        | PromptKind::GoalProposal
        | PromptKind::ComputerUse
        | PromptKind::ReadOutsideSetting => true,
        PromptKind::Trust
        | PromptKind::Question
        | PromptKind::HeldMessage
        | PromptKind::UsageLimit
        | PromptKind::ModelSwitch
        | PromptKind::AutocompactOffer
        | PromptKind::McpServer
        | PromptKind::AutoModeSetup
        | PromptKind::ConversationResume
        | PromptKind::ChromeIntro
        | PromptKind::Other => false,
    }
}

/// [`RULE_DISK_WORKER`]'s fence over `decided`, what the rules made of a disk-free worker's box
/// on screen `rows`: a tool box ([`fenced`]) keeps an approval as a read ([`READ_RULES`]) or of
/// `aterm harness disk` ([`disk_verbs`]), and any other decision of one, an approval at full power
/// or a hand-off to a person alike, becomes a decline ([`disk_decline_text`]). Under `approve =
/// "none"` every box stays the owner's; a box the reader does not vouch for, or one with no `No`
/// to amend, is handed to a person, as nothing is pressed on it.
fn disk_fence(
    reading: &Reading,
    rows: &[String],
    ctx: &ApprovalCtx,
    decided: Decision,
) -> Decision {
    let Some(p) = reading.prompt.as_ref().filter(|p| fenced(p.kind)) else {
        return decided;
    };
    if ctx.approve == Approve::None {
        return decided;
    }
    if let Decision::Approve { rule_id, .. } = &decided
        && (READ_RULES.contains(rule_id) || *rule_id == RULE_DISK_WORKER)
    {
        return decided;
    }
    let not_declined = |why: &str| {
        Decision::escalate(format!(
            "a disk-free worker's box, which its fence ({RULE_DISK_WORKER}) declines, was not \
             declined: {why} ({})",
            said(&decided)
        ))
    };
    if !draws_claude_boxes(reading.program)
        || reading.phase != Phase::Prompt
        || !reading.phase_authoritative
    {
        return not_declined("no box the reader vouches for");
    }
    let what = if !p.command.is_empty() {
        p.command.as_str()
    } else if let Some(path) = p.path.as_deref() {
        path
    } else {
        p.title.as_str()
    };
    let subject = format!("{} box: {what}", p.kind.name());
    decline(
        RULE_DISK_WORKER,
        p,
        disk_decline_text(p, rows, ctx),
        subject,
    )
    .unwrap_or_else(|not| not_declined(&not))
}

/// What [`RULE_DISK_WORKER`] tells a disk-free worker whose box `p` (on screen `rows`) its fence
/// declines: for a Bash command that is only reads, `aterm harness disk` and System State reads
/// but that a proof of [`disk_verbs`] did not hold, why it was not proven (while no shell
/// snapshot stands, [`DISK_WORKER_SHELL_UNREAD`] and [`DISK_WORKER_FIRST_BASH`]), and for one
/// that only its rows read as lines fence ([`disk_fenced_by_rows`]), [`DISK_WORKER_WRAPPED`] and
/// [`DISK_WORKER_ONE_ROW`], as the verb it is allowed is no remedy for either; for every other
/// box, [`DISK_WORKER_DECLINE`].
fn disk_decline_text(p: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> String {
    if p.kind != PromptKind::Bash {
        return DISK_WORKER_DECLINE.to_string();
    }
    match disk_verbs(p, rows, ctx) {
        Err(NotDiskVerb::Unproven(why)) => match &ctx.shell {
            Err(shell) if super::shell_startup::unwritten(shell) => decline_text(
                "this command",
                DISK_WORKER_SHELL_UNREAD,
                DISK_WORKER_FIRST_BASH,
            ),
            _ => decline_text(
                "this command",
                &format!("aterm could not prove it only reads or runs `aterm harness disk`: {why}"),
                "Keep it and name it in your report",
            ),
        },
        Err(NotDiskVerb::Fenced(_)) if disk_fenced_by_rows(p, rows, ctx) => {
            decline_text("this command", DISK_WORKER_WRAPPED, DISK_WORKER_ONE_ROW)
        }
        Ok(_) | Err(NotDiskVerb::Fenced(_)) => DISK_WORKER_DECLINE.to_string(),
    }
}

/// Whether the disk-free worker's Bash box `p`, drawn with a gutter on more than one row, is
/// fenced only by its rows read as lines: read as one line Claude Code wrapped, every command of
/// it is a read, `aterm harness disk` or a System State read ([`disk_words`]).
fn disk_fenced_by_rows(p: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> bool {
    if !p.gutter || p.command_rows.len().saturating_sub(p.description_rows) < 2 {
        return false;
    }
    let wrapped = PromptV2 {
        gutter: false,
        ..p.clone()
    };
    disk_words(&wrapped, rows, ctx).is_ok()
}

fn decide_unfenced(
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
        && prompt.kind == PromptKind::ConversationResume
    {
        let decision = if !draws_claude_boxes(reading.program)
            || reading.phase != Phase::Prompt
            || !reading.phase_authoritative
            || ctx.approve == Approve::None
        {
            Decision::escalate("no authorized conversation resume dialog")
        } else {
            conversation_resume(prompt, rows)
        };
        return decided_at(trace, "conversation-resume", decision);
    }
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
        && prompt.kind == PromptKind::ChromeIntro
    {
        let decision = if !draws_claude_boxes(reading.program)
            || reading.phase != Phase::Prompt
            || !reading.phase_authoritative
        {
            Decision::escalate("no Claude in Chrome intro the reader vouches for")
        } else {
            chrome_intro(prompt, rows, ctx)
        };
        return decided_at(trace, "chrome-intro", decision);
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
                super::login::pick_method(&picker, ctx.login_method)
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
                (ctx.question_fell_back, ctx.question_person),
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
        PromptKind::Bash => match bash(prompt, rows, ctx) {
            Decision::Escalate { reason } if ctx.disk_worker => disk_verbs(prompt, rows, ctx)
                .unwrap_or_else(|not| {
                    Decision::escalate(format!("{reason}; nor {RULE_DISK_WORKER}'s: {}", not.why()))
                }),
            decided => decided,
        },
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
        .and_then(|()| secrets_unreached_in(&readings, ctx, &|_| false))
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

/// Why [`disk_verbs`] did not approve a disk-free worker's Bash box.
enum NotDiskVerb {
    /// The box is none this rule reads, or a command of it is neither a read nor `aterm harness
    /// disk` or a System State read: the fence's own decline ([`DISK_WORKER_DECLINE`]).
    Fenced(String),
    /// Every command of it is one, but a proof a read is held to did not hold: why.
    Unproven(String),
}

impl NotDiskVerb {
    fn why(&self) -> &str {
        match self {
            Self::Fenced(why) | Self::Unproven(why) => why,
        }
    }
}

/// Whether the command `seg` is one [`RULE_DISK_WORKER`] passes beside the reads: `aterm harness
/// disk` ([`fence::runs_harness_disk`]) or a System State read ([`fence::runs_ctl_sys`]).
fn disk_passed(seg: &[String]) -> bool {
    fence::runs_harness_disk(seg) || fence::runs_ctl_sys(seg)
}

/// [`RULE_DISK_WORKER`]'s approval of a disk-free worker's Bash box that [`bash`] did not prove:
/// every command of every reading is a read the classifier proves, `aterm harness disk` or a
/// System State read (`aterm ctl … sys`, [`disk_passed`]), with no vendor note but the no-rule
/// one, no rm circuit breaker, no bypass session, this machine's shell, and no socket or brace
/// expansion ([`disk_words`]; else [`NotDiskVerb::Fenced`]), under every other proof [`bash`]
/// holds a read to: no unshown blank or proof gap, the shell startup read and its names clear,
/// git reads clear, and no secret read by a command other than aterm's (else
/// [`NotDiskVerb::Unproven`]).
fn disk_verbs(
    prompt: &PromptV2,
    rows: &[String],
    ctx: &ApprovalCtx,
) -> Result<Decision, NotDiskVerb> {
    let (readings, subject, first_row) =
        disk_words(prompt, rows, ctx).map_err(NotDiskVerb::Fenced)?;
    blanks_shown(&readings)
        .and_then(|()| proof_gaps_clear(&readings))
        .and_then(|()| environment_unread(&readings))
        .and_then(|()| shell_models_hold(&readings, None, ctx))
        .and_then(|()| plain_under_zsh(&readings, ctx))
        .and_then(|()| git_reads_clear(&readings, None, ctx))
        .and_then(|()| secrets_unreached_in(&readings, ctx, &disk_passed))
        .map_err(NotDiskVerb::Unproven)?;
    Ok(Decision::Approve {
        rule_id: RULE_DISK_WORKER,
        choice: once_choice(prompt).map_err(NotDiskVerb::Fenced)?,
        guard: row_guard(&rows[first_row]),
        subject,
        unproven: None,
    })
}

/// The words [`disk_verbs`] reads off a disk-free worker's Bash box `prompt` on screen `rows`:
/// its readings, its subject and its command's first row, when it is a Bash box this rule reads
/// whose every command is a read, `aterm harness disk` or a System State read ([`disk_passed`]);
/// else why not.
fn disk_words(
    prompt: &PromptV2,
    rows: &[String],
    ctx: &ApprovalCtx,
) -> Result<(Vec<String>, String, usize), String> {
    if prompt.base_title().as_deref() != Some(aterm_phase::anchor!("box.bash")) {
        return Err(format!(
            "the box header is not a Bash header: {}",
            prompt.title
        ));
    }
    let first = prompt
        .command_rows
        .first()
        .ok_or("a Bash box with no command row")?;
    let first_row =
        row_of(rows, prompt, first).ok_or("the command's first row is not on the screen")?;
    no_option_above_the_question(prompt, rows)?;
    if let Some(b) = prompt.rm_breaker() {
        return Err(rm_breaker_label(&b));
    }
    match prompt.notes.as_slice() {
        [] => {}
        [note] if note == aterm_phase::anchor!("box.note_no_rule") => {}
        [note, ..] => return Err(format!("the box carries a vendor note: {note}")),
    }
    if ctx.bypass_mode {
        return Err("a box in a bypass session is a vendor circuit breaker".to_string());
    }
    if let Some(machine) = prompt.runs_on() {
        return Err(format!(
            "a command on {machine}: `aterm harness disk` frees this machine's disk"
        ));
    }
    let (readings, subject) = command_readings(prompt, rows, false);
    if readings.is_empty() {
        return Err("a Bash box with no command".to_string());
    }
    network_free(&readings).and_then(|()| brace_free(&readings))?;
    for line in &readings {
        let v = classify_command_except(line, &ctx.python_allow, &disk_passed);
        if !v.read_only {
            return Err(format!(
                "neither a read nor `aterm harness disk` ({}): {}",
                reading_name(line),
                v.reason
            ));
        }
    }
    Ok((readings, subject, first_row))
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
/// `xargs` on the walk to a program, a loop variable, or an unquoted glob in the words of a program
/// off [`GLOB_SAFE`] ([`command_words_globbed`]: its own command's words or a substitution's among
/// them, never another command's, so `ls /etc/*.conf | sort` holds no glob of sort's; a glob the
/// line holds where no command's words do counts against every command).
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
            command_words_globbed(line).map_err(|why| format!("not read-only ({name}): {why}"))?;
        let stray = unquoted_glob(line).filter(|_| segments.iter().all(|(_, g)| g.is_none()));
        for (seg, own) in &segments {
            let glob = own.or(stray);
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
    let cwds = ctx.shell_dirs();
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

/// `Ok` when the Bash tool's shell may not be zsh, or every reading is a plain zsh line
/// ([`plain_zsh_line`]), held to `EXTENDED_GLOB`'s operators where the startup may set it
/// ([`ShellStartup::may_extended_glob`]).
fn plain_under_zsh(readings: &[String], ctx: &ApprovalCtx) -> Result<(), String> {
    let Some(shell) = ctx.shell.as_ref().ok().filter(|s| s.may_run_zsh()) else {
        return Ok(());
    };
    for line in readings {
        plain_zsh_line(line, shell.may_extended_glob())
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
/// no brace expansion, turns on no blank the screen cannot show, holds no proof gap, and prints
/// no value of the environment ([`environment_unread`]).
pub(crate) fn readings_proven(readings: &[String], python_allow: &[String]) -> Result<(), String> {
    read_only_every_reading(readings, python_allow)?;
    blanks_shown(readings)?;
    proof_gaps_clear(readings)?;
    environment_unread(readings)
}

/// The filter jq's `args` give it: its first operand past the options and their values; `None`
/// when it reads one from a file (`-f`, `--from-file`), `Some("")` when it has none.
fn jq_filter(args: &[String]) -> Option<&str> {
    let mut k = 0;
    while let Some(a) = args.get(k).map(String::as_str) {
        match a {
            "-f" | "--from-file" => return None,
            "--arg" | "--argjson" | "--slurpfile" | "--rawfile" => k += 3,
            "--indent" | "-L" => k += 2,
            _ if a.starts_with('-') => k += 1,
            _ => return Some(a),
        }
    }
    Some("")
}

/// The parameters a read may expand, whose values name the session's own places (Windows' home
/// as `USERPROFILE`) and the shell's counters, never a credential.
const PLAIN_PARAMETERS: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "PWD",
    "OLDPWD",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "TMPDIR",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "HOSTNAME",
    "UID",
    "EUID",
    "PPID",
    "SHLVL",
    "RANDOM",
    "SECONDS",
    "LINENO",
    "COLUMNS",
    "LINES",
    "BASH_VERSION",
    "ZSH_VERSION",
];

/// `Ok` when no reading prints a value of the environment, where a token lives as a file's
/// secret does: no parameter expansion outside single quotes ([`expanded_parameters`]) of a name
/// off [`PLAIN_PARAMETERS`] (its value prints as any word does, in an error too: `cat $TOKEN`),
/// no indirection (`${!x}`), no `ps` that shows processes' environments (`-E` in a dashed cluster,
/// or `e` in a dashless one that is no option's value: MEASURED on macOS 27.0.1, where `ps -e`
/// lists every process instead), no jq filter naming `env` (a key, `.env`, is none) or `$ENV` (nor
/// one jq reads from a file, unseen), no awk program naming `ENVIRON`, and no word naming a file
/// `environ` (Linux's `/proc/<pid>/environ`).
///
/// # Errors
///
/// The first reading that prints one, named.
fn environment_unread(readings: &[String]) -> Result<(), String> {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let names_env = |w: &str| {
        w.contains("$ENV")
            || w.match_indices("env").any(|(k, _)| {
                !w[..k].ends_with(|c: char| ident(c) || c == '$' || c == '.')
                    && !w[k + 3..].starts_with(ident)
            })
    };
    for line in readings {
        let name = reading_name(line);
        let not_read = |why: String| format!("not read-only ({name}): {why}");
        for p in expanded_parameters(line).map_err(not_read)? {
            if p.starts_with('!') {
                return Err(format!(
                    "expands ${{{p}}} ({name}), an indirection whose value may be a secret"
                ));
            }
            let var: String = p
                .trim_start_matches('#')
                .chars()
                .take_while(|c| ident(*c))
                .collect();
            if !var.is_empty()
                && !var.chars().all(|c| c.is_ascii_digit())
                && !PLAIN_PARAMETERS.contains(&var.as_str())
            {
                return Err(format!(
                    "expands ${var} ({name}), a variable whose value may be a secret"
                ));
            }
        }
        for seg in &command_words(line).map_err(not_read)? {
            if seg
                .iter()
                .any(|w| w.rsplit(['/', '\\']).next() == Some("environ"))
            {
                return Err(format!("names a process's environment ({name})"));
            }
            let Ok(Some(run)) = runs(seg) else {
                continue;
            };
            let args = &seg[run.at + 1..];
            let shown = match program(&seg[run.at]) {
                "ps" => args
                    .iter()
                    .enumerate()
                    .any(|(k, a)| match a.strip_prefix('-') {
                        Some(cluster) => !cluster.starts_with('-') && cluster.contains('E'),
                        None => {
                            a.chars().all(|c| c.is_ascii_alphabetic())
                                && a.contains('e')
                                && !matches!(
                                    k.checked_sub(1).map(|j| args[j].as_str()),
                                    Some("-o" | "-O" | "-p" | "-U" | "-u" | "-G" | "-g" | "-t")
                                )
                        }
                    }),
                "jq" => jq_filter(args).is_none_or(names_env),
                "awk" => args.iter().any(|a| a.contains("ENVIRON")),
                _ => false,
            };
            if shown {
                return Err(format!(
                    "{} prints the environment ({name}), whose values may be secrets",
                    program(&seg[run.at])
                ));
            }
        }
    }
    Ok(())
}

/// [`readings_proven`], then the default secret list under this process's `HOME`: the advisory
/// word for a Bash box. It knows no session's cwd, so a relative path is read from a directory it
/// cannot place ([`Start::Unplaced`]), and it looks at no disk, so a tree a reader descends is
/// unproven ([`unlisted`]).
///
/// # Errors
///
/// The first reading that is not read-only, else the first that holds a
/// form the shell may read otherwise than drawn or the classifier cannot
/// vet, else the first that prints a value of the environment, else the
/// first that may read a secret, named.
pub fn readings_read_only(readings: &[String], python_allow: &[String]) -> Result<(), String> {
    readings_read_only_at(readings, python_allow, advisory_home().as_deref())
}

/// [`readings_read_only`] with the secret list resolved under `home`.
fn readings_read_only_at(
    readings: &[String],
    python_allow: &[String],
    home: Option<&Path>,
) -> Result<(), String> {
    readings_read_only_from(readings, python_allow, home, Start::Unplaced, &unlisted)
}

/// [`readings_read_only_at`], a relative path read from `start`, a tree a reader descends listed
/// by `list`.
fn readings_read_only_from(
    readings: &[String],
    python_allow: &[String],
    home: Option<&Path>,
    start: Start<'_>,
    list: Lister<'_>,
) -> Result<(), String> {
    readings_proven(readings, python_allow)?;
    secrets_unreached(
        readings,
        &default_secrets(home),
        home,
        start,
        &|_| false,
        LeadingDot::Any,
        list,
    )
}

/// How [`secrets_unreached`] learns what a reader may open below a tree it descends: given the
/// tree's absolute path, how many levels below it the reader reads (`usize::MAX`: any) and the
/// rules, what lies there ([`PolicyProbe::below_tree`]).
pub type Lister<'a> = &'a dyn Fn(&[String], usize, &[SecretRule]) -> Result<Below, String>;

/// The [`Lister`] of a check that looks at no disk (the advisory word): every tree is unproven.
///
/// # Errors
///
/// Always: why the tree is unproven.
pub fn unlisted(_: &[String], _: usize, _: &[SecretRule]) -> Result<Below, String> {
    Err("a tree a reader descends, whose files this check does not list".to_string())
}

/// The advisory word's verdict ([`readings_read_only`]) on `line`, a shell command line as typed,
/// with the default secret list under `home` and a relative path read from `cwd`, where the line
/// runs (from a directory the check cannot place, as the word reads it, with none), and a tree a
/// reader descends listed by `list`: `aterm drive classify`'s verdict after the classifier's, so
/// the verb approves no line the harness's rules would refuse from there.
///
/// # Errors
///
/// The first refusal, named as [`readings_read_only`] names it.
pub fn command_read_only_at(
    line: &str,
    python_allow: &[String],
    home: Option<&Path>,
    cwd: Option<&Path>,
    list: Lister<'_>,
) -> Result<(), String> {
    let cwds: Vec<PathBuf> = cwd.map(Path::to_path_buf).into_iter().collect();
    let start = if cwds.is_empty() {
        Start::Unplaced
    } else {
        Start::Known(&cwds)
    };
    readings_read_only_from(&[line.to_string()], python_allow, home, start, list)
}

/// The home the advisory word and `aterm drive classify` resolve `~` and the secret list
/// against: `HOME`, or on Windows `USERPROFILE` when `HOME` is unset or empty. With none, a read
/// that names the home is refused ([`operand_reach`]), as the home's own secret rules cannot be
/// listed.
pub(crate) fn advisory_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::env::var_os(crate::harness::accounts::HOMEDIR_ENV).filter(|h| !h.is_empty())
        })
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
        .and_then(|()| secrets_unreached_in(&readings, ctx, &|_| false));
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

/// Whether `o` is the focused refusal with `text` typed into it, read whatever its role: the box
/// reader judges an option by its words, and a decline's own words can read as a kept grant
/// ("without asking", "always"), but no option of Claude Code's reads `No, ` and then a decline's
/// text but the refusal it was typed into. A parse that placed no role (an unsound box) is no
/// such read.
fn holds_typed(o: &Opt, text: &str) -> bool {
    o.focused
        && matches!(o.role, Role::Deny | Role::Persist)
        && squashed(&o.label) == squashed(&amended_label(text))
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
    let hits: Vec<usize> = prompt
        .options
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            holds_typed(o, text)
                || (o.role == Role::Deny
                    && (o.label == REFUSAL_LABEL || (o.focused && o.label == open)))
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
    let Some(o) = prompt
        .options
        .get(refusal)
        .filter(|o| o.role == Role::Deny || holds_typed(o, text))
    else {
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
///   resolves `..` on disk) and also taken lexically, for when none does;
/// - a rooted path led by an alias the OS keeps of the root also read without it
///   ([`root_alias_free`]).
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
    if drive.is_some() || rest.starts_with(['/', '\\']) {
        for reading in out.clone() {
            out.extend(below_aliases(&reading));
        }
    }
    out
}

/// `comps`, a rooted path's, read without each alias of the root that leads it in turn
/// ([`root_alias_free`]).
fn below_aliases(comps: &[String]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut below = root_alias_free(comps);
    while let Some(c) = below {
        below = root_alias_free(&c);
        out.push(c);
    }
    out
}

/// `comps`, a rooted path's, read without an alias the OS keeps of the root that leads it, if one
/// does: macOS's lookup prefixes `/.nofollow/` and `/.resolve/<n>/`, its data volume
/// (`/System/Volumes/Data`) and a mounted volume (`/Volumes/<name>`, whose boot volume's entry is
/// a link to `/`; each MEASURED 2026-10-29 or 2026-10-30 on macOS 27.0.1 to open the home by its
/// own inode), Linux's `/proc/<pid>/root` (a thread's `/proc/<pid>/task/<tid>/…` read as its
/// process's; REASONED from proc(5)), Git Bash's `/proc/cygdrive/<drive>/` (MEASURED 2026-10-30
/// in the Windows lane) and Cygwin's `/cygdrive/<drive>/` (REASONED), each `<drive>` as
/// [`path_readings`] spells a drive (`c`), and Windows' `Documents and Settings` junction of a
/// drive's `Users` (Git Bash's `cat` and `findstr` open a file through it, MEASURED 2026-10-29 in
/// the Windows lane). A directory that holds one of these (`/.nofollow`, `/.resolve`, `/System`,
/// `/Volumes`, `/proc`, a process's) is read as the root, for a recursive reader that descends
/// it. Names are compared by [`same_name`], `.nofollow` and `.resolve` too, which macOS reads
/// case-sensitively (MEASURED): a stricter reading, never a looser one.
fn root_alias_free(comps: &[String]) -> Option<Vec<String>> {
    let drive = |d: &String, rest: &[String]| Some([d].into_iter().chain(rest).cloned().collect());
    match comps {
        [n, rest @ ..] if same_name(n, ".nofollow") => Some(rest.to_vec()),
        [r] | [r, _] if same_name(r, ".resolve") => Some(Vec::new()),
        [r, _, rest @ ..] if same_name(r, ".resolve") => Some(rest.to_vec()),
        [p, c, d, rest @ ..]
            if same_name(p, "proc") && same_name(c, "cygdrive") && d.len() == 1 =>
        {
            drive(d, rest)
        }
        [c, d, rest @ ..] if same_name(c, "cygdrive") && d.len() == 1 => drive(d, rest),
        [p, pid, t, rest @ ..] if same_name(p, "proc") && same_name(t, "task") => Some(
            [p, pid]
                .into_iter()
                .chain(rest.iter().skip(1))
                .cloned()
                .collect(),
        ),
        [s] | [s, _]
            if same_name(s, "System") && comps.get(1).is_none_or(|v| same_name(v, "Volumes")) =>
        {
            Some(Vec::new())
        }
        [s, v, d, rest @ ..]
            if same_name(s, "System") && same_name(v, "Volumes") && same_name(d, "Data") =>
        {
            Some(rest.to_vec())
        }
        [v] if same_name(v, "Volumes") => Some(Vec::new()),
        [v, _, rest @ ..] if same_name(v, "Volumes") => Some(rest.to_vec()),
        [c] if same_name(c, "cygdrive") => Some(Vec::new()),
        [p] | [p, _] if same_name(p, "proc") => Some(Vec::new()),
        [p, _, r, rest @ ..] if same_name(p, "proc") && same_name(r, "root") => Some(rest.to_vec()),
        [d, docs, rest @ ..] if d.len() == 1 && same_name(docs, "Documents and Settings") => Some(
            [d.clone(), "Users".to_string()]
                .into_iter()
                .chain(rest.iter().cloned())
                .collect(),
        ),
        _ => None,
    }
}

/// Why one of `comps` may name a file by a handle no name rule sees: macOS's volfs
/// (`/.vol/<device>/<inode>/…`, MEASURED 2026-10-29 on macOS 27.0.1 to open the home by its device
/// and inode numbers), or Linux's `/proc/<pid>/cwd`, `fd` or `map_files` (a process's working
/// directory, or a file it holds open; a thread's, `/proc/<pid>/task/<tid>/…`, among the readings
/// [`root_alias_free`] gives).
fn handle_read<'a>(comps: impl IntoIterator<Item = &'a Vec<String>>) -> Option<String> {
    comps.into_iter().find_map(|c| match c.as_slice() {
        [v, ..] if same_name(v, ".vol") => {
            Some("a read by volume and inode number (/.vol), which may name any file".to_string())
        }
        [p, _, k, ..]
            if same_name(p, "proc")
                && ["cwd", "fd", "map_files"].iter().any(|h| same_name(k, h)) =>
        {
            Some(format!(
                "a read through /proc's {k}, which may name any file"
            ))
        }
        _ => None,
    })
}

fn secret_hit<'a>(comps: &[String], secrets: &'a [SecretRule]) -> Option<&'a SecretRule> {
    secrets.iter().find(|rule| match rule {
        SecretRule::Under(dir) => path_readings(&dir.to_string_lossy())
            .iter()
            .any(|d| comps.len() >= d.len() && d.iter().zip(comps).all(|(a, b)| same_name(a, b))),
        SecretRule::Component(name) => comps.iter().any(|c| same_name(c, name)),
        SecretRule::Basename(glob) => comps
            .last()
            .is_some_and(|c| glob_match(&folded(glob), &folded(c))),
    })
}

/// The rule a path a walk below a tree met falls under ([`PolicyProbe::below_tree`]): an
/// `Under` directory it lies in, a `Component` one of its names is, or, for a file (`dir`
/// false), a `Basename` rule its last name matches, each compared as [`secret_hit`] compares it.
/// A directory's name is held to no basename rule: a reader opens the files below it, each of
/// which the walk holds to them.
pub(super) fn walk_hit<'a>(
    comps: &[String],
    secrets: &'a [SecretRule],
    dir: bool,
) -> Option<&'a SecretRule> {
    secrets.iter().find(|rule| match rule {
        SecretRule::Basename(_) if dir => false,
        _ => secret_hit(comps, std::slice::from_ref(*rule)).is_some(),
    })
}

/// `c` case-folded fully, as APFS compares names: lower, upper, then lower again, so `ſ`, the
/// Kelvin sign, `ß`, `ẞ` and the `ﬁ` ligatures fold to the ASCII they open (`.ſsh` is `.ssh`). It
/// also folds `ı` to `i`, which APFS keeps apart: a stricter reading, never a looser one.
fn fold_char(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
}

/// `name` with each character case-folded by [`fold_char`].
fn folded(name: &str) -> String {
    if name.is_ascii() {
        name.to_ascii_lowercase()
    } else {
        name.chars().flat_map(fold_char).collect()
    }
}

/// Whether a case-insensitive volume may open `a` as `b`: equal once both are [`folded`].
fn same_name(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(b)
    } else {
        folded(a) == folded(b)
    }
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

/// The secret check on the words as written: each operand ([`path_operands_except`]) read by
/// [`operand_comps`] and held to the rules by [`secret_hit`]. Only the rm circuit breaker's arm
/// keeps it ([`RULE_RM_BREAKER`] answers in a bypass session alone, where every other command
/// runs unasked); a read rule holds a line to [`secrets_unreached`].
fn secrets_free(
    readings: &[String],
    secrets: &[SecretRule],
    home: Option<&Path>,
) -> Result<(), String> {
    if secrets.is_empty() {
        return Ok(());
    }
    for line in readings {
        let operands = path_operands_except(line, &|_| false)
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

/// Whether a reading of `prompt`'s command reads a path from where its command stands: an operand,
/// or a tree a reader descends, with no root (`~+/…` among them), which [`secrets_unreached`] reads
/// from where the Bash tool stands ([`ApprovalCtx::shell_cwds`]). The loop reads that for such a
/// box alone, so a recorded tape of a box that names none replays without the read. A reading the
/// check cannot split names none, as [`secrets_unreached`] refuses it.
pub(crate) fn reads_from_stand(prompt: &PromptV2) -> bool {
    let from_stand = |w: &str| !rooted(w) && (!w.starts_with('~') || w.starts_with("~+"));
    prompt.readings().iter().any(|line| {
        let operands = path_operands_except(line, &|_| false).unwrap_or_default();
        let segments = command_words(line).unwrap_or_default();
        operands.iter().any(|op| from_stand(op))
            || segments.iter().any(|seg| {
                runs(seg).ok().flatten().is_some_and(|run| {
                    descends(seg, run.at)
                        .0
                        .iter()
                        .any(|tree| from_stand(tree.root))
                })
            })
    })
}

/// Where a line's relative paths start, for [`secrets_unreached`].
#[derive(Clone, Copy)]
enum Start<'a> {
    /// The directories the command may run in: the session's cwd and where its Bash tool stands,
    /// or the directory `aterm drive classify` is asked from.
    Known(&'a [PathBuf]),
    /// The session's cwd is unknown (meta `cwd=-`): a relative path may name any file.
    Unknown,
    /// Unplaced (the advisory word, which knows no session's cwd, and `aterm drive classify` with
    /// no working directory): a relative path is read from a directory this check cannot place
    /// ([`Bases::floating`]) and from the directories the line itself names.
    Unplaced,
}

impl Start<'_> {
    /// Its [`Bases`] before the line moves.
    fn bases(self) -> Bases {
        let (dirs, floating, lost) = match self {
            Self::Known(dirs) => (
                dirs.iter()
                    .map(|d| d.to_string_lossy().into_owned())
                    .collect(),
                if dirs.iter().any(|d| d == Path::new("/")) {
                    vec![String::new()]
                } else {
                    Vec::new()
                },
                None,
            ),
            Self::Unknown => (
                Vec::new(),
                Vec::new(),
                Some("with the session's cwd unknown (meta cwd=-)".to_string()),
            ),
            Self::Unplaced => (Vec::new(), vec![String::new()], None),
        };
        Bases {
            dirs,
            floating,
            lost,
        }
    }
}

/// [`secrets_unreached`] for the session `ctx` describes: its relative paths start in its cwd
/// and where its Bash tool stands, or anywhere when its cwd is unknown; a glob the shell may
/// expand meets a leading dot only by a literal one once its Bash tool's shell startup is read
/// and proven to leave every glob off a leading dot ([`LeadingDot`],
/// [`ShellStartup::may_dot_glob`]); and a tree a reader descends is walked on the disk by the
/// session's probe ([`PolicyProbe::below_tree`]).
fn secrets_unreached_in(
    readings: &[String],
    ctx: &ApprovalCtx,
    passed: &dyn Fn(&[String]) -> bool,
) -> Result<(), String> {
    let dirs = ctx.shell_dirs();
    let start = if ctx.cwd_known {
        Start::Known(&dirs)
    } else {
        Start::Unknown
    };
    let dot = if ctx.shell.as_ref().is_ok_and(|s| !s.may_dot_glob()) {
        LeadingDot::Literal
    } else {
        LeadingDot::Any
    };
    let home = ctx.home.as_deref();
    secrets_unreached(
        readings,
        &ctx.secrets,
        home,
        start,
        passed,
        dot,
        &|root, below, secrets| ctx.probe.below_tree(root, below, secrets, home),
    )
}

/// How a glob meets a name's leading `.`: only by a literal `.`, as bash and zsh glob by default,
/// or also by a `*`, a `?` or a bracket expression, as under bash's `dotglob` or zsh's
/// `GLOB_DOTS`. MEASURED 2026-11-05 with bash 3.2.57 and zsh 5.9 (`/bin/zsh -f`) in a directory
/// holding `a.json` and `.credentials.json` and a home holding `.config/gh/hosts.yml` and
/// `.ssh/config`: `*.json` expanded to `a.json` alone, and `?credentials.json`,
/// `[.]credentials.json`, `~/?ssh/config`, `~/[.]ssh/config` and `~/*/gh/hosts.yml` to nothing
/// (bash printed each as written, zsh found no match), with `EXTENDED_GLOB` on or off and with
/// `GLOBIGNORE=zz` in bash's environment (bash took the variable, `echo $GLOBIGNORE` printing
/// `zz`, and did not set `dotglob` by it, as it does for one the line sets); zsh with `setopt
/// globdots` expanded the first three to `.credentials.json` (`*.json` to it and `a.json`).
///
/// A program may glob a word itself, by rules of its own: git matches a pathspec by wildmatch,
/// where `*`, `?` and a bracket expression meet a leading dot and a `/` too (MEASURED 2026-11-05
/// with git 2.56.0: `git grep --no-index token -- '*.json'`, `'?claude.json'` and
/// `'*.claude.json'`, run in a home under `/bin/bash` and `/bin/zsh -f`, each printed
/// `.claude.json`). So `Literal` holds only a word of a program on [`GLOB_SAFE`], which opens
/// the names the shell expanded, or a word as written, and globs none itself; any other
/// program's operand is met as `Any` ([`secrets_unreached`]): a glob in one may reach it as
/// written (quoted, or matching nothing, which bash hands on as written), and it may glob that
/// itself. A tree a reader descends ([`descends`]) needs no such reading: git grep's pathspecs
/// are read as every file below their literal directories ([`pathspec_tree`]), a pathspec after
/// `git diff --no-index`'s two paths only narrows what it reads below them (MEASURED 2026-11-05
/// with git 2.56.0: `git diff --no-index --stat d1 d2 -- '*.json'` listed `.x.json` alone of
/// `.x.json` and `y.rs`), and every other reader there opens its trees as the shell hands them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LeadingDot {
    /// Only a literal `.`: a word of a [`GLOB_SAFE`] program in the Bash tool's shell, whose
    /// startup [`super::shell_startup::read`] proved free of `dotglob` and `GLOB_DOTS` (an option
    /// outside its inert lists fails the read) and of every line that may set either unseen
    /// ([`ShellStartup::may_dot_glob`]).
    Literal,
    /// Any of them: a shell whose options this check does not know (the advisory word's, and a
    /// startup that may set one unseen), and a word a program off [`GLOB_SAFE`] takes.
    Any,
}

/// `Ok` when no path a reading's commands may read lies under a secret rule, as the shell
/// resolves it: each operand ([`path_operands_except`], the words of a command `passed` holds
/// read as naming no file) read by [`operand_comps`], a relative one also against every
/// directory the line may run it in ([`line_bases`]) and below each it cannot place
/// ([`secret_led`]); a glob held to the rules by what it may expand to ([`secret_reach`], a
/// leading dot met as `dot` says in a word of a program on [`GLOB_SAFE`] alone, and by any
/// glob in a word another program takes, [`path_operands_of`], [`LeadingDot`]); a path git
/// reads from its repository's top ([`git_top_paths`]) read from each directory git may run in
/// and every directory above it; and every tree a reader descends ([`descends`]) held to the
/// home-rooted rules above which it starts, within the levels it reads, and each place it names
/// walked by `list` ([`listed`]), so no file below it, and no link there, reaches any rule.
///
/// # Errors
///
/// The first read of a secret, named; else the first path this check cannot hold to the rules:
/// one the line builds at run time (a shell expansion or substitution, or an awk program's own,
/// [`path_operands_except`]), a relative one after a move it cannot follow, on a line whose
/// commands may stand in more than [`STANDS_MAX`] directories, or with the session's cwd unknown;
/// a tree a reader descends from where this check cannot place it ([`descends`]), or whose walk
/// was cut; a git grep word that names no path, so git reads it as a revision; a git read that
/// may print a file git reads from its object store or index that no word names
/// ([`object_read`]); and a git that reads its pathspecs literally or in any case
/// ([`pathspec_mode`]).
fn secrets_unreached(
    readings: &[String],
    secrets: &[SecretRule],
    home: Option<&Path>,
    start: Start<'_>,
    passed: &dyn Fn(&[String]) -> bool,
    dot: LeadingDot,
    list: Lister<'_>,
) -> Result<(), String> {
    if secrets.is_empty() {
        return Ok(());
    }
    let windows = cfg!(windows) || home.is_some_and(|h| drive_led(&h.to_string_lossy()));
    let mut doubt = None;
    for line in readings {
        let name = reading_name(line);
        let not_read = |why: String| format!("not read-only ({name}): {why}");
        let operands = path_operands_except(line, passed).map_err(not_read)?;
        let theirs = path_operands_of(line, &|p| p.is_none_or(|p| !GLOB_SAFE.contains(&p)))
            .map_err(not_read)?;
        let segments = command_words(line).map_err(not_read)?;
        let bases = line_bases(&segments, start, home);
        for op in &operands {
            let reach = windows_reach(op, home, &bases, true, windows);
            let dot = if theirs.contains(op) {
                LeadingDot::Any
            } else {
                dot
            };
            if let Some(rule) = reach.secret(secrets, 0, dot) {
                return Err(format!("a read of a secret {rule} ({name}): {op}"));
            }
            if doubt.is_none() {
                doubt = reach.doubt().map(|u| format!("{u} ({name}): {op}"));
            }
        }
        let above = Bases {
            dirs: ancestors(&bases.dirs),
            floating: bases.floating.clone(),
            lost: bases.lost.clone(),
        };
        for seg in &segments {
            let Ok(Some(run)) = runs(seg) else {
                continue;
            };
            let prog = program(&seg[run.at]);
            if doubt.is_none() {
                doubt = object_read(seg, run.at)
                    .or_else(|| pathspec_mode(seg, run.at))
                    .map(|u| format!("{u} ({name})"));
            }
            for path in git_top_paths(seg, run.at) {
                let reach = windows_reach(path, home, &above, false, windows);
                if let Some(rule) = reach.secret(secrets, 0, LeadingDot::Any) {
                    return Err(format!(
                        "a read of a secret {rule} ({name}): {path}, which git reads from its \
                         repository's top"
                    ));
                }
                if doubt.is_none() {
                    doubt = reach.doubt().map(|u| format!("{u} ({name}): {path}"));
                }
            }
            let (trees, unplaced) = descends(seg, run.at);
            if doubt.is_none() {
                doubt = unplaced.map(|u| format!("{u} ({name})"));
            }
            for tree in trees {
                let root = tree.root;
                let mut reach = windows_reach(root, home, &bases, false, windows);
                if root != "." && !operands.iter().any(|o| o == root) {
                    // A word the grammar reads as no path (grep's pattern), held to the rules
                    // only where the line may stand.
                    reach.tails.clear();
                }
                if let Some(rule) = reach.secret(secrets, tree.below, dot) {
                    return Err(format!(
                        "a read of a secret {rule} ({name}): {prog} descends from {root}"
                    ));
                }
                let walked = listed(&reach.comps, tree, secrets, list).map_err(|met| {
                    format!("a read of a secret {met} ({name}): {prog} descends from {root}")
                })?;
                let floats = !reach.tails.is_empty();
                if doubt.is_none() {
                    doubt = reach
                        .doubt()
                        .or(walked)
                        .or_else(|| {
                            floats.then(|| {
                                "a tree below a directory this check cannot place, whose files \
                                 it cannot list"
                                    .to_string()
                            })
                        })
                        .map(|u| format!("{u} ({name}): {root}, where {prog} descends"));
                }
            }
        }
    }
    doubt.map_or(Ok(()), Err)
}

/// What `list` finds below each absolute path in `comps`, a reading of `tree`'s word: each walked
/// from its last component before the first that holds a glob (`*`, `?`, `[`), as many levels
/// deeper as the components from there (any, past a whole `**`). `Err` names the first secret a
/// walk met; `Ok` holds why the tree is unproven, if it is: a walk that was cut, a glob followed by
/// `..`, which may climb above where the walk starts, or a [`Tree::revision`] word that names
/// nothing on the disk, which git reads as a revision from its object store.
fn listed(
    comps: &[Vec<String>],
    tree: Tree<'_>,
    secrets: &[SecretRule],
    list: Lister<'_>,
) -> Result<Option<String>, String> {
    let mut walked: Vec<(Vec<String>, usize)> = Vec::new();
    let mut doubt = None;
    for c in comps {
        let lit = c
            .iter()
            .position(|n| n.contains(['*', '?', '[']))
            .unwrap_or(c.len());
        let globbed = &c[lit..];
        if globbed.iter().any(|n| n == "..") {
            doubt.get_or_insert_with(|| {
                "a glob followed by `..`, which may climb above the directory this check walks"
                    .to_string()
            });
            continue;
        }
        let deeper = if globbed.iter().any(|n| matches!(n.as_str(), "**" | "***")) {
            TREE
        } else {
            globbed.len()
        };
        let at = (c[..lit].to_vec(), tree.below.saturating_add(deeper));
        if walked.contains(&at) {
            continue;
        }
        match list(&at.0, at.1, secrets) {
            Ok(Below::Secret(met)) => return Err(met),
            Ok(Below::Missing) if tree.revision => {
                doubt.get_or_insert_with(|| {
                    "a git grep word that names no path, which git then reads as a revision from \
                     its object store, whose files this check cannot list"
                        .to_string()
                });
            }
            Ok(Below::Missing | Below::Clear) => {}
            Err(why) => {
                doubt.get_or_insert(why);
            }
        }
        walked.push(at);
    }
    Ok(doubt)
}

/// Each of `dirs` and every directory above it, each once: where a git repository's top may be
/// for git run in one of them.
fn ancestors(dirs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in dirs {
        let mut p = d.trim_end_matches(['/', '\\']);
        loop {
            let here = if p.is_empty() { "/" } else { p };
            if !out.iter().any(|o| o == here) {
                out.push(here.to_string());
            }
            if p.is_empty() {
                break;
            }
            p = p
                .rfind(['/', '\\'])
                .map_or("", |k| p[..k].trim_end_matches(['/', '\\']));
        }
    }
    out
}

/// The paths the git command at `seg[at]` names from its repository's top, past a revision
/// (`REV:path`, a path led by `./` or `../` excepted, `:path` and `:N:path`) or a pathspec's
/// `top` magic (`:/path`, `:(top)path`), as [`git_tree_path`] reads each of its words that is no
/// option: for [`secrets_unreached`] to read from every directory git may run in and each above
/// it, as the top is one of them. MEASURED 2026-11-05 with git 2.56.0: `git show HEAD:x`, run in
/// a repository's subdirectory, printed the top's `x`.
fn git_top_paths(seg: &[String], at: usize) -> Vec<&str> {
    if program(&seg[at]) != "git" {
        return Vec::new();
    }
    let Some((sub, _)) = git_subcommand(seg, at) else {
        return Vec::new();
    };
    unredirected(seg, sub + 1)
        .into_iter()
        .filter(|w| !w.starts_with('-'))
        .filter_map(git_tree_path)
        .filter(|(path, top)| *top && !path.is_empty())
        .map(|(path, _)| path)
        .collect()
}

/// Why the git command at `seg[at]` may print a file git reads from its object store or index
/// whose path none of its words names, which this check cannot list (MEASURED 2026-11-05 with git
/// 2.56.0 in a repository that tracked `a/.ssh/id_rsa` and `.env`: `git log -p`, `git show`,
/// `git diff HEAD~2`, `git diff`, `git grep token HEAD~1` and `git cat-file -p
/// HEAD~1:a/.ssh/id_rsa` each printed a line of one; so did `git status -v`, `git show --stat
/// <blob>` and `git for-each-ref --format='%(raw)'` over a tag naming a blob): `log` and `stash
/// list` given an option that prints a patch ([`patch_option`]); `show` given an object that is
/// no `REV:path` (a commit's patch, or a blob or tag printed whole), unless a names-only format
/// ([`names_only`]) leaves only commits it names by a parent or ancestor ([`commit_ish`]), or
/// given none without one; `diff` that compares no two paths (`--no-index`) without a names-only
/// format; `status` given `-v` ([`verbose`]); `cat-file` printing an object that is no
/// `REV:path`, or the objects its input names (`--batch`, `--batch-command`, each by any prefix
/// of its name, as git reads `--batch-co`); `for-each-ref`, `tag` and `branch` given a format
/// holding `%(raw)` or `%(*raw)` ([`raw_atom`]); and `grep` reading the index's copies
/// (`--cached`, by any prefix), a revision before its `--`, or, with no `--`, a word that may
/// name one by a `:` (a word that names no path the walk finds, [`Tree::revision`]).
fn object_read(seg: &[String], at: usize) -> Option<String> {
    if program(&seg[at]) != "git" {
        return None;
    }
    let (sub_at, sub) = git_subcommand(seg, at)?;
    let args = unredirected(seg, sub_at + 1);
    let end = args.iter().position(|a| *a == "--").unwrap_or(args.len());
    let opts = &args[..end];
    let bounded =
        |w: &str| !w.starts_with(":/") && git_tree_path(w).is_some_and(|(p, _)| !p.is_empty());
    let untold = "which this check cannot list";
    match sub {
        "log" => patch_option(opts).map(|o| {
            format!("git log {o} prints patches of files the repository has tracked, {untold}")
        }),
        "stash" if opts.first() == Some(&"list") => patch_option(&opts[1..])
            .map(|o| format!("git stash list {o} prints the patches of stashed files, {untold}")),
        "show" => {
            let (objects, _) = git_log_operands(&args);
            let patch = patch_option(opts).is_some() || !names_only(opts);
            match objects
                .iter()
                .find(|w| !bounded(w) && (patch || !commit_ish(w)))
            {
                Some(w) => Some(format!(
                    "git show {w} may print any file the repository has tracked (a commit's \
                     patch, or a blob or tag printed whole), {untold}"
                )),
                None if objects.is_empty() && patch => Some(format!(
                    "git show prints the patch of HEAD's files, {untold}"
                )),
                None => None,
            }
        }
        "status" => opts.iter().find(|a| verbose(a)).map(|o| {
            format!("git status {o} prints the patches of the files the index holds, {untold}")
        }),
        "for-each-ref" | "tag" | "branch" => args.iter().find(|a| raw_atom(a)).map(|a| {
            format!(
                "git {sub} {a} prints whole the object a ref names, which may be a file, {untold}"
            )
        }),
        "diff" if !opts.contains(&"--no-index") => (patch_option(opts).is_some()
            || !names_only(opts))
        .then(|| format!("git diff prints patches of files the repository tracks, {untold}")),
        "cat-file" => {
            if opts.iter().any(|a| {
                a.strip_prefix("--").is_some_and(|l| {
                    let name = l.split('=').next().unwrap_or(l);
                    !name.is_empty() && "batch-command".starts_with(name)
                })
            }) {
                return Some(format!(
                    "git cat-file prints the objects its input names, {untold}"
                ));
            }
            let words: Vec<&str> = opts
                .iter()
                .copied()
                .filter(|a| !a.starts_with('-'))
                .collect();
            let shown = opts
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('p'))
                || words
                    .first()
                    .is_some_and(|w| matches!(*w, "blob" | "tree" | "commit" | "tag"));
            let object = words.last().copied();
            (shown && !object.is_some_and(bounded)).then(|| {
                format!(
                    "git cat-file prints an object by its id, which may be any file the \
                     repository has tracked, {untold}"
                )
            })
        }
        "grep" if !args.contains(&"--no-index") => {
            if let Some(o) = opts.iter().find(|a| {
                a.strip_prefix("--")
                    .is_some_and(|l| !l.is_empty() && "cached".starts_with(l))
            }) {
                return Some(format!(
                    "git grep {o} reads the index's copies of the files the repository tracks, \
                     {untold}"
                ));
            }
            let (operands, past) = git_grep_operands(&args);
            let revision = match past {
                Some(_) => operands.first(),
                None => operands.iter().find(|w| w.contains(':')),
            };
            revision.map(|w| {
                format!(
                    "git grep reads {w} as a revision from its object store, whose files this \
                     check cannot list"
                )
            })
        }
        _ => None,
    }
}

/// Why git at `seg[at]` reads its pathspecs otherwise than [`descends`] and [`git_tree_path`]
/// read them, by a global option before its subcommand: `--literal-pathspecs`, under which a
/// word's magic is part of the name it reads (`:(icase)src` a file of that name), and
/// `--icase-pathspecs`, under which `src` also reads a `SRC` a case-sensitive volume holds apart.
fn pathspec_mode(seg: &[String], at: usize) -> Option<String> {
    if program(&seg[at]) != "git" {
        return None;
    }
    let end = git_subcommand(seg, at).map_or(seg.len(), |(sub, _)| sub);
    seg[at + 1..end]
        .iter()
        .find(|w| matches!(w.as_str(), "--literal-pathspecs" | "--icase-pathspecs"))
        .map(|w| format!("git {w} reads every pathspec otherwise than this check reads it"))
}

/// Whether the object a git word `w` names is a commit by its words alone: `HEAD`, `@`, or a
/// parent or ancestor (`…^`, `…^N`, `…~`, `…~N`) or a commit peel (`…^{commit}`), which git resolves
/// to a commit or refuses (MEASURED 2026-11-05 with git 2.56.0: `git update-ref` refused to point
/// `HEAD` or a branch at a blob, as `trying to write non-commit object`).
fn commit_ish(w: &str) -> bool {
    let base = w.trim_end_matches(|c: char| c.is_ascii_digit());
    matches!(w, "HEAD" | "@")
        || w.ends_with("^{commit}")
        || base
            .strip_suffix(['~', '^'])
            .is_some_and(|rev| !rev.is_empty())
}

/// Whether git status's word `a` asks for its verbose form (`-v` in a cluster, `--verbose` by any
/// prefix of its name), which prints the patch of what the index holds, and of the worktree too
/// when given twice (MEASURED 2026-11-05 with git 2.56.0: `git status -v` and `git status --verb`
/// printed the staged lines of `.env`).
fn verbose(a: &str) -> bool {
    match a.strip_prefix("--") {
        Some(long) => {
            let name = long.split('=').next().unwrap_or(long);
            !name.is_empty() && "verbose".starts_with(name)
        }
        None => a.strip_prefix('-').is_some_and(|c| c.contains('v')),
    }
}

/// Whether a word `a` holds the `raw` or `*raw` atom of a ref format whole (`%(raw:size)`, its
/// size alone, excepted), which prints the object a ref names as it is stored, a blob a tag names
/// among them (MEASURED 2026-11-05 with git 2.56.0: `git for-each-ref --format='%(raw)'` over a
/// tag naming a blob printed its lines, `%(*raw)` over an annotated one too; `%(RAW)` is no atom).
fn raw_atom(a: &str) -> bool {
    let rest = a.replace("%(raw:size)", "").replace("%(*raw:size)", "");
    rest.contains("%(raw") || rest.contains("%(*raw")
}

/// The first of git's words `opts` (before any `--`) that prints a patch, which holds each file's
/// lines: `-p`, `-u`, `-W`, `-c`, `-m` and `-U`, `-L` in a cluster (MEASURED 2026-11-05 with git
/// 2.56.0: `git log -pn1` read `-p`, then refused `-n1`), until a letter that takes a value
/// (`-S`, `-G`, `-O`, `-l`, `-B`, `-M`, `-C`, `-X`, `-n`), and the long options that print one or
/// imply one, each by its whole name (`git log --pat` and `git diff --pat` were refused as
/// unrecognized, MEASURED the same day), `--check` among them, which prints each line it flags.
fn patch_option<'a>(opts: &[&'a str]) -> Option<&'a str> {
    let short = |a: &str| {
        a.strip_prefix('-')
            .filter(|c| !c.starts_with('-') && !c.is_empty())
            .is_some_and(|cluster| {
                for c in cluster.chars() {
                    match c {
                        'p' | 'u' | 'W' | 'c' | 'm' | 'U' | 'L' => return true,
                        'S' | 'G' | 'O' | 'l' | 'B' | 'M' | 'C' | 'X' | 'n' => return false,
                        _ => {}
                    }
                }
                false
            })
    };
    opts.iter().copied().find(|a| {
        short(a)
            || matches!(
                *a,
                "--cc"
                    | "--dd"
                    | "--patch"
                    | "--patch-with-stat"
                    | "--patch-with-raw"
                    | "--function-context"
                    | "--binary"
                    | "--remerge-diff"
                    | "--combined-all-paths"
                    | "--check"
                    | "--color-words"
                    | "--word-diff"
                    | "--full-diff"
            )
            || [
                "--unified",
                "--word-diff=",
                "--word-diff-regex",
                "--color-words=",
                "--diff-merges",
                "--submodule",
            ]
            .iter()
            .any(|p| a.starts_with(p))
    })
}

/// Whether git's words `opts` (before any `--`) give a format that prints no file's lines
/// (`--stat`, `--numstat`, `--shortstat`, `--dirstat`, `--summary`, `--name-only`,
/// `--name-status`, `--raw`, `--quiet`, `-s`, `--no-patch`): with no [`patch_option`] among them,
/// `git diff` and `git show` print no patch (MEASURED 2026-11-05 with git 2.56.0: `git diff
/// --summary` over a modified file printed nothing, `git diff --stat` its name and counts).
fn names_only(opts: &[&str]) -> bool {
    opts.iter().any(|a| {
        matches!(
            *a,
            "--stat"
                | "--numstat"
                | "--shortstat"
                | "--dirstat"
                | "--summary"
                | "--name-only"
                | "--name-status"
                | "--raw"
                | "--quiet"
                | "-s"
                | "--no-patch"
        ) || a.starts_with("--stat=")
            || a.starts_with("--dirstat=")
    })
}

/// The directories a line's commands may run in, as written, and the first move after which
/// a relative path may name any file ([`Start::Unknown`]'s, or a move this check cannot follow).
struct Bases {
    dirs: Vec<String>,
    /// Directories named from one this check cannot place, each as a path from it (`""` for that
    /// directory itself): the advisory word's start ([`Start::Unplaced`]), and a `cd` or `pushd`
    /// target bash and zsh may look up in a `CDPATH` (zsh's `cdpath`), which the worker's
    /// environment or a `.zshenv` may set ([`cdpath_searched`]), with every relative move after
    /// either. That directory is taken to lie outside every secret: a read below it is held to
    /// the rules by [`secret_led`].
    floating: Vec<String>,
    lost: Option<String>,
}

/// The most directories [`line_bases`] follows a line's commands into: each relative target is
/// joined to every directory before it, so a line of distinct ones doubles them.
const STANDS_MAX: usize = 64;

/// [`Bases`] of a line of `segments`: `start`'s, and every `cd` target and `git -C` directory
/// the line names, a relative one joined to every directory before it (a floating one's too), in
/// any order the line may run them, and a `cd` or `pushd` target `CDPATH` may place also
/// floating. A move it cannot follow: `cd -`, a directory-stack entry, `popd`, a target
/// built at run time or globbed, another user's home, a second operand (zsh's `cd old new`
/// edits `PWD`), a relative target a loop may repeat, or one that takes the line past
/// [`STANDS_MAX`] directories.
fn line_bases(segments: &[Vec<String>], start: Start<'_>, home: Option<&Path>) -> Bases {
    let mut b = start.bases();
    let looped = segments.iter().any(|seg| {
        seg.iter()
            .find(|w| w.as_str() != "!")
            .is_some_and(|w| matches!(w.as_str(), "for" | "while" | "until" | "select"))
    });
    for seg in segments {
        let Ok(Some(run)) = runs(seg) else {
            continue;
        };
        let at = run.at;
        let cd = matches!(program(&seg[at]), "cd" | "pushd");
        let mut moves: Vec<Result<Option<&str>, ()>> =
            run.chdir.iter().map(|&d| Ok(Some(d))).collect();
        match program(&seg[at]) {
            "cd" | "pushd" => moves.push(cd_operand(&seg[at + 1..])),
            "popd" => moves.push(Err(())),
            "git" => moves.extend(git_dirs(seg, at).into_iter().map(|d| d.map(Some))),
            _ => {}
        }
        if moves.is_empty() {
            continue;
        }
        for m in moves {
            let moved = match m {
                Ok(None) => home
                    .map(|h| vec![h.to_string_lossy().into_owned()])
                    .ok_or_else(|| "`cd` with no home".to_string()),
                Ok(Some(t)) => cd_moves(t, &b.dirs, home, looped),
                Err(()) => Err(format!(
                    "`{}`, a move this check cannot follow",
                    seg[at..].join(" ")
                )),
            };
            match moved {
                Ok(dirs) => b.dirs.extend(dirs),
                Err(why) => {
                    b.lost.get_or_insert(format!("after {why}"));
                }
            }
            if let Ok(Some(t)) = m
                && !rooted(t)
                && !t.starts_with('~')
            {
                let looked_up = (cd && cdpath_searched(t)).then(|| t.to_string());
                let floated: Vec<String> = b.floating.iter().map(|f| joined(f, t)).collect();
                b.floating.extend(floated.into_iter().chain(looked_up));
            }
            distinct(&mut b.dirs);
            distinct(&mut b.floating);
            if b.dirs.len() + b.floating.len() > STANDS_MAX {
                b.lost.get_or_insert(format!(
                    "on a line whose commands may stand in more than {STANDS_MAX} directories"
                ));
                // Every relative path is unproven now: reading it from each directory would
                // only name it otherwise.
                b.dirs.clear();
                b.floating.clear();
                return b;
            }
        }
    }
    b
}

/// `v` without the repeats of a directory it holds, in order.
fn distinct(v: &mut Vec<String>) {
    let mut k = 0;
    while k < v.len() {
        if v[..k].contains(&v[k]) {
            v.remove(k);
        } else {
            k += 1;
        }
    }
}

/// `rel` read from `dir`, or alone from `""` ([`Bases::floating`]'s directory it cannot place).
fn joined(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{}/{rel}", dir.trim_end_matches('/'))
    }
}

/// The directories git's `-C` options before its subcommand name, in order.
fn git_dirs(seg: &[String], at: usize) -> Vec<Result<&str, ()>> {
    let mut out = Vec::new();
    let mut j = at + 1;
    while let Some(t) = seg.get(j).map(String::as_str) {
        if t == "--" {
            break;
        }
        if let Some(glued) = t.strip_prefix("-C") {
            if glued.is_empty() {
                out.push(seg.get(j + 1).map(String::as_str).ok_or(()));
                j += 2;
            } else {
                out.push(Ok(glued));
                j += 1;
            }
        } else if matches!(
            t,
            "-c" | "--namespace" | "--super-prefix" | "--attr-source" | "--git-dir" | "--work-tree"
        ) {
            j += 2;
        } else if t.starts_with('-') {
            j += 1;
        } else {
            break;
        }
    }
    out
}

/// Whether `t` names a path from the root: `/…`, `\…`, or a drive path (`C:\…`, `C:/…`).
fn rooted(t: &str) -> bool {
    let b = t.as_bytes();
    t.starts_with(['/', '\\'])
        || (b.len() > 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'/' | b'\\'))
}

/// The directories `cd t` may move to from any of `dirs`; `Err` naming a target this check
/// cannot follow ([`line_bases`]).
fn cd_moves(
    t: &str,
    dirs: &[String],
    home: Option<&Path>,
    looped: bool,
) -> Result<Vec<String>, String> {
    if t.contains('$') || t.contains(['*', '?', '[']) {
        return Err(format!(
            "`cd {t}`, a directory the shell expands at run time"
        ));
    }
    if let Some(rest) = t.strip_prefix('~') {
        let home = home.filter(|_| rest.is_empty() || rest.starts_with('/'));
        let Some(h) = home else {
            return Err(format!("`cd {t}`, a home this check does not know"));
        };
        return Ok(vec![format!(
            "{}{rest}",
            h.to_string_lossy().trim_end_matches('/')
        )]);
    }
    if rooted(t) {
        return Ok(vec![t.to_string()]);
    }
    if looped {
        return Err(format!("`cd {t}`, a relative move a loop may repeat"));
    }
    Ok(dirs.iter().map(|d| joined(d, t)).collect())
}

/// What an operand may name, for [`secrets_unreached`] ([`operand_reach`]).
struct Reach {
    /// Every component list it may name.
    comps: Vec<Vec<String>>,
    /// What it names below a directory this check cannot place ([`Bases::floating`]), past that
    /// directory ([`lexical_tails`]).
    tails: Vec<Vec<String>>,
    /// Why it may name another file this check cannot see.
    unsure: Option<String>,
}

impl Reach {
    /// The rule it may fall under, read `below` levels down ([`secret_reach`] over its
    /// components, [`secret_led`] over its tails, each glob meeting a leading dot as `dot`
    /// says), named.
    fn secret(&self, secrets: &[SecretRule], below: usize, dot: LeadingDot) -> Option<String> {
        if let Some(rule) = self
            .comps
            .iter()
            .find_map(|c| secret_reach(c, secrets, below, dot))
        {
            return Some(format!("({rule:?})"));
        }
        self.tails
            .iter()
            .find_map(|t| secret_led(t, secrets, below, dot))
            .map(|rule| format!("({rule:?}) from a directory this check cannot place"))
    }

    /// Why it may name a file no rule sees: its `unsure`, else a read by a handle
    /// ([`handle_read`]) among its components or its tails.
    fn doubt(self) -> Option<String> {
        self.unsure
            .or_else(|| handle_read(self.comps.iter().chain(&self.tails)))
    }
}

/// Every component list `op` may name: [`operand_comps`]' readings (a relative `op`'s, its words
/// as written, only with `literal`), `~user/…` read under the home when `user` is its owner's
/// name, and a relative `op` (and `~+/…`) joined to each of `bases`' directories and read below
/// each it floats (and from the root, where an alias of the root leads what it names below one,
/// [`below_aliases`]); and why it may name another file this check cannot see (a path the line
/// builds at run time, `~-`, a home with none known to resolve it, or a relative one after
/// `bases`' lost move).
fn operand_reach(op: &str, home: Option<&Path>, bases: &Bases, literal: bool) -> Reach {
    let only = |comps: Vec<Vec<String>>, unsure: Option<String>| Reach {
        comps,
        tails: Vec::new(),
        unsure,
    };
    // A `~` (a user's home) or an expansion leading the word names the home or a place the
    // environment sets; with no home known, the home's own rules are not in the list to hold it
    // to, so the read is unproven rather than approved.
    if home.is_none() && (op.starts_with('$') || (op.starts_with('~') && !op.starts_with("~+"))) {
        return only(
            operand_comps(op, home),
            Some("a read under a home this check knows none of, which may name a secret".into()),
        );
    }
    let as_written = literal || rooted(op) || op.starts_with(['~', '$']);
    let mut comps = if as_written {
        operand_comps(op, home)
    } else {
        Vec::new()
    };
    if op.contains(SUBSTITUTION) || op.contains(QUOTED_SUBSTITUTION) {
        return only(
            comps,
            Some("a read of a path the line builds at run time, which may name a secret".into()),
        );
    }
    let mut relative = (!rooted(op) && !op.starts_with('~')).then_some(op);
    if let Some(rest) = op
        .strip_prefix('~')
        .filter(|r| !r.is_empty() && !r.starts_with('/'))
    {
        let (user, tail) = rest.split_once('/').unwrap_or((rest, ""));
        match user {
            "+" => relative = Some(tail),
            "-" => {
                return only(
                    comps,
                    Some("a read under `~-` (OLDPWD), which this check cannot resolve".into()),
                );
            }
            _ => {
                let owner = home.and_then(Path::file_name).map(|n| n.to_string_lossy());
                if let Some(h) =
                    home.filter(|_| owner.is_some_and(|o| o.eq_ignore_ascii_case(user)))
                {
                    let h = h.to_string_lossy();
                    comps.extend(path_readings(&format!(
                        "{}/{tail}",
                        h.trim_end_matches('/')
                    )));
                }
            }
        }
    }
    let Some(rel) = relative else {
        return only(comps, None);
    };
    for d in &bases.dirs {
        comps.extend(path_readings(&joined(d, rel)));
    }
    let tails: Vec<Vec<String>> = bases
        .floating
        .iter()
        .flat_map(|f| lexical_tails(&joined(f, rel)))
        .collect();
    // A directory it cannot place may be the root: a tail an alias of the root leads is also
    // read from there.
    comps.extend(tails.iter().flat_map(|t| below_aliases(t)));
    let unsure = bases
        .lost
        .as_ref()
        .map(|lost| format!("a read of a relative path {lost}"));
    Reach {
        comps,
        tails,
        unsure,
    }
}

/// The components `rel`, a path from a directory this check cannot place, names below that
/// directory or one above it: split at `/` (and at `\` too, where it holds one), each `..`
/// taking the component before it, or none.
fn lexical_tails(rel: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for seps in [&['/'][..], &['/', '\\']] {
        if seps.len() == 2 && !rel.contains('\\') {
            break;
        }
        let mut tail: Vec<String> = Vec::new();
        for c in rel.split(seps).filter(|c| !c.is_empty() && *c != ".") {
            if c == ".." {
                tail.pop();
            } else {
                tail.push(c.to_string());
            }
        }
        out.push(tail);
    }
    out
}

/// The rule `tail`, a path below a directory this check cannot place ([`lexical_tails`]), may fall
/// under, read `below` levels down. That directory is taken to lie outside every secret, so this
/// is a [`SecretRule::Under`] rule a trailing part of whose path leads `tail` (`.claude.json`,
/// `../.claude.json`, and `gh/hosts.yml` read from above `~/.config`), or a component or basename
/// rule as [`secret_reach`] holds it.
fn secret_led<'a>(
    tail: &[String],
    secrets: &'a [SecretRule],
    below: usize,
    dot: LeadingDot,
) -> Option<&'a SecretRule> {
    secrets.iter().find(|rule| match rule {
        SecretRule::Under(dir) => path_readings(&dir.to_string_lossy())
            .iter()
            .any(|d| (0..d.len()).any(|j| reaches(tail, &d[j..], below, dot))),
        _ => secret_reach(tail, std::slice::from_ref(*rule), below, dot).is_some(),
    })
}

/// [`operand_reach`], on a Windows host (`windows`: built for Windows, or a drive home) with
/// Windows' own spellings of each name ([`windows_spellings`]); a drive-relative `op` (`C:name`)
/// read as relative from each directory the line may stand in and from its drive's own working
/// directory, which this check cannot place (the drive's root among them, read as
/// [`path_readings`] reads `C:/name`); and a UNC or device path ([`unc`]), as an operand or
/// a directory a relative one is read from, unproven.
fn windows_reach(
    op: &str,
    home: Option<&Path>,
    bases: &Bases,
    literal: bool,
    windows: bool,
) -> Reach {
    if !windows {
        return operand_reach(op, home, bases, literal);
    }
    let relative = !rooted(op) && (!op.starts_with('~') || op.starts_with("~+"));
    if unc(op) || relative && bases.dirs.iter().any(|d| unc(d)) {
        return Reach {
            comps: Vec::new(),
            tails: Vec::new(),
            unsure: Some(
                "a read of a UNC or device path, which may name this machine's disk under another \
                 root"
                    .into(),
            ),
        };
    }
    let mut reach = match op.get(2..).filter(|_| drive_led(op) && !rooted(op)) {
        Some(rest) => {
            let mut reach = operand_reach(rest, home, bases, false);
            if literal {
                reach.comps.extend(operand_comps(op, home));
            }
            reach.tails.extend(lexical_tails(rest));
            // That directory may be the drive's root, read through its aliases too.
            reach
                .comps
                .extend(path_readings(&format!("{}/{rest}", &op[..2])));
            reach
        }
        None => operand_reach(op, home, bases, literal),
    };
    let (comps, short) = windows_spellings(std::mem::take(&mut reach.comps));
    let (tails, tail_short) = windows_spellings(std::mem::take(&mut reach.tails));
    reach.comps = comps;
    reach.tails = tails;
    if reach.unsure.is_none() {
        reach.unsure = short.or(tail_short);
    }
    reach
}

/// Whether `t` is led by a drive (`C:`), rooted (`C:\…`) or relative to the drive's own working
/// directory (`C:name`).
fn drive_led(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Whether `op` is a UNC or device path (`\\host\share`, `\\?\C:\…`, `//./C:/…`, and the NT
/// namespace's `\??\C:\…`, which Git Bash's `cat` opens, MEASURED 2026-10-29 in the Windows lane),
/// on Windows a spelling of this machine's disk (`\\localhost\C$`) under another root.
fn unc(op: &str) -> bool {
    let sep = |b: u8| matches!(b, b'/' | b'\\');
    match op.as_bytes() {
        [a, b, ..] if sep(*a) && sep(*b) => true,
        [a, b'?', b'?', b, ..] => sep(*a) && sep(*b),
        _ => false,
    }
}

/// Leads a component [`windows_spellings`] reads as the basis of a short 8.3 name (`\0SSH`, of
/// `SSH~1`), which [`component_meets`] and [`basename_meets`] hold to every long name Windows may
/// have made that basis of ([`short_meets`]). No path component holds a NUL.
const SHORT: char = '\0';

/// `lists` and Windows' own spellings of each: each name without its `:stream` suffix and its
/// trailing dots and spaces, and a short 8.3 name whose stem holds a whole basis (`SSH~1`, of
/// `.ssh`; `ALWU~1`, of `Al Wu`) read as that basis led by [`SHORT`]; and why a name may stand for
/// one this check cannot see: an 8.3 name its stem filled to eight characters (cut to fit, or
/// hashed) or with an extension, which Windows may resolve to any long name.
fn windows_spellings(lists: Vec<Vec<String>>) -> (Vec<Vec<String>>, Option<String>) {
    let mut out = Vec::with_capacity(lists.len() * 2);
    let mut doubt = None;
    for comps in lists {
        let mut bare = Vec::with_capacity(comps.len());
        for c in &comps {
            let name = c.split(':').next().unwrap_or(c);
            let name = match name.trim_end_matches(['.', ' ']) {
                "" => name,
                trimmed => trimmed,
            };
            match short_name(name) {
                Some(Some(basis)) => bare.push(format!("{SHORT}{basis}")),
                cut => {
                    if cut.is_some() {
                        doubt.get_or_insert_with(|| {
                            format!(
                                "a read of an 8.3 short name ({name}), which Windows may resolve \
                                 to any long name"
                            )
                        });
                    }
                    bare.push(name.to_string());
                }
            }
        }
        if bare != comps {
            out.push(bare);
        }
        out.push(comps);
    }
    (out, doubt)
}

/// Whether `name` has a short 8.3 name's shape (`STEM~N`, `STEM~N.EXT`, in characters):
/// `Some(Some(basis))` when its stem holds the whole basis Windows made it of and it has no
/// extension (`SSH~1`, `ALWU~1`: under eight bytes, which bound its OEM code page's, and not the
/// one character and four hex digits a hashed name keeps of a one-character basis), `Some(None)`
/// when it may stand for any long name (a stem filled to eight, cut or hashed, or an extension),
/// else `None`. REASONED from FAT's and NTFS's short-name rules.
fn short_name(name: &str) -> Option<Option<&str>> {
    let (stem, ext) = match name.split_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (name, None),
    };
    let (lead, n) = stem.rsplit_once('~')?;
    let shaped = !lead.is_empty()
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
        && stem.chars().count() <= 8
        && ext.is_none_or(|e| !e.is_empty() && e.chars().count() <= 3 && !e.contains('.'));
    let hashed = lead.chars().count() == 5 && lead.chars().skip(1).all(|c| c.is_ascii_hexdigit());
    shaped.then(|| (stem.len() < 8 && ext.is_none() && !hashed).then_some(lead))
}

/// Whether a short 8.3 name whose stem holds `basis` whole ([`short_name`]) may stand for a long
/// name `name` (a rule's name, or a basename rule's glob, its `*` and `?` as [`glob_match`] reads
/// them). Windows makes a long name's basis by dropping every space and dot (its leading ones
/// among them), putting `_` for each of `+,;=[]` and for a character its OEM code page lacks, and
/// upper-casing it (REASONED from FAT's and NTFS's short-name rules; MEASURED 2026-10-30 in the
/// Windows lane, `ALWU~1` of `Al Wu` and `A_B~1` of `a+b`): so a character of `name` Windows may
/// replace, and a non-ASCII one of `basis`, is any one, and a `?` of the glob, which may match a
/// dropped space or dot, any run.
fn short_meets(basis: &str, name: &str) -> bool {
    let tok = |c: char| {
        if c.is_ascii() {
            Tok::Lit(c.to_ascii_lowercase())
        } else {
            Tok::One
        }
    };
    let basis: Vec<Tok> = basis.chars().map(tok).collect();
    let long: Vec<Tok> = name
        .chars()
        .filter(|c| !matches!(c, ' ' | '.'))
        .map(|c| match c {
            '*' | '?' => Tok::Star,
            '+' | ',' | ';' | '=' | '[' | ']' => Tok::One,
            c => tok(c),
        })
        .collect();
    tokens_meet(&basis, &long)
}

/// The rule a path `comps` names may fall under, its components globs the shell may expand: an
/// [`SecretRule::Under`] directory some expansion names or lies under, a [`SecretRule::Component`]
/// some component may expand to, or a [`SecretRule::Basename`] the last component may expand to
/// a name of ([`basename_meets`]). With `below` above zero (a tree a reader descends, whose root
/// the operand check holds to every rule), the `Under` rules alone, a directory up to `below`
/// levels above one reaching it too.
fn secret_reach<'a>(
    comps: &[String],
    secrets: &'a [SecretRule],
    below: usize,
    dot: LeadingDot,
) -> Option<&'a SecretRule> {
    secrets.iter().find(|rule| match rule {
        SecretRule::Under(dir) => path_readings(&dir.to_string_lossy())
            .iter()
            .any(|d| reaches(comps, d, below, dot)),
        SecretRule::Component(_) | SecretRule::Basename(_) if below > 0 => false,
        SecretRule::Component(name) => comps.iter().any(|c| component_meets(c, name, dot)),
        SecretRule::Basename(glob) => comps.last().is_some_and(|c| basename_meets(c, glob, dot)),
    })
}

/// Whether some expansion of `comps` names `dir` or a path under it, or a directory at most
/// `below` levels above it: each component compared by [`component_meets`], a whole `**` (zsh's,
/// and bash's under `globstar`) taking any run of components.
fn reaches(comps: &[String], dir: &[String], below: usize, dot: LeadingDot) -> bool {
    let (n, m) = (comps.len(), dir.len());
    let mut ok = vec![vec![false; m + 1]; n + 1];
    for i in (0..=n).rev() {
        for j in (0..=m).rev() {
            ok[i][j] = if j == m {
                true
            } else if i == n {
                m - j <= below
            } else if matches!(comps[i].as_str(), "**" | "***") {
                ok[i + 1][j] || ok[i][j + 1]
            } else {
                component_meets(&comps[i], &dir[j], dot) && ok[i + 1][j + 1]
            };
        }
    }
    ok[0][0]
}

/// One character of a glob, for [`glob_tokens`] and [`tokens_meet`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tok {
    /// `*`: any run of characters.
    Star,
    /// `?` or a bracket expression: any one character.
    One,
    /// The character itself, case-folded ([`fold_char`]).
    Lit(char),
}

/// The tokens the shell globs one path component `pattern` by, ignoring case as the default
/// volume does: `*`, `?`, a bracket expression ([`bracket_end`]) any one character, every other
/// character itself; `None` for a component holding an extended-glob group (`(…|…)`), which may
/// expand to any name.
fn glob_tokens(pattern: &str) -> Option<Vec<Tok>> {
    if pattern.contains(['(', '|']) {
        return None;
    }
    let cs: Vec<char> = pattern.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        match (cs[i], bracket_end(&cs, i)) {
            ('*', _) => toks.push(Tok::Star),
            ('?', _) => toks.push(Tok::One),
            ('[', Some(end)) => {
                toks.push(Tok::One);
                i = end;
            }
            (c, _) if c.is_ascii() => toks.push(Tok::Lit(c.to_ascii_lowercase())),
            (c, _) => toks.extend(fold_char(c).map(Tok::Lit)),
        }
        i += 1;
    }
    Some(toks)
}

/// Where the bracket expression opening at `cs[open]` closes, as bash and zsh read one: a `!` or
/// `^` first negates, a `]` first (after it) is a member, and a POSIX class (`[:alpha:]`), an
/// equivalence class (`[=s=]`) or a collating element (`[.s.]`) inside is read whole, its `]`
/// none; `None` when `cs[open]` is no `[` or the expression never closes (the `[` is then
/// itself).
fn bracket_end(cs: &[char], open: usize) -> Option<usize> {
    if cs.get(open) != Some(&'[') {
        return None;
    }
    let mut j = open + 1;
    if matches!(cs.get(j), Some('!' | '^')) {
        j += 1;
    }
    if cs.get(j) == Some(&']') {
        j += 1;
    }
    while j < cs.len() {
        match cs[j] {
            ']' => return Some(j),
            '[' if matches!(cs.get(j + 1), Some(':' | '=' | '.')) => {
                let d = cs[j + 1];
                j = (j + 2..cs.len().saturating_sub(1))
                    .find(|&k| cs[k] == d && cs[k + 1] == ']')
                    .map_or(j + 1, |k| k + 2);
            }
            _ => j += 1,
        }
    }
    None
}

/// Whether some name matches both token runs `a` and `b`.
fn tokens_meet(a: &[Tok], b: &[Tok]) -> bool {
    let (n, m) = (a.len(), b.len());
    let mut ok = vec![vec![false; m + 1]; n + 1];
    for i in (0..=n).rev() {
        for j in (0..=m).rev() {
            ok[i][j] = match (a.get(i), b.get(j)) {
                (None, None) => true,
                (Some(Tok::Star), _) => ok[i + 1][j] || (j < m && ok[i][j + 1]),
                (_, Some(Tok::Star)) => ok[i][j + 1] || (i < n && ok[i + 1][j]),
                (Some(x), Some(y)) => {
                    let one = matches!((x, y), (Tok::One, _) | (_, Tok::One)) || x == y;
                    one && ok[i + 1][j + 1]
                }
                _ => false,
            };
        }
    }
    ok[0][0]
}

/// Whether `toks`, a glob's tokens ([`glob_tokens`]), may meet a name that `dotted` says opens
/// with `.`: always under [`LeadingDot::Any`], and under [`LeadingDot::Literal`] only by a
/// literal `.` first.
fn dot_met(toks: &[Tok], dotted: bool, dot: LeadingDot) -> bool {
    dot == LeadingDot::Any || !dotted || toks.first() == Some(&Tok::Lit('.'))
}

/// Whether the shell may expand `pattern`, one path component of a word, to `name`, ignoring
/// case as the default volume does: `*` any run of characters, `?` and a bracket expression any
/// one, a leading dot of `name` met as `dot` says ([`dot_met`]), and a component holding an
/// extended-glob group (`(…|…)`) any name ([`glob_tokens`]); a short 8.3 name's basis
/// ([`SHORT`]) also any long name Windows may have made it of ([`short_meets`]).
fn component_meets(pattern: &str, name: &str, dot: LeadingDot) -> bool {
    if let Some(basis) = pattern.strip_prefix(SHORT)
        && short_meets(basis, name)
    {
        return true;
    }
    if !pattern.contains(['*', '?', '[', '(', '|']) {
        return same_name(pattern, name);
    }
    let dotted = name.starts_with('.');
    let name: Vec<Tok> = folded(name).chars().map(Tok::Lit).collect();
    glob_tokens(pattern).is_none_or(|toks| dot_met(&toks, dotted, dot) && tokens_meet(&toks, &name))
}

/// Whether `c`, a path's last component and a glob the shell may expand, may name a file the
/// basename rule `glob` (its `*` and `?` as [`glob_match`] reads them) matches: `glob` matched
/// against `c` as written, or some expansion of `c` ([`glob_tokens`], a rule's leading dot met
/// as `dot` says, [`dot_met`]) matching `glob` when one of the two holds no `*`: a literal rule's
/// every spelling (`.netr?`, `.npmr[c]`, `.pypi*`, and under [`LeadingDot::Any`] `*.json` for
/// `.credentials.json`) and a `*`-free glob's (`a.pe?` for `*.pem`). A `*` in both is held as
/// written, so `cat src/*.rs` stays a read, though `id_rsa*` names `id_rsa.rs`. A short 8.3
/// name's basis ([`SHORT`]) also meets a rule some long name Windows may have made it of matches
/// ([`short_meets`]).
fn basename_meets(c: &str, glob: &str, dot: LeadingDot) -> bool {
    if let Some(basis) = c.strip_prefix(SHORT)
        && short_meets(basis, glob)
    {
        return true;
    }
    if glob_match(&folded(glob), &folded(c)) {
        return true;
    }
    let rule: Vec<Tok> = folded(glob)
        .chars()
        .map(|ch| match ch {
            '*' => Tok::Star,
            '?' => Tok::One,
            ch => Tok::Lit(ch),
        })
        .collect();
    let dotted = rule.first() == Some(&Tok::Lit('.'));
    glob_tokens(c).is_none_or(|toks| {
        !(toks.contains(&Tok::Star) && rule.contains(&Tok::Star))
            && dot_met(&toks, dotted, dot)
            && tokens_meet(&toks, &rule)
    })
}

/// A tree [`descends`] names: the word, how many levels below it the reader reads ([`TREE`] for
/// any), and whether the word names a revision when nothing by its name is on the disk (a word
/// past git grep's pattern on a line with no `--` that may spell one, [`may_name_revision`]).
/// MEASURED 2026-11-05 with git 2.56.0: `git grep token src2`, `src2` a branch and no path,
/// printed the branch's `a/.ssh/id_rsa`; with a folder `src2` beside the branch git refused the
/// word as both, and with neither as unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tree<'a> {
    root: &'a str,
    below: usize,
    revision: bool,
}

/// Whether a git grep word `w` may name a revision: one holding no glob character (`*`, `?`, `[`,
/// `\`), none of which a ref's name may hold, or one holding a peel or reflog suffix (`^{`, `@{`),
/// whose braces may hold any text (MEASURED 2026-11-05 with git 2.56.0: `git grep token
/// 'HEAD^{/tw*}'` grepped the commit whose message holds `tw`; `git grep token 'zz*'`, no path
/// matching, printed nothing and succeeded).
fn may_name_revision(w: &str) -> bool {
    !w.contains(['*', '?', '[', '\\']) || w.contains("^{") || w.contains("@{")
}

/// The trees the command at `seg[at]` reads files below, as written ([`Tree`]): for `grep -r` (and
/// `-R`, `--recursive`, `-d recurse`), `rg`, and `find` that runs `-exec`, `-execdir`, `-ok` or
/// `-okdir`, every word after the program that is no option (its operands among them), and `.`
/// when it names no tree to start from (grep's and rg's operands past the pattern, by
/// [`grammar_names_operand`]; find's paths before its expression, [`find_names_a_path`]); for
/// `git grep`, its pathspecs ([`pathspec_tree`]: every word past its `--`, a `-` led one among
/// them, and with no `--` every operand past its pattern, [`git_grep_operands`], each a
/// [`Tree::revision`] unless `--no-index` is given), and `.` when it has none; for `diff -r` and
/// `git diff --no-index`, its words; for `diff`, its words one level down, as it compares each
/// file two directories both hold. A word built at run time is left to the operand check, which
/// refuses it; a word a redirect takes is none. Beside them, why the command may also read below
/// a directory this check cannot place (a pathspec git reads from its repository's top).
fn descends(seg: &[String], at: usize) -> (Vec<Tree<'_>>, Option<String>) {
    fn words(args: Vec<&str>, here: bool, below: usize) -> Vec<Tree<'_>> {
        args.into_iter()
            .filter(|w| !w.starts_with('-'))
            .filter(|w| !w.contains(SUBSTITUTION) && !w.contains(QUOTED_SUBSTITUTION))
            .chain(here.then_some("."))
            .map(|root| Tree {
                root,
                below,
                revision: false,
            })
            .collect()
    }
    let args = unredirected(seg, at + 1);
    let short = |letters: &[char]| {
        args.iter().any(|a| {
            a.len() > 1 && a.starts_with('-') && !a.starts_with("--") && a[1..].contains(letters)
        })
    };
    let long = |names: &[&str]| {
        args.iter().any(|a| {
            a.strip_prefix("--")
                .map(|l| l.split('=').next().unwrap_or(""))
                .is_some_and(|l| !l.is_empty() && names.iter().any(|n| n.starts_with(l)))
        })
    };
    let trees = match program(&seg[at]) {
        "grep" | "egrep" | "fgrep"
            if short(&['r', 'R'])
                || long(&["recursive", "dereference-recursive", "directories"])
                || args.iter().any(|a| a.contains("recurse")) =>
        {
            let here = !grammar_names_operand(seg, at);
            words(args, here, TREE)
        }
        "rg" => {
            let here = !grammar_names_operand(seg, at);
            words(args, here, TREE)
        }
        "find"
            if args
                .iter()
                .any(|a| matches!(*a, "-exec" | "-execdir" | "-ok" | "-okdir")) =>
        {
            let here = !find_names_a_path(&args);
            words(args, here, TREE)
        }
        "diff" if short(&['r']) || long(&["recursive"]) => words(args, false, TREE),
        "diff" => words(args, false, 1),
        "git" => match git_subcommand(seg, at) {
            Some((sub, "grep")) => {
                let rest = unredirected(seg, sub + 1);
                let no_index = rest.contains(&"--no-index");
                let (operands, past) = git_grep_operands(&rest);
                let (specs, revision) = match past {
                    Some(past) => (past, false),
                    None => (operands, !no_index),
                };
                let specs: Vec<&str> = specs
                    .into_iter()
                    .filter(|w| !w.contains(SUBSTITUTION) && !w.contains(QUOTED_SUBSTITUTION))
                    .collect();
                let mut trees = Vec::new();
                if specs.is_empty() {
                    trees.push(Tree {
                        root: ".",
                        below: TREE,
                        revision: false,
                    });
                }
                let mut unplaced = None;
                for spec in specs {
                    match pathspec_tree(spec) {
                        Some(root) => trees.push(Tree {
                            root,
                            below: TREE,
                            revision: revision && may_name_revision(spec),
                        }),
                        None => {
                            unplaced.get_or_insert_with(|| {
                                format!(
                                    "git grep's pathspec {spec}, read from a directory this check \
                                     cannot place (the repository's top, past a `..` after a \
                                     glob, or behind magic that never closes)"
                                )
                            });
                        }
                    }
                }
                return (trees, unplaced);
            }
            Some((sub, "diff")) if args.contains(&"--no-index") => {
                words(unredirected(seg, sub + 1), false, TREE)
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    (trees, None)
}

/// The tree git reads for `word`, a word `git grep` may take as a pathspec, as [`descends`] holds
/// it: git matches a pathspec by wildmatch, where `*`, `?` and a bracket expression also match a
/// `/` and a name's leading dot, so a word holding one (or a `\`, which may escape one) reads
/// every file below the directory its components before that one name (`.` when the first holds
/// it, `/` when only a leading `/` comes before it), and a word holding none reads the file or
/// tree it names. Past its magic (`:(…)`, or `:` and its letters `/`, `!` and `^`), an exclusion
/// (`exclude`, `!`, `^`) reads below `.`, as exclusions alone read the whole tree where git
/// stands, and so does `icase`, whose names match any case of theirs, which a case-sensitive
/// volume holds apart (MEASURED 2026-11-05 with git 2.56.0 and `core.ignorecase=false`:
/// `:(icase)SRC` read `src/x.rs` where `SRC` read nothing). `None` where git may read below a
/// directory this check cannot place, the repository's top (`top`, `/`) or past a `..` after a
/// glob, and for a long magic that never closes, which this does not read.
///
/// MEASURED 2026-11-05 with git 2.56.0, each pathspec quoted, in a scratch home holding
/// `.claude.json`, `.config/gh/hosts.yml` and `proj/{.env,a.json,dir/x,sub/deep/x.rs}`
/// (docs/measured/classifier-zsh-glob-2026-11-05.md): with `--no-index` and no repository,
/// `*.yml` run in the home printed `.config/gh/hosts.yml`; `s?b?deep/x.rs`, `sub?deep?x.rs` and
/// `sub[/]deep/x.rs` run in `proj` printed `sub/deep/x.rs`, and `d*` printed `dir/x`, where
/// `d?r` printed nothing; `:!x` run in the home printed every file below it, dotfiles among them;
/// `:/` run in `proj/sub` read below it alone. In a repository whose top is the home, run in
/// `proj/sub`: `:/` read the whole top (`../../.claude.json` and `../../.config/gh/hosts.yml`
/// among its files), with `--no-index` too; `:(top)*.yml` read `../../.config/gh/hosts.yml`;
/// `../../*.json` read `../../.claude.json`; and `:!x` read below `proj/sub` alone.
fn pathspec_tree(word: &str) -> Option<&str> {
    let pattern = if let Some(long) = word.strip_prefix(":(") {
        let (magic, rest) = long.split_once(')')?;
        if magic.split(',').any(|m| m == "top") {
            return None;
        }
        if magic.split(',').any(|m| matches!(m, "exclude" | "icase")) {
            return Some(".");
        }
        rest
    } else if let Some(short) = word.strip_prefix(':') {
        let n = short
            .find(|c| !matches!(c, '/' | '!' | '^'))
            .unwrap_or(short.len());
        if short[..n].contains('/') {
            return None;
        }
        if n > 0 {
            return Some(".");
        }
        short.strip_prefix(':').unwrap_or(short)
    } else {
        word
    };
    let Some(g) = pattern.find(['*', '?', '[', '\\']) else {
        return Some(if pattern.is_empty() { "." } else { pattern });
    };
    if pattern[g..].split('/').any(|c| c == "..") {
        return None;
    }
    Some(match pattern[..g].rfind('/') {
        Some(0) => "/",
        Some(k) => &pattern[..k],
        None => ".",
    })
}

/// How many levels below a tree [`descends`] names a recursive reader reads: any.
const TREE: usize = usize::MAX;

/// Whether find's words `args` (after the program) name a path to start from: a word past its
/// options (`-H`, `-L`, `-P` and BSD's `-E`, `-X`, `-d`, `-s`, `-x`, alone or clustered; GNU's
/// `-D` with its value and `-O<level>`) that opens no expression (`-…`, `(`, `!`), or BSD's `-f`
/// with its path. With none, GNU find starts from `.`.
fn find_names_a_path(args: &[&str]) -> bool {
    let mut k = 0;
    while let Some(a) = args.get(k) {
        match *a {
            "-f" => return k + 1 < args.len(),
            "-D" => k += 2,
            "--" => {
                k += 1;
                break;
            }
            w if w.starts_with("-O") => k += 1,
            w if w.len() > 1
                && w.starts_with('-')
                && w[1..].chars().all(|c| "HLPEXdsx".contains(c)) =>
            {
                k += 1;
            }
            _ => break,
        }
    }
    args.get(k).is_some_and(|w| !w.starts_with(['-', '(', '!']))
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

const RESET_SPEND: &[&str] = &[
    "use my reset",
    "use your reset",
    "reset your session limit",
    "reset your 5-hour limit",
];

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

/// Keep the conversation the launch selected, regardless of the vendor's default focus.
fn conversation_resume(prompt: &PromptV2, rows: &[String]) -> Decision {
    let [resume, new] = prompt.options.as_slice() else {
        return Decision::escalate("a conversation resume dialog with changed options");
    };
    if prompt.head_off_screen
        || resume.label != aterm_phase::anchor!("conversation_resume.resume")
        || !new
            .label
            .starts_with(aterm_phase::anchor!("conversation_resume.new"))
    {
        return Decision::escalate("a conversation resume dialog not read whole");
    }
    let choice = match choice_of(prompt, resume) {
        Ok(choice) => choice,
        Err(why) => return Decision::escalate(why),
    };
    let Some(title) = aterm_phase::prompt::title_row(prompt, rows) else {
        return Decision::escalate("the conversation resume title is not on the screen");
    };
    Decision::Approve {
        rule_id: RULE_CONVERSATION_RESUME,
        choice,
        guard: row_guard(title),
        subject: format!("{}{SUBJECT_OPTION}{}", prompt.title, resume.label),
        unproven: None,
    }
}

/// Claude Code's `Claude in Chrome` intro (2.1.317), confirmed with its footer's Enter at `safe`
/// and `all`. Claude Code draws it, before its REPL, only in a session started with Claude in
/// Chrome on (as `--chrome` turns it on), so the person who started it asked for what it
/// introduces; it offers nothing to choose. Its Esc is no refusal: in 2.1.317 Enter and Esc both
/// call the dialog's `onDone`, the intro has recorded itself shown when it was drawn, and Chrome
/// stays on for the session either way, so confirming dismisses it and changes nothing else.
/// Enter is the answer the dialog names. Under `approve = "none"` the person's configuration keeps
/// every box theirs, this one included.
fn chrome_intro(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if ctx.approve == Approve::None {
        return Decision::escalate(
            "approve = \"none\": the Claude in Chrome intro is the owner's to confirm",
        );
    }
    let Some(confirm) = prompt.chrome_intro_confirm() else {
        if prompt.head_off_screen {
            return Decision::escalate(format!(
                "the Claude in Chrome intro's top border is not on the screen (a box taller \
                 than the pane); {UNREAD}"
            ));
        }
        return Decision::escalate("a Claude in Chrome intro not read whole");
    };
    // The title's first row: a narrow pane wraps the title under it.
    let Some(title_row) = rows.get(prompt.span.0) else {
        return Decision::escalate("the Claude in Chrome intro's title is not on the screen");
    };
    Decision::Approve {
        rule_id: RULE_CHROME_INTRO,
        choice: Choice::Focus {
            steps: 0,
            label: confirm.label.clone(),
        },
        guard: row_guard(title_row),
        subject: format!("{}{SUBJECT_OPTION}{}", prompt.title, confirm.label),
        unproven: None,
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
pub(crate) mod tests;

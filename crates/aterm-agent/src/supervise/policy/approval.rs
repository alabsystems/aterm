// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE approval decider: which option, if any, the supervisor may press on an
//! agent's approval box ([`decide`]).
//!
//! **The ruling it implements** (owner decision 1, 2026-09-23, replacing
//! "aterm never types y" for the supervisor): the supervisor presses the
//! approving option only on a box this function fully parses and proves safe,
//! and escalates everything else. Four rules approve today, each with a rule
//! id the ledger records:
//!
//! * [`RULE_READ_ONLY`] — a ` Bash command` box every reading of whose
//!   command classifies read-only ([`PromptV2::readings`]: the rows the
//!   parser shows as the command and ALL the rows the command may occupy,
//!   each joined with spaces — a soft wrap — and, in a box with `│` bars,
//!   with newlines — a statement break). A newline ends a segment and a
//!   space does not, so `git status⏎touch x` shown as two rows is one read
//!   under the space reading and a write under the other (audit APR-5); a
//!   row the parser shows as the description may be the command's tail
//!   (lane B's review: a barred `Rm -rf …` row read as prose was never
//!   judged). Never narrowed for this rule: only the rm breaker's box,
//!   whose geometry was measured, drops a row the box proves is no wrap
//!   of the command ([`command_readings`]). Never on a box that
//!   carries a vendor note row, and never in a bypass session: there every
//!   box is a vendor circuit breaker.
//! * [`RULE_RM_BREAKER`] — the vendor's `Dangerous rm operation on
//!   possibly-empty variable path` box in a bypass session, when every `rm`
//!   operand under every reading resolves strictly inside a scratch root
//!   ([`super::rm_breaker`]) AND the rest of the line is a read
//!   ([`classify_except_rm`]). The rest was once left unjudged ("in bypass it
//!   runs unasked anyway"), but the bypass it knows of was read off a footer
//!   the box has since replaced, and a shift-tab out of bypass inside that
//!   window left the whole line approved (lane B's review) — so it is judged.
//! * [`RULE_READ_OUTSIDE_CWD`] — a ` Read file(s)` box whose one path is
//!   absolute (or `~/…`), under an ALLOWED root ([`ApprovalCtx::read_roots`]:
//!   the trust roots, `/usr`, `/etc`, `/opt/homebrew`, the uid's
//!   `/private/tmp/claude-<uid>`) and outside the [`SecretRule`] list — the
//!   path as written AND as its symlinks resolve ([`real_components`]), so
//!   a committed `docs/k -> ~/.ssh/id_rsa` under an allowed root is judged
//!   where it leads (lane B2's review). A link the worker makes after the
//!   read is not seen: the check is the read's, not the Read's.
//! * [`RULE_TRUST_DIALOG`] — Claude Code's folder-trust dialog, when the
//!   folder it asks about is the session's own working directory (`meta
//!   cwd=`, [`ApprovalCtx::cwd_known`]) AND under a trust root. It is
//!   unnumbered: the choice is to move the focus ([`Choice::Focus`]) and
//!   then Enter, the Enter only once a fresh read shows the focus on
//!   `Yes, I trust this folder` (the loop's part).
//!
//! **Which option.** Only the plain one-shot allow ([`Role::Once`] — `Yes`,
//! `Yes, proceed`, `Yes, run it`), or the trust dialog's [`Role::Trust`];
//! never a [`Role::Session`], [`Role::Persist`] or [`Role::ModeSwitch`]
//! grant, which would turn one verdict into a standing wildcard, and never
//! [`Role::Other`] — what aterm-phase gives every option of a box whose
//! options are not sound, and every option of every Codex box.
//!
//! **Which program.** The box is read by the reader for the session's
//! PROGRAM ([`aterm_phase::read`]): only a Claude Code session's box is
//! judged, and only on a reading whose phase is the reader's evidence
//! ([`Reading::phase_authoritative`]). A shell showing a captured box, a
//! pager, a Codex gate: escalated, never pressed.
//!
//! **The press.** An approval carries the guard to press it under
//! ([`super::guard::row_guard`] of the judged row), so a box swapped between
//! this read and the press is not answered on this verdict.
//!
//! **What is not closed.** Git reads honour the repository's config and
//! `.gitattributes` (`core.fsmonitor`, `diff.external`, a textconv driver),
//! and a worker in accept-edits mode can write both inside its cwd: the
//! read-only rule approves `git status` there all the same (the classifier
//! refuses `--git-dir`/`--work-tree` and the driver flags, but approves
//! `git -C <dir>` reads, whose repository's config is as writable). An
//! owner decision on whether git reads stay approved is open (lane B2's
//! review raised it again; nothing here decides it).
//!
//! Pure but for the Read rule's one look at the filesystem (where the
//! path's symlinks lead): no socket, no clock, no environment. The caller supplies the screen
//! rows, the [`Reading`] of them and an [`ApprovalCtx`] (the session's cwd,
//! the permission mode its footer last showed, the toggles and roots).

use std::path::{Path, PathBuf};

use aterm_phase::prompt::{PromptKind, PromptV2, Role, Select};
use aterm_phase::{Phase, Program, Reading};

use super::guard::row_guard;
use super::rm_breaker::{RmScope, ScratchRoot, abs_components, resolve_rm_line};
use crate::supervise::classify::{classify_command_with, classify_except_rm, glob_match};

/// The read-only Bash rule's id (the classifier generation it trusts).
pub const RULE_READ_ONLY: &str = "read-only@classify-v3";
/// The rm circuit-breaker rule's id.
pub const RULE_RM_BREAKER: &str = "rm-breaker@v2";
/// The Read-outside-cwd rule's id.
pub const RULE_READ_OUTSIDE_CWD: &str = "read-outside-cwd@v2";
/// The folder-trust dialog rule's id.
pub const RULE_TRUST_DIALOG: &str = "trust-dialog@v1";

/// The note row the vendor draws on its rm circuit breaker (Claude Code
/// 2.1.278 and 2.1.280, measured) — aterm-phase's `box.rm_breaker` anchor.
pub const RM_BREAKER_NOTE: &str = aterm_phase::anchors::anchor_text("box.rm_breaker");

/// How the approving option is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Press this digit: a numbered box ([`Select::Digits`]).
    Digit(u8),
    /// Move the focus `steps` options (down when positive, up when
    /// negative), then press Enter once a fresh read shows it on the option
    /// labelled `label` — an unnumbered dialog ([`Select::ArrowsEnter`]).
    /// `steps` 0: the focus is on it already.
    Focus { steps: i32, label: String },
}

/// What to do with one box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Choose `choice` under `guard`.
    Approve {
        /// Which rule approved ([`RULE_READ_ONLY`], [`RULE_RM_BREAKER`],
        /// [`RULE_READ_OUTSIDE_CWD`], [`RULE_TRUST_DIALOG`]).
        rule_id: &'static str,
        /// The option, and how it is chosen.
        choice: Choice,
        /// The anchored `key if=` pattern of the judged row
        /// ([`super::guard::row_guard`]): the command's first row, the
        /// path's, the trust dialog's folder.
        guard: String,
        /// What was judged: the command (every row, newline-joined) or the
        /// path; for the rm rule, the resolved targets follow after ` => `.
        subject: String,
    },
    /// Hand the box to a person, with the reason.
    Escalate {
        /// Why no rule approved it.
        reason: String,
    },
}

impl Decision {
    fn escalate(reason: impl Into<String>) -> Self {
        Self::Escalate {
            reason: reason.into(),
        }
    }
}

/// Which rules may approve. Each is the owner's switch; all on is decision
/// 1's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalToggles {
    /// [`RULE_READ_ONLY`].
    pub auto_reads: bool,
    /// [`RULE_RM_BREAKER`].
    pub rm_breaker: bool,
    /// [`RULE_READ_OUTSIDE_CWD`].
    pub read_outside_cwd: bool,
    /// [`RULE_TRUST_DIALOG`].
    pub trust_dialog: bool,
}

impl Default for ApprovalToggles {
    fn default() -> Self {
        Self {
            auto_reads: true,
            rm_breaker: true,
            read_outside_cwd: true,
            trust_dialog: true,
        }
    }
}

impl ApprovalToggles {
    /// Every rule off: everything escalates.
    pub fn off() -> Self {
        Self {
            auto_reads: false,
            rm_breaker: false,
            read_outside_cwd: false,
            trust_dialog: false,
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

/// The default secrets list: key and credential stores wherever they sit,
/// the file shapes that hold keys and tokens (aterm's own control tokens are
/// `*.token`), shell histories, and — under `home` — all of `~/Library`
/// (browser profiles, keychains, cookies, app tokens) and the tools'
/// credential directories. It is checked on top of the allow-list
/// ([`ApprovalCtx::read_roots`]): a trust root holds `.env` files too.
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

/// The roots a config spells as text (`[harness] trust_roots`,
/// `SupervisorConfig::trust_roots`): a leading `~/` is `home`, a trailing
/// `*` matches any suffix of the last component (`~/aterm*` is every aterm
/// worktree), and the result is resolved lexically — a spec that is not
/// absolute after `~`, holds a `..`, or globs anywhere but at the end of its
/// last component is dropped, never widened.
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

/// The owner's defaults for `trust_roots` when no config names any
/// (`SupervisorConfig::default`'s).
const DEFAULT_TRUST_ROOTS: &[&str] = &["~/aterm*", "~/ay*", "$HOME/trust*", "/private/tmp/claude-*"];

/// Everything [`decide`] knows besides the box.
#[derive(Debug, Clone)]
pub struct ApprovalCtx {
    /// The session's working directory as the server reports it (`meta
    /// cwd=`, the shell's OSC 7): where the agent was LAUNCHED, never the
    /// Bash tool's own, which the worker moves.
    pub cwd: PathBuf,
    /// Whether [`Self::cwd`] was reported (`meta cwd=-` and an unread meta
    /// are not): the trust dialog and the rm breaker need it.
    pub cwd_known: bool,
    /// The owner's home directory (`~` in a Read path; the rm deny table).
    pub home: Option<PathBuf>,
    /// Whether the session runs with bypass permissions on — from its footer
    /// ([`footer_mode`]) as last read, since Claude Code 2.1.280 draws the box
    /// where the footer was.
    pub bypass_mode: bool,
    /// The rules' switches.
    pub toggles: ApprovalToggles,
    /// Where a folder-trust dialog's folder may be trusted
    /// ([`RULE_TRUST_DIALOG`]).
    pub trust_roots: Vec<ScratchRoot>,
    /// Where a Read box may read ([`RULE_READ_OUTSIDE_CWD`]): the trust
    /// roots and the system roots ([`Self::new`]).
    pub read_roots: Vec<ScratchRoot>,
    /// Where an rm operand may point ([`RULE_RM_BREAKER`]).
    pub scratch_roots: Vec<ScratchRoot>,
    /// The worker's `$TMPDIR`, for `$TMPDIR` and `$(mktemp -d)` on an rm line.
    pub tmpdir: Option<PathBuf>,
    /// What a Read box may not be approved for, under an allowed root.
    pub secrets: Vec<SecretRule>,
    /// The `python3 <script>` globs the classifier treats as reads (none by
    /// default).
    pub python_allow: Vec<String>,
}

impl ApprovalCtx {
    /// Decision 1's defaults for a session in `cwd`, owned by `uid`:
    /// scratch roots `/private/tmp/claude-<uid>/`, `$TMPDIR` (when it is at
    /// least two levels deep and holds neither `cwd` nor `home`), any
    /// `/tmp/<x>/` and `/private/tmp/<x>/`, and `<cwd>/target*`; the default
    /// trust roots (`~/aterm*`, `~/ay*`, `$HOME/trust*`,
    /// `/private/tmp/claude-*`; [`Self::set_trust_roots`] replaces them);
    /// Read roots the trust roots plus `/usr`, `/etc`, `/private/etc`,
    /// `/opt/homebrew` and `/private/tmp/claude-<uid>`; the
    /// [`default_secrets`]; the cwd known; not bypass until a footer says so.
    pub fn new(cwd: PathBuf, home: Option<PathBuf>, uid: u32, tmpdir: Option<PathBuf>) -> Self {
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
        let defaults: Vec<String> = DEFAULT_TRUST_ROOTS.iter().map(|s| s.to_string()).collect();
        let mut ctx = Self {
            secrets: default_secrets(home.as_deref()),
            cwd,
            cwd_known: true,
            home,
            bypass_mode: false,
            toggles: ApprovalToggles::default(),
            trust_roots: Vec::new(),
            read_roots: Vec::new(),
            scratch_roots,
            tmpdir,
            python_allow: Vec::new(),
        };
        ctx.set_trust_roots(&defaults, uid);
        ctx
    }

    /// The trust roots from the owner's `[harness] trust_roots`
    /// ([`roots_from_config`]), and the Read roots rebuilt around them. A
    /// `claude-*` spec under `/private/tmp` or `/tmp` names this uid's own
    /// `claude-<uid>` only: the glob matched every user's (lane B2's
    /// review).
    pub fn set_trust_roots(&mut self, specs: &[String], uid: u32) {
        let specs: Vec<String> = specs.iter().map(|s| own_claude_tmp(s, uid)).collect();
        self.trust_roots = roots_from_config(&specs, self.home.as_deref());
        self.read_roots = self.trust_roots.clone();
        for sys in [
            "/usr",
            "/etc",
            "/private/etc",
            "/opt/homebrew",
            &format!("/private/tmp/claude-{uid}"),
        ] {
            self.read_roots.extend(ScratchRoot::dir(Path::new(sys)));
        }
    }
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

/// The permission mode Claude Code's footer names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterMode {
    /// `? for shortcuts` and no mode word: the default mode.
    Default,
    /// `⏵⏵ bypass permissions on`.
    Bypass,
    /// `⏵⏵ auto mode on`.
    Auto,
    /// `⏵⏵ accept edits on`.
    AcceptEdits,
    /// `⏸ plan mode on`.
    Plan,
}

/// The mode the footer under the composer names, read from the last rows of
/// the screen; `None` when no footer is drawn — which is the case while a
/// 2.1.280 box is up, so a caller keeps the last answer it read.
pub fn footer_mode(rows: &[String]) -> Option<FooterMode> {
    let tail = rows.iter().rev().filter(|r| !r.trim().is_empty()).take(3);
    for r in tail {
        let t = r.trim();
        if t.contains("bypass permissions on") {
            return Some(FooterMode::Bypass);
        }
        if t.contains("auto mode on") {
            return Some(FooterMode::Auto);
        }
        if t.contains("accept edits on") {
            return Some(FooterMode::AcceptEdits);
        }
        if t.contains("plan mode on") {
            return Some(FooterMode::Plan);
        }
        if t.contains("? for shortcuts") {
            return Some(FooterMode::Default);
        }
    }
    None
}

/// Decide the box `reading` found on the screen `rows`.
///
/// `rows` are required, not only the reading: a press is guarded on the
/// judged row as drawn, and the rows around the box are cross-checked.
pub fn decide(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if reading.program != Program::Claude {
        return Decision::escalate(format!(
            "a {} session's box: only Claude Code's boxes are judged",
            reading.program.name()
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
        kind => Decision::escalate(format!("a {} box: no rule approves this kind", kind.name())),
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

/// The one plain one-shot allow of a numbered box: exactly one option with
/// [`Role::Once`], a number, and a refusing option beside it.
fn once_choice(prompt: &PromptV2) -> Result<Choice, String> {
    if prompt.select != Select::Digits {
        return Err("an unnumbered box".to_string());
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

/// `❯ 1. Yes` / `2. No`: a number, a dot, a space.
fn is_option_row(t: &str) -> bool {
    let t = t.strip_prefix('❯').map_or(t, str::trim_start);
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    (1..=2).contains(&digits) && t[digits..].starts_with(". ")
}

/// No option-shaped row above the box's question: the options are the rows
/// under `Do you want …`, and a `1. Yes` drawn among the command rows is
/// either command text or a forgery (lane B's review; aterm-phase reads the
/// options under the question too, so this is a second line of defence).
fn no_option_above_the_question(prompt: &PromptV2, rows: &[String]) -> Result<(), String> {
    let (top, bottom) = prompt.span;
    let bottom = bottom.min(rows.len());
    let Some(q) = (top + 1..bottom)
        .rev()
        .find(|&k| rows[k].trim_start().starts_with("Do you want"))
    else {
        return Err("a box with no question row".to_string());
    };
    if (top + 1..q).any(|k| is_option_row(rows[k].trim())) {
        return Err("an option-shaped row above the box's question".to_string());
    }
    Ok(())
}

/// The box's content width in cells, from its own top rule — the row of
/// `─` right above its title, drawn across the box — or `None` when the box
/// has none. `─` is one cell wide, so its count is its width.
fn box_width(prompt: &PromptV2, rows: &[String]) -> Option<usize> {
    let rule = rows.get(prompt.span.0.checked_sub(1)?)?;
    let t = rule.trim();
    (!t.is_empty() && t.chars().all(|c| c == '─')).then(|| rule.trim_end().chars().count())
}

/// Whether a row's width in cells is its character count, with nothing to
/// measure wrongly: printable ASCII only. The server's row text leaves out
/// a wide character's continuation cell, so a row of CJK text or emoji
/// reads as half the width it is drawn (lane B2's review: `cat 日×55` over
/// a wrapped `&& rm -rf ~/work` was approved as a read) — and an emoji's
/// drawn width depends on its variation selector and the font. A row with
/// anything else in it is never measured: no narrowing, every reading.
fn cells_are_chars(row: &str) -> bool {
    row.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// How far inside the box's width a row, plus the next row's first word,
/// must end before the row is taken as NOT wrapped. The command's right
/// edge is not measured on a wrapped command row; the one wrapped row in
/// the box measured (`CAP_RM`'s note, `│ Dangerous rm operation …`) ran to
/// cell 113 of a 120-cell rule, so the edge is at least `width - 7`. A
/// quarter of the width (30 cells at 120) is far inside that, so a row
/// that fits the next word within it would have fitted it at any edge the
/// box can have; the measured rm row (61 cells, `Remove` next) still does.
fn wrap_margin(width: usize) -> usize {
    (width / 4).max(8)
}

/// The readings of a Bash box's command to judge ([`PromptV2::readings`]),
/// narrowed by geometry ONLY for the rm circuit breaker (`rm_breaker`), the
/// one shape the narrowing was measured for, and only where the screen
/// proves it. Without bars the command is ONE shell line (aterm-phase), so
/// a row after its first is its tail only if it is a WRAP — and a
/// word-wrapped row ends short of the box's width only when the next row's
/// first word would not have fitted. A row that ended with room for that
/// word to spare ([`wrap_margin`]) cannot have wrapped: it ends the
/// command, and the rows under it are the description, which is then
/// judged as no command (the measured rm box: a 61-cell command row over
/// `Remove directories tmp/a and tmp/b` in a 120-cell box — its words were
/// rm operands under the all-rows reading). Every other box, a box with
/// bars, a box with no top rule to measure it by, and a box whose rows are
/// not all printable ASCII ([`cells_are_chars`]) are judged under every
/// reading aterm-phase gives: nothing is narrowed without the evidence.
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
    let title = prompt.title.as_str();
    let header = aterm_phase::anchor("box.bash");
    let from_plugin = title
        .strip_prefix(header)
        .is_some_and(|rest| rest.starts_with(" · from the \""));
    if title != header && !from_plugin {
        return Decision::escalate(format!("the box header is not a Bash header: {title}"));
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
    let rm_breaker = is_rm_breaker(&prompt.notes);
    let (readings, subject) = command_readings(prompt, rows, rm_breaker);
    let guard = row_guard(&rows[first_row]);
    if rm_breaker {
        if !ctx.toggles.rm_breaker {
            return Decision::escalate("the rm circuit breaker (rule off)");
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
            tmpdir: ctx.tmpdir.as_deref(),
            roots: &ctx.scratch_roots,
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
        return match once_choice(prompt) {
            Ok(choice) => Decision::Approve {
                rule_id: RULE_RM_BREAKER,
                choice,
                guard,
                subject: format!("{subject} => {}", targets.join(" ")),
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
    if !ctx.toggles.auto_reads {
        return Decision::escalate("a Bash box (read-only rule off)");
    }
    if let Err(why) = read_only_every_reading(&readings, &ctx.python_allow) {
        return Decision::escalate(why);
    }
    match once_choice(prompt) {
        Ok(choice) => Decision::Approve {
            rule_id: RULE_READ_ONLY,
            choice,
            guard,
            subject,
        },
        Err(why) => Decision::escalate(why),
    }
}

/// How a reason names a reading: by the join it has.
fn reading_name(line: &str) -> &'static str {
    if line.contains('\n') {
        "newline reading"
    } else {
        "space reading"
    }
}

/// `Ok` when every reading of a Bash box's command ([`command_readings`])
/// classifies read-only; `Err` names the reading
/// and the classifier's reason. The one judgment the read-only rule makes.
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

fn is_rm_breaker(notes: &[String]) -> bool {
    // One note, possibly wrapped over several rows (aterm-phase joins a
    // note block's rows): it opens with the vendor's words and holds no
    // second warning.
    let [note] = notes else {
        return false;
    };
    note.starts_with(RM_BREAKER_NOTE)
        && note.matches("Dangerous").count() == 1
        && note.trim_end().ends_with(')')
        && note.matches('(').count() == note.matches(')').count()
}

fn read_box(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if !ctx.toggles.read_outside_cwd {
        return Decision::escalate("a Read box (rule off)");
    }
    let title = prompt.title.as_str();
    let header = aterm_phase::anchor("box.read");
    if !title
        .strip_prefix(header)
        .is_some_and(|rest| matches!(rest, "" | "s" | "(s)"))
    {
        // A Bash box's description row (`Read file /etc/hosts`) is not a
        // Read header.
        return Decision::escalate(format!("the box header is not a Read header: {title}"));
    }
    let Some(path) = prompt.path.as_deref() else {
        return Decision::escalate("a Read box with no path");
    };
    let Some(path_row) = row_of(rows, prompt, path) else {
        return Decision::escalate("the Read path's row is not on the screen");
    };
    let choice = match once_choice(prompt) {
        Ok(c) => c,
        Err(why) => return Decision::escalate(why),
    };
    match read_path_allowed(path, ctx) {
        Ok(()) => Decision::Approve {
            rule_id: RULE_READ_OUTSIDE_CWD,
            choice,
            guard: row_guard(&rows[path_row]),
            subject: path.to_string(),
        },
        Err(why) => Decision::escalate(why),
    }
}

/// `path` resolved to its absolute components: `~/…` against `home`, no
/// glob, no `..`.
fn resolve_abs(path: &str, home: Option<&Path>) -> Result<Vec<String>, String> {
    if path.contains(['*', '?', '[', '{']) {
        return Err(format!("a pattern: {path}"));
    }
    let full = if let Some(rest) = path.strip_prefix("~/") {
        let home = home.ok_or_else(|| format!("{path} under ~ with no home to resolve it"))?;
        format!("{}/{rest}", home.to_string_lossy().trim_end_matches('/'))
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        return Err(format!("a path that is not absolute: {path}"));
    };
    abs_components(&full).ok_or_else(|| format!("a path with `..`: {path}"))
}

/// A Read box's path: absolute or `~/…`, one plain path (no glob, no `..`),
/// under a [`ApprovalCtx::read_roots`] root, and outside every
/// [`SecretRule`].
fn read_path_allowed(path: &str, ctx: &ApprovalCtx) -> Result<(), String> {
    let comps = resolve_abs(path, ctx.home.as_deref()).map_err(|e| format!("a Read of {e}"))?;
    read_comps_allowed(&comps, path, ctx)?;
    // Where its symlinks lead, judged the same (module header).
    let real = real_components(&comps)
        .ok_or_else(|| format!("a Read whose symlinks cannot be resolved: {path}"))?;
    if real != comps {
        read_comps_allowed(&real, &format!("{path} (-> /{})", real.join("/")), ctx)?;
    }
    Ok(())
}

/// `comps` with every symlink on the way resolved: the deepest ancestor
/// that exists, canonicalized, and the components under it that do not
/// exist yet, as written. `None` when a component is a link that does not
/// resolve (dangling — it would lead wherever it is later pointed), or the
/// filesystem refuses the look.
pub(crate) fn real_components(comps: &[String]) -> Option<Vec<String>> {
    let mut k = comps.len();
    loop {
        let p = format!("/{}", comps[..k].join("/"));
        match std::fs::canonicalize(&p) {
            Ok(real) => {
                let mut out = abs_components(&real.to_string_lossy())?;
                out.extend(comps[k..].iter().cloned());
                return Some(out);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && k > 0 => {
                if std::fs::symlink_metadata(&p).is_ok() {
                    return None;
                }
                k -= 1;
            }
            Err(_) => return None,
        }
    }
}

/// The root and secret checks of [`read_path_allowed`] on one spelling of
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

/// The folder-trust dialog ([`RULE_TRUST_DIALOG`]): its folder is the
/// session's own launch directory and under a trust root, and its options
/// are exactly the two measured ones (`No, exit`, `Yes, I trust this
/// folder`) with the focus on one of them. The choice moves the focus from
/// where it is to the trust option.
fn trust_dialog(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if !ctx.toggles.trust_dialog {
        return Decision::escalate("a folder-trust dialog (rule off)");
    }
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
    if !ctx.trust_roots.iter().any(|r| r.holds(&comps)) {
        return Decision::escalate(format!(
            "a folder-trust dialog for {path}, under no trust root"
        ));
    }
    let roles: Vec<Role> = prompt.options.iter().map(|o| o.role).collect();
    if roles.len() != 2 || !roles.contains(&Role::Trust) || !roles.contains(&Role::Exit) {
        return Decision::escalate(format!(
            "a folder-trust dialog whose options are not exactly `{}` and `{}`",
            aterm_phase::anchor("trust.no"),
            aterm_phase::anchor("trust.yes")
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
                steps: i32::try_from(target).unwrap_or(0) - i32::try_from(at).unwrap_or(0),
                label: prompt.options[target].label.clone(),
            }
        }
    };
    Decision::Approve {
        rule_id: RULE_TRUST_DIALOG,
        choice,
        guard: row_guard(&rows[path_row]),
        subject: path.to_string(),
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;

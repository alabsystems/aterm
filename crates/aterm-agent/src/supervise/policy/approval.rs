// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE approval decider: which option, if any, the supervisor may press on an
//! agent's box ([`decide`]), under the owner's `[harness] approve` level
//! ([`Approve`]) — or which refusal it amends with a reason the worker acts
//! on ("THE DECLINE" below).
//!
//! **Full power, the default** (owner, 2026-09-24: *"UNLESS aterm is
//! configured otherwise, it is in FULLY AUTOMATIC mode and there cannot be
//! any interruptions because there is nobody to address the interruption"*):
//! under [`Approve::All`] every box the reader parses gets its answer —
//!
//! * a permission box, whatever it asks: its ONE-SHOT allow ([`Role::Once`];
//!   a box with no such role, its first `Yes…` option that is no
//!   [`Role::Session`], [`Role::Persist`] or [`Role::ModeSwitch`] grant) —
//!   [`RULE_ALLOW_ONCE`]. A standing grant is never chosen: it would rewrite
//!   the agent's own settings, which no one asked for;
//! * the folder-trust dialog, for any folder: `Yes, I trust this folder`
//!   ([`RULE_TRUST_ANY`]);
//! * plan mode's approval, and its entry: its `Yes…` that grants no
//!   standing mode ([`RULE_PLAN`], [`plan_pick`]) — Claude Code's `Yes,
//!   manually approve edits` (the session back in its default mode, every
//!   edit still asked), Codex's `Yes, implement this plan`, the entry's
//!   `Yes, enter plan mode` — never `auto mode`, `accept edits`, `bypass
//!   permissions` or `clear context`, each a standing mode or the
//!   conversation wiped; a plan box with no such yes is declined;
//! * Claude Code's model-refusal pause (`Session paused`, its options exactly
//!   `Switch to <model>` and `Edit prompt and retry…`): its switch
//!   ([`RULE_MODEL_SWITCH`]) while `[harness] model_fallback` is set — the
//!   switch the owner approved for a model's limit (aab68649f) — else a
//!   person's. NO OTHER BOX'S `Switch to …` is ever pressed by that rule: on
//!   2026-09-27 and 09-28 it pressed Codex's rate-limit nudge's `Switch to
//!   gpt-6-luna`, at 99% of the weekly window and again at 1% right after a
//!   usage reset, and luna then ran the owner's work for eleven hours;
//! * Codex's RATE-LIMIT MODEL NUDGE (aterm-phase's `RateNudge`), under
//!   `[harness] rate_nudge`: its `Switch to <model>`
//!   ([`RULE_RATE_NUDGE_SWITCH`], [`rate_nudge`]) ONLY when Codex's own usage
//!   reading says the window is near its limit (≥ 90%, the owner's number,
//!   2026-09-28) and the model it leaves was read off the footer — and then
//!   only to SAVE the work: the loop ledgers the switch's intent before the
//!   key and opens it once the box has left; the loop's busy reads stop
//!   Codex's goal; the turn-end policy types a commit-and-push-then-stop
//!   instruction, puts the thread back on its own model and effort, and
//!   holds the session until the window resets (`turn_end`'s
//!   save-then-wait). Every other nudge — a stale one, one while
//!   a switch is already open, one whose reading is unknown — gets `Keep
//!   current model` ([`RULE_RATE_NUDGE_KEEP`]); `Keep current model (never
//!   show again)` is never pressed (it writes Codex's config);
//! * Codex's `/model` picker (aterm-phase's `ModelPick`) only while the
//!   harness's OWN restore is in flight ([`ApprovalCtx::model_restore`]: its
//!   picker not yet left, no person's keystroke since its `/model`): the
//!   exact model and effort the thread had before the switch, the effort by
//!   its `s` — this conversation only — never Enter, which saves a default
//!   ([`RULE_MODEL_RESTORE_PICK`], [`model_restore_pick`]). A picker a person
//!   opened is theirs;
//! * Codex's paused-goal box (aterm-phase's `GoalResume`) only where the
//!   live upgrade paused that goal for its move and owes it its resume
//!   ([`ApprovalCtx::goal_resume`]): its focused first option, the resume
//!   ([`RULE_GOAL_RESUME`], [`goal_resume_pick`]). A box a person's own
//!   `codex resume` drew is theirs, and `Leave paused` is never pressed;
//! * the model-switch confirmation (`Switch model?` / `Change effort
//!   level?`, aterm-phase's `ModelSwitch`): its `Yes, switch to <m>`
//!   ([`RULE_MODEL_CONFIRM`], [`model_confirm`]) — the box asks whether to
//!   pay an uncached re-read for a `/model` or `/effort` change the PERSON
//!   typed, so its yes carries out what they asked (owner direction of
//!   2026-09-26: it "should have been autoapproved") — only under its cost
//!   warning: when the person's own PreModelSwitch hook asked for the
//!   confirmation, or the subtitle reads otherwise, a person's;
//! * a dialog that is no permission: a held cross-session message is
//!   delivered (its one delivery, the option that does not refuse); a
//!   proposed goal, a Computer Use grant (its only grant the session's), the
//!   persistent read-outside setting (its `No, ask again`) and a setup or
//!   config dialog of no kind aterm-phase names — the auto-mode default,
//!   the Chrome upsell, Remote Control, a custom API key, a model upgrade
//!   that restarts — are DECLINED with the refusal that settles nothing
//!   ([`RULE_DECLINE`]): their yes settles something for good, their `No`
//!   or `Not now` nothing;
//! * a model's or extra usage's CONSENT to go on (`Continue with …`, the
//!   credits already on) on Claude Code's screen — never Codex's, where
//!   `Continue with Luna Reserve` is a model change and `Continue with
//!   detected credentials` a sign-in: accepted, as the owner directed (2026-09-24: *"a
//!   model/extra-usage consent dialog → accept"*) — it is a permission box
//!   like any other, and its yes is its one-shot allow. Turning usage
//!   credits back ON (`Yes, re-enable and continue`, `Turn on usage
//!   credits`) is no consent: Claude Code draws it exactly when the account
//!   holder switched the credits off, so its yes is spend the account's own
//!   configuration took away — read as a purchase ([`buys`]), and the box
//!   answered with the option that waits (the hazards review of
//!   2026-09-25; *"configuration limits power"*);
//! * and never a PURCHASE ([`buys`]: an option whose label OPENS with buy,
//!   purchase, add funds, upgrade, subscribe, usage credits, or a spend limit
//!   raised — `Yes, buy usage credits`, `Add funds to continue with …`,
//!   `Upgrade your plan`, `Switch to usage credits`, `Adjust monthly limit`):
//!   a box whose every yes buys is answered with the
//!   option that waits instead ([`RULE_NO_SPEND`]). The label's opening
//!   words are matched, never a word anywhere in it, so a path in a one-shot
//!   label (`billing/upgrade.rs`) cannot trip it. These dialogs (the
//!   consent, the rate-limit options menu) are read from the 2.1.281
//!   binary's strings, NOT captured on a screen: the rule goes by the labels
//!   alone.
//!
//! A box none of that answers — its only yes a standing grant or a purchase,
//! and nothing that waits — is DECLINED: its `No` ([`RULE_DECLINE`]), and the
//! worker's turn ends on the refusal, which the turn-end policy continues.
//! What is left for a person is what no reader can answer: a box whose
//! options carry no roles (one parsed with unsound options).
//!
//! **Program-neutral.** Full power reads the box by its options' ROLES, which
//! each program's reader assigns ([`aterm_phase::read`]): Codex's exec and
//! patch boxes get their `Yes, proceed` (never `don't ask again`, a Persist
//! rule, or `these files`, a Session grant), its plan box `Yes, implement
//! this plan`, and its question its recommended answer, else its first — each
//! by its digit, which Codex takes at once — and its folder gate `Trust and
//! continue` by its cursor and Enter (the one Codex box that takes no digit,
//! as its reader says: `Select::ArrowsEnter`). A box is never answered by its
//! cancel: Codex's question dialog's Esc INTERRUPTS the turn
//! ([`aterm_phase::prompt::CancelEffect::Interrupt`]), refusing nothing.
//!
//! What the safe rules said of a box full power answers rides with the
//! answer (`unproven` on [`Decision::Approve`]) into the ledger's reason, the
//! `--notes` line and the journal's `UNPROVEN` line — an rm outside every
//! scratch root, a Read of a secret, the rm/rmdir breaker's kind named
//! ([`rm_breaker_label`]) — so a press that was waved through, not proven,
//! is found by grep, never read as a routine allow.
//!
//! **A box taller than the pane** ([`PromptV2::head_off_screen`], and a box
//! whose first row read is its own question): nothing on the screen names
//! its kind, so the safe rules cannot prove it and `"safe"` escalates it —
//! but full power judges no command either, and its OPTIONS are on the
//! screen with their roles: it is answered by them alone
//! ([`unread_head`]) — the one-shot allow, else what [`allow_once`] takes,
//! a plan's yes by [`plan_pick`] — with the missing title said in
//! `unproven`. A 50-row heredoc in a 45-row pane had waited on a person for
//! three hours with `1. Yes` readable on the screen (the E2E probe of
//! 2026-09-25). A question whose head is cut stays a person's: which tab it
//! is on, and what was already checked, is off the screen.
//!
//! **What stays a person's** is what no reader can answer: a box whose
//! options carry no roles (parsed with unsound options), and a question
//! whose head is off the screen.
//!
//! **A question is not a permission.** The question dialog (Claude Code's
//! AskUserQuestion, Codex's `Question 1/1`) is answered by its own rule
//! ([`super::question::answer_question`],
//! [`super::question::RULE_ANSWER_RECOMMENDED`]) under its own key,
//! `answer_questions` ([`ApprovalCtx::answer_questions`]) — independent of
//! `approve`, which is about permission boxes (the owner directive of
//! 2026-09-25: "the harness must choose the recommended option(s) and
//! continue automatically"): [`decide`] hands it there before any level is
//! consulted, and `answer_questions = false` hands every question to a
//! person, a dialog as much as a question in prose.
//!
//! **The usage-limit dialog is neither.** Claude Code's usage-limit options
//! dialog (`What do you want to do?`, 2.1.282; aterm-phase's "THE
//! USAGE-LIMIT DIALOG", [`PromptKind::UsageLimit`]) is answered by its own
//! rule, [`RULE_LIMIT_WAIT`] ([`limit_wait`]), under its own key,
//! `limit_wait` ([`ApprovalCtx::limit_wait`], on by default) — decided FIRST,
//! before the question rule and before any level, so neither full power nor
//! the safe rules ever see it: when it offers `Wait here, then continue
//! automatically …` ([`Role::AutoResume`], matched by its label in any of its
//! three spellings, never by its place) the focus is moved onto that row and
//! Enter confirms it, as for the trust dialog, so the vendor's own automatic
//! continue is armed and the session goes on by itself when the limit
//! resets. Observed 2026-09-24: a weekly limit's dialog sat ~14 h for the
//! owner, because the vendor's `autoContinueAtUsageLimit` arms that wait by
//! itself only for a reset under 24 h away (the owner: "you should not
//! depend on the user to choose"). Never `Stop and wait for limit to reset`
//! (it stops for good — the option full power's [`waits`] would otherwise
//! take), never `Switch to usage credits`/`Add funds …`/`Upgrade your plan`
//! (money: [`buys`]), never a reset claim or low priority (a one-time
//! allowance), never `Don’t continue automatically`; a menu without the wait
//! row, or `limit_wait = false`, is a person's.
//!
//! [`Approve::Safe`] is the rules below alone, [`Approve::None`] escalates
//! every box. Under `All` those rules decide first, so a proven-safe box is
//! ledgered under the rule that proved it.
//!
//! **The safe rules** — the whole policy under `approve = "safe"`, the
//! first pass of the default (owner decision 1, 2026-09-23, replacing "aterm
//! never types y" for the supervisor). A header is read by its grammar
//! ([`PromptV2::header`]: any origin, `(unsandboxed)`, `(runs on <m>)`), a
//! ` · ` suffix it cannot read is no header of theirs.
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
//!   operand under every reading resolves strictly inside a scratch root,
//!   as written and where the disk leads it, with no `$(mktemp -d)` it is
//!   built on able to reach it empty and no variable the line does not
//!   assign — or is a directory the line's own `mktemp -d` made, `"$D"` or
//!   a path under it — ([`super::rm_breaker`]; `$TMPDIR` is a root a
//!   literal path may sit in, never a value) AND the rest of the line is a
//!   read
//!   ([`classify_except_rm`]), on a box that runs on this machine (a
//!   `(runs on <m>)` box's targets resolve there, not here). The rest was
//!   once left unjudged ("in bypass it
//!   runs unasked anyway"), but the bypass it knows of was read off a footer
//!   the box has since replaced, and a shift-tab out of bypass inside that
//!   window left the whole line approved (lane B's review) — so it is judged.
//!   The breaker is recognised as a family ([`PromptV2::rm_breaker`]): the
//!   resolver runs for the `rm` possibly-empty-variable form only (both of
//!   its [`RmBreakerKind::EmptyVariable`] spellings), the one it was written
//!   for; every other kind — a statically-unresolvable target (the
//!   2026-09-24 incident), a critical path, the working directory, a drive
//!   root, too many substitutions, and any `rmdir` breaker — escalates
//!   naming the kind ([`rm_breaker_label`]), never as "a vendor note".
//! * [`RULE_READ_OUTSIDE_CWD`] — a ` Read file(s)` box that runs on this
//!   machine (no `(runs on <m>)`: the checks below look at THIS
//!   filesystem and `$HOME`) whose one path is
//!   absolute (or `~/…`), under an ALLOWED root ([`ApprovalCtx::read_roots`]:
//!   the trust roots, `/usr`, `/etc`, `/opt/homebrew`, the uid's
//!   `/private/tmp/claude-<uid>`) and outside the [`SecretRule`] list — the
//!   path as written AND as its symlinks resolve ([`real_components`]), so
//!   a committed `docs/k -> ~/.ssh/id_rsa` under an allowed root is judged
//!   where it leads (lane B2's review). A link the worker makes after the
//!   read is not seen: the check is the read's, not the Read's.
//! * [`RULE_TRUST_DIALOG`] — Claude Code's folder-trust dialog, when the
//!   folder it asks about is the session's own working directory (`meta
//!   cwd=`, [`ApprovalCtx::cwd_known`]) AND under a trust root, its options
//!   exactly the measured `No, exit` and `Yes, I trust this folder`. It is
//!   unnumbered: the choice is to move the focus ([`Choice::Focus`]) and
//!   then Enter, the Enter only once a fresh read shows the focus on
//!   `Yes, I trust this folder` (the loop's part).
//!
//! **Which option** (the safe rules). Only the plain one-shot allow ([`Role::Once`] — `Yes`,
//! `Yes, proceed`, `Yes, run it`, Chrome's `Allow`) — by its digit, or on
//! an unnumbered box by moving the focus to it — or the trust dialog's
//! [`Role::Trust`];
//! never a [`Role::Session`], [`Role::Persist`] or [`Role::ModeSwitch`]
//! grant, which would turn one verdict into a standing wildcard, and never
//! [`Role::Other`] — what aterm-phase gives every option of a box whose
//! options are not sound, and every option of every Codex box.
//!
//! **Which program.** The box is read by the reader for the session's
//! PROGRAM ([`aterm_phase::read`]), and only on a reading whose phase is the
//! reader's evidence ([`Reading::phase_authoritative`]): a shell showing a
//! captured box, a pager — no agent's box, never pressed. The four safe rules were measured
//! on Claude Code's boxes and judge only those: under `"safe"` another
//! agent's box is escalated.
//!
//! **The press.** An approval carries the guard to press it under
//! ([`super::guard::row_guard`] of the judged row: a Bash or PowerShell
//! box's first command row, a file box's path row — the question row for
//! `Edit notebook` and the IDE diff, whose file only the question names —
//! the trust dialog's folder, any other box's title row), so a box swapped
//! between this read and the press is not answered on this verdict; the
//! loop adds the screen-generation fence where the server has one. Under the
//! safe rules the box must also offer a refusal — a [`Role::Deny`] option;
//! the trust dialog's refusal is its own (`No, exit`) — and wherever a box
//! has a `Do you want …` question no option-shaped row may sit above it. A
//! digit is never pressed while the focus is on an open `No, and …` input
//! ([`amend_input_open`]): the Select hands every key but the arrows to that
//! input, so the digit would be typed into it.
//!
//! **THE DECLINE** ([`Decision::Decline`], built by [`decline`]): the third
//! answer beside approving and escalating — the box refused WITH A REASON the
//! worker acts on, never handed to a person (the owner's rule of 2026-09-24:
//! "assume the user NEVER understands these complex requests for approval and
//! asking the user is just shirking responsibility"). Measured on Claude Code
//! 2.1.282 (the atpkg store build 1000002000001000282; the same code in
//! 2.1.280 and 2.1.281), read from the binary: a bare `No` — the digit `2`,
//! or Enter on it — hands the worker `The user doesn't want to proceed with
//! this tool use. … STOP what you are doing and wait for the user to tell you
//! how to proceed.` and ABORTS its turn (`cancelAndAbort` at byte 200112065:
//! the message is `Iw` (184644241) when no feedback came, and
//! `abortController.abort()` runs when `Dhn` (200110945) finds no feedback, no
//! content and no subagent) — a wait on a person. The same `No` WITH TEXT
//! hands it `rP` (184644476) — `… To tell you how to proceed, the user
//! said:\n<text>` — and aborts nothing: the worker reads the text and goes on
//! in the same turn. The text goes in through Claude Code's own `Tab to
//! amend`: Tab toggles the input of the FOCUSED option (the Select's key
//! handler, 189911253), and the Bash box's refusal is `{type:"input",
//! label:"No", placeholder:"and tell Claude what to do differently",
//! allowEmptySubmitToCancel:!0}` (`p2e`, 201506239); its answer is
//! `{behavior:"deny", feedback}` (`PB`, 201507619), carried to
//! `cancelAndAbort(S.feedback, …)` (200158251). Measured live the same day in
//! a private headless aterm (fixtures `cap-decline-*.txt`): the focus moved to
//! `❯ 2. No`, Tab drew `❯ 2. No, and tell Claude what to do differently`
//! (anchor `prompt.amend_no`) over the footer `Esc to cancel` alone, the typed
//! text replaced the placeholder (wrapped at column 10), and Enter gave the
//! worker's transcript that `rP` message with the text — it answered in the
//! same turn — where a bare `2` on the same box gave `Interrupted · What
//! should Claude do instead?`. So a decline is: the focus onto the refusal,
//! Tab, the text ([`decline_text`], opening [`DECLINE_PREFIX`]), Enter — each
//! keystroke from a fresh read of the same box, fenced on that read's
//! generation and guarded on the row that shows the state it answers
//! ([`decline_step`]), one in flight at a time and never written twice (the
//! loop's press: Tab TOGGLES the input, so a second Tab behind one not yet
//! drawn shuts it under the text); never the digit, which on the refusal is
//! the bare `No`, and never Enter on the open, EMPTY input, which is that
//! same `No` (`allowEmptySubmitToCancel`, 189912662). A box with no plain
//! `No` or no `Tab to amend` offers no way to give the worker a reason, and
//! is escalated instead ([`decline`]).
//!
//!
//! **A BOX TALLER THAN THE SCREEN, UNDER `approve = "safe"`** ([`RULE_TALL_BOX`],
//! [`tall_box`]). Measured 2026-09-25 (Claude Code 2.1.282/2.1.283, a live
//! headless aterm): Claude Code draws a box taller than the screen in ONE
//! frame, so its head never reaches the terminal or the alt-screen archive
//! (reading the archive over it was tried and dropped: it glued another
//! box's head on and approved a real `rm -rf $HOME/$1`); the vendor's
//! session record says `waitingFor: "permission prompt"` while the
//! transcript does not yet hold the pending call; and 2.1.283 denies an
//! unanswered box itself after about two minutes ("to avoid blocking
//! progress on an unattended session"). Full power answers it by its
//! options all the same ("A box taller than the pane", above). The safe
//! rules cannot prove what they cannot read — but a person handed it could
//! no more read its command than the harness can, and what IS on the screen
//! is its foot, where the vendor's note sits ([`PromptV2::foot_note`]). So
//! under `approve = "safe"`, in a bypass session (asked about nothing but
//! the vendor's circuit breakers), a box whose foot flags a removal is
//! declined with that note while the rm rule has its scratch roots: the
//! worker runs the flagged part as a short command — its paths written
//! literally, since a command run on its own keeps none of the line's
//! variables — which the rules then read whole and answer. Out of bypass,
//! with no removal note or the rm rule's roots taken away, under `approve =
//! "none"`, or on a foot that is no `Yes`/`No` permission box's, it is
//! handed over.
//!
//! **Git reads** (owner ruling, 2026-09-25). Git reads honour the
//! repository's config and `.gitattributes` (`core.fsmonitor`,
//! `diff.external`, a textconv driver), and a worker in accept-edits mode can
//! write both inside its cwd. So under the safe rules a line the classifier
//! reads as read-only is approved by the read-only rule (and the rm rule)
//! only when no git read on it would load configuration that runs a program:
//! [`super::git_config`] asks the worker's own git, in the worker's own
//! environment ([`ApprovalCtx::worker`]), for the effective configuration in
//! every directory the line's git reads may run in and escalates naming the
//! key — which full power ([`Approve::All`]) then answers all the same, the
//! key kept as its `unproven`. Not a blanket escalation — that would
//! interrupt the person on every `git status` under `approve = "safe"`. Two
//! refinements decided 2026-09-26 (owner's standing direction): a pager is
//! exempt, and a filter driver counts only where the attributes select it
//! ([`super::git_config`]).
//!
//! Pure but for two looks at the filesystem, both at where a path's
//! symlinks lead — the Read rule's, and the rm rule's
//! ([`super::rm_breaker`], "On the disk"; it runs for every rm breaker it
//! resolves, under full power too, where it only names the rule) — and the
//! git rule's `git config` in the directories a git read runs in
//! ([`ApprovalCtx::worker`]): no socket, no clock. The caller supplies the
//! screen rows, the [`Reading`] of them and an [`ApprovalCtx`] (the
//! session's cwd, the permission mode its footer last showed, the approve
//! level and roots).

use std::path::{Path, PathBuf};

use aterm_phase::prompt::{
    Opt, PromptKind, PromptV2, RmBreaker, RmBreakerKind, RmCommand, Role, Select, rm_breaker_of,
};
use aterm_phase::{Phase, Program, Reading, anchor};

use super::super::codex_usage::LimitRead;
use super::super::config::Approve;
use super::git_config::{GitView, WorkerEnv, git_reads, hazard};
use super::guard::row_guard;
use super::rm_breaker::{RmScope, ScratchRoot, abs_components, clip, resolve_rm_line};
use super::shell_startup::ShellStartup;
use super::turn_end::CodexSetting;
use crate::supervise::classify::{classify_command_with, classify_except_rm, glob_match};

/// The read-only Bash rule's id (the classifier generation it trusts).
pub const RULE_READ_ONLY: &str = "read-only@classify-v3";
/// The rm circuit-breaker rule's id.
pub const RULE_RM_BREAKER: &str = "rm-breaker@v3";
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
/// Full power: a box with no yes it may take (a standing grant, a purchase)
/// and nothing that waits, answered with the refusal that settles nothing.
pub const RULE_DECLINE: &str = "decline@v1";
/// Full power: Claude Code's model-refusal pause (`Session paused`) ALONE,
/// its `Switch to <model>` while `[harness] model_fallback` is set
/// ([`ApprovalCtx::model_fallback`], [`refusal_pause_switch`]) — the switch
/// that setting approves for a model's limit. No other box's switch is ever
/// pressed under it.
pub const RULE_MODEL_SWITCH: &str = "model-switch@v1";
/// Full power, `[harness] rate_nudge`: Codex's rate-limit nudge, its `Switch
/// to <model>`, pressed only to SAVE THE WORK when Codex's usage window is
/// near its limit ([`rate_nudge`]). Its approved row opens the switch the
/// turn-end policy winds down, restores and holds.
pub const RULE_RATE_NUDGE_SWITCH: &str = "rate-nudge-switch@v1";
/// Full power, `[harness] rate_nudge`: Codex's rate-limit nudge, its `Keep
/// current model` — a stale nudge, one while a switch is open, one no usage
/// reading vouches for ([`rate_nudge`]). Never `… (never show again)`.
pub const RULE_RATE_NUDGE_KEEP: &str = "rate-nudge-keep@v1";
/// Full power: the harness's OWN restore of a Codex thread's model after a
/// save-then-wait switch — the `/model` picker's exact original model and
/// effort, the effort for this conversation only ([`model_restore_pick`]).
pub const RULE_MODEL_RESTORE_PICK: &str = "model-restore-pick@v1";
/// Full power: the LIVE UPGRADE'S OWN resume of a Codex goal it paused for
/// its move — the paused goal's box the relaunched Codex opens with, its
/// focused first option, the resume ([`goal_resume_pick`]). Never its other
/// option, and never a box a person's own `codex resume` drew.
pub const RULE_GOAL_RESUME: &str = "goal-resume@v1";
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

/// What every decline's text opens with ([`decline_text`]): Claude Code hands
/// the text to the worker as the words of the person at the box (`To tell you
/// how to proceed, the user said:`), so it names who said them.
pub const DECLINE_PREFIX: &str = "aterm harness (not the user): ";

/// The refusal a decline amends: the Bash box's `No`, the option Claude Code
/// makes an input under Tab (module header, "THE DECLINE").
pub const REFUSAL_LABEL: &str = "No";

/// How much of a reason, or of a quoted command, a decline's text carries, in
/// characters: the text is typed into the box and read back before Enter, so
/// it stays a few rows.
const DECLINE_WHY_CHARS: usize = 160;

/// How the approving option is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Press this digit: a numbered box ([`Select::Digits`]).
    Digit(u8),
    /// Move the focus `steps` options (down when positive, up when
    /// negative), then press Enter once a fresh read shows it on the option
    /// labelled `label` — an unnumbered dialog ([`Select::ArrowsEnter`]):
    /// the folder-trust dialog, and under full power any unnumbered box (the
    /// `Tool use` box whose vendor default is `No`, Codex's folder gate) —
    /// and the usage-limit dialog's wait row, numbered but never chosen by
    /// its digit ([`limit_wait`]). `steps` 0: the focus is on it already.
    Focus { steps: i32, label: String },
    /// A question dialog's answer ([`super::question::RULE_ANSWER_RECOMMENDED`]):
    /// every key under the screen-generation fence, never unfenced, and
    /// never a digit.
    Answer(Answer),
    /// Move the focus as [`Self::Focus`] does, then press `key` — not Enter
    /// — once a fresh read shows it on `label`: Codex's effort boxes' `s`,
    /// which applies the focused effort to this conversation only, where
    /// Enter would save it as the default for every new session (the
    /// footer's `enter default · s session`, measured 0.158.0). Never a
    /// digit there: a digit chooses as Enter does.
    FocusKey {
        steps: i32,
        label: String,
        key: &'static str,
    },
}

/// How a question dialog is answered ([`Choice::Answer`]). ONE form, on
/// purpose: Enter on the focused row, after moving the focus there. A digit
/// is never sent for a question (the critique of 2026-09-25, R4): a digit
/// that lands late does harm wherever it lands — on the next tab it answers
/// that option, on the review tab `2` is Cancel (which rejects the tool
/// call, measured), and in the composer after a one-question submit it
/// leaves a draft that withholds the next dialog — where a late Enter lands
/// on the next tab's vendor focus (option 1, where the tool asks the model
/// to put its recommendation), on the review's focused `1. Submit answers`,
/// or in an empty composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Move the focus `steps` rows in the dialog's focus order (down when
    /// positive; the options, then the free-text row, then the multi-select
    /// button, then the chat row; the review's two rows) — one key at a
    /// time, each seen landing on a fresh read before the next — then Enter
    /// once a fresh read shows the focus on `target`, guarded on the focused
    /// row as drawn. `steps` is 0 in the common case: the vendor opens a tab
    /// on option 1 and the review on `1. Submit answers`.
    FocusEnter { steps: i32, target: AnswerTarget },
}

/// Where a question answer's Enter goes ([`Answer::FocusEnter`]): never the
/// free-text row, the chat row or the review's cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerTarget {
    /// The model's own option `n` (single-select and preview: answers it;
    /// multi-select: toggles its checkbox).
    Option(u8),
    /// The multi-select button (`Next`, or `Submit` on the last question):
    /// the checks made, moves on.
    Button,
    /// The review tab's `1. Submit answers`.
    ReviewSubmit,
}

/// What to do with one box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Choose `choice` under `guard`.
    Approve {
        /// Which rule approved ([`RULE_READ_ONLY`], [`RULE_RM_BREAKER`],
        /// [`RULE_READ_OUTSIDE_CWD`], [`RULE_TRUST_DIALOG`], one of full
        /// power's, [`RULE_LIMIT_WAIT`], or a question's
        /// [`super::question::RULE_ANSWER_RECOMMENDED`]).
        rule_id: &'static str,
        /// The option, and how it is chosen.
        choice: Choice,
        /// The anchored `key if=` pattern of the judged row
        /// ([`super::guard::row_guard`]): the command's first row, the
        /// path's, the trust dialog's folder, the usage-limit dialog's
        /// title.
        guard: String,
        /// What was judged: the command or the path; for the rm rule, the
        /// resolved targets follow after ` => `; for a trust dialog's
        /// backstop, its warnings in parentheses; under full power, which
        /// judges no command, the box's subject as shown (its command, else
        /// its title) and ` => ` the option chosen. What the ledger row, the
        /// notes and the journal's `APPROVED` line name.
        subject: String,
        /// Under full power, why the safe rules did not prove the box (their
        /// escalation reason, the rm/rmdir breaker's kind first when the box
        /// is one, and `vendor default was No` for an unnumbered box that
        /// lists its refusal first or whose focus the vendor put on it): the
        /// ledger row's and the journal's `unproven:`. `None` for a proven
        /// approval.
        unproven: Option<String>,
    },
    /// Refuse the box WITH A REASON the worker acts on (module header, "THE
    /// DECLINE"): its `No`, amended with `text`, so the worker is told why
    /// and goes on by itself — never the bare `No`, which stops it to wait
    /// for a person. Built only by [`decline`]; carried out by the loop one
    /// keystroke at a time, each chosen by [`decline_step`] from a fresh read
    /// of the same box.
    Decline {
        /// The rule that declined.
        rule_id: &'static str,
        /// The refusal's index in the box's options ([`refusal_of`]).
        refusal: usize,
        /// What the worker is told ([`decline_text`]).
        text: String,
        /// What was judged: what the ledger row, the notes and the journal's
        /// `DECLINED` line name.
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
    /// What may be answered (`[harness] approve`).
    pub approve: Approve,
    /// Whether a question dialog is answered
    /// ([`super::question::answer_question`], `[harness] answer_questions`),
    /// whatever [`Self::approve`] says: off, it is a person's, as a question
    /// in prose is.
    pub answer_questions: bool,
    /// Whether full power may switch the session's model on Claude Code's
    /// model-refusal pause ([`RULE_MODEL_SWITCH`]): `[harness]
    /// model_fallback` set. Written empty, the pause is a person's.
    pub model_fallback: bool,
    /// What answers Codex's rate-limit nudge ([`rate_nudge`]).
    pub nudge: NudgeCtx,
    /// The Codex model and effort the harness's own restore is putting back,
    /// while it is in flight (`TurnEndState::restore_target`): the only time
    /// the `/model` picker is answered ([`model_restore_pick`]).
    pub model_restore: Option<CodexSetting>,
    /// The LIVE UPGRADE owes this tab's Codex goal its resume — it paused
    /// the goal for its move (the tab's goal record beside the loop's
    /// ledger, `harness::goal_hold`): the only time the paused goal's box is
    /// answered ([`goal_resume_pick`]).
    pub goal_resume: bool,
    /// Whether the usage-limit dialog's wait row is chosen
    /// ([`RULE_LIMIT_WAIT`], `[harness] limit_wait`), whatever
    /// [`Self::approve`] says: off, the dialog is a person's.
    pub limit_wait: bool,
    /// Where a folder-trust dialog's folder may be trusted
    /// ([`RULE_TRUST_DIALOG`]).
    pub trust_roots: Vec<ScratchRoot>,
    /// Where a Read box may read ([`RULE_READ_OUTSIDE_CWD`]): the trust
    /// roots and the system roots ([`Self::new`]).
    pub read_roots: Vec<ScratchRoot>,
    /// Where an rm operand may point ([`RULE_RM_BREAKER`]). Empty is the
    /// rule withheld (the loop clears it for the retired `rm_breaker =
    /// false`), and then no rm is proven at all ([`RmScope::roots`]).
    pub scratch_roots: Vec<ScratchRoot>,
    /// What a Read box may not be approved for, under an allowed root.
    pub secrets: Vec<SecretRule>,
    /// The `python3 <script>` globs the classifier treats as reads (none by
    /// default).
    pub python_allow: Vec<String>,
    /// The worker's environment ([`WorkerEnv`]), which a git read's
    /// configuration is read with ([`super::git_config`]) — or why it could
    /// not be read, which every git read then escalates with. Unread until
    /// the loop reads it for a box that may run git ([`worker_unread`]).
    pub worker: Result<WorkerEnv, String>,
    /// Where the session's Bash tool may stand beyond [`Self::cwd`], as Claude
    /// Code's transcripts last recorded it
    /// ([`crate::harness::footer::shell_cwds`]): a git read is checked there
    /// too. Empty until the loop reads it for a box that may run git.
    pub shell_cwds: Vec<PathBuf>,
    /// What the worker's Bash tool's shell starts with beyond the defaults
    /// the rm rule and the classifier model ([`super::shell_startup::read`]):
    /// the aliases and functions that fail the lines naming them — or the
    /// option, variable or file that fails every Bash line, or why the
    /// startup could not be read. Unread until the loop reads it for a Bash
    /// box ([`shell_unread`]).
    pub shell: Result<ShellStartup, String>,
}

impl ApprovalCtx {
    /// The defaults for a session in `cwd`, owned by `uid` — full power
    /// ([`Approve::All`], the owner's default), questions answered, the
    /// usage-limit dialog's wait chosen:
    /// scratch roots `/private/tmp/claude-<uid>/`, `tmpdir` (this process's
    /// `$TMPDIR`: a root a literal path may sit in, never what `$TMPDIR`
    /// means on the worker's line; when it is at least two levels deep and
    /// holds neither `cwd` nor `home`), any
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
            approve: Approve::All,
            answer_questions: true,
            model_fallback: true,
            nudge: NudgeCtx::default(),
            model_restore: None,
            goal_resume: false,
            limit_wait: true,
            trust_roots: Vec::new(),
            read_roots: Vec::new(),
            scratch_roots,
            python_allow: Vec::new(),
            worker: worker_unread(),
            shell_cwds: Vec::new(),
            shell: shell_unread(),
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

/// What [`ApprovalCtx::worker`] holds before the loop reads the worker's
/// environment: nothing, so a git read escalates — but in this crate's own
/// unit-test build a hermetic stand-in ([`WorkerEnv::hermetic`]), so no
/// test's verdict depends on the developer's git config.
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

/// What [`ApprovalCtx::shell`] holds before the loop reads the worker's
/// shell startup: nothing, so no Bash line is proven or read-only — but in
/// this crate's own unit-test build the defaults the line models assume, so
/// no test's verdict depends on the developer's shell or live Claude Code
/// sessions (a test that means the startup sets it).
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

/// `Ok` when the worker's Bash tool's shell starts as the rm rule and the
/// classifier model it for every reading ([`ApprovalCtx::shell`],
/// [`ShellStartup::admits`]); `Err` names the option, alias, function,
/// variable or file that makes them prove nothing, or why the startup is
/// unknown. A box that runs on another machine (`runs_on`) has that
/// machine's startup, which this one cannot read.
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
        if t.contains(aterm_phase::anchor("footer.shortcuts")) {
            return Some(FooterMode::Default);
        }
    }
    None
}

/// Decide the box `reading` found on the screen `rows`, under
/// [`ApprovalCtx::approve`] (module header): the safe rules, and under
/// [`Approve::All`] full power's answer to what they leave.
///
/// `rows` are required, not only the reading: a press is guarded on the
/// judged row as drawn, and the rows around the box are cross-checked.
pub fn decide(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    // The usage-limit dialog is no permission and no question (module
    // header): its one rule answers it under its own key, FIRST — before the
    // question rule and before any level, so full power (whose no-spend
    // answer is `Stop and wait …`, and whose model switch once read `Switch
    // to usage credits` as one) never sees it.
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::UsageLimit
    {
        return if reading.program != Program::Claude
            || reading.phase != Phase::Prompt
            || !reading.phase_authoritative
        {
            Decision::escalate("no usage-limit dialog the reader vouches for")
        } else {
            limit_wait(prompt, rows, ctx)
        };
    }
    // A question is no permission (module header): answered under its own
    // key, before any level is consulted.
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::Question
    {
        return if reading.phase != Phase::Prompt || !reading.phase_authoritative {
            Decision::escalate("no box the reader vouches for")
        } else if ctx.answer_questions {
            super::question::answer_question(prompt, rows)
        } else {
            Decision::escalate("answer_questions is off: a question is the person's to answer")
        };
    }
    // The owner's own Claude Code configuration sends this box to a person
    // (an ask rule, a hook: its reason block): no level presses it — full
    // power only takes power the owner left on (owner directive of
    // 2026-09-25). Built-in safety checks are not the owner's settings and
    // stay the levels' to answer.
    if let Some(review) = reading.prompt.as_ref().and_then(|p| p.owner_review()) {
        return Decision::escalate(format!(
            "the owner's Claude Code settings send this box to a person ({}): {}",
            review.kind.words(),
            review.text
        ));
    }
    if let Some(why) = head_unread(reading) {
        return match ctx.approve {
            Approve::All => unread_head(reading, rows, why),
            Approve::Safe => tall_box(reading, rows, ctx, why),
            Approve::None => Decision::escalate(why),
        };
    }
    match ctx.approve {
        Approve::None => Decision::escalate("approve = \"none\": every box is the owner's"),
        Approve::Safe => decide_safe(reading, rows, ctx),
        Approve::All => match decide_safe(reading, rows, ctx) {
            answered @ (Decision::Approve { .. } | Decision::Decline { .. }) => answered,
            Decision::Escalate { reason } => full_power(reading, rows, ctx, reason),
        },
    }
}

/// Why the box's title is not what the reader found, when it is not: a box
/// taller than the pane (aterm-phase reads it so rather than idle, the
/// harness round-3 review of 2026-09-24), or a box of no kind whose first
/// row is its own question — the walk up stopped inside its body (a `⎿`
/// row in a Bash command), so its title is not what the reader found (the
/// same review: it was pressed under the generic question row as a proven
/// box, its command unaudited).
fn head_unread(reading: &Reading) -> Option<String> {
    let p = reading.prompt.as_ref()?;
    if p.head_off_screen {
        Some(format!(
            "the box's title row is not on the screen (a box taller than the pane): {}",
            p.title
        ))
    } else if p.kind == PromptKind::Other && p.title.starts_with("Do you want") {
        Some(format!(
            "the box's title row is not on the screen (its first row is its question): {}",
            p.title
        ))
    } else {
        None
    }
}

/// Full power's answer to a box whose head it could not read (`why`,
/// [`head_unread`]; module header, "A box taller than the pane"): no kind
/// to judge, so its options' roles decide — a plan's yes by [`plan_pick`]
/// when the options are a plan approval's, else [`allow_once`] — and the
/// missing title rides in `unproven`. (A question never gets here: its
/// reader says whether its dialog was read whole, [`decide`].)
fn unread_head(reading: &Reading, rows: &[String], why: String) -> Decision {
    if reading.phase != Phase::Prompt || !reading.phase_authoritative {
        return Decision::escalate(why);
    }
    let Some(p) = reading.prompt.as_ref() else {
        return Decision::escalate(why);
    };
    let plan = p.options.iter().any(|o| {
        let l = o.label.to_lowercase();
        PLAN_WORDS.iter().any(|w| l.contains(w))
    });
    let pick = if plan { plan_pick(p) } else { allow_once(p) };
    answer(p, rows, pick, why)
}

/// What a declined box taller than the screen is told of itself.
const TALL_WHY: &str =
    "it is taller than the screen, so the harness cannot read all of it to check it";

/// How a declined tall box's flagged removal is to be run on its own: the
/// vendor flagged it because a variable in it may be EMPTY, and the
/// invocation its note quotes (`rm -rf $S/$1`) leans on variables the line
/// around it binds (`S=…; for p …; set -- $p; …`). Claude Code's Bash tool
/// keeps no shell state from one call to the next (its own tool description:
/// "Shell state (env vars, functions) does not persist"), so a command run
/// on its own keeps none of them: run as quoted it expands to `rm -rf /`,
/// and with `S` alone re-bound to `rm -rf <S>/` — the whole directory (the
/// review of 2026-09-25, F2b). So its paths go in literally, the vendor's
/// own remedy ("or use a literal path"); the rm rule then reads the short
/// box whole and judges its operands itself.
const TALL_RM_PATHS: &str = "its paths written as literal absolute paths (a command run on its \
                             own keeps none of this one's shell variables)";

/// THE SAFE RULES' ANSWER TO A BOX WHOSE HEAD IS OFF THE SCREEN (`why`,
/// [`head_unread`]; module header, "A box taller than the screen, under
/// `approve = \"safe\"`"): no rule can APPROVE what it cannot read, but a
/// person handed it could no more read its command than the harness can.
/// In a bypass session (asked about nothing but the vendor's circuit
/// breakers), a box whose foot note ([`PromptV2::foot_note`]) flags a
/// removal, while the rm rule has its scratch roots, on a foot that is a
/// permission box's `Yes`/`No` under `Do you want to proceed?`
/// ([`yes_no_to_proceed`]), is DECLINED ([`decline`], [`RULE_TALL_BOX`]):
/// the worker is told to run the invocation the note quotes on its own as a
/// short command, its paths written literally ([`TALL_RM_PATHS`]) — a box
/// the rules then read whole and answer. A decline runs nothing, so it
/// needs no proof about the command. Everything else, a question among it,
/// is handed over with `why` (and why it was not declined, where that is
/// the reason).
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
    if reading.program != Program::Claude
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
    decline(RULE_TALL_BOX, prompt, rows, text, subject).unwrap_or_else(|not| handed_over(Some(not)))
}

/// `Ok` when a box's foot is a permission box's: its question `Do you want
/// to proceed?` (anchor `prompt.proceed`) and its options exactly a one-shot
/// allow and a refusal, in that order — the Bash box's `1. Yes` / `2. No`,
/// with a decline under way too (the refusal's input open or filled).
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
/// or an escalation.
fn decide_safe(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    if reading.program != Program::Claude {
        return Decision::escalate(format!(
            "a {} session's box: the safe rules judge Claude Code's boxes",
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

/// The one plain one-shot allow of a numbered box — what the safe rules
/// press: exactly one option with [`Role::Once`], a number, and a refusing
/// option ([`Role::Deny`]) beside it — never while the focus is on an open
/// amend input ([`amend_input_open`]).
fn once_choice(prompt: &PromptV2, rows: &[String]) -> Result<Choice, String> {
    if prompt.select != Select::Digits {
        return Err("an unnumbered box".to_string());
    }
    if amend_input_open(prompt, rows) {
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

/// Whether the focus is on the refusal's amend input, OPEN: a `No, …` label
/// — the placeholder (`No, and tell Claude what to do differently`: a
/// decline under way, or a person's Tab) or text typed into it (`No,
/// <text>`) — over a footer that reads `Esc to cancel` ALONE, its `Tab to
/// amend` hint gone (measured on 2.1.282, `cap-decline-3`/`-4`). Claude
/// Code's Select hands every key but the arrows to that input (module
/// header, "THE DECLINE"), so a digit pressed now is typed into it, never
/// chosen. A FIXED refusal label is no input: the footerless Fetch and
/// network boxes' `No, and tell Claude what to do differently (esc)` and
/// the Chrome box's `Deny (esc)` draw no footer, and a digit there chooses
/// as it always does (the review of 2026-09-25, F2).
fn amend_input_open(prompt: &PromptV2, rows: &[String]) -> bool {
    let amended = format!("{REFUSAL_LABEL}, ");
    let footer_alone = rows
        .get(prompt.span.1)
        .is_some_and(|f| f.trim() == anchor("prompt.cancel"));
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
    // Every header form aterm-phase's grammar reads (module header): an
    // origin, `(unsandboxed)`, `(runs on <m>)`. A suffix it cannot read is
    // no Bash header.
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
        // The scratch-root proof is this machine's (its cwd, `$TMPDIR`,
        // roots): an rm that runs on another machine is not proven by it
        // (harness round-1 review, 2026-09-24).
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
        // After the line's own proof, as the read-only rule orders it: a
        // line the proof refuses is refused on where it points, and one it
        // proves still needs the shell startup the proof assumes.
        if let Err(why) = shell_models_hold(&readings, None, ctx) {
            return Decision::escalate(format!("rm circuit breaker: {why}"));
        }
        if let Err(why) = git_reads_clear(&readings, None, ctx) {
            return Decision::escalate(format!("rm circuit breaker: {why}"));
        }
        return match once_choice(prompt, rows) {
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
    if let Err(why) = git_reads_clear(&readings, prompt.runs_on().as_deref(), ctx) {
        return Decision::escalate(why);
    }
    match once_choice(prompt, rows) {
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
/// key, the hook, or why the check could not be made. A box that runs on
/// another machine (`runs_on`) has that machine's configuration, which this
/// one cannot read.
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
        worker.settings_env(&ctx.cwd)?;
        let writes = |dir: &Path| worker_writes(ctx, dir);
        for dir in &reads.dirs {
            if !seen.iter().any(|(d, _)| d == dir) {
                seen.push((dir.clone(), worker.view(dir, &writes)?));
            }
            let view = &seen.iter().find(|(d, _)| d == dir).expect("just seen").1;
            if let Some(why) = hazard(view, &reads) {
                return Err(why);
            }
        }
    }
    Ok(())
}

/// Whether the worker may write under `dir` without anyone approving it — so
/// a `git` there is the worker's to choose ([`WorkerEnv::view`]): the
/// session's cwd and every directory its Bash tool stands in (an accept-edits
/// worker writes there), and the scratch roots (temp directories, and where
/// the rm rule itself deletes). Judged on `dir` as written and as it
/// resolves, against the directories as written and as they resolve.
fn worker_writes(ctx: &ApprovalCtx, dir: &Path) -> bool {
    let resolved = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let forms = [dir.to_path_buf(), resolved(dir)];
    let bases: Vec<PathBuf> = std::iter::once(&ctx.cwd)
        .chain(&ctx.shell_cwds)
        .flat_map(|b| [b.clone(), resolved(b)])
        .collect();
    forms.iter().any(|d| {
        bases.iter().any(|b| d.starts_with(b))
            || abs_components(&d.to_string_lossy())
                .is_some_and(|comps| ctx.scratch_roots.iter().any(|r| r.holds(&comps)))
    })
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

/// `Ok` when [`RULE_RM_BREAKER`]'s resolver may judge this breaker: an `rm`
/// on a possibly-empty variable (either spelling of
/// [`RmBreakerKind::EmptyVariable`]) — the form the resolver was written and
/// measured for — whose note is read whole: aterm-phase has read it as ONE
/// breaker (one note, one `Dangerous`), and its advice closes its
/// parentheses (`… (bind $1 and rewrite its $S as "${S:?}" or use a literal
/// path)`), so no second warning hides in a block cut short. `Err` names
/// the breaker's kind and why not.
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

/// The vendor's rm/rmdir circuit breaker as a reason names it: the command
/// and why the vendor stopped it, in the note's own words less their `on `
/// and colon (`the rm circuit breaker (statically-unresolvable target)`).
/// The words are aterm-phase's anchors, so a vendor rewording moves this
/// with the reader.
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

/// Decision 1's escalation `reason` for a breaker box, with the breaker's
/// kind named ONCE (D2): unchanged when it already carries
/// [`rm_breaker_label`]; the label in place of its own leading "the rm
/// circuit breaker" / "an rm circuit breaker" / "rm circuit breaker" (rmdir
/// alike) — "the rm circuit breaker (possibly-empty variable path) outside
/// a bypass session", where prefixing it read "…: the rm circuit breaker
/// outside …" (the harness round-2 review of 2026-09-24, minor); the label
/// and a colon in front of any other reason.
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

/// What a decline tells the worker ([`Decision::Decline`]):
/// [`DECLINE_PREFIX`], that `what` was not run and why (cut at
/// [`DECLINE_WHY_CHARS`]), and `fix` — what to do instead. One line of
/// printable text: a control character, a tab (Tab would close the input) or
/// a newline (Enter would submit it) is a space.
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

/// THE DECLINE's one constructor (module header): `text` typed into the
/// box's refusal ([`refusal_of`]) under `rule_id`, naming `subject` — or
/// `Err` with why not, where the box offers no refusal to amend (no plain
/// `No`, no `Tab to amend`, two of them); the caller escalates then. A box
/// read with a decline already under way (the focus on the refusal, its
/// input open or holding `text`) is the same decline, so a loop that
/// stopped part-way picks up where it stopped.
pub fn decline(
    rule_id: &'static str,
    prompt: &PromptV2,
    rows: &[String],
    text: String,
    subject: String,
) -> Result<Decision, String> {
    let refusal = refusal_of(prompt, rows, &text)?;
    Ok(Decision::Decline {
        rule_id,
        refusal,
        text,
        subject,
    })
}

/// `text` with every whitespace character dropped: rows, however Claude Code
/// wrapped them (between words, or inside one too long for the width), read
/// back as the text that was written — an option's label here, a composer's
/// draft in the turn-end loop (`turn_end_loop.rs`).
pub(crate) fn squashed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The refusal as Claude Code draws it with its input OPEN and empty: `No,
/// and tell Claude what to do differently` (the label, `, `, the
/// placeholder; measured on 2.1.282).
fn amend_open_label() -> String {
    format!("{REFUSAL_LABEL}, {}", anchor("prompt.amend_no"))
}

/// The refusal with `text` typed into its input: `No, <text>` (measured).
fn amended_label(text: &str) -> String {
    format!("{REFUSAL_LABEL}, {text}")
}

/// Whether the box's footer offers `Tab to amend` (Claude Code draws it
/// while the focused option is an allow, or a refusal whose input is shut).
fn offers_amend(prompt: &PromptV2, rows: &[String]) -> bool {
    let hint = anchor("prompt.amend");
    rows.get(prompt.span.1)
        .is_some_and(|f| f.trim().split(" · ").any(|h| h == hint))
}

/// The option a decline amends: the box's one [`Role::Deny`] option that is
/// the plain `No` ([`REFUSAL_LABEL`]) — on a footer that offers `Tab to
/// amend` — or, a decline under way, the focused refusal whose input is
/// open ([`amend_open_label`]) or holds `text` ([`amended_label`]).
fn refusal_of(prompt: &PromptV2, rows: &[String], text: &str) -> Result<usize, String> {
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
    if prompt.options[k].label == REFUSAL_LABEL && !offers_amend(prompt, rows) {
        return Err(format!(
            "the box does not offer `{}`",
            anchor("prompt.amend")
        ));
    }
    Ok(k)
}

/// The next keystroke of a decline ([`decline_step`]), with the anchored
/// guard of the row that shows the state it answers
/// ([`super::guard::row_guard`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclineStep {
    /// `down` (or `up`) toward the refusal, guarded on the focused row.
    Move { down: bool, guard: String },
    /// `tab` on the focused `No`: its input opens.
    Amend { guard: String },
    /// The text, typed into the open, EMPTY input.
    Type { guard: String },
    /// `enter` on the input that shows the text: the refusal, with it.
    Submit { guard: String },
}

/// Where a decline of option `refusal` stands on a fresh read of the same
/// box, and so its next keystroke: the focus elsewhere, onto the refusal;
/// on the shut `No` (the footer offering `Tab to amend`), Tab; on the open,
/// empty input, the text; on the input showing exactly the text, Enter.
/// `Err` for anything else — the refusal gone or relabelled, the focus
/// elsewhere with the input open, text in the input that is not `text` —
/// where no keystroke is sound.
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
        if !offers_amend(prompt, rows) {
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
    // The rule's proof is LOCAL — where the path's symlinks lead on this
    // machine, the secrets under this `$HOME` — so a Read that runs on
    // another machine is not proven by it (harness round-1 review,
    // 2026-09-24: `Read file (runs on m3)` was approved by the local look).
    if let Some(machine) = prompt.runs_on() {
        return Decision::escalate(format!(
            "a Read on {machine}: its path is not on this machine"
        ));
    }
    let Some(path) = prompt.path.as_deref() else {
        return Decision::escalate("a Read box with no path");
    };
    let Some(path_row) = row_of(rows, prompt, path) else {
        return Decision::escalate("the Read path's row is not on the screen");
    };
    let choice = match once_choice(prompt, rows) {
        Ok(c) => c,
        Err(why) => return Decision::escalate(why),
    };
    match read_path_allowed(path, ctx) {
        Ok(()) => Decision::Approve {
            rule_id: RULE_READ_OUTSIDE_CWD,
            choice,
            guard: row_guard(&rows[path_row]),
            subject: path.to_string(),
            unproven: None,
        },
        Err(why) => Decision::escalate(why),
    }
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
/// where it is to the trust option (or presses its digit, should the dialog
/// be numbered).
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
    if !ctx.trust_roots.iter().any(|r| r.holds(&comps)) {
        return Decision::escalate(format!(
            "a folder-trust dialog for {path}, under no trust root"
        ));
    }
    let roles: Vec<Role> = prompt.options.iter().map(|o| o.role).collect();
    if roles.len() != 2 || !roles.contains(&Role::Trust) || !roles.contains(&Role::Exit) {
        return Decision::escalate(format!(
            "a folder-trust dialog whose options are not exactly `{}` and `{}`",
            anchor("trust.no"),
            anchor("trust.yes")
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

/// The trust backstop's `⚠` warnings in `body`, each with the rows listed
/// under it — the tool permissions (`Bash(git:*)`), the directories, the
/// headersHelper the folder's settings carry, which the dialog's Yes makes
/// "apply without asking" in this and every later session in the folder
/// (the vendor's words): `This folder pre-approves 3 tool permissions in
/// .claude/settings.json: Bash(git:*)`. The ledger subject names what the
/// press accepted, not just the headline (the harness final review r3 of
/// 2026-09-24, major).
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

/// The words a PURCHASE opens with ([`buys`]), lowercased, after a leading
/// `yes, ` (the 2.1.281 binary's `Yes, buy usage credits`, `Buy more`, `Add
/// funds to continue with …`, `Upgrade your plan`, `Adjust monthly limit`,
/// `Set to unlimited`, `Set monthly spend limit`) — and turning usage
/// credits back ON (`Yes, re-enable and continue`, `Turn on usage credits`):
/// the 2.1.282 binary draws that yes exactly when the account holder turned
/// the credits off and a balance remains (`Usage credits are turned off.
/// Re-enable to use`), so its yes is spend the account's own setting took
/// away (the hazards review of 2026-09-25). Moving the session onto usage
/// credits is spend too: the usage-limit dialog's `Switch to usage credits`
/// (the 2.1.282 `rate_limit_options_menu`'s `extra-usage` row), which opens
/// with `Switch to ` as the model-refusal pause's switch does and was read as
/// one ([`switches_model`]) — so it, and any option that opens by naming the
/// credits, is a purchase wherever it is drawn. Consent to go on with credits
/// already on (`Continue with …`) buys nothing and is none of them.
const PURCHASE: &[&str] = &[
    "re-enable",
    "turn on usage credits",
    "switch to usage credits",
    "usage credits",
    "buy",
    "purchase",
    "add funds",
    "upgrade",
    "subscribe",
    "adjust monthly limit",
    "set limit",
    "set to unlimited",
    "set monthly spend limit",
];

/// Whether choosing `opt` would buy something ([`PURCHASE`]): its label,
/// past a leading `yes, `, OPENS with one of those words as a whole word —
/// never a word further in, so a one-shot label that names a path
/// (`Yes, allow edits to billing/upgrade.rs`) is not a purchase.
#[must_use]
pub fn buys(opt: &Opt) -> bool {
    let l = opt.label.trim().to_lowercase();
    let l = l
        .strip_prefix("yes,")
        .or_else(|| l.strip_prefix("yes "))
        .map_or(l.as_str(), str::trim_start);
    PURCHASE.iter().any(|w| {
        l.strip_prefix(w)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
    })
}

/// Whether `opt` waits rather than goes on: `Stop and wait for limit to
/// reset`, `No, keep my current model`, `Stay on …` (the 2.1.281 binary's
/// labels for declining a purchase).
fn waits(opt: &Opt) -> bool {
    let l = opt.label.to_lowercase();
    ["stop and wait", "wait", "no, keep", "stay on", "stay "]
        .iter()
        .any(|w| l.starts_with(w))
}

/// Full power's answer ([`Approve::All`], module header) to a box the safe
/// rules left (`safe`: their reason — kept in an escalation, and in the
/// answer's `unproven`, the rm/rmdir breaker's kind named).
fn full_power(reading: &Reading, rows: &[String], ctx: &ApprovalCtx, safe: String) -> Decision {
    if reading.phase != Phase::Prompt || !reading.phase_authoritative {
        return Decision::escalate(safe);
    }
    let Some(p) = reading.prompt.as_ref() else {
        return Decision::escalate(safe);
    };
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
        // The persistent read-outside setting: its refusal that settles
        // nothing — the last `No` (`No, ask again next time`), never `Yes,
        // keep allowing …` nor `No, block … from now on`, each for good.
        PromptKind::ReadOutsideSetting => p
            .options
            .iter()
            .rev()
            .find(|o| o.role == Role::Deny)
            .map(|o| (RULE_DECLINE, o))
            .ok_or_else(|| "the read-outside setting with no `No`".to_string()),
        PromptKind::ModelSwitch => model_confirm(p, rows),
        PromptKind::RateNudge => {
            return match rate_nudge(p, reading.program, ctx) {
                Ok((rule, o, why)) => answer(p, rows, Ok((rule, o)), why),
                Err(why) => Decision::escalate(format!("{why} (the safe rules: {safe})")),
            };
        }
        PromptKind::ModelPick => return model_restore_pick(p, rows, reading.program, ctx),
        PromptKind::GoalResume => return goal_resume_pick(p, rows, reading.program, ctx),
        PromptKind::Other => other_dialog(reading.program, p, ctx),
        _ => allow_once(p),
    };
    answer(p, rows, pick, safe)
}

/// Full power's answer to the model-switch confirmation (aterm-phase's
/// [`PromptKind::ModelSwitch`]: ` Switch model?` / ` Change effort level?`).
/// Claude Code asks it before a `/model` or `/effort` change the PERSON typed
/// takes effect on a warm conversation — whether to pay one uncached re-read
/// of the history — so its `Yes, switch to <m>` carries out what they asked:
/// it grants no permission and buys nothing, and what it persists (`/model`
/// saves the person's default for new sessions) is what their own command
/// asked for. Pressed ([`RULE_MODEL_CONFIRM`]; owner direction of 2026-09-26,
/// "this should have been autoapproved") only when the box's subtitle — the
/// rows under its title down to the first blank one, joined, however a narrow
/// pane wrapped them — is exactly the cost warning (anchor
/// `model_switch.cost`). Anything else is handed over, failing closed: the
/// person's own PreModelSwitch hook asking for the confirmation (anchor
/// `model_switch.hook`, a confirmation being what that configuration asks
/// for), or a subtitle a later build reworded.
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

/// The press full power makes of `pick` on `p`: its choice, the guard of its
/// judged row, the subject and `unproven` (`safe`: why the safe rules did
/// not prove the box, or why its head was not read — kept in an escalation).
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
    let (choice, label) = match choice_of(p, rows, pick) {
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
    // An unnumbered box the vendor opened on its refusal (the irreversible
    // tool's `Tool use` box, y5n()'s defaultToNo lists `No` first): pressed
    // all the same, and said so — read from the option ORDER too, not only
    // from the focus, which the press moves (the harness round-3 review of
    // 2026-09-24).
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
    // A trust backstop's Yes makes the folder's listed tool permissions,
    // directories and headersHelper apply without asking in later sessions:
    // the subject names every listed row (the harness final review r3 of
    // 2026-09-24).
    let what = match backstop_of(p, rows) {
        Some(warnings) => {
            format!("{what} (backstop: {warnings}; trusting makes these apply without asking)")
        }
        None => what.to_string(),
    };
    // A one-shot allow names what it allowed, as a proven press does (the
    // ledger hashes it as the command); any other answer names the option
    // chosen too — which answer, which plan option, which refusal.
    let subject = if matches!(rule_id, RULE_ALLOW_ONCE | RULE_TRUST_ANY) {
        what
    } else {
        format!("{what} => {label}")
    };
    Decision::Approve {
        rule_id,
        choice,
        guard: row_guard(&rows[judged_row(p, rows)]),
        subject,
        unproven: Some(unproven),
    }
}

/// A trust dialog's gated-grants backstop's warnings, joined (`None` for any
/// other box): the option `No, continue without these permissions` beside
/// the trust option.
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

/// The words a limit's CONSENT to go on opens with, lowercased (module
/// header; the 2.1.281 binary's `Continue with …`, drawn while the credits
/// are on). Turning them back on is a purchase ([`PURCHASE`]).
const CONSENT: &[&str] = &["continue with"];

/// Whether `opt` is a consent to go on ([`CONSENT`]): its label OPENS so.
fn consents(opt: &Opt) -> bool {
    let l = opt.label.trim().to_lowercase();
    CONSENT.iter().any(|w| l.starts_with(w))
}

/// A dialog of no kind aterm-phase names ([`PromptKind::Other`]): on Claude
/// Code's screen a limit's consent to go on is accepted (module header) —
/// on Codex's a `Continue with …` is a model change (`Continue with Luna
/// Reserve`) or a sign-in (`Continue with detected credentials`), never a
/// consent; a box whose every yes buys gets the option that waits; any other
/// — a setup or config dialog: the auto-mode default, the Chrome upsell,
/// Remote Control, a custom API key, a model upgrade that restarts — is
/// DECLINED with its refusal ([`RULE_DECLINE`]): its yes settles something
/// for good (a default mode, the key's billing, a restart), its `No` or `Not
/// now` settles nothing. Claude Code's model-refusal pause is answered with
/// its switch while `model_fallback` is set ([`refusal_pause_switch`],
/// [`RULE_MODEL_SWITCH`]); NO other box's `Switch
/// to …` is ever chosen here, nor taken for the option that waits. With none
/// of these it is a person's.
fn other_dialog<'p>(
    program: Program,
    p: &'p PromptV2,
    ctx: &ApprovalCtx,
) -> Result<(&'static str, &'p Opt), String> {
    if program == Program::Claude
        && let Some(o) = p.options.iter().find(|o| consents(o) && !buys(o))
    {
        return Ok((RULE_ALLOW_ONCE, o));
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

/// Whether choosing `opt` switches the session's model: a `Switch to
/// <model>` label (Claude Code's refusal pause, the 2.1.282 render code's
/// `_5e()`; Codex's nudge), with no purchase in it.
fn switches_model(opt: &Opt) -> bool {
    opt.label.trim_start().starts_with("Switch to ") && !buys(opt)
}

/// CLAUDE CODE'S MODEL-REFUSAL PAUSE, and nothing else: its switch when the
/// box is exactly it — program Claude, its head read, titled `Session
/// paused` (anchor `paused.title`), its options exactly `Switch to <model>`
/// and `Edit prompt and retry…` (anchor `paused.edit`) — Claude Code's
/// `refusal_fallback_prompt` (2.1.282/2.1.283).
fn refusal_pause_switch(program: Program, p: &PromptV2) -> Option<&Opt> {
    if program != Program::Claude || p.head_off_screen || p.title.trim() != anchor("paused.title") {
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

/// What answers Codex's rate-limit nudge ([`rate_nudge`]): the loop's word
/// on the session at the moment the box shows. The default is the full-power
/// default with nothing read: enabled, no switch open, the model and the
/// usage window unread — which keeps the current model.
#[derive(Debug, Clone, PartialEq)]
pub struct NudgeCtx {
    /// `[harness] rate_nudge`: off, the nudge is a person's.
    pub enabled: bool,
    /// A save-then-wait switch is open already (winding down, restoring or
    /// holding): the same nudge again is no new switch.
    pub open: bool,
    /// The thread's model and effort as the footer last showed them — the
    /// box covers the footer, so the loop's cache (`None`: never read).
    pub from: Option<CodexSetting>,
    /// What Codex's own records say of its usage window
    /// ([`crate::supervise::codex_usage::read_limits`]).
    pub limits: LimitRead,
}

impl Default for NudgeCtx {
    fn default() -> Self {
        Self {
            enabled: true,
            open: false,
            from: None,
            limits: LimitRead::default(),
        }
    }
}

/// A model name as the picker and the nudge compare it: lowercased, its
/// `(current)` / `(default)` marks dropped, spaces as `-` — `GPT-6-Astra
/// (default)` and `gpt-6-astra` are one model.
fn model_key(label: &str) -> String {
    let mut l = label.trim().to_lowercase();
    loop {
        let before = l.len();
        for mark in [" (current)", " (default)"] {
            if let Some(head) = l.strip_suffix(mark) {
                l = head.trim_end().to_string();
            }
        }
        if l.len() == before {
            break;
        }
    }
    l.split_whitespace().collect::<Vec<_>>().join("-")
}

/// An effort as the effort boxes and the footer compare it: [`model_key`]'s
/// marks dropped, `extra high` as the config's `xhigh`.
fn effort_key(label: &str) -> String {
    let k = model_key(label);
    if k == "extra-high" {
        "xhigh".to_string()
    } else {
        k
    }
}

/// FULL POWER'S ANSWER TO CODEX'S RATE-LIMIT NUDGE (module header): its
/// switch ([`RULE_RATE_NUDGE_SWITCH`]) only when every one of these holds —
/// no switch is open, Codex's usage window reads NEAR its limit, the model
/// it leaves was read off the footer, and the option names another model;
/// otherwise its plain keep ([`RULE_RATE_NUDGE_KEEP`]). Never the keep that
/// never shows it again (a [`Role::Persist`]: it writes Codex's config).
/// `Err` — a person's — on another program's screen, under `rate_nudge =
/// false`, and on a nudge without exactly one plain keep. The `String` is
/// why, for the ledger.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "SupervisorCodexRateNudge",
        action = "PressSwitch",
        project = "supervise_conformance_codex_rate_nudge::ph_of"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "SupervisorCodexRateNudge",
        action = "PressKeep",
        project = "supervise_conformance_codex_rate_nudge::ph_of"
    )
)]
fn rate_nudge<'p>(
    p: &'p PromptV2,
    program: Program,
    ctx: &ApprovalCtx,
) -> Result<(&'static str, &'p Opt, String), String> {
    if program != Program::Codex {
        return Err(format!(
            "a rate-limit nudge on a {} session's screen",
            program.name()
        ));
    }
    if !ctx.nudge.enabled {
        return Err("rate_nudge is off: Codex's rate-limit model nudge is a person's".to_string());
    }
    let keeps: Vec<&Opt> = p
        .options
        .iter()
        .filter(|o| o.role == Role::Deny && o.label == anchor("codex.nudge.keep"))
        .collect();
    let [keep] = keeps.as_slice() else {
        return Err(format!(
            "a rate-limit nudge without exactly one `{}` ({})",
            anchor("codex.nudge.keep"),
            p.options
                .iter()
                .map(|o| format!("{}:{}", o.role.name(), o.label))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let keep_because = |why: String| Ok((RULE_RATE_NUDGE_KEEP, *keep, why));
    let n = &ctx.nudge;
    if n.open {
        return keep_because("a save-then-wait switch is open already".to_string());
    }
    let LimitRead::Near { back_at, .. } = &n.limits else {
        return keep_because(format!("{}: no switch", n.limits.words()));
    };
    let Some(from) = &n.from else {
        return keep_because("the thread's model was never read off its footer".to_string());
    };
    let switches: Vec<&Opt> = p
        .options
        .iter()
        .filter(|o| o.role == Role::Other && switches_model(o))
        .collect();
    let [switch] = switches.as_slice() else {
        return keep_because("no one `Switch to <model>` among its options".to_string());
    };
    let to = switch
        .label
        .trim_start()
        .trim_start_matches("Switch to ")
        .trim();
    if model_key(to) == model_key(&from.model) {
        return keep_because(format!("its model is the current one, {}", from.model));
    }
    Ok((
        RULE_RATE_NUDGE_SWITCH,
        *switch,
        format!(
            "{}{}: switched only to save the work, then back to {}",
            n.limits.words(),
            back_at
                .map(|t| format!(
                    ", back at its reset {}",
                    aterm_types::rfc3339::format_rfc3339(u64::try_from(t).unwrap_or(0))
                ))
                .unwrap_or_default(),
            from.words()
        ),
    ))
}

/// FULL POWER'S ANSWER TO CODEX'S `/model` PICKER: only the harness's OWN
/// restore in flight ([`ApprovalCtx::model_restore`]) — every other picker
/// is the person's who opened it. The model box: the option that IS the
/// original model (its `(current)` / `(default)` marks aside), by the focus
/// and Enter, which opens its effort box and saves nothing. The effort box,
/// titled for that model: the original effort by the focus and `s` — this
/// conversation only — never Enter or a digit, which save it as Codex's
/// default; `max` and `ultra` by `More reasoning…` (Enter opens the advanced
/// box). The advanced box: `Max` or `Ultra` by the focus and `s`. A box
/// offering no `s` (anchor `codex.pick.session`), no exact option, or
/// titled for another model is escalated.
fn model_restore_pick(
    p: &PromptV2,
    rows: &[String],
    program: Program,
    ctx: &ApprovalCtx,
) -> Decision {
    match restore_pick(p, rows, program, ctx) {
        Ok((choice, label, why)) => Decision::Approve {
            rule_id: RULE_MODEL_RESTORE_PICK,
            choice,
            guard: row_guard(&rows[p.span.0.min(rows.len().saturating_sub(1))]),
            subject: format!("{} => {label}", p.title),
            unproven: Some(why),
        },
        Err(why) => Decision::escalate(why),
    }
}

/// FULL POWER'S ANSWER TO CODEX'S PAUSED-GOAL BOX (aterm-phase's
/// `GoalResume`: the box a `codex resume` with no prompt opens with over a
/// paused goal): only where the LIVE UPGRADE paused this goal for its move
/// and owes it its resume ([`ApprovalCtx::goal_resume`]) — the relaunched
/// Codex opens on it — and only its FOCUSED option when that is the resume
/// (anchor `codex.goal.resume.yes`, which Codex focuses first: the same
/// `thread/goal/set … active` as `/goal resume`), pressed by the focus and
/// Enter under the guard of its row ([`RULE_GOAL_RESUME`]). The upgrade's
/// own step answers it first where it can (`upgrade_codex_drive`'s
/// `resume_on_relaunch`); this answers the box that came later. Every other
/// such box — a person's own `codex resume` over their paused goal, a box
/// whose focus a person moved — is the person's, escalated, and its `Leave
/// paused` is never pressed.
fn goal_resume_pick(
    p: &PromptV2,
    rows: &[String],
    program: Program,
    ctx: &ApprovalCtx,
) -> Decision {
    if program != Program::Codex || !ctx.goal_resume {
        return Decision::escalate(
            "Codex asks whether to resume a paused goal aterm did not pause: the person's",
        );
    }
    let Some(o) = p.options.iter().find(|o| o.focused) else {
        return Decision::escalate("the paused goal's box with no focused option");
    };
    if o.label != anchor("codex.goal.resume.yes") || o.row >= rows.len() {
        return Decision::escalate(format!(
            "the paused goal's box has `{}` focused, not the resume: the person's",
            o.label
        ));
    }
    Decision::Approve {
        rule_id: RULE_GOAL_RESUME,
        choice: Choice::Focus {
            steps: 0,
            label: o.label.clone(),
        },
        guard: row_guard(&rows[o.row]),
        subject: format!("{} => {}", p.title, o.label),
        unproven: Some("the live upgrade's resume of the goal it paused for its move".to_string()),
    }
}

fn restore_pick(
    p: &PromptV2,
    rows: &[String],
    program: Program,
    ctx: &ApprovalCtx,
) -> Result<(Choice, String, String), String> {
    if program != Program::Codex {
        return Err(format!(
            "a model picker on a {} session's screen",
            program.name()
        ));
    }
    let Some(target) = &ctx.model_restore else {
        return Err(
            "Codex's model picker, not opened by the harness's own restore: the person's"
                .to_string(),
        );
    };
    let title = p.title.trim();
    let focus_to = |o: &Opt| -> Result<i32, String> {
        let at = p
            .options
            .iter()
            .position(|q| q.focused)
            .ok_or_else(|| "a picker with no focused option".to_string())?;
        let to = p
            .options
            .iter()
            .position(|q| std::ptr::eq(q, o))
            .unwrap_or(at);
        Ok(i32::try_from(to).unwrap_or(0) - i32::try_from(at).unwrap_or(0))
    };
    let find =
        |want: &str, key: fn(&str) -> String| p.options.iter().find(|o| key(&o.label) == key(want));
    let session_key = || {
        let footer = rows.get(p.span.1).map_or("", |r| r.as_str());
        if footer.contains(anchor("codex.pick.session")) {
            Ok(())
        } else {
            Err(format!(
                "`{title}` offers no this-conversation choice (`{}`): Enter would save a default",
                anchor("codex.pick.session")
            ))
        }
    };
    let why = format!("the harness's own restore to {}", target.words());
    if title == anchor("codex.pick.model") {
        let o = find(&target.model, model_key)
            .ok_or_else(|| format!("`{title}` lists no {}", target.model))?;
        return Ok((
            Choice::Focus {
                steps: focus_to(o)?,
                label: o.label.clone(),
            },
            o.label.clone(),
            why,
        ));
    }
    let effort = target
        .effort
        .as_deref()
        .ok_or_else(|| format!("the effort {} ran at was never read", target.model))?;
    let advanced = matches!(effort_key(effort).as_str(), "max" | "ultra");
    if let Some(model) = title.strip_prefix(anchor("codex.pick.effort")) {
        if model_key(model) != model_key(&target.model) {
            return Err(format!(
                "`{title}` is for another model than {}",
                target.model
            ));
        }
        if advanced {
            let o = find(anchor("codex.pick.more"), model_key)
                .ok_or_else(|| format!("`{title}` has no `{}`", anchor("codex.pick.more")))?;
            return Ok((
                Choice::Focus {
                    steps: focus_to(o)?,
                    label: o.label.clone(),
                },
                o.label.clone(),
                why,
            ));
        }
        session_key()?;
        let o = find(effort, effort_key)
            .ok_or_else(|| format!("`{title}` lists no effort {effort}"))?;
        return Ok((
            Choice::FocusKey {
                steps: focus_to(o)?,
                label: o.label.clone(),
                key: "s",
            },
            o.label.clone(),
            why,
        ));
    }
    if title == anchor("codex.pick.advanced") && advanced {
        session_key()?;
        let o = find(effort, effort_key)
            .ok_or_else(|| format!("`{title}` lists no effort {effort}"))?;
        return Ok((
            Choice::FocusKey {
                steps: focus_to(o)?,
                label: o.label.clone(),
                key: "s",
            },
            o.label.clone(),
            why,
        ));
    }
    Err(format!(
        "`{title}` is no step of the restore to {}",
        target.words()
    ))
}

/// The words only plan mode's boxes' options carry, lowercased (Claude Code
/// 2.1.281/2.1.282, Codex 0.156.1): how a plan box whose head is off the
/// screen is still read as one ([`unread_head`]) — never by a mode switch
/// alone, which an Edit box's `allow all edits during this session
/// (shift+tab)` is too.
const PLAN_WORDS: &[&str] = &[
    "manually approve edits",
    "keep planning",
    "enter plan mode",
    "implement this plan",
    "stay in plan mode",
];

/// The words of a plan yes that grants a STANDING mode or wipes the
/// conversation, lowercased (the 2.1.281 and 2.1.282 binaries' plan
/// options; Codex's `Yes, clear context and implement`).
const BROAD_PLAN: &[&str] = &[
    "auto mode",
    "accept edits",
    "auto-accept",
    "bypass permissions",
    "clear context",
];

/// Plan mode's box (module header): the yes that grants no standing mode —
/// a [`Role::Once`] (Codex's `Yes, implement this plan`), else a yes whose
/// words name none of [`BROAD_PLAN`] (`Yes, manually approve edits`: the
/// default mode, every edit still asked; the entry's `Yes, enter plan
/// mode`). Claude Code lists its broadest yes FIRST (`Yes, clear context …
/// and bypass permissions` when bypass is available, else `Yes, and use
/// auto mode`), so the first yes was the most power the session could be
/// given (the hazards review of 2026-09-25, blocking). With no such yes the
/// plan is declined (its `No, keep planning`), the refusal that settles
/// nothing, and the turn-end policy continues the worker.
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

/// A permission box's one-shot allow (module header): its one
/// [`Role::Once`]; else, on a box whose options carry roles (a sound parse),
/// its first yes that grants nothing standing; else, on a box that would
/// have something bought, the option that waits; else its `No`
/// ([`RULE_DECLINE`]). None of them buys anything or grants anything
/// standing.
fn allow_once(p: &PromptV2) -> Result<(&'static str, &Opt), String> {
    let once: Vec<&Opt> = p.options.iter().filter(|o| o.role == Role::Once).collect();
    if let [o] = once.as_slice()
        && !buys(o)
    {
        return Ok((RULE_ALLOW_ONCE, o));
    }
    // Roles only on a sound parse (aterm-phase): a yes is taken by its role.
    let sound = p.options.iter().any(|o| o.role != Role::Other);
    let standing = |o: &Opt| matches!(o.role, Role::Session | Role::Persist | Role::ModeSwitch);
    if sound
        && let Some(o) = p
            .options
            .iter()
            .find(|o| o.affirms() && !standing(o) && !buys(o))
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
fn choice_of(p: &PromptV2, rows: &[String], o: &Opt) -> Result<Choice, String> {
    match p.select {
        Select::Digits if amend_input_open(p, rows) => Err(OPEN_INPUT.to_string()),
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

/// The row a full-power press is guarded on: a Bash or PowerShell box's
/// first command row (drawn bare, or behind its program's prompt glyph —
/// Codex's `$ `), a file box's path (the question naming the file for `Edit
/// notebook` and the IDE diff), the trust dialog's folder, else the box's
/// own first row.
fn judged_row(p: &PromptV2, rows: &[String]) -> usize {
    let own = match p.kind {
        PromptKind::Bash | PromptKind::PowerShell => p.command_rows.first().and_then(|c| {
            row_of(rows, p, c).or_else(|| {
                let (top, bottom) = p.span;
                (top + 1..bottom.min(rows.len())).find(|&k| {
                    rows[k]
                        .trim()
                        .strip_suffix(c.as_str())
                        .is_some_and(|glyph| !glyph.is_empty() && glyph.trim_end() == "$")
                })
            })
        }),
        PromptKind::Trust => (p.span.0 + 1..p.span.1.min(rows.len())).find(|&k| {
            let t = rows[k].trim();
            t.starts_with('/') || t.starts_with('~')
        }),
        // A file box's path row — or, for `Edit notebook` and the IDE diff,
        // whose file only the question names, the question row.
        _ => p
            .path
            .as_deref()
            .and_then(|path| row_of(rows, p, path))
            .or(p.question.filter(|&q| q < rows.len() && p.path.is_some())),
    };
    own.unwrap_or(p.span.0)
}

/// The usage-limit options dialog ([`RULE_LIMIT_WAIT`], module header): its
/// wait row, by label, reached with the arrows from where the focus is and
/// confirmed with Enter once a fresh read shows the focus on it (the loop's
/// part, `press_focused`) — the same verified path as the trust dialog, never
/// a digit, never Esc. The guard is the dialog's own title row. Nothing else
/// on it is ever chosen: a menu with no wait row (or more than one), with no
/// focus, or with the rule off is a person's.
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
    let Some(title_row) = rows.get(prompt.span.0).filter(|r| r.trim() == title) else {
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

#[cfg(test)]
#[path = "approval_tests.rs"]
pub(crate) mod tests;

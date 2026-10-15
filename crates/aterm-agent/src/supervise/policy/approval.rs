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
//! under [`Approve::All`] every box the reader parses WHOLE gets its answer —
//!
//! * a permission box, whatever it asks: its ONE-SHOT allow ([`Role::Once`];
//!   a box with no such role, its first `Yes…` option that is no
//!   [`Role::Session`], [`Role::Persist`] or [`Role::ModeSwitch`] grant) —
//!   [`RULE_ALLOW_ONCE`]; the read-outside dialog's is 2.1.287's `Yes, but
//!   ask again next time`. A standing grant is never chosen: it would rewrite
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
//!   read-outside dialog with no one-shot allow (2.1.282: its `No, ask again
//!   next time`) and a setup or
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
//!   answered with the option that waits (*"configuration limits power"*);
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
//! whose first row read is its own question) is NOT READ WHOLE, and no level
//! presses an option of it: nothing on the screen names its kind, and its
//! visible tail cannot say what its options do. Measured 2026-10-02 on Claude
//! Code 2.1.287 (an 80x18 pane): the auto-mode read-outside dialog's tail
//! lost its title, its standing `1. Yes, and keep allowing any reads outside
//! the working directories` read as no role the table knew, and full power
//! pressed it by its tail — Claude Code then saved
//! `hasSeenAutoModeOutsideReadPrompt` and never asked again; on 2.1.282's
//! wording the same cut pressed `No, block reads … from now on`, which
//! writes the block into the user's settings for every project. So such a box
//! waits: every look reads it afresh, and once the pane shows it whole (a
//! split closed, the window grown) it is answered as any other; until then
//! it is a person's. The one answer it can get is THE DECLINE of the
//! measured rm-breaker foot in a bypass session ([`tall_box`],
//! [`RULE_TALL_BOX`]): the plain `No` with a reason, which settles nothing
//! whatever the cut title says. A question whose head is cut stays a
//! person's too: which tab it is on, and what was already checked, is off
//! the screen.
//!
//! **THE PRESS GATE** ([`decide`]): whatever rule chose it, no automatic
//! press chooses an option whose answer is KEPT for every later ask
//! ([`Role::Persist`], [`aterm_phase::prompt::settles_later`]: `don't ask`,
//! `keep allowing`, `always`, `from now on`, `remember this`, …, a refusal
//! among them) or one that buys ([`buys`]): such a decision becomes an
//! escalation naming the option. A question's options are judged the same
//! way by the question rule, their descriptions read too
//! ([`super::question`]).
//!
//! **What stays a person's** is what no reader can answer: a box whose
//! options carry no roles (parsed with unsound options), a box not read
//! whole, and a question whose head is off the screen.
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
//! person, a dialog as much as a question in prose. HOW it is answered is
//! [`ApprovalCtx::question_priority`] ([`QuestionPriority`]): by default its
//! recommended option; under the priority words `long-term` / `short-term`
//! by the worker itself, to whom the supervisor writes that word's fixed
//! policy text in the question's free-text row ([`Answer::Type`]).
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
//! **A limit reset is taken only by its own rule, and only where the owner
//! turned it on.** In front of the wait row, in the same branch, the dialog's
//! GRANT row — `Use your reset now (<n> left, until <day>)`, read by its
//! exact template and never by its place ([`aterm_phase::reset_row`]) — is
//! chosen under [`RULE_LIMIT_RESET`] ([`limit_reset`]) where `[harness]
//! limit_reset` is on ([`ApprovalCtx::limit_reset`], OFF by default, at every
//! level and whatever `limit_wait` says). With the key off, or no such row,
//! or the weekly session reset's row in that slot, the branch is the wait
//! row's exactly as before. The decision is only the row's: whether this
//! session may take it (the host's claim) and what follows it (the
//! confirmation, [`super::reset::confirm_verdict`]) are the loop's. EVERY
//! OTHER RULE NEVER SPENDS ONE ([`spends`]): each pick that can say yes
//! passes the reset's rows and its confirmation's yes over ([`buys`]), and
//! whatever [`decide`] arrived at is checked once more before it is returned
//! ([`never_spends`]). On the confirmation itself — told by ANY part left of
//! it ([`confirms_reset`]), because the server may reword its yes and a short
//! pane cuts its title off — nothing but its refusal is chosen. No reset text
//! has been seen on a screen: they are read in the 2.1.287 bundle.
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
//!   under the space reading and a write under the other; a row the parser
//!   shows as the description may be the command's tail (a barred `Rm -rf …`
//!   row read as prose). Never narrowed for this rule: only the rm breaker's box,
//!   whose geometry was measured, drops a row the box proves is no wrap
//!   of the command ([`command_readings`]). Never on a box that
//!   carries a vendor note row, and never in a bypass session: there every
//!   box is a vendor circuit breaker. Where the Bash tool may run zsh (its
//!   shell on macOS), every reading must also be PLAIN
//!   ([`plain_zsh_line`]: nothing the shell expands) — the classifier's
//!   zsh sweeps were exhaustive only over the forms tried, so a line outside
//!   the plain grammar is a person's (decided 2026-09-30 under the owner's
//!   standing direction; under full power it is pressed all the same, the
//!   reason kept as `unproven`).
//! * [`RULE_RM_BREAKER`] — the vendor's `Dangerous rm operation on
//!   possibly-empty variable path` box in a bypass session, when every `rm`
//!   operand under every reading resolves strictly inside a scratch root,
//!   as written and where the disk leads it, with no `$(mktemp -d)` it is
//!   built on able to reach it empty and no variable the line does not
//!   assign — or is a directory the line's own `mktemp -d` made, `"$D"` or
//!   a path under it — ([`super::rm_breaker`]; `$TMPDIR` is a root a
//!   literal path may sit in, never a value) AND the rest of the line is a
//!   read ([`classify_except_rm`]), on a box that runs on this machine (a
//!   `(runs on <m>)` box's targets resolve there, not here). The rest is
//!   judged, not left to bypass: the bypass the breaker knows of was read
//!   off a footer the box has since replaced, so a shift-tab out of bypass
//!   in that window would approve the whole line.
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
//!   where it leads. A link the worker makes after the
//!   read is not seen: the check is the read's, not the Read's.
//! * [`RULE_TRUST_DIALOG`] — Claude Code's folder-trust dialog, when the
//!   folder it asks about is the session's own working directory (`meta
//!   cwd=`, [`ApprovalCtx::cwd_known`]) AND under a trust root, its options
//!   exactly the measured `No, exit` and `Yes, I trust this folder`. It is
//!   unnumbered: the choice is to move the focus ([`Choice::Focus`]) and
//!   then Enter, the Enter only once a fresh read shows the focus on
//!   `Yes, I trust this folder` (the loop's part).
//!
//! **Which option** (the safe rules). Only the plain one-shot allow
//! ([`Role::Once`] — `Yes`, `Yes, proceed`, `Yes, run it`, Chrome's `Allow`)
//! — by its digit, or on an unnumbered box by moving the focus to it — or
//! the trust dialog's [`Role::Trust`]; never a [`Role::Session`], [`Role::Persist`] or [`Role::ModeSwitch`]
//! grant, which would turn one verdict into a standing wildcard, and never
//! [`Role::Other`] — what aterm-phase gives every option of a box whose
//! options are not sound, and every option of every Codex box.
//!
//! **Which program.** The box is read by the reader for the session's
//! PROGRAM ([`aterm_phase::read`]), and only on a reading whose phase is the
//! reader's evidence ([`Reading::phase_authoritative`]): a shell showing a
//! captured box, a pager — no agent's box, never pressed. The four safe rules
//! were measured on Claude Code's boxes and judge only those: under `"safe"` another
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
//! **A BOX TALLER THAN THE SCREEN, DECLINED** ([`RULE_TALL_BOX`],
//! [`tall_box`]). Measured 2026-09-25 (Claude Code 2.1.282/2.1.283, a live
//! headless aterm): Claude Code draws a box taller than the screen in ONE
//! frame, so its head never reaches the terminal or the alt-screen archive
//! (reading the archive over it was tried and dropped: it glued another
//! box's head on and approved a real `rm -rf $HOME/$1`); the vendor's
//! session record says `waitingFor: "permission prompt"` while the
//! transcript does not yet hold the pending call; and 2.1.283 denies an
//! unanswered box itself after about two minutes ("to avoid blocking
//! progress on an unattended session"). No rule presses an option of it
//! ("A box taller than the pane", above) — but a person handed it could
//! no more read its command than the harness can, and what IS on the screen
//! is its foot, where the vendor's note sits ([`PromptV2::foot_note`]). So
//! under `approve = "safe"` and full power alike, in a bypass session (asked
//! about nothing but the vendor's circuit breakers), a box whose foot flags a removal is
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
    Opt, PromptKind, PromptV2, RmBreaker, RmBreakerKind, RmCommand, Role, Select, asks,
    rm_breaker_of, settles_later,
};
use aterm_phase::{Phase, Program, Reading, anchor};

use super::super::codex_usage::LimitRead;
use super::super::config::Approve;
use super::a_or_an;
use super::claude_settings::AskRule;
use super::git_config::{GitView, WorkerEnv, git_reads, hazard};
use super::guard::row_guard;
use super::rm_breaker::{RmScope, ScratchRoot, abs_components, clip, resolve_rm_line};
use super::shell_startup::ShellStartup;
use super::turn_end::CodexSetting;
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
/// Full power: a box with no yes it may take (a standing grant, a purchase)
/// and nothing that waits, answered with the refusal that settles nothing.
pub const RULE_DECLINE: &str = "decline@v1";
/// Full power: Claude Code's model-refusal pause (`Session paused`) alone, its
/// `Switch to <model>` while `[harness] model_fallback` is set
/// ([`ApprovalCtx::model_fallback`], [`refusal_pause_switch`]).
pub const RULE_MODEL_SWITCH: &str = "model-switch@v1";
/// Full power, `[harness] rate_nudge`: Codex's rate-limit nudge `Switch to
/// <model>`, pressed only to save the work near the usage limit
/// ([`rate_nudge`]); the turn-end policy winds down, restores and holds.
pub const RULE_RATE_NUDGE_SWITCH: &str = "rate-nudge-switch@v1";
/// Full power, `[harness] rate_nudge`: the nudge's `Keep current model` for a
/// stale nudge, one during an open switch, or one no reading vouches for
/// ([`rate_nudge`]). Never `… (never show again)`.
pub const RULE_RATE_NUDGE_KEEP: &str = "rate-nudge-keep@v1";
/// Full power: the harness's own restore of a Codex thread's model after a
/// save-then-wait switch: the original model and effort, the effort for this
/// conversation only ([`model_restore_pick`]).
pub const RULE_MODEL_RESTORE_PICK: &str = "model-restore-pick@v1";
/// Full power: the live upgrade's resume of a Codex goal it paused, the
/// focused first option of the relaunched Codex's paused-goal box
/// ([`goal_resume_pick`]). Never a box a person's own `codex resume` drew.
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
/// The usage-limit dialog's grant row ([`limit_reset`]): `Use your reset now
/// (<n> left, until <day>)`, at every level, only under
/// [`ApprovalCtx::limit_reset`]. The one rule that may choose an option that
/// spends a reset ([`never_spends`]).
pub const RULE_LIMIT_RESET: &str = "limit-reset@v1";

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

/// The refusal a decline amends: the Bash box's `No`, the option Claude Code
/// makes an input under Tab (module header, "The decline").
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
    /// fresh read shows it on `label`: Codex's effort boxes' `s` applies the
    /// effort to this conversation only, where Enter (or a digit) saves it as
    /// the default.
    FocusKey {
        steps: i32,
        label: String,
        key: &'static str,
    },
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
        /// What was judged, as the ledger, notes and journal's `APPROVED` line
        /// name it: the command or path (rm rule: ` => ` resolved targets;
        /// trust backstop: warnings in parentheses; full power: the box's
        /// subject and ` => ` the option chosen).
        subject: String,
        /// Under full power, why the safe rules did not prove the box (the
        /// breaker kind first, and `vendor default was No` where it applies):
        /// the ledger's and journal's `unproven:`. `None` when proven.
        unproven: Option<String>,
    },
    /// Refuse the box with a reason the worker acts on (module doc, "The
    /// decline"): `No` amended with `text`, never the bare `No`. Built only by
    /// [`decline`]; the loop carries it out one keystroke at a time via
    /// [`decline_step`].
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
/// `*` matches any suffix of the last component (`~/aterm*`), resolved
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

/// The defaults for `trust_roots` when no config names any
/// (`SupervisorConfig::default`'s).
const DEFAULT_TRUST_ROOTS: &[&str] = &["~/aterm*", "~/ay*", "$HOME/trust*", "/private/tmp/claude-*"];

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
    /// What answers Codex's rate-limit nudge ([`rate_nudge`]).
    pub nudge: NudgeCtx,
    /// The Codex model and effort the harness's restore is putting back, while
    /// in flight (`TurnEndState::restore_target`); the only time the `/model`
    /// picker is answered ([`model_restore_pick`]).
    pub model_restore: Option<CodexSetting>,
    /// The live upgrade paused this tab's Codex goal and owes its resume
    /// (`harness::goal_hold`); the only time the paused-goal box is answered
    /// ([`goal_resume_pick`]).
    pub goal_resume: bool,
    /// Whether the usage-limit dialog's wait row is chosen ([`RULE_LIMIT_WAIT`],
    /// `[harness] limit_wait`), regardless of [`Self::approve`].
    pub limit_wait: bool,
    /// Whether the usage-limit dialog's grant row is chosen
    /// ([`RULE_LIMIT_RESET`], `[harness] limit_reset`), regardless of
    /// [`Self::approve`] and [`Self::limit_wait`]. Off unless the loop sets
    /// it: with it off the dialog is the wait row's, as it was before a reset
    /// could be taken.
    pub limit_reset: bool,
    /// Where a folder-trust dialog's folder may be trusted
    /// ([`RULE_TRUST_DIALOG`]).
    pub trust_roots: Vec<ScratchRoot>,
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
    /// and remembers writing; `None` when none (or forgotten after a restart or
    /// handover). The same text with `None` here is a person's.
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
    /// The bare-tool ask rules of the agent's own Claude Code settings
    /// ([`super::claude_settings::bare_asks`]), or why they could not be
    /// read: what holds a box Claude Code draws no ask reason on
    /// ([`unvoiced_tools`], [`settings_ask`]). Read by the loop only for such
    /// a box ([`asks_unread`]).
    pub owner_asks: Result<Vec<AskRule>, String>,
}

impl ApprovalCtx {
    /// Defaults for a session in `cwd` owned by `uid`: full power
    /// ([`Approve::All`]), questions answered, the usage-limit wait chosen
    /// and no limit reset taken.
    /// Scratch roots: `/private/tmp/claude-<uid>/`, `tmpdir` (a root for literal
    /// paths, when at least two levels deep and holding neither `cwd` nor
    /// `home`), any `/tmp/<x>/` and `/private/tmp/<x>/`, and `<cwd>/target*`.
    /// Trust roots `~/aterm*`, `~/ay*`, `$HOME/trust*`, `/private/tmp/claude-*`
    /// ([`Self::set_trust_roots`] replaces them). Read roots: trust roots plus
    /// `/usr`, `/etc`, `/private/etc`, `/opt/homebrew`,
    /// `/private/tmp/claude-<uid>`. [`default_secrets`], cwd known, not bypass,
    /// recommended answers, pane size unknown.
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
            limit_reset: false,
            trust_roots: Vec::new(),
            read_roots: Vec::new(),
            scratch_roots,
            rm_breaker_warn: true,
            python_allow: Vec::new(),
            question_priority: QuestionPriority::Recommended,
            question_own: None,
            screen_cols: 0,
            screen_rows: 0,
            question_fell_back: false,
            worker: worker_unread(),
            shell_cwds: Vec::new(),
            shell: shell_unread(),
            owner_asks: asks_unread(),
        };
        ctx.set_trust_roots(&defaults, uid);
        ctx
    }

    /// The trust roots from `[harness] trust_roots` ([`roots_from_config`]), and
    /// the Read roots rebuilt around them. A `claude-*` spec under
    /// `/private/tmp` or `/tmp` names only this uid's `claude-<uid>`.
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
fn asks_unread() -> Result<Vec<AskRule>, String> {
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

/// The permission mode Claude Code's footer names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterMode {
    /// `⏸ manual mode on`, or `? for shortcuts` and no pill: the default mode.
    Default,
    /// `⏵⏵ bypass permissions on`.
    Bypass,
    /// `⏵⏵ auto mode on`.
    Auto,
    /// `⏵⏵ accept edits on`.
    AcceptEdits,
    /// `⏵⏵ don't ask on`.
    DontAsk,
    /// `⏸ plan mode on`.
    Plan,
}

/// The mode the footer under the composer names, read from its pill
/// (`harness::footer::pill_indicator`); `None` when no footer is drawn (as
/// while a box is up), so a caller keeps its last answer.
pub fn footer_mode(rows: &[String]) -> Option<FooterMode> {
    use crate::harness::lights::Mode;
    let tail = rows.iter().rev().filter(|r| !r.trim().is_empty()).take(3);
    for r in tail {
        if let Some(mode) = crate::harness::footer::pill_indicator(r).and_then(Mode::of_indicator) {
            return Some(match mode {
                Mode::Bypass => FooterMode::Bypass,
                Mode::Auto => FooterMode::Auto,
                Mode::AcceptEdits => FooterMode::AcceptEdits,
                Mode::DontAsk => FooterMode::DontAsk,
                Mode::Plan => FooterMode::Plan,
                Mode::Manual => FooterMode::Default,
            });
        }
        if r.contains(aterm_phase::anchor("footer.shortcuts")) {
            return Some(FooterMode::Default);
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
    let decision = never_spends(reading, rows, decide_box(reading, rows, ctx));
    match reading
        .prompt
        .as_ref()
        .and_then(|p| kept_or_bought(p, &decision))
    {
        Some(why) => Decision::escalate(why),
        None => decision,
    }
}

/// THE PRESS GATE (module header): why the option `decision` presses on `p`
/// may not be pressed, whatever rule chose it — its answer is kept for every
/// later ask ([`Role::Persist`], [`settles_later`]) or it buys something
/// ([`buys`]). A question's options are the question rule's to judge
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
        Choice::Focus { label, .. } | Choice::FocusKey { label, .. } => {
            p.options.iter().find(|o| o.label == *label)
        }
        Choice::Answer(_) => None,
    }?;
    let what = if o.role == Role::Persist || settles_later(&o.label) {
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
/// place. A question's answer is the worker's own option, not the vendor's.
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
        Choice::Focus { label, .. } | Choice::FocusKey { label, .. } => Some(label.as_str()),
        Choice::Answer(_) => None,
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

/// [`decide`], before its last checks ([`never_spends`], THE PRESS GATE).
fn decide_box(reading: &Reading, rows: &[String], ctx: &ApprovalCtx) -> Decision {
    // The usage-limit dialog is decided first by its own rules, before
    // questions and levels, so full power never sees it: the grant row where
    // the owner turned limit resets on, else the wait row.
    if let Some(prompt) = reading.prompt.as_ref()
        && prompt.kind == PromptKind::UsageLimit
    {
        return if reading.program != Program::Claude
            || reading.phase != Phase::Prompt
            || !reading.phase_authoritative
        {
            Decision::escalate("no usage-limit dialog the reader vouches for")
        } else {
            limit_reset(prompt, rows, ctx).unwrap_or_else(|| limit_wait(prompt, rows, ctx))
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
    }
    // The user's own Claude Code configuration (an ask rule or hook) sends this
    // box to a person: no level presses it — read off the box's reason where
    // Claude Code draws one, else off the agent's own settings (a bare-tool
    // ask rule for a file tool), else, for an in-project Read, off the missing
    // folder grant. Built-in safety checks stay the levels' to answer.
    if let Some(review) = reading.prompt.as_ref().and_then(|p| p.owner_review()) {
        return Decision::escalate(format!(
            "the owner's Claude Code settings send this box to a person ({}): {}",
            review.kind.words(),
            review.text
        ));
    }
    if let Some(why) = settings_ask(reading, ctx) {
        return Decision::escalate(why);
    }
    if let Some(why) = forced_read(reading) {
        return Decision::escalate(why);
    }
    // A box not read whole is pressed by no level (module header, "A box
    // taller than the pane"): it waits to be read whole, or is a person's.
    if let Some(why) = head_unread(reading) {
        return match ctx.approve {
            Approve::All | Approve::Safe => tall_box(reading, rows, ctx, why),
            Approve::None => Decision::escalate(why),
        };
    }
    match ctx.approve {
        Approve::None => Decision::escalate("approve = \"none\": every box is the owner's"),
        Approve::Safe => match decide_safe(reading, rows, ctx) {
            Decision::Escalate { reason } if ctx.rm_breaker_warn => {
                rm_breaker_warn(reading, rows, reason)
            }
            decided => decided,
        },
        Approve::All => match decide_safe(reading, rows, ctx) {
            answered @ (Decision::Approve { .. } | Decision::Decline { .. }) => answered,
            Decision::Escalate { reason } => full_power(reading, rows, ctx, reason),
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
/// Fetch or Tool box ([`PromptV2::owner_review`]).
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

/// Why the owner's own Claude Code settings hold the box `reading` found, at
/// every level, where Claude Code draws no reason to read it by
/// ([`unvoiced_tools`]): a BARE-TOOL ask rule along the agent's settings
/// chain ([`ApprovalCtx::owner_asks`]) for any tool Claude Code draws the box
/// for — `Write`, `Edit`, `Bash`, `Read`…, with no specifier, so every call
/// of the tool is the person's and no matcher needs evaluating — or those
/// settings unread, where no rule can be ruled out. A rule with a specifier (`Edit(src/**)`) is Claude Code's to
/// match and holds nothing here (`tools/real-claude-lab/README.md`, DECIDED
/// 2026-10-09). A Codex box is not Claude Code's.
fn settings_ask(reading: &Reading, ctx: &ApprovalCtx) -> Option<String> {
    let p = reading.prompt.as_ref()?;
    let tools = unvoiced_tools(p);
    if reading.program != Program::Claude || tools.is_empty() {
        return None;
    }
    match &ctx.owner_asks {
        Ok(rules) => rules
            .iter()
            .find(|r| tools.contains(&r.tool.as_str()))
            .map(|r| {
                format!(
                    "the owner's Claude Code settings send this box to a person (an ask rule): \
                     `{}` in {}",
                    r.rule, r.source
                )
            }),
        Err(why) => Some(format!(
            "{} box the agent's Claude Code settings may send to a person, and they are \
             unknown: {why}",
            a_or_an(p.kind.name())
        )),
    }
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
    if reading.program != Program::Claude
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

/// Under `approve = "safe"`, the rm breaker the safe rules left (`safe`: their
/// reason), approved with a warning ([`RULE_RM_BREAKER_WARN`]) only in the form
/// the rm rule was written for: a vouched Claude Code Bash box whose note is one
/// `rm` on a possibly-empty variable ([`RmBreakerKind::EmptyVariable`],
/// [`resolvable_breaker`]) with one plain one-shot `Yes` and a `No`. Every
/// other breaker kind, another machine's box, or other options keep the
/// escalation (owner, 2026-09-29).
fn rm_breaker_warn(reading: &Reading, rows: &[String], safe: String) -> Decision {
    if reading.program != Program::Claude
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
        "{} \u{2014} approved with this warning under approve = \"safe\" ([harness] \
         rm_breaker_ask = true hands it to a person)",
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

/// The screen row of a file box's `path`: the bare path (2.1.282), or a Read
/// box's call (2.1.284 and later), `Read(<path>)`. ONE row: a call that wraps
/// names no row, and nothing is pressed under it.
fn file_path_row(rows: &[String], prompt: &PromptV2, path: &str) -> Option<usize> {
    row_of(rows, prompt, path)
        .or_else(|| row_of(rows, prompt, &format!("{}{path})", anchor("box.read_call"))))
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

/// Whether the worker may write under `dir` unapproved, making a `git` there the
/// worker's to configure ([`WorkerEnv::view`]): the session cwd, every Bash
/// tool directory, and the scratch roots, compared as written and resolved.
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
    for reading in read_readings(path) {
        if let Err(why) = read_path_allowed(reading, ctx) {
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
        format!("{}/{rest}", home.to_string_lossy().trim_end_matches('/'))
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        return Err(format!("a path that is not absolute: {path}"));
    };
    abs_components(&full).ok_or_else(|| format!("a path with `..`: {path}"))
}

/// A Read box's path: absolute or `~/…`, one plain path (no glob, no `..`),
/// under an [`ApprovalCtx::read_roots`] root and outside every [`SecretRule`].
fn read_path_allowed(path: &str, ctx: &ApprovalCtx) -> Result<(), String> {
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
    // Where its symlinks lead, judged the same (module header).
    let real = real_components(&comps)
        .ok_or_else(|| format!("a Read whose symlinks cannot be resolved: {path}"))?;
    if real != comps {
        read_comps_allowed(&real, &format!("{path} (-> /{})", real.join("/")), ctx)?;
    }
    // The scan lists folders, so it runs for a path every cheaper test passed:
    // one that is held lists nothing.
    if let Some(twin) = hidden_twin(&comps)? {
        return Err(format!(
            "a Read of {path}: {twin:?} differs from it only by characters the box may not \
             draw (and by case, which the default volume ignores), so the box may not show \
             which one is read"
        ));
    }
    Ok(())
}

/// How many entries of one folder [`hidden_twin`] lists before it holds the press
/// for a person (a build folder of hundreds of thousands costs a decision a
/// third of a second, and the loop looks again until the box leaves).
const HIDDEN_TWIN_ENTRIES: usize = 20_000;

/// An entry of a folder on `comps`' way whose name differs from the one the box
/// draws only by characters the box may not have drawn. Claude Code's Read
/// opens the REAL name, and this screen draws some characters as nothing at all
/// (a zero-width space or joiner, a soft hyphen, a byte-order mark, a tag
/// character: elided, not substituted, so the plain-ASCII rule cannot see
/// them): `x<ZWSP>y`, a link to a secret, is drawn `xy`, a name that does not
/// exist, or a decoy that does. Such an entry beside the shown name means the
/// shown name is not proven to be the one read: the press waits for a person.
/// Only ASCII reaches here, so what an entry shows is its name less every
/// character outside it, compared as the default volume compares names (APFS
/// ignores case: `Q<ZWSP>R` is what a Read of `qr` opens). The twin's full
/// path (it may be a folder on the way), or `Err` for a folder that cannot be
/// listed, and for one too big to list in a decision
/// ([`HIDDEN_TWIN_ENTRIES`]: about a microsecond an entry, measured).
fn hidden_twin(comps: &[String]) -> Result<Option<String>, String> {
    hidden_twin_within(comps, HIDDEN_TWIN_ENTRIES)
}

fn hidden_twin_within(comps: &[String], max_entries: usize) -> Result<Option<String>, String> {
    let mut dir = PathBuf::from("/");
    for comp in comps {
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for (n, entry) in entries.flatten().enumerate() {
                    if n >= max_entries {
                        return Err(format!(
                            "a Read in a folder too big to prove (over {max_entries} entries): {dir:?}"
                        ));
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let shown: String = name.chars().filter(|c| (' '..='~').contains(c)).collect();
                    if name != shown && shown.eq_ignore_ascii_case(comp) {
                        return Ok(Some(dir.join(&name).to_string_lossy().into_owned()));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "a Read whose folder cannot be listed ({e}): {dir:?}"
                ));
            }
        }
        dir.push(comp);
        // Nothing below a name that is no folder, or is not there.
        match std::fs::metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// `comps` with every symlink resolved: the deepest existing ancestor
/// canonicalized, plus the not-yet-existing components as written. `None` for
/// a dangling link or a refused look.
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

/// The folder-trust dialog ([`RULE_TRUST_DIALOG`]): its folder is the session's
/// launch directory under a trust root, its options exactly `No, exit` and
/// `Yes, I trust this folder`, focus on one of them. The choice moves the focus
/// to the trust option (or presses its digit if numbered).
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
/// already on buys nothing.
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
/// [`never_spends`]): Claude Code's `Use your reset now (…)` and `Reset your
/// session limit now` rows on the usage-limit dialog, and `Yes, use my reset`
/// on the confirmation the first opens, by the reader's own anchors
/// ([`aterm_phase::spends_reset`]) and by the wider words of
/// [`RESET_SPEND`]. READ in the 2.1.286 and 2.1.287 bundles; no such box has
/// been captured on a screen.
///
/// A reset is a counted allowance with a deadline, and the weekly one claims
/// with no confirmation at all. A confirmation whose title row is off the
/// screen would fall to [`allow_once`], whose first plain yes is `Yes, use my
/// reset`; so such a box is pressed by no rule (it is not read whole,
/// [`head_unread`]), a box that offers one gets the option that waits (`No,
/// keep it`, [`RULE_NO_SPEND`]), and the grant row is chosen by
/// [`RULE_LIMIT_RESET`] alone.
fn spends(label: &str) -> bool {
    aterm_phase::spends_reset(label) || opens_with(label, RESET_SPEND)
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

/// Whether `opt` is a consent to go on ([`CONSENT`]): its label opens so.
fn consents(opt: &Opt) -> bool {
    let l = opt.label.trim().to_lowercase();
    CONSENT.iter().any(|w| l.starts_with(w))
}

/// A dialog of no kind aterm-phase names ([`PromptKind::Other`]). On Claude
/// Code's screen a limit consent is accepted (on Codex's, `Continue with …` is
/// a model change or sign-in); a box whose every yes buys gets the option that
/// waits; the model-refusal pause gets its switch while `model_fallback` is set
/// ([`refusal_pause_switch`], [`RULE_MODEL_SWITCH`]); any other setup or config
/// dialog is declined with its refusal ([`RULE_DECLINE`]), since its yes
/// settles something for good. No other `Switch to …` is chosen here. Else a
/// person's.
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

/// What answers Codex's rate-limit nudge ([`rate_nudge`]): the loop's view of
/// the session when the box shows. The default (nothing read) keeps the
/// current model.
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

/// A model name as the picker and nudge compare it: lowercased, `(current)` /
/// `(default)` dropped, spaces as `-`.
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

/// Full power's answer to Codex's rate-limit nudge: its switch
/// ([`RULE_RATE_NUDGE_SWITCH`]) only when no switch is open, the usage window
/// reads near its limit, the current model was read off the footer, and the
/// option names another model; otherwise the plain keep
/// ([`RULE_RATE_NUDGE_KEEP`]), never the persisting keep. `Err` (a person's) on
/// another program, under `rate_nudge = false`, or without exactly one plain
/// keep. The `String` is the ledger reason.
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

/// Full power's answer to Codex's `/model` picker, only during the harness's
/// own restore ([`ApprovalCtx::model_restore`]). Model box: the original model
/// by focus and Enter (opens the effort box, saves nothing). Effort box for
/// that model: the original effort by focus and `s` (this conversation only),
/// never Enter or a digit; `max` and `ultra` via `More reasoning…`, then focus
/// and `s`. A box without `s` (anchor `codex.pick.session`), without the exact
/// option, or for another model is escalated.
fn model_restore_pick(
    p: &PromptV2,
    rows: &[String],
    program: Program,
    ctx: &ApprovalCtx,
) -> Decision {
    match restore_pick(p, program, ctx) {
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

/// Full power's answer to Codex's paused-goal box, only where the live upgrade
/// paused this goal and owes its resume ([`ApprovalCtx::goal_resume`]), and
/// only when the focused option is the resume (anchor
/// `codex.goal.resume.yes`), by focus and Enter under its row's guard
/// ([`RULE_GOAL_RESUME`]). The upgrade's `resume_on_relaunch` answers it first
/// where it can. Any other such box is the person's; `Leave paused` is never
/// pressed.
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
        if p.footer
            .as_deref()
            .is_some_and(|f| f.contains(anchor("codex.pick.session")))
        {
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
/// parse, its first yes granting nothing standing; else, on a box that would
/// buy, the option that waits; else its `No` ([`RULE_DECLINE`]).
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

/// The row a full-power press is guarded on: a Bash or PowerShell box's first
/// command row (bare or behind a prompt glyph like Codex's `$ `), a file box's
/// path (the question for `Edit notebook` and the IDE diff), the trust
/// dialog's folder, else the box's first row.
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
            .and_then(|path| file_path_row(rows, p, path))
            .or(p.question.filter(|&q| q < rows.len() && p.path.is_some())),
    };
    own.unwrap_or(p.span.0)
}

/// The usage-limit options dialog's GRANT ROW ([`RULE_LIMIT_RESET`]), decided
/// in front of the wait row: `Some` only where `[harness] limit_reset` is on
/// ([`ApprovalCtx::limit_reset`]), the dialog holds exactly one row read by
/// the grant's whole template and no weekly session reset's row
/// ([`aterm_phase::reset_row`]: never by its place), its title is the
/// dialog's and on the screen, and an option has the focus. The row is then
/// reached and confirmed as the wait row is — arrows, and Enter once a fresh
/// read shows the focus on that very label, under the title row's guard,
/// never a digit. `None` — the wait row's rule, exactly as with the key off —
/// in every other case. What follows the row is the loop's.
fn limit_reset(prompt: &PromptV2, rows: &[String], ctx: &ApprovalCtx) -> Option<Decision> {
    if !ctx.limit_reset {
        return None;
    }
    let row = aterm_phase::reset_row(prompt)?;
    let title = anchor("limit.title");
    if prompt.title != title {
        return None;
    }
    let at = prompt.options.iter().position(|o| o.focused)?;
    let title_row = rows.get(prompt.span.0).filter(|r| r.trim() == title)?;
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

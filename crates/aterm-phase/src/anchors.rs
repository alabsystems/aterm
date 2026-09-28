// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The literal strings a supervisor guards on, in ONE table.
//!
//! A guard (`key if=<regex> 1`, `await match`, `turn … submit=guarded:<re>`) is only as
//! good as the text it names, and that text is Claude Code's, not ours: the
//! day a release renames a header or a hint, a guard spelled inline in some
//! other crate silently stops matching. So the strings live here, each with
//! an id a caller imports ([`anchor`]) instead of re-typing the words, the
//! release they were read on, and whether they are one literal in the
//! vendor's binary or composed at run time from key hints.
//!
//! Codex's strings are their own table, [`CODEX_ANCHORS`] (ids `codex.…`);
//! [`anchor`] and [`anchor_text`] read both.
//!
//! The ignored tests `every_literal_anchor_is_in_the_store_claude` and
//! `every_literal_codex_anchor_is_in_the_store_codex` grep the store's
//! binaries for every [`AnchorKind::Literal`] row (the Codex one for every
//! [`crate::codex::WALLS`] phrase too) and print the misses — the drift
//! canaries to run when a pinned build moves.

/// How an anchor's text reaches the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    /// One string in the vendor's binary (a `·` in it is drawn from a
    /// `\xB7` escape, so the canary looks for the parts either side of it).
    Literal,
    /// Composed at run time from the user's key bindings (`Esc to cancel` is
    /// the `confirm:no` binding's key plus a description): no single literal
    /// in the binary carries it, and a rebinding changes the key word.
    KeyHint,
    /// Assembled at run time from parts (`Do you want to ` + `make this edit
    /// to` + the file name; the rm breaker's warning from a template): the
    /// canary cannot look for it whole.
    Composed,
}

/// One guarded string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// The id callers import it by (`prompt.proceed`).
    pub id: &'static str,
    /// The text as the screen shows it.
    pub text: &'static str,
    pub kind: AnchorKind,
    /// The vendor release it was read on (a capture or the binary).
    pub since: &'static str,
}

const fn lit(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::Literal,
        since,
    }
}

const fn composed(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::Composed,
        since,
    }
}

const fn hint(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::KeyHint,
        since,
    }
}

/// Every string aterm guards on in Claude Code's screen.
pub const ANCHORS: &[Anchor] = &[
    // The approval box.
    lit("prompt.proceed", "Do you want to proceed?", "2.1.267"),
    composed("prompt.edit", "Do you want to make this edit to", "2.1.280"),
    hint("prompt.cancel", "Esc to cancel", "2.1.267"),
    hint("prompt.amend", "Tab to amend", "2.1.267"),
    // A permission box's REASON BLOCK when the OWNER's own Claude Code
    // configuration sends the step to a person (2.1.282, read from its
    // render code and measured on the S13 capture B, 2026-09-25): an ask
    // rule (`Permission rule Bash(touch:*) requires confirmation for this
    // command.`), an ask rule that overrides auto mode, a hook. A box that
    // carries one is the owner's own review (`PromptV2::owner_review`):
    // nothing approves it.
    lit("box.reason_rule", "Permission rule ", "2.1.282"),
    lit(
        "box.reason_confirm",
        "requires confirmation for this ",
        "2.1.282",
    ),
    lit(
        "box.reason_auto_rule",
        "overrides auto mode for this ",
        "2.1.282",
    ),
    lit("box.reason_hook", "Hook ", "2.1.282"),
    lit(
        "box.reason_remote_hook",
        "A hook configured in the remote workspace requires confirmation: ",
        "2.1.282",
    ),
    // The Bash box's `No` under Tab: an input whose placeholder this is,
    // drawn after the label as `No, and tell Claude what to do differently`
    // (2.1.282, measured; the same option in 2.1.280 and 2.1.281). The
    // supervisor's decline types its reason into it.
    lit(
        "prompt.amend_no",
        "and tell Claude what to do differently",
        "2.1.280",
    ),
    hint("prompt.confirm", "Enter to confirm", "2.1.280"),
    lit("box.bash", "Bash command", "2.1.267"),
    lit("box.edit", "Edit file", "2.1.267"),
    lit("box.write", "Write file", "2.1.267"),
    lit("box.create", "Create file", "2.1.267"),
    lit("box.overwrite", "Overwrite file", "2.1.280"),
    lit("box.read", "Read file", "2.1.267"),
    lit("box.workflow", "Run a dynamic workflow?", "2.1.267"),
    // The rest of the permission boxes' titles (the `title` each dialog hands
    // its Ei() box, read from the 2.1.282 render code and present in the
    // 2.1.280, 2.1.281 and 2.1.282 store binaries — 2.1.280 is the earliest
    // build read, so `since` says no more than that). Every box below is
    // HAND-BUILT in the fixtures until a live capture replaces it.
    // `crate::prompt::header_kind` reads a kind from these and nothing else.
    lit("box.powershell", "PowerShell command", "2.1.280"),
    // Two more Edit titles: the notebook edit (no path row; the question
    // names the file) and the IDE diff (`Opened changes in VS Code ⧉`).
    lit("box.edit_notebook", "Edit notebook", "2.1.280"),
    lit("box.ide_diff", "Opened changes in ", "2.1.280"),
    lit("box.ide_diff.mark", "⧉", "2.1.280"),
    // WebFetch's box (Zpe()), the sandbox's network box (uue()) and the
    // Chrome extension's (Xme()): drawn with NO key-hint footer, their
    // refusal the last option, marked ` (esc)`.
    lit("box.fetch", "Fetch", "2.1.280"),
    lit(
        "box.network",
        "Network request outside of sandbox",
        "2.1.280",
    ),
    lit("box.browser", "Claude in Chrome wants to ", "2.1.280"),
    lit("box.skill", "Use skill \"", "2.1.280"),
    lit("box.skill_this", "Use this skill?", "2.1.280"),
    // The Monitor box's title is the Monitor tool's name (`var il =
    // "Monitor"`, the one export of its chunk, imported by Wpe() — the
    // permission_monitor dialog — as its Ei() title; 2.1.282).
    lit("box.monitor", "Monitor", "2.1.280"),
    // The generic tool box (y5n(): an MCP tool) when it is titled so; titled
    // by its card's question or the tool's own name (card mode) it has no
    // anchor and is read by its shape (`crate::prompt::PromptV2::tool_card`).
    lit("box.tool", "Tool use", "2.1.280"),
    // The header's origin suffix after ` · ` (y7() in 2.1.282): `from the
    // "<name>" workflow`, `from a workflow`, `from the <name> agent`, `from
    // a subagent`, `from a remote cloud agent`, `from the <name> plugin`,
    // `from a plugin` — read by `crate::prompt::parse_header` (the named
    // forms' `from the ` is plain English, spelled there: a row here is
    // fenced out of every other crate by tools/grep_guard.sh V1). And the
    // title's own parenthetical (f2e(): `Bash command (unsandboxed)`, `Bash
    // command (runs on <machine>)`; Xo() gives the file boxes the second).
    lit("origin.workflow", "from a workflow", "2.1.280"),
    lit("origin.subagent", "from a subagent", "2.1.280"),
    lit("origin.remote", "from a remote cloud agent", "2.1.280"),
    lit("origin.plugin", "from a plugin", "2.1.280"),
    lit("header.unsandboxed", " (unsandboxed)", "2.1.280"),
    lit("header.runs_on", " (runs on ", "2.1.280"),
    // A footerless box's refusal: its label with a bold `(esc)` appended
    // (`No, and tell Claude what to do differently (esc)`, `Deny (esc)`).
    composed("option.esc", " (esc)", "2.1.280"),
    // The notebook box's questions (`Do you want to insert this cell into
    // a.ipynb?`), beside `prompt.edit`.
    composed(
        "prompt.insert_cell",
        "Do you want to insert this cell into",
        "2.1.280",
    ),
    composed(
        "prompt.delete_cell",
        "Do you want to delete this cell from",
        "2.1.280",
    ),
    // Dialogs that are NOT tool permissions — a supervisor must see them (a
    // footerless one read as idle stalled a session silently) and answers
    // each by its own rule, never as a tool permission's one-shot allow:
    // plan mode in and out, a held cross-session message, a proposed goal,
    // a Computer Use grant (session-only), the setting that blocks reads
    // outside the working directories (persistent).
    lit("plan.enter", "Enter plan mode?", "2.1.280"),
    lit("plan.ready", "Ready to code?", "2.1.280"),
    lit("plan.exit", "Exit plan mode?", "2.1.280"),
    // The plan dialog's own second titled row, under its second rule: a
    // plan taller than the rows read keeps only this one on them.
    lit(
        "plan.proceed",
        "Claude has written up a plan and is ready to execute. Would you like to proceed?",
        "2.1.282",
    ),
    lit("held.title", "Held message from another session", "2.1.280"),
    lit("goal.proposal", "Claude proposes a goal", "2.1.280"),
    lit(
        "computer_use.title",
        "Computer Use wants to control these apps",
        "2.1.280",
    ),
    lit(
        "read_outside.title",
        "Read outside the working directories",
        "2.1.280",
    ),
    // The cache-miss confirmation Claude Code draws before a `/model` or
    // `/effort` change a person typed takes effect on a warm conversation
    // (2.1.283's `$J`, one component for both): its two titles, its confirm
    // (`Yes, switch to <model or effort>`, a template), its refusal, its
    // subtitle — the cost warning — and the subtitle that replaces it when the
    // person's own PreModelSwitch hook asked for the confirmation. MEASURED 2026-09-26 on
    // 2.1.283 (the model form; the others read from the same render code).
    lit("model_switch.title", "Switch model?", "2.1.283"),
    lit("effort_switch.title", "Change effort level?", "2.1.283"),
    lit("model_switch.yes", "Yes, switch to", "2.1.283"),
    lit("model_switch.no", "No, go back", "2.1.283"),
    lit(
        "model_switch.cost",
        "Your next response will be slower and use more tokens",
        "2.1.283",
    ),
    lit(
        "model_switch.hook",
        "A PreModelSwitch hook asked you to confirm",
        "2.1.283",
    ),
    // The question dialog (AskUserQuestion): its free-text option's
    // placeholder (`Type something.`; `Type something` when several answers
    // may be picked), its `Chat about this` row, and its footer's `Enter to
    // select` (its `… to navigate` item is read by its verb: the key words
    // are the bindings', `↑/↓` or `Tab/Arrow keys`). The free-text and chat
    // rows are read by their POSITION in a dialog read whole
    // (`crate::question`); these texts name the kind of a dialog that is not.
    lit("question.other", "Type something", "2.1.280"),
    lit("question.chat", "Chat about this", "2.1.280"),
    hint("question.select", "Enter to select", "2.1.280"),
    // The dialog as 2.1.282 draws it, read whole (`crate::question`; the
    // live captures of 2026-09-25): the several-question footer item and the
    // preview form's (`n to add notes`, `Tab to switch questions`), the
    // preview's notes row (`Notes:` and ` press n to add notes`, two Text
    // runs, so composed), the label mark the tool's prompt asks for, the tab
    // bar's Submit chip (the tick figure and ` Submit`), the review tab's
    // rows, and the AFK countdown row the host may draw under the dialog.
    // The multi-select button (`Next`/`Submit`) and the review's `Cancel`
    // are read by POSITION, never by anchor: one-word anchors are fenced
    // everywhere else in the tree (tools/grep_guard.sh V1) and those words
    // are spelled in aterm-gui and aterm-search.
    lit(
        "question.navigate_tabs",
        "Tab/Arrow keys to navigate",
        "2.1.282",
    ),
    hint("question.switch", "Tab to switch questions", "2.1.282"),
    hint("question.add_notes", "n to add notes", "2.1.282"),
    composed("question.notes", "Notes: press n to add notes", "2.1.282"),
    lit("question.recommended", "(Recommended)", "2.1.281"),
    composed("question.tab_submit", "✔ Submit", "2.1.282"),
    lit("question.review_title", "Review your answers", "2.1.282"),
    lit(
        "question.review_unanswered",
        "You have not answered all questions",
        "2.1.282",
    ),
    lit(
        "question.review_ready",
        "Ready to submit your answers?",
        "2.1.281",
    ),
    lit("question.review_submit", "Submit answers", "2.1.281"),
    lit("question.auto_continue", "auto-continue in ", "2.1.282"),
    lit("question.stay", "any key to stay", "2.1.282"),
    // The tool result's row when a person pressed Esc on the dialog or
    // `2. Cancel` on its review tab (measured 2026-09-25, S7 and S8): the
    // model is told to STOP, so the turn it ends is a person's stop
    // (`crate::turn::interrupted`), never one to continue (R12).
    lit(
        "question.declined",
        "User declined to answer questions",
        "2.1.282",
    ),
    // A dialog WITHHELD while the composer holds a draft (Ufe()'s reason
    // `draft`, 2.1.282), drawn dim in its place; its dash is a `\u2014`
    // escape in the binary, so composed. And the REPL's row while a panel
    // holds the message queue (Y_(): `Background task update` or `<N>
    // background task updates`, then this), which may sit under a dialog.
    composed(
        "dialog.deferred_question",
        "Claude has a question for you — it shows once you send or clear what you're typing.",
        "2.1.282",
    ),
    composed(
        "dialog.deferred_suggestion",
        "Claude has a suggestion for you — it shows once you send or clear what you're typing.",
        "2.1.282",
    ),
    lit(
        "dialog.panel_waiting",
        "waiting while this panel is open",
        "2.1.282",
    ),
    // The vendor's rm/rmdir circuit breaker. Its note is ONE template,
    // `Dangerous ${rm|rmdir} operation ${suffix}` (Uy() in the 2.1.282
    // binary), with seven suffixes; `crate::prompt::rm_breaker_of` reads the
    // whole family. `box.rm_breaker` is the possibly-empty-variable member
    // (the one the supervisor's rm-breaker rule resolves); the rest were
    // read from the 2.1.280/2.1.281/2.1.282 binaries, and
    // `on statically-unresolvable target:` was measured live on 2.1.280
    // (2026-09-24, a workflow subagent's box).
    composed(
        "box.rm_breaker",
        "Dangerous rm operation on possibly-empty variable path",
        "2.1.278",
    ),
    composed("box.rm_breaker.head", "Dangerous rm operation ", "2.1.278"),
    composed(
        "box.rmdir_breaker.head",
        "Dangerous rmdir operation ",
        "2.1.280",
    ),
    lit(
        "box.rm_breaker.empty_var",
        "on possibly-empty variable path",
        "2.1.278",
    ),
    composed(
        "box.rm_breaker.empty_var_in_substitution",
        "on possibly-empty variable path inside command substitution",
        "2.1.280",
    ),
    lit(
        "box.rm_breaker.unresolvable",
        "on statically-unresolvable target: ",
        "2.1.280",
    ),
    lit(
        "box.rm_breaker.critical_path",
        "on critical path: ",
        "2.1.280",
    ),
    lit(
        "box.rm_breaker.working_dir",
        "on working directory or its ancestor: ",
        "2.1.280",
    ),
    lit("box.rm_breaker.drive_root", "on drive root: ", "2.1.281"),
    lit(
        "box.rm_breaker.too_many",
        "too many command substitutions to analyze (",
        "2.1.280",
    ),
    // 2.1.281: a box in a session read as unattended carries a countdown
    // row under its notes (`⚠ Claude Code will automatically deny this
    // request in 1:59, to avoid blocking progress on an unattended
    // session`), the time ticking — its own row, never a note.
    composed(
        "box.auto_deny",
        "Claude Code will automatically deny this request in",
        "2.1.281",
    ),
    // The folder-trust dialog.
    lit("trust.title", "Accessing workspace:", "2.1.280"),
    lit("trust.yes", "Yes, I trust this folder", "2.1.280"),
    lit("trust.no", "No, exit", "2.1.280"),
    // The gated-grants backstop's cancel (the dialog when the folder
    // pre-approves tool permissions, adds directories or mints headers):
    // its Esc and this option decline the grants and go on, never exit.
    lit(
        "trust.no_backstop",
        "No, continue without these permissions",
        "2.1.280",
    ),
    // The live zone.
    lit("busy.interrupt", "esc to interrupt", "2.1.267"),
    // A turn a person stopped with Esc: the vendor's row under the last
    // message or tool row (`  ⎿  Interrupted · What should Claude do
    // instead?`, measured 2026-09-23 on the lane probe's worker).
    lit(
        "turn.interrupted",
        "Interrupted · What should Claude do instead?",
        "2.1.280",
    ),
    lit(
        "survey.question",
        "How is Claude doing this session",
        "2.1.267",
    ),
    lit("context.indicator", "until auto-compact", "2.1.267"),
    // The launch card's name row (`▐▛███▜▌   Claude Code v2.1.282`): a
    // session with no turn yet (`turn::fresh`).
    lit("launch.banner", "Claude Code v", "2.1.267"),
    lit("goal.active", "/goal active", "2.1.278"),
    lit(
        "notice.update",
        "Update installed · Restart to update",
        "2.1.267",
    ),
    lit(
        "model.saved",
        "saved as your default for new sessions",
        "2.1.267",
    ),
    // The walls (see `wall::PHRASES` for the whole classification table).
    lit("wall.fable", "You've reached your Fable limit", "2.1.267"),
    lit("wall.auto_continue", "continuing automatically", "2.1.268"),
    lit("wall.context", "Context limit reached", "2.1.280"),
    lit("wall.server", "This is a server-side issue", "2.1.280"),
    lit(
        "wall.overloaded",
        "Repeated 529 Overloaded errors",
        "2.1.280",
    ),
    lit("wall.login", "Please run /login", "2.1.280"),
    // The vendor's API-error message row opens with it (`phase::error_row_notice`).
    lit("wall.api_error", "API Error", "2.1.283"),
    lit("wall.goal_paused", "Goal paused", "2.1.280"),
    // Claude Code's critical-memory banner, right-aligned on its own row
    // between the spinner and the composer's top rule (`<anchor> (140.4GB) —
    // restart and resume with claude --continue`, measured 2026-09-24 on the
    // incident tab whose worker sat at 38.8 GiB resident and read no input
    // for 2h41m). Placed by `wall::memory_wall`, never by `PHRASES`: a
    // composer draft quoting it carries the same words.
    lit("wall.memory", "Critical memory usage", "2.1.280"),
];

/// Every string aterm guards on in Codex's screen (codex 0.156.1: the
/// captures in [`crate::codex::fixtures`] and the binary).
pub const CODEX_ANCHORS: &[Anchor] = &[
    // The approval boxes.
    lit(
        "codex.box.exec",
        "Would you like to run the following command?",
        "0.156.1",
    ),
    lit(
        "codex.box.patch",
        "Would you like to make the following edits?",
        "0.156.1",
    ),
    lit("codex.box.once", "Yes, proceed", "0.156.1"),
    lit(
        "codex.box.persist",
        "Yes, and don't ask again for commands that start with",
        "0.156.1",
    ),
    lit(
        "codex.box.session",
        "Yes, and don't ask again for these files",
        "0.156.1",
    ),
    lit(
        "codex.box.deny",
        "No, and tell Codex what to do differently",
        "0.156.1",
    ),
    hint(
        "codex.box.confirm",
        "Press enter to confirm or esc to cancel",
        "0.156.1",
    ),
    // The folder-trust gate.
    lit("codex.trust.title", "Folder access", "0.156.1"),
    lit("codex.trust.yes", "Trust and continue", "0.156.1"),
    // Plan mode.
    lit("codex.plan.title", "Implement this plan?", "0.156.1"),
    lit("codex.plan.yes", "Yes, implement this plan", "0.156.1"),
    lit("codex.question.recommended", "(Recommended)", "0.156.1"),
    lit("codex.question.none", "None of the above", "0.156.1"),
    // The live zone and the turn's end.
    hint("codex.busy.interrupt", "esc to interrupt", "0.156.1"),
    lit(
        "codex.composer.placeholder",
        "Ask Codex to do anything",
        "0.156.1",
    ),
    lit("codex.turn.worked", "Worked for", "0.156.1"),
    lit(
        "codex.turn.interrupted",
        "Conversation interrupted - tell the model what to do differently.",
        "0.156.1",
    ),
    lit("codex.context.left", "% context left", "0.156.1"),
    // The tip 0.157.0 draws right-aligned between a turn's end and the
    // composer.
    lit("codex.tip", "Tip: ", "0.157.0"),
    // The walls' reset (`codex::WALLS` is their phrases, canaried beside
    // this table).
    lit("codex.wall.retry_at", "Try again at", "0.156.1"),
];

/// The text of the anchor `id`. Panics on an id the table does not have —
/// a typo in a caller is a bug to find in its first test, not a guard that
/// silently matches nothing.
#[must_use]
pub fn anchor(id: &str) -> &'static str {
    match ANCHORS.iter().chain(CODEX_ANCHORS).find(|a| a.id == id) {
        Some(a) => a.text,
        None => panic!("aterm-phase has no anchor {id:?}"),
    }
}

/// [`anchor`] at COMPILE time, for a caller that needs the text in a
/// `const` (`const NOTE: &str = anchor_text("box.rm_breaker");`): an id the
/// table does not have fails the build, not the first test.
#[must_use]
pub const fn anchor_text(id: &str) -> &'static str {
    const fn same(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }
    let mut i = 0;
    while i < ANCHORS.len() {
        if same(ANCHORS[i].id, id) {
            return ANCHORS[i].text;
        }
        i += 1;
    }
    let mut i = 0;
    while i < CODEX_ANCHORS.len() {
        if same(CODEX_ANCHORS[i].id, id) {
            return CODEX_ANCHORS[i].text;
        }
        i += 1;
    }
    panic!("aterm-phase has no anchor with that id")
}

/// The first `words` words of anchor `id` as a guard regex
/// ([`guard_regex`]): a fence that must still match when the vendor's row
/// runs on past them (`How.is.Claude.doing` over the whole survey question).
#[must_use]
pub fn guard_prefix(id: &str, words: usize) -> String {
    let text = anchor(id);
    let cut = text.split(' ').take(words).map(str::len).sum::<usize>() + words.saturating_sub(1);
    guard_regex(&text[..cut.min(text.len())])
}

/// `text` as a regex that matches it literally in a `key if=` / `await
/// match` guard: every regex metacharacter escaped, and each space spelled
/// `.` because the control verbs split their arguments on whitespace.
#[must_use]
pub fn guard_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            ' ' => out.push('.'),
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_text_is_non_empty() {
        let all: Vec<&Anchor> = ANCHORS.iter().chain(CODEX_ANCHORS).collect();
        for (i, a) in all.iter().enumerate() {
            assert!(!a.text.trim().is_empty(), "{}", a.id);
            assert!(
                all[i + 1..].iter().all(|b| b.id != a.id),
                "duplicate id {}",
                a.id
            );
        }
        // Codex's ids are namespaced, and both accessors read its table.
        for a in CODEX_ANCHORS {
            assert!(a.id.starts_with("codex."), "{}", a.id);
            assert_eq!(anchor(a.id), a.text);
        }
        assert_eq!(anchor("prompt.proceed"), "Do you want to proceed?");
        // The compile-time accessor reads the same table.
        const PROCEED: &str = anchor_text("prompt.proceed");
        assert_eq!(PROCEED, anchor("prompt.proceed"));
        for a in ANCHORS {
            assert_eq!(anchor_text(a.id), a.text, "{}", a.id);
        }
        // grep_guard's V1a refuses a table it reads fewer than 20 texts
        // from; the memory banner's row is one of them.
        assert!(ANCHORS.len() >= 20, "{}", ANCHORS.len());
        assert_eq!(
            ANCHORS
                .iter()
                .find(|a| a.id == "wall.memory")
                .map(|a| a.kind),
            Some(AnchorKind::Literal)
        );
    }

    #[test]
    fn a_guard_prefix_is_the_leading_words_as_a_guard() {
        assert_eq!(guard_prefix("survey.question", 4), "How.is.Claude.doing");
        assert_eq!(guard_prefix("busy.interrupt", 3), "esc.to.interrupt");
        // More words than the anchor has is the whole anchor, never a panic.
        assert_eq!(guard_prefix("busy.interrupt", 9), "esc.to.interrupt");
        assert_eq!(
            guard_prefix("prompt.proceed", 5),
            "Do.you.want.to.proceed\\?"
        );
    }

    #[test]
    #[should_panic(expected = "no anchor")]
    fn an_unknown_id_panics() {
        let _ = anchor("prompt.nope");
    }

    #[test]
    fn a_guard_regex_escapes_metacharacters_and_dots_its_spaces() {
        assert_eq!(
            guard_regex("Do you want to proceed?"),
            "Do.you.want.to.proceed\\?"
        );
        assert_eq!(
            guard_regex("Run a (dynamic) x.y"),
            "Run.a.\\(dynamic\\).x\\.y"
        );
    }

    /// The drift canaries (run by hand when a pinned build moves:
    /// `targo --unverified test -p aterm-phase -- --ignored --nocapture`).
    /// Each reads `$<env>`, else asks `aterm pkg which <program>` for the
    /// store path, and FAILS naming every literal anchor the binary no
    /// longer has — and when it finds no binary, since a canary that cannot
    /// look has not passed. A `·` in an anchor is drawn from a `\xB7`
    /// escape, so each side of it is looked for on its own.
    fn store_binary(program: &str, env: &str) -> (String, Vec<u8>) {
        let path = std::env::var(env).ok().or_else(|| {
            let out = std::process::Command::new("aterm")
                .args(["pkg", "which", program])
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            // `<program> → <shim> → … → <store path> — managed …`
            let first = text.lines().next()?;
            let head = first.split(" — ").next().unwrap_or(first);
            head.rsplit(" → ").next().map(|s| s.trim().to_string())
        });
        let Some(path) = path else {
            panic!("no {program} binary found: set {env}");
        };
        let bytes = std::fs::read(&path).expect("read the binary");
        (path, bytes)
    }

    fn contains(bytes: &[u8], needle: &str) -> bool {
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    /// The literal anchors of `table` missing from `bytes`, and the
    /// [`crate::codex::WALLS`] phrases — which are read lowercased — missing
    /// from them lowercased.
    fn missing(bytes: &[u8], table: &[Anchor], walls: bool) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = table
            .iter()
            .filter(|a| a.kind == AnchorKind::Literal)
            .filter(|a| !a.text.split(" · ").all(|part| contains(bytes, part)))
            .map(|a| a.id)
            .collect();
        if walls {
            let lower = bytes.to_ascii_lowercase();
            out.extend(
                crate::codex::WALLS
                    .iter()
                    .map(|(phrase, _)| *phrase)
                    .filter(|phrase| !contains(&lower, phrase)),
            );
        }
        out
    }

    /// The canary's matcher on a stand-in binary. NEGATIVE CONTROL: a wall
    /// phrase or an anchor the bytes lack is named.
    #[test]
    fn the_canary_names_what_a_binary_lacks() {
        let every: String = CODEX_ANCHORS
            .iter()
            .map(|a| a.text.to_string())
            .chain(crate::codex::WALLS.iter().map(|(p, _)| p.to_uppercase()))
            .collect::<Vec<_>>()
            .join("\0");
        assert!(missing(every.as_bytes(), CODEX_ANCHORS, true).is_empty());
        let short = every.replace("SPEND CAP", "").replace("Folder access", "");
        assert_eq!(
            missing(short.as_bytes(), CODEX_ANCHORS, true),
            vec!["codex.trust.title", "spend cap"]
        );
    }

    fn literal_anchors_are_in_the_store(program: &str, env: &str, table: &[Anchor], walls: bool) {
        let (path, bytes) = store_binary(program, env);
        let missing = missing(&bytes, table, walls);
        assert!(
            missing.is_empty(),
            "{path}: {} literal anchors or wall phrases missing: {missing:?}",
            missing.len()
        );
        eprintln!("{path}: every literal anchor (and wall phrase) is present");
    }

    #[test]
    #[ignore = "reads the installed claude binary"]
    fn every_literal_anchor_is_in_the_store_claude() {
        literal_anchors_are_in_the_store("claude", "ATERM_CLAUDE_BIN", ANCHORS, false);
    }

    #[test]
    #[ignore = "reads the installed codex binary"]
    fn every_literal_codex_anchor_is_in_the_store_codex() {
        literal_anchors_are_in_the_store("codex", "ATERM_CODEX_BIN", CODEX_ANCHORS, true);
    }
}

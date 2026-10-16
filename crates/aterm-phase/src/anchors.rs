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
//! Each other agent's strings are a table of its own — [`CODEX_ANCHORS`]
//! (ids `codex.…`), [`AGY_ANCHORS`] (`agy.…`), [`MUSE_ANCHORS`] (`muse.…`);
//! [`anchor`] and [`anchor_text`] read them all.
//!
//! The tests `every_literal_anchor_is_in_the_store_claude`,
//! `every_literal_codex_anchor_is_in_the_store_codex`,
//! `every_literal_agy_anchor_is_in_the_store_agy` and
//! `every_literal_muse_anchor_is_in_the_store_muse` grep the store's
//! binaries for every [`AnchorKind::Literal`] row (the Codex one for every
//! [`crate::codex::WALLS`] phrase and [`crate::codex::TOOL_HEADS`] head too)
//! and name the misses — the drift canaries. They RUN in the suite (`verify
//! test` included) wherever a store binary is found (`$ATERM_CLAUDE_BIN`,
//! `$ATERM_CODEX_BIN`, `$ATERM_AGY_BIN`, `$ATERM_MUSE_BIN`, else `aterm pkg
//! which <program>`), so a new build the vendor's auto-updater or `aterm pkg
//! update` brought is judged by the next test run, and say so loudly and
//! return where there is none — a canary that could not look has not passed,
//! and is not counted as having. None is `#[ignore]`d: an ignored test is run
//! by nobody.

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
    // A remote session's hook, as the head and the tail every build shares:
    // 2.1.282 wrote `A hook configured in the remote workspace requires
    // confirmation: <reason>`, 2.1.287 `A hook run for this remote session
    // requires confirmation: <reason>` (read from both binaries' strings).
    // The old sentence whole matched no 2.1.287 box.
    lit("box.reason_remote_hook", "A hook ", "2.1.282"),
    lit(
        "box.reason_remote_confirm",
        "requires confirmation: ",
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
    // The note 2.1.291 appends to the FOOTER of a permission box a SUBAGENT or a
    // workflow's agent raised (`ZTe()`, for an async agent: every subagent the lab
    // started drew it, `run_in_background` true, absent or false): the kill-agents
    // chord and an action joined by one space, where every other hint says ` to `
    // (`terse`) — `Esc to cancel · Tab to amend · ctrl+x ctrl+k twice to stop
    // background agents`, `again` once the first press is in. The chord is the
    // user's binding (`chat:killAgents`). MEASURED on 2.1.291 and 2.1.292
    // (`claude-2.1.292-box-bash-background-agent.txt`): before it the footer was
    // read as no footer at all, and the box as no box.
    lit(
        "hint.agents_twice",
        "twice to stop background agents",
        "2.1.291",
    ),
    lit(
        "hint.agents_again",
        "again to stop background agents",
        "2.1.291",
    ),
    lit("box.bash", "Bash command", "2.1.267"),
    lit("box.edit", "Edit file", "2.1.267"),
    lit("box.write", "Write file", "2.1.267"),
    lit("box.create", "Create file", "2.1.267"),
    lit("box.overwrite", "Overwrite file", "2.1.280"),
    lit("box.read", "Read file", "2.1.267"),
    // The folder grant on a Read box for a folder OUTSIDE Claude Code's working
    // set (`Yes, allow reading from /etc and /private/etc during this session`;
    // 2.1.287: `… from this project`): none on a box for a path inside it.
    lit("box.read_grant", "allow reading from ", "2.1.282"),
    // A Read box's call, `Read(<path>)` (the tool's name and its argument,
    // joined at draw time): alone under the title in 2.1.284, between two
    // dashed rules from 2.1.287.
    composed("box.read_call", "Read(", "2.1.284"),
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
    // imported by id everywhere else). And the
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
    // are read by POSITION, never by anchor: those words are spelled in
    // aterm-gui and aterm-search.
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
    // (`crate::turn::interrupted`), never one to continue.
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
    // The usage-limit options dialog (the 2.1.282 binary's
    // `rate_limit_options_menu`): its title, its "stop" row, and the one row
    // the supervisor may choose — `Wait here, then continue automatically`
    // followed by ` shortly`, ` at <time>` or ` when the limit resets`.
    lit("limit.title", "What do you want to do?", "2.1.282"),
    lit("limit.stop", "Stop and wait for limit to reset", "2.1.282"),
    lit(
        "limit.wait",
        "Wait here, then continue automatically",
        "2.1.282",
    ),
    // The live zone.
    lit("busy.interrupt", "esc to interrupt", "2.1.267"),
    // The pill-less footer row's left hint slot under the composer (2.1.283
    // render code: `? for shortcuts` at rest, the escape key's `clear` hint
    // with a draft) — a live REPL's own row where no mode pill is drawn
    // (`aterm_agent::harness::footer::live_repl_row`).
    lit("footer.shortcuts", "? for shortcuts", "2.1.283"),
    hint("footer.clear", "esc to clear", "2.1.283"),
    // The permission-mode pill on that row: a glyph, the mode's indicator and
    // ` on`, joined at run time (`⏵⏵ bypass permissions on`, `⏸ manual mode
    // on`), then the cycle key's hint (`(shift+tab to cycle)`). The footer and
    // the mode light read the mode from it (`aterm_agent::harness::footer`).
    composed("mode.bypass", "bypass permissions on", "2.1.283"),
    composed("mode.auto", "auto mode on", "2.1.283"),
    composed("mode.accept_edits", "accept edits on", "2.1.283"),
    composed("mode.dont_ask", "don't ask on", "2.1.283"),
    composed("mode.plan", "plan mode on", "2.1.283"),
    composed("mode.manual", "manual mode on", "2.1.283"),
    hint("mode.cycle", "to cycle", "2.1.283"),
    // A turn a person stopped with Esc: the vendor's row under the last
    // message or tool row (`  ⎿  Interrupted · What should Claude do
    // instead?`, measured 2026-09-23 on the lane probe's worker).
    lit(
        "turn.interrupted",
        "Interrupted · What should Claude do instead?",
        "2.1.280",
    ),
    // The notice Claude Code appends when it compacts an idle conversation by
    // itself (`compact_idle`, mode `compact`: nobody at the tab, no turn in
    // flight), as a `⏺` row of its own (measured 2026-10-15 on two tabs).
    lit(
        "turn.idle_compacted",
        "Compacted while idle, before the prompt cache expired",
        "2.1.300",
    ),
    // The label of the summary Claude Code appends to an idle conversation
    // (`※ recap: <summary> (disable recaps in /config)`, its `※` drawn from a
    // `※` escape, the summary wrapped two columns in; measured 2026-10-15).
    lit("turn.recap", "recap:", "2.1.301"),
    lit(
        "survey.question",
        "How is Claude doing this session",
        "2.1.267",
    ),
    // The rest of the rating cards, drawn by the same component as the session
    // survey (`VM` in 2.1.284) in the same slot above the composer, each with the
    // options row `1: Bad … 0: Dismiss` under its question. Read from the render
    // code and present in the 2.1.284 store binary; HAND-BUILT in the fixtures until
    // a capture replaces them (only the session survey has one). `survey.plugin` is
    // the head both of its spellings share (`How helpful has the <plugin> plugin
    // been?`, `How helpful has this plugin been?`); the memory card is either of the
    // two heads, the recollection row being its last message row.
    lit(
        "survey.long_context",
        "How well is Claude following the instructions you gave earlier in this conversation",
        "2.1.284",
    ),
    lit("survey.plugin", "How helpful has", "2.1.284"),
    lit("survey.memory", "Claude recalled a memory:", "2.1.284"),
    lit(
        "survey.memory_plain",
        "How well did Claude use its memory?",
        "2.1.284",
    ),
    lit(
        "survey.memory_recollection",
        "How was Claude's recollection?",
        "2.1.284",
    ),
    // The rating's own aftermath, drawn in the card's slot after a person's `1`/`2`/`3`
    // (`t1e` and `o1e` in 2.1.286, MEASURED on it 2026-10-01): for 3 s `Feedback: <Bad |
    // Fine | Good | Unsure> · Esc to undo` (the key word is the user's `escape` binding),
    // then for 5 s `Thanks for the feedback!` over `(Optional) Press [1] to tell us what
    // went wrong · /feedback` — or, where the follow-up is off, `Use /feedback to share
    // detailed feedback anytime.` (the render code's other branch, not captured).
    lit("survey.pending", "Feedback: ", "2.1.286"),
    hint("survey.pending_undo", "Esc to undo", "2.1.286"),
    lit("survey.thanks", "Thanks for the feedback!", "2.1.286"),
    lit("survey.optional_press", "(Optional) Press [", "2.1.286"),
    lit(
        "survey.feedback_anytime",
        "Use /feedback to share detailed feedback anytime.",
        "2.1.286",
    ),
    // The Claude-drafted-feedback card (`SendFeedback`, on for accounts whose flag
    // is): parked in the same slot, NOT a survey — the readers step over it and
    // nothing presses it. Its head is `<glyph> <Label> drafted: <title>`; its hint
    // row is composed from key hints.
    lit("card.drafted", "drafted:", "2.1.284"),
    hint(
        "card.drafts_hint",
        "1 to review · 2 to send · 0 to dismiss",
        "2.1.284",
    ),
    // The `Don't ask again` choice the transcript-share ask (`y: Yes    n: No     d:
    // Don't ask again`, `Ele` in 2.1.284) and the web-setup offer share: the label
    // that tells those prompts from a rating card, whose options row ends `0: Dismiss`.
    // Read from the render code and present in the 2.1.284 store binary; never pressed.
    lit("card.dont_ask", "Don't ask again", "2.1.284"),
    lit("context.indicator", "until auto-compact", "2.1.267"),
    // The launch card's name row (`▐▛███▜▌   Claude Code v2.1.282`): a
    // session with no turn yet (`turn::fresh`).
    lit("launch.banner", "Claude Code v", "2.1.267"),
    // What an EXITING Claude Code prints under its last frame, alone on its
    // row in column 0 (`\nResume this session with:\nclaude --resume <id>`,
    // the 2.1.283 binary's resume hint; measured after `/exit` and after
    // Ctrl-C twice, 2026-09-27): every row above it was drawn by a process
    // that has exited (`phase::busy_zone_start`).
    lit("exit.resume", "Resume this session with:", "2.1.283"),
    lit("goal.active", "/goal active", "2.1.278"),
    // The model-refusal pause (`refusal_fallback_prompt`, read from the
    // 2.1.282 render code; both strings in the 2.1.283 binary): its title,
    // and its second option's head (`Edit prompt and retry with <model>`).
    lit("paused.title", "Session paused", "2.1.282"),
    lit("paused.edit", "Edit prompt and retry", "2.1.282"),
    lit(
        "notice.update",
        "Update installed · Restart to update",
        "2.1.267",
    ),
    // The vendor's OWN `⏺` paragraphs that are nobody's words: drawn as a
    // message row in the transcript, but no turn's reply, so the readers of
    // what the worker last said step over them (`phase::vendor_paragraph`).
    // The auto-mode notice (`getAutoModeDescription` in 2.1.300, its head
    // `… automatically \u2014 Claude checks each tool call …` read from the
    // binary): drawn after the first reply of a session relaunched
    // `--permission-mode auto --resume <id>` (MEASURED on 2.1.300, the
    // harness sweep of 2026-10-15, ATERM-KI-1015-1 LC-10), and over the
    // first turn of a session in auto mode (2.1.287's
    // `claude-2.1.287-box-bash-ask-rule-auto.txt`).
    lit(
        "notice.auto_mode",
        "Auto mode lets Claude handle permission prompts automatically",
        "2.1.287",
    ),
    lit(
        "model.saved",
        "saved as your default for new sessions",
        "2.1.267",
    ),
    // The RESULTS of Claude Code's own model and effort commands, as its
    // transcript records them under `<local-command-stdout>` — what aterm's
    // footer names the model and effort by the moment they are chosen
    // (`aterm_agent::harness::footer`). Read from the 2.1.283 binary
    // (`UEe`/`BEe`, the `/effort` messages, `mrt`/`grt`); `Set model to ` from
    // the 2.1.201 transcripts on (its ANSI-bold form, then backticks).
    lit("model.set", "Set model to ", "2.1.201"),
    lit("model.kept", "Kept model as ", "2.1.283"),
    lit("effort.set", "Set effort level to ", "2.1.283"),
    lit("effort.current", "Current effort level: ", "2.1.283"),
    lit("effort.auto", "Effort level set to auto", "2.1.283"),
    lit("fast.on", "Fast mode ON", "2.1.283"),
    lit("fast.model_set", "model set to ", "2.1.283"),
    // The Usage tab of the Settings panel (`/usage`), for the accounts probe
    // (`crate::panel`, `docs/SPEC-claude-accounts-probe-2026-10-01.md`). READ
    // in the 2.1.287 bundle's plan-limits component: the loading line and
    // the refresh line (each `…`-ended on the screen; the bundle spells the
    // `…` as an escape, so the rows carry the words without it), the four
    // enrich-error lines' heads, the failed load's reason (drawn after
    // `Error: `), and the result the panel closes with on Escape. MEASURED
    // on a 2.1.287 screen, 2026-10-01: `Refreshing…` above the cancel hint
    // (`prompt::fixtures::USAGE_REFRESHING`), and `⎿  Settings dialog
    // dismissed` under `❯ /usage` after the Escape under the INLINE renderer
    // (the fullscreen one leaves no row); the rest were not seen.
    // `usage.refreshing` is one word: a matcher reads it through
    // `crate::anchor`, never as a quoted copy.
    lit("usage.refreshing", "Refreshing", "2.1.287"),
    lit("usage.loading", "Loading usage data", "2.1.287"),
    lit("usage.last_known", "Showing last-known usage", "2.1.287"),
    lit(
        "usage.could_not_refresh",
        "Could not refresh usage data",
        "2.1.287",
    ),
    lit("usage.partial", "Partial usage data", "2.1.287"),
    lit(
        "usage.per_model",
        "Per-model breakdown unavailable",
        "2.1.287",
    ),
    lit("usage.load_failed", "Failed to load usage data", "2.1.287"),
    lit("usage.dismissed", "Settings dialog dismissed", "2.1.287"),
    // The limit reset (`crate::reset`,
    // `docs/SPEC-claude-accounts-resets-2026-10-02.md`). EVERY ROW IS READ
    // in the 2.1.287 bundle and NONE WAS SEEN on a screen or in a
    // transcript: the offer's texts are one table there (`lg`), the pending
    // lines and the answers their own literals, and the server's flag
    // payload may replace any text of the table. The rows are the words the
    // readers match by, never a whole template: the grant row's head and the
    // words between its count and its day (`Use your reset now ({left} left,
    // until {date})`); the weekly session reset's row, which the vendor
    // draws in the grant row's slot; the confirmation's title, the head of
    // its body, the two joins of body and detail (`Refills your {limits} now
    // · your weekly reset day stays {week}`, `{resets} left · use by
    // {deadline}`), its yes and its no, and the tail both early lines share
    // (`… — use your reset anyway?`); the two pending lines, each `…`-ended
    // on the screen (an escape in the bundle, so the rows carry the words
    // without it); and of the answers the four a reader tells apart — the
    // spent line's head, the two lifted lines, the kept line's head, the
    // login line's — with the ending the vendor adds where it queued no
    // continuation of its own. The other answers are no rows on purpose: a
    // line no row names is a failure.
    lit("reset.row", "Use your reset now", "2.1.287"),
    lit("reset.row.until", "left, until", "2.1.287"),
    lit(
        "reset.weekly_row",
        "Reset your session limit now",
        "2.1.287",
    ),
    lit("reset.confirm.title", "Use your reset?", "2.1.287"),
    lit("reset.confirm.refills", "Refills your", "2.1.287"),
    lit(
        "reset.confirm.stays",
        "now · your weekly reset day stays",
        "2.1.287",
    ),
    lit("reset.confirm.use_by", "left · use by", "2.1.287"),
    lit("reset.yes", "Yes, use my reset", "2.1.287"),
    lit("reset.no", "No, keep it", "2.1.287"),
    lit("reset.early", "use your reset anyway?", "2.1.287"),
    lit("reset.checking", "Checking for a reset", "2.1.287"),
    lit("reset.resetting", "Resetting your limits", "2.1.287"),
    lit("reset.done", "Limits reset", "2.1.287"),
    lit(
        "reset.lifted",
        "Your limit had already reset on its own · nothing was used",
        "2.1.287",
    ),
    lit("reset.clear", "Your limits are clear", "2.1.287"),
    lit("reset.kept", "Reset kept", "2.1.287"),
    lit(
        "reset.login",
        "Couldn't reset your limits with this login",
        "2.1.287",
    ),
    lit("reset.continue", "send a message to continue", "2.1.287"),
    // The walls (see `wall::PHRASES` for the whole classification table).
    lit("wall.fable", "You've reached your Fable limit", "2.1.267"),
    lit("wall.auto_continue", "continuing automatically", "2.1.268"),
    // How that auto-continue ENDED, as the vendor's notice opens
    // (`wall::auto_continue_end`): it went on — at the reset, or before it
    // when the usage came back — or it will not go on by itself: Esc on its
    // countdown (captured on 2.1.296), stopped or turned off, or a reset
    // that passed with nothing sent.
    lit(
        "wall.auto_continue.fired",
        "Usage limit reset · continuing automatically",
        "2.1.283",
    ),
    lit(
        "wall.auto_continue.now",
        "Usage limit available again · continuing now",
        "2.1.284",
    ),
    lit(
        "wall.auto_continue.cancelled",
        "Automatic continue cancelled",
        "2.1.296",
    ),
    lit(
        "wall.auto_continue.stopped",
        "Automatic continue stopped",
        "2.1.299",
    ),
    lit(
        "wall.auto_continue.off",
        "Automatic continue was turned off",
        "2.1.299",
    ),
    lit(
        "wall.auto_continue.stale",
        "Usage limit has reset · press enter to continue",
        "2.1.283",
    ),
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
    // The other /goal echoes, and the cause the loop reads through them.
    lit("wall.goal_still", "Goal still active", "2.1.283"),
    lit(
        "wall.goal_cleared",
        "Goal cleared after an unrecoverable error",
        "2.1.283",
    ),
    lit(
        "wall.goal_auto",
        "continues automatically when it resets",
        "2.1.283",
    ),
    // The head of the vendor's word that a `/compact` could not lower the context (`Compaction failed · conversation
    // could not be reduced below the context limit`); the wall table reads the same row (`wall::PHRASES`).
    lit("wall.compaction_failed", "Compaction failed", "2.1.286"),
    // Claude Code's critical-memory banner, right-aligned on its own row
    // between the spinner and the composer's top rule (`<anchor> (140.4GB) —
    // restart and resume with claude --continue`, measured 2026-09-24 on the
    // incident tab whose worker sat at 38.8 GiB resident and read no input
    // for 2h41m). Placed by `wall::memory_wall`, never by `PHRASES`: a
    // composer draft quoting it carries the same words.
    lit("wall.memory", "Critical memory usage", "2.1.280"),
    // Usage credits carry the session (the 2.1.300 bundle's own list of these
    // notices, measured in the lab): the footer's lasting item, the
    // notification as they start, and the same two under an admin's
    // allocation. Read by place, `credits::in_use`.
    lit("credits.in_use", "Now using usage credits", "2.1.300"),
    lit(
        "credits.started",
        "You're now using usage credits",
        "2.1.300",
    ),
    lit(
        "credits.allocation",
        "Now using your usage allocation",
        "2.1.300",
    ),
    lit(
        "credits.allocation_started",
        "You're now using your usage allocation",
        "2.1.300",
    ),
    // The vendor's own warning that a window is filling (`You've used 80% of
    // your weekly limit · resets Oct 18 at 8:50pm (…)`, measured):
    // `credits::usage_warning`.
    lit("usage.warning", "You've used", "2.1.300"),
    // Claude Code's own word that the stored login ends soon (READ in the
    // 2.1.300 bundle, the `oauth-expiry` warning notification: `Your login
    // expires in ` + N + ` day`/`days`, then ` · run /login to renew` drawn
    // dim; shown only while the refresh token's end is 3 days away or
    // nearer, and the same words as a footer segment at 1 day or less).
    // Read by `wall::login_expires_in` for the login's forecast; its screen
    // placement was NOT measured.
    lit("login.expires_in", "Your login expires in ", "2.1.300"),
    lit("login.renew_hint", "run /login to renew", "2.1.300"),
];

/// Every string aterm guards on in Codex's screen (codex 0.156.1 to
/// 0.163.0: the captures in [`crate::codex::fixtures`] and the binary).
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
    // The exec box's Reason row when an owner's prompt rule sent it
    // (`prefix_rule(…, decision="prompt")`, MEASURED on 0.163.0): `` `<the
    // shell-wrapped command>` requires approval by policy``, or `` … requires
    // approval: <the rule's justification>`` — `PromptV2::owner_review`.
    lit("codex.box.policy", "requires approval by policy", "0.163.0"),
    lit("codex.box.policy_why", "requires approval: ", "0.163.0"),
    // The `Usage limit reached` banner a workspace member is drawn under the
    // out-of-credits and spend-cap walls (MEASURED on 0.162.1 and 0.163.0):
    // its `Yes` asks the workspace's owner for a limit increase, and no rule
    // presses it.
    lit("codex.banner.request", "Request increase?", "0.162.1"),
    // The session card's title, `>_ OpenAI Codex (v<version>)`; the `(v`
    // is the literal's, the version the build's.
    lit("codex.card.title", "OpenAI Codex (v", "0.156.1"),
    // The folder-trust gate.
    lit("codex.trust.title", "Folder access", "0.156.1"),
    lit("codex.trust.yes", "Trust and continue", "0.156.1"),
    // Plan mode.
    lit("codex.plan.title", "Implement this plan?", "0.156.1"),
    lit("codex.plan.yes", "Yes, implement this plan", "0.156.1"),
    lit("codex.question.recommended", "(Recommended)", "0.156.1"),
    lit("codex.question.none", "None of the above", "0.156.1"),
    // The question's NOTES FIELD (MEASURED on 0.163.0): Tab on `None of the
    // above` opens it, drawn `  › Add notes` while empty, the dialog's
    // footer offering it (`tab to add notes | …`) and, open, its clearing
    // (`tab or esc to clear notes | …`).
    lit("codex.question.notes", "Add notes", "0.163.0"),
    hint("codex.question.notes.tab", "tab to add notes", "0.163.0"),
    hint(
        "codex.question.notes.open",
        "tab or esc to clear notes",
        "0.163.0",
    ),
    // The live zone and the turn's end.
    hint("codex.busy.interrupt", "esc to interrupt", "0.156.1"),
    lit(
        "codex.composer.placeholder",
        "Ask Codex to do anything",
        "0.156.1",
    ),
    lit("codex.turn.worked", "Worked for", "0.156.1"),
    // The head of the Esc row, which is all 0.156.1 and 0.159.0 share: the
    // first went on `tell the model what to do differently.`, the second
    // (its binary's strings) on `use /feedback if something went wrong`.
    lit(
        "codex.turn.interrupted",
        "Conversation interrupted - ",
        "0.156.1",
    ),
    lit("codex.context.left", "% context left", "0.156.1"),
    // The tip 0.157.0 draws right-aligned between a turn's end and the
    // composer.
    lit("codex.tip", "Tip: ", "0.157.0"),
    // What a TUI prints on the main screen as it LEAVES, which the live
    // upgrade takes the thread it relaunches from (`aterm-agent`'s
    // `upgrade_codex::parse_exit_hint`): the heading over an embedded
    // session's `  codex resume <id>` row, that row's words and the token
    // tally printed with it (MEASURED after `/quit`,
    // [`crate::codex::fixtures::EXIT`]); the daemon-mode hint's last line
    // (0.157.0) and, from 0.159, its heading over the same row (the 0.159.3
    // and 0.160.0 strings). 0.157's one-line `Reconnect: codex resume <id>`
    // is in no build since, so it is no anchor: the upgrade reads it as
    // history. A reworded one leaves a daemon-mode client's thread unnamed
    // after `/exit` was typed, and the upgrade fails `no-resume-hint` at a
    // shell.
    lit("codex.exit.continue", "To continue this session", "0.156.1"),
    lit("codex.exit.resume", "codex resume ", "0.156.1"),
    lit("codex.exit.tokens", "Token usage", "0.156.1"),
    lit("codex.exit.stop", "Stop the current turn:", "0.157.0"),
    lit("codex.exit.reconnect", "To reconnect", "0.159.3"),
    // The walls' reset (`codex::WALLS` is their phrases, canaried beside
    // this table).
    lit("codex.wall.retry_at", "Try again at", "0.156.1"),
    // The rate-limit model nudge (view `rate-limit-switch-prompt`, the same
    // strings in the 0.157.1 and 0.158.0 binaries): its title and its two
    // keeps. Its switch (`Switch to <model>`) is composed at run time and
    // matched by its label's words in the decider, never anchored here. The
    // never-show-again keep writes `[notice] hide_rate_limit_model_nudge =
    // true` into Codex's own config.
    lit("codex.nudge.title", "Approaching rate limits", "0.157.1"),
    lit("codex.nudge.keep", "Keep current model", "0.157.1"),
    lit(
        "codex.nudge.never",
        "Keep current model (never show again)",
        "0.157.1",
    ),
    // The `/model` picker (MEASURED 2026-09-28 on 0.158.0, a private
    // headless aterm): the model box, the effort box (titled with the model
    // it sets), its `More reasoning…` row (composed: the ellipsis is drawn
    // after the words) and the advanced box it opens. The effort boxes'
    // footer offers `s session` beside `enter default`: Enter SAVES the
    // choice as Codex's default, `s` applies it to this conversation only.
    // The hint is anchored with the footer's tail (`… · s session · esc
    // back`, under `enter default` and `enter apply` alike): the two words
    // alone are a substring of every `this session` in the tree.
    lit("codex.pick.model", "Select Model and Effort", "0.158.0"),
    lit("codex.pick.effort", "Select Reasoning Level for", "0.158.0"),
    lit("codex.pick.advanced", "Advanced Reasoning", "0.158.0"),
    composed("codex.pick.more", "More reasoning…", "0.158.0"),
    hint("codex.pick.session", "s session · esc back", "0.158.0"),
    // The footer's goal states (the 0.157.1 and 0.158.0 binaries; `Pursuing
    // goal (10d 3h 2m)` MEASURED on a live 0.158.0 tab, read only).
    lit("codex.goal.pursuing", "Pursuing goal (", "0.157.1"),
    lit("codex.goal.paused", "Goal paused (/goal resume)", "0.157.1"),
    lit(
        "codex.goal.limited",
        "Goal hit usage limits (/goal resume)",
        "0.157.1",
    ),
    lit(
        "codex.goal.stalled",
        "Goal stalled (/goal resume)",
        "0.157.1",
    ),
    // A goal that ENDED (read from the vendor's source at rust-v0.157.1 and
    // rust-v0.158.0, `tui/src/bottom_pane/footer.rs`
    // `goal_status_indicator_line`: `Complete` → `Goal achieved (<usage>)` or
    // `Goal achieved`, `BudgetLimited` → `Goal unmet (<usage>)` or `Goal
    // abandoned`; every string in both store binaries): the positive reading
    // of a goal no longer paused nor pursued.
    lit("codex.goal.achieved", "Goal achieved", "0.157.1"),
    lit("codex.goal.unmet", "Goal unmet (", "0.157.1"),
    lit("codex.goal.abandoned", "Goal abandoned", "0.157.1"),
    // The box `codex resume` draws over a PAUSED goal (the 0.157.1 and
    // 0.158.0 binaries; openai/codex `tui/src/chatwidget/goal_menu.rs`
    // `show_resume_paused_goal_prompt`, drawn by `app/startup.rs` for a
    // `codex resume <thread>` with no prompt whose goal is paused, blocked or
    // usage-limited): its title, and its two options — the first focused,
    // which sends the same `thread/goal/set … active` as `/goal resume`, and
    // the one that leaves it paused.
    lit("codex.goal.resume.title", "Resume paused goal?", "0.157.1"),
    lit("codex.goal.resume.yes", "Resume goal", "0.157.1"),
    lit("codex.goal.resume.leave", "Leave paused", "0.157.1"),
];

/// Every string aterm guards on in agy's screen (agy 1.4.0: the captures in
/// [`crate::agy::fixtures`] and the binary).
pub const AGY_ANCHORS: &[Anchor] = &[
    // The footer's left side, and the key row under a box or a menu (`↑/↓`
    // and `Navigate` are two strings in the binary).
    lit("agy.footer.idle", "? for shortcuts", "1.4.0"),
    lit("agy.footer.cancel", "esc to cancel", "1.4.0"),
    hint("agy.footer.exit", "press ctrl+c again to exit", "1.4.0"),
    hint("agy.keys.navigate", "↑/↓ Navigate", "1.4.0"),
    // The logo row naming the build, and the turn's end after an Esc or a
    // refused box.
    lit("agy.banner", "Antigravity CLI", "1.4.0"),
    lit("agy.turn.interrupted", "Interrupted · What should", "1.4.0"),
    // The sign-in wall: no option of its menu is ever pressed (its first
    // opens a browser).
    lit(
        "agy.signin.wall",
        "You are currently not signed in.",
        "1.4.0",
    ),
    lit("agy.signin.method", "Select login method:", "1.4.0"),
    lit("agy.signin.oauth", "Google OAuth", "1.4.0"),
    // The workspace-trust box.
    lit(
        "agy.trust.title",
        "Do you trust the contents of this project?",
        "1.4.0",
    ),
    lit("agy.trust.yes", "Yes, I trust this folder", "1.4.0"),
    lit("agy.trust.no", "No, exit", "1.4.0"),
    // The boxes' titles.
    lit("agy.title.command", "Command", "1.4.0"),
    lit("agy.title.create", "Create file", "1.4.0"),
    lit("agy.title.edit", "Pending edit", "1.4.0"),
    lit("agy.title.question", "Question", "1.4.0"),
    // The command box.
    lit("agy.box.command.for", "Requesting permission for:", "1.4.0"),
    lit("agy.box.command.ask", "Run this command?", "1.4.0"),
    lit("agy.box.once", "Yes, run command", "1.4.0"),
    lit(
        "agy.box.session",
        "Yes, and always allow in this conversation for commands that start with",
        "1.4.0",
    ),
    lit(
        "agy.box.persist",
        "Yes, and always allow for commands that start with",
        "1.4.0",
    ),
    lit("agy.box.deny", "No, cancel", "1.4.0"),
    // The file boxes.
    lit("agy.create.reason", "Reason:", "1.4.0"),
    lit("agy.create.ask", "Allow creation of this file?", "1.4.0"),
    lit("agy.edit.ask", "Accept this file edit?", "1.4.0"),
    lit("agy.create.once", "Yes, allow creation", "1.4.0"),
    lit(
        "agy.create.persist",
        "Yes, and always allow non-workspace access",
        "1.4.0",
    ),
    lit("agy.create.deny", "No, deny creation", "1.4.0"),
    lit("agy.edit.once", "Yes, accept this change", "1.4.0"),
    lit("agy.edit.deny", "No, reject this change", "1.4.0"),
    // The question's row for the person's own words.
    lit("agy.question.write_in", "Write-in...", "1.4.0"),
];

/// Every string aterm guards on in Muse Code's screen (muse 1.4.9-R6297.1:
/// the captures in [`crate::muse::fixtures`] and the binary).
pub const MUSE_ANCHORS: &[Anchor] = &[
    // The composer's labelled top rule, the status row's hint, the banner.
    lit(
        "muse.composer.voice",
        "Voice input (⌥ + v to start)",
        "1.4.9-R6297.1",
    ),
    lit("muse.busy.interrupt", "esc to interrupt", "1.4.9-R6297.1"),
    lit("muse.banner", "Muse Code", "1.4.9-R6297.1"),
    // The sign-in wall: no option of it is ever pressed (its first opens a
    // browser).
    lit(
        "muse.signin.browser",
        "Log in with browser",
        "1.4.9-R6297.1",
    ),
    lit(
        "muse.signin.keys",
        "↓↑ to select · Esc to quit",
        "1.4.9-R6297.1",
    ),
    // The workspace-trust box.
    lit(
        "muse.trust.title",
        "Do you trust this workspace?",
        "1.4.9-R6297.1",
    ),
    lit("muse.trust.path", "Workspace:", "1.4.9-R6297.1"),
    lit("muse.trust.yes", "Trust and continue", "1.4.9-R6297.1"),
    lit("muse.trust.quit", "Quit", "1.4.9-R6297.1"),
    lit("muse.trust.keys", "Esc quits.", "1.4.9-R6297.1"),
];

/// The text of the anchor `id`. Panics on an id the table does not have —
/// a typo in a caller is a bug to find in its first test, not a guard that
/// silently matches nothing.
#[must_use]
pub fn anchor(id: &str) -> &'static str {
    match ANCHORS
        .iter()
        .chain(CODEX_ANCHORS)
        .chain(AGY_ANCHORS)
        .chain(MUSE_ANCHORS)
        .find(|a| a.id == id)
    {
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
    let mut i = 0;
    while i < AGY_ANCHORS.len() {
        if same(AGY_ANCHORS[i].id, id) {
            return AGY_ANCHORS[i].text;
        }
        i += 1;
    }
    let mut i = 0;
    while i < MUSE_ANCHORS.len() {
        if same(MUSE_ANCHORS[i].id, id) {
            return MUSE_ANCHORS[i].text;
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

/// Whether `bytes` are a build of a program rather than a stub: a Mach-O or
/// ELF header, or at least [`MIN_BUILD_BYTES`]. A reroute stub
/// (`<prefix>/reroute/<program>`, a few lines of shell) answers `aterm pkg
/// which` on a box whose resolved token is the stub itself; it holds none of
/// the anchors, so "missing" would be the stub's, not the build's.
#[cfg(test)]
pub(crate) fn looks_like_a_build(bytes: &[u8]) -> bool {
    const MAGICS: [[u8; 4]; 6] = [
        [0xfe, 0xed, 0xfa, 0xce],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
        [0x7f, b'E', b'L', b'F'],
    ];
    bytes.len() >= MIN_BUILD_BYTES
        || bytes
            .get(..4)
            .is_some_and(|head| MAGICS.iter().any(|magic| magic == head))
}

/// Below this, a file that is no native executable is a stub script.
#[cfg(test)]
const MIN_BUILD_BYTES: usize = 4 * 1024 * 1024;

/// Whether `needle` occurs in `hay` (Boyer-Moore-Horspool: a 225 MB binary
/// is scanned in a fraction of a second per needle, where a window-by-
/// window compare took most of a minute for the table).
#[cfg(test)]
pub(crate) fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    if n == 0 {
        return true;
    }
    if hay.len() < n {
        return false;
    }
    let mut skip = [n; 256];
    for (i, &b) in needle[..n - 1].iter().enumerate() {
        skip[usize::from(b)] = n - 1 - i;
    }
    let mut at = 0;
    while at + n <= hay.len() {
        if &hay[at..at + n] == needle {
            return true;
        }
        at += skip[usize::from(hay[at + n - 1])];
    }
    false
}

/// Where the store keeps managed programs under `home`: MIRRORS
/// `atpkg::platform::default_prefix` (this crate has no dependencies, and a
/// moved prefix makes a canary say `CANARY NOT RUN`, never pass).
#[cfg(test)]
fn canary_prefix(home: &std::path::Path) -> std::path::PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
    } else {
        home.join(".local").join("share").join("aterm").join("pkg")
    }
}

/// The path the drift canaries read a program's build at, from the first line
/// `aterm pkg which <program>` prints. Its head ends in the copy that runs in
/// the shell that asked, and that is the MANAGED build only when it lies in
/// the store (`claude → <shim> → … → <prefix>/store/claude/<build>/bin/claude
/// — managed 2.1.286 — …`). Any other — a system copy, one SHADOWED by a copy
/// ahead on PATH, or a managed program whose row says `not what runs outside
/// aterm: … this shell's own copy` (atpkg's outside-aterm rows, `state.rs`) —
/// names a copy aterm does not manage (an old brew build judged against the
/// managed build's anchors fails the test). The managed copy is then read
/// where the store keeps it
/// (`<prefix>/store/<program>/current/bin/<program>`), and `None` where that
/// is not there either.
#[cfg(test)]
pub(crate) fn canary_path(
    which_line: &str,
    home: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let (head, said) = which_line
        .split_once(" \u{2014} ")
        .unwrap_or((which_line, ""));
    let program = head.split(" \u{2192} ").next()?.trim();
    if said.starts_with("managed")
        && let Some(end) = head
            .rsplit(" \u{2192} ")
            .next()
            .map(str::trim)
            .filter(|end| in_store(end, program))
    {
        return Some(end.into());
    }
    let current = canary_prefix(home?)
        .join("store")
        .join(program)
        .join("current")
        .join("bin")
        .join(program);
    current.exists().then_some(current)
}

/// Whether `path` lies in a store's tree for `program` (`…/store/<program>/…`),
/// the layout [`canary_path`]'s fallback reads.
#[cfg(test)]
fn in_store(path: &str, program: &str) -> bool {
    let parts: Vec<&std::ffi::OsStr> = std::path::Path::new(path)
        .components()
        .map(|c| c.as_os_str())
        .collect();
    parts.windows(2).any(|w| w[0] == "store" && w[1] == program)
}

/// [`canary_path`] for `program`: `$<env>` first, else what `aterm pkg which`
/// says (`None` where it says nothing).
#[cfg(test)]
pub(crate) fn canary_binary(program: &str, env: &str) -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os(env) {
        return Some(path.into());
    }
    let out = std::process::Command::new("aterm")
        .args(["pkg", "which", program])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    canary_path(text.lines().next()?, home.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE CANARIES READ THE MANAGED BUILD, whatever copy stands ahead on
    /// PATH: the first line of `aterm pkg which` names the store's binary only
    /// when it says `managed` AND its head ends in the store. Behind a brew
    /// cask's `claude` it says SHADOWED and names THAT copy; in a shell outside
    /// aterm whose reroute stub passes through (or that has none) it says
    /// `managed …` and then `not what runs outside aterm`, its head the shell's
    /// own copy. NEGATIVE
    /// CONTROLS: with no managed copy where the store keeps one, a shadowed,
    /// outside-aterm or system line names nothing to read.
    #[test]
    fn the_canaries_read_the_managed_build_whatever_stands_ahead_on_path() {
        let managed = "claude \u{2192} /p/reroute/claude \u{2192} /p/agents/claude \u{2192} \
                       /p/store/claude/1000002000001000286/bin/claude \u{2014} managed 2.1.286 \
                       \u{2014} Anthropic latest";
        let shadowed = "claude \u{2192} /opt/homebrew/bin/claude \u{2014} managed 2.1.286 \u{2014} \
                        SHADOWED in this shell by /opt/homebrew/bin/claude: its PATH has /p/agents \
                        behind /opt/homebrew/bin";
        let system =
            "claude \u{2192} /usr/local/bin/claude \u{2014} system copy (not managed by aterm)";
        // atpkg `state.rs` agent_passed_through / agent_own_copy_outside_aterm.
        let passed_through = "claude \u{2192} /opt/homebrew/bin/claude \u{2014} managed 2.1.286 \
                              \u{2014} not what runs outside aterm: /p/reroute/claude passes \
                              through to /opt/homebrew/bin/claude, this shell's own copy (the \
                              managed copy leads inside aterm only; aterm help reroute)";
        let own_copy = "claude \u{2192} /opt/homebrew/bin/claude \u{2014} managed 2.1.286 \
                        \u{2014} not what runs outside aterm: /opt/homebrew/bin/claude runs \
                        here, this shell's own copy (the managed copy leads inside aterm only; \
                        aterm help reroute)";
        assert_eq!(
            canary_path(managed, None),
            Some("/p/store/claude/1000002000001000286/bin/claude".into()),
            "a managed line ends in the store's binary"
        );
        let home = std::env::temp_dir().join(format!("aterm-phase-canary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let current = canary_prefix(&home).join("store/claude/current/bin");
        std::fs::create_dir_all(&current).expect("a store directory");
        std::fs::write(current.join("claude"), b"a build").expect("a binary");
        for line in [shadowed, passed_through, own_copy, system] {
            assert_eq!(
                canary_path(line, Some(&home)),
                Some(current.join("claude")),
                "{line}"
            );
            assert_eq!(
                canary_path(line, Some(&home.join("nowhere"))),
                None,
                "{line}"
            );
            assert_eq!(canary_path(line, None), None, "{line}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ids_are_unique_and_every_text_is_non_empty() {
        let all: Vec<&Anchor> = ANCHORS
            .iter()
            .chain(CODEX_ANCHORS)
            .chain(AGY_ANCHORS)
            .chain(MUSE_ANCHORS)
            .collect();
        for (i, a) in all.iter().enumerate() {
            assert!(!a.text.trim().is_empty(), "{}", a.id);
            assert!(
                all[i + 1..].iter().all(|b| b.id != a.id),
                "duplicate id {}",
                a.id
            );
        }
        // Each agent's ids are namespaced, and both accessors read its
        // table.
        for (space, table) in [
            ("codex.", CODEX_ANCHORS),
            ("agy.", AGY_ANCHORS),
            ("muse.", MUSE_ANCHORS),
        ] {
            for a in table {
                assert!(a.id.starts_with(space), "{}", a.id);
                assert_eq!(anchor(a.id), a.text);
                assert_eq!(anchor_text(a.id), a.text);
            }
        }
        assert!(ANCHORS.iter().all(|a| {
            !["codex.", "agy.", "muse."]
                .iter()
                .any(|space| a.id.starts_with(space))
        }));
        assert_eq!(anchor("prompt.proceed"), "Do you want to proceed?");
        // The compile-time accessor reads the same table.
        const PROCEED: &str = anchor_text("prompt.proceed");
        assert_eq!(PROCEED, anchor("prompt.proceed"));
        for a in ANCHORS {
            assert_eq!(anchor_text(a.id), a.text, "{}", a.id);
        }
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

    /// The drift canaries (they run in the suite; `--nocapture` shows which
    /// binary answered). Each reads `$<env>`, else asks `aterm pkg which
    /// <program>` for the store path, and FAILS naming every literal anchor
    /// the binary no longer has; where it finds no build it says `CANARY
    /// NOT RUN` and returns, for a canary that could not look has not
    /// passed. A `·` in an anchor is drawn from a `\xB7` escape, so each
    /// side of it is looked for on its own.
    fn store_binary(program: &str, env: &str) -> Option<(String, Vec<u8>)> {
        let path = super::canary_binary(program, env)?
            .to_string_lossy()
            .into_owned();
        let bytes = std::fs::read(&path).ok()?;
        if !super::looks_like_a_build(&bytes) {
            eprintln!("{path} is a script of a few lines, not a build: skipped");
            return None;
        }
        Some((path, bytes))
    }

    /// The version `<path> --version` prints, or `None` when it cannot be
    /// read ([`version_in`]).
    fn build_version(path: &str) -> Option<String> {
        let out = std::process::Command::new(path)
            .arg("--version")
            .output()
            .ok()?;
        version_in(&String::from_utf8_lossy(&out.stdout))
    }

    /// The version a `--version` line names: a dotted one (`2.1.286 (Claude
    /// Code)`, `codex-cli 0.130.0`, agy's bare `1.4.0`), or Muse Code's with
    /// its revision, `x.y.z-R<n>.<s>` — `Muse Code 1.4.9 (1.4.9-R6297.1)`
    /// names both, and the revision is the build its anchors were read on.
    fn version_in(text: &str) -> Option<String> {
        let dotted = |v: &str| {
            v.split('.').count() >= 2
                && v.split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        };
        let words: Vec<&str> = text
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| matches!(c, '(' | ')' | ',')))
            .collect();
        words
            .iter()
            .find(|w| {
                w.split_once("-R").is_some_and(|(v, rev)| {
                    dotted(v) && (dotted(rev) || rev.parse::<u64>().is_ok())
                })
            })
            .or_else(|| words.iter().find(|w| dotted(w)))
            .map(|w| (*w).to_string())
    }

    /// Whether version `a` is newer than `b`, numerically part by part, a
    /// revision's parts after the version's (`1.4.9-R6297.1` is `1.4.9.6297.1`).
    fn newer_than(a: &str, b: &str) -> bool {
        let parts = |v: &str| {
            v.replace("-R", ".")
                .split('.')
                .map(|p| p.parse::<u64>().unwrap_or(0))
                .collect::<Vec<_>>()
        };
        parts(a) > parts(b)
    }

    #[test]
    fn an_anchor_newer_than_the_build_is_not_owed_and_versions_compare_by_number() {
        assert!(newer_than("2.1.291", "2.1.286"));
        assert!(!newer_than("2.1.286", "2.1.286"));
        assert!(!newer_than("2.1.9", "2.1.10"), "by number, not by text");
        assert!(newer_than("0.130.0", "0.99.0"));
        // Muse Code's revisions, by number after the version.
        assert!(!newer_than("1.4.9-R6297.1", "1.4.9-R6297.1"));
        assert!(newer_than("1.4.9-R6297.2", "1.4.9-R6297.1"));
        assert!(newer_than("1.4.9-R6300.0", "1.4.9-R6297.9"));
        assert!(newer_than("1.4.10-R1.0", "1.4.9-R6297.1"));
        assert!(!newer_than("1.4.9-R6297.1", "1.4.10-R1.0"));
    }

    /// MUSE CODE'S VERSION IS READ WITH ITS REVISION: `muse --version` prints
    /// `Muse Code 1.4.9 (1.4.9-R6297.1)` (measured), and the build is the
    /// revision its anchors carry — read as the dotted `1.4.9` alone, every
    /// anchor of the build it was measured on would be "newer than the build"
    /// and owed by none. The other agents' lines read as they did. NEGATIVE
    /// CONTROLS: a line with no version names none, and a revision that is no
    /// number is no revision.
    #[test]
    fn a_muse_version_is_read_with_its_revision() {
        assert_eq!(
            version_in("Muse Code 1.4.9 (1.4.9-R6297.1)\n").as_deref(),
            Some("1.4.9-R6297.1")
        );
        let since = MUSE_ANCHORS[0].since;
        assert!(!newer_than(
            since,
            &version_in("Muse Code 1.4.9 (1.4.9-R6297.1)").unwrap()
        ));
        assert!(newer_than(since, "1.4.9"), "why the revision is read");
        for (line, want) in [
            ("2.1.286 (Claude Code)", "2.1.286"),
            ("codex-cli 0.130.0", "0.130.0"),
            ("1.4.0\n", "1.4.0"),
            ("Muse Code 1.4.9 (1.4.9-Rnext)", "1.4.9"),
        ] {
            assert_eq!(version_in(line).as_deref(), Some(want), "{line}");
        }
        assert_eq!(version_in("Muse Code"), None);
    }

    fn contains(bytes: &[u8], needle: &str) -> bool {
        super::contains_bytes(bytes, needle.as_bytes())
    }

    /// The literal anchors of `table` missing from `bytes`, and for Codex
    /// (`codex`) the [`crate::codex::WALLS`] phrases — which are read
    /// lowercased — missing from them lowercased, and the
    /// [`crate::codex::TOOL_HEADS`] missing from them, each less its trailing
    /// space (the vendor draws the head and the space after it as two spans).
    /// A head the build no longer spells is an agent's last words read for a
    /// tool row's, or a tool row's for the agent's: 0.163.0 draws `Viewed
    /// image`, and the table said `Viewed Image`.
    fn missing(bytes: &[u8], table: &[Anchor], codex: bool) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = table
            .iter()
            .filter(|a| a.kind == AnchorKind::Literal)
            .filter(|a| !a.text.split(" · ").all(|part| contains(bytes, part)))
            .map(|a| a.id)
            .collect();
        if codex {
            let lower = bytes.to_ascii_lowercase();
            out.extend(
                crate::codex::WALLS
                    .iter()
                    .map(|(phrase, _)| *phrase)
                    .filter(|phrase| !contains(&lower, phrase)),
            );
            out.extend(
                crate::codex::TOOL_HEADS
                    .iter()
                    .copied()
                    .filter(|head| !contains(bytes, head.trim_end())),
            );
        }
        out
    }

    /// The scan finds a needle wherever it stands — first bytes, last bytes,
    /// after a near miss — and not one that is absent.
    #[test]
    fn the_scan_finds_a_needle_wherever_it_stands() {
        let hay = b"xxabcabdabcabcxx";
        for needle in [&b"abcabc"[..], b"xx", b"abd", b"cxx", b"x"] {
            assert!(
                super::contains_bytes(hay, needle),
                "{}",
                String::from_utf8_lossy(needle)
            );
        }
        for needle in [&b"abcabx"[..], b"xxx", b"dabd", b"abcabcxxx"] {
            assert!(
                !super::contains_bytes(hay, needle),
                "{}",
                String::from_utf8_lossy(needle)
            );
        }
        assert!(
            super::contains_bytes(b"a", b"a")
                && !super::contains_bytes(b"", b"a")
                && super::contains_bytes(b"", b"")
        );
    }

    /// A reroute stub is no build: the canaries skip it (`CANARY NOT RUN`)
    /// instead of naming every anchor missing from a few lines of shell.
    /// NEGATIVE CONTROLS: a Mach-O or ELF header, and a file of several
    /// megabytes, are builds.
    #[test]
    fn a_stub_script_is_no_build() {
        assert!(!super::looks_like_a_build(
            b"#!/bin/sh\nexec aterm pkg run claude \"$@\"\n"
        ));
        assert!(!super::looks_like_a_build(b""));
        assert!(super::looks_like_a_build(&[
            0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1
        ]));
        assert!(super::looks_like_a_build(b"\x7fELF\x02\x01\x01"));
        assert!(super::looks_like_a_build(&vec![
            b'#';
            super::MIN_BUILD_BYTES
        ]));
    }

    /// The canary's matcher on a stand-in binary. NEGATIVE CONTROL: a wall
    /// phrase or an anchor the bytes lack is named.
    #[test]
    fn the_canary_names_what_a_binary_lacks() {
        let every: String = CODEX_ANCHORS
            .iter()
            .map(|a| a.text.to_string())
            .chain(crate::codex::WALLS.iter().map(|(p, _)| p.to_uppercase()))
            .chain(
                crate::codex::TOOL_HEADS
                    .iter()
                    .map(|h| h.trim_end().to_string()),
            )
            .collect::<Vec<_>>()
            .join("\0");
        assert!(missing(every.as_bytes(), CODEX_ANCHORS, true).is_empty());
        let short = every.replace("SPEND CAP", "").replace("Folder access", "");
        assert_eq!(
            missing(short.as_bytes(), CODEX_ANCHORS, true),
            vec!["codex.trust.title", "spend cap"]
        );
        // A tool head the build spells otherwise is named: a binary with
        // 0.156.1's `Viewed Image` and not 0.163.0's `Viewed image`.
        let old_heads = every.replace("Viewed image", "Viewed Image");
        assert_eq!(
            missing(old_heads.as_bytes(), CODEX_ANCHORS, true),
            vec!["Viewed image"]
        );
        // Claude's canary asks nothing of Codex's tables.
        assert!(missing(b"", &[], false).is_empty());
        // The exit hint's words are looked for too: a build that prints
        // 0.157's `Reconnect: codex resume <id>` line and no `To reconnect`
        // heading is named.
        let old = every.replace("To reconnect", "Reconnect: codex resume ");
        assert_eq!(
            missing(old.as_bytes(), CODEX_ANCHORS, true),
            vec!["codex.exit.reconnect"]
        );
    }

    fn literal_anchors_are_in_the_store(program: &str, env: &str, table: &[Anchor], walls: bool) {
        let Some((path, bytes)) = store_binary(program, env) else {
            eprintln!(
                "CANARY NOT RUN: no {program} binary (set {env} or `aterm pkg install \
                 {program}`); anchors not checked"
            );
            return;
        };
        // AN ANCHOR IS OWED ONLY BY THE BUILDS IT WAS WRITTEN FOR: each carries
        // the version it was measured on (`since`), and a store holding an older
        // build is not a build that lost it. A version that cannot be read owes
        // every anchor.
        let version = build_version(&path);
        let (owed, newer): (Vec<Anchor>, Vec<Anchor>) = table
            .iter()
            .copied()
            .partition(|a| version.as_deref().is_none_or(|v| !newer_than(a.since, v)));
        if !newer.is_empty() {
            eprintln!(
                "{path}: {} anchor(s) are for builds newer than {}, not checked: {:?}",
                newer.len(),
                version.as_deref().unwrap_or("?"),
                newer.iter().map(|a| a.id).collect::<Vec<_>>()
            );
        }
        let missing = missing(&bytes, &owed, walls);
        assert!(
            missing.is_empty(),
            "{path}: {} literal anchors, wall phrases or tool heads missing: {missing:?}",
            missing.len()
        );
        eprintln!("{path}: every literal anchor (and wall phrase and tool head) is present");
    }

    #[test]
    fn every_literal_anchor_is_in_the_store_claude() {
        literal_anchors_are_in_the_store("claude", "ATERM_CLAUDE_BIN", ANCHORS, false);
    }

    /// THE LIMIT RESET'S TEMPLATES, WHOLE, as the bundle spells them — a `·`
    /// as its `\xB7` escape, a value as its `{placeholder}` — each built
    /// from the rows the readers match by (`crate::reset`) and named by the
    /// row it stands for: the grant row, the confirmation's body and its
    /// detail, and the opening of each answer a reader tells apart.
    ///
    /// [`missing`] looks for each side of a `·` on its own, and the sides of
    /// the confirmation's two joins are `now`, `left` and `use by`: words a
    /// build that rewords the confirmation still carries elsewhere. A
    /// template is ONE literal of the vendor's table, so a reworded one is
    /// missing — and what a rewording costs is every confirmation read
    /// inexact and Escaped, the wall's one attempt with it.
    fn reset_templates() -> Vec<(&'static str, String)> {
        let escaped = |id: &str| anchor(id).replace('·', "\\xB7");
        let led = |id: &str| format!("{} \\xB7 ", anchor(id));
        vec![
            (
                "reset.row.until",
                format!(
                    "{} ({{left}} {} {{date}})",
                    anchor("reset.row"),
                    anchor("reset.row.until")
                ),
            ),
            (
                "reset.confirm.stays",
                format!(
                    "{} {{limits}} {} {{week}}",
                    anchor("reset.confirm.refills"),
                    escaped("reset.confirm.stays")
                ),
            ),
            (
                "reset.confirm.use_by",
                format!(
                    "{{resets}} {} {{deadline}}",
                    escaped("reset.confirm.use_by")
                ),
            ),
            ("reset.done", led("reset.done")),
            ("reset.kept", led("reset.kept")),
            ("reset.lifted", escaped("reset.lifted")),
            ("reset.login", led("reset.login")),
        ]
    }

    /// The rows whose template `bytes` do not hold whole.
    fn missing_templates(bytes: &[u8]) -> Vec<&'static str> {
        reset_templates()
            .iter()
            .filter(|(_, template)| !contains(bytes, template))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The templates' matcher on a stand-in binary: one that carries each is
    /// missing none. NEGATIVE CONTROL, and why the templates are looked for
    /// whole: with the confirmation's detail reworded and the footer
    /// notice's `{resets} left \xB7 use by {date}` still there, the table's
    /// own canary finds every side of every reset row and names nothing,
    /// and this one names the row.
    #[test]
    fn a_reworded_reset_template_is_named_though_its_sides_remain() {
        let table: Vec<Anchor> = ANCHORS
            .iter()
            .filter(|a| a.id.starts_with("reset."))
            .copied()
            .collect();
        let notice = "/limit-reset to refill your limits \\xB7 {resets} left \\xB7 use by {date}";
        let every = table
            .iter()
            .map(|a| a.text.to_string())
            .chain(reset_templates().into_iter().map(|(_, template)| template))
            .chain([notice.to_string()])
            .collect::<Vec<_>>()
            .join("\0");
        assert!(missing(every.as_bytes(), &table, false).is_empty());
        assert_eq!(missing_templates(every.as_bytes()), Vec::<&str>::new());
        let reworded = every.replace(
            "{resets} left \\xB7 use by {deadline}",
            "{resets} remaining \\xB7 expires {deadline}",
        );
        assert_ne!(reworded, every);
        assert!(missing(reworded.as_bytes(), &table, false).is_empty());
        assert_eq!(
            missing_templates(reworded.as_bytes()),
            vec!["reset.confirm.use_by"]
        );
        let reworded = every.replace(
            "now \\xB7 your weekly reset day stays {week}",
            "today \\xB7 your weekly reset day stays {week}",
        );
        assert!(missing(reworded.as_bytes(), &table, false).is_empty());
        assert_eq!(
            missing_templates(reworded.as_bytes()),
            vec!["reset.confirm.stays"]
        );
    }

    /// The drift canary for [`reset_templates`], run and skipped as the
    /// table's own is ([`store_binary`]).
    #[test]
    fn every_reset_template_is_whole_in_the_store_claude() {
        let Some((path, bytes)) = store_binary("claude", "ATERM_CLAUDE_BIN") else {
            eprintln!(
                "CANARY NOT RUN: no claude binary found (set ATERM_CLAUDE_BIN, or install one: \
                 `aterm pkg install claude`) — the reset's templates are NOT checked against a \
                 build"
            );
            return;
        };
        let missing = missing_templates(&bytes);
        assert!(
            missing.is_empty(),
            "{path}: {} reset templates not whole: {missing:?}",
            missing.len()
        );
        eprintln!("{path}: every reset template is whole");
    }

    #[test]
    fn every_literal_codex_anchor_is_in_the_store_codex() {
        literal_anchors_are_in_the_store("codex", "ATERM_CODEX_BIN", CODEX_ANCHORS, true);
    }

    #[test]
    fn every_literal_agy_anchor_is_in_the_store_agy() {
        literal_anchors_are_in_the_store("agy", "ATERM_AGY_BIN", AGY_ANCHORS, false);
    }

    #[test]
    fn every_literal_muse_anchor_is_in_the_store_muse() {
        literal_anchors_are_in_the_store("muse", "ATERM_MUSE_BIN", MUSE_ANCHORS, false);
    }
}

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
//! Each other agent's strings are a table of its own — [`AGY_ANCHORS`]
//! (ids `agy.…`), [`MUSE_ANCHORS`] (`muse.…`), [`GEMINI_ANCHORS`]
//! (`gemini.…`); [`anchor`] and [`anchor_text`] read them all. Codex has none:
//! aterm reads no Codex screen (its screen lane was retired on 2026-10-16).
//!
//! The tests `every_literal_anchor_is_in_the_store_claude`,
//! `every_literal_agy_anchor_is_in_the_store_agy` and
//! `every_literal_muse_anchor_is_in_the_store_muse` grep the store's
//! binaries for every [`AnchorKind::Literal`] row and name the misses — the drift canaries. They RUN in the suite (`verify
//! test` included) wherever a store binary is found (`$ATERM_CLAUDE_BIN`,
//! `$ATERM_AGY_BIN`, `$ATERM_MUSE_BIN`, else `aterm pkg
//! which <program>`), so a new build the vendor's auto-updater or `aterm pkg
//! update` brought is judged by the next test run, and say so loudly and
//! return where there is none — a canary that could not look has not passed,
//! and is not counted as having. None is `#[ignore]`d: an ignored test is run
//! by nobody. Gemini CLI is no store binary but an npm package of JavaScript
//! files: `every_literal_gemini_anchor_is_in_its_bundle` reads the bundle
//! that `$ATERM_GEMINI_BIN` (else the first `gemini` on PATH, else
//! Homebrew's link) resolves into, every `.js` file beside its `gemini.js`.

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
    /// to` + the file name; the rm breaker's warning from a template), or
    /// written by the API and only passed on (`API Error: <its message>`):
    /// the canary cannot look for it whole.
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
    // The approval of a project's `.mcp.json` servers, drawn at launch once
    // the folder is trusted: one new server's title (its name follows), its
    // three unnumbered options (the cursor starts on the last), and the title
    // and submit row of the dialog for several (`<n> new MCP servers found in
    // this project`, a checkbox row per server). EVERY answer, Esc's too, is
    // saved for good in the project's `.claude/settings.local.json`. MEASURED
    // on 2.1.301 (2026-10-16, the real-Claude lab); the same literals in
    // 2.1.295.
    lit(
        "mcp.title",
        "New MCP server found in this project:",
        "2.1.295",
    ),
    lit("mcp.use", "Use this MCP server", "2.1.295"),
    lit(
        "mcp.use_all",
        "Use this and all future MCP servers in this project",
        "2.1.295",
    ),
    lit(
        "mcp.skip",
        "Continue without using this MCP server",
        "2.1.295",
    ),
    lit(
        "mcp.title_many",
        "new MCP servers found in this project",
        "2.1.295",
    ),
    lit("mcp.submit", "Enable selected", "2.1.295"),
    // The Chrome setup panel's consent in its connected phase (est() in the
    // 2.1.301 bundle, `Setting up Claude in Chrome`): it turns the browser
    // tools on for every later session too (`claudeInChromeDefaultEnabled`
    // in the global config), so it is kept for good. Its `Continue without
    // browser tools` saves nothing.
    lit(
        "setup.chrome_continue",
        "Continue with browser tools",
        "2.1.301",
    ),
    // The launch dialog over a settings file the validator rejects with an
    // error: its Esc EXITS Claude Code (`Settings Warning`'s, the same
    // dialog over warnings alone, goes on). MEASURED on 2.1.301
    // ([`crate::prompt::fixtures::SETTINGS_ERROR`]), its Esc read in the
    // bundle.
    lit("settings.error", "Settings Error", "2.1.301"),
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
    // The autocompact offer (the 2.1.302 binary's `dbe`, title `G8e`, footer
    // `Q8e`): Claude Code's own dialog offering to autocompact earlier than
    // its default. Its question, its accept row's words (the size between
    // `‹ ` and ` ›` is the stepper's), its outcome line once saved, and its
    // footer — drawn from `↑`/`\xB7` escapes, so composed for the canary.
    lit(
        "autocompact.title",
        "We recommend compacting earlier",
        "2.1.302",
    ),
    lit("autocompact.question", "Want to autocompact at", "2.1.302"),
    lit("autocompact.accept", "Autocompact at", "2.1.302"),
    lit("autocompact.saved", "Autocompact set to ", "2.1.302"),
    composed(
        "autocompact.footer",
        "↑/↓ choose · ←/→ change size · enter confirm · esc not now",
        "2.1.302",
    ),
    lit(
        "autocompact.more_above",
        "More text above: scroll up, or enlarge the terminal",
        "2.1.302",
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
    // The vendor's other words that a `/compact` cannot help: a context that is
    // one exchange (`Prompt is too long · … A single-exchange conversation
    // cannot be compacted; reduce attached files/tools or start with less
    // context.`, 2.1.300), and `/compact`'s own answer on one, plain text with
    // no error level (`⎿  Not enough messages to compact.`, measured on
    // 2.1.286). The turn-end policy latches its full context's track on them
    // (`policy/turn_end.rs`, `compaction_exhausted_row`).
    lit("wall.cannot_be_compacted", "cannot be compacted", "2.1.300"),
    lit(
        "wall.not_enough_to_compact",
        "Not enough messages to compact",
        "2.1.286",
    ),
    // The remedy of 2.1.300's notice that an image is past the many-image
    // request's size (`An image in the conversation exceeds the dimension
    // limit for many-image requests (2000px). Run /compact to remove old
    // images from context, or start a new session.`, drawn only where
    // `/compact` is on): aterm-agent reads such a notice as the full
    // context's wall, which the turn-end policy's `/compact` answers
    // (`supervise/wall_confirm.rs`, `notice_wall`).
    lit(
        "wall.compact_images",
        "Run /compact to remove old images",
        "2.1.300",
    ),
    // The same remedy on 2.1.300's notice that the request is past the API's
    // size because of the images and attachments it carries (`Request too
    // large (max 32 MB; …). Accumulated images and attachments in the
    // conversation pushed the request over the limit. Run /compact, or double
    // press esc to go back and remove attachments.`): read the same way. Its
    // form for a conversation no compaction makes fit (`… so removing
    // attachments or compacting cannot make it fit.`) names no `/compact`.
    lit(
        "wall.compact_attachments",
        "pushed the request over the limit. Run /compact",
        "2.1.300",
    ),
    // The heads of the vendor's own refusals of a request (`wall::PHRASES`'
    // `Tag::Refused` rows, 2.1.300's error builder): sent again unchanged, each
    // is refused again.
    lit("wall.refused.cant_help", "can't help with this", "2.1.300"),
    lit("wall.refused.safeguards", "safeguards flagged", "2.1.300"),
    lit("wall.refused.rewind", "Run /rewind", "2.1.300"),
    lit(
        "wall.refused.thinking",
        "isn't available with thinking",
        "2.1.300",
    ),
    lit(
        "wall.refused.advisor",
        "advisor model is not compatible",
        "2.1.300",
    ),
    lit(
        "wall.refused.model_support",
        "does not support this model",
        "2.1.300",
    ),
    lit(
        "wall.refused.selected_model",
        "issue with the selected model",
        "2.1.300",
    ),
    lit(
        "wall.refused.pick_model",
        "to pick a different model",
        "2.1.300",
    ),
    lit(
        "wall.refused.deployment",
        "not available on your",
        "2.1.300",
    ),
    lit("wall.refused.on_hold", "account is on hold", "2.1.300"),
    lit(
        "wall.refused.pro_plan",
        "not available with the Claude Pro plan",
        "2.1.300",
    ),
    // Two refusals whose words are the API's own, which Claude Code passes on
    // as `API Error: <its message>` (an identity check's 403, an
    // organization's data-loss-prevention 400): no build carries them, so the
    // canary cannot look for them.
    composed("wall.refused.identity", "Verify your identity", "2.1.300"),
    composed("wall.refused.dlp", "data loss prevention", "2.1.300"),
    // The long context's entitlement as 2.1.300 words it (`API Error: Usage
    // credits required for 1M context · run /model to switch to standard
    // context`), read as the older `Extra usage is required for long context`
    // is: an account limit no reset ends.
    lit(
        "wall.usage_credits_required",
        "Usage credits required",
        "2.1.300",
    ),
    // The high load the transcript writes as `Opus is experiencing high load,
    // …` and 2.1.300 draws under the gutter as `We are experiencing high
    // demand for Opus 4.` (its renderer's own case); `Switched to Sonnet due to
    // high demand for Opus` is its model fallback, which goes on.
    lit("wall.high_demand", "high demand", "2.1.300"),
    // Claude Code's critical-memory banner, right-aligned on its own row
    // between the spinner and the composer's top rule (`<anchor> (140.4GB) —
    // restart and resume with claude --continue`, measured 2026-09-24 on the
    // incident tab whose worker sat at 38.8 GiB resident and read no input
    // for 2h41m). Placed by `wall::memory_wall`, never by `PHRASES`: a
    // composer draft quoting it carries the same words.
    lit("wall.memory", "Critical memory usage", "2.1.280"),
    // The disk is full (2.1.302's bundle, read 2026-10-16 after two incidents
    // in one day): the pinned `transcript-writer-degraded` notification, a
    // tool's lost output, and a goal the vendor ended on it. Placed by
    // `disk_full::disk_full`, never by `PHRASES`: a draft can quote them.
    lit(
        "wall.disk_full",
        "Transcript writes are failing (",
        "2.1.302",
    ),
    lit(
        "wall.disk_full.tool",
        "Command output was lost: the temp filesystem at",
        "2.1.302",
    ),
    lit(
        "wall.disk_full.goal",
        "Goal cleared after an unrecoverable error (",
        "2.1.302",
    ),
    // The rows of Claude Code's own that a HOOK'S WORDS stand under (2.1.302,
    // measured in the lab, ATERM-KI-1015-14; `phase::block_is_vendors`): the
    // stop-hook summary, `⏺ Ran 1 stop hook (ctrl+o to expand)` (`Ran `, the
    // count, then `stop hook` or `stop hooks`, joined at draw time), whose
    // first rows are the hooks' commands and `stopReason`; and the row a hook's
    // error opens on, `<hook> hook error` (the next row its stderr, behind
    // `Failed with non-blocking status code: `) or `<hook> hook returned
    // blocking error` (the next row its reason, or `Blocked by hook`).
    lit("hook.ran", "Ran ", "2.1.294"),
    composed("hook.ran.stop", "stop hook", "2.1.294"),
    lit("hook.error", " hook error", "2.1.294"),
    lit(
        "hook.blocking_error",
        " hook returned blocking error",
        "2.1.302",
    ),
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

/// Every string aterm guards on in agy's screen (agy 1.4.0: the captures in
/// [`crate::agy::fixtures`] and the binary).
pub const AGY_ANCHORS: &[Anchor] = &[
    // The footer's left side, and the key row under a box or a menu (`↑/↓`
    // and `Navigate` are two strings in the binary).
    lit("agy.footer.idle", "? for shortcuts", "1.4.0"),
    lit("agy.footer.cancel", "esc to cancel", "1.4.0"),
    // An exit pending: one ctrl+c at the idle composer, empty or over a held
    // draft (the draft stays), or one ctrl+d at the EMPTY idle composer (a
    // ctrl+d on a draft deletes forward), and the next press of that key ends
    // agy. The binary's `press %s again to exit`, its key filled in at run
    // time: the two screen texts are hints, and the words after the key the
    // literal the reader matches by (`agy::footer_of`). Measured signed in
    // (`agy-1.4.0-exit-confirm`) and in the lab
    // (`agy-1.4.0-lab-exit-confirm-ctrl-d`, `agy-1.4.0-lab-ctrl-c-draft`).
    hint("agy.footer.exit", "press ctrl+c again to exit", "1.4.0"),
    hint(
        "agy.footer.exit_ctrl_d",
        "press ctrl+d again to exit",
        "1.4.0",
    ),
    lit("agy.footer.exit_again", "again to exit", "1.4.0"),
    hint("agy.keys.navigate", "↑/↓ Navigate", "1.4.0"),
    // The footer's hint while messages wait behind a running turn (`Press ↑
    // to edit queued messages`: the binary's `Press %s to edit queued
    // messages`, its key filled in at run time).
    lit("agy.footer.queued", "to edit queued messages", "1.4.0"),
    // The permission modes' hints, drawn in the EMPTY composer while one is
    // on (shift+tab cycles them).
    lit(
        "agy.mode.hint.accept_edits",
        "Accept-edits mode: file edits auto-approved (shift+tab to cycle)",
        "1.4.0",
    ),
    lit(
        "agy.mode.hint.auto",
        "Auto mode: edits & safe commands auto-approved (shift+tab to cycle)",
        "1.4.0",
    ),
    lit(
        "agy.mode.hint.plan",
        "Plan mode: research & plan only (shift+tab to cycle)",
        "1.4.0",
    ),
    lit(
        "agy.mode.hint.best_of_n",
        "Best-of-N mode: fork into parallel arms and pick a winner (shift+tab to cycle)",
        "1.4.0",
    ),
    // The logo row naming the build, and the turn's end after an Esc or a
    // refused box.
    lit("agy.banner", "Antigravity CLI", "1.4.0"),
    lit("agy.turn.interrupted", "Interrupted · What should", "1.4.0"),
    // A failed turn's id row, and the failures it names that are walls: the
    // service overloaded, the model's quota spent, the AI credits run out.
    lit("agy.turn.error_id", "Error ID:", "1.4.0"),
    lit(
        "agy.wall.overloaded",
        "Our servers are experiencing high traffic",
        "1.4.0",
    ),
    lit(
        "agy.wall.quota",
        "You have exhausted your quota on this model",
        "1.4.0",
    ),
    lit(
        "agy.wall.credits",
        "Your AI credits balance is too low to continue",
        "1.4.0",
    ),
    // A subagent's alert in the turn (`<agent> needs approval for <tool>`,
    // `<agent> has a question`): the subagent waits on a person.
    lit("agy.subagent.approval", " needs approval for ", "1.4.0"),
    lit("agy.subagent.question", " has a question", "1.4.0"),
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
    // The pending edit's word after an Esc, which does nothing there.
    lit(
        "agy.edit.esc_disabled",
        "Esc disabled during file edits",
        "1.4.0",
    ),
    // The question's row for the person's own words.
    lit("agy.question.write_in", "Write-in...", "1.4.0"),
    // The mark on the answer the model recommends: agy's ask-the-user tool
    // tells the model to PREFIX the option with it, where Codex's and Claude
    // Code's dialogs draw it last.
    lit("agy.question.recommended", "(Recommended)", "1.4.0"),
    // A question no digit answers: a multi-select's checkboxes (a digit only
    // toggles one), and the person's own answer kept on its write-in row.
    lit("agy.question.unchecked", "[ ] ", "1.4.0"),
    lit("agy.question.checked", "[x] ", "1.4.0"),
    lit("agy.question.written", "✓ ", "1.4.0"),
    // agy's suffixes on every answer it keeps (its binary's labels): saved
    // to settings.json, or kept for the conversation.
    lit("agy.box.persisted", "(Persist to settings.json)", "1.4.0"),
    lit("agy.box.conversation", "in this conversation", "1.4.0"),
    // The row that closes a failed turn's `⚠` block (its id).
    lit("agy.error.id", "Error ID:", "1.4.0"),
    // The feedback survey agy draws unasked in place of its composer (the
    // lab's captures): one literal for its question, and options composed
    // from a `[%d] %s` template, of which `[0] Skip` is the one a press
    // writes.
    lit(
        "agy.survey.question",
        "How's the CLI experience so far? Help us improve:",
        "1.4.0",
    ),
    composed("agy.survey.skip", "[0] Skip", "1.4.0"),
    // The first run's onboarding screens with no sign-in (the Gemini API
    // provider seeded): the colour scheme, then the terms and data use.
    lit(
        "agy.onboarding.scheme",
        "Choose your color scheme:",
        "1.4.0",
    ),
    lit(
        "agy.onboarding.terms",
        "Terms of Service & Data Use",
        "1.4.0",
    ),
    // The terms screen's button back to the colour scheme, the way its Esc
    // goes too (measured in the lab, 2026-10-16).
    lit("agy.onboarding.previous", "[Previous]", "1.4.0"),
];

/// Every string aterm guards on in Muse Code's screen (muse 1.4.9-R6297.1
/// and 1.4.10-R6427.1: the captures in [`crate::muse::fixtures`] and the
/// binary).
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
    // The approval boxes, drawn in the composer's frame (1.4.10, the lab's
    // captures): the shell box's and the file box's titles, the file box's
    // path row (`write` and ` path: ` are two strings), the one-shot allow,
    // the refusal (the product's name put between two strings) and the hint
    // under the options.
    lit(
        "muse.box.shell",
        "Would you like to run the following command?",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.box.file",
        "Would you like to allow this file access?",
        "1.4.10-R6427.1",
    ),
    composed("muse.box.write_path", "write path: ", "1.4.10-R6427.1"),
    lit("muse.box.once", "Yes, proceed", "1.4.10-R6427.1"),
    composed(
        "muse.box.deny",
        "No, and tell Muse Code what to do differently",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.box.permissions",
        "Change approval defaults in /permissions",
        "1.4.10-R6427.1",
    ),
    // The network box (the lab's capture `box-network`: a web fetch and a
    // monitor's WebSocket): its title, its two body rows, its session grant
    // and its always grant; and the tool-action box (the capture
    // `box-tool-action`, an MCP server's tool, drawn outside the lab's
    // jail): its title, its body row and its always grant.
    lit(
        "muse.box.network",
        "Would you like to allow this network access?",
        "1.4.10-R6427.1",
    ),
    lit("muse.box.network_row", "network: ", "1.4.10-R6427.1"),
    lit("muse.box.full_url", "full URL: ", "1.4.10-R6427.1"),
    lit(
        "muse.box.session",
        "Yes, don't ask again this session",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.box.always",
        "Always allow this network destination",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.box.tool",
        "Would you like to allow this tool action?",
        "1.4.10-R6427.1",
    ),
    lit("muse.box.tool_row", "tool action: ", "1.4.10-R6427.1"),
    lit(
        "muse.box.always_tool",
        "Always allow this MCP tool",
        "1.4.10-R6427.1",
    ),
    // The refusal of the box a shell call naming a Unix socket draws (the
    // capture `box-unix-socket`: the network title over `requested by:`,
    // `Allow once (y)` and `Reject (esc)`). The canary cannot guard `Reject`
    // itself: the 1.4.10 binary holds it hundreds of times, so a build that
    // dropped the label would still pass. What the box's refusal answers the
    // model, held in one place in that binary, stands guard for it (the
    // capture `box-unix-socket-denied`).
    lit("muse.box.reject", "Reject", "1.4.10-R6427.1"),
    lit(
        "muse.box.socket_refused",
        "Unix socket access was not approved",
        "1.4.10-R6427.1",
    ),
    // The background-work panel under the composer's bottom rule (the lab's
    // captures `end-of-turn-work-panel`, `end-of-turn-work-running`).
    lit("muse.panel.select", "↓ to select", "1.4.10-R6427.1"),
    // The transcript's pointer to that panel, `(↓ to view terminals)` (the
    // binary holds ` to view ` and puts the rest together; the lab's capture
    // `end-of-turn-work-hint`).
    composed("muse.panel.view", "(↓ to view ", "1.4.10-R6427.1"),
    // The warning a resumed session opens with after a run that recorded no
    // clean shutdown (SIGHUP, SIGKILL): `warning: the previous run of this
    // session ended abnormally around <time> …` (the harness's relaunch on
    // exit, the lab's capture `resumed-after-crash`).
    lit(
        "muse.resume.abnormal",
        "the previous run of this session ended abnormally",
        "1.4.10-R6427.1",
    ),
    // The shell box asking for one stage of a pipeline (the lab's capture
    // `box-shell-stage`): its one-shot allow and its refusal, and the rows
    // under the command that name the stages (`Stage 3/3` is put together
    // from its numbers).
    lit(
        "muse.box.stage_once",
        "Allow this stage once",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.box.stage_abort",
        "Abort the entire command",
        "1.4.10-R6427.1",
    ),
    lit("muse.box.stage_skipped", "Skipped Stage", "1.4.10-R6427.1"),
    composed("muse.box.stage", "Stage ", "1.4.10-R6427.1"),
    lit("muse.box.stage_argv", "Current argv", "1.4.10-R6427.1"),
    // The question: its head, its key row (one literal of four; the other
    // three switch questions or toggle answers), the key row's Esc, the row
    // Muse Code adds, and the suffix the tool tells the model to put on its
    // recommended option.
    lit(
        "muse.question.title",
        "Request user input",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.question.keys",
        "Enter to select · ↑/↓ to move · Tab for an optional note · Esc to interrupt",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.question.keys_multi",
        "Enter to select · ↑/↓ to move · ←/→ to switch question · Tab for an optional note · Esc to interrupt",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.question.keys_toggle",
        "Enter to toggle · Submit row to continue · ↑/↓ to move · Tab for an optional note · Esc to interrupt",
        "1.4.10-R6427.1",
    ),
    lit("muse.question.esc", "Esc to interrupt", "1.4.10-R6427.1"),
    lit("muse.question.none", "None of the above", "1.4.10-R6427.1"),
    // The question's note field (Tab), and the review its answers go to:
    // its key row's head and end, its two options (the lab's captures
    // `question-note-*`, `question-multi-review`).
    lit("muse.question.note", "Note (optional): ", "1.4.10-R6427.1"),
    lit(
        "muse.question.review",
        "Review answers before submit",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.question.review.back",
        "Esc to go back",
        "1.4.10-R6427.1",
    ),
    composed("muse.question.review.note", "; note: ", "1.4.10-R6427.1"),
    lit("muse.question.submit", "Submit answers", "1.4.10-R6427.1"),
    lit(
        "muse.question.interrupt",
        "Interrupt turn",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.question.recommended",
        "(Recommended)",
        "1.4.10-R6427.1",
    ),
    // A model's turn stopped with Esc, and the footer while the run stops.
    lit(
        "muse.turn.interrupted",
        "Interrupted · Report issues with /feedback",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.footer.interrupting",
        "interrupting run",
        "1.4.10-R6427.1",
    ),
    // Its own rows under a turn's end: the away recap, drawn when a key is
    // typed after a pause, and the time a long turn took (the lab's captures
    // `end-of-turn-finished`, `end-of-turn-worked-recap`).
    lit("muse.recap", "Recap: ", "1.4.10-R6427.1"),
    lit("muse.worked", "Worked for ", "1.4.10-R6427.1"),
    // The heads of its own tool rows.
    lit("muse.tool.ran", "Ran ", "1.4.10-R6427.1"),
    lit("muse.tool.wrote", "Wrote ", "1.4.10-R6427.1"),
    lit("muse.tool.edited", "Edited ", "1.4.10-R6427.1"),
    // The walls: the key refused (measured) and its two siblings in the
    // binary; the usage wall's pieces (its URL and its time put between
    // them); a model call that failed, and its status.
    lit(
        "muse.wall.key",
        "your API key from META_API_KEY was rejected — update or unset it",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.saved_key",
        "your saved API key was rejected — run `muse login` or add a new key",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.no_login",
        "no login to refresh a key from; run `muse login` or set META_API_KEY",
        "1.4.10-R6427.1",
    ),
    // A Meta account's walls (the binary's strings; the lab never logs in, so
    // none was drawn).
    lit(
        "muse.wall.payment_api",
        "Model API requires a payment method on your account — add one at https://dev.meta.ai, then log in again.",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.payment_muse",
        "Muse Code requires a payment method — subscribe at https://accountscenter.meta.com/muse_code/?ep=no_payg, then log in again.",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.onboarding",
        "your Meta account isn't set up for Model API yet — finish onboarding at https://dev.meta.ai, then log in again.",
        "1.4.10-R6427.1",
    ),
    lit("muse.wall.usage", "Usage limit reached", "1.4.10-R6427.1"),
    lit("muse.wall.usage.upgrade", " · /upgrade (", "1.4.10-R6427.1"),
    lit(
        "muse.wall.usage.more",
        ") for increased limits",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.usage.reset",
        "wait for usage to reset at",
        "1.4.10-R6427.1",
    ),
    lit("muse.wall.model_failed", "model failed: ", "1.4.10-R6427.1"),
    lit("muse.wall.api_error", "API error ", "1.4.10-R6427.1"),
    // The row under the status row while a failed request is retried.
    lit("muse.retry.api", "Meta Model API ", "1.4.10-R6427.1"),
    lit("muse.retry.in", " · retrying in ", "1.4.10-R6427.1"),
    lit("muse.retry.attempt", " · attempt ", "1.4.10-R6427.1"),
    // Strings of the binary, found by the dogfood runs of 2026-10-16 (muse
    // 1.4.10 reviewing this reader of its own screens) and unmeasured on a
    // screen. The composer's top-rule labels past `Voice input (…)`: voice
    // input's other states, and the side chat's (its footer too).
    lit(
        "muse.composer.voice_prefix",
        "Voice input (",
        "1.4.10-R6427.1",
    ),
    lit("muse.composer.talk", "to talk", "1.4.10-R6427.1"),
    lit("muse.composer.recording", "Recording", "1.4.10-R6427.1"),
    lit(
        "muse.composer.transcribing",
        "Transcribing",
        "1.4.10-R6427.1",
    ),
    lit("muse.composer.offline", "Voice offline", "1.4.10-R6427.1"),
    lit("muse.composer.error", "Voice error", "1.4.10-R6427.1"),
    lit("muse.sidechat.label", "Side Chat", "1.4.10-R6427.1"),
    lit("muse.sidechat.footer", "Side chat mode", "1.4.10-R6427.1"),
    // The standing grants that do not begin `Yes, …` (`Always allow in this
    // workspace: ls ... (p)` measured on 1.4.9's staged box, `Always allow
    // this network destination` on 1.4.10's network box, `muse.box.always`).
    // The review after several questions' answers, measured on 1.4.9 too, is
    // `muse.question.review`.
    lit("muse.box.always_prefix", "Always allow ", "1.4.10-R6427.1"),
    // The notices its background work leaves under a reply.
    lit("muse.notice.finished", "Finished ", "1.4.10-R6427.1"),
    lit("muse.notice.message", "Message from ", "1.4.10-R6427.1"),
    // The turn failures: a login gone (a sign-in wall), a quota spent (a
    // usage wall), and the faults only a person can fix.
    lit(
        "muse.wall.invalid",
        "Your saved Meta credentials are invalid",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.missing",
        "No Meta credentials were found",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.denied",
        "macOS denied Muse Code access to your saved Meta credentials",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.canceled",
        "macOS Keychain access was canceled",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.wall.unreachable",
        "Muse Code couldn't access your Meta credentials",
        "1.4.10-R6427.1",
    ),
    lit("muse.wall.quota", "You've used your", "1.4.10-R6427.1"),
    lit(
        "muse.fault.config",
        "A configuration problem stopped this turn.",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.fault.prepare",
        "This turn could not prepare the conversation.",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.fault.state",
        "A problem accessing saved state",
        "1.4.10-R6427.1",
    ),
];

/// Every string aterm guards on in Gemini CLI's screen (gemini-cli 0.46.0:
/// the captures in [`crate::gemini::fixtures`] and its bundle).
pub const GEMINI_ANCHORS: &[Anchor] = &[
    // The logo row naming the build, the status row's idle hint, the
    // composer's placeholder and the footer's first item.
    lit("gemini.banner", "Gemini CLI", "0.46.0"),
    lit("gemini.status.shortcuts", "? for shortcuts", "0.46.0"),
    lit("gemini.status.cancel", "(esc to cancel, ", "0.46.0"),
    // The status row's hint with the UI details hidden, and the approval
    // modes it names at its left there (`● YOLO`).
    lit(
        "gemini.status.details",
        "press tab twice for more",
        "0.46.0",
    ),
    lit("gemini.mode.yolo", "YOLO", "0.46.0"),
    lit("gemini.mode.auto_edit", "auto edit", "0.46.0"),
    lit("gemini.mode.plan", "plan", "0.46.0"),
    lit(
        "gemini.composer.placeholder",
        "Type your message or @path/to/file",
        "0.46.0",
    ),
    lit(
        "gemini.footer.workspace",
        "workspace (/directory)",
        "0.46.0",
    ),
    // A turn's ends: Esc, and an API error with the notice under it.
    lit("gemini.turn.cancelled", "Request cancelled.", "0.46.0"),
    lit("gemini.error.api", "[API Error:", "0.46.0"),
    lit("gemini.error.failed", "This request failed", "0.46.0"),
    // The approval boxes: their questions (the shell's tool name is drawn
    // as `[Shell]` after it), the write tool's title and their options.
    lit("gemini.box.exec.ask", "Allow execution of", "0.46.0"),
    // The warning the shell box draws under its question for a command
    // with a redirection (`Redirection detected. To auto-accept, press …`).
    lit("gemini.box.redirection", "Redirection detected.", "0.46.0"),
    lit("gemini.box.change.ask", "Apply this change?", "0.46.0"),
    lit("gemini.box.writing", "Writing to ", "0.46.0"),
    lit("gemini.box.once", "Allow once", "0.46.0"),
    lit("gemini.box.session", "Allow for this session", "0.46.0"),
    // The MCP box's standing grants: `Allow tool for this session`, `Allow
    // all server tools for this session`.
    lit("gemini.box.session.tail", "for this session", "0.46.0"),
    // The MCP and fetch boxes' head, the fetch box's question, and the
    // sandbox expansion's (`To run [Shell], allow access to the following?`,
    // drawn from parts: `To run`, a blank, the bracketed names).
    lit("gemini.box.action", "Action Required", "0.46.0"),
    lit("gemini.box.proceed", "Do you want to proceed?", "0.46.0"),
    lit("gemini.box.expansion.head", "To run", "0.46.0"),
    lit(
        "gemini.box.expansion.ask",
        ", allow access to the following?",
        "0.46.0",
    ),
    lit("gemini.box.saved", "all future sessions", "0.46.0"),
    lit("gemini.box.editor", "Modify with external editor", "0.46.0"),
    lit("gemini.box.deny", "No, suggest changes (esc)", "0.46.0"),
    // The question dialog.
    lit("gemini.question.title", "Answer Questions", "0.46.0"),
    lit("gemini.question.custom", "Enter a custom value", "0.46.0"),
    hint("gemini.question.keys", "Enter to select", "0.46.0"),
    // Several questions: the review tab's head and its key row.
    lit("gemini.question.review", "Review your answers:", "0.46.0"),
    hint("gemini.question.submit", "Enter to submit", "0.46.0"),
    // The per-day quota's dialog: neither option is pressed.
    lit("gemini.usage.title", "Usage limit reached for", "0.46.0"),
    lit("gemini.usage.retry", "Keep trying", "0.46.0"),
    // The messages typed while the agent worked, over the status row (and
    // under a box kept over the composer) — drawn from a `\u2191` escape, so
    // composed for the canary.
    composed("gemini.queued", "Queued (press ↑ to edit):", "0.46.0"),
    // The same dialog's other first rows (bundle: `ProQuotaDialog`): a 404
    // for a model the key cannot reach (measured), a model it does not know
    // (`Model "<m>" was not found or is invalid.`), and the service's demand.
    lit(
        "gemini.fallback.no_access",
        "It seems like you don't have access to ",
        "0.46.0",
    ),
    lit(
        "gemini.fallback.not_found",
        "was not found or is invalid.",
        "0.46.0",
    ),
    lit(
        "gemini.fallback.demand",
        "We are currently experiencing high demand.",
        "0.46.0",
    ),
    // The folder-trust dialog: its options name the folder after these.
    lit(
        "gemini.trust.title",
        "Do you trust the files in this folder?",
        "0.46.0",
    ),
    lit("gemini.trust.folder", "Trust folder (", "0.46.0"),
    lit("gemini.trust.parent", "Trust parent folder (", "0.46.0"),
    lit("gemini.trust.no", "Don't trust", "0.46.0"),
    // Leaving: the farewell box's head and its resume line.
    lit(
        "gemini.exit.farewell",
        "Agent powering down. Goodbye!",
        "0.46.0",
    ),
    lit(
        "gemini.exit.resume",
        "To resume this session: gemini --resume",
        "0.46.0",
    ),
];

/// The text of the anchor `id`. Panics on an id the table does not have —
/// a typo in a caller is a bug to find in its first test, not a guard that
/// silently matches nothing.
#[must_use]
pub fn anchor(id: &str) -> &'static str {
    match ANCHORS
        .iter()
        .chain(AGY_ANCHORS)
        .chain(MUSE_ANCHORS)
        .chain(GEMINI_ANCHORS)
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
    let mut i = 0;
    while i < GEMINI_ANCHORS.len() {
        if same(GEMINI_ANCHORS[i].id, id) {
            return GEMINI_ANCHORS[i].text;
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

    /// The version atpkg's record beside the build names (`<build>.vendor`'s
    /// `version = "…"`: the build of the binary's directory, reached through
    /// a `current` link, else of the file a link from outside the store
    /// names), or `None` where there is no record ([`version_in`]). The binary is never
    /// run, as the Gemini canary reads its `package.json`: a vendor build the
    /// suite starts runs with the owner's HOME, and its `--version` is no
    /// pure read there (measured 2026-10-17 in a scratch HOME: Muse Code
    /// 1.4.10's takes `~/.config/muse/.auth.json.lock`, Codex 0.163.0's makes
    /// its PATH-alias helpers under `~/.codex`).
    fn build_version(path: &str) -> Option<String> {
        use std::path::{Path, PathBuf};
        let path = Path::new(path);
        let build_of = |bin_dir: Option<PathBuf>| bin_dir?.parent().map(Path::to_path_buf);
        let record = [
            build_of(path.parent().and_then(|d| d.canonicalize().ok())),
            build_of(
                path.canonicalize()
                    .ok()
                    .and_then(|f| f.parent().map(Path::to_path_buf)),
            ),
        ]
        .into_iter()
        .flatten()
        .find_map(|build| std::fs::read_to_string(format!("{}.vendor", build.display())).ok())?;
        let version = record
            .lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == "version")?
            .1;
        version_in(version.trim().trim_matches('"'))
    }

    /// The canary's version is the store record's, read through a `current`
    /// link, and the build is never started: its stand-in would leave a
    /// mark if it ran. NEGATIVE CONTROLS: a build with no record, and a
    /// record with no `version`, name none (every anchor is then owed).
    #[cfg(unix)]
    #[test]
    fn the_canary_reads_the_build_record_and_never_runs_the_build() {
        let root = aterm_tempfile::TempDir::new().expect("a private store");
        let root = root.path();
        let build = root.join("store/agy/1000001000004000000");
        std::fs::create_dir_all(build.join("bin")).expect("a bin directory");
        let mark = root.join("ran");
        let bin = build.join("bin/agy");
        std::fs::write(&bin, format!("#!/bin/sh\ntouch '{}'\n", mark.display())).expect("a build");
        std::fs::set_permissions(&bin, {
            let mut p = std::fs::metadata(&bin).expect("its mode").permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
            p
        })
        .expect("executable");
        let record = root.join("store/agy/1000001000004000000.vendor");
        std::fs::write(
            &record,
            "schema = 1\nprogram = \"agy\"\nversion = \"1.4.0\"\n",
        )
        .expect("a record");
        std::os::unix::fs::symlink(&build, root.join("store/agy/current")).expect("a link");
        let via_link = root.join("store/agy/current/bin/agy");
        assert_eq!(
            build_version(&via_link.to_string_lossy()).as_deref(),
            Some("1.4.0")
        );
        assert!(!mark.exists(), "the build was never started");
        let outside = root.join("agy");
        std::os::unix::fs::symlink(&bin, &outside).expect("a link from outside the store");
        assert_eq!(
            build_version(&outside.to_string_lossy()).as_deref(),
            Some("1.4.0")
        );
        std::fs::write(&record, "schema = 1\nprogram = \"agy\"\n").expect("a record");
        assert_eq!(build_version(&bin.to_string_lossy()), None);
        std::fs::remove_file(&record).expect("removed");
        assert_eq!(build_version(&bin.to_string_lossy()), None);
        assert!(!mark.exists(), "the build was never started");
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
        // 1.4.10's anchors are owed by 1.4.10 (measured: `Muse Code 1.4.10
        // (1.4.10-R6427.1)`) and by no 1.4.9 build.
        let boxes = MUSE_ANCHORS
            .iter()
            .find(|a| a.id == "muse.box.shell")
            .map(|a| a.since)
            .expect("the shell box's anchor");
        let built = version_in("Muse Code 1.4.10 (1.4.10-R6427.1)").unwrap();
        assert_eq!(built, "1.4.10-R6427.1");
        assert!(!newer_than(boxes, &built));
        assert!(!newer_than(since, &built));
        assert!(newer_than(boxes, "1.4.9-R6297.1"));
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

    /// The literal anchors of `table` missing from `bytes`: a row whose
    /// parts (split at ` · `) are not all in them.
    fn missing(bytes: &[u8], table: &[Anchor]) -> Vec<&'static str> {
        table
            .iter()
            .filter(|a| a.kind == AnchorKind::Literal)
            .filter(|a| !a.text.split(" · ").all(|part| contains(bytes, part)))
            .map(|a| a.id)
            .collect()
    }

    fn literal_anchors_are_in_the_store(program: &str, env: &str, table: &[Anchor]) {
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
        let missing = missing(&bytes, &owed);
        assert!(
            missing.is_empty(),
            "{path}: {} literal anchors missing: {missing:?}",
            missing.len()
        );
        eprintln!(
            "{path} ({}): every literal anchor is present",
            version
                .as_deref()
                .unwrap_or("no store record: every anchor owed")
        );
    }

    #[test]
    fn every_literal_anchor_is_in_the_store_claude() {
        literal_anchors_are_in_the_store("claude", "ATERM_CLAUDE_BIN", ANCHORS);
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
    fn every_literal_agy_anchor_is_in_the_store_agy() {
        literal_anchors_are_in_the_store("agy", "ATERM_AGY_BIN", AGY_ANCHORS);
    }

    #[test]
    fn every_literal_muse_anchor_is_in_the_store_muse() {
        literal_anchors_are_in_the_store("muse", "ATERM_MUSE_BIN", MUSE_ANCHORS);
    }

    /// Gemini CLI's npm bundle, found from `bin` (a `gemini` on PATH, a
    /// Homebrew link or the script itself): the directory the script
    /// resolves into, every `.js` file in it (joined, NUL between), and the
    /// version its package's `package.json` (one up) names. `None` where
    /// `bin` leads to no `@google/gemini-cli` package: a stub, another
    /// program of that name.
    fn gemini_bundle(bin: &std::path::Path) -> Option<(String, Vec<u8>, Option<String>)> {
        let script = std::fs::canonicalize(bin).ok()?;
        let dir = script.parent()?;
        let package = std::fs::read_to_string(dir.parent()?.join("package.json")).ok()?;
        if !package.contains("\"@google/gemini-cli\"") {
            return None;
        }
        let version = package
            .split_once("\"version\"")
            .and_then(|(_, rest)| rest.split('"').nth(1))
            .map(str::to_string);
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "js"))
            .collect();
        files.sort();
        let mut bytes = Vec::new();
        for file in files {
            bytes.extend(std::fs::read(file).ok()?);
            bytes.push(0);
        }
        Some((dir.display().to_string(), bytes, version))
    }

    /// Where the Gemini CLI canary looks: `$ATERM_GEMINI_BIN`, else the first
    /// `gemini` on PATH, else Homebrew's link.
    fn gemini_bin() -> Option<std::path::PathBuf> {
        if let Some(path) = std::env::var_os("ATERM_GEMINI_BIN") {
            return Some(path.into());
        }
        std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths)
            .map(|d| d.join("gemini"))
            .chain(["/opt/homebrew/bin/gemini", "/usr/local/bin/gemini"].map(Into::into))
            .find(|p| p.is_file())
    }

    /// The bundle finder on a stand-in package: the version its
    /// `package.json` names and every `.js` file of the script's directory,
    /// found through a link. NEGATIVE CONTROLS: a package of another name,
    /// and a script with no package over it, are no bundle. Unix only: the
    /// stand-in's link is a symlink.
    #[cfg(unix)]
    #[test]
    fn the_gemini_canary_reads_the_bundle_its_script_resolves_into() {
        let root = aterm_tempfile::TempDir::new().expect("a private Gemini bundle");
        let root = root.path();
        let bundle = root.join("lib/node_modules/@google/gemini-cli/bundle");
        std::fs::create_dir_all(&bundle).expect("a bundle directory");
        std::fs::write(bundle.join("gemini.js"), b"import './chunk-A.js';").expect("a script");
        std::fs::write(bundle.join("chunk-A.js"), b"Allow once").expect("a chunk");
        std::fs::write(bundle.join("notes.txt"), b"Keep trying").expect("a file");
        let package = bundle.parent().expect("the package").join("package.json");
        std::fs::write(
            &package,
            b"{\n  \"name\": \"@google/gemini-cli\",\n  \"version\": \"0.46.0\"\n}",
        )
        .expect("a package.json");
        std::fs::create_dir_all(root.join("bin")).expect("a bin directory");
        let link = root.join("bin/gemini");
        std::os::unix::fs::symlink(bundle.join("gemini.js"), &link).expect("a link");
        let (_, bytes, version) = gemini_bundle(&link).expect("the bundle");
        assert_eq!(version.as_deref(), Some("0.46.0"));
        assert!(contains(&bytes, "Allow once") && contains(&bytes, "chunk-A.js"));
        assert!(!contains(&bytes, "Keep trying"), "only its .js files");
        std::fs::write(&package, b"{\"name\": \"another-gemini\"}").expect("a package.json");
        assert!(gemini_bundle(&link).is_none(), "another package");
        std::fs::remove_file(&package).expect("removed");
        assert!(gemini_bundle(&link).is_none(), "no package");
    }

    /// The drift canary for [`GEMINI_ANCHORS`], run and skipped as the
    /// store's are ([`literal_anchors_are_in_the_store`]), over the npm
    /// bundle ([`gemini_bundle`]).
    #[test]
    fn every_literal_gemini_anchor_is_in_its_bundle() {
        let Some((dir, bytes, version)) = gemini_bin().as_deref().and_then(gemini_bundle) else {
            eprintln!(
                "CANARY NOT RUN: no Gemini CLI bundle (set ATERM_GEMINI_BIN to its `gemini`, or \
                 `brew install gemini-cli`); anchors not checked"
            );
            return;
        };
        let (owed, newer): (Vec<Anchor>, Vec<Anchor>) = GEMINI_ANCHORS
            .iter()
            .copied()
            .partition(|a| version.as_deref().is_none_or(|v| !newer_than(a.since, v)));
        if !newer.is_empty() {
            eprintln!(
                "{dir}: {} anchor(s) are for builds newer than {}, not checked: {:?}",
                newer.len(),
                version.as_deref().unwrap_or("?"),
                newer.iter().map(|a| a.id).collect::<Vec<_>>()
            );
        }
        let missing = missing(&bytes, &owed);
        assert!(
            missing.is_empty(),
            "{dir}: {} literal anchors missing: {missing:?}",
            missing.len()
        );
        eprintln!(
            "{dir} ({}): every literal Gemini CLI anchor is present",
            version
                .as_deref()
                .unwrap_or("no package version: every anchor owed")
        );
    }
}

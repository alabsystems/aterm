// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The literal strings a supervisor guards on, in one table.
//!
//! A guard (`key if=<regex> 1`, `await match`, `turn … submit=guarded:<re>`) is only as good as
//! the text it names, and that text is the vendor's: a release that renames a header or hint
//! silently breaks a guard spelled inline elsewhere. So each string lives here with an id
//! callers import ([`anchor`]), the release it was read on, and whether it is one literal in
//! the vendor's binary or composed at run time. Codex's strings are their own table,
//! [`CODEX_ANCHORS`] (ids `codex.…`); [`anchor`] and [`anchor_text`] read both.
//!
//! Each other agent's strings are a table of its own — [`AGY_ANCHORS`]
//! (ids `agy.…`), [`MUSE_ANCHORS`] (`muse.…`), [`GEMINI_ANCHORS`]
//! (`gemini.…`) — and so are the keys of Claude Code's transcript rows the
//! session log's step fold reads, [`TRANSCRIPT_KEYS`] (`transcript.…`), and
//! the keys of Codex's rollout rows its Codex step fold reads,
//! [`ROLLOUT_KEYS`] (`rollout.…`); [`anchor`] and [`anchor_text`] read them
//! all. Codex has no screen table: aterm reads no Codex screen (its screen
//! lane was retired on 2026-10-16).
//!
//! The explicit `vendor_drift` bench reads managed builds for their literal
//! anchors and transcript/rollout keys, plus Claude's reset templates and wall
//! phrases. Run `targo --unverified bench -p aterm-phase --bench vendor_drift`.
//! Each missing vendor is COULD NOT RUN, including Gemini's npm bundle; the
//! ordinary unit suite keeps deterministic resolver and table fixtures only.
//!
//! A row is owed only by the builds that draw it ([`Anchor::drawn_by`]): those
//! from its `since` on. Build versions come from atpkg records
//! or Gemini's package.json; no vendor executable is started. The bench accepts
//! `$ATERM_CLAUDE_BIN`, `$ATERM_CODEX_BIN`, `$ATERM_AGY_BIN`, `$ATERM_MUSE_BIN`
//! and `$ATERM_GEMINI_BIN`; otherwise it resolves managed builds through
//! `aterm pkg which`, or Gemini's script on PATH/Homebrew and adjacent .js files.

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

impl Anchor {
    /// Whether a build of `version` draws this anchor: it is not older than
    /// `since`. A version that could not be read (`None`) owes every anchor:
    /// a canary that cannot tell the build judges it by the newest's rows.
    #[must_use]
    pub fn drawn_by(&self, version: Option<&str>) -> bool {
        version.is_none_or(|v| !newer_than(self.since, v))
    }
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
    // A permission box's reason block when the owner's own Claude Code configuration sends the
    // step to a person: an ask rule (`Permission rule Bash(touch:*) requires confirmation for
    // this command.`), an ask rule that overrides auto mode, a hook. A box that carries one is
    // the owner's own review (`PromptV2::owner_review`): nothing approves it.
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
    // A remote session's hook, as the head and tail every build shares: `A hook configured in
    // the remote workspace requires confirmation: <reason>` (2.1.282), `A hook run for this
    // remote session requires confirmation: <reason>` (2.1.287).
    lit("box.reason_remote_hook", "A hook ", "2.1.282"),
    lit(
        "box.reason_remote_confirm",
        "requires confirmation: ",
        "2.1.282",
    ),
    // The Bash box's `No` under Tab: an input with this placeholder, drawn after the label as
    // `No, and tell Claude what to do differently`. The supervisor's decline types its reason
    // into it.
    lit(
        "prompt.amend_no",
        "and tell Claude what to do differently",
        "2.1.280",
    ),
    hint("prompt.confirm", "Enter to confirm", "2.1.280"),
    // The note on the footer of a permission box a subagent or a workflow's agent raised: the
    // kill-agents chord (the user's `chat:killAgents` binding) and an action joined by one
    // space, where every other hint says ` to ` — `Esc to cancel · Tab to amend · ctrl+x ctrl+k
    // twice to stop background agents`, `again` once the first press is in
    // (`claude-2.1.292-box-bash-background-agent.txt`).
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
    // The folder grant on a Read box for a folder outside Claude Code's working set (`Yes, allow
    // reading from /etc and /private/etc during this session`; 2.1.287: `… from this project`);
    // a box for a path inside it has none.
    lit("box.read_grant", "allow reading from ", "2.1.282"),
    // A Read box's call, `Read(<path>)` (the tool's name and its argument,
    // joined at draw time): alone under the title in 2.1.284, between two
    // dashed rules from 2.1.287.
    composed("box.read_call", "Read(", "2.1.284"),
    lit("box.workflow", "Run a dynamic workflow?", "2.1.267"),
    // The other permission-box titles, from the 2.1.282 render code (2.1.280 is the earliest
    // build read, hence `since`). `crate::prompt::header_kind` reads a box's kind from these.
    lit("box.powershell", "PowerShell command", "2.1.280"),
    // Two more Edit titles: the notebook edit (no path row; the question
    // names the file) and the IDE diff (`Opened changes in VS Code ⧉`).
    lit("box.edit_notebook", "Edit notebook", "2.1.280"),
    lit("box.ide_diff", "Opened changes in ", "2.1.280"),
    lit("box.ide_diff.mark", "⧉", "2.1.280"),
    // WebFetch's box, the sandbox's network box and the Chrome extension's: drawn with no
    // key-hint footer, their refusal the last option, marked ` (esc)`.
    lit("box.fetch", "Fetch", "2.1.280"),
    lit(
        "box.network",
        "Network request outside of sandbox",
        "2.1.280",
    ),
    lit("box.browser", "Claude in Chrome wants to ", "2.1.280"),
    lit("box.skill", "Use skill \"", "2.1.280"),
    lit("box.skill_this", "Use this skill?", "2.1.280"),
    // The Monitor box's title is the Monitor tool's name.
    lit("box.monitor", "Monitor", "2.1.280"),
    // The generic tool box (an MCP tool) when it is titled so; titled by its card's question or
    // the tool's own name (card mode) it has no anchor and is read by its shape
    // (`crate::prompt::PromptV2::tool_card`).
    lit("box.tool", "Tool use", "2.1.280"),
    // The header's origin suffix after ` · `: `from the "<name>" workflow`, `from a workflow`,
    // `from the <name> agent`, `from a subagent`, `from a remote cloud agent`, `from the <name>
    // plugin`, `from a plugin`, read by `crate::prompt::parse_header` (which spells the named
    // forms' plain-English `from the ` itself). And the title's own parenthetical (`Bash
    // command (unsandboxed)`, `Bash command (runs on <machine>)`, the second on file boxes too).
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
    // Dialogs that are not tool permissions: a supervisor must see them (a footerless one read
    // as idle stalls a session) and answers each by its own rule, never a tool permission's
    // one-shot allow: plan mode in and out, a held cross-session message, a proposed goal, a
    // Computer Use grant (session-only), the setting that blocks reads outside the working
    // directories (persistent).
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
    // Two Select labels with a NAME mid-sentence, which Ink may wrap onto a
    // row of its own in a narrow pane: the held message's delivery and
    // Computer Use's refusal (drawn with ` (esc)` after it). `prompt`'s
    // options read a row that opens with a capital as the rest of the label
    // above only where the two make the start of one of these.
    lit("held.deliver", "Deliver this message to Claude", "2.1.282"),
    lit(
        "computer_use.deny",
        "Deny, and tell Claude what to do differently",
        "2.1.282",
    ),
    lit(
        "read_outside.title",
        "Read outside the working directories",
        "2.1.280",
    ),
    // The cache-miss confirmation before a typed `/model` or `/effort` change takes effect on a
    // warm conversation (one component for both): its two titles, its confirm (`Yes, switch to
    // <model or effort>`), its refusal, its cost-warning subtitle, and the subtitle that
    // replaces it when the person's own PreModelSwitch hook asked. The model form is measured
    // on 2.1.283; the rest read from the same render code.
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
    // The question dialog (AskUserQuestion): its free-text placeholder (`Type something.`;
    // `Type something` when several answers may be picked), its `Chat about this` row, and its
    // footer's `Enter to select` (the `… to navigate` item is read by its verb: the keys are the
    // bindings', `↑/↓` or `Tab/Arrow keys`). The free-text and chat rows are read by position in
    // a dialog read whole (`crate::question`); these texts name the kind of one that is not.
    lit("question.other", "Type something", "2.1.280"),
    lit("question.chat", "Chat about this", "2.1.280"),
    hint("question.select", "Enter to select", "2.1.280"),
    // The dialog read whole (`crate::question`): the several-question footer item and the
    // preview form's (`n to add notes`, `Tab to switch questions`), the preview's notes row (two
    // text runs, so composed), the label mark the tool's prompt asks for, the tab bar's Submit
    // chip, the review tab's rows, and the AFK countdown row the host may draw under the dialog.
    // The multi-select button (`Next`/`Submit`) and the review's `Cancel` are read by position,
    // never by anchor: aterm-gui and aterm-search spell those words too.
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
    // The tool result's row when a person pressed Esc on the dialog or `2. Cancel` on its review
    // tab: the model is told to stop, so the turn it ends is a person's stop
    // (`crate::turn::interrupted`), never one to continue.
    lit(
        "question.declined",
        "User declined to answer questions",
        "2.1.282",
    ),
    // A dialog withheld while the composer holds a draft, drawn dim in its place (its dash is a
    // `—` escape in the binary, so composed). And the REPL's row while a panel holds the
    // message queue (`Background task update` or `<N> background task updates`, then this),
    // which may sit under a dialog.
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
    // binary), with seven suffixes on 2.1.282 and nine on 2.1.307;
    // `crate::prompt::rm_breaker_of` reads the whole family. `box.rm_breaker`
    // is the possibly-empty-variable member (the one the supervisor's
    // rm-breaker rule resolves); the rest were read from the
    // 2.1.280/2.1.281/2.1.282 binaries (the mounted drive's and the nested
    // why's from later ones, below), and
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
    // `rm -r` of a mounted drive or the top of one: in the 2.1.304, 2.1.306
    // and 2.1.307 binaries, MEASURED on 2.1.307 (2026-10-21, `rm -rf
    // /Volumes/zz-lab-none` in the lab).
    lit(
        "box.rm_breaker.mounted_drive",
        "on mounted drive path: ",
        "2.1.304",
    ),
    lit(
        "box.rm_breaker.too_many",
        "too many command substitutions to analyze (",
        "2.1.280",
    ),
    // 2.1.307 qualifies the `— <why>` forms as `not ruled out — <why>`
    // (`Dangerous rm operation not ruled out — too many command substitutions
    // to analyze (65)`, MEASURED in the lab 2026-10-21), and adds a why of
    // its own (read from the 2.1.307 binary).
    lit("box.rm_breaker.not_ruled_out", "not ruled out ", "2.1.307"),
    lit(
        "box.rm_breaker.nested_unanalyzed",
        "some nested commands or function substitutions could not be analyzed",
        "2.1.307",
    ),
    // A box in a session read as unattended carries a countdown row under its notes (`⚠ Claude
    // Code will automatically deny this request in 1:59, to avoid blocking progress on an
    // unattended session`), the time ticking: its own row, never a note.
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
    // The usage-limit options dialog (`rate_limit_options_menu`): its title, its stop row, and
    // the one row the supervisor may choose — `Wait here, then continue automatically` followed
    // by ` shortly`, ` at <time>` or ` when the limit resets`.
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
    // `‹ ` and ` ›` is the stepper's), and its footer — drawn from
    // `↑`/`\xB7` escapes, so composed for the canary.
    lit(
        "autocompact.title",
        "We recommend compacting earlier",
        "2.1.302",
    ),
    lit("autocompact.question", "Want to autocompact at", "2.1.302"),
    lit("autocompact.accept", "Autocompact at", "2.1.302"),
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
    // The pill-less footer row's left hint slot under the composer (`? for shortcuts` at rest,
    // the escape key's `clear` hint with a draft): a live REPL's own row where no mode pill is
    // drawn (`aterm_agent::harness::footer::live_repl_row`).
    lit("footer.shortcuts", "? for shortcuts", "2.1.283"),
    hint("footer.clear", "esc to clear", "2.1.283"),
    // The permission-mode pill on that row: a glyph, the mode's indicator and ` on`, joined at
    // run time (`⏵⏵ bypass permissions on`, `⏸ manual mode on`), then the cycle key's hint
    // (`(shift+tab to cycle)`). The footer and the mode light read the mode from it
    // (`aterm_agent::harness::footer`).
    composed("mode.bypass", "bypass permissions on", "2.1.283"),
    composed("mode.auto", "auto mode on", "2.1.283"),
    composed("mode.accept_edits", "accept edits on", "2.1.283"),
    composed("mode.dont_ask", "don't ask on", "2.1.283"),
    composed("mode.plan", "plan mode on", "2.1.283"),
    composed("mode.manual", "manual mode on", "2.1.283"),
    hint("mode.cycle", "to cycle", "2.1.283"),
    // Vim's INSERT indicator at that row's head, before the pill, while the
    // composer's vim editor mode (`editorMode: "vim"`) is in INSERT: drawn
    // dim from parts (the 2.1.304 bundle's `FooterHintLine`: `["-- ", mode,
    // " --"]`, none in NORMAL), then one cell, then the pill — MEASURED
    // `  -- INSERT -- ⏵⏵ auto mode on (shift+tab to cycle)`, the pill at
    // column 15 (`aterm_agent::harness::footer::vim_insert`).
    composed("vim.insert", "-- INSERT --", "2.1.304"),
    // The agents panel under that row (`crate::agents_panel`), read in the
    // 2.1.304 bundle (its `TQ` panel, `bie` main row, `vie` agent row, `Cie`
    // idle summary, `BB` workflow row, `PHe` status parts): the main row's
    // word after its bullet, the `↑ N more` / `↓ N more` rows' word, the idle
    // summary's noun (`1 idle agent`, `N idle agents`), the token count's
    // word, the status's three words in place of an elapsed time, the queued
    // count's word, and the large-workflow warning's two parts.
    lit("panel.main", " main", "2.1.304"),
    lit("panel.more", " more", "2.1.304"),
    lit("panel.idle_agents", "idle agent", "2.1.304"),
    lit("panel.tokens", " tokens", "2.1.304"),
    lit("panel.awaiting", "awaiting approval", "2.1.304"),
    lit("panel.idle", "idle", "2.1.304"),
    lit("panel.waiting", "waiting", "2.1.304"),
    lit("panel.queued", " queued", "2.1.304"),
    lit("panel.large_workflow", "Large workflow", "2.1.304"),
    lit("panel.workflows_stop", "/workflows to stop", "2.1.304"),
    // A turn a person stopped with Esc: the vendor's row under the last message or tool row
    // (`  ⎿  Interrupted · What should Claude do instead?`).
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
    // The other rating cards, drawn by the session survey's component in the same slot above
    // the composer, each with the options row `1: Bad … 0: Dismiss` under its question.
    // `survey.plugin` is the head both its spellings share (`How helpful has the <plugin>
    // plugin been?`, `How helpful has this plugin been?`); the memory card is either of the two
    // heads, the recollection row being its last message row.
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
    // The rating's aftermath in the card's slot after a `1`/`2`/`3`: for 3 s `Feedback: <Bad |
    // Fine | Good | Unsure> · Esc to undo` (the key is the user's `escape` binding), then for 5 s
    // `Thanks for the feedback!` over `(Optional) Press [1] to tell us what went wrong ·
    // /feedback`, or, with the follow-up off, `Use /feedback to share detailed feedback
    // anytime.` (never captured).
    lit("survey.pending", "Feedback: ", "2.1.286"),
    hint("survey.pending_undo", "Esc to undo", "2.1.286"),
    lit("survey.thanks", "Thanks for the feedback!", "2.1.286"),
    lit("survey.optional_press", "(Optional) Press [", "2.1.286"),
    lit(
        "survey.feedback_anytime",
        "Use /feedback to share detailed feedback anytime.",
        "2.1.286",
    ),
    // The Claude-drafted-feedback card (`SendFeedback`, behind an account flag), parked in the
    // survey slot but no survey: readers step over it and nothing presses it. Its head is
    // `<glyph> <Label> drafted: <title>`; its hint row is composed from key hints.
    lit("card.drafted", "drafted:", "2.1.284"),
    hint(
        "card.drafts_hint",
        "1 to review · 2 to send · 0 to dismiss",
        "2.1.284",
    ),
    // `Don't ask again`, the choice the transcript-share ask (`y: Yes    n: No     d: Don't ask
    // again`) and the web-setup offer share: it tells those prompts from a rating card, whose
    // options row ends `0: Dismiss`. Never pressed.
    lit("card.dont_ask", "Don't ask again", "2.1.284"),
    lit("context.indicator", "until auto-compact", "2.1.267"),
    // The launch card's name row (`▐▛███▜▌   Claude Code v2.1.282`): a
    // session with no turn yet (`turn::fresh`).
    lit("launch.banner", "Claude Code v", "2.1.267"),
    // What an exiting Claude Code prints under its last frame, alone on its row at column 0
    // (`Resume this session with:`, then `claude --resume <id>`), after `/exit` or Ctrl-C twice:
    // every row above it was drawn by a process that has exited (`phase::busy_zone_start`).
    lit("exit.resume", "Resume this session with:", "2.1.283"),
    lit("goal.active", "/goal active", "2.1.278"),
    // The model-refusal pause (`refusal_fallback_prompt`): its title and its second option's
    // head (`Edit prompt and retry with <model>`).
    lit("paused.title", "Session paused", "2.1.282"),
    lit("paused.edit", "Edit prompt and retry", "2.1.282"),
    lit(
        "notice.update",
        "Update installed · Restart to update",
        "2.1.267",
    ),
    // The UPDATE NOTICE under the launch card, drawn on the first launch of
    // each new build (`lastReleaseNotesSeen` older than the build; forced by
    // `CLAUDE_CODE_FORCE_FIRST_LAUNCH=<older version>`): a bold summary
    // (`Updated to latest.`, or `Updated to latest. Got 33 features, 240
    // bugfixes, and 103 other changes.`) over a dim details row whose link
    // is the changelog's URL without its `https://` and ` for details` after
    // it, in a Box with `paddingLeft: 2` (2.1.304's `ty`/`_a` render code;
    // the inline captures `claude-2.1.283-effort-switch.txt` and the
    // supervisor's 2.1.282 `cap-decline-*` screens). The harness banner is
    // drawn over them on the glass (`crate::notice`). The details row is
    // composed (the link, then ` for details`): matched whole, never
    // canaried.
    lit("notice.updated", "Updated to latest.", "2.1.282"),
    lit(
        "notice.changelog",
        "code.claude.com/docs/en/changelog",
        "2.1.282",
    ),
    composed(
        "notice.updated_details",
        "code.claude.com/docs/en/changelog for details",
        "2.1.282",
    ),
    // The vendor's own `⏺` paragraphs that are no turn's reply: readers of what the worker last
    // said step over them (`phase::vendor_paragraph`). The auto-mode notice is drawn over a
    // session's first turn in auto mode, and after the first reply of one relaunched
    // `--permission-mode auto --resume <id>`.
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
    // The results of Claude Code's model and effort commands as its transcript records them
    // under `<local-command-stdout>`: what aterm's footer names the model and effort by once
    // chosen (`aterm_agent::harness::footer`). `Set model to ` is in transcripts from 2.1.201
    // on (ANSI-bold, later backticks).
    lit("model.set", "Set model to ", "2.1.201"),
    lit("model.kept", "Kept model as ", "2.1.283"),
    lit("effort.set", "Set effort level to ", "2.1.283"),
    lit("effort.current", "Current effort level: ", "2.1.283"),
    lit("effort.auto", "Effort level set to auto", "2.1.283"),
    // The top-effort key's own `/effort <key> on|off` results and refusals,
    // READ in the 2.1.304 bundle (its `D()` answer and `A()` parser): `<Key>
    // on (this session only): <what it does>. Effort stays <level>.`, `<Key>
    // off. Effort stays <level>.`, `<Key> needs dynamic workflows enabled (see
    // /config). …` and `<Key> isn't available on <model>. …`. SUFFIX-ONLY:
    // the key's capitalised head is compared against
    // `aterm_primer::CLAUDE_TOP_EFFORT_KEY` by the reader
    // (`aterm_agent::harness::footer`), so no anchor spells the key.
    lit("effort.top_on", " on (this session only): ", "2.1.304"),
    lit("effort.top_off", " off. Effort stays ", "2.1.304"),
    lit("effort.stays", ". Effort stays ", "2.1.304"),
    lit(
        "effort.top_needs",
        " needs dynamic workflows enabled",
        "2.1.304",
    ),
    lit("effort.top_unavailable", " isn't available on ", "2.1.304"),
    // The effort result's last clause, the key's state (`… · <Key> off`:
    // READ in the 2.1.304 bundle's `D()` answer, the state a word after the
    // key; SUFFIX-ONLY, as above), and the tail of the level hint Claude
    // draws over the top rule a moment after an `/effort` (`◐ medium ·
    // /effort`, MEASURED on 2.1.304). COMPOSED: the bundle joins them. The
    // clauses' ` · ` join is no anchor: aterm's own rows draw it too (the
    // tap's `5h 42% · 7d 61%`), and no anchor's text is on one of them.
    composed("effort.key_on", " on", "2.1.304"),
    composed("effort.key_off", " off", "2.1.304"),
    composed("effort.hint_tail", " \u{b7} /effort", "2.1.304"),
    lit("fast.on", "Fast mode ON", "2.1.283"),
    lit("fast.model_set", "model set to ", "2.1.283"),
    // The Settings panel's Usage tab (`crate::panel`,
    // `docs/SPEC-claude-accounts-probe-2026-10-01.md`), read in the 2.1.287 bundle: the loading
    // and refresh lines (`…`-ended on screen, an escape in the bundle, so the rows stop before
    // it), the four enrich-error heads, the failed load's reason (after `Error: `), and the row
    // Escape leaves under the inline renderer. Only `Refreshing…` and `Settings dialog
    // dismissed` were seen on a screen. `usage.refreshing` is one word: match it through
    // `crate::anchor`, never a quoted copy.
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
    // in the 2.1.304 bundle (the rows `since` 2.1.287 hold the words that
    // build kept) and NONE WAS SEEN on a screen or in a transcript: the
    // offer's texts are one table there (`Ug`), the kinds' words its
    // functions (`X()`, `ce()`, `Mt()`), the pending lines and the answers
    // their own literals, and the server's flag payload may replace any text
    // of the table. The rows are the words the readers match by, never a
    // whole template — a template's placeholders are no text a screen shows
    // — so a `{kind}` is one of the four kind rows, and a text that holds
    // one is its fixed parts either side: the grant row's head and the words
    // between its count and its day (`Use a {kind} ({left} left, until
    // {date})`), which the confirmation's title shares (`Use a {kind}?`);
    // the kinds, as `{kind}` draws them and as `{kindResets}` counts the
    // five-hour one (`2 five-hour resets`); the weekly session reset's row,
    // which the vendor draws only where no grant row is; the head of the
    // confirmation's body and its two joins (`Refills your {limits} now ·
    // your weekly reset day stays {week}`, `{kindResets} left · use by
    // {deadline}`), its yes's head (`Yes, use my {kind}`) and its no, and the
    // words either side of the kind in the early lines' tail (`… — use your
    // {kind} anyway?`); the two pending lines, each `…`-ended on the screen
    // (an escape in the bundle, so the rows carry the words without it); and
    // of the answers the ones a reader tells apart — each kind's head of the
    // spent line (`{done}`), the still-walled line's words after its kind,
    // the two lifted lines, the word after the kind in the kept line, the
    // login line's — with the ending the vendor adds where it queued no
    // continuation of its own. The other answers are no rows on purpose: a
    // line no row names is a failure. The 2.1.287 wording (`Use your reset
    // now (…)`, `Use your reset?`, `Yes, use my reset`, `Reset kept`) has no
    // rows: the readers refuse it (approval.rs's `spends` still reads its
    // words as a spend).
    lit("reset.row", "Use a", "2.1.304"),
    lit("reset.row.until", "left, until", "2.1.287"),
    lit("reset.kind.session", "5-hour reset", "2.1.304"),
    lit("reset.kind.weekly", "weekly reset", "2.1.304"),
    lit("reset.kind.fable", "Fable reset", "2.1.304"),
    lit("reset.kind.full", "full reset", "2.1.304"),
    lit("reset.kind.session_count", "five-hour reset", "2.1.304"),
    lit(
        "reset.weekly_row",
        "Reset your session limit now",
        "2.1.287",
    ),
    lit("reset.confirm.refills", "Refills your", "2.1.287"),
    lit(
        "reset.confirm.stays",
        "now · your weekly reset day stays",
        "2.1.287",
    ),
    lit("reset.confirm.use_by", "left · use by", "2.1.287"),
    lit("reset.yes", "Yes, use my", "2.1.304"),
    lit("reset.no", "No, keep it", "2.1.287"),
    lit("reset.early", "use your", "2.1.304"),
    lit("reset.early.anyway", "anyway?", "2.1.304"),
    lit("reset.checking", "Checking for a reset", "2.1.287"),
    lit("reset.resetting", "Resetting your limits", "2.1.287"),
    lit("reset.done.session", "Session limit reset", "2.1.304"),
    lit("reset.done.weekly", "Weekly limits reset", "2.1.304"),
    lit(
        "reset.done.fable",
        "Fable reset used · what Fable used is back in your weekly limit",
        "2.1.304",
    ),
    lit("reset.done.full", "Limits reset", "2.1.287"),
    lit("reset.walled", "used · you're still at your", "2.1.304"),
    lit(
        "reset.lifted",
        "Your limit had already reset on its own · nothing was used",
        "2.1.287",
    ),
    lit("reset.clear", "Your limits are clear", "2.1.287"),
    lit("reset.kept", "kept", "2.1.304"),
    lit(
        "reset.login",
        "Couldn't reset your limits with this login",
        "2.1.287",
    ),
    lit("reset.continue", "send a message to continue", "2.1.287"),
    // The walls (see `wall::PHRASES` for the whole classification table).
    lit("wall.fable", "You've reached your Fable limit", "2.1.267"),
    lit("wall.auto_continue", "continuing automatically", "2.1.268"),
    // How that auto-continue ended, as the vendor's notice opens (`wall::auto_continue_end`): it
    // went on (at the reset, or early when usage came back), or it will not go on by itself (Esc
    // on its countdown, stopped, turned off, or a reset that passed with nothing sent).
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
    // The command Claude Code submits itself to open its usage-limit dialog
    // (a reset over 24 h away), echoed as `❯ /rate-limit-options` with the
    // dialog's answer under it (`wall::left_wall`).
    lit("wall.limit_dialog", "/rate-limit-options", "2.1.300"),
    lit("wall.context", "Context limit reached", "2.1.280"),
    lit("wall.server", "This is a server-side issue", "2.1.280"),
    lit(
        "wall.overloaded",
        "Repeated 529 Overloaded errors",
        "2.1.280",
    ),
    lit("wall.login", "Please run /login", "2.1.280"),
    // Claude Code's `/login` dialog (crate::login; MEASURED on 2.1.303 in
    // the real-Claude lab, the success state READ in its bundle): the
    // method picker and its three methods, Console's own menu, the browser
    // hand-off, the success state, and the row Esc leaves.
    lit("login.method", "Select login method:", "2.1.303"),
    lit(
        "login.subscription",
        "Claude account with subscription",
        "2.1.303",
    ),
    lit("login.console", "Anthropic Console account", "2.1.303"),
    lit("login.platform", "3rd-party platform", "2.1.303"),
    lit(
        "login.console_how",
        "How do you want to sign in?",
        "2.1.303",
    ),
    lit("login.opening", "Opening browser to sign in", "2.1.303"),
    lit("login.no_browser", "Browser didn't open?", "2.1.303"),
    lit("login.paste", "Paste code here if prompted", "2.1.303"),
    composed(
        "login.success",
        "Login successful. Press Enter to continue",
        "2.1.303",
    ),
    lit("login.interrupted", "Login interrupted", "2.1.303"),
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
    // Claude Code's critical-memory banner, right-aligned on its own row between the spinner
    // and the composer's top rule (`<anchor> (140.4GB) — restart and resume with claude
    // --continue`). Placed by `wall::memory_wall`, never by `PHRASES`: a composer draft quoting
    // it carries the same words.
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
    // measured in the lab; `phase::block_is_vendors`): the
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
    // The stop-hook summary's OWN labelled rows, the first of which ends the
    // hooks' words under it (2.1.304's bundle: `N??"Stop"," hook error: "`
    // per blocking error, `"Stop hook feedback: "` per context a hook added;
    // `phase::is_summary_label`).
    lit("hook.error.label", " hook error:", "2.1.302"),
    lit("hook.feedback", "Stop hook feedback: ", "2.1.302"),
    // Claude Code's own LABEL OVER A HOOK'S VERDICT ON A PROMPT, the head of
    // the `⏺` message it draws for it (`turn::said_message`: no words of the
    // worker's, and no answer to the `❯` row over it). A hook's
    // `{"continue": false}`: `Operation stopped by hook`, then `: ` and its
    // `stopReason` where it gave one (measured on 2.1.302; the bundle's own
    // constant on 2.1.303) — drawn under the prompt's own `❯` row. A hook's
    // exit 2: the event's name, then ` operation blocked by hook:` and its
    // stderr on the next row (UserPromptSubmit measured on 2.1.302; a slash
    // command's UserPromptExpansion read from the 2.1.303 bundle) — the
    // prompt drawn under it as `Original prompt: …`, never as a `❯` row.
    lit("hook.stopped", "Operation stopped by hook", "2.1.302"),
    lit(
        "hook.prompt_blocked",
        "UserPromptSubmit operation blocked by hook:",
        "2.1.302",
    ),
    lit(
        "hook.expansion_blocked",
        "UserPromptExpansion operation blocked by hook:",
        "2.1.303",
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
    // Usage credits carrying a MODEL's requests (READ in the 2.1.301 to
    // 2.1.304 bundles, the `fable-usage-credits` notification; never
    // captured): a credits-only model billed per request with no dialog, its
    // consent stored or its tier billed by an admin, says so only as `<M> is
    // drawing from usage credits` (once a process, then gone) or `<M> is now
    // using usage credits instead of your plan limits` — the model's name
    // first, so each is read as the SEGMENT'S END. And lower-priority mode's
    // end as credits take over: `Extra usage is now covering your requests ·
    // lower-priority mode ended`. Read by place, `credits::in_use`.
    lit(
        "credits.model_drawing",
        " is drawing from usage credits",
        "2.1.301",
    ),
    lit(
        "credits.model_instead",
        " is now using usage credits instead of your plan limits",
        "2.1.301",
    ),
    lit(
        "credits.extra_covering",
        "Extra usage is now covering your requests",
        "2.1.301",
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
    // The background-work panel under the composer's bottom rule, one row an
    // item (`  ● [17:50:42] <what> running`, `  ● Agent(research: <name>)
    // Working · 1s`), and the footer's counts of that work (`· 1 task(s) ·
    // /tasks`, `· 1 subagent(s)`: the binary's `%d task(s)` and `%d
    // subagent(s)`). Measured in the lab on 2026-10-17 (`agy-1.4.0-lab-bg-*`).
    lit("agy.panel.running", " running", "1.4.0"),
    lit("agy.panel.agent", "Agent(", "1.4.0"),
    lit("agy.panel.working", "Working", "1.4.0"),
    lit("agy.footer.tasks", " task(s)", "1.4.0"),
    lit("agy.footer.subagents", " subagent(s)", "1.4.0"),
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
    // The status row's first frame, the verb alone (`◇ Thinking`; the
    // capture `lab-busy-first-frame`).
    lit("muse.busy.first", "Thinking", "1.4.10-R6427.1"),
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
    // A panel branch's state while its work runs: a backgrounded shell's
    // `running` (`└ ◈ Wait in the background  running  13s`), a monitor's
    // `watching` (`└ ◇ Watch the lab socket  watching  14s`). A finished
    // one says `ran 1s, exit 0` or `failed` (the lab's captures,
    // 2026-10-16 and 2026-10-17).
    lit("muse.panel.running", "running", "1.4.10-R6427.1"),
    lit("muse.panel.watching", "watching", "1.4.10-R6427.1"),
    // The transcript's pointer to that panel, `(↓ to view terminals)` (the
    // binary holds ` to view ` and puts the rest together; the lab's capture
    // `end-of-turn-work-hint`).
    composed("muse.panel.view", "(↓ to view ", "1.4.10-R6427.1"),
    // The head of a `/status` card and the run's state at its right, `│
    // MUSE CODE 1.4.10 / sea-pasiphae  RUNNING │`: `IDLE` before any turn,
    // `RUNNING` while one runs, `COMPLETED` after a turn, `FAILED` after a
    // wall, `CANCELLED` after a person's Esc — each put in capitals at run
    // time (the lab's captures `status`, `status-mid-turn`,
    // `limit-status-cards`, `fresh-status-cards`, `interrupted-status-cards`,
    // `limit-unnamed-status-cards`, `completed-status-cards`).
    composed("muse.card.status", "MUSE CODE ", "1.4.10-R6427.1"),
    composed("muse.card.idle", "IDLE", "1.4.10-R6427.1"),
    composed("muse.card.running", "RUNNING", "1.4.10-R6427.1"),
    composed("muse.card.completed", "COMPLETED", "1.4.10-R6427.1"),
    composed("muse.card.failed", "FAILED", "1.4.10-R6427.1"),
    composed("muse.card.cancelled", "CANCELLED", "1.4.10-R6427.1"),
    // A `/status` card's count of the tokens the session spent, `│  USAGE
    // 0 tokens · 0 turns · 0 subagents │` (`0` before any turn: the lab's
    // captures `fresh-status-cards`, `interrupted-status-cards`).
    composed("muse.card.usage", "USAGE", "1.4.10-R6427.1"),
    composed("muse.card.tokens", "tokens", "1.4.10-R6427.1"),
    // A `/usage` card's count of the session's turns, `│    Turns    1 │`
    // (`0` before any, and after a first turn a person stopped: the lab's
    // captures `usage`, `fresh-status-cards`).
    composed("muse.card.turns", "Turns", "1.4.10-R6427.1"),
    // A signed-in `/status` card's account and billing rows and a signed-in
    // `/usage` card's subscription rows (MEASURED 2026-10-21 on
    // 1.4.11-R6667.1 signed in with a Meta account, the capture
    // `usage-signed-in`): `│  ACCOUNT        Name (address) │`,
    // `│  BILLING        Subscription · Muse Code Max Usage │`,
    // `│    Weekly         71% used · Resets Oct 25 at 5:00 PM │`. The
    // API-key lab captures draw none of them.
    composed("muse.card.account", "ACCOUNT", "1.4.11-R6667.1"),
    composed("muse.card.billing", "BILLING", "1.4.11-R6667.1"),
    composed("muse.card.subscription", "Subscription", "1.4.11-R6667.1"),
    composed("muse.card.weekly", "Weekly", "1.4.11-R6667.1"),
    composed("muse.card.used", "% used", "1.4.11-R6667.1"),
    composed("muse.card.session_usage", "Session usage", "1.4.10-R6427.1"),
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
    // Its read row, two columns in with no `◆` (`  Read README.md`; the
    // captures `box-network-digit-approved`, `lab-end-of-turn-read-row`).
    lit("muse.tool.read", "Read ", "1.4.10-R6427.1"),
    // Its search row, two columns in too (`  Searched "parser" (1 match)`;
    // the capture `lab-tool-row-search`), and the heads of its other own
    // `◆` tool rows the lab measured (`lab-tool-row-read-memory`,
    // `-add-memory`, `-goal`, `-work-list`). `Used tool` is put together at
    // run time (no one literal in the binary holds it).
    lit("muse.tool.searched", "Searched ", "1.4.10-R6427.1"),
    lit("muse.tool.read_memory", "Read memory", "1.4.10-R6427.1"),
    lit("muse.tool.saved_memory", "Saved memory", "1.4.10-R6427.1"),
    lit("muse.tool.read_goal", "Read goal", "1.4.10-R6427.1"),
    composed("muse.tool.used", "Used tool", "1.4.10-R6427.1"),
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
    // The error a turn ends on once its usage-limit (or no-access) dialog
    // is answered `2. Stop`: `✕ [API Error: An unknown error occurred.]`.
    lit(
        "gemini.error.unknown",
        "An unknown error occurred.",
        "0.46.0",
    ),
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
    // The usage-limit dialog: a per-day quota's is pressed by no rule; a
    // spent quota's that names its reset is kept trying past it.
    lit("gemini.usage.title", "Usage limit reached for", "0.46.0"),
    lit("gemini.usage.retry", "Keep trying", "0.46.0"),
    // A Google account's spent quota names its reset under the title:
    // `Access resets at 5:06 AM PDT.` (`gemini::limit_reset`).
    lit("gemini.usage.resets", "Access resets at ", "0.46.0"),
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
    // The notices about the folder's trust drawn when a session starts or
    // its trust level changes, under a resumed reply: chrome, not the
    // turn's end (`gemini::trust_notices_above`). The first is the head of
    // two literals (`… because a parent folder is trusted.`, `… because the
    // connected IDE workspace is trusted.`).
    lit(
        "gemini.notice.still_trusted",
        "Note: This folder is still trusted because",
        "0.46.0",
    ),
    lit(
        "gemini.notice.untrusted",
        "This folder is untrusted, project settings, hooks, MCPs, and GEMINI.md files will not be applied for this folder.",
        "0.46.0",
    ),
    lit(
        "gemini.notice.agents_skipped",
        "Skipping project agents due to untrusted folder.",
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

/// The KEYS of Claude Code's transcript rows the session log's step fold
/// (`aterm-agent`'s `harness::steps`) depends on (ids `transcript.…`): a
/// key a build renames is a tool call, turn end or wall the fold no longer
/// sees, so each is a literal the drift canary greps the store's build for
/// (`every_transcript_key_is_in_the_store_claude`), as it does the screen's
/// strings. Each was read on 2.1.294's transcripts (`aterm-agent`'s
/// `supervise/fixtures/claude-2.1.294-transcript-*.jsonl`), and the fold
/// spells its raw-byte tests from this table ([`anchor_text`]), so the
/// two cannot drift apart.
pub const TRANSCRIPT_KEYS: &[Anchor] = &[
    lit("transcript.tool_use", "tool_use", "2.1.294"),
    lit("transcript.tool_result", "tool_result", "2.1.294"),
    lit("transcript.tool_use_id", "tool_use_id", "2.1.294"),
    lit("transcript.is_error", "is_error", "2.1.294"),
    lit("transcript.turn_duration", "turn_duration", "2.1.294"),
    lit("transcript.durationMs", "durationMs", "2.1.294"),
    lit("transcript.compact_boundary", "compact_boundary", "2.1.294"),
    lit("transcript.isCompactSummary", "isCompactSummary", "2.1.294"),
    lit(
        "transcript.queueTranscriptOnly",
        "queueTranscriptOnly",
        "2.1.294",
    ),
    lit(
        "transcript.isApiErrorMessage",
        "isApiErrorMessage",
        "2.1.294",
    ),
    lit("transcript.isSidechain", "isSidechain", "2.1.294"),
    lit("transcript.isMeta", "isMeta", "2.1.294"),
    lit("transcript.promptId", "promptId", "2.1.294"),
    lit("transcript.parentUuid", "parentUuid", "2.1.294"),
    lit("transcript.local_command", "local_command", "2.1.294"),
];

/// The KEYS of Codex's rollout rows the session log's Codex step fold
/// (`aterm-agent`'s `harness::codex_steps`) reads (ids `rollout.…`): the
/// row and payload types it opens, ends, interrupts, calls and compacts by,
/// the keys it names a thread, a turn, a call and a model by, the one wall
/// word it reads, and the one call it classes by name. ONLY literal strings a committed rollout shows, each
/// `since` the build it was read on — the lab's Codex 0.163.0 rollouts
/// (session log 5G, 2026-10-19), reduced and committed as `aterm-agent`'s
/// `harness/testdata/steps/codex-0.163.0.jsonl` and its `-subagent` leg
/// (`every_rollout_key_is_in_its_committed_rollout`), never a guessed key.
/// The codex-alab builds 0.164.0-dev.12521 and 12479 the lab measured
/// (`lab codex-alab-proof`, their legs committed as
/// `codex-alab-<release>-<leg>.jsonl`, `rollout_reduction_stem`) spell
/// every key of the steps their rollouts show as 0.163.0 did, and add one
/// call name the fold classes: `exec_command`, a shell command; so does
/// the forever-port build 0.164.0-dev.12672 (B5c), with no new key.
/// A key a build renames is a turn, a call or a wall the fold no longer
/// sees, so the drift canary greps the store's Codex build for each
/// (`every_rollout_key_is_in_the_store_codex`), as it does Claude Code's
/// transcript keys, and the fold spells its keys from this table
/// ([`anchor_text`]), so the two cannot drift apart.
pub const ROLLOUT_KEYS: &[Anchor] = &[
    // The rows: a thread's own first row, a turn's context (the one key a
    // turn opens by), an event, a model's item, a compaction.
    lit("rollout.session_meta", "session_meta", "0.163.0"),
    lit("rollout.turn_context", "turn_context", "0.163.0"),
    lit("rollout.event_msg", "event_msg", "0.163.0"),
    lit("rollout.response_item", "response_item", "0.163.0"),
    lit("rollout.compacted", "compacted", "0.163.0"),
    // The events and items it reads.
    lit("rollout.task_complete", "task_complete", "0.163.0"),
    lit("rollout.turn_aborted", "turn_aborted", "0.163.0"),
    lit(
        "rollout.thread_settings_applied",
        "thread_settings_applied",
        "0.163.0",
    ),
    lit("rollout.token_count", "token_count", "0.163.0"),
    lit("rollout.function_call", "function_call", "0.163.0"),
    lit(
        "rollout.function_call_output",
        "function_call_output",
        "0.163.0",
    ),
    lit("rollout.custom_tool_call", "custom_tool_call", "0.163.0"),
    lit(
        "rollout.custom_tool_call_output",
        "custom_tool_call_output",
        "0.163.0",
    ),
    // The keys it reads them by.
    lit("rollout.id", "id", "0.163.0"),
    lit("rollout.forked_from_id", "forked_from_id", "0.163.0"),
    lit("rollout.cli_version", "cli_version", "0.163.0"),
    lit("rollout.thread_id", "thread_id", "0.163.0"),
    lit("rollout.turn_id", "turn_id", "0.163.0"),
    lit("rollout.call_id", "call_id", "0.163.0"),
    lit("rollout.name", "name", "0.163.0"),
    lit("rollout.namespace", "namespace", "0.163.0"),
    lit("rollout.duration_ms", "duration_ms", "0.163.0"),
    lit("rollout.last_token_usage", "last_token_usage", "0.163.0"),
    lit("rollout.total_token_usage", "total_token_usage", "0.163.0"),
    lit("rollout.model", "model", "0.163.0"),
    lit("rollout.effort", "effort", "0.163.0"),
    lit("rollout.error", "error", "0.163.0"),
    lit("rollout.codex_error_info", "codex_error_info", "0.163.0"),
    // The one call the fold classes by name, in its namespace: a subagent
    // (`spawn_agent` in `collaboration`; the lab's subagent leg).
    lit("rollout.collaboration", "collaboration", "0.163.0"),
    lit("rollout.spawn_agent", "spawn_agent", "0.163.0"),
    // And a shell command, in no namespace (codex-alab's approval leg: the
    // command its model ran, approved).
    lit(
        "rollout.exec_command",
        "exec_command",
        "0.164.0-dev.12479+g07a25d86a",
    ),
    // The usage wall's word, on the turn's `task_complete` (the lab's
    // usage leg: the stub's 429 `usage_limit_reached`).
    lit(
        "rollout.usage_limit_exceeded",
        "usage_limit_exceeded",
        "0.163.0",
    ),
];

/// Whether version `a` is newer than `b`, numerically part by part, a
/// revision's parts after the version's (`1.4.9-R6297.1` is `1.4.9.6297.1`;
/// a part that is no number reads 0). The drift canaries owe an anchor only
/// to the builds it was read on, and the step fold reads a Claude Code
/// build newer than the newest it measured as unmeasured.
#[must_use]
pub fn newer_than(a: &str, b: &str) -> bool {
    let parts = |v: &str| {
        v.replace("-R", ".")
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parts(a) > parts(b)
}

/// The name stem a Codex build's committed rollout reductions carry in
/// `aterm-agent`'s step testdata (`harness/testdata/steps/<stem>.jsonl`, or
/// a leg of it, `<stem>-<leg>.jsonl`): `codex-<build>`, and for an ALab
/// build, whose version is a pre-release with its commit as build metadata
/// (`X.Y.Z-dev.N+g<sha>`, the publication scheme a codex-alab package
/// names), `codex-alab-<the version before its +>`. The
/// one rule both the key table's test and the step fold's read it by.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn rollout_reduction_stem(build: &str) -> String {
    match build.split_once('+') {
        Some((release, meta)) if release.contains("-dev.") && meta.starts_with('g') => {
            format!("codex-alab-{release}")
        }
        _ => format!("codex-{build}"),
    }
}

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
        .chain(TRANSCRIPT_KEYS)
        .chain(ROLLOUT_KEYS)
        .find(|a| a.id == id)
    {
        Some(a) => a.text,
        None => panic!("aterm-phase has no anchor {id:?}"),
    }
}

/// [`anchor`] at compile time, for a caller that needs the text in a `const`
/// (`const NOTE: &str = anchor_text("box.rm_breaker");`): an unknown id fails the build.
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
    let mut i = 0;
    while i < TRANSCRIPT_KEYS.len() {
        if same(TRANSCRIPT_KEYS[i].id, id) {
            return TRANSCRIPT_KEYS[i].text;
        }
        i += 1;
    }
    let mut i = 0;
    while i < ROLLOUT_KEYS.len() {
        if same(ROLLOUT_KEYS[i].id, id) {
            return ROLLOUT_KEYS[i].text;
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
    use crate::vendor_drift::*;
    #[cfg(unix)]
    use crate::vendor_files::*;

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
        let managed = format!("agy → {} — managed 1.4.0", via_link.display());
        assert_eq!(canary_path(&managed, Some(root)), Some(via_link.clone()));
        assert_eq!(
            canary_path("agy — absent from PATH", None),
            None,
            "no unmanaged path is guessed without a default store"
        );
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

    /// [`newer_than`] orders versions by number, part by part, never by
    /// text: `2.1.300` is newer than `2.1.30` and `2.1.299`, a longer
    /// version newer than its own prefix, a revision after its version.
    /// NEGATIVE CONTROLS: a version is not newer than itself, and the text
    /// order (`"2.1.9" > "2.1.10"`) is not the rule.
    #[test]
    fn newer_than_orders_versions_by_number() {
        for (a, b) in [
            ("2.1.300", "2.1.30"),
            ("2.1.300", "2.1.299"),
            ("2.1.10", "2.1.9"),
            ("2.1.294.1", "2.1.294"),
            ("1.4.9-R6297.1", "1.4.9"),
            ("1.4.10-R6427.1", "1.4.9-R6297.1"),
            ("3", "2.9.9"),
        ] {
            assert!(newer_than(a, b), "{a} > {b}");
            assert!(!newer_than(b, a), "{b} > {a}");
        }
        assert!(!newer_than("2.1.300", "2.1.300"));
        assert!("2.1.9" > "2.1.10" && !newer_than("2.1.9", "2.1.10"));
    }

    /// The transcript's keys are their own table, read by [`anchor`] and
    /// [`anchor_text`] like any other, and every one is a literal the
    /// canary looks for. NEGATIVE CONTROL: no id is two tables'.
    #[test]
    fn the_transcript_keys_are_literals_the_lookups_read() {
        for a in TRANSCRIPT_KEYS {
            assert_eq!(a.kind, AnchorKind::Literal, "{}", a.id);
            assert_eq!(anchor(a.id), a.text);
            assert_eq!(anchor_text(a.id), a.text);
            assert_eq!(a.id.strip_prefix("transcript."), Some(a.text));
        }
        let mut ids: Vec<&str> = ANCHORS
            .iter()
            .chain(AGY_ANCHORS)
            .chain(MUSE_ANCHORS)
            .chain(GEMINI_ANCHORS)
            .chain(TRANSCRIPT_KEYS)
            .chain(ROLLOUT_KEYS)
            .map(|a| a.id)
            .collect();
        let all = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all, "an id is two anchors'");
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

    /// Where the lab's Codex rollouts are committed, reduced (`aterm-agent`'s
    /// step testdata, session log 5G, 6D-3a): `<stem>.jsonl` and its legs,
    /// `<stem>-<leg>.jsonl`, the stem [`rollout_reduction_stem`]'s.
    const ROLLOUTS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aterm-agent/src/harness/testdata/steps"
    );

    /// The committed rollout reductions of `build`, joined, each checked to
    /// open with the reducer's own line naming that build.
    fn committed_rollouts(build: &str) -> Vec<u8> {
        let stem = rollout_reduction_stem(build);
        let leg = format!("{stem}-");
        let mut names: Vec<String> = std::fs::read_dir(ROLLOUTS)
            .expect("the step testdata")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| {
                n.strip_suffix(".jsonl")
                    .is_some_and(|b| b == stem || b.starts_with(&leg))
            })
            .collect();
        names.sort();
        let head = format!("# codex {build} ");
        let mut out = Vec::new();
        for n in names {
            let text = std::fs::read_to_string(format!("{ROLLOUTS}/{n}")).expect("a reduction");
            assert!(text.starts_with(&head), "{n} is a reduction of {build}");
            out.extend_from_slice(text.as_bytes());
        }
        out
    }

    /// every rollout key is a literal the committed rollout of
    /// the build it names (`since`) shows, quoted — a JSON key or a closed
    /// word — never a guess; and the lookups read it like any anchor.
    /// NEGATIVE CONTROLS: the planted key is named missing, and a build with
    /// no committed rollout shows none.
    #[test]
    fn every_rollout_key_is_in_its_committed_rollout() {
        let quoted = |a: &Anchor| -> Vec<u8> { format!("\"{}\"", a.text).into_bytes() };
        for a in ROLLOUT_KEYS {
            assert_eq!(a.kind, AnchorKind::Literal, "{}", a.id);
            assert_eq!(anchor(a.id), a.text);
            assert_eq!(anchor_text(a.id), a.text);
            assert_eq!(a.id.strip_prefix("rollout."), Some(a.text));
            let rollouts = committed_rollouts(a.since);
            assert!(!rollouts.is_empty(), "{}: no rollout of {}", a.id, a.since);
            assert!(
                contains_bytes(&rollouts, &quoted(a)),
                "{}: {:?} is not in the committed rollout of {}",
                a.id,
                a.text,
                a.since
            );
        }
        // The codex-alab build the lab measured (6D-3a) needed one new key,
        // the shell call's name (`rollout.exec_command`, checked above
        // against the earlier measured build's rollouts, and here against
        // this one's): what else its rollouts show of the steps it took (a
        // turn's context and end, its tokens, a call and its output, the
        // usage wall, the thread's own settings line) is spelled as 0.163.0
        // spelled it.
        for build in [ALAB_BUILD, FOREVER_BUILD] {
            let alab = committed_rollouts(build);
            for key in [
                "rollout.session_meta",
                "rollout.turn_context",
                "rollout.task_complete",
                "rollout.thread_settings_applied",
                "rollout.token_count",
                "rollout.function_call",
                "rollout.function_call_output",
                "rollout.cli_version",
                "rollout.turn_id",
                "rollout.call_id",
                "rollout.duration_ms",
                "rollout.last_token_usage",
                "rollout.total_token_usage",
                "rollout.model",
                "rollout.codex_error_info",
                "rollout.usage_limit_exceeded",
                "rollout.exec_command",
            ] {
                let a = ROLLOUT_KEYS.iter().find(|a| a.id == key).expect("a key");
                assert!(contains_bytes(&alab, &quoted(a)), "{key} on {build}");
            }
            // NEGATIVE CONTROL.
            assert!(!contains_bytes(&alab, &quoted(&PLANTED_ROLLOUT_KEY[0])));
        }
        // NEGATIVE CONTROLS.
        let rollouts = committed_rollouts("0.163.0");
        assert!(!contains_bytes(&rollouts, &quoted(&PLANTED_ROLLOUT_KEY[0])));
        assert!(committed_rollouts("9.9.999").is_empty());
    }

    /// The codex-alab build the lab measured.
    const ALAB_BUILD: &str = "0.164.0-dev.12521+g3d3eb12d8";

    /// The forever-port codex-alab build the lab measured (B5c), whose
    /// rollouts spell every key of the steps they show as 0.163.0 did.
    const FOREVER_BUILD: &str = "0.164.0-dev.12672+g089291d37";

    /// THE NAME RULE (6D-3a): a vendor build's reductions are
    /// `codex-<build>`, an ALab build's `codex-alab-<release>`, its commit
    /// left to the reducer's own line; each committed reduction is of the
    /// build its own line names. NEGATIVE CONTROLS: build metadata that is
    /// no ALab commit, or on no pre-release, keeps the whole build.
    #[test]
    fn a_rollout_reduction_is_named_by_its_build() {
        assert_eq!(rollout_reduction_stem("0.163.0"), "codex-0.163.0");
        assert_eq!(
            rollout_reduction_stem(ALAB_BUILD),
            "codex-alab-0.164.0-dev.12521"
        );
        assert!(!committed_rollouts(ALAB_BUILD).is_empty());
        for e in std::fs::read_dir(ROLLOUTS)
            .expect("the step testdata")
            .filter_map(Result::ok)
        {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("codex-") {
                continue;
            }
            let text = std::fs::read_to_string(e.path()).expect("a reduction");
            let build = text
                .strip_prefix("# codex ")
                .and_then(|t| t.split(' ').next())
                .unwrap_or_else(|| panic!("{name} opens with the reducer's line"));
            let stem = rollout_reduction_stem(build);
            assert!(
                name == format!("{stem}.jsonl") || name.starts_with(&format!("{stem}-")),
                "{name} is a reduction of {build}"
            );
        }
        // NEGATIVE CONTROLS.
        assert_eq!(
            rollout_reduction_stem("1.0.0+g3d3eb12"),
            "codex-1.0.0+g3d3eb12"
        );
        assert_eq!(
            rollout_reduction_stem("0.164.0-dev.1+build7"),
            "codex-0.164.0-dev.1+build7"
        );
    }

    /// THE RESET'S TEMPLATES ARE ITS ANCHORS' WORDS, WHOLE: each is built
    /// from the rows the readers match by, none is empty or another's, and
    /// a text that spells them all holds every one. A template with two
    /// spellings (the model limits' strip, 2.1.306's and 2.1.307's) holds in
    /// a text with either: the older spellings alone, or the newest alone.
    /// NEGATIVE CONTROLS: that text with the confirmation's yes reworded
    /// misses that template, a planted template of a kind no build names is
    /// missing from it, and the newest spellings without the strip's helper
    /// body (the helper's argument alone) miss the helper row.
    #[test]
    fn the_reset_templates_are_built_from_the_readers_rows() {
        type Templates = Vec<(&'static str, Vec<String>)>;
        let templates = reset_templates();
        let mut names: Vec<&str> = templates.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), templates.len(), "a name is two templates'");
        assert!(
            templates
                .iter()
                .all(|(_, spellings)| !spellings.is_empty()
                    && spellings.iter().all(|t| !t.is_empty()))
        );
        let spelled = |templates: &[(&'static str, Vec<String>)]| {
            templates
                .iter()
                .flat_map(|(_, spellings)| spellings.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join("|")
                .into_bytes()
        };
        let bytes = spelled(&templates);
        assert!(missing_templates(&bytes, &templates).is_empty());
        let one = |pick: fn(&[String]) -> &String| -> Templates {
            templates
                .iter()
                .map(|(name, spellings)| (*name, vec![pick(spellings).clone()]))
                .collect()
        };
        let (older, newest) = (one(|s| &s[0]), one(|s| &s[s.len() - 1]));
        assert!(missing_templates(&spelled(&older), &templates).is_empty());
        assert!(missing_templates(&spelled(&newest), &templates).is_empty());
        // The strip's two spellings as the 2.1.306 and 2.1.307 bundles hold
        // them (each once, read from the store's binaries 2026-10-21).
        let strip = templates
            .iter()
            .find(|(name, _)| *name == "model limit names' strip")
            .map(|(_, spellings)| spellings.as_slice());
        assert_eq!(
            strip,
            Some(
                &[
                    ".replace(/ limit$/,\"\"));break;case\"seven_day_sonnet\":".to_string(),
                    ",\" limit\"));break;case\"seven_day_sonnet\":".to_string(),
                ][..]
            )
        );
        let no_helper: Templates = newest
            .iter()
            .filter(|(name, _)| *name != "model limit names' strip helper")
            .cloned()
            .collect();
        assert_eq!(
            missing_templates(&spelled(&no_helper), &templates),
            ["model limit names' strip helper"]
        );
        let reworded: Templates = templates
            .iter()
            .map(|(name, spellings)| {
                (
                    *name,
                    spellings
                        .iter()
                        .map(|t| t.replace(anchor("reset.yes"), "Yes, spend my"))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            missing_templates(&spelled(&reworded), &templates),
            ["confirmation yes"]
        );
        let planted = [(
            "planted",
            vec![format!(
                "{} planted-by-no-build reset?",
                anchor("reset.row")
            )],
        )];
        assert_eq!(missing_templates(&bytes, &planted), ["planted"]);
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
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vendor literal anchor tables and transcript/rollout key registries.
//!
//! [`anchor`] and [`anchor_text`] resolve Claude Code, [`CODEX_ANCHORS`], [`AGY_ANCHORS`],
//! [`MUSE_ANCHORS`], [`GEMINI_ANCHORS`], [`TRANSCRIPT_KEYS`], and [`ROLLOUT_KEYS`] entries gated
//! by [`Anchor::drawn_by`].

/// How an anchor's text reaches the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    /// One string in the vendor's binary.
    Literal,
    /// Composed at run time from the user's key bindings.
    KeyHint,
    /// Assembled at run time from parts.
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
    /// Whether a build of `version` draws this anchor.
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
    lit("prompt.proceed", "Do you want to proceed?", "2.1.267"),
    composed("prompt.edit", "Do you want to make this edit to", "2.1.280"),
    hint("prompt.cancel", "Esc to cancel", "2.1.267"),
    hint("prompt.amend", "Tab to amend", "2.1.267"),
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
    // Claude Code's `no-rule-match` note on a Bash box (2.1.308's bundle): no allow rule matched
    // and its own checks did not clear the command (aterm-agent's `no-rule-read-only`).
    lit(
        "box.note_no_rule",
        "This command requires approval",
        "2.1.284",
    ),
    lit("box.reason_remote_hook", "A hook ", "2.1.282"),
    lit(
        "box.reason_remote_confirm",
        "requires confirmation: ",
        "2.1.282",
    ),
    lit(
        "prompt.amend_no",
        "and tell Claude what to do differently",
        "2.1.280",
    ),
    hint("prompt.confirm", "Enter to confirm", "2.1.280"),
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
    lit("box.read_grant", "allow reading from ", "2.1.282"),
    composed("box.read_call", "Read(", "2.1.284"),
    lit("box.workflow", "Run a dynamic workflow?", "2.1.267"),
    lit("box.powershell", "PowerShell command", "2.1.280"),
    lit("box.edit_notebook", "Edit notebook", "2.1.280"),
    lit("box.ide_diff", "Opened changes in ", "2.1.280"),
    lit("box.ide_diff.mark", "⧉", "2.1.280"),
    lit("box.fetch", "Fetch", "2.1.280"),
    lit(
        "box.network",
        "Network request outside of sandbox",
        "2.1.280",
    ),
    lit("box.browser", "Claude in Chrome wants to ", "2.1.280"),
    lit("box.skill", "Use skill \"", "2.1.280"),
    lit("box.skill_this", "Use this skill?", "2.1.280"),
    lit("box.monitor", "Monitor", "2.1.280"),
    lit("box.tool", "Tool use", "2.1.280"),
    lit("origin.workflow", "from a workflow", "2.1.280"),
    lit("origin.subagent", "from a subagent", "2.1.280"),
    lit("origin.remote", "from a remote cloud agent", "2.1.280"),
    lit("origin.plugin", "from a plugin", "2.1.280"),
    lit("header.unsandboxed", " (unsandboxed)", "2.1.280"),
    lit("header.runs_on", " (runs on ", "2.1.280"),
    composed("option.esc", " (esc)", "2.1.280"),
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
    lit("plan.enter", "Enter plan mode?", "2.1.280"),
    lit("plan.ready", "Ready to code?", "2.1.280"),
    lit("plan.exit", "Exit plan mode?", "2.1.280"),
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
    lit(
        "setup.chrome_continue",
        "Continue with browser tools",
        "2.1.301",
    ),
    lit("settings.error", "Settings Error", "2.1.301"),
    lit("question.other", "Type something", "2.1.280"),
    lit("question.chat", "Chat about this", "2.1.280"),
    hint("question.select", "Enter to select", "2.1.280"),
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
    lit(
        "question.declined",
        "User declined to answer questions",
        "2.1.282",
    ),
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
        "box.rm_breaker.mounted_drive",
        "on mounted drive path: ",
        "2.1.304",
    ),
    lit(
        "box.rm_breaker.too_many",
        "too many command substitutions to analyze (",
        "2.1.280",
    ),
    lit("box.rm_breaker.not_ruled_out", "not ruled out ", "2.1.307"),
    lit(
        "box.rm_breaker.nested_unanalyzed",
        "some nested commands or function substitutions could not be analyzed",
        "2.1.307",
    ),
    composed(
        "box.auto_deny",
        "Claude Code will automatically deny this request in",
        "2.1.281",
    ),
    lit("trust.title", "Accessing workspace:", "2.1.280"),
    lit("trust.yes", "Yes, I trust this folder", "2.1.280"),
    lit("trust.no", "No, exit", "2.1.280"),
    lit(
        "trust.no_backstop",
        "No, continue without these permissions",
        "2.1.280",
    ),
    lit("limit.title", "What do you want to do?", "2.1.282"),
    lit("limit.stop", "Stop and wait for limit to reset", "2.1.282"),
    lit(
        "limit.wait",
        "Wait here, then continue automatically",
        "2.1.282",
    ),
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
    lit("busy.interrupt", "esc to interrupt", "2.1.267"),
    lit("footer.shortcuts", "? for shortcuts", "2.1.283"),
    hint("footer.clear", "esc to clear", "2.1.283"),
    composed("mode.bypass", "bypass permissions on", "2.1.283"),
    composed("mode.auto", "auto mode on", "2.1.283"),
    composed("mode.accept_edits", "accept edits on", "2.1.283"),
    composed("mode.dont_ask", "don't ask on", "2.1.283"),
    composed("mode.plan", "plan mode on", "2.1.283"),
    composed("mode.manual", "manual mode on", "2.1.283"),
    hint("mode.cycle", "to cycle", "2.1.283"),
    composed("vim.insert", "-- INSERT --", "2.1.304"),
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
    lit(
        "turn.interrupted",
        "Interrupted · What should Claude do instead?",
        "2.1.280",
    ),
    lit(
        "turn.idle_compacted",
        "Compacted while idle, before the prompt cache expired",
        "2.1.300",
    ),
    lit("turn.recap", "recap:", "2.1.301"),
    lit(
        "survey.question",
        "How is Claude doing this session",
        "2.1.267",
    ),
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
    lit("survey.pending", "Feedback: ", "2.1.286"),
    hint("survey.pending_undo", "Esc to undo", "2.1.286"),
    lit("survey.thanks", "Thanks for the feedback!", "2.1.286"),
    lit("survey.optional_press", "(Optional) Press [", "2.1.286"),
    lit(
        "survey.feedback_anytime",
        "Use /feedback to share detailed feedback anytime.",
        "2.1.286",
    ),
    lit("card.drafted", "drafted:", "2.1.284"),
    hint(
        "card.drafts_hint",
        "1 to review · 2 to send · 0 to dismiss",
        "2.1.284",
    ),
    lit("card.dont_ask", "Don't ask again", "2.1.284"),
    lit("context.indicator", "until auto-compact", "2.1.267"),
    lit("launch.banner", "Claude Code v", "2.1.267"),
    lit("exit.resume", "Resume this session with:", "2.1.283"),
    lit("goal.active", "/goal active", "2.1.278"),
    lit("paused.title", "Session paused", "2.1.282"),
    lit("paused.edit", "Edit prompt and retry", "2.1.282"),
    lit(
        "notice.update",
        "Update installed · Restart to update",
        "2.1.267",
    ),
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
    lit("model.set", "Set model to ", "2.1.201"),
    lit("model.kept", "Kept model as ", "2.1.283"),
    lit("effort.set", "Set effort level to ", "2.1.283"),
    lit("effort.current", "Current effort level: ", "2.1.283"),
    lit("effort.auto", "Effort level set to auto", "2.1.283"),
    lit("effort.top_on", " on (this session only): ", "2.1.304"),
    lit("effort.top_off", " off. Effort stays ", "2.1.304"),
    lit("effort.stays", ". Effort stays ", "2.1.304"),
    lit(
        "effort.top_needs",
        " needs dynamic workflows enabled",
        "2.1.304",
    ),
    lit("effort.top_unavailable", " isn't available on ", "2.1.304"),
    composed("effort.key_on", " on", "2.1.304"),
    composed("effort.key_off", " off", "2.1.304"),
    composed("effort.hint_tail", " \u{b7} /effort", "2.1.304"),
    lit("fast.on", "Fast mode ON", "2.1.283"),
    lit("fast.model_set", "model set to ", "2.1.283"),
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
    lit("wall.fable", "You've reached your Fable limit", "2.1.267"),
    lit("wall.auto_continue", "continuing automatically", "2.1.268"),
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
    lit("wall.limit_dialog", "/rate-limit-options", "2.1.300"),
    lit("wall.context", "Context limit reached", "2.1.280"),
    lit("wall.server", "This is a server-side issue", "2.1.280"),
    lit(
        "wall.overloaded",
        "Repeated 529 Overloaded errors",
        "2.1.280",
    ),
    lit("wall.login", "Please run /login", "2.1.280"),
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
    lit("wall.api_error", "API Error", "2.1.283"),
    lit("wall.goal_paused", "Goal paused", "2.1.280"),
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
    lit("wall.compaction_failed", "Compaction failed", "2.1.286"),
    lit("wall.cannot_be_compacted", "cannot be compacted", "2.1.300"),
    lit(
        "wall.not_enough_to_compact",
        "Not enough messages to compact",
        "2.1.286",
    ),
    lit(
        "wall.compact_images",
        "Run /compact to remove old images",
        "2.1.300",
    ),
    lit(
        "wall.compact_attachments",
        "pushed the request over the limit. Run /compact",
        "2.1.300",
    ),
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
    composed("wall.refused.identity", "Verify your identity", "2.1.300"),
    composed("wall.refused.dlp", "data loss prevention", "2.1.300"),
    lit(
        "wall.usage_credits_required",
        "Usage credits required",
        "2.1.300",
    ),
    lit("wall.high_demand", "high demand", "2.1.300"),
    lit("wall.memory", "Critical memory usage", "2.1.280"),
    lit(
        "wall.disk_full",
        "Transcript writes are failing (",
        "2.1.302",
    ),
    lit("wall.disk_full.tool", "Disk full: ", "2.1.309"),
    lit(
        "wall.disk_full.tool_inodes",
        "Nearly out of inodes: ",
        "2.1.309",
    ),
    lit(
        "wall.disk_full.tool_quota",
        "Disk quota used up: ",
        "2.1.309",
    ),
    lit(
        "wall.disk_full.tool_dir",
        " (Claude Code's temporary directory)",
        "2.1.309",
    ),
    // A command 2.1.309 did not start because it could not create the file
    // that captures its output (`zy(`The command was not started: ${ki} on
    // the volume holding …`)`, `ki` naming the code in parentheses).
    lit(
        "wall.disk_full.pre_start",
        "The command was not started: ",
        "2.1.309",
    ),
    lit(
        "wall.disk_full.pre_start_full",
        "this computer's disk is full (",
        "2.1.309",
    ),
    lit(
        "wall.disk_full.pre_start_quota",
        "the user's disk quota is used up (",
        "2.1.309",
    ),
    // The rest of that template: `${ki} on the volume holding ${dir} (Claude
    // Code's temporary directory), so the file … nothing ran. ${e9}`, `e9`
    // being the advice the bundle keeps in a variable of its own.
    lit(
        "wall.disk_full.pre_start_volume",
        " on the volume holding ",
        "2.1.309",
    ),
    lit(
        "wall.disk_full.pre_start_ran",
        ", so the file that captures command output could not be created there and nothing ran.",
        "2.1.309",
    ),
    lit(
        "wall.disk_full.free_space",
        "Free space on that volume, or restart Claude Code with CLAUDE_CODE_TMPDIR set to a \
         directory on another one, then retry. If this is the user's own computer, tell them \
         before deleting anything to make room.",
        "2.1.309",
    ),
    // A failed command's result opens `Exit code <n>` (the bundle's
    // `[`Exit code ${e.code}`, …]`, read on 2.1.304, 2.1.308 and 2.1.309):
    // the line the notice is drawn under.
    lit("wall.disk_full.tool_exit", "Exit code ", "2.1.304"),
    lit(
        "wall.disk_full.goal",
        "Goal cleared after an unrecoverable error (",
        "2.1.302",
    ),
    lit("hook.ran", "Ran ", "2.1.294"),
    composed("hook.ran.stop", "stop hook", "2.1.294"),
    lit("hook.error", " hook error", "2.1.294"),
    lit(
        "hook.blocking_error",
        " hook returned blocking error",
        "2.1.302",
    ),
    lit("hook.error.label", " hook error:", "2.1.302"),
    lit("hook.feedback", "Stop hook feedback: ", "2.1.302"),
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
    lit("usage.warning", "You've used", "2.1.300"),
    lit("login.expires_in", "Your login expires in ", "2.1.300"),
    lit("login.renew_hint", "run /login to renew", "2.1.300"),
];

/// Every string aterm guards on in agy's screen.
pub const AGY_ANCHORS: &[Anchor] = &[
    lit("agy.footer.idle", "? for shortcuts", "1.4.0"),
    lit("agy.footer.cancel", "esc to cancel", "1.4.0"),
    hint("agy.footer.exit", "press ctrl+c again to exit", "1.4.0"),
    hint(
        "agy.footer.exit_ctrl_d",
        "press ctrl+d again to exit",
        "1.4.0",
    ),
    lit("agy.footer.exit_again", "again to exit", "1.4.0"),
    hint("agy.keys.navigate", "↑/↓ Navigate", "1.4.0"),
    lit("agy.footer.queued", "to edit queued messages", "1.4.0"),
    lit("agy.panel.running", " running", "1.4.0"),
    lit("agy.panel.agent", "Agent(", "1.4.0"),
    lit("agy.panel.working", "Working", "1.4.0"),
    lit("agy.footer.tasks", " task(s)", "1.4.0"),
    lit("agy.footer.subagents", " subagent(s)", "1.4.0"),
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
    lit("agy.banner", "Antigravity CLI", "1.4.0"),
    lit("agy.turn.interrupted", "Interrupted · What should", "1.4.0"),
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
    lit("agy.subagent.approval", " needs approval for ", "1.4.0"),
    lit("agy.subagent.question", " has a question", "1.4.0"),
    lit(
        "agy.signin.wall",
        "You are currently not signed in.",
        "1.4.0",
    ),
    lit("agy.signin.method", "Select login method:", "1.4.0"),
    lit("agy.signin.oauth", "Google OAuth", "1.4.0"),
    lit(
        "agy.trust.title",
        "Do you trust the contents of this project?",
        "1.4.0",
    ),
    lit("agy.trust.yes", "Yes, I trust this folder", "1.4.0"),
    lit("agy.trust.no", "No, exit", "1.4.0"),
    lit("agy.title.command", "Command", "1.4.0"),
    lit("agy.title.create", "Create file", "1.4.0"),
    lit("agy.title.edit", "Pending edit", "1.4.0"),
    lit("agy.title.question", "Question", "1.4.0"),
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
    lit(
        "agy.edit.esc_disabled",
        "Esc disabled during file edits",
        "1.4.0",
    ),
    lit("agy.question.write_in", "Write-in...", "1.4.0"),
    lit("agy.question.recommended", "(Recommended)", "1.4.0"),
    lit("agy.question.unchecked", "[ ] ", "1.4.0"),
    lit("agy.question.checked", "[x] ", "1.4.0"),
    lit("agy.question.written", "✓ ", "1.4.0"),
    lit("agy.box.persisted", "(Persist to settings.json)", "1.4.0"),
    lit("agy.box.conversation", "in this conversation", "1.4.0"),
    lit("agy.error.id", "Error ID:", "1.4.0"),
    lit(
        "agy.survey.question",
        "How's the CLI experience so far? Help us improve:",
        "1.4.0",
    ),
    composed("agy.survey.skip", "[0] Skip", "1.4.0"),
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
    lit("agy.onboarding.previous", "[Previous]", "1.4.0"),
];

/// Every string aterm guards on in Muse Code's screen.
pub const MUSE_ANCHORS: &[Anchor] = &[
    lit(
        "muse.composer.voice",
        "Voice input (⌥ + v to start)",
        "1.4.9-R6297.1",
    ),
    lit("muse.busy.interrupt", "esc to interrupt", "1.4.9-R6297.1"),
    lit("muse.busy.first", "Thinking", "1.4.10-R6427.1"),
    lit("muse.banner", "Muse Code", "1.4.9-R6297.1"),
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
    lit(
        "muse.trust.title",
        "Do you trust this workspace?",
        "1.4.9-R6297.1",
    ),
    lit("muse.trust.path", "Workspace:", "1.4.9-R6297.1"),
    lit("muse.trust.yes", "Trust and continue", "1.4.9-R6297.1"),
    lit("muse.trust.quit", "Quit", "1.4.9-R6297.1"),
    lit("muse.trust.keys", "Esc quits.", "1.4.9-R6297.1"),
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
    lit("muse.box.reject", "Reject", "1.4.10-R6427.1"),
    lit(
        "muse.box.socket_refused",
        "Unix socket access was not approved",
        "1.4.10-R6427.1",
    ),
    lit("muse.panel.select", "↓ to select", "1.4.10-R6427.1"),
    lit("muse.panel.running", "running", "1.4.10-R6427.1"),
    lit("muse.panel.watching", "watching", "1.4.10-R6427.1"),
    composed("muse.panel.view", "(↓ to view ", "1.4.10-R6427.1"),
    composed("muse.card.status", "MUSE CODE ", "1.4.10-R6427.1"),
    composed("muse.card.idle", "IDLE", "1.4.10-R6427.1"),
    composed("muse.card.running", "RUNNING", "1.4.10-R6427.1"),
    composed("muse.card.completed", "COMPLETED", "1.4.10-R6427.1"),
    composed("muse.card.failed", "FAILED", "1.4.10-R6427.1"),
    composed("muse.card.cancelled", "CANCELLED", "1.4.10-R6427.1"),
    composed("muse.card.usage", "USAGE", "1.4.10-R6427.1"),
    composed("muse.card.tokens", "tokens", "1.4.10-R6427.1"),
    composed("muse.card.turns", "Turns", "1.4.10-R6427.1"),
    composed("muse.card.account", "ACCOUNT", "1.4.11-R6667.1"),
    composed("muse.card.billing", "BILLING", "1.4.11-R6667.1"),
    composed("muse.card.subscription", "Subscription", "1.4.11-R6667.1"),
    composed("muse.card.weekly", "Weekly", "1.4.11-R6667.1"),
    composed("muse.card.used", "% used", "1.4.11-R6667.1"),
    composed("muse.card.session_usage", "Session usage", "1.4.10-R6427.1"),
    lit(
        "muse.resume.abnormal",
        "the previous run of this session ended abnormally",
        "1.4.10-R6427.1",
    ),
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
    lit("muse.recap", "Recap: ", "1.4.10-R6427.1"),
    lit("muse.worked", "Worked for ", "1.4.10-R6427.1"),
    lit("muse.tool.ran", "Ran ", "1.4.10-R6427.1"),
    lit("muse.tool.wrote", "Wrote ", "1.4.10-R6427.1"),
    lit("muse.tool.edited", "Edited ", "1.4.10-R6427.1"),
    lit("muse.tool.read", "Read ", "1.4.10-R6427.1"),
    lit("muse.tool.searched", "Searched ", "1.4.10-R6427.1"),
    lit("muse.tool.read_memory", "Read memory", "1.4.10-R6427.1"),
    lit("muse.tool.saved_memory", "Saved memory", "1.4.10-R6427.1"),
    lit("muse.tool.read_goal", "Read goal", "1.4.10-R6427.1"),
    composed("muse.tool.used", "Used tool", "1.4.10-R6427.1"),
    lit("muse.tool.fetched", "Fetched", "1.4.10-R6427.1"),
    lit(
        "muse.tool.invalid_work",
        "Invalid work ID",
        "1.4.10-R6427.1",
    ),
    lit("muse.queued", "Queued input · ↑ to edit", "1.4.10-R6427.1"),
    lit(
        "muse.footer.steering",
        "steering the running turn",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.footer.queued",
        "queued for the next turn",
        "1.4.10-R6427.1",
    ),
    lit(
        "muse.footer.replacing",
        "replacing the running turn",
        "1.4.10-R6427.1",
    ),
    lit("muse.title.spinner", "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏", "1.4.10-R6427.1"),
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
    lit("muse.retry.api", "Meta Model API ", "1.4.10-R6427.1"),
    lit("muse.retry.in", " · retrying in ", "1.4.10-R6427.1"),
    lit("muse.retry.attempt", " · attempt ", "1.4.10-R6427.1"),
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
    lit("muse.box.always_prefix", "Always allow ", "1.4.10-R6427.1"),
    lit("muse.notice.finished", "Finished ", "1.4.10-R6427.1"),
    lit("muse.notice.message", "Message from ", "1.4.10-R6427.1"),
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

/// Every string aterm guards on in Gemini CLI's screen.
pub const GEMINI_ANCHORS: &[Anchor] = &[
    lit("gemini.banner", "Gemini CLI", "0.46.0"),
    lit("gemini.status.shortcuts", "? for shortcuts", "0.46.0"),
    lit("gemini.status.cancel", "(esc to cancel, ", "0.46.0"),
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
    // The footer's last column head (its value under it: `Auto`, or the
    // model the person chose), and the banner's login row
    // (`UserIdentity`: `Authenticated with <authType> /auth`, or `Signed in
    // with Google: <email> /auth`).
    lit("gemini.footer.model", "/model", "0.46.0"),
    lit("gemini.banner.auth", "Authenticated with ", "0.46.0"),
    lit("gemini.banner.google", "Signed in with Google", "0.46.0"),
    lit("gemini.banner.auth.tail", " /auth", "0.46.0"),
    lit("gemini.turn.cancelled", "Request cancelled.", "0.46.0"),
    lit("gemini.error.api", "[API Error:", "0.46.0"),
    lit(
        "gemini.error.unknown",
        "An unknown error occurred.",
        "0.46.0",
    ),
    lit("gemini.error.failed", "This request failed", "0.46.0"),
    lit("gemini.box.exec.ask", "Allow execution of", "0.46.0"),
    lit("gemini.box.redirection", "Redirection detected.", "0.46.0"),
    lit("gemini.box.change.ask", "Apply this change?", "0.46.0"),
    lit("gemini.box.writing", "Writing to ", "0.46.0"),
    lit("gemini.box.once", "Allow once", "0.46.0"),
    lit("gemini.box.session", "Allow for this session", "0.46.0"),
    lit("gemini.box.session.tail", "for this session", "0.46.0"),
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
    lit("gemini.question.title", "Answer Questions", "0.46.0"),
    lit("gemini.question.custom", "Enter a custom value", "0.46.0"),
    hint("gemini.question.keys", "Enter to select", "0.46.0"),
    lit("gemini.question.review", "Review your answers:", "0.46.0"),
    hint("gemini.question.submit", "Enter to submit", "0.46.0"),
    lit("gemini.usage.title", "Usage limit reached for", "0.46.0"),
    lit("gemini.usage.retry", "Keep trying", "0.46.0"),
    // The per-model bucket's dialog (bundle: `ProQuotaDialog`, a terminal
    // quota on a model whose fallback is another): `Switch to <fallback>`
    // (a template), over `Upgrade for higher limits` under the Google login,
    // over `Stop` — no `Keep trying` on offer.
    composed("gemini.usage.switch", "Switch to ", "0.46.0"),
    lit(
        "gemini.usage.upgrade",
        "Upgrade for higher limits",
        "0.46.0",
    ),
    lit("gemini.usage.resets", "Access resets at ", "0.46.0"),
    composed("gemini.queued", "Queued (press ↑ to edit):", "0.46.0"),
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
    lit(
        "gemini.trust.title",
        "Do you trust the files in this folder?",
        "0.46.0",
    ),
    lit("gemini.trust.folder", "Trust folder (", "0.46.0"),
    lit("gemini.trust.parent", "Trust parent folder (", "0.46.0"),
    lit("gemini.trust.no", "Don't trust", "0.46.0"),
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

/// The keys of Claude Code's transcript rows the session log's step fold.
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

/// The keys of Codex's rollout rows the session log's Codex step fold.
pub const ROLLOUT_KEYS: &[Anchor] = &[
    lit("rollout.session_meta", "session_meta", "0.163.0"),
    lit("rollout.turn_context", "turn_context", "0.163.0"),
    lit("rollout.event_msg", "event_msg", "0.163.0"),
    lit("rollout.response_item", "response_item", "0.163.0"),
    lit("rollout.compacted", "compacted", "0.163.0"),
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
    lit("rollout.collaboration", "collaboration", "0.163.0"),
    lit("rollout.spawn_agent", "spawn_agent", "0.163.0"),
    lit(
        "rollout.exec_command",
        "exec_command",
        "0.164.0-dev.12479+g07a25d86a",
    ),
    lit(
        "rollout.usage_limit_exceeded",
        "usage_limit_exceeded",
        "0.163.0",
    ),
];

/// Whether version `a` is newer than `b`, numerically part by part.
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

/// The name stem a Codex build's committed rollout reductions carry in `aterm-agent`'s step testdata.
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

/// The text of the anchor `id`.
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

/// [`anchor`] at compile time.
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

/// The first `words` words of anchor `id` as a guard regex ([`guard_regex`]).
#[must_use]
pub fn guard_prefix(id: &str, words: usize) -> String {
    let text = anchor(id);
    let cut = text.split(' ').take(words).map(str::len).sum::<usize>() + words.saturating_sub(1);
    guard_regex(&text[..cut.min(text.len())])
}

/// `text` as a regex that matches it literally in a `key if=` / `await match` guard.
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
    /// mark if it ran.
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
    /// canary looks for.
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
    /// and owed by none.
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
            assert!(!contains_bytes(&alab, &quoted(&PLANTED_ROLLOUT_KEY[0])));
        }
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
    /// build its own line names.
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
    /// a text that spells them all holds every one.
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
                .flat_map(|(_, spellings)| spellings.iter().map(|t| t.replace(JS_NAME, "a")))
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

    /// A MINIFIER'S NAME IN A TEMPLATE IS A SLOT: the limit names' switch
    /// holds as 2.1.308 spells it (`s.add(a.givesBackWeekly…`) and as
    /// 2.1.309 does (`i.givesBackWeekly`), and with any identifier there.
    /// NEGATIVE CONTROLS: no identifier in a slot (a digit first, a call,
    /// nothing), or a word of the switch reworded, is missing. MUTANT: the
    /// slot read as its own char (the template matched byte for byte) holds
    /// in neither build.
    #[test]
    fn a_minifiers_name_fills_a_templates_slot() {
        let templates = reset_templates();
        let (_, spellings) = templates
            .iter()
            .find(|(name, _)| *name == "limit names")
            .expect("the limit names' switch");
        let names = &spellings[0];
        assert_eq!(names.matches(JS_NAME).count(), 4, "{names:?}");
        let built = |set: &str, reset: &str| {
            let mut slots = [set, set, set, reset].into_iter();
            let mut out = String::new();
            for (i, piece) in names.split(JS_NAME).enumerate() {
                if i > 0 {
                    out.push_str(slots.next().expect("a name per slot"));
                }
                out.push_str(piece);
            }
            format!("function sr(x){{let s=new Set;for(let _ of x.clears)switch(_){{{out}}}}}")
        };
        let v308 = built("s", "a");
        let v309 = built("s", "i");
        assert!(v308.contains("s.add(a.givesBackWeekly?\"Fable\":\"weekly\")"));
        assert!(v309.contains("s.add(i.givesBackWeekly?\"Fable\":\"weekly\")"));
        let one = [("limit names", vec![names.clone()])];
        for ok in [v308.as_str(), v309.as_str(), &built("$e", "_Ab9")] {
            assert!(missing_templates(ok.as_bytes(), &one).is_empty(), "{ok}");
            assert!(!contains(ok.as_bytes(), names), "the mutant holds in none");
        }
        for bad in [
            built("s", "9a"),
            built("s", "f()"),
            built("s", ""),
            built("s.x", "a"),
        ] {
            assert_eq!(
                missing_templates(bad.as_bytes(), &one),
                ["limit names"],
                "{bad}"
            );
        }
        let reworded = v309.replace(
            "\"weekly\");break;case\"seven_day_overage",
            "\"week\");break;case\"seven_day_overage",
        );
        assert_ne!(reworded, v309);
        assert_eq!(
            missing_templates(reworded.as_bytes(), &one),
            ["limit names"]
        );
        assert!(holds(b"a.b", "a"), "a template with no slot is a literal");
        assert!(!holds(b"a.b", "c"));
    }

    /// A TEMPLATE HOLDS WHEREVER ITS HEAD RECURS: the store bundle (2.1.308
    /// and 2.1.309, `grep -F 'case"five_hour":'`) holds the limit names'
    /// switch head ten times, the switch the ninth, the other nine heads
    /// other switches over the same windows. Built that way, with names
    /// the minifier could pick, the switch holds. NEGATIVE CONTROL: the
    /// nine other heads alone hold no switch. MUTANT: a match tried only
    /// where the head first occurs holds in neither build's shape.
    #[test]
    fn a_template_holds_past_its_heads_earlier_places() {
        let templates = reset_templates();
        let (_, spellings) = templates
            .iter()
            .find(|(name, _)| *name == "limit names")
            .expect("the limit names' switch");
        let names = &spellings[0];
        let mut slots = ["s", "s", "s", "i"].into_iter();
        let switch = names
            .split(JS_NAME)
            .enumerate()
            .fold(String::new(), |mut out, (i, piece)| {
                if i > 0 {
                    out.push_str(slots.next().expect("a name per slot"));
                }
                out.push_str(piece);
                out
            });
        let decoy = |k: usize| format!("switch(w{k}){{case\"five_hour\":return\"5h\";default:}}");
        let decoys = |range: std::ops::Range<usize>| range.map(decoy).collect::<String>();
        let one = [("limit names", vec![names.clone()])];
        let bundle = format!("{}{switch}{}", decoys(0..8), decoys(8..9));
        assert_eq!(bundle.matches("case\"five_hour\":").count(), 10);
        assert!(
            bundle.find(&switch) > bundle.find("case\"five_hour\":"),
            "the switch is not the head's first place"
        );
        assert!(missing_templates(bundle.as_bytes(), &one).is_empty());
        assert_eq!(
            missing_templates(decoys(0..9).as_bytes(), &one),
            ["limit names"]
        );
    }

    /// NO SLOT TOUCHES AN IDENTIFIER'S CHAR: JavaScript reads the longest
    /// name, so a slot glued to a word could never be filled; such a
    /// template is refused when it is matched, never silently missing.
    /// MUTANT: the guard removed lets `x=` + slot + `abc;` through.
    #[test]
    #[should_panic(expected = "a slot touches an identifier's char")]
    fn a_slot_glued_to_a_word_is_refused() {
        holds(b"x=fooabc;", &format!("x={JS_NAME}abc;"));
    }

    /// The guard's other side: a word glued before a slot.
    #[test]
    #[should_panic(expected = "a slot touches an identifier's char")]
    fn a_word_glued_before_a_slot_is_refused() {
        holds(b"let sa=1", &format!("let s{JS_NAME}=1"));
    }

    /// The bundle finder on a stand-in package: the version its
    /// `package.json` names and every `.js` file of the script's directory,
    /// found through a link.
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

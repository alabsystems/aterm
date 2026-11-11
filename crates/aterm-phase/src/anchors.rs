// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vendor literal anchor tables and transcript/rollout key registries.

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
    /// The first vendor release whose build no longer carries it (read on that build's binary),
    /// or `None` while every release since `since` does.
    pub gone_in: Option<&'static str>,
}

impl Anchor {
    /// Whether a build of `version` draws this anchor: one of `since` or later, and older than
    /// `gone_in` where it has one. A build of no known version is taken for the newest, which
    /// draws no anchor a release dropped.
    #[must_use]
    pub fn drawn_by(&self, version: Option<&str>) -> bool {
        match version {
            Some(v) => {
                !newer_than(self.since, v) && self.gone_in.is_none_or(|gone| newer_than(gone, v))
            }
            None => self.gone_in.is_none(),
        }
    }
}

/// `anchor`, which builds of `release` and later no longer carry ([`Anchor::gone_in`]).
const fn gone(anchor: Anchor, release: &'static str) -> Anchor {
    Anchor {
        gone_in: Some(release),
        ..anchor
    }
}

const fn lit(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::Literal,
        since,
        gone_in: None,
    }
}

const fn composed(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::Composed,
        since,
        gone_in: None,
    }
}

const fn hint(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::KeyHint,
        since,
        gone_in: None,
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
    lit(
        "box.rm_breaker.shell_unchecked",
        "This shell -c script runs rm and could not be checked",
        "2.1.302",
    ),
    lit(
        "box.rm_breaker.shell_value",
        "in a shell -c script, on a target built from a value known only when it runs",
        "2.1.302",
    ),
    lit(
        "box.reason_shell_deny",
        "A permission rule denies a command in this shell -c script",
        "2.1.302",
    ),
    composed(
        "box.auto_deny",
        "Claude Code will automatically deny this request in",
        "2.1.281",
    ),
    // The same countdown's later words (`… in 1:59, to avoid holding up an unattended session`):
    // read in 2.1.314's build, which no longer holds the `deny` words, measured on 2.1.316, and the
    // only countdown words the 2.1.316, 2.1.317 and 2.1.318 builds hold. 2.1.308 was the last build
    // measured with `deny`; no build between was read.
    lit(
        "box.auto_block",
        "Claude Code will automatically block this request in",
        "2.1.314",
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
    lit(
        "automode.title",
        "Teach auto mode about your environment?",
        "2.1.310",
    ),
    lit(
        "automode.body",
        "Auto mode works better when it knows your environment. Takes about a minute.",
        "2.1.310",
    ),
    lit("automode.not_now", "Not now", "2.1.310"),
    lit("automode.never", "Don't show again", "2.1.310"),
    lit(
        "conversation_resume.title",
        "Resume this conversation?",
        "2.1.318",
    ),
    lit("conversation_resume.resume", "Resume", "2.1.318"),
    lit(
        "conversation_resume.new",
        "Start a new conversation",
        "2.1.318",
    ),
    // The `Claude in Chrome` intro ([`crate::chrome_intro`]), as 2.1.317 drew it on 2026-11-02
    // and as its bundle's `ClaudeInChromeOnboarding` composes it: the title; the first paragraph;
    // while the bundle's extension check has not found the extension, the `Requires…` words and
    // the store link after one space; the permissions words, closed by `.`, or once the
    // extension is found by one space, the permissions link in parentheses and `.`; and the
    // last paragraph's words, the `/chrome` command, `or visit` and the docs link, one space
    // between each. Each link and the command is a literal of its own in the bundle, which
    // composes the paragraphs from them. The title's id does not end `.title`: the generic
    // title joiner ([`crate::prompt`]'s `is_title`) would stop a wrapped `Claude in Chrome wants
    // to …` box title at these three words.
    lit("chrome.intro_heading", "Claude in Chrome", "2.1.317"),
    lit(
        "chrome.intro_body",
        "Claude in Chrome works with the Chrome extension to let you control your browser \
         directly from Claude Code. You can navigate websites, fill forms, capture screenshots, \
         record GIFs, and debug with console logs and network requests.",
        "2.1.317",
    ),
    lit(
        "chrome.intro_requires",
        "Requires the Chrome extension. Get started at",
        "2.1.317",
    ),
    lit(
        "chrome.intro_store_url",
        "https://claude.ai/chrome",
        "2.1.317",
    ),
    lit(
        "chrome.intro_permissions",
        "Site-level permissions are inherited from the Chrome extension. Manage permissions in \
         the Chrome extension settings to control which sites Claude can browse, click, and type \
         on",
        "2.1.317",
    ),
    lit(
        "chrome.intro_permissions_url",
        "https://clau.de/chrome/permissions",
        "2.1.317",
    ),
    lit("chrome.intro_more", "For more info, use", "2.1.317"),
    lit("chrome.intro_command", "/chrome", "2.1.317"),
    lit("chrome.intro_visit", "or visit", "2.1.317"),
    lit(
        "chrome.intro_docs_url",
        "https://code.claude.com/docs/en/chrome",
        "2.1.317",
    ),
    lit("busy.interrupt", "esc to interrupt", "2.1.267"),
    // The window title's lead glyph (`crate::phase::title_runs`): one of two frames while the
    // REPL's status is busy (a turn loading, or a background agent, teammate or workflow
    // running), flipped every 960 ms while the terminal has focus and held on one without it,
    // the rest glyph otherwise, a blank after each. The bundle spells each as a `\u` escape
    // (2.1.311: `["\u25D0","\u25D1"]` and `"\u2733"`), so neither is a literal of its bytes.
    // Claude Code writes no title with its title off, leads it with no glyph where its status
    // goes to the terminal's tab, and holds the rest glyph under a terminal multiplexer.
    composed("title.running", "◐◑", "2.1.304"),
    composed("title.rest", "✳", "2.1.304"),
    lit("footer.shortcuts", "? for shortcuts", "2.1.283"),
    hint("footer.clear", "esc to clear", "2.1.283"),
    // The prompt-history search's label (Ctrl-R) on a footer row, its query after it; in a narrow
    // window wrapped in its own column beside the query (`crate::phase::history_search`).
    lit("history.search", "search prompts:", "2.1.318"),
    lit("history.no_match", "no matching prompt:", "2.1.318"),
    composed("mode.bypass", "bypass permissions on", "2.1.283"),
    composed("mode.auto", "auto mode on", "2.1.283"),
    composed("mode.accept_edits", "accept edits on", "2.1.283"),
    composed("mode.dont_ask", "don't ask on", "2.1.283"),
    composed("mode.plan", "plan mode on", "2.1.283"),
    composed("mode.manual", "manual mode on", "2.1.283"),
    hint("mode.cycle", "to cycle", "2.1.283"),
    // The mode row's items after the pill, which the glass keeps or drops
    // (`aterm_agent::harness::footer::mode_row_rewrite_under`).
    hint("mode.manage", "\u{2193} to manage", "2.1.283"),
    hint("mode.for_agents", "\u{2190} for agents", "2.1.283"),
    composed("mode.tasks", "/tasks to see subagents", "2.1.304"),
    composed("mode.remote", "/rc active", "2.1.300"),
    composed("mode.auto_compact", "% until auto-compact", "2.1.267"),
    composed("jobs.shell", "shell", "2.1.271"),
    composed("jobs.shells", "shells", "2.1.271"),
    composed("jobs.monitor", "monitor", "2.1.283"),
    composed("jobs.monitors", "monitors", "2.1.283"),
    composed("jobs.agent", "agent", "2.1.310"),
    composed("jobs.agents", "agents", "2.1.310"),
    composed("jobs.done", "done", "2.1.310"),
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
    lit("turn.task_status", "Task \"", "2.1.312"),
    lit(
        "turn.task_running",
        "still running in background",
        "2.1.312",
    ),
    lit("turn.task_completed", "completed in background", "2.1.312"),
    lit("turn.task_stopped", "stopped", "2.1.312"),
    lit("turn.recap", "recap:", "2.1.301"),
    hint("turn.group.expand", "(ctrl+o to expand)", "2.1.284"),
    composed("turn.group.reading", "Reading", "2.1.284"),
    composed("turn.group.read", "Read", "2.1.280"),
    composed("turn.group.searched", "Searched for", "2.1.295"),
    composed("turn.group.searching", "Searching for", "2.1.308"),
    composed("turn.group.listed", "Listed", "2.1.283"),
    composed("turn.group.listing", "Listing", "2.1.308"),
    composed("turn.group.running", "Running", "2.1.283"),
    // Read in 2.1.268's group renderer: the 2026-09-12 captures that draw it carry no version.
    composed("turn.group.ran", "Ran", "2.1.268"),
    composed("turn.group.called", "Called", "2.1.294"),
    // Drawn hint-less in wait_bg17.out (`  Pushed to main, rebased onto origin/main`, no version) and read in 2.1.268's group renderer.
    composed("turn.group.pushed", "Pushed to", "2.1.268"),
    composed("turn.group.rebased", "Rebased onto", "2.1.268"),
    // The rest of 2.1.268's group renderer, read in its binary (each clause capitalised where it opens the row): no capture draws them.
    composed("turn.group.thought", "Thought for", "2.1.268"),
    composed("turn.group.thinking", "Thinking for", "2.1.268"),
    composed("turn.group.edited", "Edited", "2.1.268"),
    composed("turn.group.editing", "Editing", "2.1.268"),
    composed("turn.group.made", "Made", "2.1.268"),
    composed("turn.group.making", "Making", "2.1.268"),
    composed("turn.group.committed", "Committed", "2.1.268"),
    composed("turn.group.amended", "Amended commit", "2.1.268"),
    composed("turn.group.cherry_picked", "Cherry-picked", "2.1.268"),
    composed("turn.group.merged", "Merged", "2.1.268"),
    composed("turn.group.pr", "PR #", "2.1.268"),
    composed("turn.group.mr", "MR !", "2.1.268"),
    // The renderer's pull and merge request verbs besides `edited` and `merged`.
    composed("turn.group.created", "Created", "2.1.268"),
    composed("turn.group.commented_on", "Commented on", "2.1.268"),
    composed("turn.group.closed", "Closed", "2.1.268"),
    composed("turn.group.reopened", "Reopened", "2.1.268"),
    composed("turn.group.marked_ready", "Marked ready", "2.1.268"),
    composed("turn.group.marked_draft", "Marked draft", "2.1.268"),
    composed(
        "turn.group.enabled_auto_merge",
        "Enabled auto-merge on",
        "2.1.268",
    ),
    composed(
        "turn.group.disabled_auto_merge",
        "Disabled auto-merge on",
        "2.1.268",
    ),
    composed("turn.group.published", "Published", "2.1.268"),
    composed("turn.group.publishing", "Publishing", "2.1.268"),
    composed("turn.group.repld", "REPL'd", "2.1.268"),
    composed("turn.group.repling", "REPL'ing", "2.1.268"),
    composed("turn.group.calling", "Calling", "2.1.268"),
    composed("turn.group.agent_named", "agent · ", "2.1.268"),
    // How 2.1.268's group renderer draws a lone agent's description (its UWt, the same bytes in 2.1.310): its first word's
    // irregular past and -ing forms by base, the participles no description opens with, and the prefixes a hyphen joins.
    lit(
        "turn.agent.past",
        r#"begin:"began",bind:"bound",bring:"brought",build:"built",buy:"bought",catch:"caught",choose:"chose",come:"came",cut:"cut",debug:"debugged",dig:"dug",do:"did",draw:"drew",feed:"fed",feel:"felt",fight:"fought",find:"found",fly:"flew",forget:"forgot",freeze:"froze",get:"got",give:"gave",go:"went",have:"had",hide:"hid",hit:"hit",hold:"held",input:"input",keep:"kept",know:"knew",lead:"led",leave:"left",let:"let",lose:"lost",make:"made",mean:"meant",meet:"met",override:"overrode",overwrite:"overwrote",pay:"paid",put:"put",quit:"quit",read:"read",rebuild:"rebuilt",redo:"redid",rerun:"reran",reset:"reset",rewrite:"rewrote",run:"ran",see:"saw",seek:"sought",send:"sent",set:"set",show:"showed",shut:"shut",sit:"sat",sleep:"slept",spend:"spent",spin:"spun",split:"split",spread:"spread",stand:"stood",sweep:"swept",sync:"synced",take:"took",teach:"taught",tear:"tore",tell:"told",think:"thought",throw:"threw",understand:"understood",undo:"undid",unset:"unset",win:"won",unwrap:"unwrapped",unzip:"unzipped",write:"wrote""#,
        "2.1.268",
    ),
    lit(
        "turn.agent.running",
        r#"begin:"beginning",commit:"committing",control:"controlling",debug:"debugging",emit:"emitting",equip:"equipping",forget:"forgetting",format:"formatting",input:"inputting",occur:"occurring",omit:"omitting",output:"outputting",permit:"permitting",prefer:"preferring",quit:"quitting",refer:"referring",rerun:"rerunning",reset:"resetting",screenshot:"screenshotting",snapshot:"snapshotting",submit:"submitting",sync:"syncing",transfer:"transferring",unset:"unsetting",unwrap:"unwrapping",unzip:"unzipping""#,
        "2.1.268",
    ),
    lit(
        "turn.agent.participles",
        r#""been","broken","chosen","done","drawn","driven","eaten","fallen","forgotten","given","gone","grown","hidden","known","ridden","risen","seen","shown","spoken","taken","thrown","torn","worn","written""#,
        "2.1.268",
    ),
    lit(
        "turn.agent.prefixes",
        "re|un|de|pre|co|sub|over|out|mis|auto|post",
        "2.1.268",
    ),
    composed("turn.group.recalled", "Recalled", "2.1.268"),
    composed("turn.group.recalling", "Recalling", "2.1.268"),
    composed("turn.group.searched_bare", "Searched", "2.1.268"),
    composed("turn.group.searching_bare", "Searching", "2.1.268"),
    composed("turn.group.wrote", "Wrote", "2.1.268"),
    composed("turn.group.writing", "Writing", "2.1.268"),
    // A hint-less group's nouns, each singular as measured; the plural is the renderer's count agreement (`shell commands` measured at 2.1.283).
    composed("turn.group.file", "file", "2.1.280"),
    composed("turn.group.pattern", "pattern", "2.1.295"),
    composed("turn.group.directory", "directory", "2.1.283"),
    composed("turn.group.shell_command", "shell command", "2.1.283"),
    // Read in 2.1.268's group renderer, singular and plural.
    composed("turn.group.agent", "agent", "2.1.268"),
    composed("turn.group.tool", "tool", "2.1.268"),
    composed("turn.group.memory", "memory", "2.1.268"),
    composed("turn.group.team_memory", "team memory", "2.1.268"),
    // TaskStop's userFacingName, read in 2.1.268's binary (its renderToolUseMessage empty, so no `(…)`): the captures that draw it carry no version.
    lit("turn.call.stop_task", "Stop Task", "2.1.268"),
    // Edit's and Write's userFacingName for a file in the plans directory, read in 2.1.268's binary; measured over `⎿  /plan to preview` at 2.1.308.
    lit("turn.call.updated_plan", "Updated plan", "2.1.268"),
    // A background task's notice of its end, its description quoted between head and end: Bash's measured in
    // claude-2.1.304-verbose-background-command-login-words.txt, a dynamic workflow's in
    // claude-2.1.283-wall-session-goal-checkin-shell-measured.txt, an agent's in claude-2.1.292-box-bash-rm-glob-subagent.txt
    // and its wait in claude-2.1.304-agents-panel-long-bash-4m57s-120col.txt; the rest read in 2.1.268's notice builders.
    composed("turn.task.command", "Background command \"", "2.1.304"),
    composed("turn.task.workflow", "Dynamic workflow \"", "2.1.283"),
    composed("turn.task.agent", "Agent \"", "2.1.292"),
    composed("turn.task.monitor", "Monitor \"", "2.1.268"),
    composed("turn.task.remote", "Remote task \"", "2.1.268"),
    composed("turn.task.completed", "completed", "2.1.283"),
    composed("turn.task.exit_code", " (exit code ", "2.1.304"),
    composed("turn.task.finished", "finished", "2.1.292"),
    composed(
        "turn.task.waiting",
        "is waiting on its own background work",
        "2.1.304",
    ),
    composed("turn.task.failed", "failed", "2.1.268"),
    composed("turn.task.with_exit_code", " with exit code ", "2.1.268"),
    composed("turn.task.failed_with", "failed: ", "2.1.268"),
    composed("turn.task.was_stopped", "was stopped", "2.1.268"),
    composed(
        "turn.task.memory_pressure",
        " because the system is running low on memory",
        "2.1.268",
    ),
    composed("turn.task.by_claude", " by Claude", "2.1.268"),
    composed("turn.task.by_user", " by user", "2.1.268"),
    composed("turn.task.turn_limit", "stopped at its ", "2.1.268"),
    composed(
        "turn.task.quiet",
        "ended without producing output",
        "2.1.268",
    ),
    composed("turn.task.exit", " (exit ", "2.1.268"),
    composed("turn.task.stream_ended", "stream ended", "2.1.268"),
    composed("turn.task.script_failed", "script failed", "2.1.268"),
    composed("turn.task.stopped", "stopped", "2.1.268"),
    composed("turn.task.succeeded", "completed successfully", "2.1.268"),
    composed("turn.task.blocked", "is blocked", "2.1.268"),
    composed(
        "turn.task.delivered",
        "delivered to Claude as a ",
        "2.1.268",
    ),
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
    composed("notice.more", " more notice", "2.1.280"),
    composed("notice.hidden", " hidden", "2.1.280"),
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
    composed("effort.auto", "Effort level set to auto", "2.1.283"),
    lit("effort.auto_head", "Effort level set to ", "2.1.283"),
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
    lit("reset.weekly_row", "Reset your 5-hour limit now", "2.1.312"),
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
    lit("reset.done.session", "5-hour limit reset", "2.1.312"),
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
    // The `/limit-reset` command's name (`var Oj="limit-reset"` in 2.1.318, `PZ` in 2.1.303); the
    // screen echoes it typed as `❯ /limit-reset`, its answer on the `⎿` row under it.
    lit("reset.command", "limit-reset", "2.1.303"),
    lit("wall.fable", "You've reached your Fable limit", "2.1.267"),
    // The per-model wall as 2.1.301 builds it (`You've hit your ${Rye[rateLimitType]}${resets}`,
    // `seven_day_overage_included:"Fable limit"`, `seven_day_sonnet:"Sonnet limit"` in 2.1.319).
    composed("wall.fable_hit", "You've hit your Fable limit", "2.1.301"),
    composed("wall.sonnet_hit", "You've hit your Sonnet limit", "2.1.300"),
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
        "wall.goal_api_unavailable",
        "the API was unavailable or the connection dropped",
        "2.1.283",
    ),
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
    // The Fable week's name in the warning (`You've used ${n}% of your ${Rye[rateLimitType]}`).
    lit("usage.fable_limit", "Fable limit", "2.1.301"),
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
    lit("agy.turn.thought", "Thought for ", "1.4.0"),
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
    lit(
        "agy.wall.network",
        "There was a network issue connecting to the server",
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
    composed("agy.box.persisted", "(Persist to ", "1.4.0"),
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
    composed("muse.tool.expand", " · ctrl+o", "1.4.10-R6427.1"),
    lit("muse.tool.monitor", "Monitor ", "1.4.10-R6427.1"),
    lit("muse.tool.stopped", "Stopped terminal", "1.4.10-R6427.1"),
    lit("muse.tool.scheduled", "Scheduled", "1.4.10-R6427.1"),
    lit("muse.tool.unscheduled", "Unscheduled", "1.4.10-R6427.1"),
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
        "muse.wall.saved_login",
        "your saved login is no longer valid. Log in again or use a different account.",
        "1.4.13-R6996.1",
    ),
    lit(
        "muse.wall.still_unauthorized",
        "still unauthorized after a token refresh; run `muse login` again",
        "1.4.13-R6996.1",
    ),
    lit(
        "muse.wall.not_logged_in_cli",
        "not logged in: run `muse login` first",
        "1.4.13-R6996.1",
    ),
    lit(
        "muse.wall.not_logged_in_key",
        "not logged in: run /login to add an API key",
        "1.4.13-R6996.1",
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
    lit(
        "muse.wall.usage.resets",
        "Your usage resets at",
        "1.4.11-R6667.1",
    ),
    lit("muse.wall.model_failed", "model failed: ", "1.4.10-R6427.1"),
    // Its own tries spent: `model failed after 10 attempts: transport error [body-truncated]: …`,
    // measured on the lab stub's dropped host (v6 §14.17.5).
    composed(
        "muse.wall.model_failed_after",
        "model failed after ",
        "1.4.13-R6996.1",
    ),
    // What follows the count there (`1 attempt: …` or `N attempts: …`).
    composed(
        "muse.wall.model_failed_attempt",
        "attempt: ",
        "1.4.13-R6996.1",
    ),
    composed(
        "muse.wall.model_failed_attempts",
        "attempts: ",
        "1.4.13-R6996.1",
    ),
    // The cause it names where the host dropped the request: the connection's, never the API's
    // answer.
    composed(
        "muse.wall.transport_error",
        "transport error",
        "1.4.13-R6996.1",
    ),
    lit("muse.wall.api_error", "API error ", "1.4.10-R6427.1"),
    // The error body's `type` Muse Code draws in parentheses, as the lab stub's Codex-style body sends it: unverified against Meta's API.
    composed(
        "muse.wall.usage_limit_reached",
        "(usage_limit_reached)",
        "1.4.10-R6427.1",
    ),
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
    // What follows the scope on the prompt-history search's row (Ctrl-R), its query after it
    // (`crate::muse::history_search`).
    lit("muse.history.search", "search prompts:", "1.4.14-R7309.1"),
    lit(
        "muse.history.no_match",
        "no matching prompt:",
        "1.4.14-R7309.1",
    ),
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

/// Every string aterm guards on in Grok Build's screen ([`crate::grok`]): the sign-in screen as
/// 1.0.70 drew it, the limit and login texts as 1.0.67's and 1.0.70's binaries carry them, and
/// the team's API rate limit, the `API error (status ` its sampler's limit texts sit after, the
/// usage and credit upsells' titles and `Try Again` row, and the texts a sweep of 1.0.70's
/// strings found for a login of its own that is missing, expired, rejected or could not be
/// renewed (`grok.auth.*`, whose texts aterm-agent's Grok Build login-ended words are read from),
/// as 1.0.70's carries them. The failure a 401 draws is composed on the screen (`Authentication
/// required: …`, measured on 1.0.70), where the binary's own sentence says `grok login`. The
/// running turn's `[stop]` at its status row's end, the busy key row's `Ctrl+c:cancel` (composed
/// from the key bindings) and the plan approval's `plan approval` in the composer's bottom edge
/// are read as 1.0.70's busy and plan frames draw them ([`crate::grok::phase`]), and so are the
/// boxes that wait on the person: the plan approval's `a:approve` key, a card's key row in either
/// focus (`Tab:next option`, `Tab:next answer`, `Esc:scrollback`, `Tab/Space:permission`,
/// composed from the key bindings) and the file card's `Allow Edit to ` title
/// ([`crate::grok::prompt`]).
pub const GROK_ANCHORS: &[Anchor] = &[
    lit(
        "grok.signin.approve",
        "Approve in your browser to finish signing in.",
        "1.0.67",
    ),
    lit("grok.signin.waiting", "Waiting for approval...", "1.0.67"),
    composed("grok.keys.quit", "ctrl+q  quit", "1.0.67"),
    lit(
        "grok.wall.plan_rate_limit",
        "You've hit the rate limit for your plan.",
        "1.0.67",
    ),
    lit(
        "grok.wall.plan_rate_limit.sampler",
        "You\u{2019}ve hit the rate limit for your plan.",
        "1.0.67",
    ),
    lit(
        "grok.wall.team_rate_limit",
        "You\u{2019}ve hit your team\u{2019}s API rate limit.",
        "1.0.70",
    ),
    lit("grok.wall.api_error", "API error (status ", "1.0.70"),
    lit(
        "grok.wall.free_usage",
        "You\u{2019}ve reached your free Grok Build usage limit",
        "1.0.67",
    ),
    lit(
        "grok.wall.free_usage.upsell",
        "You hit your free usage limit.",
        "1.0.70",
    ),
    lit("grok.wall.weekly", "You hit your weekly limit.", "1.0.70"),
    lit(
        "grok.wall.credit_limit",
        "You\u{2019}ve hit the credit limit for your plan.",
        "1.0.70",
    ),
    lit(
        "grok.wall.spending_cap",
        "You\u{2019}ve hit your spending cap.",
        "1.0.70",
    ),
    lit("grok.wall.try_again", "Try Again", "1.0.70"),
    lit(
        "grok.wall.try_again.resubmit",
        "Resubmit the last prompt once you have usage again",
        "1.0.70",
    ),
    lit(
        "grok.auth.expired",
        "Authentication required: your session has expired",
        "1.0.67",
    ),
    lit(
        "grok.auth.not_logged_in",
        "Not logged in. Run `grok login`.",
        "1.0.70",
    ),
    lit(
        "grok.auth.token_expired",
        "Token expired. Run `grok login` to re-authenticate.",
        "1.0.70",
    ),
    lit(
        "grok.auth.token_invalid",
        "Your auth token is invalid or expired.",
        "1.0.70",
    ),
    lit(
        "grok.auth.rejected",
        "Authentication rejected by server.",
        "1.0.70",
    ),
    lit(
        "grok.auth.recovery_exhausted",
        "Auth recovery exhausted; re-authentication required.",
        "1.0.70",
    ),
    lit(
        "grok.auth.reauth_required",
        "Session expired, re-authentication required",
        "1.0.70",
    ),
    lit(
        "grok.auth.session_expired",
        "Session expired. Run `grok login` to re-authenticate.",
        "1.0.70",
    ),
    lit(
        "grok.auth.session_has_expired",
        "Your session has expired. Run `grok login` to sign in again.",
        "1.0.70",
    ),
    lit(
        "grok.auth.required",
        "Authentication required. Run `grok login` to re-authenticate.",
        "1.0.70",
    ),
    composed(
        "grok.auth.required.open",
        "Authentication required:",
        "1.0.70",
    ),
    composed(
        "grok.auth.required.tail",
        "Run /login to re-authenticate",
        "1.0.70",
    ),
    lit(
        "grok.auth.not_renewed",
        "Your session expired and your sign-in helper could not renew it in the background.",
        "1.0.70",
    ),
    lit(
        "grok.auth.not_renewed.named",
        "Your session expired and",
        "1.0.70",
    ),
    lit(
        "grok.auth.not_renewed.tail",
        "could not renew it in the background.",
        "1.0.70",
    ),
    lit(
        "grok.auth.refresh_failed",
        "Authentication could not be refreshed.",
        "1.0.70",
    ),
    lit(
        "grok.auth.team_rejected",
        "Your team sign-in was rejected.",
        "1.0.70",
    ),
    lit(
        "grok.auth.not_signed_in",
        "Not signed in. Run `grok login` to authenticate",
        "1.0.70",
    ),
    lit(
        "grok.auth.not_signed_in.no_browser",
        "Not signed in. To authenticate without a browser, run:",
        "1.0.70",
    ),
    lit(
        "grok.auth.not_signed_in.bare",
        "not signed in \u{2014} run `grok login`",
        "1.0.70",
    ),
    lit(
        "grok.auth.not_authenticated",
        "You are not authenticated.",
        "1.0.70",
    ),
    lit(
        "grok.auth.no_cached_token",
        "No cached auth token found",
        "1.0.70",
    ),
    lit(
        "grok.auth.failed",
        "Authentication failed. Run `grok login`, set XAI_API_KEY, or add api_key to ~/.grok/config.toml.",
        "1.0.70",
    ),
    lit(
        "grok.auth.no_cached_credentials",
        "No cached credentials found. Run `grok login` first.",
        "1.0.70",
    ),
    lit(
        "grok.auth.headless",
        "Headless mode requires a grok.com session.",
        "1.0.70",
    ),
    lit(
        "grok.auth.no_session",
        "preferred_method=oidc but no session is available. Run `grok login` to authenticate.",
        "1.0.70",
    ),
    lit(
        "grok.auth.signed_out",
        "Signed out: /feedback works, and /login signs in.",
        "1.0.70",
    ),
    lit(
        "grok.auth.compaction",
        "Unauthorized (401): compaction failed \u{2014} re-authenticate with /login and retry.",
        "1.0.70",
    ),
    lit(
        "grok.auth.compaction.reason",
        "authentication problem \u{2014} re-authenticate using /login and retry.",
        "1.0.70",
    ),
    lit("grok.busy.stop", "[stop]", "1.0.70"),
    hint("grok.keys.cancel", "Ctrl+c:cancel", "1.0.70"),
    lit("grok.plan.edge", "plan approval", "1.0.70"),
    hint("grok.plan.keys.approve", "a:approve", "1.0.70"),
    hint("grok.card.keys.next_option", "Tab:next option", "1.0.70"),
    hint("grok.card.keys.next_answer", "Tab:next answer", "1.0.70"),
    hint("grok.card.keys.scrollback", "Esc:scrollback", "1.0.70"),
    hint(
        "grok.card.keys.permission",
        "Tab/Space:permission",
        "1.0.70",
    ),
    lit("grok.card.edit", "Allow Edit to ", "1.0.70"),
];

/// Every string aterm guards on in Kimi Code's screen ([`crate::kimi`]), as 2.3.1 drew it; the
/// texts 2.3.3's binary carries for a login it lacks or lost (`kimi.auth.*` beside the screen's
/// `kimi.auth.llm_not_set`; where kimi builds the sentence from a template, the fixed part), whose
/// texts aterm-agent's Kimi Code login-ended words are read from; and its own quota classifier's
/// patterns and codes (`kimi.wall.*`, as their source writes them), which its limit words are
/// (`docs/measured/kimi-review-2026-10-27.md` §8). 2.3.3 classified a 429 by the server's
/// message (`KIMI_QUOTA_EXHAUSTED_MESSAGE_PATTERNS`) and code (`KIMI_QUOTA_EXHAUSTED_ERROR_CODES`);
/// 2.5.0 and 2.5.1 carry no message patterns and classify by code alone
/// (`exceeded_current_quota_error`, and for an OpenAI-type provider `insufficient_quota` too),
/// fail at once with no retry, and show the error as `[provider.api_error] <server's message>`
/// (`formatErrorPayload`): no code is drawn, so only the server's words, which 2.3.3's patterns
/// matched, say a quota on screen. The spinner's `Working…`, the exit hint a first Ctrl-C puts in
/// the footer and the notice a resume draws over the composer are read as 2.4.0 drew them
/// ([`crate::kimi::phase`]), and so are the titles of the boxes it draws in the composer's place
/// (a command's approval, a plan's, a question and the folder-trust prompt, [`crate::kimi::prompt`]).
/// The approval panel's key row ends `↵ confirm` as 2.4.0 drew it, and `· ctrl+e preview` over
/// a file's block as 2.5.2 drew it, and a command its own patterns call dangerous draws a
/// `Dangerous: <label>` row over its lines, in 2.5.1's and 2.5.2's binaries and 2.5.2's frames.
/// Its other titles (a file's write and edits, a task's stop,
/// `Approve <tool>?` for any other tool) and a multi-select question's `showing a-b of n` row are
/// read from 2.5.1's and 2.5.2's binaries (`headerFor`, `renderQuestionTab`); 2.5.2's frames
/// measured the write's and edits' titles, and none the others.
pub const KIMI_ANCHORS: &[Anchor] = &[
    lit("kimi.welcome", "Welcome to Kimi Code!", "2.3.1"),
    lit("kimi.footer.context", "context: ", "2.3.1"),
    lit(
        "kimi.footer.exit_confirm",
        "Press Ctrl+C again to exit",
        "2.4.0",
    ),
    lit("kimi.busy.working", "Working\u{2026}", "2.4.0"),
    lit("kimi.resumed", "Resumed session (", "2.4.0"),
    lit("kimi.turn.interrupted", "Interrupted by user", "2.4.0"),
    lit("kimi.box.run", "Run this command?", "2.4.0"),
    lit("kimi.box.plan", "Ready to build with this plan?", "2.4.0"),
    lit("kimi.box.question", "question", "2.4.0"),
    lit("kimi.trust", "Trust this folder?", "2.4.0"),
    lit("kimi.box.write", "Write this file?", "2.5.1"),
    lit("kimi.box.edit", "Apply these edits?", "2.5.1"),
    lit("kimi.box.stop", "Stop this task?", "2.5.1"),
    lit("kimi.box.approve", "Approve ", "2.5.1"),
    lit("kimi.question.showing", "showing ", "2.5.1"),
    lit("kimi.box.keys.confirm", "\u{21b5} confirm", "2.4.0"),
    lit("kimi.box.keys.preview", "\u{b7} ctrl+e preview", "2.5.1"),
    lit("kimi.box.dangerous", "Dangerous: ", "2.5.1"),
    lit("kimi.error", "Error: ", "2.3.1"),
    lit("kimi.auth.llm_not_set", "LLM not set", "2.3.1"),
    lit(
        "kimi.auth.no_model",
        "No model configured. Run `kimi` and use /login to sign in, then retry; or set \
         default_model in config.toml.",
        "2.3.3",
    ),
    lit(
        "kimi.auth.no_credential",
        " has no credential configured",
        "2.3.3",
    ),
    lit(
        "kimi.auth.no_token",
        "\". Run /login to authenticate.",
        "2.3.3",
    ),
    lit(
        "kimi.auth.rejected",
        "\" was rejected; re-login required.",
        "2.3.3",
    ),
    lit(
        "kimi.auth.no_refresh_token",
        "\" has no refresh_token; re-login required.",
        "2.3.3",
    ),
    lit(
        "kimi.auth.oauth",
        "Run /login to authenticate with the OAuth provider.",
        "2.3.3",
    ),
    lit(
        "kimi.auth.relogin_notice",
        "notice: run /login for: ",
        "2.3.3",
    ),
    lit(
        "kimi.auth.no_models",
        "Run /login to sign in to Kimi, or /provider to add another provider from a model \
         catalog.",
        "2.3.3",
    ),
    lit(
        "kimi.auth.logged_out",
        "Run /login or /provider to get started.",
        "2.3.3",
    ),
    lit(
        "kimi.auth.model_not_set",
        "not set, run /login or /provider",
        "2.3.3",
    ),
    gone(
        lit(
            "kimi.wall.quota",
            "exceeded your current (?:token )?quota",
            "2.3.3",
        ),
        "2.5.0",
    ),
    gone(
        lit("kimi.wall.balance", "check your account balance", "2.3.3"),
        "2.5.0",
    ),
    gone(
        lit("kimi.wall.insufficient", "insufficient balance", "2.3.3"),
        "2.5.0",
    ),
    gone(
        lit(
            "kimi.wall.recharge",
            "recharge your account|please recharge",
            "2.3.3",
        ),
        "2.5.0",
    ),
    gone(
        lit("kimi.wall.arrears", "account (?:is )?in arrears", "2.3.3"),
        "2.5.0",
    ),
    lit(
        "kimi.wall.quota_code",
        "exceeded_current_quota_error",
        "2.3.3",
    ),
];

/// The words of Kimi Code's session record that `aterm-agent`'s wire reader (`kimi_wire`) reads,
/// each through [`anchor!`](macro@crate::anchor): the event types and approval fields of an
/// agent's `wire.jsonl`, the tool name the read-only rule passes, and the names of the session
/// layout that leads to it (`docs/DESIGN-kimi-wire-verdict-2026-11-07.md` §2, §4). A build that
/// renames one never lends a false `read-only`: a renamed event type, approval field or layout
/// name leaves the reader finding nothing, which withholds the verdict; a renamed `display`,
/// `command` or `Bash` leaves a request with no Bash command, which fails the box; a renamed
/// `interaction.resolved` leaves answered requests open, each still held to the rule.
/// `vendor_drift`'s `every_wire_key_is_in_the_store_kimi` finds each word in the installed
/// bundle, and each event type as a quoted string, so it names a build that drops a distinctive
/// word (an event type, `toolName`, `projects.json`, `state.json`, `wire.jsonl`). A short word
/// (`id`, `type`, `kind`, `time`, …) is in any bundle for other reasons, so the row cannot see the
/// record rename it; the reader's withheld or failed verdict is then the only sign.
pub const KIMI_WIRE_KEYS: &[Anchor] = &[
    lit(
        "kimi.wire.interaction.request",
        "interaction.request",
        "2.5.2",
    ),
    lit(
        "kimi.wire.interaction.resolved",
        "interaction.resolved",
        "2.5.2",
    ),
    lit("kimi.wire.type", "type", "2.5.2"),
    lit("kimi.wire.kind", "kind", "2.5.2"),
    lit("kimi.wire.approval", "approval", "2.5.2"),
    lit("kimi.wire.id", "id", "2.5.2"),
    lit("kimi.wire.time", "time", "2.5.2"),
    lit("kimi.wire.request", "request", "2.5.2"),
    lit("kimi.wire.toolName", "toolName", "2.5.2"),
    lit("kimi.wire.display", "display", "2.5.2"),
    lit("kimi.wire.command", "command", "2.5.2"),
    lit("kimi.wire.Bash", "Bash", "2.5.2"),
    lit("kimi.wire.projects.json", "projects.json", "2.5.2"),
    lit("kimi.wire.sessions", "sessions", "2.5.2"),
    lit("kimi.wire.agents", "agents", "2.5.2"),
    lit("kimi.wire.state.json", "state.json", "2.5.2"),
    lit("kimi.wire.cwd", "cwd", "2.5.2"),
    lit("kimi.wire.wire.jsonl", "wire.jsonl", "2.5.2"),
];

/// The keys and closed words of Claude Code's transcript rows the session log's step fold reads:
/// `lab claude-measure` refuses GONE when a build drops one the previous measured build showed,
/// and NEW-KEY when a build shows one no measured build showed. aterm-agent's step tests hold the
/// table to the fold: it is exactly the keys the fold's projections keep and the closed words its
/// arms compare (`the_fold_reads_what_the_transcript_keys_name`).
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
    lit("transcript.version", "version", "2.1.303"),
    lit("transcript.timestamp", "timestamp", "2.1.303"),
    lit("transcript.type", "type", "2.1.303"),
    lit("transcript.subtype", "subtype", "2.1.303"),
    lit("transcript.message", "message", "2.1.303"),
    lit("transcript.model", "model", "2.1.303"),
    lit("transcript.usage", "usage", "2.1.303"),
    lit("transcript.input_tokens", "input_tokens", "2.1.303"),
    lit("transcript.output_tokens", "output_tokens", "2.1.303"),
    lit(
        "transcript.cache_creation_input_tokens",
        "cache_creation_input_tokens",
        "2.1.303",
    ),
    lit("transcript.cache_creation", "cache_creation", "2.1.303"),
    lit(
        "transcript.ephemeral_5m_input_tokens",
        "ephemeral_5m_input_tokens",
        "2.1.303",
    ),
    lit(
        "transcript.ephemeral_1h_input_tokens",
        "ephemeral_1h_input_tokens",
        "2.1.303",
    ),
    lit(
        "transcript.cache_read_input_tokens",
        "cache_read_input_tokens",
        "2.1.303",
    ),
    lit("transcript.id", "id", "2.1.303"),
    lit("transcript.effort", "effort", "2.1.303"),
    lit("transcript.content", "content", "2.1.303"),
    lit("transcript.text", "text", "2.1.303"),
    lit("transcript.name", "name", "2.1.303"),
    lit("transcript.origin", "origin", "2.1.303"),
    lit("transcript.kind", "kind", "2.1.303"),
    lit("transcript.uuid", "uuid", "2.1.303"),
    lit("transcript.turnOrigin", "turnOrigin", "2.1.303"),
    lit("transcript.scheduledTaskId", "scheduledTaskId", "2.1.312"),
    lit("transcript.user", "user", "2.1.294"),
    lit("transcript.assistant", "assistant", "2.1.294"),
    lit("transcript.system", "system", "2.1.294"),
    lit("transcript.error", "error", "2.1.303"),
    lit("transcript.apiError", "apiError", "2.1.303"),
    lit("transcript.apiErrorStatus", "apiErrorStatus", "2.1.303"),
    lit("transcript.quotaLimits", "quotaLimits", "2.1.303"),
    lit("transcript.rateLimitType", "rateLimitType", "2.1.303"),
    lit("transcript.human", "human", "2.1.303"),
    lit(
        "transcript.task-notification",
        "task-notification",
        "2.1.303",
    ),
    lit(
        "transcript.auto-continuation",
        "auto-continuation",
        "2.1.303",
    ),
];

/// The keys and closed words of Codex's rollout rows the session log's Codex step fold reads:
/// `lab codex-measure` refuses GONE when a build drops one the previous measured build showed.
/// The closed word a gated read reads (`codex_steps::GATED_WORDS`: `alab_autopilot`) is not
/// here: a build that drops it reads it off through its own reductions (`CODEX_SHOWN`), and the
/// measurement notes it.
pub const ROLLOUT_KEYS: &[Anchor] = &[
    lit("rollout.timestamp", "timestamp", "0.163.0"),
    lit("rollout.type", "type", "0.163.0"),
    lit("rollout.payload", "payload", "0.163.0"),
    lit("rollout.info", "info", "0.163.0"),
    lit("rollout.input_tokens", "input_tokens", "0.163.0"),
    lit("rollout.output_tokens", "output_tokens", "0.163.0"),
    lit("rollout.total_tokens", "total_tokens", "0.163.0"),
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
    lit("rollout.item_completed", "item_completed", "0.163.0"),
    lit("rollout.item", "item", "0.163.0"),
    lit("rollout.exit_code", "exit_code", "0.163.0"),
    lit("rollout.task_started", "task_started", "0.163.0"),
    lit("rollout.turn_attribution", "turn_attribution", "0.163.0"),
    lit("rollout.turn_trigger", "turn_trigger", "0.163.0"),
];

/// Whether version `a` is newer than `b`, numerically part by part.
#[must_use]
pub fn newer_than(a: &str, b: &str) -> bool {
    let parts = |v: &str| {
        v.replace("-R", ".")
            .split('.')
            .map(|p| {
                if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
                    p.parse::<u64>().unwrap_or(0)
                } else {
                    0
                }
            })
            .collect::<Vec<_>>()
    };
    parts(a) > parts(b)
}

/// The name stem a Codex build's committed rollout reductions carry in `aterm-agent`'s step testdata.
#[must_use]
pub fn rollout_reduction_stem(build: &str) -> String {
    match build.split_once('+') {
        Some((release, meta)) if release.contains("-dev.") && meta.starts_with('g') => {
            format!("codex-alab-{release}")
        }
        _ => format!("codex-{build}"),
    }
}

/// The text of the anchor `id`, for an id known only at run time; a literal id is
/// [`anchor!`](macro@crate::anchor), which the build resolves and checks.
#[must_use]
pub fn anchor(id: &str) -> &'static str {
    match ANCHORS
        .iter()
        .chain(AGY_ANCHORS)
        .chain(MUSE_ANCHORS)
        .chain(GROK_ANCHORS)
        .chain(KIMI_ANCHORS)
        .chain(KIMI_WIRE_KEYS)
        .chain(TRANSCRIPT_KEYS)
        .chain(ROLLOUT_KEYS)
        .find(|a| a.id == id)
    {
        Some(a) => a.text,
        None => panic!("aterm-phase has no anchor {id:?}"),
    }
}

/// The text of the anchor with the literal id `$id`, evaluated by the compiler: an id no
/// table names fails the build, and the reader pays nothing to look it up.
#[macro_export]
macro_rules! anchor {
    ($id:literal) => {{
        const TEXT: &str = $crate::anchor_text($id);
        TEXT
    }};
}

/// [`anchor`](fn@anchor) at compile time: the walk [`anchor!`](macro@crate::anchor) evaluates.
/// Called at run time it walks every table, so a run-time id goes through [`anchor`](fn@anchor).
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
    while i < GROK_ANCHORS.len() {
        if same(GROK_ANCHORS[i].id, id) {
            return GROK_ANCHORS[i].text;
        }
        i += 1;
    }
    let mut i = 0;
    while i < KIMI_ANCHORS.len() {
        if same(KIMI_ANCHORS[i].id, id) {
            return KIMI_ANCHORS[i].text;
        }
        i += 1;
    }
    let mut i = 0;
    while i < KIMI_WIRE_KEYS.len() {
        if same(KIMI_WIRE_KEYS[i].id, id) {
            return KIMI_WIRE_KEYS[i].text;
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
#[path = "anchors_tests.rs"]
mod tests;

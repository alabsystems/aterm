// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! LIVE UPGRADE OF A RUNNING CLAUDE CODE SESSION (owner ask, 2026-09-23: "I want
//! the HARNESS to handle upgrading the live claude session. if that means some
//! nice way of restarting, ok." — and: "we'd want to not kill the background
//! processes. we'd want to wait for them, maybe give a prompt to claude that
//! we're going to prepare a restart to upgrade and prep the AI to get to a good
//! stopping point").
//!
//! A running Claude Code cannot have its binary replaced: the kernel mapped the
//! old image at exec, and Claude's own `/update` relaunch ("Switch to the latest
//! version (conversation continues)", alias `/restart`) is compiled into 2.1.278
//! through 2.1.281 but switched off. So the least disruptive live form is the
//! one this module plans: a COOPERATIVE, IN-PLACE resume relaunch —
//!
//! 1. **Announce** — at an idle point with an empty composer, type ONE turn
//!    telling the agent an upgrade restart is coming. The turn asks it to let
//!    its live background work finish (never cancel it) but stop any wait that
//!    can never end ([`STOPPING_POINT`]), names the shells aterm sees under it
//!    ([`running_clause`]), and asks it to save its work and answer with a
//!    one-time READY marker ([`prepare_prompt`]). Never into a session at a
//!    usage or rate limit ([`Facts::limited`]): what is typed there is queued
//!    unread, and no ask, and no minute of the re-ask clock, is spent on it.
//!    Nor into one at the LOGIN WALL ([`Facts::login`]): what is typed there
//!    is answered by `Login expired · Please run /login` and read by no model;
//!    a notice whose own turn the wall answered all the same (the login went
//!    as it was typed) spent no ask, and is typed again once the login is
//!    back ([`Facts::undelivered`]).
//! 2. **Drain** — wait until the agent has answered with the marker
//!    ([`transcript_has_ready`]), Claude's own session file says `idle`, nothing
//!    runs under the agent (no shell, and no `caffeinate` but Claude Code's own
//!    keep-awake), and the composer is still empty ([`gate_restart`]). aterm
//!    never kills anything to get there: the agent's own work is waited for,
//!    and never ended. The wait is not silent, though. A notice unanswered, or
//!    a READY answer that work under the agent outlives, is asked again every
//!    [`REASK_S`], each time naming what runs. After [`MAX_ASKS`] notices the
//!    upgrade gives up and says what held it ([`next_step`]) — for this round
//!    only: NO STOP IS FOR GOOD (the owner, 2026-09-27: "you should NEVER have
//!    upgrades stalled"), and [`RETRY_S`] after any round stops the upgrade
//!    starts a new one ([`Step::Rearm`]) with new markers and its asks
//!    reset. A PERSON is not
//!    waited for: a box nobody answers or a typed draft
//!    nobody sends ([`person_hold`]), standing for [`HOLD_S`], holds the READY
//!    answer for at most [`DRAIN_S`] after the announcement; past it the answer
//!    is void (the driver forgets its marker and says why in the ledger) and the
//!    upgrade asks again at the next idle point.
//! 3. **Restart** — end the agent with SIGTERM (Claude runs its graceful
//!    shutdown; no keystroke is typed into the TUI, so no draft can be
//!    concatenated), wait for the tab's own shell to hold the terminal again,
//!    and type ONE line ([`relaunch_line`]) that re-runs the NEWER build with the
//!    original flags ([`rewrite_argv`]) and `--resume <sessionId>`, healing a
//!    stale shell's PATH in the same line when the tab needs it — and its shell
//!    integration too, when the window reports it `degraded` ([`with_rekey`]:
//!    the shell takes a fresh key from a one-use file the window wrote) — or
//!    running a script from before loaders, which the same line then upgrades
//!    in place by sourcing the window's own loader (2026-09-26).
//! 4. **Continue** — once the new process has re-registered the SAME session,
//!    type one turn telling the agent it was upgraded, which model it ran
//!    before the restart — and, when the relaunch asked for one, which model
//!    it runs now — and to carry on ([`continue_prompt`],
//!    [`continue_prompt_with_model`]).
//! 5. **Confirm** — read the model the resumed session's first answer names
//!    ([`transcript_first_model`], past the mark the restart took, in the new
//!    build's rows) and record it beside the one before ([`restart_outcome`]).
//!    The rewrite keeps an explicit `--model` ([`launch_model`]) and adds none
//!    of its own, so a session launched without one comes back on whatever
//!    Claude Code picks at the relaunch — unless the model rule moves it
//!    (`upgrade_models`, owner decision 2026-09-27: the NEWEST MODEL OF ITS
//!    OWN FAMILY first, Opus 5 -> Opus 5.5; only with none newer, up the
//!    priority list, for a model nobody chose; never down), when the
//!    announcement and the continuation name the model and why ([`prepare_prompt_with_model`],
//!    [`continue_prompt_with_model`]), the relaunch carries it as `--model`
//!    in place of the launch's own, and the outcome says whether it was taken
//!    ([`restart_outcome_listed`]): a change is the expected outcome and is
//!    only said, with what decided it; a session that has not answered in
//!    time is said to be UNCONFIRMED. The explicit flag is what makes the
//!    move certain: without one the relaunch leaves the model to Claude
//!    Code's own resume, and the 2026-09-25 incident (2.1.282 -> 2.1.283, no
//!    `--model`) came back on `claude-opus-5` while that build's newest Opus
//!    was `claude-opus-5-5`.
//!
//! A conversation with NO TASK — nobody but the harness has asked it anything
//! ([`TaskScan`]) — skips all of it (D1 of the live E2E of 2026-09-26): no
//! notice, no READY, no `--resume` and no continuation; once idle it is ended
//! and the new build started AFRESH in its tab ([`Step::Fresh`]). There is
//! nothing to preserve, and a notice would start the conversation nobody
//! asked for.
//!
//! AN AGENT THE UPGRADE ASKED IS NEVER LEFT STOPPED (the 2026-09-26 incident:
//! an agent obeyed four queued notices at once, answered READY, and sat idle
//! under an upgrade that had given up hours before). It is either restarted
//! and told to carry on (4), or RELEASED: whenever the upgrade abandons an
//! announcement without restarting — it gives up asking ([`MAX_ASKS`] bounds
//! the nagging, not the agent), voids a READY answer a person held
//! ([`DRAIN_S`]), is held by the owner's `--skip`/`--defer`, or a restart
//! after READY stops — ONE line ([`release_prompt`]) is owed and typed at the
//! next point the notice itself could be ([`gate_release`]). An upgrade that
//! gave up still honours a READY answer that comes before that line
//! ([`GAVE_UP`]). The line is dropped only where it is no longer the
//! upgrade's to type: another process holds the conversation, or the agent
//! took up direction given after its last answer to the upgrade
//! ([`directed_since_ready`]) — never over a direction it answered with
//! READY. ONE STATED EXCEPTION, outside the derived model
//! (`harness_upgrade_never_strands_model`): an agent found to be no job of a
//! job-control shell is refused and owed no line (`upgrade_drive::not_a_job`)
//! — the line is typed under the notice's fences, the agent its shell's
//! foreground job among them, and such an agent can never be proven to read
//! it.
//!
//! Everything here is PURE: parsers, the target rule, the argv rewrite, the
//! line, the prompts, the model reads, the outcome line and the two gates,
//! each over facts a driver measured. The driver (`cli::run_upgrade`) owns the
//! I/O; the facts it feeds in are named at each function. The session file
//! `~/.claude/sessions/<pid>.json` is Claude Code's own record (measured
//! 2.1.278-2.1.281: `pid`, `sessionId`, `cwd`, `version`, `status`
//! busy|shell|idle|waiting, `statusUpdatedAt` ms, `procStart`, `kind`,
//! `entrypoint`); it is a third party's file, so every read refuses on an
//! unexpected shape rather than guessing. So is the transcript
//! (`~/.claude/projects/<dir>/<sessionId>.jsonl`), read for the READY answer
//! and the model.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::upgrade_models as models;

/// A dotted-digits version (`2.1.281`), compared numerically part by part, a
/// missing part reading as 0 — so `2.1` EQUALS `2.1.0`, and equality agrees with
/// the ordering (a derived `PartialEq` over the parts would not).
#[derive(Clone, Debug)]
pub struct Version(Vec<u64>);

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl Version {
    /// Parse `2.1.281` (a leading `v` tolerated). `None` for anything that is not
    /// one or more dot-separated runs of ASCII digits — a pre-release suffix is
    /// refused rather than ordered, because the rule "never downgrade" must not
    /// rest on a guess about how a vendor orders its suffixes.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let t = t.strip_prefix('v').unwrap_or(t);
        if t.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for p in t.split('.') {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 12 {
                return None;
            }
            parts.push(p.parse().ok()?);
        }
        Some(Self(parts))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.0.len().max(other.0.len());
        for i in 0..n {
            let a = self.0.get(i).copied().unwrap_or(0);
            let b = other.0.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for p in &self.0 {
            if !first {
                f.write_char('.')?;
            }
            write!(f, "{p}")?;
            first = false;
        }
        Ok(())
    }
}

/// Claude Code's own record of one running interactive process,
/// `<config>/sessions/<pid>.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFile {
    /// The process id the record describes.
    pub pid: u32,
    /// The conversation this process holds now (Claude rewrites it when the
    /// session changes, so it is re-read at every decision).
    pub session_id: String,
    /// The process's working directory as Claude last recorded it.
    pub cwd: String,
    /// The running build's version, as the running build reports itself.
    pub version: String,
    /// `busy`, `shell` (a background shell runs), `idle` or `waiting` (a
    /// dialog or permission prompt is up).
    pub status: String,
    /// When `status` last changed, unix milliseconds.
    pub status_updated_at_ms: u64,
    /// The process start time as Claude renders it (`ps -o lstart=` in UTC,
    /// C locale): the pid-reuse guard.
    pub proc_start: String,
    /// `interactive` for a TUI session.
    pub kind: String,
    /// `cli` for a session started from a shell.
    pub entrypoint: String,
}

/// Parse a session file, refusing any shape this module was not written for.
///
/// # Errors
/// The text is not a JSON object, or a required field is missing or of the
/// wrong type — named, so a schema change in a new Claude reads as exactly
/// that rather than as an idle session.
pub fn parse_session_file(text: &str) -> Result<SessionFile, String> {
    let v: Value = aterm_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let obj = v.as_object().ok_or("not a JSON object")?;
    let s = |k: &str| -> Result<String, String> {
        obj.get(k)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("no string `{k}`"))
    };
    let n = |k: &str| -> Result<u64, String> {
        obj.get(k)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("no number `{k}`"))
    };
    let pid = u32::try_from(n("pid")?).map_err(|_| "pid out of range".to_string())?;
    Ok(SessionFile {
        pid,
        session_id: s("sessionId")?,
        cwd: s("cwd")?,
        version: s("version")?,
        status: s("status")?,
        status_updated_at_ms: n("statusUpdatedAt")?,
        proc_start: s("procStart")?,
        kind: s("kind")?,
        entrypoint: s("entrypoint")?,
    })
}

/// Whether a session id is safe to put on a command line: Claude's ids are
/// UUIDs, so nothing but hex and dashes, 8 to 64 bytes.
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Where a relaunch target comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// aterm's managed build (the agents twin's store build).
    Managed,
    /// The vendor's own installer's copy (Claude's native `~/.local` install,
    /// which its updater moves — the source of `Update installed · Restart to
    /// update`).
    Native,
}

impl Source {
    /// The word the ledger and the notice print.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Managed => "managed",
            Source::Native => "native",
        }
    }
}

/// Which agent an upgrade moves: the Claude lane (this module) or the Codex
/// lane ([`super::upgrade_codex`]). One state directory, one ledger and one
/// owner's view carry both; this word tells their rows apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code (a state written before the word existed is one).
    #[default]
    Claude,
    /// Codex.
    Codex,
}

impl Agent {
    /// The word the state file carries: empty for Claude (every state an
    /// older build wrote), `codex`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Agent::Claude => "",
            Agent::Codex => "codex",
        }
    }

    /// [`Self::word`] read back; anything but `codex` is Claude's.
    #[must_use]
    pub fn parse(word: &str) -> Self {
        if word == "codex" {
            Agent::Codex
        } else {
            Agent::Claude
        }
    }

    /// The product's name, as the owner reads it.
    #[must_use]
    pub fn product(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
        }
    }
}

/// A build a session could be relaunched on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What to execute: an absolute path.
    pub exe: PathBuf,
    /// Its version, as it reports itself.
    pub version: Version,
    /// Where it comes from.
    pub source: Source,
}

/// THE TARGET RULE: the newest candidate strictly newer than what runs, and on a
/// version tie the MANAGED one (inside aterm the managed copy leads — owner
/// decision 2026-09-10 — but never at the price of going backwards). `None`
/// when nothing is newer: a session is never moved sideways or down, so a native
/// 2.1.281 session is left alone while the managed copy is 2.1.280, and moved
/// onto managed the day managed reaches 2.1.282.
#[must_use]
pub fn choose_target(running: &Version, candidates: &[Candidate]) -> Option<Candidate> {
    let mut best: Option<&Candidate> = None;
    for c in candidates.iter().filter(|c| c.version > *running) {
        best = match best {
            None => Some(c),
            Some(b) => match c.version.cmp(&b.version) {
                Ordering::Greater => Some(c),
                Ordering::Equal if c.source == Source::Managed => Some(c),
                _ => Some(b),
            },
        };
    }
    best.cloned()
}

/// Why a flag list cannot be carried into a resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgvRefusal {
    /// A flag this table does not know: fail closed rather than guess whether it
    /// takes a value.
    UnknownFlag(String),
    /// A flag whose session cannot be resumed as a TUI in place (`--print`,
    /// `--bg`, `--tmux`, `--worktree`, `--no-session-persistence`, …).
    NotResumable(String),
}

impl std::fmt::Display for ArgvRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgvRefusal::UnknownFlag(fl) => write!(f, "unknown-flag {fl}"),
            ArgvRefusal::NotResumable(fl) => write!(f, "not-resumable {fl}"),
        }
    }
}

/// How a Claude flag takes its value (from `claude --help`, 2.1.281), consumed
/// exactly as Claude's own parser consumes it (see [`maybe_option`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    /// A switch.
    None,
    /// Exactly one value (`<value>`): the next token, whatever it looks like.
    One,
    /// A REQUIRED list (`<values...>`): the next token whatever it looks like,
    /// then every further token until one is an option. An OPTIONAL list
    /// (`[values...]`) is not this — none exists in 2.1.281, and one added later
    /// needs its own arm, not this one.
    Many,
    /// An optional value (`[value]`): the next token if it is not an option (a
    /// lone `-` is a value).
    Optional,
}

/// What the rewrite does with a flag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Carried verbatim.
    Keep,
    /// Dropped with its value — the resume family, replaced by `--resume <id>`.
    Drop,
    /// The whole relaunch is refused.
    Refuse,
}

/// Claude Code 2.1.281's options. A flag missing from here refuses the rewrite
/// (fail closed): a new version's flag is added here, on purpose, when it is
/// known to be safe to carry.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--add-dir", Arity::Many, Fate::Keep),
    ("--agent", Arity::One, Fate::Keep),
    ("--agents", Arity::One, Fate::Keep),
    (
        "--allow-dangerously-skip-permissions",
        Arity::None,
        Fate::Keep,
    ),
    ("--allowedTools", Arity::Many, Fate::Keep),
    ("--allowed-tools", Arity::Many, Fate::Keep),
    ("--append-system-prompt", Arity::One, Fate::Keep),
    ("--append-system-prompt-file", Arity::One, Fate::Keep),
    ("--autocompact", Arity::One, Fate::Keep),
    ("--ax-screen-reader", Arity::None, Fate::Keep),
    ("--bg", Arity::None, Fate::Refuse),
    ("--background", Arity::None, Fate::Refuse),
    ("--bare", Arity::None, Fate::Keep),
    ("--betas", Arity::Many, Fate::Keep),
    ("--brief", Arity::None, Fate::Keep),
    ("--chrome", Arity::None, Fate::Keep),
    ("--cloud", Arity::Optional, Fate::Refuse),
    ("-c", Arity::None, Fate::Drop),
    ("--continue", Arity::None, Fate::Drop),
    ("--dangerously-skip-permissions", Arity::None, Fate::Keep),
    ("-d", Arity::Optional, Fate::Keep),
    ("--debug", Arity::Optional, Fate::Keep),
    ("--debug-file", Arity::One, Fate::Keep),
    ("--disable-slash-commands", Arity::None, Fate::Keep),
    ("--disallowedTools", Arity::Many, Fate::Keep),
    ("--disallowed-tools", Arity::Many, Fate::Keep),
    // DROPPED (owner direction 2026-09-23: "we prefer to use the default
    // settings unless there is a great reason not to use that"): the resumed
    // session comes back at the user's default effort — their Claude
    // settings — never pinned to the effort it was launched with. A launch `--settings` is still carried.
    ("--effort", Arity::One, Fate::Drop),
    ("--environment", Arity::One, Fate::Refuse),
    (
        "--exclude-dynamic-system-prompt-sections",
        Arity::None,
        Fate::Keep,
    ),
    ("--fallback-model", Arity::One, Fate::Keep),
    ("--file", Arity::Many, Fate::Refuse),
    ("--fork-session", Arity::None, Fate::Drop),
    ("--forward-subagent-text", Arity::None, Fate::Keep),
    ("--from-pr", Arity::Optional, Fate::Drop),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    ("--ide", Arity::None, Fate::Keep),
    ("--include-hook-events", Arity::None, Fate::Keep),
    ("--include-partial-messages", Arity::None, Fate::Keep),
    ("--input-format", Arity::One, Fate::Refuse),
    ("--json-schema", Arity::One, Fate::Refuse),
    ("--max-budget-usd", Arity::One, Fate::Keep),
    ("--mcp-config", Arity::Many, Fate::Keep),
    ("--model", Arity::One, Fate::Keep),
    ("-n", Arity::One, Fate::Keep),
    ("--name", Arity::One, Fate::Keep),
    ("--no-chrome", Arity::None, Fate::Keep),
    ("--no-session-persistence", Arity::None, Fate::Refuse),
    ("--output-format", Arity::One, Fate::Refuse),
    ("--permission-mode", Arity::One, Fate::Keep),
    ("--permission-prompts", Arity::One, Fate::Keep),
    ("--plugin-dir", Arity::One, Fate::Keep),
    ("--plugin-url", Arity::One, Fate::Keep),
    ("-p", Arity::None, Fate::Refuse),
    ("--print", Arity::None, Fate::Refuse),
    ("--prompt-suggestions", Arity::Optional, Fate::Keep),
    ("--remote-control", Arity::Optional, Fate::Refuse),
    (
        "--remote-control-session-name-prefix",
        Arity::One,
        Fate::Refuse,
    ),
    ("--replay-user-messages", Arity::None, Fate::Refuse),
    ("--restricted", Arity::None, Fate::Keep),
    ("-r", Arity::Optional, Fate::Drop),
    ("--resume", Arity::Optional, Fate::Drop),
    ("--safe-mode", Arity::None, Fate::Keep),
    ("--session-id", Arity::One, Fate::Drop),
    ("--setting-sources", Arity::One, Fate::Keep),
    ("--settings", Arity::One, Fate::Keep),
    ("--strict-mcp-config", Arity::None, Fate::Keep),
    ("--system-prompt", Arity::One, Fate::Keep),
    ("--system-prompt-file", Arity::One, Fate::Keep),
    ("--system-prompt-snapshot", Arity::One, Fate::Keep),
    ("--teleport", Arity::Optional, Fate::Refuse),
    ("--tmux", Arity::Optional, Fate::Refuse),
    ("--tools", Arity::Many, Fate::Keep),
    ("--verbose", Arity::None, Fate::Keep),
    ("-v", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    ("-w", Arity::Optional, Fate::Refuse),
    ("--worktree", Arity::Optional, Fate::Refuse),
];

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Whether Claude's own parser reads `tok` as an option. The Commander bundled
/// in Claude Code 2.1.281 asks `o.length>1&&o[0]==="-"` (its `parseOptions`,
/// read out of the shipped binary), so a lone `-` is a VALUE or a positional,
/// never a flag. Every "does a flag start here" question [`rewrite_argv`] asks
/// is this one, so the rewrite consumes exactly the tokens Claude consumed — a
/// flag left bare by a rewrite that consumed less would swallow the appended
/// `--resume <id>` as its own value, and the relaunch would not resume at all.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// THE ARGV REWRITE: the original argv (argv[0] included, which is dropped)
/// becomes the flags a resume carries — every known flag and its values kept
/// verbatim, the resume family (`--resume`/`-r [id]`, `--continue`/`-c`,
/// `--session-id <v>`, `--fork-session`, `--from-pr [v]`) dropped, then
/// `--resume <session_id>` appended.
///
/// A POSITIONAL word (the prompt a session was started with, `claude "fix the
/// bug"`) is DROPPED, not refused: the conversation already holds it, and
/// sending it again on resume would ask the same thing twice. `--` ends option
/// parsing, so everything after it is positional and dropped the same way —
/// unless it is a flag's value (`--add-dir --`), which Claude's parser takes
/// whatever it looks like, and so does this one.
///
/// # Errors
/// An unknown flag ([`ArgvRefusal::UnknownFlag`]) or one whose session is not a
/// TUI resumable in place ([`ArgvRefusal::NotResumable`]).
pub fn rewrite_argv(argv: &[String], session_id: &str) -> Result<Vec<String>, ArgvRefusal> {
    let mut out: Vec<String> = carried(argv)?
        .into_iter()
        .flat_map(|(_, tokens)| tokens.iter().cloned())
        .collect();
    out.push("--resume".to_string());
    out.push(session_id.to_string());
    Ok(out)
}

/// Every flag [`rewrite_argv`] carries, in argv order: its name and its tokens
/// as the CLI parsed them (`--model=<v>` is one token, `--model <v>` two).
fn carried(argv: &[String]) -> Result<Vec<(&str, &[String])>, ArgvRefusal> {
    let mut out = Vec::new();
    for (name, fate, tokens) in parsed(argv) {
        match fate {
            None => return Err(ArgvRefusal::UnknownFlag(name.to_string())),
            Some(Fate::Refuse) => return Err(ArgvRefusal::NotResumable(name.to_string())),
            Some(Fate::Keep) => out.push((name, tokens)),
            Some(Fate::Drop) => {}
        }
    }
    Ok(out)
}

/// Every flag in `argv` as Claude's parser reads it, in argv order: its name,
/// its fate in [`FLAGS`] (`None`: a flag the table does not know, read as a
/// switch — [`carried`] stops at it), and its tokens. Positionals and
/// everything after `--` are not flags.
fn parsed(argv: &[String]) -> Vec<(&str, Option<Fate>, &[String])> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let tok = &argv[i];
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            // A positional: the first prompt. Dropped (see the doc).
            i += 1;
            continue;
        }
        let (name, inline) = match tok.split_once('=') {
            Some((n, _)) if n.starts_with("--") => (n, true),
            _ => (tok.as_str(), false),
        };
        let known = flag(name);
        // The token and its values, as the CLI parsed them.
        let start = i;
        i += 1;
        if !inline {
            match known.map_or(Arity::None, |(arity, _)| arity) {
                Arity::None => {}
                Arity::One => {
                    if i < argv.len() {
                        i += 1;
                    }
                }
                Arity::Optional => {
                    if i < argv.len() && !maybe_option(&argv[i]) {
                        i += 1;
                    }
                }
                Arity::Many => {
                    // The first value is REQUIRED, so Claude's parser takes it
                    // whatever it looks like (`--add-dir -`, `--add-dir -c`);
                    // stopping at it would leave the flag bare.
                    if i < argv.len() {
                        i += 1;
                    }
                    while i < argv.len() && !maybe_option(&argv[i]) {
                        i += 1;
                    }
                }
            }
        }
        out.push((name, known.map(|(_, fate)| fate), &argv[start..i]));
    }
    out
}

/// The flags that make a launch a ONE-SHOT RUN rather than a session: it
/// prints one answer and exits (`-p`), or answers about itself and exits.
const ONE_SHOT: &[&str] = &["-p", "--print", "-h", "--help", "-v", "--version"];

/// Whether `argv` (argv[0] included) is a ONE-SHOT RUN ([`ONE_SHOT`]): its
/// exit is the end it was launched for, never a crash to relaunch — wherever
/// the flag stands, an unknown flag before it included.
#[must_use]
pub fn one_shot(argv: &[String]) -> bool {
    parsed(argv)
        .iter()
        .any(|(name, _, _)| ONE_SHOT.contains(name))
}

/// THE MODEL THE RELAUNCH ASKS FOR: the value of the `--model` [`rewrite_argv`]
/// keeps from `argv` — the last one, as Claude's parser takes it — or `None`
/// when the launch named none, or its argv is one the rewrite refuses.
#[must_use]
pub fn launch_model(argv: &[String]) -> Option<String> {
    let flags = carried(argv).ok()?;
    let (_, tokens) = flags
        .into_iter()
        .rev()
        .find(|(name, _)| *name == "--model")?;
    match tokens {
        [one] => one.split_once('=').map(|(_, v)| v.to_string()),
        [_, value] => Some(value.clone()),
        _ => None,
    }
}

/// The tab's shell, as its executable names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// zsh.
    Zsh,
    /// bash.
    Bash,
    /// fish.
    Fish,
}

impl Dialect {
    /// From the shell's executable basename (a login shell's `-zsh` included).
    /// `None` for any other shell: the relaunch line is written for these three
    /// and no other.
    #[must_use]
    pub fn from_exe_name(name: &str) -> Option<Self> {
        let base = name.rsplit('/').next().unwrap_or(name);
        match base.trim_start_matches('-') {
            "zsh" => Some(Dialect::Zsh),
            "bash" => Some(Dialect::Bash),
            "fish" => Some(Dialect::Fish),
            _ => None,
        }
    }

    /// The atpkg shell hook for this dialect's extension.
    #[must_use]
    pub fn hook_ext(self) -> &'static str {
        match self {
            Dialect::Zsh => "zsh",
            Dialect::Bash => "bash",
            Dialect::Fish => "fish",
        }
    }
}

/// Quote one word for `dialect`. POSIX shells: single quotes with `'\''`. fish:
/// single quotes, where `\\` and `\'` are the only escapes.
#[must_use]
pub fn quote(dialect: Dialect, word: &str) -> String {
    match dialect {
        Dialect::Zsh | Dialect::Bash => format!("'{}'", word.replace('\'', r"'\''")),
        Dialect::Fish => format!("'{}'", word.replace('\\', r"\\").replace('\'', r"\'")),
    }
}

/// The longest relaunch line this module writes on the kernel it runs on
/// (`LineDiscipline::HOST`). A longer one is refused, never truncated: a cut
/// line would run something no one planned.
///
/// The bound is the TERMINAL's, not ours. The line is typed once the shell holds
/// the terminal again, and that can be before its line editor has put the tty in
/// raw mode: prompt work the shell does ITSELF (a `$(git …)` or `$(starship
/// prompt)` substitution, `vcs_info`, a loop) runs with the terminal already
/// back and the tty still canonical. Typing that lands in that window goes to
/// the kernel's line buffer, which keeps a bounded number of bytes of one line
/// (`LineDiscipline::line_keeps`) and loses the rest. `--resume <id>` is the
/// END of the line, so any cut loses it: measured 2026-09-23 (Darwin 25.6, zsh
/// 5.9) with the Enter in that window, a 1023-byte line ran and a 1024-byte one
/// did not, a 1213-byte relaunch line left the shell at `quote>`, and one cut on
/// a word boundary ran claude WITHOUT `--resume` — a new conversation. (An
/// external command in a `precmd` holds the terminal itself, and the driver's
/// wait for the shell to hold it again already covers that case.)
///
/// So the line, a bracketed-paste frame around it (`PASTE_FRAME`) and the
/// Enter fit in one canonical line of each kernel, with room to spare; the
/// checks below hold that for both at compile time. The bound is per kernel
/// because the kernels differ fourfold, and one bound for both would refuse on
/// Linux the lines it keeps whole. The driver builds the line before it types
/// the upgrade notice, and again before its one signal, so a launch whose line
/// would not fit is refused before the agent is asked to wind down, and a
/// refusal ends nothing.
#[cfg(test)]
pub(crate) const MAX_LINE: usize = LineDiscipline::HOST.max_line();

/// The kernel line discipline a relaunch line is typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineDiscipline {
    /// macOS — and any kernel not named here, which gets the smaller bound.
    Darwin,
    /// Linux `n_tty`.
    Linux,
}

impl LineDiscipline {
    /// The kernel this build runs on.
    const HOST: Self = if cfg!(target_os = "linux") {
        Self::Linux
    } else {
        Self::Darwin
    };

    /// The most bytes of one canonical line the kernel keeps, the Enter
    /// included. macOS: `MAX_INPUT` (`getconf MAX_INPUT /dev/tty` = 1024; the
    /// rest is DROPPED, Enter included, so a cut line never runs — measured
    /// above). Linux: `N_TTY_BUF_SIZE`, 4095 bytes and the newline (termios(3);
    /// the rest is discarded but the newline still arrives, so a cut line RUNS).
    const fn line_keeps(self) -> usize {
        match self {
            Self::Darwin => 1024,
            Self::Linux => 4096,
        }
    }

    /// The longest relaunch line written for this kernel.
    const fn max_line(self) -> usize {
        match self {
            Self::Darwin => 1000,
            Self::Linux => 4000,
        }
    }
}

/// `ESC [ 200 ~` and `ESC [ 201 ~`, the frame a paste may be wrapped in.
const PASTE_FRAME: usize = 12;

// The line and its frame, strictly under each kernel's limit: the Enter is the
// last byte kept.
const _: () =
    assert!(LineDiscipline::Darwin.max_line() + PASTE_FRAME < LineDiscipline::Darwin.line_keeps());
const _: () =
    assert!(LineDiscipline::Linux.max_line() + PASTE_FRAME < LineDiscipline::Linux.line_keeps());

/// THE RELAUNCH LINE the tab's shell runs, prefixed with one space (kept out of
/// history where the shell honours that). `heal` is the atpkg hook to source
/// first when the tab's shell predates aterm's managed `agents/` directory (a
/// frozen PATH: the heal and the relaunch are one line, so the shell is right for
/// every later command too); `cd` is the agent's own launch directory when it
/// differs from the shell's (Claude files a conversation under its launch cwd,
/// so `--resume` must run there), entered in a subshell so the person's shell
/// keeps its own directory.
///
/// # Errors
/// A line past `MAX_LINE`, a control character in any word, or a `cd` for fish
/// (no subshell form is written for it).
pub fn relaunch_line(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_on(LineDiscipline::HOST, dialect, heal, cd, exe, args)
}

/// [`relaunch_line`] for the kernel `tty`, whose bound it keeps.
fn relaunch_line_on(
    tty: LineDiscipline,
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    let exe_s = exe.to_string_lossy();
    for w in std::iter::once(exe_s.as_ref())
        .chain(args.iter().map(String::as_str))
        .chain(cd)
    {
        if w.chars().any(char::is_control) {
            return Err("a control character in the line".to_string());
        }
    }
    let mut line = String::from(" ");
    if let Some(hook) = heal {
        let h = quote(dialect, &hook.to_string_lossy());
        match dialect {
            Dialect::Zsh => {
                let _ = write!(line, ". {h}; rehash; ");
            }
            Dialect::Bash => {
                let _ = write!(line, ". {h}; hash -r; ");
            }
            Dialect::Fish => {
                let _ = write!(line, "source {h}; ");
            }
        }
    }
    let mut cmd = quote(dialect, &exe_s);
    for a in args {
        cmd.push(' ');
        cmd.push_str(&quote(dialect, a));
    }
    match cd {
        None => line.push_str(&cmd),
        Some(dir) => match dialect {
            Dialect::Zsh | Dialect::Bash => {
                let _ = write!(line, "( cd -- {} && exec {cmd} )", quote(dialect, dir));
            }
            Dialect::Fish => {
                return Err("fish: the agent's directory differs from the shell's".to_string());
            }
        },
    }
    let max = tty.max_line();
    if line.len() > max {
        return Err(format!(
            "the line would be {} bytes (max {max})",
            line.len()
        ));
    }
    Ok(line)
}

/// THE RELAUNCH LINE THAT ALSO HEALS THE SHELL (2026-09-26): `line`
/// ([`relaunch_line`]) with, in front of everything it runs, the shell's own
/// take of a fresh shell-integration key from the one-use file `path` — which
/// the window wrote, and authorized as a key the shell has not taken yet, when
/// the sweep asked it to (`rekey`, for a tab whose `status integration=` reads
/// `degraded`). A shell spawned before the re-key channel has no hook that
/// would ever read such a file, and its foreground is usually an agent, where
/// typed text is a prompt; the relaunch is the one line typed while the SHELL
/// holds the terminal, so the heal rides it and costs no extra typing. The
/// text is the scripts' own ([`aterm_shell_integration::typed_rekey`]): only
/// the PATH is typed, never the key.
///
/// The same line UPGRADES a shell whose integration predates loaders
/// (2026-09-26): the window names this build's script folder and the tab's
/// body pointer in the file for such a shell — healthy too, with the key it
/// already signs with — and the text
/// ([`aterm_shell_integration::typed_rekey_with_loader`]) sources this build's
/// loader when they are there. Where only the key-only text fits the bound,
/// that one is typed.
///
/// # Errors
/// A control character in the path, or a line past the kernel's bound in both
/// forms (the same bound [`relaunch_line`] keeps). The caller then types `line`
/// as it is: the heal is best-effort, never a reason to strand the agent.
pub fn with_rekey(dialect: Dialect, line: &str, path: &Path) -> Result<String, String> {
    with_rekey_on(LineDiscipline::HOST, dialect, line, path)
}

/// [`with_rekey`] for the kernel `tty`, whose bound it keeps.
fn with_rekey_on(
    tty: LineDiscipline,
    dialect: Dialect,
    line: &str,
    path: &Path,
) -> Result<String, String> {
    let p = path.to_string_lossy();
    if p.chars().any(char::is_control) {
        return Err("a control character in the re-key path".to_string());
    }
    let shell = match dialect {
        Dialect::Zsh => aterm_shell_integration::ShellType::Zsh,
        Dialect::Bash => aterm_shell_integration::ShellType::Bash,
        Dialect::Fish => aterm_shell_integration::ShellType::Fish,
    };
    // The form that ALSO upgrades a shell from before loaders in place
    // (2026-09-26: the window names this build's loader in the file for such a
    // shell), then — where only it fits — the key-only form, which reads the
    // same file's first line: a relaunch never loses its heal to the upgrade.
    let q = quote(dialect, &p);
    let takes = [
        aterm_shell_integration::typed_rekey_with_loader(shell, &q),
        aterm_shell_integration::typed_rekey(shell, &q),
    ];
    // After the one leading space that keeps the line out of history.
    let rest = line.strip_prefix(' ').unwrap_or(line);
    let max = tty.max_line();
    let mut shortest = None;
    for take in takes.into_iter().flatten() {
        let healed = format!(" {take} {rest}");
        if healed.len() <= max {
            return Ok(healed);
        }
        shortest = Some(healed.len());
    }
    Err(shortest.map_or_else(
        || "no re-key for this shell".to_string(),
        |len| format!("the healed line would be {len} bytes (max {max})"),
    ))
}

/// The one-time READY marker for one upgrade: fixed words plus a short id the
/// agent cannot have seen before this announcement, so an assistant message
/// carrying it can only be the answer to it.
#[must_use]
pub fn ready_marker(session_id: &str, to: &Version, salt: u64) -> String {
    // FNV-1a over the inputs: a label, not a secret.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session_id
        .bytes()
        .chain(to.to_string().bytes())
        .chain(salt.to_le_bytes())
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{READY_PREFIX}{:08x}", h & 0xffff_ffff)
}

/// The fixed words every READY marker opens with ([`ready_marker`]).
pub const READY_PREFIX: &str = "ATERM-UPGRADE-READY-";

/// Whether one line of an agent's words IS the READY answer `marker` by the
/// rule [`transcript_has_ready`] reads the transcript with: the line, trimmed
/// of spaces and backticks, is the whole marker. Words that merely QUOTE a
/// marker (`I'll reply with ATERM-UPGRADE-READY-… once the build is done`)
/// are no answer (review of 2026-09-25).
fn is_ready_line(line: &str, marker: &str) -> bool {
    line.trim().trim_matches('`') == marker
}

/// How [`prepare_prompt`]'s announcement opens: the tag and the first words
/// that set it apart from [`continue_prompt`]'s (`[aterm harness] Upgraded:`).
/// The supervisor's turn-end policy types nothing at a point that answers an
/// announcement: the session's worker owns that point until it restarts the
/// session (`IdleHost::owns_turn_end`).
pub const ANNOUNCE_HEAD: &str = concat!(harness_mark!(), " Claude Code ");

/// THE ANNOUNCEMENT, typed as one ordinary user turn. It asks for a good
/// stopping point ([`STOPPING_POINT`]) and names the one line to answer with;
/// it never asks the agent to cancel work that is still making progress.
#[must_use]
pub fn prepare_prompt(from: &Version, to: &Version, source: Source, marker: &str) -> String {
    format!(
        "{ANNOUNCE_HEAD}{to} ({}) is installed; this session runs {from}. \
         To move you onto it, aterm will restart this Claude Code in place and resume this \
         same conversation (claude --resume, same tab, same flags). {STOPPING_POINT} When \
         nothing of yours is still running, reply with {marker} on a line by itself. If you \
         cannot stop now, say why; aterm will wait and ask again later.",
        source.as_str()
    )
}

/// WHAT THE ANNOUNCEMENT ASKS OF THE AGENT'S OWN WORK. Live work is waited
/// for and never cancelled (the owner's ask of 2026-09-23: "we'd want to not
/// kill the background processes. we'd want to wait for them"). A wait that
/// can never end is not such work, and the agent is the one who can tell. On
/// 2026-09-26 a tab sat on Claude Code 2.1.278 for four days: it held two
/// background `until [ <count> -ge 6 ]; do sleep 15; done` loops, and two of
/// the six agents they counted had died on API 529s. The notice's old words
/// ("let any background tasks … finish (do not cancel them)") told the agent
/// to keep exactly that. Told what ran, with the clause below, the agent
/// checked the count, stopped both loops, and answered READY within
/// minutes. aterm itself still ends nothing ([`gate_restart`]).
pub const STOPPING_POINT: &str = "Please get to a good stopping point first: let \
     background tasks, subagents or workflows you started finish while they are still making \
     progress (do not cancel them), but stop any background shell or task of yours that only \
     waits for something that has already ended or can never happen (a poll loop on a workflow, \
     agent, job or file that is gone): that wait is not work. Save or commit work in progress, \
     and do not start new long-running work.";

/// One process under the agent that the restart waits on, as the notice
/// names it ([`running_clause`]). `age_s` is its elapsed time. `command` is
/// what it runs, cut to its point by `upgrade_drive::command_head`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// Its pid.
    pub pid: u32,
    /// Its executable's basename (`zsh`, `caffeinate`).
    pub name: String,
    /// Seconds since it started.
    pub age_s: u64,
    /// What it runs, one line. Empty unless it is the agent's own Bash-tool
    /// shell, a direct child of the agent. The notice is a user turn, so it
    /// quotes only what the agent itself ran, never a line a script it ran
    /// put deeper down (`upgrade_drive::held_of`).
    pub command: String,
}

/// The most processes [`running_clause`] names; the rest are counted.
pub const HELD_NAMED: usize = 5;

/// The longest command [`running_clause`] quotes, in characters.
pub const HELD_COMMAND_CHARS: usize = 160;

/// THE NOTICE'S LIST OF WHAT RUNS UNDER THE AGENT (the same 2026-09-26 tab as
/// [`STOPPING_POINT`]). Every notice appends it when anything runs, a re-ask
/// included. The agent then judges from what aterm itself sees: each process
/// by pid, name and age, and the command it runs when it is the agent's own
/// Bash-tool shell ([`Held::command`]). The four-day-old loops that held that
/// tab were plain in this list, and invisible in "background tasks you
/// started". Empty when nothing runs. One line with no control characters,
/// because the notice is typed as one paste ([`held_list`]).
#[must_use]
pub fn running_clause(held: &[Held]) -> String {
    let list = held_list(held);
    if list.is_empty() {
        return String::new();
    }
    format!(" Running under you now, as aterm sees it: {list}.")
}

/// What runs under the agent as a bare list, with no lead-in and no final
/// period: `pid 63492 (zsh, 5d4h): <command>; pid …; and 2 more`. The notice
/// wraps it for the agent ([`running_clause`]) and the give-up's ledger row
/// for the owner. At most [`HELD_NAMED`] processes are named and the rest
/// counted, and each command is cut to [`HELD_COMMAND_CHARS`]. Empty when
/// nothing runs.
#[must_use]
pub fn held_list(held: &[Held]) -> String {
    let mut out = String::new();
    for (i, h) in held.iter().take(HELD_NAMED).enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let command = one_line(&h.command, HELD_COMMAND_CHARS);
        let _ = write!(
            out,
            "pid {} ({}, {})",
            h.pid,
            one_line(&h.name, 32),
            span(h.age_s)
        );
        if !command.is_empty() {
            let _ = write!(out, ": {command}");
        }
    }
    if held.len() > HELD_NAMED {
        let _ = write!(out, "; and {} more", held.len() - HELD_NAMED);
    }
    out
}

/// `text` as one line of at most `max` characters: every control character
/// a space, runs of spaces one, and a cut marked with `…`.
fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// [`prepare_prompt`] for a restart that ALSO moves the conversation to
/// `model` — or ONLY does (`to == from`: the build is current, the model is
/// not) — from `running`, the model it runs now, which says why
/// ([`models::move_why`]: the newest of its family, or the priority list's
/// best). The text still starts with [`ANNOUNCE_HEAD`], so the supervisor's
/// turn-end policy knows it for what it is.
#[must_use]
pub fn prepare_prompt_with_model(
    from: &Version,
    to: &Version,
    source: Source,
    marker: &str,
    model: Option<&str>,
    running: Option<&str>,
) -> String {
    let Some(model) = model else {
        return prepare_prompt(from, to, source, marker);
    };
    let why = models::move_why(running, model).words();
    let what = if to == from {
        format!(
            "{ANNOUNCE_HEAD}{to} can run {model}, {why}, which this session does not run. To \
             move you onto it, aterm will restart this Claude Code in place and resume this same \
             conversation on {model} (claude --resume --model {model}, same tab, same flags)."
        )
    } else {
        format!(
            "{ANNOUNCE_HEAD}{to} ({}) is installed; this session runs {from}. To move you onto \
             it, and onto {model} ({why}), aterm will restart this Claude Code in place and \
             resume this same conversation (claude --resume --model {model}, same tab, same \
             flags).",
            source.as_str()
        )
    };
    format!(
        "{what} {STOPPING_POINT} When nothing of yours is still running, reply with {marker} on \
         a line by itself. If you cannot stop now, say why; aterm will wait and ask again later."
    )
}

/// [`continue_prompt`] for a relaunch that asked for `model` on its command
/// line: it says the model the session ran before the restart (`ran`), the one
/// it runs NOW, and why ([`models::move_why`] of the two: the newest of its
/// family, or the priority list's best). The claim
/// is the relaunch line's own: a command-line `--model` is what a resumed
/// Claude Code runs, over its transcript's model and the saved default alike
/// (MEASURED 2026-09-24, design §9); the outcome row confirms it from the
/// resumed session's first answer ([`restart_outcome_listed`]).
#[must_use]
pub fn continue_prompt_with_model(
    from: &Version,
    to: &Version,
    ran: Option<&str>,
    model: Option<&str>,
) -> String {
    let Some(model) = model else {
        return continue_prompt(from, to, ran);
    };
    let build = if to == from {
        format!("on Claude Code {to}")
    } else {
        format!("on Claude Code {to} (from {from})")
    };
    let runs = ran.map_or_else(
        || "it now runs".to_string(),
        |m| format!("it ran {m} before the restart and now runs"),
    );
    let why = models::move_why(ran, model).words();
    format!(
        "{HARNESS_MARK} Upgraded: this session was restarted {build} and resumed; {runs} \
         {model}, {why} (for this session only: your default model is unchanged). {CARRY_ON}"
    )
}

/// THE CONTINUATION, typed once the relaunched process holds the same session.
/// `ran` is the model the session's last turn before the restart named
/// ([`transcript_model`]), said in one neutral clause so the agent — and the
/// person reading the tab — knows what it ran before; `None` (no turn named
/// one) leaves the clause out.
#[must_use]
pub fn continue_prompt(from: &Version, to: &Version, ran: Option<&str>) -> String {
    let resumed = match ran {
        Some(model) => format!("; it ran {model} before the restart and was resumed"),
        None => " and resumed".to_string(),
    };
    format!(
        "{HARNESS_MARK} Upgraded: this session was restarted on Claude Code {to} (from \
         {from}){resumed}. {CARRY_ON}"
    )
}

/// How every continuation after a restart ends — the upgrade's
/// ([`continue_prompt`]) and a relaunch's (`super::relaunch::resumed_prompt`):
/// under full automation nobody is at the keyboard, so an agent that was
/// waiting on the user decides for itself and keeps going (the same rule as
/// `[harness] answer_text`), never stopping to wait.
pub const CARRY_ON: &str = "Continue where you left off. If you were waiting on the user, \
                            nobody is here to answer: decide for yourself, prefer reversible \
                            steps, and keep going.";

/// THE MODEL the LAST assistant turn in `jsonl` (a transcript's tail) names as
/// `message.model` — what the session ran when it last answered. Read like
/// [`transcript_has_ready`] reads the same tail: a line that is not JSON (the
/// tail's cut first line, a last line caught half-written) is skipped.
///
/// A row is a turn of THE SESSION'S model only when it is an `assistant` row,
/// not a subagent's (`isSidechain`), and its model looks like a model id
/// ([`is_model_id`]). Claude Code writes assistant rows of its own that name
/// `<synthetic>` (measured 2026-09-24: 34 in the owner's session transcripts,
/// 22 of them in one, and 90 counting subagents' files — a limit notice,
/// an API error, and `No response requested.` written just after a restart);
/// those are skipped, never taken as the model.
#[must_use]
pub fn transcript_model(jsonl: &str) -> Option<String> {
    assistant_models(jsonl, None).last()
}

/// THE MODEL the FIRST assistant turn in `jsonl` that the build `version`
/// wrote names, by the rules of [`transcript_model`] — for the bytes past a
/// restart's mark, whose first answer from the NEW build is the resumed
/// session's own. Every transcript row carries the `version` that wrote it
/// (measured 2026-09-24: all 25,018 assistant rows in the owner's session
/// transcripts), so a row the old process wrote past the mark — alive past its
/// SIGTERM for one more turn — is skipped, and so is a row naming no version.
#[must_use]
pub fn transcript_first_model(jsonl: &str, version: &str) -> Option<String> {
    assistant_models(jsonl, Some(version)).next()
}

/// Every model the session's assistant turns in `jsonl` name, in order — only
/// the rows the build `version` wrote, when one is given.
fn assistant_models<'a>(
    jsonl: &'a str,
    version: Option<&'a str>,
) -> impl Iterator<Item = String> + 'a {
    jsonl.lines().filter_map(move |line| {
        if !line.contains("assistant") {
            return None;
        }
        let v = aterm_json::from_str::<Value>(line).ok()?;
        if v.get("type").and_then(Value::as_str) != Some("assistant")
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || version.is_some_and(|want| v.get("version").and_then(Value::as_str) != Some(want))
        {
            return None;
        }
        let model = v.get("message")?.get("model")?.as_str()?;
        is_model_id(model).then(|| model.to_string())
    })
}

/// Whether `m` looks like a model id: 1 to 256 bytes of ASCII letters, digits
/// and `. _ - : @ / [ ]` — `claude-opus-5-5`, `claude-opus-5-5[1m]`, a cloud
/// provider's `us.anthropic.…-v1:0`, `…@<date>` or an inference profile's ARN
/// (well past 64 bytes). `<synthetic>` is not one, and neither is anything with
/// a space or a control character: the id is typed into the tab in the
/// continuation, so a third party's file never puts a keystroke there, nor an
/// unbounded run of bytes.
pub(crate) fn is_model_id(m: &str) -> bool {
    (1..=256).contains(&m.len())
        && m.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@/[]".contains(&b))
}

/// THE OUTCOME LINE for the owner, once the resumed session has answered (or
/// has not): the build it restarted on and the model its first answer named.
/// A model other than the one before is said with what decided the model
/// after, the expected outcome and not a fault: `kept`, the `--model` the
/// relaunch kept from the launch ([`launch_model`]) — which undoes a `/model`
/// choice made since and resolves an alias against the new build — or, with
/// none kept, the rule that a session launched without one takes whatever
/// Claude Code's default is at the relaunch. `after` of `None` is UNCONFIRMED,
/// with the model before when there was one.
#[must_use]
pub fn restart_outcome(
    to: &str,
    before: Option<&str>,
    after: Option<&str>,
    kept: Option<&str>,
) -> String {
    let head = format!("claude restarted on {to} · model");
    match (before, after) {
        (Some(b), Some(a)) if b != a => {
            let why = kept.map_or_else(
                || "a session launched without --model takes the current default".to_string(),
                |m| format!("the relaunch kept the launch's --model {m}"),
            );
            format!("{head} {b} -> {a} ({why}; /model changes it)")
        }
        (_, Some(a)) => format!("{head} {a}"),
        (Some(b), None) => format!("{head} unconfirmed (it ran {b} before the restart)"),
        (None, None) => format!("{head} unconfirmed"),
    }
}

/// [`restart_outcome`] for a relaunch that asked for `chosen` by the model
/// rule (`upgrade_models`): a change is said with its reason — the newest of
/// its family, or the priority list's choice ([`models::move_why`] from
/// `before`) — and a model after other than `chosen` is said as not taken.
#[must_use]
pub fn restart_outcome_listed(
    to: &str,
    before: Option<&str>,
    after: Option<&str>,
    chosen: &str,
) -> String {
    let head = format!("claude restarted on {to} · model");
    let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
    let (asked, chose) = match models::move_why(before, chosen) {
        models::MoveWhy::Family => (
            format!("the relaunch asked for {chosen}, the newest of its family"),
            "the newest of its family",
        ),
        models::MoveWhy::List => (
            format!("the priority list asked for {chosen}"),
            "the priority list chose it",
        ),
        models::MoveWhy::Unknown => (
            format!("the relaunch asked for {chosen}"),
            "the model rule chose it",
        ),
    };
    match (before, after) {
        (_, Some(a)) if base(a) != base(chosen) => {
            format!("{head} {a} ({asked}; it was not taken)")
        }
        (Some(b), Some(a)) if base(b) != base(a) => {
            format!("{head} {b} -> {a} ({chose}; /model changes it)")
        }
        (_, Some(a)) => format!("{head} {a}"),
        (Some(b), None) => {
            format!("{head} unconfirmed (it ran {b} before the restart; asked for {chosen})")
        }
        (None, None) => format!("{head} unconfirmed (asked for {chosen})"),
    }
}

/// Whether the agent's LAST answer carries `marker` as a line of its own text
/// ([`is_ready_line`]). Only ASSISTANT entries are read: the announcement
/// itself (a user entry) contains the marker too, and must never count as the
/// answer. `jsonl` is the transcript's tail; a line that is not JSON is
/// skipped (the tail's first line may be cut).
///
/// THE LAST ONE, not any one (review of 2026-09-25): a READY answer is
/// consent to a restart only while it is still the agent's last word. Once
/// anything moved the agent past it — the continuation the window's
/// supervisor types once its hold on the notice's answer runs out, a
/// `keep going` while the owner held the upgrade, a person's message — the
/// agent answered again and went back to work, and the old
/// READY row, still in the tail, was read as consent to SIGTERM that work.
/// So the marker counts only in the session's newest assistant row: its own
/// turns, never a subagent's (`isSidechain`), and never the rows Claude Code
/// writes itself (`<synthetic>`, [`is_model_id`]).
///
/// AND ONLY AFTER THE LATEST NOTICE (review of 2026-09-25): a main-chain
/// user row that IS an announcement ([`ANNOUNCE_HEAD`]) ends any READY
/// before it. A notice typed after the owner's hold asks afresh, and until
/// the agent writes a real assistant row after it the pre-hold READY was
/// still the last assistant word — an API error Claude Code records as a
/// `<synthetic>` row, or a person's Esc (a USER row `[Request interrupted by
/// user]`), left it standing, and a SIGTERM followed a notice nobody
/// answered.
///
/// AND NEVER PAST A DIRECTION GIVEN AFTER IT (the second review of
/// 2026-09-26): a main-chain user row that DIRECTS the conversation
/// ([`is_direction`]: a person's words, an Esc, a peer's message, the
/// supervisor's continuation) ends the READY before it too, whether or not
/// the agent has written a real row since. A person's Esc, or a message
/// answered only by a `<synthetic>` row (an API error, the usage limit),
/// left the READY the agent's last assistant word, and an upgrade that had
/// given up asking restarted the session over the person who had just
/// spoken after the answer.
#[must_use]
pub fn transcript_has_ready(jsonl: &str, marker: &str) -> bool {
    let mut ready = false;
    for line in jsonl.lines() {
        match turn_of(line) {
            Some(Turn::Notice | Turn::Direction) => ready = false,
            Some(Turn::Agent(message)) => ready = answers(&message, marker),
            None => {}
        }
    }
    ready
}

/// Whether someone has DIRECTED the conversation since the agent last
/// answered the upgrade, and the agent has taken that direction up — so a
/// release the upgrade owes ([`release_prompt`]) would be typed over newer
/// direction (the review of 2026-09-26: a person's draft voided a READY, the
/// person then sent it, the agent worked on it, and the release typed at the
/// next idle point told it to go back to the work from before the notice).
/// `jsonl` is a transcript's tail, `markers` every READY marker the round
/// typed (`upgrade_drive::St::asked`).
///
/// SINCE THE AGENT'S LATEST READY (the second review of 2026-09-26): a
/// direction counts only after the agent's latest READY row answering one of
/// `markers` — with none after the latest notice ([`ANNOUNCE_HEAD`]), after
/// that notice. A direction BEFORE the READY is one the agent answered by
/// holding for the restart. Counted, a peer's message after the notice, then
/// the agent's READY, then a void of that READY dropped the release: nothing
/// restarted the agent, nothing released it, and under `[harness] continue =
/// false`, a hand-run sweep or a spent continuation budget nothing continued
/// it — the end state of the stall this module was fixed for.
///
/// AND ONLY ONCE THE AGENT HAS ANSWERED IT ([`Turn::Agent`]: a row of its own
/// model after the direction that is no READY): a direction the agent has not
/// answered yet is one it may still answer with READY, and a release dropped
/// at a look mid-turn left that READY answering a round nothing hears any
/// more. A direction answered by nothing the agent wrote — an Esc, a message
/// met by a `<synthetic>` row — leaves the release to be typed: the agent is
/// idle, and the line tells it to carry on, over nothing it took up.
///
/// A direction is a main-chain user turn that is words typed into the
/// conversation: a person's (a prompt, a `!` shell command, an Esc —
/// `[Request interrupted by user]`), a peer's (`[from s-…]`), or the
/// supervisor's continuation or answer. Not a direction: the harness's own
/// lines (`[aterm harness] …`), and every user row Claude Code writes on its
/// own, each measured in the owner's transcripts on 2026-09-26 — a tool's
/// result, an `isMeta` row (the limit's reset, a command's caveat, an image),
/// a compaction summary, a background task's notification
/// (`<task-notification>`), a command and its output (`<command-name>`,
/// `<local-command-stdout>`, `<bash-stdout>`: `/rate-limit-options` is one
/// Claude Code runs at a limit). With no notice in the tail the whole tail is
/// read: the notice is far behind, and any direction since is one.
#[must_use]
pub fn directed_since_ready(jsonl: &str, markers: &[String]) -> bool {
    let (mut told, mut directed) = (false, false);
    for line in jsonl.lines() {
        match turn_of(line) {
            Some(Turn::Notice) => (told, directed) = (false, false),
            Some(Turn::Direction) => told = true,
            Some(Turn::Agent(message)) => {
                if markers.iter().any(|m| answers(&message, m)) {
                    (told, directed) = (false, false);
                } else if told {
                    directed = true;
                }
            }
            None => {}
        }
    }
    directed
}

/// One transcript row as the upgrade reads it ([`turn_of`]).
enum Turn {
    /// A main-chain user row that IS the live upgrade's announcement
    /// ([`is_announcement`]).
    Notice,
    /// A main-chain user row that DIRECTS the conversation ([`is_direction`]).
    Direction,
    /// A main-chain assistant row the agent wrote itself — never a
    /// subagent's (`isSidechain`), never one of the rows Claude Code writes
    /// on its own (`<synthetic>`, [`is_model_id`]) — and its `message`.
    Agent(Value),
}

/// What one transcript `line` is to the upgrade ([`Turn`]); `None` for any
/// other row — a subagent's, a user row that directs nothing (a tool's
/// result, an `isMeta` row, a compaction summary, a tagged row Claude Code
/// writes itself, the harness's own lines), a `<synthetic>` assistant row —
/// and for a line that is not JSON (the tail's first line may be cut).
fn turn_of(line: &str) -> Option<Turn> {
    if !line.contains("\"user\"") && !line.contains("assistant") {
        return None;
    }
    let v = aterm_json::from_str::<Value>(line).ok()?;
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let message = v.get("message").cloned().unwrap_or_default();
            let theirs = message
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|m| !is_model_id(m));
            (!theirs).then_some(Turn::Agent(message))
        }
        Some("user") => {
            let message = v.get("message")?;
            if is_announcement(message) {
                return Some(Turn::Notice);
            }
            if v.get("isMeta").and_then(Value::as_bool) == Some(true)
                || v.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            {
                return None;
            }
            let directs = match message.get("content") {
                Some(Value::String(t)) => is_direction(t),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .any(is_direction),
                _ => false,
            };
            directs.then_some(Turn::Direction)
        }
        _ => None,
    }
}

/// Whether an assistant `message` answers with `marker`: a text part with the
/// marker on a line of its own ([`is_ready_line`]).
fn answers(message: &Value, marker: &str) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content.iter().any(|part| {
                part.get("type").and_then(Value::as_str) == Some("text")
                    && part
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.lines().any(|l| is_ready_line(l, marker)))
            })
        })
}

/// Whether one user row's words are a DIRECTION ([`directed_since_ready`]):
/// anything typed but the harness's own lines and the tagged rows Claude Code
/// writes itself — of those, only a person's shell command (`<bash-input>`)
/// is one.
fn is_direction(text: &str) -> bool {
    let t = text.trim_start();
    if t.is_empty() || t.starts_with("[aterm harness]") {
        return false;
    }
    !t.starts_with('<') || t.starts_with("<bash-input>")
}

/// Whether a transcript user `message` is the live upgrade's announcement:
/// its text — the whole content, or a text part of it — begins with
/// [`ANNOUNCE_HEAD`].
fn is_announcement(message: &Value) -> bool {
    let head = |t: &str| t.trim_start().starts_with(ANNOUNCE_HEAD);
    match message.get("content") {
        Some(Value::String(t)) => head(t),
        Some(Value::Array(parts)) => parts.iter().any(|part| {
            part.get("type").and_then(Value::as_str) == Some("text")
                && part.get("text").and_then(Value::as_str).is_some_and(head)
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------- the login wall

/// THE LOGIN WALL'S SIGNATURE in a transcript row, as Claude Code 2.1.281
/// writes it (measured 2026-09-27, the owner's session `03396a15…`: every turn
/// from 05:00:24 to 14:33:36 UTC ended on one, 45-86 ms after it began): a
/// main-chain assistant row of Claude Code's own (`<synthetic>`), flagged
/// `isApiErrorMessage`, whose `error` is `authentication_failed` — its text
/// `Login expired · Please run /login`, drawn `⏺ Login expired · …` on the
/// screen ([`login_wall`]).
fn is_auth_wall_row(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("assistant")
        && v.get("isSidechain").and_then(Value::as_bool) != Some(true)
        && v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        && v.get("error").and_then(Value::as_str) == Some("authentication_failed")
}

/// What one transcript row says of the login ([`login_rows`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoginRow {
    /// The wall ([`is_auth_wall_row`]).
    Wall,
    /// What lifts it: the vendor's own `Login successful` (a person's
    /// `/login` finished: `<local-command-stdout>Login successful…`), or a
    /// row of the session's own model — a turn the API answered.
    Lifted,
}

/// Whether `text` is the output of a `/login` that FINISHED, as Claude Code
/// writes it to the transcript: `<local-command-stdout>Login successful…`.
fn login_succeeded(text: &str) -> bool {
    text.trim_start()
        .starts_with("<local-command-stdout>Login successful")
}

/// Every row of `jsonl` (a transcript's tail) that says something of the
/// login, in order, with its time (unix seconds, `None` when it names none).
fn login_rows(jsonl: &str) -> impl Iterator<Item = (LoginRow, Option<u64>)> + '_ {
    jsonl.lines().filter_map(|line| {
        if !line.contains("authentication_failed")
            && !line.contains("Login successful")
            && !line.contains("\"assistant\"")
        {
            return None;
        }
        let v = aterm_json::from_str::<Value>(line).ok()?;
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(super::upgrade_models::parse_utc);
        if is_auth_wall_row(&v) {
            return Some((LoginRow::Wall, at));
        }
        let lifted = match v.get("type").and_then(Value::as_str) {
            Some("assistant") => v
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .is_some_and(is_model_id),
            _ => is_login_success_row(&v),
        };
        lifted.then_some((LoginRow::Lifted, at))
    })
}

/// Whether one transcript row is a person's `/login` that FINISHED
/// ([`login_succeeded`]). It comes in two shapes: 2.1.281 writes it as a
/// `user` row's `message.content` (the incident's session, 14:33:36 UTC);
/// 2.1.283 as a `system` row, `subtype` `local_command`, with a top-level
/// `content` (the owner's other session, 14:33:54 UTC the same day —
/// measured; read as neither, the wall stood until the model next answered).
fn is_login_success_row(v: &Value) -> bool {
    match v.get("type").and_then(Value::as_str) {
        Some("user") => v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .is_some_and(login_succeeded),
        Some("system") => {
            v.get("subtype").and_then(Value::as_str) == Some("local_command")
                && v.get("content")
                    .and_then(Value::as_str)
                    .is_some_and(login_succeeded)
        }
        _ => false,
    }
}

/// THE SESSION STANDS AT THE LOGIN WALL, by its transcript (`jsonl`, a
/// tail): the last row that says anything of the login is the wall — Claude
/// Code's `authentication_failed` row ([`is_auth_wall_row`]) — with neither
/// the person's `Login successful` nor an answer of the session's own model
/// after it. Read beside the screen ([`login_wall`]): the wall's row leaves
/// the screen when a `/login` dialog is dismissed, and the login is still
/// gone. A login finished in ANOTHER tab writes nothing here: this reads the
/// wall until something typed into this session is answered.
#[must_use]
pub fn transcript_login_wall(jsonl: &str) -> bool {
    login_rows(jsonl)
        .last()
        .is_some_and(|(row, _)| row == LoginRow::Wall)
}

/// When THE LAST LOGIN WALL in `jsonl` (a transcript's tail) was LIFTED
/// (unix seconds): the time of the first row after its last wall row that
/// lifts it ([`LoginRow::Lifted`]). `None` when the tail holds no wall, when
/// the wall still stands, or when the lifting row names no time. What holds
/// the upgrade's clocks through the wall ([`clock_held_until`]): a notice
/// the agent read before the login went — its wind-down turn the one the
/// wall answered — gets its whole window once the agent can answer again.
#[must_use]
pub fn login_lifted_at(jsonl: &str) -> Option<u64> {
    let (mut walled, mut lifted) = (false, None);
    for (row, at) in login_rows(jsonl) {
        match row {
            LoginRow::Wall => (walled, lifted) = (true, None),
            LoginRow::Lifted if walled => (walled, lifted) = (false, at),
            LoginRow::Lifted => {}
        }
    }
    lifted
}

/// Whether the LATEST NOTICE carrying `marker` NEVER REACHED THE MODEL: in
/// `jsonl` (a transcript's tail), the first main-chain assistant row after
/// the last announcement ([`ANNOUNCE_HEAD`]) whose words carry `marker` is
/// the login wall ([`is_auth_wall_row`]) — its own turn ended on
/// `authentication_failed` before the model said a word (the incident of
/// 2026-09-27: all four of the upgrade's notices were answered so, each in
/// under 90 ms). `false` when the model answered it, when nothing has
/// answered it yet, and when the tail holds no such notice. Claude Code's
/// other rows of its own (`No response requested.`) are passed over. A
/// notice the model read LATER, as history in a turn someone else began
/// (the owner's `continue` after the login), is still one that never reached
/// it as asked: what it answers there is read as any answer is
/// ([`transcript_has_ready`]). So is one Claude Code ASKS AGAIN ITSELF once
/// a `/login` lifts the wall — 2.1.283 did, measured 2026-09-27 in the
/// owner's other session: `Login successful`, then the model answering the
/// prompt the wall had answered, nothing typed (the binaries' `shouldQuery`
/// after an `authentication_failed` row; 2.1.281 carries the same branch and
/// did not take it in the incident). The model HAS read that notice; unless
/// its answer is READY, the notice is typed once more, as the same ask — one
/// prompt too many, no ask spent.
#[must_use]
pub fn notice_undelivered(jsonl: &str, marker: &str) -> bool {
    notice_fate(jsonl, marker) == Some(true)
}

/// [`notice_undelivered`], and `None` where `jsonl` holds no announcement
/// carrying `marker` at all — a tail too short to judge it by.
#[must_use]
pub fn notice_fate(jsonl: &str, marker: &str) -> Option<bool> {
    notice_fate_of(jsonl, marker).map(|fate| fate == NoticeFate::Wall)
}

/// What became of ONE NOTICE, by the transcript ([`notice_fate_of`]): whether
/// its own turn reached the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeFate {
    /// Nothing has answered its own turn yet (the turn is in flight).
    Open,
    /// The session's own model answered its own turn: it was read as asked.
    Answered,
    /// THE LOGIN WALL answered its own turn ([`is_auth_wall_row`]): it never
    /// reached the model ([`notice_undelivered`]).
    Wall,
    /// A USAGE LIMIT answered its own turn ([`is_limit_row`]) and the model
    /// has written nothing since: the notice waits in the conversation,
    /// unread, and reaches the model with the next turn that gets past the
    /// limit — Claude Code's own `Continuing automatically at …` at the
    /// reset, or anyone's message. NOT TAKEN: it is no ask the agent could
    /// answer, and a second notice typed meanwhile only queues behind it
    /// (the owner's report of 2026-09-27: four of them, delivered at once).
    Queued,
    /// A usage limit answered it, and the model TOOK it later, at `.0` (unix
    /// seconds; `None` when the row names no time) — the first row of the
    /// session's own model after it: as history in a turn the reset or a
    /// person began, together with every notice queued beside it. Its
    /// window opens then (the driver holds the clock until that row).
    Taken(Option<u64>),
}

/// THE FATE of the latest notice carrying `marker` in `jsonl` (a transcript's
/// tail): the first decisive main-chain assistant row after it — the login
/// wall's, a usage limit's (after which the model's first row is the
/// notice TAKEN), or the model's own. Claude Code's other rows of its own
/// (`No response requested.`, an API error) are passed over, as a subagent's
/// rows are. `None` where the tail holds no such notice.
#[must_use]
pub fn notice_fate_of(jsonl: &str, marker: &str) -> Option<NoticeFate> {
    notice_scan(jsonl, marker).map(|scan| scan.fate)
}

/// UNTIL WHEN A NOTICE QUEUED BEHIND A USAGE LIMIT WAITS ON IT (review of
/// 2026-09-27: a queued notice held the upgrade `limited` with no bound, and
/// a session simply left idle past its reset — the weekly limit continues on
/// its own only after `/rate-limit-options`, and a person's Esc cancels even
/// that — or answered by a limit that names no reset, waited until a person
/// typed): the latest notice carrying `marker` is [`NoticeFate::Queued`] in
/// `jsonl`, and the limit row that last answered it names its reset
/// (`quotaLimits.resetsAt`, unix seconds) — or, naming none (an API rate
/// limit, a model's own bucket), holds for [`REASK_S`] from when it was
/// written. `Some(0)`: a person's `/login` finished after that row (an
/// account switched: the owner's session of 2026-09-27 went on so), or the
/// row names neither — the limit is over by the transcript's word, and the
/// screen alone says whether one stands. `None`: the notice is not queued.
/// Past it, a notice still queued never reached the model: the driver types
/// it again as the same ask ([`Facts::undelivered`]), where the screen
/// shows no limit — straight away while the conversation holds at most
/// [`REQUEUE_MAX`] upgrade notices untaken ([`queued_copies`]), past that
/// once a rest has run ([`Scan::rests_until`]).
#[must_use]
pub fn queued_until(jsonl: &str, marker: &str) -> Option<u64> {
    notice_scan(jsonl, marker)?.queued_until()
}

/// HOW MANY UPGRADE NOTICES THE CONVERSATION HOLDS UNTAKEN while the latest
/// notice carrying `marker` is [`NoticeFate::Queued`] in `jsonl`: every
/// notice the upgrade typed since the session's own model last wrote a row
/// — this one's copies and any other's (an earlier ask's, an earlier round's,
/// one typed for another target, an older build's), whatever answered each
/// (a usage limit, the login wall) — all of which reach the agent together
/// once a turn gets past the limit (the owner's report of 2026-09-27: four
/// of them at once). `0`: the notice is not queued. What bounds typing it
/// again ([`REQUEUE_MAX`]); only a row of the session's own model starts the
/// count again (a `/login` does not: the notices stay in the conversation).
#[must_use]
pub fn queued_copies(jsonl: &str, marker: &str) -> u32 {
    notice_scan(jsonl, marker).map_or(0, |scan| scan.queued_copies())
}

/// What [`notice_scan`] found of the latest notice carrying a marker, and of
/// the conversation it waits in — one read of the tail for all of it
/// ([`notice_fate_of`], [`queued_until`], [`queued_copies`],
/// [`Scan::rests_until`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scan {
    /// Its fate ([`notice_fate_of`]).
    pub fate: NoticeFate,
    /// While it is queued, until when the limit that answered it holds by
    /// the transcript's word ([`queued_until`]; `None` here: the row named
    /// nothing to go by, or a `/login` finished since).
    pub until: Option<u64>,
    /// The upgrade notices the conversation holds that the session's model
    /// has not taken — typed since its last row of its own, this one
    /// included, whatever their marker and whatever answered each
    /// ([`queued_copies`] reads it while the notice is queued).
    pub untaken: u32,
    /// When the limit row that last answered it was written (unix seconds;
    /// `None`: not queued, or the row names no time).
    pub answered_at: Option<u64>,
    /// When it was typed (unix seconds; `None`: its row names no time).
    pub typed_at: Option<u64>,
}

impl Scan {
    /// [`queued_until`] of this scan.
    #[must_use]
    pub fn queued_until(&self) -> Option<u64> {
        (self.fate == NoticeFate::Queued).then(|| self.until.unwrap_or(0))
    }

    /// [`queued_copies`] of this scan.
    #[must_use]
    pub fn queued_copies(&self) -> u32 {
        if self.fate == NoticeFate::Queued {
            self.untaken
        } else {
            0
        }
    }

    /// UNTIL WHEN A FULL QUEUE RESTS (review of 2026-09-27): once the
    /// conversation holds more than [`REQUEUE_MAX`] upgrade notices untaken
    /// ([`queued_copies`]) and the limit that answered the latest is over by
    /// the transcript's word, that notice is typed once more by itself only
    /// [`queue_rest`] after the word ran out — the reset its row named, or
    /// [`REASK_S`] after the row where it named none or a `/login` ended it.
    /// Held for good instead, an idle session whose limit had long ended
    /// waited for a person to type, and the owner was told it "moves once
    /// that ends". The rest GROWS with every copy the model has not read —
    /// [`RETRY_S`], then twice, four and eight times that, then a day — so a
    /// limit that names no reset and lasts for days adds a handful of copies
    /// and then one a day, never one every two and a half hours for as long
    /// as it stands. `None`: not queued, or the limit row names no time —
    /// nothing to count a rest from; the queue then waits for the session to
    /// go on or the owner's word.
    #[must_use]
    pub fn rests_until(&self) -> Option<u64> {
        if self.fate != NoticeFate::Queued {
            return None;
        }
        let at = self.answered_at?;
        Some(
            self.until
                .unwrap_or(0)
                .max(at.saturating_add(REASK_S))
                .saturating_add(queue_rest(self.untaken)),
        )
    }
}

/// THE LONGEST A FULL QUEUE RESTS before one more copy: a day. What bounds
/// the copies a usage limit that lasts for days adds, once the rest has
/// grown to it ([`queue_rest`]): one a day.
pub const QUEUE_REST_MAX_S: u64 = 24 * 3_600;

/// How many times a full queue's rest doubles before it reaches
/// [`QUEUE_REST_MAX_S`]: the copies that go after a rest shorter than a day,
/// at most, while the model reads none of them ([`queue_rest`]). Four today:
/// two, four, eight and sixteen hours. The derived model's `Daily`
/// (`aterm_spec::derive::harness_upgrade_limit_queue_model`).
pub const QUEUE_REST_DOUBLINGS: u32 = {
    let mut k = 0;
    while RETRY_S << k < QUEUE_REST_MAX_S {
        k += 1;
    }
    k
};

/// HOW LONG A FULL QUEUE OF `untaken` NOTICES RESTS before the upgrade types
/// one more copy by itself ([`Scan::rests_until`]; the owner, 2026-09-27: a
/// limit whose row names no reset still added a copy every `REASK_S +
/// RETRY_S` — two and a half hours, about ten a day — for as long as it
/// lasted). The rest doubles with every copy the model has not read beyond
/// the `1 + REQUEUE_MAX` a full queue holds: [`RETRY_S`] for the first,
/// then twice, four and eight times that, then [`QUEUE_REST_MAX_S`] for
/// every one after. Every copy the limit answered again is its own word that
/// it still stands, so each one more is worth less; and a model's row of its
/// own starts the count again ([`queued_copies`]), so the next limit's queue
/// rests two hours again. The owner's `Upgrade now` still types one more at
/// once, whatever the rest.
#[must_use]
pub fn queue_rest(untaken: u32) -> u64 {
    let over = untaken.saturating_sub(REQUEUE_MAX + 1);
    if over >= QUEUE_REST_DOUBLINGS {
        QUEUE_REST_MAX_S
    } else {
        (RETRY_S << over).min(QUEUE_REST_MAX_S)
    }
}

/// [`notice_fate_of`], with — while the notice is queued — until when the
/// limit that answered it holds, when that limit's row was written, how many
/// upgrade notices wait untaken, and when the notice was typed: one read of
/// `jsonl` for every question the driver asks of it (`queue_facts`).
#[must_use]
pub fn notice_scan(jsonl: &str, marker: &str) -> Option<Scan> {
    let (mut armed, mut fate, mut until) = (false, None, None);
    let (mut answered_at, mut typed_at, mut untaken) = (None, None, 0_u32);
    for line in jsonl.lines() {
        if !line.contains("\"user\"")
            && !line.contains("assistant")
            && !line.contains("Login successful")
        {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if armed && fate == Some(NoticeFate::Queued) && is_login_success_row(&v) {
            until = None;
            continue;
        }
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                if v.get("message").is_some_and(is_announcement) {
                    // One more notice in the conversation the model has not
                    // taken, whatever it carries.
                    untaken = untaken.saturating_add(1);
                    if line.contains(marker) {
                        (armed, fate, until, answered_at) =
                            (true, Some(NoticeFate::Open), None, None);
                        typed_at = row_time(&v);
                    }
                }
            }
            Some("assistant") => {
                let own = v
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .is_some_and(is_model_id);
                if armed {
                    if is_auth_wall_row(&v) {
                        (armed, fate, until) = (false, Some(NoticeFate::Wall), None);
                    } else if is_limit_row(&v) {
                        fate = Some(NoticeFate::Queued);
                        until = limit_holds_until(&v);
                        answered_at = row_time(&v);
                    } else if own {
                        armed = false;
                        until = None;
                        fate = Some(if fate == Some(NoticeFate::Queued) {
                            NoticeFate::Taken(row_time(&v))
                        } else {
                            NoticeFate::Answered
                        });
                    }
                }
                // The model's own row: every notice before it was taken.
                if own {
                    untaken = 0;
                }
            }
            _ => {}
        }
    }
    fate.map(|fate| Scan {
        fate,
        until,
        untaken,
        answered_at,
        typed_at,
    })
}

/// A transcript row's `timestamp`, in unix seconds.
fn row_time(v: &Value) -> Option<u64> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(super::upgrade_models::parse_utc)
}

/// Until when one limit row ([`is_limit_row`]) holds the session: the reset
/// it names (`quotaLimits.resetsAt`), else [`REASK_S`] after it was written,
/// else `None`.
fn limit_holds_until(v: &Value) -> Option<u64> {
    v.get("quotaLimits")
        .and_then(|q| q.get("resetsAt"))
        .and_then(Value::as_u64)
        .or_else(|| row_time(v).map(|at| at.saturating_add(REASK_S)))
}

/// THE USAGE LIMIT'S SIGNATURE in a transcript row, as Claude Code writes it
/// (measured 2026-09-27 in the owner's sessions `25e3b26e…` and
/// `77eb4f89…`, Claude Code 2.1.280 and 2.1.281: each of the eight notices
/// 0.93.0 typed into them at the weekly limit was answered within two
/// seconds by one): a main-chain `<synthetic>` assistant row flagged
/// `isApiErrorMessage`, whose `error` is `rate_limit` — its text `You've hit
/// your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)` — or whose
/// text the supervisor's own wall table reads as a limit
/// ([`aterm_phase::classify_wall`], [`aterm_phase::WallKind::reads_limited`]:
/// a session, weekly, model-bucket, spend or API rate limit), never a
/// second pattern of the upgrade's own.
fn is_limit_row(v: &Value) -> bool {
    if v.get("type").and_then(Value::as_str) != Some("assistant")
        || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || v.get("isApiErrorMessage").and_then(Value::as_bool) != Some(true)
    {
        return false;
    }
    if v.get("error").and_then(Value::as_str) == Some("rate_limit") {
        return true;
    }
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts.iter().any(|p| {
                p.get("text")
                    .and_then(Value::as_str)
                    .and_then(aterm_phase::classify_wall)
                    .is_some_and(|kind| kind.reads_limited())
            })
        })
}

/// UNTIL WHEN THE SESSION STANDS AT A USAGE LIMIT, by its transcript (`jsonl`,
/// a tail): the last main-chain row that says whether the model can answer —
/// a row of the session's own model, or Claude Code's limit row
/// ([`is_limit_row`]) — is the limit's, and it names the instant the limit
/// resets (`quotaLimits.resetsAt`, unix seconds: `1791046800`, Oct 3 10:00
/// PDT, on every limit row of the owner's session of 2026-09-27) — or, naming
/// none (an API rate limit: `API Error: Rate limit reached`), [`REASK_S`]
/// after it was written, as a notice queued behind such a row waits
/// ([`queued_until`]; review of 2026-09-27: read as no limit at all, a
/// pending upgrade typed a fresh notice minutes after one, behind a notice
/// of an earlier round still queued). `None`: the model answered last, the
/// tail holds no limit row, or the row names neither a reset nor a time.
/// What keeps even the FIRST notice out of a session whose limit banner has
/// left the screen — a person's Esc on `Continuing automatically at …`, a
/// screen the recogniser misses ([`Facts::limited`] until then); past that
/// instant the session can take a turn again.
///
/// A person's `/login` that FINISHED after the limit row
/// ([`is_login_success_row`]) ends it too (review of 2026-09-27: in the
/// owner's session the weekly row, reset Oct 3, was followed by `Login
/// successful` at 19:57:47Z — an account switched — and a pending upgrade
/// held `limited` for days, which no `--now` waives): the transcript no
/// longer knows of a limit, and the screen judges, as
/// [`login_lifted_at`] leaves it for the login wall.
#[must_use]
pub fn transcript_limit_until(jsonl: &str) -> Option<u64> {
    let mut until = None;
    for line in jsonl.lines() {
        if !line.contains("assistant") && !line.contains("Login successful") {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if is_login_success_row(&v) {
            until = None;
            continue;
        }
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if is_limit_row(&v) {
            until = limit_holds_until(&v);
        } else if v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .is_some_and(is_model_id)
        {
            until = None;
        }
    }
    until
}

/// What a transcript's tail says of the login to an upgrade ([`transcript_login`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptLogin {
    /// The wall stands ([`transcript_login_wall`]): [`Facts::login`].
    pub stands: bool,
    /// When the last wall was lifted ([`login_lifted_at`]): the clocks start
    /// again from it ([`clock_held_until`]).
    pub lifted_at: Option<u64>,
    /// Whether the latest notice never reached the model
    /// ([`notice_fate`]), for a phase that asks it — an announced upgrade,
    /// and one that gave up; `None` where the tail holds no such notice (or
    /// the phase asks nothing): [`Facts::undelivered`] reads it `false`, and
    /// the driver looks further back for a give-up's.
    pub undelivered: Option<bool>,
}

/// THE TRANSCRIPT'S WORD ON THE LOGIN for an upgrade in `phase` whose latest
/// notice carries `marker`, from `tail` — the one fold the driver makes of
/// it (`upgrade_drive::login_facts`) and the conformance walk binds.
#[must_use]
pub fn transcript_login(phase: &Phase, marker: &str, tail: &str) -> TranscriptLogin {
    let asks = match phase {
        Phase::Announced { .. } => true,
        Phase::Failed(why) => why == GAVE_UP,
        _ => false,
    };
    TranscriptLogin {
        stands: transcript_login_wall(tail),
        lifted_at: login_lifted_at(tail),
        undelivered: (asks && !marker.is_empty())
            .then(|| notice_fate(tail, marker))
            .flatten(),
    }
}

/// THE LOGIN FACT ([`Facts::login`]) of a screen of `agent`'s, by the ONE
/// recogniser the supervisor reads its walls with ([`aterm_phase::wall`]):
/// the turn ended on the auth wall — Claude Code's `⏺ Login expired · Please
/// run /login` error row, `Not logged in · …` under the gutter; Codex's
/// login notice ([`aterm_phase::codex::wall`]).
#[must_use]
pub fn login_wall(agent: Agent, rows: &[String]) -> bool {
    let wall = match agent {
        Agent::Claude => aterm_phase::wall(rows),
        Agent::Codex => aterm_phase::codex::wall(rows),
    };
    wall.is_some_and(|w| w.kind == aterm_phase::WallKind::Auth)
}

/// The ask number of the next notice ([`Step::Announce`]) of an upgrade in
/// `phase`: one more than the asks it has made — except where its last
/// notice NEVER REACHED THE MODEL (`undelivered`: the login wall's,
/// [`notice_undelivered`], or one a usage limit answered and that limit is
/// over, [`queued_until`]), which spent no ask: typed again as the same ask
/// (and so with the same READY marker).
#[must_use]
pub fn announce_asks(phase: &Phase, undelivered: bool) -> u32 {
    match phase {
        Phase::Announced { asks, .. } if undelivered => (*asks).max(1),
        Phase::Announced { asks, .. } => asks.saturating_add(1),
        _ => 1,
    }
}

/// Every READY marker of the round an older build left with only its latest
/// on record (0.93.0's state keeps `marker` alone): each ask's, minted as its
/// notice minted it ([`ready_marker`] over the round's `salt` and the ask —
/// the ledger of 2026-09-27 names exactly these four).
#[must_use]
pub fn round_markers(session_id: &str, to: &Version, salt: u64) -> Vec<String> {
    (1..=MAX_ASKS)
        .map(|asks| ready_marker(session_id, to, salt.wrapping_add(u64::from(asks))))
        .collect()
}

/// How many of the round's notices (`markers`) REACHED THE MODEL, by
/// `jsonl`: every one whose own turn was not the login wall's
/// ([`notice_fate`]) — and one the text no longer holds, which cannot be
/// shown unread.
#[must_use]
pub fn notices_received(jsonl: &str, markers: &[String]) -> u32 {
    let received = markers
        .iter()
        .filter(|m| notice_fate(jsonl, m) != Some(true))
        .count();
    u32::try_from(received).unwrap_or(u32::MAX)
}

/// A GIVE-UP SPENT ON NOTICES THAT NEVER REACHED THE MODEL IS TAKEN BACK (the
/// incident of 2026-09-27: 0.93.0 typed all four into the login wall and gave
/// up at 07:03): an upgrade that gave up ([`GAVE_UP`]) whose last notice was
/// the wall's (`undelivered`) is asked about as the upgrade it would have been
/// had the unread notices spent no ask — pending when none of them reached
/// the model (`received`, [`notices_received`]), else announced with the
/// asks that did, its window run out — so its next notice is the next ask
/// the model has not had, and it gives up again only after [`MAX_ASKS`]
/// notices the model received. `None`: nothing to take back — any other
/// phase, a last notice the model received, or every notice received.
#[must_use]
pub fn rearmed(phase: &Phase, undelivered: bool, received: u32) -> Option<Phase> {
    match phase {
        Phase::Failed(why) if why == GAVE_UP && undelivered && received < MAX_ASKS => {
            Some(if received == 0 {
                Phase::Pending
            } else {
                Phase::Announced {
                    at_s: 0,
                    asks: received,
                }
            })
        }
        _ => None,
    }
}

/// THE HARNESS'S MARK: what every turn the harness types into an agent's
/// conversation begins with — the upgrade's notice ([`ANNOUNCE_HEAD`]), its
/// continuation ([`continue_prompt`]), a relaunch's (`relaunch::
/// resumed_prompt`) and the Codex branch's. What tells the harness's own turns
/// from anyone else's in the conversation's record ([`TaskScan`]).
pub const HARNESS_MARK: &str = harness_mark!();

/// WHETHER A CONVERSATION HAS A TASK (D1 of the live E2E of 2026-09-26): a
/// conversation nobody has asked anything — no turn from a PERSON or an
/// ORCHESTRATOR (a control-socket client other than the harness) — is
/// TASKLESS, however many turns the harness itself typed into it. Folded over
/// the conversation's transcript, one line at a time ([`Self::line`]), oldest
/// first: the record is appended to, so a caller can read on from where it
/// stopped (`upgrade_drive::Tasked`).
///
/// A conversation has a task from the first PROMPT of someone else's: a
/// main-chain user row whose words are not the harness's ([`HARNESS_MARK`]) —
/// recorded as it is submitted, before the agent answers it, so a restart
/// that re-reads the record just before its signal never ends a first prompt
/// in flight (the review of 2026-09-26: counting only an ANSWERED prompt let
/// an orchestrator's `send` + Enter that had not been answered yet read
/// taskless). A command of someone else's (`<command-name>…`) is a task once
/// it starts a turn (a main-chain assistant row after it, a `<synthetic>`
/// one too), none once its local output says it started none (`/model`
/// leaves no task), and counts as one while neither has been read. Not a
/// prompt: a subagent's row (`isSidechain`), Claude Code's own expansion of
/// a command or a skill (`isMeta`), a tool's result, a local command's
/// output (`<local-command-…>`) and Esc's note (`[Request interrupted by
/// user…`). Any other user row counts as someone's — an unknown shape reads
/// as a task, the old behaviour, never as none.
///
/// THE HARNESS'S OWN TURNS are of two kinds, and neither is a task: the ones
/// the upgrade and the relaunch type, which begin with [`HARNESS_MARK`], and
/// the ones the supervisor's turn-end policy types — `keep going`,
/// `answer_text`, an accepted suggestion, a wall's retry — whose words are
/// the owner's and carry no mark, but which its loop's own ledger records as
/// it types them ([`Self::supervisor_typed`]:
/// `crate::supervise::approvals::typed_texts`). The policy types those only
/// where a task already stands, except where nobody could say (a record
/// that could not be read yet, a Codex, a conversation not registered yet):
/// there one `keep going` read as someone's would make the conversation
/// tasked for good (the review of 2026-09-26).
///
/// WHY THE TEXT AND NOT THE VENDOR'S FIELDS (measured on that E2E's
/// transcripts, Claude Code 2.1.281 and 2.1.283): the harness's notice, its
/// carry-on, the supervisor's `keep going` and a tester's prompt typed over
/// the control socket all read `"promptSource":"typed"`, `"origin":
/// {"kind":"human"}` — every one of them came through the terminal as typed
/// input, so the vendor cannot tell them apart. The mark and the ledger are
/// the harness's own record of what it typed — the mark written into the
/// conversation's own record, where it survives every restart and resume;
/// the screen (the launch card) is lost as soon as anything is typed, and a
/// resumed conversation's screen shows its history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskScan {
    /// A command of someone else's waits: the turn it starts, or the local
    /// output that says it started none. Read as a task meanwhile.
    command: bool,
    /// Someone else's prompt, or a command that started a turn, was read:
    /// the conversation has a task, for good.
    tasked: bool,
    /// What the session's supervisor typed into it, from its ledger: a row
    /// with the same words is its, not someone else's.
    ours: Vec<String>,
}

impl TaskScan {
    /// Read on knowing `ours`: the texts the session's supervisor typed
    /// into it ([`TaskScan`]'s second kind of the harness's own turns).
    pub fn supervisor_typed(&mut self, ours: &[String]) {
        ours.clone_into(&mut self.ours);
    }

    /// Fold one transcript line in; whether the conversation has a task now
    /// ([`Self::tasked`]). A line that is not a JSON row (a last line caught
    /// half-written) is skipped.
    pub fn line(&mut self, line: &str) -> bool {
        if self.tasked || !(line.contains("\"user\"") || line.contains("\"assistant\"")) {
            return self.tasked();
        }
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            return self.tasked();
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return self.tasked();
        }
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => self.tasked = self.command,
            Some("user") => match prompt_of(&v, &self.ours) {
                Prompt::Theirs => self.tasked = true,
                Prompt::Command => self.command = true,
                Prompt::Harness | Prompt::LocalOutput => self.command = false,
                Prompt::None => {}
            },
            _ => {}
        }
        self.tasked()
    }

    /// Whether the conversation has a task: someone else's prompt was read,
    /// or a command of theirs that has not said it started no turn.
    #[must_use]
    pub fn tasked(&self) -> bool {
        self.tasked || self.command
    }

    /// Whether it has one FOR GOOD: nothing read later can take it away, so
    /// nothing more need be read.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.tasked
    }
}

/// [`TaskScan`] over a whole transcript, its session's supervisor having
/// typed `ours` into it.
#[must_use]
pub fn transcript_tasked(jsonl: &str, ours: &[String]) -> bool {
    let mut scan = TaskScan::default();
    scan.supervisor_typed(ours);
    for line in jsonl.lines() {
        if scan.line(line) && scan.settled() {
            break;
        }
    }
    scan.tasked()
}

/// Whose prompt a main-chain user row is ([`TaskScan`]).
enum Prompt {
    /// The harness's own turn ([`HARNESS_MARK`]).
    Harness,
    /// A person's or an orchestrator's.
    Theirs,
    /// A command of theirs (`<command-name>…`): a turn only if it starts one.
    Command,
    /// A local command's output: the command before it started no turn.
    LocalOutput,
    /// No prompt at all: an expansion, a tool's result, Claude Code's own note.
    None,
}

fn prompt_of(row: &Value, ours: &[String]) -> Prompt {
    if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return Prompt::None;
    }
    let content = row.get("message").and_then(|m| m.get("content"));
    let text = match content {
        Some(Value::String(t)) => Some(t.as_str()),
        Some(Value::Array(parts)) => {
            let mut texts = parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str));
            match texts.next() {
                Some(t) => Some(t),
                // Every part is a tool's result (or an image): the agent's
                // own loop, no prompt.
                None if !parts.is_empty()
                    && parts
                        .iter()
                        .all(|p| p.get("type").and_then(Value::as_str) == Some("tool_result")) =>
                {
                    return Prompt::None;
                }
                None => None,
            }
        }
        _ => None,
    };
    let Some(text) = text.map(str::trim_start) else {
        return Prompt::Theirs;
    };
    let supervisors = |said: &str| ours.iter().any(|o| same_words(o, said));
    if text.starts_with(HARNESS_MARK) {
        Prompt::Harness
    } else if text.starts_with("<local-command-") {
        Prompt::LocalOutput
    } else if text.starts_with("<command-name>") || text.starts_with("<command-message>") {
        if supervisors(&command_line(text)) {
            Prompt::Harness
        } else {
            Prompt::Command
        }
    } else if text.starts_with("[Request interrupted by user") {
        Prompt::None
    } else if supervisors(text) {
        Prompt::Harness
    } else {
        Prompt::Theirs
    }
}

/// Whether two texts are the same words, however spaced or wrapped.
fn same_words(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

/// The command a command row records, as it was typed: `/<name> <args>`
/// from Claude Code's `<command-name>/<name></command-name>` and
/// `<command-args><args></command-args>`.
fn command_line(row: &str) -> String {
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        row.split_once(&open)
            .and_then(|(_, rest)| rest.split_once(&close))
            .map_or("", |(inner, _)| inner.trim())
    };
    format!("{} {}", tag("command-name"), tag("command-args"))
        .trim()
        .to_string()
}

/// What the driver measured about one session, for the gates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Claude's own `status` (the session file).
    pub status: String,
    /// Seconds since Claude last changed `status`.
    pub status_age_s: u64,
    /// The composer is present and holds no typed text (a dim placeholder
    /// suggestion is not typed text — [`composer_is_empty`]).
    pub composer_empty: bool,
    /// An approval or question box is up.
    pub approval_box: bool,
    /// The busy footer (`esc to interrupt`) is on screen.
    pub busy_footer: bool,
    /// Executable basenames of the agent's descendants that are background
    /// WORK — shells and `caffeinate` (MCP servers are not: resume restarts them;
    /// nor is the `caffeinate` the agent itself starts every turn to keep the Mac
    /// awake, its direct child: `upgrade_drive::background`).
    pub background: Vec<String>,
    /// aterm's hold on the session (a halt, or a driver's custody or lease).
    pub held: bool,
    /// Seconds the screen has been unchanged (no output, no keystroke echo).
    pub quiet_s: u64,
    /// Seconds a person's hold ([`person_hold`]) has stood, every sample of it
    /// the driver took showing one, none more than [`HOLD_GAP_S`] apart
    /// ([`hold_since`]; 0: none now).
    pub hold_s: u64,
    /// The owner's `aterm harness upgrade <sid> --now` is in force for this
    /// tab ([`Request::Now`], applied by [`requested_step`]): it waives the
    /// settling window ([`QUIET_S`]) and the attended-tab guard
    /// ([`Self::attended`]) — both exist so a person reading the answer is not
    /// typed over, and this is that person asking — and nothing else: Claude
    /// idle, an empty composer, no box, no busy footer and no hold are still
    /// asked of the screen as it is now, and the READY answer and an empty
    /// process tree of the restart ([`gate_restart`]).
    pub owner_now: bool,
    /// A person is at the tab: a PERSON gave THIS session input within
    /// `[harness] human_grace_s`, by the server's per-session person stamp
    /// ([`attended_by`] over `text --json`'s `"human_ms"`, `status
    /// human_ms=`) — the one presence fact the harness has, and the one grace
    /// the supervisor's question policy and relaunch read too. Not the tab's
    /// placement and not the machine's input clock (review of 2026-09-25):
    /// `wfocus` is never cleared on blur, and the OS-wide HID idle answers "is
    /// anyone at this Mac", so an owner working in a browser all afternoon
    /// held the front tab's upgrade back while that same tab's stamp read
    /// hours. Nothing the upgrade decides for itself is typed into, or
    /// signalled in, such a tab ([`gate_announce`], [`gate_restart`]) unless
    /// the owner's `--now` is in force ([`Self::owner_now`]). It is the gate's
    /// FIRST wait after a hold, ahead of Claude's status, so a busy attended
    /// tab waits `attended` too, and the upgrade owns none of its turn ends.
    pub attended: bool,
    /// The step is taken at A BREAK OF THE AGENT'S OWN BACKGROUND WORK (the
    /// owner's answer of 2026-09-26: "Busy agentic sessions get upgraded at
    /// their next natural break. The notice interrupts the agent's
    /// orchestration once, and the restart still never kills running work"):
    /// its turn is over and all that runs is work it started — a dynamic
    /// workflow, a background agent, a shell, a Codex background terminal —
    /// as the session's own loop read it and saw it stand [`QUIET_S`]
    /// (`supervise`'s `BACKGROUND_SETTLE`). There, and only there, Claude's
    /// own `busy`/`shell` status is no wait for a NOTICE: the first one, or a
    /// re-ask once [`REASK_S`] has passed since the last. The loop's settle
    /// stands for the screen's, since a workflow's progress line never holds
    /// still. The restart is an idle point's ([`next_step`]: `background`);
    /// a READY a person held past the drain is voided here as there.
    pub background_point: bool,
    /// THE CONVERSATION HAS NO TASK ([`TaskScan`]: nobody but the harness has
    /// asked it anything): there is nothing to preserve and no answer a
    /// person reads, so no notice is typed and nothing is resumed — the agent
    /// is restarted onto the new build AFRESH ([`Step::Fresh`]), once idle
    /// the full [`QUIET_S`] as any step; a person at the tab, a hold, a box,
    /// a draft and running work still wait (D1 of the live E2E of
    /// 2026-09-26: the notice started a conversation nobody asked for, and a
    /// tester's first prompt met `ERR busy`).
    pub taskless: bool,
    /// THE SESSION STANDS AT A USAGE OR RATE LIMIT ([`limited`], read from
    /// the same screen every other fact here is read from): what is typed
    /// now is queued behind the limit, or answered by an error, never read.
    /// Measured 2026-09-25/26 (tab `s-d3346b29dd236432b852`): Claude Code
    /// 2.1.280 parked a session at `⚠ Usage limit reached · continuing
    /// automatically at 6am · esc to cancel`; its status read `idle`, the
    /// composer empty, no box — so four notices were typed at half-hour
    /// intervals, the upgrade gave up two hours later, and at 06:00 all four
    /// were delivered at once to an agent the upgrade had stopped listening
    /// to. Nothing is typed, and no clock of the upgrade runs, while this
    /// stands ([`gate_announce`], [`next_step`], [`clock_held`]); no
    /// person's `--now` waives it — a person cannot make the notice read.
    ///
    /// The transcript says it too (the owner's report of 2026-09-27): a
    /// latest notice whose own turn the limit answered and that the model
    /// has not taken since ([`NoticeFate::Queued`], read by the driver) is a
    /// session at its limit whatever the screen shows — the banner a
    /// person's Esc dismissed, a screen the recogniser misses — so no second
    /// notice is typed behind the first, and its window does not run. Until
    /// that limit is over by the transcript's word ([`queued_until`]); past
    /// it, the notice is [`Facts::undelivered`] — or, the conversation's
    /// queue full, [`Facts::queued`].
    pub limited: bool,
    /// THE SESSION STANDS AT THE LOGIN WALL: its screen shows the auth wall
    /// ([`login_wall`], the supervisor's own recogniser), or its transcript's
    /// last word on the login is Claude Code's `authentication_failed` row
    /// ([`transcript_login_wall`]). Measured 2026-09-27 (tab
    /// `s-b5cf2faabac5ce5127bd`, Claude Code 2.1.281): the login expired, and
    /// every turn after it — the supervisor's `continue`s and four notices
    /// half an hour apart — was answered in under 90 ms by `Login expired ·
    /// Please run /login`; the upgrade gave up at 07:03, and the READY the
    /// agent gave once the owner logged in at 14:33 answered an upgrade that
    /// had stopped. A wait like the limit's ([`gate_announce`], [`next_step`],
    /// [`clock_held`]), which a person's `/login` lifts and no `--now` waives.
    pub login: bool,
    /// THE LATEST NOTICE NEVER REACHED THE MODEL: its own turn ended on the
    /// login wall ([`notice_undelivered`]). It spent no ask: [`next_step`]
    /// types it again, as the same ask ([`announce_asks`]), at the first
    /// point the notice could be typed — never waits out its window for it,
    /// and never gives up on it. (A give-up spent on it is taken back before
    /// the reducer is asked: [`rearmed`].)
    ///
    /// The same for a notice a USAGE LIMIT answered whose limit is over by the
    /// transcript's word ([`queued_until`]: the reset its row names has
    /// passed, a `/login` finished since, or — a row naming no reset —
    /// [`REASK_S`] ran) while the screen shows none (review of 2026-09-27):
    /// a session left idle past its reset never took the notice, and it is
    /// typed again as the same ask — never behind a limit that stands:
    /// straight away while the conversation holds at most [`REQUEUE_MAX`]
    /// upgrade notices the model has not taken ([`queued_copies`]), past that
    /// only once the queue has rested or on the owner's word
    /// ([`Facts::queued`] until then).
    pub undelivered: bool,
    /// THE CONVERSATION'S QUEUE IS FULL (review of 2026-09-27): its latest
    /// upgrade notice was answered by a usage limit that is over by the
    /// transcript's word and the screen's, but more than [`REQUEUE_MAX`]
    /// notices already wait in the conversation untaken ([`queued_copies`]),
    /// and the rest before one more ([`Scan::rests_until`]: [`queue_rest`]
    /// after the limit's word ran out, longer for every copy) has not run, nor has the owner's
    /// `--now` come since the latest was typed. Every copy the limit answered
    /// is its own word that it still stood, so nothing is typed behind them
    /// ([`gate_announce`]: `queued`) and no clock runs ([`clock_held`]) —
    /// until a turn gets past the limit and the model takes them all
    /// (anyone's message, Claude Code's own at a reset), the rest runs out
    /// (one copy more), or the owner presses `Upgrade now` (one copy more:
    /// that is the person asking). Held with no end instead, an idle session
    /// whose limit had long ended waited for good, `Upgrade now` moved
    /// nothing, and the owner was told it "moves once that ends".
    pub queued: bool,
    /// Seconds the READY answer the upgrade acts on has stood unacted on,
    /// the driver's stamp ([`ready_since`]; 0: no such answer now, or one
    /// first heard at this very look) — held, like every clock of the
    /// upgrade, while the session is at its limit, and begun again by every
    /// new notice. What bounds how long the agent's own background work may
    /// hold a READY restart ([`next_step`]): the answer gets a whole
    /// [`REASK_S`] of its own before that work supersedes it, however late in
    /// its notice's window it came (the review of 2026-09-26: a notice's clock
    /// alone could give up on a READY answered seconds before, when the agent
    /// wound down past the interval as the notice itself asks).
    pub ready_s: u64,
    /// Seconds since the upgrade's round STOPPED ([`Phase::Failed`]), the
    /// driver's stamp (`upgrade_drive::St::failed_at`; 0: not stopped, or
    /// stopped at this very look). A stop an older build recorded carries no
    /// stamp, and reads as stopped long ago. Past [`RETRY_S`] the round is
    /// re-armed ([`Step::Rearm`]): no stop is for good.
    pub failed_s: u64,
}

/// THE LIMIT FACT ([`Facts::limited`]) of a screen of `agent`'s, by the ONE
/// recogniser the supervisor reads a usage or rate limit with — never a
/// second pattern of the upgrade's own. Claude Code: [`aterm_phase::limit_notice`],
/// the notice the last turn ENDED on, wherever Claude Code draws it (the
/// footer, a banner under the last thing said, the `⎿` gutter) and of a
/// kind that [`aterm_phase::WallKind::reads_limited`] — asked of the notice
/// itself rather than of [`aterm_phase::worker_phase`], where a busy signal
/// wins: a break of the agent's own background work (`✻ Waiting for 2
/// dynamic workflows to finish`) reads BUSY with the wall still standing
/// over it, and the notice typed at such a break (`background_point`) is
/// queued behind the limit all the same. Codex: the wall its reader places
/// at the end of the turn ([`aterm_phase::codex::wall`]), of the same kinds.
#[must_use]
pub fn limited(agent: Agent, rows: &[String]) -> bool {
    match agent {
        Agent::Claude => aterm_phase::limit_notice(rows).is_some(),
        Agent::Codex => aterm_phase::codex::wall(rows).is_some_and(|w| w.kind.reads_limited()),
    }
}

/// A gate's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Go.
    Go,
    /// Not yet, and why (one word).
    Wait(&'static str),
}

/// Seconds Claude must have been idle, and the screen quiet, before anything is
/// typed or ended: long enough that a person who just read the answer and
/// started to think is not typed over, short enough that an idle session
/// moves the same hour. Measured on the agent's own verdict
/// ([`Facts::quiet_s`]), which any turn moves.
///
/// A CONVERSATION WITH NO TASK SETTLES THE SAME ([`Facts::taskless`],
/// [`Step::Fresh`]): there is no answer to read, but the session the harness
/// looks at right after it was launched is the one whose FIRST PROMPT is on
/// its way — an orchestrator's `await agent idle` returns at the very first
/// idle verdict, and it sends then (ND1 of the live re-test of 2026-09-26:
/// the fresh restart fired 1.3 s after that verdict, and a first prompt sent
/// then would have met the shell). Settled, the prompt has either come — a
/// task, recorded at once ([`TaskScan`]) — or the session has sat still for
/// this long, and the restart holds the tab while it is under way.
pub const QUIET_S: u64 = 20;

/// Whether a session whose person stamp reads `human` is ATTENDED
/// ([`Facts::attended`]): a person gave it input within `grace_s` seconds —
/// `[harness] human_grace_s`, the one knob for how long a person's hand keeps
/// the harness's off (the gap review measured the need, not the length: its
/// ten minutes became that knob). No person since the host began serving it
/// (`null`, `-`) is no one at it. A stamp the host does not send (a build
/// older than the stamp, read by a hand-run sweep) proves no absence, so it
/// FAILS CLOSED: the session reads attended, and only the owner's `--now`
/// moves it.
#[must_use]
pub fn attended_by(human: crate::supervise::screen::HumanInput, grace_s: u32) -> bool {
    human.within(grace_s).unwrap_or(true)
}

/// May the ANNOUNCEMENT be typed now? No hold, no person at the tab
/// ([`Facts::attended`]), Claude idle and settled, the composer empty, no
/// box, no busy footer, and the screen quiet — the person and the quiet
/// waived by the owner's `--now` alone ([`Facts::owner_now`]) — for a
/// conversation with no task too ([`Facts::taskless`]: what the gate opens
/// there is [`Step::Fresh`], never a notice).
///
/// THE PERSON IS ASKED FIRST, before Claude's status (review of
/// 2026-09-25): a busy attended tab waits `attended`, not `not-idle` — the
/// wait that names what actually holds it, so the owner reads the person, and
/// the upgrade owns none of the tab's turn ends for a notice it would refuse
/// to type.
///
/// `not-idle` IS ALSO A TURN THAT ENDED WITH WORK STILL RUNNING. While a
/// dynamic workflow or another background task the agent started is in
/// flight, Claude Code keeps its status off `idle` (`busy`, measured
/// 2026-09-24 with `✻ Waiting for N dynamic workflow…` animated over the
/// composer; `shell` for a background shell), and that work runs inside the
/// agent's process or under it, so the RESTART this notice leads to would end
/// it: [`gate_restart`] waits for an idle point with nothing running under the
/// agent before the signal, however long that takes. The NOTICE, though, is
/// typed at such a break — the window's loop reads it as the agent's own
/// background work and nothing else, stood [`QUIET_S`]
/// ([`Facts::background_point`]; the owner's answer of 2026-09-26: an agent
/// that orchestrates all day would otherwise never hear of its upgrade). A
/// step taken anywhere else still waits `not-idle` there.
///
/// AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK ([`Facts::background_point`])
/// the notice is typed all the same (the owner's answer of 2026-09-26). It
/// interrupts the agent's orchestration once, and again only after a whole
/// [`REASK_S`] with the work still running ([`next_step`]). The agent answers
/// READY when its work is done. Claude's `busy` (a workflow waited on) or
/// `shell` status is no wait there, and the settle is the loop's. A person, a
/// hold, a box, a draft and a live turn's busy footer still wait.
/// [`gate_restart`] asks an idle point with nothing running under the agent
/// all the same.
///
/// A SESSION AT ITS LIMIT IS NEVER ASKED ([`Facts::limited`]): it waits
/// `limited`, right after the person, whatever else holds — a notice typed
/// there is queued unread behind the limit (the 2026-09-25 incident: four of
/// them, delivered at once when the limit reset). Nor is one at THE LOGIN
/// WALL ([`Facts::login`]): it waits `login` — a notice typed there is
/// answered by the wall and read by no model (the 2026-09-27 incident: four
/// of them, each met by `Login expired · Please run /login`). Nothing waives
/// either, the owner's `--now` included. Nor is one whose conversation
/// already holds a FULL QUEUE of notices it has not taken
/// ([`Facts::queued`]): it waits `queued` — the driver lifts that fact
/// itself for the owner's `--now` given since the latest notice, and once
/// the queue has rested.
#[must_use]
pub fn gate_announce(f: &Facts) -> Gate {
    if f.held {
        return Gate::Wait("held");
    }
    if f.attended && !f.owner_now {
        return Gate::Wait("attended");
    }
    if f.limited {
        return Gate::Wait("limited");
    }
    if f.login {
        return Gate::Wait("login");
    }
    if f.queued {
        return Gate::Wait("queued");
    }
    let at_break = f.background_point && matches!(f.status.as_str(), "idle" | "busy" | "shell");
    if f.status != "idle" && !at_break {
        return Gate::Wait("not-idle");
    }
    if f.busy_footer {
        return Gate::Wait("busy");
    }
    if f.approval_box {
        return Gate::Wait("box");
    }
    if !f.composer_empty {
        return Gate::Wait("draft");
    }
    if f.owner_now || f.background_point {
        return Gate::Go;
    }
    if f.status_age_s < QUIET_S || f.quiet_s < QUIET_S {
        return Gate::Wait("settling");
    }
    Gate::Go
}

/// May the agent be ended now? Everything [`gate_announce`] asks, plus the
/// agent's READY answer and NOTHING running under it. Nothing is ever killed to
/// reach this gate: background work is waited for. What bounds that wait is
/// the re-ask in [`next_step`], never this gate.
///
/// AN ATTENDED TAB IS NEVER SIGNALLED ON THE UPGRADE'S OWN JUDGMENT (review of
/// 2026-09-25): [`gate_announce`]'s attended-tab guard stands here too, READY
/// answer or not, and only the owner's own `--now` waives it
/// ([`Facts::owner_now`]). A SIGTERM in front of a person reading the answer is
/// the one act the upgrade cannot take back.
#[must_use]
pub fn gate_restart(f: &Facts, ready: bool) -> Gate {
    if !ready {
        return Gate::Wait("not-ready");
    }
    if !f.background.is_empty() {
        return Gate::Wait("background");
    }
    gate_announce(f)
}

/// Which of the gates' facts only a PERSON can clear, if one holds now: `box` — an
/// approval or question box is up, or Claude's own status says `waiting` (a dialog
/// or permission prompt) — and `draft`, text typed into the composer and not sent.
/// One sample of either is not yet a person's: the supervisor answers or declines
/// every box it can read within seconds, so a box is one handed to a person only
/// once it has stood for [`HOLD_S`] ([`Facts::hold_s`]). The agent's own work
/// (`busy`, `shell`, background tasks), aterm's hold and the settle window are not
/// a person's, and are waited for as long as they last.
#[must_use]
pub fn person_hold(f: &Facts) -> Option<&'static str> {
    if f.approval_box || f.status == "waiting" {
        return Some("box");
    }
    if !f.composer_empty {
        return Some("draft");
    }
    None
}

/// How long, after the announcement, a READY answer still authorizes the restart
/// while a PERSON holds the gate ([`person_hold`]). Past it the answer is void: the
/// agent said it was at a good stopping point half an hour ago, and whatever has
/// happened since — a box it raised and nobody answered, a draft somebody left in
/// the composer — is not that stopping point. Without the bound the drain waited on
/// the person for good, and a person back hours later who answered the box or
/// cleared the draft had the agent ended under them twenty seconds after, on the
/// strength of the stale answer. On expiry the driver forgets the markers, records
/// why, RELEASES the agent (it stopped for a restart that is not coming: one line
/// tells it to go back to work, [`release_prompt`]) and restarts the re-ask clock —
/// the ordinary re-ask asks again [`REASK_S`] later, at an idle point. The bound is
/// at least [`REASK_S`]: a void is never sooner than a re-ask would have been. It
/// also bounds how long the agent's OWN background work may hold a READY
/// restart: [`DRAIN_S`] after the answer ([`Facts::ready_s`]), still running,
/// it voids the answer the same way (review of 2026-09-26: a `run_in_background`
/// server under an agent that answered READY held it stopped for good).
pub const DRAIN_S: u64 = 30 * 60;

const _: () = assert!(DRAIN_S >= REASK_S);

/// How long a person's hold ([`person_hold`]) must have stood, sample after
/// sample — none more than [`HOLD_GAP_S`] apart — before it voids a READY answer
/// past [`DRAIN_S`]: the supervisor answers or declines a box it can read within
/// seconds, and the driver looks at a held session once a minute, so a box one
/// sample catches is not yet one nobody answers.
pub const HOLD_S: u64 = 5 * 60;

/// The longest gap between two samples that both showed a person's hold for the
/// hold to count as having STOOD through it ([`hold_since`]): three of the host's
/// looks at a held session (`upgrade_drive::HOLD_LOOK`, a minute; asserted
/// there). A longer gap — a system sleep, a driver restarted, a worker busy
/// elsewhere — is time nobody sampled: a box seen at both ends of it may be two
/// boxes, each answered in seconds, so the dwell begins again. Measured on the
/// reducer (the review of 2026-09-25, F8): with the dwell kept across any gap,
/// two samples two hours apart that each caught a box voided the READY answer
/// and spent one of [`MAX_ASKS`].
pub const HOLD_GAP_S: u64 = 3 * 60;

/// [`HOLD_S`] is never met across one gap: a hold voids only once at least three
/// samples, none more than [`HOLD_GAP_S`] apart, have shown it.
const _: () = assert!(HOLD_GAP_S < HOLD_S);

/// The person's hold standing now, for [`Facts::hold_s`]: `(since, seen)` — the
/// sample it began at and the last sample that showed it — from the driver's last
/// answer `prior` (`(0, 0)`: none). A sample with no hold ends it, `(0, 0)`; one
/// with a hold carries it on while the last sample to show one is at most
/// [`HOLD_GAP_S`] back, and past that begins it again at `now_s`.
#[must_use]
pub fn hold_since(prior: (u64, u64), f: &Facts, now_s: u64) -> (u64, u64) {
    let (since, seen) = prior;
    match person_hold(f) {
        None => (0, 0),
        Some(_) if since == 0 || now_s.saturating_sub(seen) > HOLD_GAP_S => (now_s, now_s),
        Some(_) => (since, now_s),
    }
}

/// Whether the composer holds no TYPED text: present, and either blank or a dim
/// placeholder suggestion — the cursor at column 2 of the caret row with text
/// after it ([`aterm_phase::phase::is_placeholder`]) AND that text drawn DIM.
/// `dim_at_caret` is the caret row's column 2 read with the `cell` verb
/// (measured 2026-09-23 under Claude Code 2.1.280 and 2.1.281: the suggestion's
/// first letter reads `dim`, a typed draft's `none`). The cursor alone cannot
/// tell them apart: a typed draft whose caret was moved home (←, Home, ctrl-a)
/// leaves it at column 2 too, and a notice typed there lands IN FRONT of the
/// person's draft and is submitted with it (measured the same day). A
/// suggestion drawn without SGR dim (no colour at all) therefore reads as a
/// draft: nothing is typed, which is the safe way to be wrong. A draft on a
/// second row is typed text whatever the cursor says. `cursor` is `(row, col)`
/// in the same coordinates as `rows`.
#[must_use]
pub fn composer_is_empty(
    rows: &[String],
    cursor: Option<(usize, usize)>,
    dim_at_caret: bool,
) -> bool {
    let Some((caret, lines)) = aterm_phase::phase::composer_draft(rows) else {
        return false;
    };
    if lines.iter().skip(1).any(|l| !l.is_empty()) {
        return false;
    }
    if lines.first().is_none_or(|l| l.is_empty()) {
        return true;
    }
    dim_at_caret && matches!(cursor, Some((row, col)) if row == caret && col == 2)
}

/// Where the upgrade of one session stands. Persisted per session id, so the
/// next step — the window's host's at the next idle point, or a hand-run
/// sweep's — picks up exactly where the last one stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Nothing typed yet.
    #[default]
    Pending,
    /// The announcement was typed at `at_s` (unix seconds), `asks` times so far.
    Announced {
        /// When the latest announcement was typed.
        at_s: u64,
        /// How many announcements were typed for this upgrade.
        asks: u32,
    },
    /// SIGTERM was sent at `at_s`.
    Exiting {
        /// When.
        at_s: u64,
    },
    /// The relaunch line was typed at `at_s`.
    Relaunched {
        /// When.
        at_s: u64,
    },
    /// The new process holds the session and the continuation was typed.
    Done,
    /// This ROUND stopped, and why. Never for good (the owner, 2026-09-27:
    /// "you should NEVER have upgrades stalled"): [`RETRY_S`] after it
    /// stopped ([`Facts::failed_s`]) the upgrade starts a new round
    /// ([`Step::Rearm`], [`retry_due`]) — unless the owner's `--skip` of the
    /// target, or a `--defer` not run out, holds it ([`requested_step`]). Until
    /// then it waits `failed`, and an upgrade that stopped ASKING
    /// ([`GAVE_UP`]) still honours a READY answer to a notice of its own while
    /// the session that was asked lives ([`next_step`]).
    Failed(String),
}

/// The [`Phase::Failed`] reason of a round that stopped asking after
/// [`MAX_ASKS`] notices went unanswered ([`Step::GiveUp`]) — the owner's view
/// says `gave-up`, and when the next round starts ([`RETRY_S`]).
pub const GAVE_UP: &str = "unanswered";

/// Seconds between announcements when the agent has not answered READY (it
/// may have said it cannot stop yet, or the person took the turn), or when
/// work under it outlives a READY answer ([`next_step`]). The same interval
/// holds at an idle point and at a break of the agent's background work.
/// Spent only while the session can READ a notice: at a usage limit the clock
/// is held ([`clock_held`]), so the agent has the whole window once the limit
/// resets.
pub const REASK_S: u64 = 30 * 60;

/// The most announcements one ROUND of an upgrade makes: a bound on NAGGING,
/// never a reason to strand the agent. Past it the round stops asking
/// ([`Step::GiveUp`]), says so in the ledger naming what still runs under the
/// agent, releases the agent ([`release_prompt`]), and still honours a READY
/// answer to one of its notices that comes late ([`GAVE_UP`]) — until the next
/// round, [`RETRY_S`] later, asks again. Never a forced restart.
///
/// ONLY A NOTICE THE MODEL COULD READ COUNTS (the owner's report of
/// 2026-09-27: four notices typed into a session at its weekly limit read as
/// `no READY answer after 4 notices`). A notice that never reached the model
/// — the login wall's ([`Facts::undelivered`]), or one a usage limit answered
/// and that limit is over ([`queued_until`]) — is typed again as the same
/// ask, and none is typed behind a notice the limit still holds
/// ([`Facts::limited`]).
pub const MAX_ASKS: u32 = 4;

/// The most times a notice a usage limit answered is typed again STRAIGHT
/// AWAY before the model takes one ([`queued_copies`],
/// [`Facts::undelivered`]): at most `1 + REQUEUE_MAX` upgrade notices wait in
/// the conversation untaken at once — this ask's copies and any other
/// notice's — and past that one more only after each rest
/// ([`Scan::rests_until`], [`Facts::queued`]). The owner's report of
/// 2026-09-27 was four notices typed behind the weekly limit and delivered
/// at once; the fix types a queued notice again once its limit is over by
/// the transcript's word (a reset passed, a `/login`, or [`REASK_S`] after a
/// limit row naming no reset), and a row naming no reset is over by that word
/// every [`REASK_S`] however long the limit really stands — so unbounded,
/// the same notice would pile up a copy every half hour. Each copy the limit
/// answers again is the limit's own word that it still stands; past the
/// bound the upgrade waits `queued` for the session's model to write a row
/// of its own (a turn that got past the limit — Claude Code's own at the
/// reset, a person's, a peer's), which takes every copy at once and opens the
/// ask's window — or for the queue's rest ([`queue_rest`]: [`RETRY_S`],
/// growing with every copy the model has not read, up to a day) after the
/// limit's word ran out, when one copy more goes (the review of 2026-09-27:
/// held for good, an idle session whose limit had ended was never asked
/// again), or for the owner's `--now`, one copy more at once. Derived model:
/// `aterm_spec::derive::harness_upgrade_limit_queue_model` (`RetypeMax`,
/// and `Daily` for [`QUEUE_REST_DOUBLINGS`]).
pub const REQUEUE_MAX: u32 = 2;

/// Seconds a STOPPED round of an upgrade ([`Phase::Failed`], whatever stopped
/// it) rests before the upgrade starts a new one ([`Step::Rearm`]): one
/// round's worth of asking, `MAX_ASKS × REASK_S` (two hours today).
///
/// WHY THAT LONG AND NO LONGER (the owner, 2026-09-27: "you should NEVER have
/// upgrades stalled" — a tab sat `failed:unanswered` for 1d22h, its agent's
/// late READY voided under the background gate it always runs, and nothing
/// would ever ask it again). A round asks for at most `MAX_ASKS × REASK_S`
/// before it gives up; resting exactly as long again keeps the nagging to at
/// most half of any stretch of time, one round on, one round off, however
/// long the agent's work lasts. And it BOUNDS THE SILENCE, though not to one
/// rest (the no-stall review of 2026-09-27). At an agent that can read, the
/// stretch with no notice after a round's last one is the give-up's window,
/// `REASK_S`, plus the rest: this, stretched up to
/// `RETRY_S << RETRY_BACKOFF_MAX_SHIFT` for a stop that repeats
/// ([`rest_extension`]). A late READY the gave-up round hears holds the new
/// round back while the answer stands ([`retry_due`]: the round acts on it
/// instead), and one the agent's own work outlives is voided [`DRAIN_S`]
/// after it — the rest then begins again at the void, a whole `RETRY_S`. So
/// the longest silence is `REASK_S + rest + DRAIN_S + RETRY_S`, reached by a
/// READY heard just before the rest runs out: five hours today after a first
/// stop, eleven after one repeated to the cap (two and a half and eight and a
/// half with no late READY). Any other stop (a refused signal, a relaunch
/// that never came up, a conversation resumed elsewhere) hears no late READY:
/// its silence is its rest. Whatever else a late READY's restart waits on
/// ends the stretch when it passes, with the restart or the void, never with
/// a notice — the settle, a turn in progress, aterm's hold, a person's box or
/// draft (voided [`HOLD_S`] into its hold): the agent's or a person's to end,
/// not the upgrade's silence. The rest is not held at a usage limit: the
/// round it starts types nothing there (a limited session is never asked),
/// so no clock needs holding for it.
pub const RETRY_S: u64 = MAX_ASKS as u64 * REASK_S;

const _: () = assert!(RETRY_S >= REASK_S);

/// THE LONGEST A REPEATED STOP STRETCHES THE REST, as a power of two of
/// [`RETRY_S`] (the no-stall review of 2026-09-27, S1): a stop whose reason
/// repeats round after round — a relaunch that never comes up would otherwise
/// ask for a READY, signal the agent and leave it dead at its prompt every
/// [`RETRY_S`] — rests `RETRY_S`, then twice, then four times as long, and no
/// longer. Never terminal: the upgrade still asks again, a few times a day.
pub const RETRY_BACKOFF_MAX_SHIFT: u32 = 2;

/// How much LATER than [`RETRY_S`] after a stop the next round starts, for
/// the `streak`-th stop in a row with the same reason (`St::fail`): nothing
/// for the first, then `RETRY_S`, then `3 × RETRY_S` — the rest doubling up
/// to `RETRY_S << RETRY_BACKOFF_MAX_SHIFT`.
#[must_use]
pub fn rest_extension(streak: u32) -> u64 {
    let shift = streak.saturating_sub(1).min(RETRY_BACKOFF_MAX_SHIFT);
    RETRY_S.saturating_mul((1_u64 << shift) - 1)
}

impl Phase {
    /// The word the ledger and `status` print.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Phase::Pending => "pending".to_string(),
            Phase::Announced { asks, .. } => format!("announced:{asks}"),
            Phase::Exiting { .. } => "exiting".to_string(),
            Phase::Relaunched { .. } => "relaunched".to_string(),
            Phase::Done => "done".to_string(),
            Phase::Failed(why) => format!("failed:{why}"),
        }
    }
}

/// One step the driver takes next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Type the announcement.
    Announce,
    /// Nothing to do this tick, and why.
    Wait(&'static str),
    /// End the agent (SIGTERM to its pid only).
    Terminate,
    /// The READY answer is void, and why (the [`person_hold`] word, or
    /// `background`): past [`DRAIN_S`] a person's state has held the gate for
    /// [`HOLD_S`] ([`person_void`]), or — for an upgrade that gave up asking,
    /// which has no re-ask left to supersede the answer with — the agent's own
    /// background work for [`DRAIN_S`] after the answer ([`void_of`]). Forget
    /// the markers, release the agent and record it; an announced upgrade asks
    /// again [`REASK_S`] later.
    Void(&'static str),
    /// Stop asking: [`MAX_ASKS`] announcements went unanswered, or none left a
    /// READY answer the restart could act on (the agent's own work outlived
    /// it). The upgrade is then [`GAVE_UP`]: the agent is owed its release
    /// line, and a READY answer that still comes before that line is honoured.
    GiveUp,
    /// The conversation has no task ([`Facts::taskless`]): end the agent
    /// (SIGTERM to its pid only) and start the new build AFRESH in its tab —
    /// no notice before, no `--resume`, nothing typed after.
    Fresh,
    /// A stopped round has rested [`RETRY_S`] ([`retry_due`]): start a NEW
    /// ROUND of the upgrade — pending again, a fresh salt (new READY markers),
    /// its asks reset, the owner's spent `--now` gone — and say so in the
    /// ledger (`rearmed:<why>`). Types nothing and ends nothing, so it is
    /// taken anywhere, at a break and at a limit too: the round it starts
    /// asks under every gate a first notice is typed under.
    Rearm,
}

/// WHETHER A STOPPED ROUND STARTS A NEW ONE NOW ([`Step::Rearm`]): it stopped
/// (`phase` is [`Phase::Failed`], for any reason) at least [`RETRY_S`] ago
/// (`failed_s`, [`Facts::failed_s`]) — and it is not an upgrade that gave up
/// asking with a READY answer to one of its notices in hand (`ready`), which
/// is still acted on as it is ([`next_step`]): the re-arm replaces only the
/// forever-wait, never an answer the agent gave.
#[must_use]
pub fn retry_due(phase: &Phase, failed_s: u64, ready: bool) -> bool {
    match phase {
        Phase::Failed(why) => !(why == GAVE_UP && ready) && failed_s >= RETRY_S,
        _ => false,
    }
}

/// THE REDUCER for the cooperative half: from where the upgrade stands, the
/// facts and whether the agent has answered, the one next step. The exit and
/// relaunch halves are the driver's, because each is a wait on the kernel.
///
/// At a break of the agent's own background work
/// ([`Facts::background_point`]) only a NOTICE is taken: the first one, and
/// a re-ask once a whole [`REASK_S`] has passed since the last one
/// ([`reask`]) — or, its asks spent, the give-up. A READY a person held past
/// the drain is voided there as at an idle point. The restart, and a gave-up
/// upgrade's late READY, wait for an idle point (`background`: the agent's
/// own work runs); [`break_step`] is the one rule both drivers hold a break to.
///
/// THE AGENT'S OWN WORK BOUNDS NO WAIT, BUT IT DOES BOUND THE SILENCE
/// (2026-09-26). A tab sat four days on an old Claude Code: behind
/// two background poll loops whose workflow had died, every look after the
/// first notice answered `background`, before this function looked at an
/// answer, a re-ask or [`MAX_ASKS`]. With READY in hand, the restart's gate
/// answered `background` too ([`gate_restart`]), and only a person's hold
/// could void the answer. Nothing was ever asked again, nothing gave up, and
/// the owner was told the move would come "once that ends". Now the work is
/// still waited for and never ended, but a notice whose [`REASK_S`] has run
/// out is asked again. That holds at a break, and with a READY answer that
/// work under the agent still outlives by a whole [`REASK_S`] of the answer's
/// own ([`Facts::ready_s`]): the new notice SUPERSEDES that answer — it is no
/// longer the agent's last word after the latest notice — where a void would
/// type the release line ("no restart is coming now") only for the next
/// notice to follow it on its heels. Each re-ask names what runs (the driver's
/// [`running_clause`]), and past [`MAX_ASKS`] the upgrade gives up and says
/// what held it. `gave-up` is what the owner's `--now` re-arms.
///
/// A conversation with NO TASK ([`Facts::taskless`]) is never announced to:
/// once [`gate_restart`] would let a READY agent go — its settle waived — it
/// is restarted afresh ([`Step::Fresh`]), whether nothing was typed yet or a
/// notice an older aterm typed stands unanswered — never at its limit
/// (below): it waits `limited` as every step does.
///
/// A SESSION AT ITS LIMIT ([`Facts::limited`]) is never asked, re-asked,
/// given up on, voided or ended, at a break or at an idle point: every step
/// waits `limited` — an announcement that could not be read is not an ask,
/// so neither the asks budget nor, with the driver's [`clock_held`], the
/// re-ask clock is spent.
///
/// AN UPGRADE THAT GAVE UP ASKING ([`GAVE_UP`]) STILL HEARS A LATE READY
/// (the 2026-09-26 incident: four notices queued behind a usage limit were
/// delivered at 06:00, the agent stopped its work and answered the last
/// one's marker at 06:02, and the upgrade — failed since 18:02 — waited
/// `failed` for ever, the agent stopped under it). `ready` there is the
/// driver's reading of a marker this upgrade issued, answered by the very
/// process its notice reached, and the step is the one an announced upgrade
/// takes on it: [`gate_restart`], every gate kept, then the restart and the
/// carry-on — or, past the drain's bound, the void ([`void_of`]: a person's
/// hold, or the agent's own background work, that outlasted it). With no
/// re-ask left, the void — and the release it owes — is the way off for an
/// answer the agent's work outlives. At a BREAK of that work the restart is
/// never taken, so there the answer waits `background` until the same
/// bound voids it (the owner's tab of 2026-09-27 sat 2h56m and counting at
/// `wait=background`: it ran a background gate at every look, and the break
/// answered `background` before the bound was ever asked).
///
/// NO STOP IS FOR GOOD (the owner, 2026-09-27: "you should NEVER have
/// upgrades stalled"). Any other stopped round — and a gave-up one with no
/// READY in hand — waits `failed` for [`RETRY_S`] after it stopped
/// ([`Facts::failed_s`]), then starts a new round ([`Step::Rearm`],
/// [`retry_due`]): at a break, at a limit and at the login wall too, since
/// it types nothing; the round it starts asks under every gate. Until
/// 2026-09-27 `failed` was terminal for the target: a new round came only
/// with a newer build.
///
/// AT THE LOGIN WALL ([`Facts::login`]) every step waits `login`, as at a
/// limit, at a break or at an idle point. A NOTICE THAT NEVER REACHED THE
/// MODEL ([`Facts::undelivered`]: its own turn ended on the login wall) spent
/// no ask: it is typed again at the first point a notice could be — a break
/// included — never waited out as an unanswered one, never given up on
/// ([`announce_asks`]: the same ask). An upgrade that GAVE UP on such
/// notices (the 2026-09-27 incident: 0.93.0 spent all four into the wall and
/// gave up at 07:03) is asked about as [`rearmed`] makes it, unless a READY
/// came first.
///
/// A NOTICE QUEUED BEHIND A USAGE LIMIT ([`NoticeFate::Queued`], the owner's
/// report of 2026-09-27: four notices typed into a session at its weekly
/// limit reached the agent at once) was never read: the driver reads it as
/// [`Facts::limited`], so nothing is typed behind it and its window waits
/// for the model to take it — or, its limit over by the transcript's word
/// ([`queued_until`]) and none on the screen, as [`Facts::undelivered`]:
/// typed again as the same ask, at once while the conversation's queue has
/// room ([`REQUEUE_MAX`]), else — [`Facts::queued`], `queued` — once it has
/// rested or on the owner's `--now`. Only a notice the model could read
/// spends one of [`MAX_ASKS`].
#[must_use]
pub fn next_step(phase: &Phase, f: &Facts, ready: bool, now_s: u64) -> Step {
    if retry_due(phase, f.failed_s, ready) {
        return Step::Rearm;
    }
    if f.taskless && matches!(phase, Phase::Pending | Phase::Announced { .. }) {
        if f.limited {
            return Step::Wait("limited");
        }
        if f.background_point {
            return Step::Wait("background");
        }
        return match gate_restart(f, true) {
            Gate::Go => Step::Fresh,
            Gate::Wait(w) => Step::Wait(w),
        };
    }
    let reannounce = || match gate_announce(f) {
        Gate::Go => Step::Announce,
        Gate::Wait(w) => Step::Wait(w),
    };
    if f.background_point && *phase != Phase::Pending {
        let Phase::Announced { at_s, asks } = phase else {
            // A gave-up upgrade's late READY at a break: the restart is an
            // idle point's, but the agent's own work that outlives the
            // answer voids it here as there — at a break that work runs by
            // definition — so a session that is always at a break is never
            // held on the answer for good.
            if matches!(phase, Phase::Failed(why) if why == GAVE_UP) && ready {
                if f.limited {
                    return Step::Wait("limited");
                }
                if f.login {
                    return Step::Wait("login");
                }
                let drained = REASK_S.saturating_add(f.ready_s) >= DRAIN_S;
                return void_of(f, "background", drained)
                    .map_or(Step::Wait("background"), Step::Void);
            }
            // A stopped round resting to its next says so, at a break as at an
            // idle point: `wait=background` there was the owner's own stalled
            // tab's word, and it named the agent's work, not the rest.
            if matches!(phase, Phase::Failed(_)) {
                return Step::Wait("failed");
            }
            return Step::Wait("background");
        };
        if f.limited {
            return Step::Wait("limited");
        }
        if f.login {
            return Step::Wait("login");
        }
        let since = now_s.saturating_sub(*at_s);
        // A person's hold voids a READY answer here as at an idle point: the
        // restart a stale answer would authorize comes at the next idle point.
        if ready && let Some(who) = person_void(f, since >= DRAIN_S) {
            return Step::Void(who);
        }
        // A notice the wall answered is no ask the clock may count toward a
        // give-up: typed again here, where a notice may go.
        if !ready && f.undelivered {
            return reannounce();
        }
        return if clock(since, f, ready) >= REASK_S {
            reask(f, *asks)
        } else {
            Step::Wait("background")
        };
    }
    match phase {
        Phase::Pending => reannounce(),
        Phase::Announced { at_s, asks } => {
            if f.limited {
                return Step::Wait("limited");
            }
            if f.login {
                return Step::Wait("login");
            }
            let since = now_s.saturating_sub(*at_s);
            if ready {
                return match gate_restart(f, true) {
                    Gate::Go => Step::Terminate,
                    Gate::Wait(w) => match person_void(f, since >= DRAIN_S) {
                        Some(who) => Step::Void(who),
                        // The agent's own work outlives the answer: asked
                        // again, naming what runs, or given up on.
                        None if w == "background" && clock(since, f, ready) >= REASK_S => {
                            reask(f, *asks)
                        }
                        None => Step::Wait(w),
                    },
                };
            }
            if f.undelivered {
                return reannounce();
            }
            if since < REASK_S {
                return Step::Wait("awaiting-ready");
            }
            reask(f, *asks)
        }
        Phase::Exiting { .. } | Phase::Relaunched { .. } => Step::Wait("in-flight"),
        Phase::Done => Step::Wait("done"),
        Phase::Failed(why) if why == GAVE_UP && ready => {
            if f.limited {
                return Step::Wait("limited");
            }
            if f.login {
                return Step::Wait("login");
            }
            match gate_restart(f, true) {
                Gate::Go => Step::Terminate,
                // The drain's clock counts from the latest notice, which a
                // gave-up upgrade no longer keeps — but it gave up REASK_S
                // after that notice at the soonest, and has heard this answer
                // for `ready_s` since: their sum bounds the notice's age from
                // below, so the bound is never met sooner than an announced
                // upgrade's (today the sum is past DRAIN_S from the start:
                // DRAIN_S == REASK_S).
                Gate::Wait(w) => void_of(f, w, REASK_S.saturating_add(f.ready_s) >= DRAIN_S)
                    .map_or(Step::Wait(w), Step::Void),
            }
        }
        Phase::Failed(_) => Step::Wait("failed"),
    }
}

/// THE PERSON'S VOID ([`DRAIN_S`], [`HOLD_S`]): past the drain's bound
/// (`drained`, [`DRAIN_S`] after the notice), a PERSON's hold
/// ([`person_hold`]: a box nobody answers, a draft nobody sends) that has
/// stood [`HOLD_S`] over the READY answer, which the answer then no longer
/// outlasts. `None` for no hold, one short of [`HOLD_S`], or inside the
/// bound. Both arms of [`next_step`] void on it, at a break as at an idle
/// point: the restart a stale answer would authorize comes at the next idle
/// point, after the person let go.
fn person_void(f: &Facts, drained: bool) -> Option<&'static str> {
    if !drained || f.hold_s < HOLD_S {
        return None;
    }
    person_hold(f)
}

/// Why the READY answer a gave-up upgrade's restart gate still holds (`w`,
/// the gate's wait) is VOID now, if it is — past the drain's bound
/// (`drained`): the person's hold ([`person_void`]), or the agent's own
/// BACKGROUND work ([`Facts::background`]) still running under it
/// [`DRAIN_S`] after the answer ([`Facts::ready_s`]) — the agent said
/// nothing of its own still runs, and whatever does (a `run_in_background`
/// server, a watcher) will not end on its own. Anything else the gate waits
/// on — the settle, the agent's own turn, a person's hand, aterm's hold, the
/// limit — passes, and the restart follows it.
///
/// The review of 2026-09-26 found the gave-up arm with no bound at all: a
/// READY a person's box held for hours ended the agent seconds after the box
/// was answered — the stale answer [`DRAIN_S`] exists to void — and one a
/// background server held left the agent holding for good, the release it
/// owed held behind the READY. Voided, the answer is forgotten and the agent
/// released (`upgrade_drive::drain_expired`). An ANNOUNCED upgrade voids only
/// on the person: the agent's own work outliving its answer is met by a
/// re-ask that supersedes the answer ([`next_step`]), not by a release line
/// the next notice would contradict.
fn void_of(f: &Facts, w: &'static str, drained: bool) -> Option<&'static str> {
    person_void(f, drained)
        .or_else(|| (drained && w == "background" && f.ready_s >= DRAIN_S).then_some("background"))
}

/// Since when the READY answer the upgrade acts on has stood, for
/// [`Facts::ready_s`]: the driver's `prior` stamp (0: none), kept while the
/// answer stands, begun at `now_s` when it is first heard — and again at
/// every look that finds the session at its limit ([`Facts::limited`]): no
/// clock of the upgrade runs while the agent cannot read. 0 when there is no
/// such answer (`ready` false).
#[must_use]
pub fn ready_since(prior: u64, ready: bool, f: &Facts, now_s: u64) -> u64 {
    if !ready {
        0
    } else if prior == 0 || f.limited || f.login {
        now_s
    } else {
        prior
    }
}

/// THE RE-ASK CLOCK IS HELD AT A LIMIT: the phase a visit records once it has
/// read `f` at `now_s` — an announced upgrade's clock restarted at `now_s`
/// while the session stands at its limit ([`Facts::limited`]), anything else
/// as it is. A notice typed before the limit hit (the agent's wind-down turn
/// is the one that hit it) is answered only after the limit resets, so the
/// [`REASK_S`] window, the [`DRAIN_S`] bound and the path to [`MAX_ASKS`] are
/// all counted from the last look that found the session limited: the agent
/// gets the whole window once it can read again. The same at THE LOGIN WALL
/// ([`Facts::login`]): what a notice's reader cannot answer, no clock runs
/// on — and, the wall lifted, the driver starts the clock again from the
/// lift the transcript records ([`login_lifted_at`]). And behind a FULL
/// QUEUE ([`Facts::queued`]): the notice waits in the conversation unread.
#[must_use]
pub fn clock_held(phase: &Phase, f: &Facts, now_s: u64) -> Phase {
    if f.limited || f.login || f.queued {
        clock_held_until(phase, now_s)
    } else {
        phase.clone()
    }
}

/// [`clock_held`] for a limit known to have stood until `until` (unix
/// seconds): an announced upgrade's clock starts no sooner than then. What
/// the window's host applies when its loop's limit episode opens and closes
/// (`upgrade_drive::hold_clock`) — the host takes no step during an episode,
/// so no look of the upgrade's own ever finds the limit there.
#[must_use]
pub fn clock_held_until(phase: &Phase, until: u64) -> Phase {
    match phase {
        Phase::Announced { at_s, asks } => Phase::Announced {
            at_s: (*at_s).max(until),
            asks: *asks,
        },
        other => other.clone(),
    }
}

/// Whether a step that WAITS on `why` leaves the RELEASE ([`release_prompt`])
/// as the upgrade's next act, if one is owed: the upgrade itself waits on
/// nothing it would type or signal — its re-ask window (`awaiting-ready`), a
/// stop (`failed`), the owner's hold (`skipped`, `deferred`). Every other wait
/// is the notice's or the restart's own gate, and what it waits for goes
/// first: a re-ask supersedes the release, and a restart's carry-on tells the
/// agent to go on. The review of 2026-09-26 measured what reading every wait
/// as the release's did: a READY a gave-up upgrade was about to act on waited
/// `settling`, the step said `wait:release:ready` instead, and the window's
/// host — which owns the session's turn ends only while the restart's gate
/// settles — let the supervisor type over the answer.
#[must_use]
pub fn release_is_next(why: &str) -> bool {
    matches!(why, "awaiting-ready" | "failed" | "skipped" | "deferred")
}

/// May the RELEASE LINE ([`release_prompt`]) be typed now? Never while the
/// agent's READY answer stands to be acted on (`ready`: a READY THE PHASE
/// ACTS ON — an announced upgrade's, or a late one a gave-up upgrade still
/// hears; the restart goes first, and its carry-on is what tells the agent to
/// continue), and otherwise under exactly the gate the notice is typed under
/// ([`gate_announce`]) at an idle point: no hold, no person, no limit, Claude
/// idle and settled, no box, no draft, no busy footer. A READY no phase acts
/// on — the answer to an upgrade that has since stopped — holds nothing: the
/// review of 2026-09-26 found a restart refused after READY waiting
/// `ready` here for ever, the agent it had asked neither restarted nor
/// released.
#[must_use]
pub fn gate_release(f: &Facts, ready: bool) -> Gate {
    if ready {
        return Gate::Wait("ready");
    }
    gate_announce(&Facts {
        background_point: false,
        ..f.clone()
    })
}

/// How [`release_prompt`] opens: the harness's tag, and words no notice
/// ([`ANNOUNCE_HEAD`]) or continuation (`[aterm harness] Upgraded:`) opens
/// with.
pub const RELEASE_HEAD: &str = "[aterm harness] Upgrade off: ";

/// THE RELEASE LINE, typed ONCE into a session the upgrade asked to wind down
/// and then abandoned without restarting it — it gave up asking, voided the
/// READY answer, was held by the owner's `--skip`/`--defer`, or a restart
/// after READY stopped (the owner's direction of 2026-09-26: "debug aterm
/// about why you stalled and didn't continue working"). The agent stopped
/// for a restart that is not coming; this tells it so, and to go on as if
/// the notice had never come — and that nothing restarts it without asking
/// first: a later round ([`Step::Rearm`]) asks with a notice and a READY of
/// its own, never on the strength of this one. It points at no work of its own — the review
/// of 2026-09-26: "continue the work you were doing before the notice"
/// pulled an agent back to what it did before, over direction it had been
/// given since — and it is typed only to the process the notice reached,
/// never over direction the agent took up after its last answer to the
/// upgrade (`upgrade_drive::release`, [`directed_since_ready`]). `agent`
/// names the product (`Claude Code`).
#[must_use]
pub fn release_prompt(agent: Agent) -> String {
    format!(
        "{RELEASE_HEAD}The {} upgrade is off for now: nothing will restart this session without \
         asking you again first, and nothing the notice asked of you still applies. Carry on as \
         you would have without it.",
        agent.product()
    )
}

/// WHAT A BREAK OF THE AGENT'S OWN BACKGROUND WORK MAY DO
/// ([`Facts::background_point`]): type a notice, void a READY answer a person
/// or the agent's own work held, give up asking, start a new round of a
/// stopped one, or wait. All of them type at most a notice and end
/// nothing. Any other step (the end) becomes a wait on that work
/// (`background`), whatever the plan says. There is one rule, and both
/// drivers apply it: `upgrade_drive` and the Codex lane.
#[must_use]
pub fn break_step(step: Step) -> Step {
    match step {
        Step::Announce | Step::GiveUp | Step::Void(_) | Step::Wait(_) | Step::Rearm => step,
        Step::Terminate | Step::Fresh => Step::Wait("background"),
    }
}

/// THE RE-ASK CLOCK: seconds since the latest notice. With a READY answer in
/// hand, the clock runs from the later of the notice and the answer
/// ([`Facts::ready_s`], begun again by every notice and held at a limit), so
/// an answer given late still gets a whole [`REASK_S`] before work it
/// outlives supersedes it.
fn clock(since: u64, f: &Facts, ready: bool) -> u64 {
    if ready { since.min(f.ready_s) } else { since }
}

/// THE RE-ASK ([`next_step`]): past [`MAX_ASKS`] notices the upgrade gives
/// up. Before that the notice is typed again wherever [`gate_announce`] lets
/// a notice go. A new notice carries a new READY marker and follows any
/// earlier answer, so an answer the agent's work outlived is superseded,
/// never acted on.
fn reask(f: &Facts, asks: u32) -> Step {
    if asks >= MAX_ASKS {
        return Step::GiveUp;
    }
    match gate_announce(f) {
        Gate::Go => Step::Announce,
        Gate::Wait(w) => Step::Wait(w),
    }
}
// ---------------------------------------------------------------- the owner's word

/// THE OWNER'S WORD ON ONE SESSION'S UPGRADE (gap audit 2026-09-24, "no owner
/// control": the only choices were quitting Claude by hand — which the harness
/// then never records — or switching the whole feature off). Written by
/// `aterm harness upgrade <sid> --now|--defer <dur>|--skip` into the session's
/// state under the sweep lock, and read by every sweep through
/// [`requested_step`]. Each word only waives the waits that exist so a person
/// is not typed over (`Now`: the settling window and the attended-tab guard)
/// or adds one (`DeferUntil`, `Skip`) — with ONE exception, made where the
/// word is written (`upgrade_drive::ask`), never here: `Now` on an upgrade
/// that GAVE UP (no READY answer it could act on after its last ask) re-arms
/// it at once, so the
/// notice is typed again. A refused or failed upgrade is not re-armed by the
/// word (review of 2026-09-25: re-arming every stopped kind on the owner's
/// word restored typing, and after READY a SIGTERM, for what had just
/// stopped it); it rests [`RETRY_S`] like every stopped round, and then the
/// upgrade starts a new round itself ([`Step::Rearm`]). The word
/// is on the tab the owner named, not on the conversation wherever it goes.
/// A word that HOLDS the upgrade ([`Request::holds`], and a stopped round's
/// new one, [`rearm_held`]) makes the step wait
/// `skipped`/`deferred`, which no idle point cures, so the upgrade owns none
/// of the session's turn ends: its supervisor continues the worker as if none
/// were pending. Every word also arms a NEW ROUND of the upgrade — a fresh
/// salt, so a notice after it asks for a READY marker of its own
/// (`upgrade_drive::ask`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Request {
    /// Nothing asked: the gates alone decide.
    #[default]
    None,
    /// Move this session at its next turn end: the settling window
    /// ([`QUIET_S`]) and the attended-tab guard ([`Facts::attended`]) are
    /// waived ([`Facts::owner_now`]), for the notice and at the signal alike:
    /// both waits exist so that a person reading the answer is not typed
    /// over, and this is that person asking — and nothing else: an idle
    /// status, an empty composer, no box, no busy footer, no hold, the READY
    /// answer, an empty process tree and the typed turn's screen fence are
    /// still required. On a supervised session the turn end it moves at is
    /// the next idle point the session's worker takes its step at: the waits
    /// `--now` leaves are the idle point's.
    Now,
    /// Not before this unix second; the request lapses on its own after it.
    DeferUntil(u64),
    /// Not onto this version: the session stays where it is until a NEWER
    /// build is the target.
    Skip(String),
}

impl Request {
    /// The word the state file, `--status` and the `upgrade=` column carry:
    /// `-`, `now`, `defer-until:<unix s>`, `skip:<version>`.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Self::None => "-".to_string(),
            Self::Now => "now".to_string(),
            Self::DeferUntil(t) => format!("defer-until:{t}"),
            Self::Skip(v) => format!("skip:{v}"),
        }
    }

    /// Whether this word HOLDS the upgrade of `phase` to `target` at `now_s`:
    /// a skip of THAT target, or a deferral not yet run out, while nothing
    /// has begun restarting (a restart in flight is never held). What
    /// [`requested_step`] waits `skipped`/`deferred` on, and what releases
    /// the session's supervisor from the upgrade.
    #[must_use]
    pub fn holds(&self, phase: &Phase, target: &str, now_s: u64) -> bool {
        if !matches!(phase, Phase::Pending | Phase::Announced { .. }) {
            return false;
        }
        match self {
            Self::Skip(v) => Version::parse(v).is_some_and(|v| Some(v) == Version::parse(target)),
            Self::DeferUntil(t) => now_s < *t,
            Self::None | Self::Now => false,
        }
    }

    /// [`Self::word`] read back; an unknown word is `None` (the caller keeps
    /// [`Request::None`]: an unreadable request asks for nothing).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "" | "-" => Some(Self::None),
            "now" => Some(Self::Now),
            _ => {
                if let Some(t) = word.strip_prefix("defer-until:") {
                    return t.parse().ok().map(Self::DeferUntil);
                }
                let v = word.strip_prefix("skip:")?;
                Version::parse(v).map(|v| Self::Skip(v.to_string()))
            }
        }
    }
}

/// [`next_step`] under the owner's [`Request`] for the upgrade to `target`.
/// A skip of THIS target or a deferral not yet run out holds a session that
/// has not begun restarting (`skipped`, `deferred`, [`Request::holds`]) — and
/// a stopped round's new one ([`rearm_held`]); `Now`
/// waives the settling window and the attended-tab guard and nothing else. A
/// restart already in flight is never held: the agent was signalled, and
/// stopping half way would strand the conversation.
///
/// `Now` is applied as [`Facts::owner_now`], never by aging the facts: an
/// earlier spelling raised `status_age_s` and `quiet_s` to [`QUIET_S`], which
/// read a turn that had just ended as one that had settled. It also clears
/// [`Facts::attended`], at the notice and at the signal alike.
#[must_use]
pub fn requested_step(
    request: &Request,
    phase: &Phase,
    f: &Facts,
    ready: bool,
    now_s: u64,
    target: &str,
) -> Step {
    if request.holds(phase, target, now_s) {
        return Step::Wait(held_word(request));
    }
    let step = if *request == Request::Now {
        let waived = Facts {
            owner_now: true,
            attended: false,
            ..f.clone()
        };
        next_step(phase, &waived, ready, now_s)
    } else {
        next_step(phase, f, ready, now_s)
    };
    rearm_held(request, step, target, now_s)
}

/// The wait the owner's holding word says: `skipped` or `deferred`.
fn held_word(request: &Request) -> &'static str {
    if matches!(request, Request::Skip(_)) {
        "skipped"
    } else {
        "deferred"
    }
}

/// THE OWNER'S WORD HOLDS A NEW ROUND AS IT HOLDS ANY ([`Step::Rearm`],
/// [`Request::holds`]): a stopped round the owner skipped (`--skip` of this
/// target) or deferred (a `--defer` not run out) is not re-armed — it waits
/// `skipped`/`deferred` — since the round it would start is held at once.
/// A deferral that runs out lets it re-arm. Both lanes' [`requested_step`]
/// end here (`upgrade_codex::requested_step`).
#[must_use]
pub fn rearm_held(request: &Request, step: Step, target: &str, now_s: u64) -> Step {
    if step == Step::Rearm && request.holds(&Phase::Pending, target, now_s) {
        Step::Wait(held_word(request))
    } else {
        step
    }
}

/// The WAIT a sweep records for the owner: the gate's word, and for
/// `not-idle` Claude's own status beside it (`not-idle:busy`, `:shell`,
/// `:waiting`) — the difference between a turn in progress, a background
/// shell and a question waiting on a person, which `not-idle` alone erased
/// (measured 2026-09-24: `step=wait:not-idle` for 8h22m on two sessions, and
/// nothing said which). The status is a third party's word, so it is cut to
/// `[a-z-]`, 16 bytes; nothing else is added to it.
#[must_use]
pub fn wait_word(why: &str, status: &str) -> String {
    if why != "not-idle" {
        return why.to_string();
    }
    let status: String = status
        .chars()
        .filter(|c| c.is_ascii_lowercase() || *c == '-')
        .take(16)
        .collect();
    if status.is_empty() {
        why.to_string()
    } else {
        format!("{why}:{status}")
    }
}

/// A span of seconds as the owner reads it: `45s`, `12m`, `8h22m`, `3d4h` —
/// the two largest units, the smaller dropped when it is zero. One spelling
/// for `--status`, the dry run's `pending_for=`/`wait_for=` and the window's
/// `upgrade=` column.
#[must_use]
pub fn span(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h{m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d{h}h"),
    }
}

#[cfg(test)]
#[path = "upgrade_tests.rs"]
mod tests;

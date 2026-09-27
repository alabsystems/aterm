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
//!    telling the agent an upgrade restart is coming, asking it to let its
//!    background work finish (never cancel it), save its work, and answer with a
//!    one-time READY marker ([`prepare_prompt`]). Never into a session at a
//!    usage or rate limit ([`Facts::limited`]): what is typed there is queued
//!    unread, and no ask, and no minute of the re-ask clock, is spent on it.
//! 2. **Drain** — wait until the agent has answered with the marker
//!    ([`transcript_has_ready`]), Claude's own session file says `idle`, nothing
//!    runs under the agent (no shell, and no `caffeinate` but Claude Code's own
//!    keep-awake), and the composer is still empty ([`gate_restart`]). Nothing is
//!    ever killed to get there, and the agent's own work is waited for however
//!    long it takes. A PERSON is not: a box nobody answers or a typed draft
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
//!    type one turn telling the agent it was upgraded, and which model it ran
//!    before the restart, and to carry on ([`continue_prompt`]).
//! 5. **Confirm** — read the model the resumed session's first answer names
//!    ([`transcript_first_model`], past the mark the restart took, in the new
//!    build's rows) and record it beside the one before ([`restart_outcome`]).
//!    The rewrite keeps an explicit `--model` ([`launch_model`]) and adds none
//!    of its own, so a session launched without one comes back on Claude
//!    Code's default at the relaunch — unless the model priority list
//!    (`upgrade_models`) moves the conversation, when the announcement and the
//!    continuation name the model ([`prepare_prompt_with_model`],
//!    [`continue_prompt_with_model`]), the relaunch carries `--model`, and the
//!    outcome says whether it was taken ([`restart_outcome_listed`]): a change
//!    is the expected outcome and is only said, with what decided it; a
//!    session that has not answered in time is said to be
//!    UNCONFIRMED.
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
pub const ANNOUNCE_HEAD: &str = "[aterm harness] Claude Code ";

/// THE ANNOUNCEMENT, typed as one ordinary user turn. It asks for a good
/// stopping point and names the one line to answer with; it never asks the
/// agent to cancel anything.
#[must_use]
pub fn prepare_prompt(from: &Version, to: &Version, source: Source, marker: &str) -> String {
    format!(
        "{ANNOUNCE_HEAD}{to} ({}) is installed; this session runs {from}. \
         To move you onto it, aterm will restart this Claude Code in place and resume this \
         same conversation (claude --resume, same tab, same flags). Please get to a good \
         stopping point first: let any background tasks, subagents or workflows you started \
         finish (do not cancel them), save or commit work in progress, and do not start new \
         long-running work. When nothing of yours is still running, reply with {marker} on a \
         line by itself. If you cannot stop now, say why; aterm will wait and ask again later.",
        source.as_str()
    )
}

/// [`prepare_prompt`] for a restart that ALSO moves the conversation to `model`
/// from the priority list — or ONLY does (`to == from`: the build is current,
/// the model is not). The text still starts with [`ANNOUNCE_HEAD`], so the
/// supervisor's turn-end policy knows it for what it is.
#[must_use]
pub fn prepare_prompt_with_model(
    from: &Version,
    to: &Version,
    source: Source,
    marker: &str,
    model: Option<&str>,
) -> String {
    let Some(model) = model else {
        return prepare_prompt(from, to, source, marker);
    };
    let what = if to == from {
        format!(
            "{ANNOUNCE_HEAD}{to} can run {model}, the best available model on aterm's priority \
             list, and this session runs an older one. To move you onto it, aterm will restart \
             this Claude Code in place and resume this same conversation on {model} (claude \
             --resume --model {model}, same tab, same flags)."
        )
    } else {
        format!(
            "{ANNOUNCE_HEAD}{to} ({}) is installed; this session runs {from}. To move you onto \
             it, and onto {model} (the best available model on aterm's priority list), aterm \
             will restart this Claude Code in place and resume this same conversation (claude \
             --resume --model {model}, same tab, same flags).",
            source.as_str()
        )
    };
    format!(
        "{what} Please get to a good stopping point first: let any background tasks, subagents \
         or workflows you started finish (do not cancel them), save or commit work in progress, \
         and do not start new long-running work. When nothing of yours is still running, reply \
         with {marker} on a line by itself. If you cannot stop now, say why; aterm will wait and \
         ask again later."
    )
}

/// [`continue_prompt`] for a relaunch that asked for `model` from the list.
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
    let before = ran.map_or(String::new(), |m| format!(" (it ran {m} before)"));
    let build = if to == from {
        format!("on Claude Code {to}")
    } else {
        format!("on Claude Code {to} (from {from})")
    };
    format!(
        "[aterm harness] Upgraded: this session was restarted {build} with {model}, the best \
         available model on aterm's priority list{before}, and resumed. {CARRY_ON}"
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
        "[aterm harness] Upgraded: this session was restarted on Claude Code {to} (from \
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

/// [`restart_outcome`] for a relaunch that asked for `chosen` from the
/// model PRIORITY LIST (`upgrade_models`): a change is said with that reason,
/// and a model after other than `chosen` is said as not taken.
#[must_use]
pub fn restart_outcome_listed(
    to: &str,
    before: Option<&str>,
    after: Option<&str>,
    chosen: &str,
) -> String {
    let head = format!("claude restarted on {to} · model");
    let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
    match (before, after) {
        (_, Some(a)) if base(a) != base(chosen) => {
            format!("{head} {a} (the priority list asked for {chosen}; it was not taken)")
        }
        (Some(b), Some(a)) if base(b) != base(a) => {
            format!("{head} {b} -> {a} (the priority list chose it; /model changes it)")
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
    /// own `busy`/`shell` status is no wait for the FIRST notice, and the
    /// loop's settle stands for the screen's (a workflow's progress line
    /// never holds still); every other step — a re-ask, the restart — is an
    /// idle point's ([`next_step`]: `background`).
    pub background_point: bool,
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
    pub limited: bool,
    /// Seconds the READY answer the upgrade acts on has stood unacted on,
    /// the driver's stamp ([`ready_since`]; 0: no such answer now) — held,
    /// like every clock of the upgrade, while the session is at its limit.
    /// What bounds how long the agent's own background work may hold a
    /// READY restart ([`next_step`]).
    pub ready_s: u64,
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
/// typed: long enough that a person who just read the answer and started to
/// think is not typed over, short enough that an idle session moves the same
/// hour.
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
/// waived by the owner's `--now` alone ([`Facts::owner_now`]).
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
/// the notice is typed all the same — the owner's answer of 2026-09-26: it
/// interrupts the agent's orchestration once, and the agent answers READY
/// when its work is done — Claude's `busy` (a workflow waited on) or `shell`
/// status no wait there, and the settle the loop's; a person, a hold, a box,
/// a draft and a live turn's busy footer still wait. [`gate_restart`] asks
/// an idle point with nothing running under the agent all the same.
///
/// A SESSION AT ITS LIMIT IS NEVER ASKED ([`Facts::limited`]): it waits
/// `limited`, right after the person, whatever else holds — a notice typed
/// there is queued unread behind the limit (the 2026-09-25 incident: four of
/// them, delivered at once when the limit reset). Nothing waives it, the
/// owner's `--now` included.
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
/// reach this gate: background work is waited for.
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
    /// Stopped, and why. Not retried for the same (session, target) — with
    /// ONE exception, [`GAVE_UP`]: an upgrade that stopped ASKING still
    /// honours a READY answer to a notice of its own while the session that
    /// was asked lives ([`next_step`]).
    Failed(String),
}

/// The [`Phase::Failed`] reason of an upgrade that stopped asking after
/// [`MAX_ASKS`] notices went unanswered ([`Step::GiveUp`]) — the owner's view
/// says `gave-up`.
pub const GAVE_UP: &str = "unanswered";

/// Seconds between announcements when the agent has not answered READY: the
/// agent may have said it cannot stop yet, or the person took the turn. Spent
/// only while the session can READ a notice: at a usage limit the clock is held
/// ([`clock_held`]), so the agent has the whole window once the limit resets.
pub const REASK_S: u64 = 30 * 60;

/// The most announcements one upgrade makes: a bound on NAGGING, never a reason
/// to strand the agent. Past it the upgrade stops asking ([`Step::GiveUp`]) —
/// it releases the agent ([`release_prompt`]) and still honours a READY answer
/// to one of its notices that comes late ([`GAVE_UP`]). Never a forced restart.
pub const MAX_ASKS: u32 = 4;

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
    /// [`HOLD_S`], or the agent's own background work for [`DRAIN_S`] after the
    /// answer ([`void_of`]). Forget the markers, release the agent and record
    /// it; an announced upgrade asks again [`REASK_S`] later.
    Void(&'static str),
    /// Stop asking: [`MAX_ASKS`] announcements went unanswered. The upgrade
    /// is then [`GAVE_UP`]: the agent is owed its release line, and a READY
    /// answer that still comes before that line is honoured.
    GiveUp,
}

/// THE REDUCER for the cooperative half: from where the upgrade stands, the
/// facts and whether the agent has answered, the one next step. The exit and
/// relaunch halves are the driver's, because each is a wait on the kernel.
///
/// At a break of the agent's own background work
/// ([`Facts::background_point`]) only the FIRST notice is taken: the break
/// interrupts the orchestration once, and a re-ask, the restart and every
/// other step wait for an idle point (`background`: the agent's own work
/// runs).
///
/// A SESSION AT ITS LIMIT ([`Facts::limited`]) is never asked, re-asked,
/// given up on, voided or ended: every step waits `limited` — an
/// announcement that could not be read is not an ask, so neither the asks
/// budget nor, with the driver's [`clock_held`], the re-ask clock is spent.
///
/// AN UPGRADE THAT GAVE UP ASKING ([`GAVE_UP`]) STILL HEARS A LATE READY
/// (the 2026-09-26 incident: four notices queued behind a usage limit were
/// delivered at 06:00, the agent stopped its work and answered the last
/// one's marker at 06:02, and the upgrade — failed since 18:02 — waited
/// `failed` for ever, the agent stopped under it). `ready` there is the
/// driver's reading of a marker this upgrade issued, answered by the very
/// process its notice reached, and the step is the one an announced upgrade
/// takes on it: [`gate_restart`], every gate kept, then the restart and the
/// carry-on — or, past the drain's bound, the same void ([`void_of`]: a
/// person's hold, or the agent's own background work, that outlasted it).
/// Any other stop waits `failed`.
#[must_use]
pub fn next_step(phase: &Phase, f: &Facts, ready: bool, now_s: u64) -> Step {
    if f.background_point && *phase != Phase::Pending {
        return Step::Wait("background");
    }
    match phase {
        Phase::Pending => match gate_announce(f) {
            Gate::Go => Step::Announce,
            Gate::Wait(w) => Step::Wait(w),
        },
        Phase::Announced { at_s, asks } => {
            if f.limited {
                return Step::Wait("limited");
            }
            if ready {
                return match gate_restart(f, true) {
                    Gate::Go => Step::Terminate,
                    Gate::Wait(w) => void_of(f, w, now_s.saturating_sub(*at_s) >= DRAIN_S)
                        .map_or(Step::Wait(w), Step::Void),
                };
            }
            if now_s.saturating_sub(*at_s) < REASK_S {
                return Step::Wait("awaiting-ready");
            }
            if *asks >= MAX_ASKS {
                return Step::GiveUp;
            }
            match gate_announce(f) {
                Gate::Go => Step::Announce,
                Gate::Wait(w) => Step::Wait(w),
            }
        }
        Phase::Exiting { .. } | Phase::Relaunched { .. } => Step::Wait("in-flight"),
        Phase::Done => Step::Wait("done"),
        Phase::Failed(why) if why == GAVE_UP && ready => {
            if f.limited {
                return Step::Wait("limited");
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

/// Why the READY answer a restart's gate still holds (`w`, the gate's wait)
/// is VOID now, if it is — past the drain's bound (`drained`, [`DRAIN_S`] after
/// the notice): a PERSON's hold ([`person_hold`]: a box nobody answers, a
/// draft nobody sends) that has stood [`HOLD_S`], or the agent's own
/// BACKGROUND work ([`Facts::background`]) still running under it
/// [`DRAIN_S`] after the answer ([`Facts::ready_s`]) — the agent said
/// nothing of its own still runs, and whatever does (a `run_in_background`
/// server, a watcher) will not end on its own. Anything else the gate waits
/// on — the settle, the agent's own turn, a person's hand, aterm's hold, the
/// limit — passes, and the restart follows it.
///
/// The same bound for an announced upgrade and for one that gave up and
/// hears a late READY (the review of 2026-09-26: the gave-up arm had none, so
/// a READY a person's box held for hours ended the agent seconds after the
/// box was answered — the stale answer [`DRAIN_S`] exists to void — and one a
/// background server held left the agent holding for good, the release it
/// owed held behind the READY). Voided, the answer is forgotten and the
/// agent released (`upgrade_drive::drain_expired`).
fn void_of(f: &Facts, w: &'static str, drained: bool) -> Option<&'static str> {
    if !drained {
        return None;
    }
    if let Some(who) = person_hold(f)
        && f.hold_s >= HOLD_S
    {
        return Some(who);
    }
    (w == "background" && f.ready_s >= DRAIN_S).then_some("background")
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
    } else if prior == 0 || f.limited {
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
/// gets the whole window once it can read again.
#[must_use]
pub fn clock_held(phase: &Phase, f: &Facts, now_s: u64) -> Phase {
    if f.limited {
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
/// the notice had never come. It points at no work of its own — the review
/// of 2026-09-26: "continue the work you were doing before the notice"
/// pulled an agent back to what it did before, over direction it had been
/// given since — and it is typed only to the process the notice reached,
/// never over direction the agent took up after its last answer to the
/// upgrade (`upgrade_drive::release`, [`directed_since_ready`]). `agent`
/// names the product (`Claude Code`).
#[must_use]
pub fn release_prompt(agent: Agent) -> String {
    format!(
        "{RELEASE_HEAD}The {} upgrade is off for now: nothing will restart this session, and \
         nothing the notice asked of you still applies. Carry on as you would have without it.",
        agent.product()
    )
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
/// that GAVE UP (no READY answer after its last ask) re-arms it, so the
/// notice is typed again. A refused or failed upgrade is not re-armed (review
/// of 2026-09-25: re-arming every stopped kind restored typing, and after
/// READY a SIGTERM, for upgrades the harness had stopped for good). The word
/// is on the tab the owner named, not on the conversation wherever it goes.
/// A word that HOLDS the upgrade ([`Request::holds`]) makes the step wait
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
/// has not begun restarting (`skipped`, `deferred`, [`Request::holds`]); `Now`
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
        return Step::Wait(if matches!(request, Request::Skip(_)) {
            "skipped"
        } else {
            "deferred"
        });
    }
    if *request == Request::Now {
        let waived = Facts {
            owner_now: true,
            attended: false,
            ..f.clone()
        };
        return next_step(phase, &waived, ready, now_s);
    }
    next_step(phase, f, ready, now_s)
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

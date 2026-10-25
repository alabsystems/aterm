// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure state machine, transcript scanners, and argv/shell helpers for live agent upgrades; the
//! line builders' one read is the shim environment of a store binary they type
//! ([`with_store_env`]).

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::steps::{BASH_INPUT, BASH_STDERR, BASH_STDOUT, INTERRUPTED, TASK_NOTIFICATION};
use super::upgrade_models as models;
use crate::supervise::approvals::Typed;
use crate::supervise::policy::turn_end;

/// Numeric dotted version (`2.1.281`) where missing trailing components compare as 0.
#[derive(Clone, Debug)]
pub struct Version(Vec<u64>);

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl Version {
    /// Parses a dotted ASCII digit version (tolerating a leading `v`).
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

/// Claude Code record of a running interactive process (`<config>/sessions/<pid>.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFile {
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
    pub version: String,
    pub status: String,
    pub status_updated_at_ms: u64,
    pub proc_start: String,
    pub kind: String,
    pub entrypoint: String,
    pub claude_dir: Option<PathBuf>,
    pub profile: Option<PathBuf>,
}

/// Parses and validates a Claude Code session JSON file.
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
        claude_dir: None,
        profile: None,
    })
}

/// Returns whether `id` is a valid session UUID (8..=64 ASCII hex/dash bytes).
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Source of a relaunch target binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Managed,
    Native,
}

impl Source {
    /// Identifier word used in the ledger and notices.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Source::Managed => "managed",
            Source::Native => "native",
        }
    }
}

/// Agent lane for an upgrade or relaunch record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Agent {
    #[default]
    Claude,
    Codex,
    Agy,
    Muse,
}

const fn next_agent(agent: Agent) -> Option<Agent> {
    match agent {
        Agent::Claude => Some(Agent::Codex),
        Agent::Codex => Some(Agent::Agy),
        Agent::Agy => Some(Agent::Muse),
        Agent::Muse => None,
    }
}

const _: () = {
    let mut at = 0;
    let mut next = Some(Agent::Claude);
    while let Some(agent) = next {
        assert!(at < Agent::ALL.len() && agent as usize == at && Agent::ALL[at] as usize == at);
        at += 1;
        next = next_agent(agent);
    }
    assert!(at == Agent::ALL.len());
};

impl Agent {
    pub const ALL: [Agent; 4] = [Agent::Claude, Agent::Codex, Agent::Agy, Agent::Muse];

    /// All non-Claude agents in the fixed order visited by live upgrade passes.
    #[must_use]
    pub fn others() -> &'static [Agent] {
        &Self::ALL[1..]
    }

    /// Per-tab record key prefix for non-Claude lanes, or `None` for Claude Code.
    #[must_use]
    pub const fn record_prefix(self) -> Option<&'static str> {
        match self {
            Agent::Claude => None,
            Agent::Codex => Some("codex-"),
            Agent::Muse => Some("muse-"),
            Agent::Agy => Some("agy-"),
        }
    }

    /// Per-tab upgrade record key (`<word>-<tab>`) for non-Claude lanes.
    #[must_use]
    pub fn record_key(self, tab: &str) -> Option<String> {
        self.record_prefix().map(|prefix| format!("{prefix}{tab}"))
    }

    /// Extracts the tab id from a per-tab record key for this agent.
    #[must_use]
    pub fn record_tab(self, key: &str) -> Option<&str> {
        key.strip_prefix(self.record_prefix()?)
    }

    /// Identifies the non-Claude agent lane that owns the per-tab record `key`.
    #[must_use]
    pub fn keyed(key: &str) -> Option<Agent> {
        Self::others()
            .iter()
            .copied()
            .find(|agent| agent.record_tab(key).is_some())
    }

    /// State-file identifier word (empty for Claude Code).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Agent::Claude => "",
            Agent::Codex => "codex",
            Agent::Agy => "agy",
            Agent::Muse => "muse",
        }
    }

    /// Parses an agent lane from its state-file or CLI word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "" | "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "agy" => Some(Agent::Agy),
            "muse" => Some(Agent::Muse),
            _ => None,
        }
    }

    /// User-facing product name.
    #[must_use]
    pub const fn product(self) -> &'static str {
        self.program().product()
    }

    /// Corresponding [`aterm_phase::Program`] for screen classification.
    #[must_use]
    pub const fn program(self) -> aterm_phase::Program {
        match self {
            Agent::Claude => aterm_phase::Program::Claude,
            Agent::Codex => aterm_phase::Program::Codex,
            Agent::Agy => aterm_phase::Program::Agy,
            Agent::Muse => aterm_phase::Program::Muse,
        }
    }

    /// Maps a screen-reader [`aterm_phase::Program`] to its upgrade [`Agent`].
    #[must_use]
    pub const fn of(program: aterm_phase::Program) -> Option<Self> {
        match program {
            aterm_phase::Program::Claude => Some(Agent::Claude),
            aterm_phase::Program::Codex => Some(Agent::Codex),
            aterm_phase::Program::Agy => Some(Agent::Agy),
            aterm_phase::Program::Muse => Some(Agent::Muse),
            aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Generic => None,
        }
    }
}

/// Candidate build on which a session can be relaunched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub exe: PathBuf,
    pub version: Version,
    pub source: Source,
}

/// Selects the newest candidate strictly newer than `running` (preferring managed on ties).
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

/// Reason an argv list cannot be safely carried into a `--resume` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgvRefusal {
    UnknownFlag(String),
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    None,
    One,
    Many,
    Optional,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    Keep,
    Drop,
    Refuse,
}

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

fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// Rewrites launch `argv` (without `argv[0]`) into resume flags for `session_id`.
pub fn rewrite_argv(argv: &[String], session_id: &str) -> Result<Vec<String>, ArgvRefusal> {
    let mut out: Vec<String> = carried(argv)?
        .into_iter()
        .flat_map(|(_, tokens)| tokens.iter().cloned())
        .collect();
    out.push("--resume".to_string());
    out.push(session_id.to_string());
    Ok(out)
}

const PERMISSION_FLAGS: &[&str] = &[
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--permission-mode",
];

/// Extracts `argv[0]` plus any permission-mode flags from `argv`.
#[must_use]
pub(crate) fn permission_argv(argv: &[String]) -> Vec<String> {
    let mut out: Vec<String> = argv.first().cloned().into_iter().collect();
    if let Ok(flags) = carried(argv) {
        out.extend(
            flags
                .into_iter()
                .filter(|(name, _)| PERMISSION_FLAGS.contains(name))
                .flat_map(|(_, tokens)| tokens.iter().cloned()),
        );
    }
    out
}

fn carried(argv: &[String]) -> Result<Vec<(&str, &[String])>, ArgvRefusal> {
    let mut out = Vec::new();
    for (name, fate, tokens) in parsed(argv) {
        match fate {
            None => return Err(ArgvRefusal::UnknownFlag(name.to_string())),
            Some(Fate::Refuse) => return Err(ArgvRefusal::NotResumable(name.to_string())),
            Some(Fate::Keep) if name == "--settings" && tap_tokens(tokens) => {}
            Some(Fate::Keep) => out.push((name, tokens)),
            Some(Fate::Drop) => {}
        }
    }
    Ok(out)
}

fn tap_tokens(tokens: &[String]) -> bool {
    let value = match tokens {
        [flag, value] if flag == "--settings" => Some(value.as_str()),
        [one] => one.strip_prefix("--settings="),
        _ => None,
    };
    value.is_some_and(super::tap::is_ours)
}

pub(super) fn carried_names(argv: &[String]) -> Result<Vec<&str>, ArgvRefusal> {
    Ok(carried(argv)?.into_iter().map(|(name, _)| name).collect())
}

/// Each option of an argv as [`parsed`] reads it: its name, its fate where a table knows it, and
/// its tokens (the name and the values it took).
type Options<'a> = Vec<(&'a str, Option<Fate>, &'a [String])>;

fn parsed(argv: &[String]) -> Options<'_> {
    walked(argv).0
}

/// The first operand of `argv` (`argv[0]` first) before a literal `--`, as Claude's parser
/// reads one: a token no option takes as its value (a prompt, or a subcommand's name).
#[must_use]
pub(super) fn first_operand(argv: &[String]) -> Option<&str> {
    walked(argv).1
}

/// [`parsed`]'s options, and the first operand it stepped over ([`first_operand`]).
fn walked(argv: &[String]) -> (Options<'_>, Option<&str>) {
    let mut out = Vec::new();
    let mut operand = None;
    let mut i = 1;
    while i < argv.len() {
        let tok = &argv[i];
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            operand = operand.or(Some(tok.as_str()));
            i += 1;
            continue;
        }
        let (name, inline) = match tok.split_once('=') {
            Some((n, _)) if n.starts_with("--") => (n, true),
            _ => (tok.as_str(), false),
        };
        let known = flag(name);
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
                    // The first value is required, so Claude's parser takes it
                    // whatever it looks like (`--add-dir -c`).
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
    (out, operand)
}

const ONE_SHOT: &[&str] = &["-p", "--print", "-h", "--help", "-v", "--version"];

/// Returns whether `argv` (including `argv[0]`) is a one-shot invocation.
#[must_use]
pub fn one_shot(argv: &[String]) -> bool {
    parsed(argv)
        .iter()
        .any(|(name, _, _)| ONE_SHOT.contains(name))
}

/// Extracts the last kept `--model` value from `argv`.
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

/// Extracts the `--resume` / `-r` argument from `argv`, if present.
#[must_use]
pub fn launch_resume(argv: &[String]) -> Option<Option<String>> {
    let mut resumed: Option<Option<String>> = None;
    let mut fork = false;
    for (name, _, tokens) in parsed(argv) {
        match name {
            "--resume" | "-r" => {
                let id = match tokens {
                    [one] => one.split_once('=').map(|(_, v)| v.to_string()),
                    [_, value] => Some(value.clone()),
                    _ => None,
                };
                resumed = Some(id.filter(|id| !id.is_empty()));
            }
            "--continue" | "-c" => resumed = Some(None),
            "--fork-session" => fork = true,
            _ => {}
        }
    }
    resumed.map(|id| id.filter(|_| !fork))
}

/// Shell dialect inferred from the tab shell executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Zsh,
    Bash,
    Fish,
}

impl Dialect {
    /// Recognizes a shell dialect from an executable basename (including login `-zsh`).
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

    /// File extension for this dialect's `atpkg` shell hook.
    #[must_use]
    pub const fn hook_ext(self) -> &'static str {
        match self {
            Dialect::Zsh => "zsh",
            Dialect::Bash => "bash",
            Dialect::Fish => "fish",
        }
    }
}

/// Quotes a single word for `dialect`.
#[must_use]
pub fn quote(dialect: Dialect, word: &str) -> String {
    match dialect {
        Dialect::Zsh | Dialect::Bash => format!("'{}'", word.replace('\'', r"'\''")),
        Dialect::Fish => format!("'{}'", word.replace('\\', r"\\").replace('\'', r"\'")),
    }
}

#[cfg(test)]
pub(crate) const MAX_LINE: usize = LineDiscipline::HOST.max_line();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineDiscipline {
    Darwin,
    Linux,
}

impl LineDiscipline {
    const HOST: Self = if cfg!(target_os = "linux") {
        Self::Linux
    } else {
        Self::Darwin
    };

    const fn line_keeps(self) -> usize {
        match self {
            Self::Darwin => 1024,
            Self::Linux => 4096,
        }
    }

    const fn max_line(self) -> usize {
        match self {
            Self::Darwin => 1000,
            Self::Linux => 4000,
        }
    }
}

const PASTE_FRAME: usize = 12;

const _: () =
    assert!(LineDiscipline::Darwin.max_line() + PASTE_FRAME < LineDiscipline::Darwin.line_keeps());
const _: () =
    assert!(LineDiscipline::Linux.max_line() + PASTE_FRAME < LineDiscipline::Linux.line_keeps());

/// Builds the space-prefixed shell command line to relaunch a session.
///
/// Every line these builders make for an `exe` inside an atpkg store build also exports what that
/// package's shim does, read from atpkg's record of it; a line too long with all of it keeps only
/// the program's updater switch ([`with_store_env`]).
pub fn relaunch_line(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_env(dialect, heal, cd, &[], exe, args)
}

/// Builds [`relaunch_line`] with `env` variables exported to the relaunched process.
pub fn relaunch_line_env(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_on(LineDiscipline::HOST, dialect, heal, cd, env, exe, args)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn relaunch_line_env_clear(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    unset: &[&str],
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_with_clear(
        LineDiscipline::HOST,
        dialect,
        heal,
        cd,
        env,
        Some(unset),
        exe,
        args,
    )
}

pub const TASK_OPERAND: &str = r#"-- "$(command cat -- "$ATERM_TASK_FILE")""#;

pub const TASK_PROMPT_INTERACTIVE: &str =
    r#""--prompt-interactive=$(command cat -- "$ATERM_TASK_FILE")""#;

/// Builds the initial launch command line for a spawned worker with `env`.
pub fn task_line_env(
    dialect: Dialect,
    env: &[(&str, &str)],
    exe: &Path,
    args: &[String],
    operand: &str,
) -> Result<String, String> {
    task_line_env_clear(dialect, env, &[], exe, args, operand)
}

/// [`task_line_env`] with each of `unset` removed from the worker's environment
/// (`/usr/bin/env -u`, as [`relaunch_line_env_clear`] spells it); with none, the same line.
pub fn task_line_env_clear(
    dialect: Dialect,
    env: &[(&str, &str)],
    unset: &[&str],
    exe: &Path,
    args: &[String],
    operand: &str,
) -> Result<String, String> {
    let tty = LineDiscipline::HOST;
    let unset = Some(unset).filter(|names| !names.is_empty());
    with_store_env(env, unset, exe, |env| {
        let line = relaunch_line_exact(tty, dialect, None, None, env, unset, exe, args)?;
        let line = format!("{line} {operand}");
        let max = tty.max_line();
        if line.len() > max {
            return Err(format!(
                "the line would be {} bytes (max {max})",
                line.len()
            ));
        }
        Ok(line)
    })
}

/// `env`, then each pair of `shim` that `env` neither sets nor `unset` removes: a line that types a
/// store binary by its own path gives it what its managed shim exports
/// ([`atpkg::shim_env::of_store_binary`]), and a value the caller names for itself wins.
pub(super) fn with_shim_env<'a>(
    env: &[(&'a str, &'a str)],
    unset: &[&str],
    shim: &'a [(String, String)],
) -> Vec<(&'a str, &'a str)> {
    let named = |name: &str| env.iter().any(|(n, _)| *n == name) || unset.contains(&name);
    env.iter()
        .copied()
        .chain(
            shim.iter()
                .filter(|(name, _)| !named(name))
                .map(|(name, value)| (name.as_str(), value.as_str())),
        )
        .collect()
}

/// `build`'s line with `env` and every pair of `exe`'s shim record ([`with_shim_env`]), or, when
/// that line is refused, with only the record's pairs that turn the program's own updater off
/// ([`atpkg::shim_env::turns_updater_off`]): the record's other pairs (Claude Code's survey switch)
/// are niceties a line drops before it is refused for length, as a caller drops its own, and the
/// updater switch it never drops.
fn with_store_env(
    env: &[(&str, &str)],
    unset: Option<&[&str]>,
    exe: &Path,
    build: impl Fn(&[(&str, &str)]) -> Result<String, String>,
) -> Result<String, String> {
    let shim = atpkg::shim_env::of_store_binary(exe);
    let unset = unset.unwrap_or_default();
    let all = with_shim_env(env, unset, shim.entries());
    let switches: Vec<(String, String)> = shim
        .entries()
        .iter()
        .filter(|(name, value)| atpkg::shim_env::turns_updater_off(name, value))
        .cloned()
        .collect();
    let lean = with_shim_env(env, unset, &switches);
    build(&all).or_else(|refused| {
        if lean.len() < all.len() {
            build(&lean)
        } else {
            Err(refused)
        }
    })
}

/// What a bash relaunch line that changes directory opens with when no heal precedes it: a
/// simple command. bash's DEBUG trap fires before simple commands only, and bash before 5.3 has
/// no PS0 hook (5.3's sees only a line bash saved to history, never a space-led one), so a line
/// that opened with its subshell ran with no command block, no `cmd` run and no exit code: the
/// relaunch's own run never opened, and nothing could lift the failure it followed (6C-1).
pub(super) const BASH_LINE_OPENS: &str = "builtin :; ";

fn relaunch_line_on(
    tty: LineDiscipline,
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_with_clear(tty, dialect, heal, cd, env, None, exe, args)
}

#[allow(clippy::too_many_arguments)]
fn relaunch_line_with_clear(
    tty: LineDiscipline,
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    unset: Option<&[&str]>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    with_store_env(env, unset, exe, |env| {
        relaunch_line_exact(tty, dialect, heal, cd, env, unset, exe, args)
    })
}

/// The line with exactly `env` exported ([`relaunch_line_with_clear`] adds the store record's).
#[allow(clippy::too_many_arguments)]
fn relaunch_line_exact(
    tty: LineDiscipline,
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    unset: Option<&[&str]>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    if let Some((name, _)) = env.iter().find(|(n, _)| {
        n.is_empty()
            || !n
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    }) {
        return Err(format!("{name:?} is not an environment variable name"));
    }
    if let Some(names) = unset {
        for name in names {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(format!("{name:?} is not an environment variable name"));
            }
            if env.iter().any(|(key, _)| key == name) {
                return Err(format!("{name:?} is both set and removed"));
            }
        }
    }
    let exe_s = exe.to_string_lossy();
    for w in std::iter::once(exe_s.as_ref())
        .chain(args.iter().map(String::as_str))
        .chain(env.iter().map(|(_, v)| *v))
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
    if let Some(names) = unset {
        let mut exact = quote(dialect, "/usr/bin/env");
        for name in names {
            let _ = write!(exact, " -u {}", quote(dialect, name));
        }
        for (name, value) in env {
            let _ = write!(exact, " {}", quote(dialect, &format!("{name}={value}")));
        }
        cmd = format!("{exact} {cmd}");
    }
    let mut vars = String::new();
    for (name, value) in env.iter().filter(|_| unset.is_none()) {
        let plain = !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-'));
        if plain {
            let _ = write!(vars, "{name}={value} ");
        } else {
            let _ = write!(vars, "{name}={} ", quote(dialect, value));
        }
    }
    match cd {
        None if dialect == Dialect::Fish && !vars.is_empty() => {
            let _ = write!(line, "env {vars}{cmd}");
        }
        None => {
            line.push_str(&vars);
            line.push_str(&cmd);
        }
        Some(dir) => match dialect {
            Dialect::Zsh | Dialect::Bash => {
                if dialect == Dialect::Bash && heal.is_none() {
                    line.push_str(BASH_LINE_OPENS);
                }
                let _ = write!(
                    line,
                    "( cd -- {} && {vars}exec {cmd} )",
                    quote(dialect, dir)
                );
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

/// Prefixes a relaunch `line` with a one-use shell-integration rekey read from `path`.
pub fn with_rekey(dialect: Dialect, line: &str, path: &Path) -> Result<String, String> {
    with_rekey_on(LineDiscipline::HOST, dialect, line, path)
}

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
    let q = quote(dialect, &p);
    let takes = [
        aterm_shell_integration::typed_rekey_with_loader(shell, &q),
        aterm_shell_integration::typed_rekey(shell, &q),
    ];
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

/// Mints the one-time reference marker correlating an upgrade notice with its round.
#[must_use]
pub fn ready_marker(session_id: &str, to: &Version, salt: u64) -> String {
    format!(
        "{READY_PREFIX}{:08x}",
        marker_id(session_id, &to.to_string(), salt)
    )
}

const fn marker_id(session_id: &str, to: &str, salt: u64) -> u64 {
    let mut h = aterm_hash::Fnv1a64::new();
    h.write(session_id.as_bytes());
    h.write(to.as_bytes());
    h.write(&salt.to_le_bytes());
    h.finish() & 0xffff_ffff
}

pub const READY_PREFIX: &str = "ATERM-UPGRADE-READY-";

fn is_ready_line(line: &str, marker: &str) -> bool {
    line.trim().trim_matches('`') == marker
}

/// Returns whether `words` affirm readiness for restart or contain `marker`.
#[must_use]
pub fn says_ready(words: &str, marker: &str) -> bool {
    !marker.is_empty()
        && (words.lines().any(|l| is_ready_line(l, marker)) || readiness::says_ready(words))
}

pub const ANNOUNCE_HEAD: &str = concat!(harness_mark!(), " Claude Code ");

/// Builds the user-turn prompt announcing a pending upgrade from `from` to `to`.
#[must_use]
pub fn prepare_prompt(from: &Version, to: &Version, source: Source, marker: &str) -> String {
    format!(
        "{ANNOUNCE_HEAD}{to} ({}) is installed; this session runs {from}. \
         To move you onto it, aterm will restart this Claude Code in place and resume this \
         same conversation (claude --resume, same tab, same flags). {STOPPING_POINT} \
         {READINESS_ANSWER} Notice reference: {marker}. \
         {NOTICE_CLOSE}",
        source.as_str()
    )
}

const READINESS_ANSWER: &str = "Say in ordinary language whether this session is ready to \
    restart now, or explain what must finish first.";

pub const STOPPING_POINT: &str = "Please get to a good stopping point first: let \
     background tasks, subagents or workflows you started finish while they are still making \
     progress (do not cancel them), but stop any background shell or task of yours that only \
     waits for something that has already ended or can never happen (a poll loop on a workflow, \
     agent, job or file that is gone): that wait is not work. A watch or monitor loop you can \
     start again after the resume (a disk or swap watch, a status poll) is not work either: \
     stop it, and note it. Save or commit work in progress, and do not start new long-running \
     work.";

pub const RESTART_WATCHES: &str =
    "If you stopped a watch or loop for this restart, start it again now.";

pub const RELAUNCH_RESTORES: &str = "Anything this session ran in the background before (a \
     watch, a monitor, a loop, a shell) may not have come back: start again only what is no \
     longer running and you still rely on.";

/// Child process running under the agent that blocks a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub pid: u32,
    pub name: String,
    pub age_s: u64,
    pub command: String,
}

pub const HELD_NAMED: usize = 5;

pub const HELD_COMMAND_CHARS: usize = 160;

pub const HELD_PATH_CHARS: usize = 64;

pub const NOTICE_CLOSE: &str =
    "If you cannot stop now, say why; aterm will wait and ask again later.";

/// Inserts `running` before the closing sentence of `notice`.
#[must_use]
pub fn with_running(notice: &str, running: &str) -> String {
    match notice.strip_suffix(NOTICE_CLOSE) {
        Some(head) if !running.is_empty() => {
            format!("{}{running} {NOTICE_CLOSE}", head.trim_end())
        }
        _ => format!("{notice}{running}"),
    }
}

/// Formats the notice clause listing child processes running under the agent.
#[must_use]
pub fn running_clause(held: &[Held]) -> String {
    running_clause_shaped(held, HELD_NAMED, HELD_COMMAND_CHARS)
}

fn running_clause_shaped(held: &[Held], named: usize, chars: usize) -> String {
    let list = held_list_shaped(held, named, chars);
    if list.is_empty() {
        return String::new();
    }
    format!(" Running under you now, as aterm sees it: {list}.")
}

/// Formats running child processes as a semicolon-separated list.
#[must_use]
pub fn held_list(held: &[Held]) -> String {
    held_list_shaped(held, HELD_NAMED, HELD_COMMAND_CHARS)
}

fn held_list_shaped(held: &[Held], named: usize, chars: usize) -> String {
    let mut held: Vec<&Held> = held.iter().collect();
    held.sort_by_key(|h| std::cmp::Reverse(h.age_s));
    let mut out = String::new();
    for (i, h) in held.iter().take(named).enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let command = command_words(&h.command, chars);
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
    if held.len() > named {
        let _ = write!(out, "; and {} more", held.len() - named);
    }
    out
}

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

fn command_words(text: &str, max: usize) -> String {
    let flat = one_line(text, usize::MAX);
    if flat.chars().count() <= max {
        return flat;
    }
    let mut quote = None;
    let words: Vec<String> = flat
        .split(' ')
        .map(|word| {
            let elided = quote.is_none().then(|| path_middle(word)).flatten();
            quote = quote_after(quote, word);
            elided.unwrap_or_else(|| word.to_string())
        })
        .collect();
    one_line(&words.join(" "), max)
}

fn quote_after(mut quote: Option<char>, word: &str) -> Option<char> {
    let mut escaped = false;
    for c in word.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if quote == Some(c) {
            quote = None;
        }
    }
    quote
}

fn path_middle(word: &str) -> Option<String> {
    if word.chars().count() <= HELD_PATH_CHARS {
        return None;
    }
    let lead = match word.split_once('=') {
        Some((name, _))
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) =>
        {
            name.len() + 1
        }
        _ => {
            let redirect = word
                .trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '&' | '<' | '>'));
            let redirect = &word[..word.len() - redirect.len()];
            if redirect.contains(['<', '>']) {
                redirect.len()
            } else {
                0
            }
        }
    };
    let rest = &word[lead..];
    let path = rest.trim_end_matches([';', '&', '|', ')']);
    let root = usize::from(path.starts_with("~/"));
    let rooted = &path[root..];
    if !rooted.starts_with('/')
        || !rooted.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | ',')
        })
    {
        return None;
    }
    let named = path.len() - rooted.trim_start_matches('/').len();
    let first = named + path[named..].find('/')?;
    let last = path.trim_end_matches('/').rfind('/')?;
    if last <= first {
        return None;
    }
    let elided = format!(
        "{}{}/…{}{}",
        &word[..lead],
        &path[..first],
        &path[last..],
        &rest[path.len()..]
    );
    (elided.chars().count() < word.chars().count()).then_some(elided)
}

/// Builds [`prepare_prompt`] when the restart also switches models from `running` to `model`.
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
    format!("{what} {STOPPING_POINT} {READINESS_ANSWER} Notice reference: {marker}. {NOTICE_CLOSE}")
}

/// Builds [`continue_prompt`] when the relaunch requested `model`.
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
         {model}, {why} (for this session only: your default model is unchanged). \
         {RESTART_WATCHES} {CARRY_ON}"
    )
}

/// Builds the continuation prompt typed once the relaunched process resumes the session.
#[must_use]
pub fn continue_prompt(from: &Version, to: &Version, ran: Option<&str>) -> String {
    let resumed = match ran {
        Some(model) => format!("; it ran {model} before the restart and was resumed"),
        None => " and resumed".to_string(),
    };
    format!(
        "{HARNESS_MARK} Upgraded: this session was restarted on Claude Code {to} (from \
         {from}){resumed}. {RESTART_WATCHES} {CARRY_ON}"
    )
}

/// Appends the post-restart verification instruction to `text`.
#[must_use]
pub fn checking(text: &str, irreversible: bool) -> String {
    text.strip_suffix(CARRY_ON).map_or_else(
        || text.to_string(),
        |facts| format!("{facts}{}", turn_end::done_check_text(None, irreversible)),
    )
}

pub const CARRY_ON: &str = concat!(
    "Continue where you left off. If you were waiting on the user, nobody is here to answer: \
     decide for yourself, prefer reversible steps, and keep going. ",
    crate::supervise::policy::needs::needs_hint!()
);

/// Prompt typed after a quiet restart when the resumed build warned of unrestored state.
#[must_use]
pub fn restore_prompt(to: Option<&Version>, upgraded: bool) -> String {
    let on = to.map_or_else(String::new, |to| format!(" {to}"));
    let head = if upgraded { "Upgraded" } else { "Restarted" };
    format!(
        "{HARNESS_MARK} {head}: this session was restarted on Claude Code{on} and resumed \
         as it was. Claude Code reported that something this session ran did not come back \
         after the restart: restore what you rely on (a watch, a monitor, a loop). Nothing \
         else is asked."
    )
}

/// Returns whether build `version` logged a state-restoration warning in `jsonl` `[from..to]`.
#[must_use]
pub fn resumed_build_warned(jsonl: &str, version: &str, from: u64, to: u64) -> bool {
    !version.is_empty()
        && jsonl.lines().any(|line| {
            if !line.contains("\"warning\"") {
                return false;
            }
            let Ok(v) = super::transcript::conversation(line) else {
                return false;
            };
            let field = |key: &str| v.get(key).and_then(Value::as_str);
            let at = field("timestamp").and_then(models::parse_utc);
            field("type") == Some("system")
                && field("subtype") == Some("informational")
                && field("level") == Some("warning")
                && v.get("isSidechain").and_then(Value::as_bool) != Some(true)
                && field("version") == Some(version)
                && at.is_some_and(|at| at >= from && at <= to)
        })
}

/// Returns the `message.model` of the last assistant turn in `jsonl`.
#[must_use]
pub fn transcript_model(jsonl: &str) -> Option<String> {
    assistant_models(jsonl, None).last()
}

/// Returns the `message.model` of the first assistant turn written by `version` in `jsonl`.
#[must_use]
pub fn transcript_first_model(jsonl: &str, version: &str) -> Option<String> {
    assistant_models(jsonl, Some(version)).next()
}

fn assistant_models<'a>(
    jsonl: &'a str,
    version: Option<&'a str>,
) -> impl Iterator<Item = String> + 'a {
    jsonl.lines().filter_map(move |line| {
        if !line.contains("assistant") {
            return None;
        }
        let v = super::transcript::metadata(line).ok()?;
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

/// Returns whether `m` is a syntactically valid model identifier.
pub(crate) fn is_model_id(m: &str) -> bool {
    (1..=256).contains(&m.len())
        && m.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@/[]".contains(&b))
}

/// Formats the user-facing outcome summary after a session restarts.
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

/// Formats [`restart_outcome`] when the relaunch requested `chosen` via `upgrade_models`.
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

/// Returns whether the last assistant turn after `marker` in `jsonl` affirms readiness.
#[must_use]
pub fn transcript_has_ready(jsonl: &str, marker: &str) -> bool {
    let (mut ready, mut matching_notice) = (false, false);
    for line in jsonl.lines() {
        match turn_of(line) {
            Some(Turn::Notice(message)) => {
                ready = false;
                matching_notice = notice_matches(&message, marker);
            }
            Some(Turn::Direction) => (ready, matching_notice) = (false, false),
            Some(Turn::Agent(message)) => ready = answers(&message, marker, matching_notice),
            None => {}
        }
    }
    ready
}

/// Returns whether a user or operator turn has directed the conversation since `markers`.
#[must_use]
pub fn directed_since_ready(jsonl: &str, markers: &[String]) -> bool {
    let (mut told, mut directed) = (false, false);
    let mut matching_notice = false;
    for line in jsonl.lines() {
        match turn_of(line) {
            Some(Turn::Notice(message)) => {
                (told, directed) = (false, false);
                matching_notice = markers.iter().any(|m| notice_matches(&message, m));
            }
            Some(Turn::Direction) => {
                told = true;
                matching_notice = false;
            }
            Some(Turn::Agent(message)) => {
                if markers
                    .iter()
                    .any(|m| answers(&message, m, matching_notice))
                {
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

/// Incremental transcript scanner for whether a worker task has finished and landed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FinishedScan {
    finished: bool,
    said: bool,
    land: bool,
    done_said: bool,
    notice: bool,
    done_irreversible: bool,
    cut: bool,
    command: bool,
    turn: Option<String>,
    ours: Vec<Typed>,
    turn_fold: TurnFold,
    land_typed: bool,
    settled: bool,
}

enum Asked {
    Theirs,
    Command,
    Check,
    Land,
    Challenge,
    Nobody,
    Nothing,
}

impl FinishedScan {
    /// Registers prompts typed by the session supervisor along with their timestamps.
    pub fn supervisor_typed(&mut self, ours: &[Typed]) {
        ours.clone_into(&mut self.ours);
    }

    /// Folds one JSONL transcript line and returns whether the task is now finished.
    pub fn line(&mut self, line: &str) -> bool {
        if !(line.contains("\"user\"")
            || line.contains("\"assistant\"")
            || line.contains("\"queued_command\"")
            || line.contains("\"turn_duration\""))
        {
            return self.finished;
        }
        let Ok(v) = super::transcript::conversation(line) else {
            return self.finished;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return self.finished;
        }
        self.turn_fold.row(&v);
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => self.answered(&v),
            Some("user") => self.asked(&v),
            Some("attachment") => self.queued(&v),
            _ => {}
        }
        self.finished
    }

    /// Returns whether the transcript scanned so far indicates the task is finished.
    #[must_use]
    pub const fn finished(&self) -> bool {
        self.finished
    }

    /// Returns whether a decisive transcript row has settled the finish state.
    #[must_use]
    pub const fn settled(&self) -> bool {
        self.settled
    }

    /// Returns whether the model's last text turn claims the work is done.
    #[must_use]
    pub const fn said_done(&self) -> bool {
        self.done_said
    }

    /// Key identifying the turn on which the scanned transcript ends.
    #[must_use]
    pub fn last_turn_key(&self) -> Option<String> {
        self.turn_fold.turn().last
    }

    /// Full [`LastTurn`] key set for the turn on which the scanned transcript ends.
    #[must_use]
    pub fn last_turn(&self) -> LastTurn {
        self.turn_fold.turn()
    }

    /// Returns whether the model's done report mentions an irreversible action.
    #[must_use]
    pub const fn said_irreversible(&self) -> bool {
        self.done_said && self.done_irreversible
    }

    fn answered(&mut self, row: &Value) {
        // A command of someone else's that started a turn was a prompt of
        // theirs (any answer counts, `<synthetic>` included: [`TaskScan`]).
        if std::mem::take(&mut self.command) {
            (self.finished, self.said, self.land, self.done_said) = (false, false, false, false);
            (self.notice, self.cut) = (false, false);
            self.settled = true;
        }
        if row.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
            self.cut = self.land;
            return;
        }
        let Some(message) = row.get("message") else {
            return;
        };
        if message
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|m| !is_model_id(m))
        {
            return;
        }
        self.cut = false;
        let Some(words) = answer_words(message) else {
            return;
        };
        if !self.notice {
            self.done_said = turn_end::says_done(Some(&words));
            self.done_irreversible = self.done_said
                && matches!(
                    turn_end::classify_said(Some(&words)),
                    turn_end::Said::Irreversible(_)
                );
        }
        if self.land && turn_end::clean_defense(Some(&words)) {
            (self.finished, self.said, self.settled) = (true, true, true);
        } else if self.said {
            (self.finished, self.said) = (false, false);
        }
    }

    fn asked(&mut self, row: &Value) {
        let id = row.get("promptId").and_then(Value::as_str);
        let anew = numbered_anew(id, self.turn.as_deref(), || begins_unnumbered(row));
        if anew {
            self.turn = id.map(str::to_owned);
        }
        if row.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            || row.get("queueTranscriptOnly").and_then(Value::as_bool) == Some(true)
            || only_tool_results(row)
        {
            return;
        }
        let asked = match asked_by(row, &self.ours) {
            Asked::Nobody if self.land_typed && lands_answer(row, &self.ours) => Asked::Land,
            asked => asked,
        };
        if matches!(asked, Asked::Nothing) && !anew {
            return;
        }
        let notice = row.get("message").is_some_and(is_announcement);
        let retry = std::mem::take(&mut self.cut)
            && matches!(asked, Asked::Nobody)
            && (supervisors_plain(row, &self.ours) || vendor_continues(row));
        (self.said, self.notice) = (false, notice);
        if retry {
            self.command = false;
            return;
        }
        self.land = false;
        match asked {
            Asked::Theirs => {
                (self.finished, self.command, self.done_said) = (false, false, false);
                (self.land_typed, self.settled) = (false, true);
            }
            Asked::Command => self.command = true,
            Asked::Check | Asked::Challenge => {
                (self.finished, self.command, self.settled) = (false, false, true);
            }
            Asked::Land => {
                (self.finished, self.land, self.command) = (false, true, false);
                (self.land_typed, self.settled) = (true, true);
            }
            Asked::Nobody => self.command = false,
            Asked::Nothing => {}
        }
    }

    fn queued(&mut self, row: &Value) {
        let Some(queued) = row
            .get("attachment")
            .filter(|a| a.get("type").and_then(Value::as_str) == Some("queued_command"))
        else {
            return;
        };
        let Some(prompt) = queued.get("prompt").filter(|p| !p.is_null()) else {
            return;
        };
        let mut message = aterm_json::Map::new();
        message.insert("content".to_owned(), prompt.clone());
        let mut asked = aterm_json::Map::new();
        asked.insert("message".to_owned(), Value::Object(message));
        for (key, value) in [
            ("origin", queued.get("origin")),
            ("timestamp", row.get("timestamp")),
        ] {
            if let Some(value) = value {
                asked.insert(key.to_owned(), value.clone());
            }
        }
        if matches!(asked_by(&Value::Object(asked), &self.ours), Asked::Theirs) {
            (self.finished, self.said, self.land, self.command) = (false, false, false, false);
            (self.done_said, self.notice, self.cut) = (false, false, false);
            (self.land_typed, self.settled) = (false, true);
        }
    }
}

/// Runs [`FinishedScan`] over a complete transcript slice.
#[must_use]
pub fn transcript_finished(jsonl: &str, ours: &[Typed]) -> bool {
    let mut scan = FinishedScan::default();
    scan.supervisor_typed(ours);
    for line in jsonl.lines() {
        scan.line(line);
    }
    scan.finished()
}

/// Returns the primary turn key (`answer:<uuid>` or `turn:<promptId>`) at the end of `tail`.
#[must_use]
pub fn last_turn_key(tail: &str) -> Option<String> {
    tail.lines()
        .rev()
        .find_map(|line| {
            if !(line.contains("\"user\"") || line.contains("\"assistant\"")) {
                return None;
            }
            let v = super::transcript::conversation(line).ok()?;
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
                || (v.get("type").and_then(Value::as_str) == Some("user")
                    && first_text(&v)
                        .map(typed_words)
                        .and_then(local_row)
                        .is_some())
            {
                return None;
            }
            turn_key_of(&v)
        })
        .flatten()
}

/// Keys identifying the final turn of a transcript tail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LastTurn {
    pub last: Option<String>,
    pub opened: Option<String>,
    pub opened_said: Option<u64>,
    pub ended: Option<std::time::Duration>,
    pub opened_ms: Option<u64>,
    pub asked: Vec<String>,
}

impl LastTurn {
    /// Scans `tail` to extract its final turn keys.
    #[must_use]
    pub fn of(tail: &str) -> Self {
        let mut fold = TurnFold::default();
        for line in tail.lines() {
            if !(line.contains("\"user\"")
                || line.contains("\"assistant\"")
                || line.contains("\"turn_duration\""))
            {
                continue;
            }
            let Ok(v) = super::transcript::conversation(line) else {
                continue;
            };
            if v.get("isSidechain").and_then(Value::as_bool) != Some(true) {
                fold.row(&v);
            }
        }
        fold.turn()
    }

    /// Returns the ledger key under which an unanswered exchange is recorded.
    #[must_use]
    pub fn key(&self) -> Option<String> {
        self.asked
            .last()
            .cloned()
            .or_else(|| self.opened.clone())
            .or_else(|| self.last.clone())
    }

    /// Returns whether `key` matches any identifier of this turn.
    #[must_use]
    pub fn names(&self, key: &str) -> bool {
        if let Some(at_ms) = crate::supervise::approvals::parse_left_before(key) {
            return self.opened_ms.is_some_and(|o| o <= at_ms);
        }
        if let Some((said, wall)) = crate::supervise::approvals::parse_left_retried(key) {
            let before = crate::supervise::approvals::parse_left_at_wall(wall)
                .and_then(|(_, work)| work)
                .unwrap_or_default();
            return self.opened_said == Some(said)
                && self
                    .ended
                    .is_some_and(|d| before.saturating_add(d) < turn_end::EXCHANGE_WORK);
        }
        self.last.as_deref() == Some(key)
            || self.opened.as_deref() == Some(key)
            || self.asked.iter().any(|k| k == key)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TurnFold {
    last: Option<Option<String>>,
    opened: Option<String>,
    said: Option<u64>,
    ended: Option<std::time::Duration>,
    opened_ms: Option<u64>,
    prompt_id: Option<String>,
    answered: bool,
    aside: bool,
    held: Vec<HeldCommand>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HeldCommand {
    key: Option<String>,
    said: Option<u64>,
    prompt_id: Option<String>,
    at_ms: Option<u64>,
}

const HELD_COMMANDS: usize = 16;

impl TurnFold {
    fn row(&mut self, v: &Value) {
        let key = turn_key_of(v);
        let words = || first_text(v).map(typed_words);
        match v.get("type").and_then(Value::as_str) {
            Some("user")
                if key.is_some() && words().and_then(local_row) == Some(LocalRow::Command) =>
            {
                if self.held.len() >= HELD_COMMANDS {
                    self.held.remove(0);
                }
                self.held.push(HeldCommand {
                    key: key.flatten(),
                    said: words().map(crate::supervise::approvals::words_hash),
                    prompt_id: v.get("promptId").and_then(Value::as_str).map(str::to_owned),
                    at_ms: recorded_ms(v),
                });
                return;
            }
            Some("user") if key.is_some() => {
                self.held.clear();
                self.opened = key.clone().flatten();
                self.said = first_text(v)
                    .map(typed_words)
                    .map(crate::supervise::approvals::words_hash);
                self.ended = None;
                self.opened_ms = recorded_ms(v);
                self.prompt_id = v.get("promptId").and_then(Value::as_str).map(str::to_owned);
                (self.answered, self.aside) = (false, false);
            }
            Some("system")
                if v.get("subtype").and_then(Value::as_str) == Some("turn_duration")
                    && self.opened.is_some()
                    && !self.aside =>
            {
                self.ended = v
                    .get("durationMs")
                    .and_then(Value::as_u64)
                    .map(std::time::Duration::from_millis);
            }
            Some("user")
                if (self.answered || !self.held.is_empty())
                    && !only_tool_results(v)
                    && self.turn_of_its_own(v) =>
            {
                self.aside = true;
                self.held.clear();
            }
            Some("assistant") if key.is_some() => {
                if let Some(command) = self.held.pop() {
                    self.held.clear();
                    (self.opened, self.said) = (command.key, command.said);
                    (self.prompt_id, self.ended, self.aside) = (command.prompt_id, None, false);
                    self.opened_ms = command.at_ms;
                } else if std::mem::take(&mut self.aside) {
                    (self.opened, self.opened_ms) = (None, None);
                }
                self.answered = true;
            }
            _ => {}
        }
        if let Some(key) = key {
            self.last = Some(key);
        }
    }

    fn turn_of_its_own(&self, v: &Value) -> bool {
        let origin = v
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str);
        let id = v.get("promptId").and_then(Value::as_str);
        let running = self
            .held
            .last()
            .map_or(self.prompt_id.as_deref(), |c| c.prompt_id.as_deref());
        begins_unnumbered(v)
            || origin.is_some_and(|k| k != "human")
            || id.is_some_and(|id| running != Some(id))
    }

    fn turn(&self) -> LastTurn {
        let opened = self.opened.is_some();
        LastTurn {
            last: self.last.clone().flatten(),
            opened: self.opened.clone(),
            opened_said: self.said.filter(|_| opened),
            ended: self.ended.filter(|_| opened),
            opened_ms: self.opened_ms.filter(|_| opened),
            asked: self.held.iter().filter_map(|c| c.key.clone()).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalRow {
    Command,
    Output,
}

fn local_row(words: &str) -> Option<LocalRow> {
    if ["<command-name>", "<command-message>", BASH_INPUT]
        .iter()
        .any(|tag| words.starts_with(tag))
    {
        Some(LocalRow::Command)
    } else if ["<local-command-", BASH_STDOUT, BASH_STDERR]
        .iter()
        .any(|tag| words.starts_with(tag))
    {
        Some(LocalRow::Output)
    } else {
        None
    }
}

fn turn_key_of(v: &Value) -> Option<Option<String>> {
    let uuid = || v.get("uuid").and_then(Value::as_str).map(str::to_owned);
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let model = v
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str);
            if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
                || model.is_some_and(|m| !is_model_id(m))
            {
                return None;
            }
            Some(uuid().map(|u| format!("answer:{u}")))
        }
        Some("user") => {
            if v.get("isMeta").and_then(Value::as_bool) == Some(true)
                || v.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
                || v.get("queueTranscriptOnly").and_then(Value::as_bool) == Some(true)
                || only_tool_results(v)
                || first_text(v).is_none_or(|t| t.trim().is_empty())
                || first_text(v).map(typed_words).and_then(local_row) == Some(LocalRow::Output)
            {
                return None;
            }
            Some(uuid().map(|u| format!("prompt:{u}")))
        }
        _ => None,
    }
}

fn asked_by(row: &Value, ours: &[Typed]) -> Asked {
    if origin_is_someone(row) {
        return Asked::Theirs;
    }
    if matches!(
        row.get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str),
        Some("task-notification" | "auto-continuation")
    ) {
        return Asked::Nobody;
    }
    let words = first_text(row).map(typed_words);
    if words.is_some_and(|t| t.starts_with(TASK_NOTIFICATION)) {
        return Asked::Nobody;
    }
    if words.is_some_and(|t| t.starts_with(BASH_INPUT)) {
        return Asked::Command;
    }
    if words.is_some_and(|t| t.starts_with(BASH_STDOUT) || t.starts_with(BASH_STDERR)) {
        return Asked::Nobody;
    }
    if words.is_some_and(is_done_check) {
        return Asked::Check;
    }
    if words.is_some_and(is_land_check) {
        return Asked::Land;
    }
    if words.is_some_and(turn_end::is_challenge_text) {
        return Asked::Challenge;
    }
    match prompt_of(row, ours) {
        Prompt::Theirs => Asked::Theirs,
        Prompt::Command => Asked::Command,
        Prompt::Harness | Prompt::LocalOutput => Asked::Nobody,
        Prompt::None => Asked::Nothing,
    }
}

fn is_done_check(text: &str) -> bool {
    let checks = || std::iter::once(turn_end::DONE_CHECK).chain(turn_end::RETIRED_DONE_CHECKS);
    checks().any(|check| opens_with(check, text))
        || (text.starts_with(HARNESS_MARK) && {
            let words: Vec<&str> = text.split_whitespace().collect();
            checks().any(|check| {
                let check: Vec<&str> = check.split_whitespace().collect();
                words.windows(check.len()).any(|w| w == check.as_slice())
            })
        })
}

fn is_land_check(text: &str) -> bool {
    [turn_end::LAND, turn_end::LAND_NUDGE]
        .into_iter()
        .chain(turn_end::RETIRED_LANDS)
        .chain(turn_end::RETIRED_LAND_NUDGES)
        .any(|head| opens_with(head, text))
}

/// Returns whether `text` opens an Autopilot Land instruction or nudge.
pub(crate) fn is_land_act(text: &str) -> bool {
    is_land_check(text)
        || std::iter::once(turn_end::LAND_ANSWER)
            .chain(turn_end::RETIRED_LAND_ANSWERS)
            .any(|answer| opens_with(answer, text))
}

fn vendor_continues(row: &Value) -> bool {
    row.get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str)
        == Some("auto-continuation")
}

fn lands_answer(row: &Value, ours: &[Typed]) -> bool {
    row.get("isMeta").and_then(Value::as_bool) != Some(true)
        && first_text(row).map(typed_words).is_some_and(|t| {
            std::iter::once(turn_end::LAND_ANSWER)
                .chain(turn_end::RETIRED_LAND_ANSWERS)
                .any(|answer| opens_with(answer, t))
                && typed_by(ours, t, recorded_ms(row), |o| {
                    o.rule == turn_end::RULE_LAND_ANSWER
                })
        })
}

fn supervisors_plain(row: &Value, ours: &[Typed]) -> bool {
    row.get("isMeta").and_then(Value::as_bool) != Some(true)
        && first_text(row).map(typed_words).is_some_and(|t| {
            !t.starts_with('<')
                && typed_by(ours, t, recorded_ms(row), |o| {
                    turn_end::is_wall_retry(&o.rule)
                })
        })
}

fn opens_with(check: &str, text: &str) -> bool {
    let mut words = text.split_whitespace();
    check.split_whitespace().all(|w| words.next() == Some(w))
}

pub(crate) const MISSED_TASK_NOTICE: &str = "The following one-shot scheduled task";

fn begins_unnumbered(row: &Value) -> bool {
    begins_unnumbered_by(
        |key| row.get(key).is_some_and(|v| !v.is_null()),
        row.get("isMeta").and_then(Value::as_bool) == Some(true),
        first_text(row),
    )
}

/// Part-wise check for whether a user row begins an unnumbered operator turn.
pub(crate) fn begins_unnumbered_by(
    named: impl Fn(&str) -> bool,
    meta: bool,
    lead: Option<&str>,
) -> bool {
    named(TURN_ORIGIN)
        || named(SCHEDULED_TASK_ID)
        || (meta && lead.is_some_and(|t| t.trim_start().starts_with(MISSED_TASK_NOTICE)))
}

pub(crate) const TURN_ORIGIN: &str = "turnOrigin";
pub(crate) const SCHEDULED_TASK_ID: &str = "scheduledTaskId";

/// Returns whether `id` starts a newly numbered turn distinct from `running`.
pub(crate) fn numbered_anew<T: PartialEq>(
    id: Option<T>,
    running: Option<T>,
    unnumbered: impl FnOnce() -> bool,
) -> bool {
    match id {
        Some(id) => running != Some(id),
        None => unnumbered(),
    }
}

fn first_text(row: &Value) -> Option<&str> {
    match row.get("message")?.get("content")? {
        Value::String(t) => Some(t.as_str()),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .find_map(|p| p.get("text").and_then(Value::as_str)),
        _ => None,
    }
}

fn only_tool_results(row: &Value) -> bool {
    row.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            !parts.is_empty()
                && parts
                    .iter()
                    .all(|p| p.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

fn answer_words(message: &Value) -> Option<String> {
    let texts: Vec<&str> = match message.get("content")? {
        Value::String(t) => vec![t.as_str()],
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect(),
        _ => return None,
    };
    let texts: Vec<&str> = texts.into_iter().filter(|t| !t.trim().is_empty()).collect();
    (!texts.is_empty()).then(|| texts.join("\n\n"))
}

enum Turn {
    Notice(Value),
    Direction,
    Agent(Value),
}

fn turn_of(line: &str) -> Option<Turn> {
    if !line.contains("\"user\"") && !line.contains("assistant") {
        return None;
    }
    let mut v = super::transcript::conversation(line).ok()?;
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let message = v.as_object_mut()?.remove("message").unwrap_or_default();
            let theirs = message
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|m| !is_model_id(m));
            (!theirs).then_some(Turn::Agent(message))
        }
        Some("user") => {
            let message = v.get("message")?;
            if is_announcement(message) {
                return Some(Turn::Notice(message.clone()));
            }
            if v.get("isMeta").and_then(Value::as_bool) == Some(true)
                || v.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
                || v.get("queueTranscriptOnly").and_then(Value::as_bool) == Some(true)
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

fn notice_matches(message: &Value, marker: &str) -> bool {
    !marker.is_empty() && answer_words(message).is_some_and(|words| words.contains(marker))
}

fn answers(message: &Value, marker: &str, matching_notice: bool) -> bool {
    let Some(words) = answer_words(message) else {
        return false;
    };
    if !marker.is_empty() && words.lines().any(|line| is_ready_line(line, marker)) {
        return true;
    }
    matching_notice
        && message
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(is_model_id)
        && message.get("stop_reason").and_then(Value::as_str) == Some("end_turn")
        && readiness::says_ready(&words)
}

/// Returns whether the agent's readiness reply after `markers` was superseded by new work.
#[must_use]
pub fn ready_superseded(jsonl: &str, markers: &[String]) -> bool {
    let (mut answered, mut superseded) = (false, false);
    let mut matching_notice = false;
    for line in jsonl.lines() {
        match turn_of(line) {
            Some(Turn::Notice(message)) => {
                (answered, superseded) = (false, false);
                matching_notice = markers.iter().any(|m| notice_matches(&message, m));
            }
            Some(Turn::Direction) => {
                superseded |= answered;
                matching_notice = false;
            }
            Some(Turn::Agent(message))
                if markers
                    .iter()
                    .any(|m| answers(&message, m, matching_notice)) =>
            {
                (answered, superseded) = (true, false);
            }
            Some(Turn::Agent(_)) | None => {}
        }
    }
    superseded
}

/// Strips Claude Code's `<pasted_content>` wrapper to recover the typed text.
pub(crate) fn typed_words(text: &str) -> &str {
    let t = text.trim_start();
    let Some((tag, body)) = t
        .strip_prefix("<pasted_content")
        .and_then(|rest| rest.split_once('>'))
    else {
        return t;
    };
    if tag.contains('\n') {
        return t;
    }
    let body = body.split_once("</pasted_content").map_or(body, |(b, _)| b);
    body.trim()
}

fn is_direction(text: &str) -> bool {
    let t = typed_words(text);
    if t.is_empty() || t.starts_with("[aterm harness]") {
        return false;
    }
    !t.starts_with('<') || t.starts_with(BASH_INPUT)
}

fn is_announcement(message: &Value) -> bool {
    let head = |t: &str| typed_words(t).starts_with(ANNOUNCE_HEAD);
    match message.get("content") {
        Some(Value::String(t)) => head(t),
        Some(Value::Array(parts)) => parts.iter().any(|part| {
            part.get("type").and_then(Value::as_str) == Some("text")
                && part.get("text").and_then(Value::as_str).is_some_and(head)
        }),
        _ => false,
    }
}

fn is_auth_wall_row(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("assistant")
        && v.get("isSidechain").and_then(Value::as_bool) != Some(true)
        && v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        && v.get("error").and_then(Value::as_str) == Some("authentication_failed")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoginRow {
    Wall,
    Lifted,
}

fn login_succeeded(text: &str) -> bool {
    text.trim_start()
        .starts_with("<local-command-stdout>Login successful")
}

fn login_row(line: &str) -> Option<(LoginRow, Option<u64>)> {
    if !line.contains("authentication_failed")
        && !line.contains("Login successful")
        && !line.contains("\"assistant\"")
    {
        return None;
    }
    let v = super::transcript::conversation(line).ok()?;
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
}

/// Incremental transcript scanner for login-wall state and lift timestamp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoginScan {
    last: Option<LoginRow>,
    walled: bool,
    lifted: Option<u64>,
}

impl LoginScan {
    /// Scans all lines in `jsonl`.
    #[must_use]
    pub fn of(jsonl: &str) -> Self {
        let mut scan = Self::default();
        for line in jsonl.lines() {
            scan.line(line);
        }
        scan
    }

    /// Folds one JSONL transcript line into the login scan.
    pub fn line(&mut self, line: &str) {
        let Some((row, at)) = login_row(line) else {
            return;
        };
        self.last = Some(row);
        match row {
            LoginRow::Wall => (self.walled, self.lifted) = (true, None),
            LoginRow::Lifted if self.walled => (self.walled, self.lifted) = (false, at),
            LoginRow::Lifted => {}
        }
    }

    /// Returns whether the scanned transcript currently stands at a login wall.
    #[must_use]
    pub fn wall(&self) -> bool {
        self.last == Some(LoginRow::Wall)
    }

    /// Returns the Unix timestamp when the last login wall was lifted.
    #[must_use]
    pub const fn lifted_at(&self) -> Option<u64> {
        self.lifted
    }
}

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

/// Returns whether `jsonl` ends at an unlifted login wall.
#[must_use]
pub fn transcript_login_wall(jsonl: &str) -> bool {
    LoginScan::of(jsonl).wall()
}

/// Returns the Unix timestamp when the last login wall in `jsonl` was lifted.
#[must_use]
pub fn login_lifted_at(jsonl: &str) -> Option<u64> {
    LoginScan::of(jsonl).lifted_at()
}

/// Returns whether the latest notice with `marker` was blocked by a login wall.
#[must_use]
pub fn notice_fate(jsonl: &str, marker: &str) -> Option<bool> {
    notice_fate_of(jsonl, marker).map(|fate| fate == NoticeFate::Wall)
}

/// Delivery fate of an upgrade notice in the transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeFate {
    Open,
    Answered,
    Wall,
    Queued,
    Taken(Option<u64>),
}

/// Determines the [`NoticeFate`] of the latest notice carrying `marker` in `jsonl`.
#[must_use]
pub fn notice_fate_of(jsonl: &str, marker: &str) -> Option<NoticeFate> {
    notice_scan(jsonl, marker).map(|scan| scan.fate)
}

/// Returns the usage-limit reset timestamp holding up a queued notice with `marker`.
#[must_use]
pub fn queued_until(jsonl: &str, marker: &str) -> Option<u64> {
    notice_scan(jsonl, marker)?.queued_until()
}

/// Counts untaken upgrade notices queued while `marker` is held by a usage limit.
#[must_use]
pub fn queued_copies(jsonl: &str, marker: &str) -> u32 {
    notice_scan(jsonl, marker).map_or(0, |scan| scan.queued_copies())
}

/// Transcript scan results for the latest upgrade notice carrying a marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scan {
    pub fate: NoticeFate,
    pub until: Option<u64>,
    pub untaken: u32,
    pub answered_at: Option<u64>,
    pub typed_at: Option<u64>,
}

impl Scan {
    /// Usage-limit reset timestamp holding up the queued notice, if any.
    #[must_use]
    pub fn queued_until(&self) -> Option<u64> {
        (self.fate == NoticeFate::Queued).then(|| self.until.unwrap_or(0))
    }

    /// Number of untaken upgrade notices waiting in the queue.
    #[must_use]
    pub fn queued_copies(&self) -> u32 {
        if self.fate == NoticeFate::Queued {
            self.untaken
        } else {
            0
        }
    }

    /// Timestamp until which a full notice queue rests before re-queuing.
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

pub const QUEUE_REST_MAX_S: u64 = 24 * 3_600;

pub const QUEUE_REST_DOUBLINGS: u32 = {
    let mut k = 0;
    while RETRY_S << k < QUEUE_REST_MAX_S {
        k += 1;
    }
    k
};

/// Backoff duration in seconds before typing another notice into a queue of `untaken` copies.
#[must_use]
pub fn queue_rest(untaken: u32) -> u64 {
    let over = untaken.saturating_sub(REQUEUE_MAX + 1);
    if over >= QUEUE_REST_DOUBLINGS {
        QUEUE_REST_MAX_S
    } else {
        (RETRY_S << over).min(QUEUE_REST_MAX_S)
    }
}

/// Scans `jsonl` for the fate and queue state of the latest notice carrying `marker`.
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
        let Ok(v) = super::transcript::conversation(line) else {
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

fn row_time(v: &Value) -> Option<u64> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(super::upgrade_models::parse_utc)
}

fn limit_holds_until(v: &Value) -> Option<u64> {
    v.get("quotaLimits")
        .and_then(|q| q.get("resetsAt"))
        .and_then(Value::as_u64)
        .or_else(|| row_time(v).map(|at| at.saturating_add(REASK_S)))
}

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
    row_texts(v)
        .any(|text| aterm_phase::classify_wall(text).is_some_and(|kind| kind.reads_limited()))
}

fn row_texts(v: &Value) -> impl Iterator<Item = &str> {
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
}

fn is_spend_row(v: &Value) -> bool {
    row_texts(v).any(|text| aterm_phase::classify_wall(text) == Some(aterm_phase::WallKind::Spend))
}

/// Returns the timestamp until which the session stands at a usage limit in `jsonl`.
#[must_use]
pub fn transcript_limit_until(jsonl: &str) -> Option<u64> {
    StandingScan::of(jsonl).until()
}

/// Returns the raw reset timestamp named by the standing usage limit in `jsonl`.
#[must_use]
pub fn transcript_limit_reset(jsonl: &str) -> Option<u64> {
    StandingScan::of(jsonl).reset()
}

fn reset_named(v: &Value) -> Option<u64> {
    v.get("quotaLimits")
        .and_then(|q| q.get("resetsAt"))
        .and_then(Value::as_u64)
        .filter(|&at| at > 0)
}

/// Returns whether the standing usage limit in `jsonl` is a spend or credit wall.
#[must_use]
pub fn transcript_limit_spend(jsonl: &str) -> bool {
    StandingScan::of(jsonl).spend()
}

pub const LIMIT_TYPE_MAX: usize = 64;

const CREDIT_LIMIT_TYPE: &str = "overage";

fn is_limit_type(word: &str) -> bool {
    (1..=LIMIT_TYPE_MAX).contains(&word.len())
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Quota wall details extracted from a session's standing limit transcript row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandingWall {
    pub at: u64,
    pub until: Option<u64>,
    pub window: Option<String>,
    pub credits: Option<CreditsWord>,
}

/// Usage-credit status reported on a quota limit row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreditsWord {
    On,
    Off {
        reason: Option<String>,
        until: Option<u64>,
    },
}

impl CreditsWord {
    fn of(quota: &Value) -> Option<CreditsWord> {
        match quota.get("overageStatus").and_then(Value::as_str)? {
            "allowed" | "allowed_warning" => Some(CreditsWord::On),
            "rejected" => Some(CreditsWord::Off {
                reason: quota
                    .get("overageDisabledReason")
                    .and_then(Value::as_str)
                    .filter(|word| is_limit_type(word))
                    .map(str::to_owned),
                until: quota
                    .get("overageResetsAt")
                    .and_then(Value::as_u64)
                    .filter(|&at| at > 0),
            }),
            _ => None,
        }
    }
}

/// Extracts the [`StandingWall`] from the transcript tail `jsonl`, if one stands.
#[must_use]
pub fn transcript_standing_wall(jsonl: &str) -> Option<StandingWall> {
    StandingScan::of(jsonl).wall()
}

fn standing_wall_of(v: &Value) -> Option<StandingWall> {
    let quota = v.get("quotaLimits")?;
    if quota.get("status").and_then(Value::as_str) != Some("rejected") || is_spend_row(v) {
        return None;
    }
    let named = quota
        .get("rateLimitType")
        .and_then(Value::as_str)
        .filter(|word| is_limit_type(word));
    if named == Some(CREDIT_LIMIT_TYPE) {
        return None;
    }
    Some(StandingWall {
        at: row_time(v)?,
        until: quota
            .get("resetsAt")
            .and_then(Value::as_u64)
            .filter(|&at| at > 0),
        window: named
            .or_else(|| row_texts(v).find_map(super::usage::notice_window))
            .map(str::to_owned),
        credits: CreditsWord::of(quota),
    })
}

/// Standing usage-limit row from a session transcript before account rotation checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandingLimit {
    pub text: String,
    pub until: Option<u64>,
    pub window: Option<String>,
    pub at: Option<u64>,
    pub row: Option<String>,
}

/// Extracts the [`StandingLimit`] from the transcript tail `jsonl`, if one stands.
#[must_use]
pub fn transcript_standing_limit(jsonl: &str) -> Option<StandingLimit> {
    StandingScan::of(jsonl).limit()
}

fn standing_limit_of(v: &Value) -> Option<StandingLimit> {
    let quota = v.get("quotaLimits");
    Some(StandingLimit {
        text: row_texts(v).collect::<Vec<_>>().join(" "),
        until: quota
            .and_then(|q| q.get("resetsAt"))
            .and_then(Value::as_u64)
            .filter(|&at| at > 0),
        window: quota
            .and_then(|q| q.get("rateLimitType"))
            .and_then(Value::as_str)
            .filter(|word| is_limit_type(word))
            .map(str::to_owned),
        at: row_time(v),
        row: v.get("uuid").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Single-pass transcript scanner for standing usage limits and quota walls.
#[derive(Clone, Debug, Default)]
pub struct StandingScan {
    row: Option<Value>,
    continued: bool,
}

impl StandingScan {
    /// Scans all lines in `jsonl`.
    #[must_use]
    pub fn of(jsonl: &str) -> Self {
        let mut scan = Self::default();
        for line in jsonl.lines() {
            scan.line(line);
        }
        scan
    }

    /// Folds one JSONL transcript line into the standing-limit scan.
    pub fn line(&mut self, line: &str) {
        if !line.contains("assistant")
            && !line.contains("Login successful")
            && !line.contains("auto-continuation")
        {
            return;
        }
        let Ok(v) = super::transcript::conversation(line) else {
            return;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        if is_login_success_row(&v) {
            (self.row, self.continued) = (None, false);
            return;
        }
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                self.continued |= self.row.is_some() && vendor_continues(&v);
                return;
            }
            Some("assistant") => {}
            _ => return,
        }
        if is_limit_row(&v) {
            (self.row, self.continued) = (Some(v), false);
        } else if v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .is_some_and(is_model_id)
        {
            (self.row, self.continued) = (None, false);
        }
    }

    fn read<T>(&self, read: impl Fn(&Value) -> Option<T>) -> Option<T> {
        self.row.as_ref().and_then(read)
    }

    /// Returns [`transcript_limit_until`] for the scanned rows.
    #[must_use]
    pub fn until(&self) -> Option<u64> {
        self.read(limit_holds_until)
    }

    /// Returns [`transcript_limit_reset`] for the scanned rows.
    #[must_use]
    pub fn reset(&self) -> Option<u64> {
        self.read(reset_named)
    }

    /// Returns [`transcript_limit_spend`] for the scanned rows.
    #[must_use]
    pub fn spend(&self) -> bool {
        self.read(|v| is_spend_row(v).then_some(())).is_some()
    }

    /// Returns [`transcript_standing_wall`] for the scanned rows.
    #[must_use]
    pub fn wall(&self) -> Option<StandingWall> {
        self.read(standing_wall_of)
    }

    /// Returns [`transcript_standing_limit`] for the scanned rows.
    #[must_use]
    pub fn limit(&self) -> Option<StandingLimit> {
        if self.continued {
            return None;
        }
        self.read(standing_limit_of)
    }
}

/// Returns the timestamp of the newest assistant answer in `jsonl` at or before `by`.
#[must_use]
pub fn transcript_last_answer(jsonl: &str, by: u64) -> Option<u64> {
    jsonl
        .lines()
        .filter(|line| line.contains("assistant"))
        .filter_map(|line| {
            let v = super::transcript::conversation(line).ok()?;
            let answer = v.get("type").and_then(Value::as_str) == Some("assistant")
                && (is_limit_row(&v)
                    || v.get("message")
                        .and_then(|m| m.get("model"))
                        .and_then(Value::as_str)
                        .is_some_and(is_model_id));
            answer.then(|| row_time(&v)).flatten()
        })
        .filter(|at| *at <= by)
        .max()
}

const LIMIT_DIALOG_COMMAND: &str = "/rate-limit-options";

const COMMAND_STDOUT: (&str, &str) = ("<local-command-stdout>", "</local-command-stdout>");

pub const RESET_LINE_MAX: usize = 400;

fn local_command_text(v: &Value) -> Option<&str> {
    match v.get("type").and_then(Value::as_str) {
        Some("user") => v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str),
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("local_command") => {
            v.get("content").and_then(Value::as_str)
        }
        _ => None,
    }
}

/// Extracts the last usage-limit dialog stdout line written at or after `since`.
#[must_use]
pub fn transcript_reset_line(jsonl: &str, since: u64) -> Option<String> {
    let mut dialog = false;
    let mut last = None;
    for line in jsonl.lines() {
        if !line.contains("<command-") && !line.contains("<local-command-") {
            continue;
        }
        let Ok(v) = super::transcript::conversation(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(text) = local_command_text(&v).map(str::trim_start) else {
            continue;
        };
        if text.starts_with("<command-name>") || text.starts_with("<command-message>") {
            dialog = command_line(text) == LIMIT_DIALOG_COMMAND;
        } else if let Some(said) = text.strip_prefix(COMMAND_STDOUT.0) {
            if std::mem::take(&mut dialog) && row_time(&v).is_some_and(|at| at >= since) {
                let said = said.trim_end();
                let said = said.strip_suffix(COMMAND_STDOUT.1).unwrap_or(said);
                last = Some(super::one_line(said, RESET_LINE_MAX));
            }
        } else if text.starts_with("<local-command-stderr>") {
            dialog = false;
        }
    }
    last
}

/// Login-wall state derived from a transcript tail for an upgrade in `phase`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptLogin {
    pub stands: bool,
    pub lifted_at: Option<u64>,
    pub undelivered: Option<bool>,
}

/// Computes [`TranscriptLogin`] for `phase` and `marker` from transcript `tail`.
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

/// Returns whether `rows` show a login wall on `agent`'s screen.
#[must_use]
pub fn login_wall(agent: Agent, rows: &[String]) -> bool {
    agent
        .program()
        .reader()
        .wall(rows)
        .is_some_and(|w| w.kind == aterm_phase::WallKind::Auth)
}

/// Computes the ask count for the next [`Step::Announce`] in `phase`.
#[must_use]
pub fn announce_asks(phase: &Phase, undelivered: bool) -> u32 {
    match phase {
        Phase::Announced { asks, .. } if undelivered => (*asks).max(1),
        Phase::Announced { asks, .. } => asks.saturating_add(1),
        _ => 1,
    }
}

/// Reconstructs all READY markers minted in a round up to `salt`.
#[must_use]
pub fn round_markers(session_id: &str, to: &Version, salt: u64) -> Vec<String> {
    (1..=MAX_ASKS)
        .map(|asks| ready_marker(session_id, to, salt.wrapping_add(u64::from(asks))))
        .collect()
}

/// Counts how many notices in `markers` reached the model in `jsonl`.
#[must_use]
pub fn notices_received(jsonl: &str, markers: &[String]) -> u32 {
    let received = markers
        .iter()
        .filter(|m| notice_fate(jsonl, m) != Some(true))
        .count();
    u32::try_from(received).unwrap_or(u32::MAX)
}

/// Rearms a given-up phase when its notices were undelivered or fewer than `MAX_ASKS` reached the model.
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

pub const HARNESS_MARK: &str = harness_mark!();

const _: () = {
    const fn opens_with_a_card_key(text: &str) -> bool {
        matches!(
            text.as_bytes()[0],
            b'0'..=b'9' | b'y' | b'n' | b'd' | b'Y' | b'N' | b'D'
        )
    }
    assert!(!opens_with_a_card_key(HARNESS_MARK));
    assert!(!opens_with_a_card_key(ANNOUNCE_HEAD));
    assert!(!opens_with_a_card_key(RELEASE_HEAD));
};

/// Incremental transcript scanner for whether a conversation has been given a task.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskScan {
    command: bool,
    tasked: bool,
    ours: Vec<Typed>,
}

pub const TYPED_WITHIN_MS: u64 = 60_000;

impl TaskScan {
    /// Registers prompts typed by the session supervisor along with their timestamps.
    pub fn supervisor_typed(&mut self, ours: &[Typed]) {
        ours.clone_into(&mut self.ours);
    }

    /// Folds one JSONL transcript line and returns whether the conversation has a task.
    pub fn line(&mut self, line: &str) -> bool {
        if self.tasked
            || !(line.contains("\"user\"")
                || line.contains("\"assistant\"")
                || line.contains("\"local_command\""))
        {
            return self.tasked();
        }
        let Ok(v) = super::transcript::conversation(line) else {
            return self.tasked();
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return self.tasked();
        }
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => self.tasked = self.command,
            Some("user") if origin_is_someone(&v) => self.tasked = true,
            Some("user") => match prompt_of(&v, &self.ours) {
                Prompt::Theirs => self.tasked = true,
                Prompt::Command => self.command = true,
                Prompt::Harness | Prompt::LocalOutput => self.command = false,
                Prompt::None => {}
            },
            Some("system")
                if local_command_text(&v)
                    .is_some_and(|t| t.trim_start().starts_with("<local-command-")) =>
            {
                self.command = false;
            }
            _ => {}
        }
        self.tasked()
    }

    /// Returns whether the scanned transcript has a task.
    #[must_use]
    pub const fn tasked(&self) -> bool {
        self.tasked || self.command
    }

    /// Returns whether the task state is permanently settled.
    #[must_use]
    pub const fn settled(&self) -> bool {
        self.tasked
    }
}

fn origin_is_someone(row: &Value) -> bool {
    row.get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| !matches!(kind, "human" | "task-notification" | "auto-continuation"))
}

/// Runs [`TaskScan`] over a complete transcript slice.
#[must_use]
pub fn transcript_tasked(jsonl: &str, ours: &[Typed]) -> bool {
    let mut scan = TaskScan::default();
    scan.supervisor_typed(ours);
    for line in jsonl.lines() {
        if scan.line(line) && scan.settled() {
            break;
        }
    }
    scan.tasked()
}

enum Prompt {
    Harness,
    Theirs,
    Command,
    LocalOutput,
    None,
}

fn prompt_of(row: &Value, ours: &[Typed]) -> Prompt {
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
    let Some(text) = text.map(typed_words) else {
        return Prompt::Theirs;
    };
    let supervisors = |said: &str| typed_by(ours, said, recorded_ms(row), |_| true);
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
    } else if text.starts_with(INTERRUPTED) {
        Prompt::None
    } else if supervisors(text) {
        Prompt::Harness
    } else {
        Prompt::Theirs
    }
}

fn recorded_ms(row: &Value) -> Option<u64> {
    row.get("timestamp")
        .and_then(Value::as_str)
        .and_then(models::parse_utc)
        .map(|s| s.saturating_mul(1000))
}

fn typed_by(ours: &[Typed], said: &str, at_ms: Option<u64>, of: impl Fn(&Typed) -> bool) -> bool {
    at_ms.is_some_and(|at| {
        ours.iter().any(|o| {
            same_words(&o.text, said)
                && u64::try_from(o.at_ms).is_ok_and(|t| t.abs_diff(at) <= TYPED_WITHIN_MS)
                && of(o)
        })
    })
}

fn same_words(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

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

/// Per-session observations gathered by the upgrade driver for gate evaluation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub status: String,
    pub status_age_s: u64,
    pub composer_empty: bool,
    pub approval_box: bool,
    pub busy_footer: bool,
    pub background: Vec<String>,
    pub held: bool,
    pub quiet_s: u64,
    pub hold_s: u64,
    pub owner_now: bool,
    pub attended: bool,
    pub background_point: bool,
    pub taskless: bool,
    pub limited: bool,
    pub login: bool,
    pub undelivered: bool,
    pub queued: bool,
    pub ready_s: u64,
    pub idle_looks: u32,
    pub failed_s: u64,
    pub work_ended: bool,
    pub ready_unrecorded: bool,
    pub ready_superseded: bool,
    pub quiet: bool,
    pub unnoticed: bool,
    pub rung: Rung,
    pub typing: bool,
    pub still_s: u64,
}

/// Escalation rung on the upgrade urgency ladder based on how long a session is behind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    #[default]
    Prefer,
    Settled,
    KeysOnly,
    Land,
}

impl Rung {
    /// Short identifier word for `--status`, ledger rows, and UI tables.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Rung::Prefer => "prefer",
            Rung::Settled => "settled",
            Rung::KeysOnly => "keys-only",
            Rung::Land => "land",
        }
    }
}

pub const RUNG_SETTLED_S: u64 = 20 * 60;

pub const RUNG_KEYS_S: u64 = 3_600;

pub const RUNG_LAND_S: u64 = 2 * 3_600;

pub const KEYS_GAP_S: u32 = 20;

const _: () = assert!(RUNG_SETTLED_S < RUNG_KEYS_S && RUNG_KEYS_S < RUNG_LAND_S);
const _: () = assert!(KEYS_GAP_S as u64 <= QUIET_S);

/// Fingerprint of screen rows above `composer_row` for repaint-proof settle detection.
#[must_use]
pub fn still_print(pid: u32, rows: &[String], composer_row: usize) -> String {
    let above = rows.get(..composer_row).unwrap_or(rows);
    let kept = above
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .map_or(0, |i| i + 1);
    let mut h = aterm_hash::Fnv1a64::new();
    h.write(&pid.to_le_bytes());
    for row in &above[..kept] {
        h.write(row.trim_end().as_bytes());
        h.write(b"\n");
    }
    format!("{:016x}", h.finish())
}

#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeLadder",
        action = "Advance",
        project = "aterm_agent::harness::upgrade_drive::ladder_projection"
    )
)]
/// Maps seconds behind the target build to an escalation [`Rung`].
pub fn rung(behind_s: u64) -> Rung {
    if behind_s >= RUNG_LAND_S {
        Rung::Land
    } else if behind_s >= RUNG_KEYS_S {
        Rung::KeysOnly
    } else if behind_s >= RUNG_SETTLED_S {
        Rung::Settled
    } else {
        Rung::Prefer
    }
}

/// Effective [`Rung`] for `f` (`Rung::Land` under `owner_now`, otherwise [`rung`]).
#[must_use]
pub const fn rung_in_force(f: &Facts) -> Rung {
    if f.owner_now { Rung::Land } else { f.rung }
}

/// Human-input grace window in seconds in force at `rung`.
#[must_use]
pub fn person_grace(rung: Rung, grace_s: u32) -> u32 {
    if rung >= Rung::KeysOnly {
        grace_s.min(KEYS_GAP_S)
    } else {
        grace_s
    }
}

pub const IDLE_LOOKS: u32 = 2;

/// Returns whether a Claude Code screen shows a standing usage limit.
#[must_use]
pub fn limited(rows: &[String]) -> bool {
    aterm_phase::limit_notice(rows).is_some()
}

/// Result of an upgrade gate check (`Proceed` or `Wait(reason)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    Go,
    Wait(&'static str),
}

pub const QUIET_S: u64 = 20;

pub const QUIET_SEAM: &str = "ATERM_UPGRADE_QUIET_S";

/// Required idle settle duration in seconds (honoring `ATERM_UPGRADE_QUIET_S` in dev builds).
#[must_use]
pub fn quiet_s() -> u64 {
    static SETTLE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SETTLE.get_or_init(|| {
        aterm_types::dev_seam!(QUIET_SEAM)
            .and_then(|v| v.to_str()?.trim().parse::<u64>().ok())
            .filter(|&s| s > 0)
            .unwrap_or(QUIET_S)
    })
}

/// Returns whether `human` input timestamps mark the session as actively attended within `grace_s`.
#[must_use]
pub fn attended_by(human: crate::supervise::screen::HumanInput, grace_s: u32) -> bool {
    human.within(grace_s).unwrap_or(true)
}

/// Evaluates whether an upgrade announcement may be typed into the session now.
#[must_use]
pub fn gate_announce(f: &Facts) -> Gate {
    gate(f, status_lags(f))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeLadder",
        action = "Move",
        project = "aterm_agent::harness::upgrade_drive::ladder_projection"
    )
)]
fn gate(f: &Facts, lags: bool) -> Gate {
    let rung = rung_in_force(f);
    if f.held {
        return Gate::Wait("held");
    }
    if f.typing || (f.attended && rung < Rung::KeysOnly) {
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
    let own_work = matches!(f.status.as_str(), "busy" | "shell");
    let at_break = (f.background_point || lags) && (f.status == "idle" || own_work);
    if f.status != "idle" && !at_break {
        return Gate::Wait(if own_work && f.idle_looks > 0 {
            "status-stale"
        } else {
            "not-idle"
        });
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
    if rung == Rung::Land || f.background_point {
        return Gate::Go;
    }
    let quiet = quiet_s();
    let settled = (f.status_age_s >= quiet && f.quiet_s >= quiet)
        || (rung >= Rung::Settled && f.still_s >= quiet);
    if !settled {
        return Gate::Wait("settling");
    }
    Gate::Go
}

/// Returns whether Claude's status line lags a completed turn (`busy` or `shell`).
#[must_use]
pub fn status_lags(f: &Facts) -> bool {
    matches!(f.status.as_str(), "busy" | "shell")
        && f.status_age_s >= quiet_s()
        && f.idle_looks >= IDLE_LOOKS
}

#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeProgress",
        action = "RestartAtIdle",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
/// Evaluates whether the session may be exited and relaunched now.
pub fn gate_restart(f: &Facts, ready: bool) -> Gate {
    if !ready {
        return Gate::Wait("not-ready");
    }
    restart_gate(f, false)
}

fn restart_gate(f: &Facts, lags: bool) -> Gate {
    if !f.background.is_empty() {
        return Gate::Wait("background");
    }
    match gate(f, lags) {
        Gate::Go if f.background_point => Gate::Wait("background"),
        gate => gate,
    }
}

pub(super) fn own_work_holds(w: &str) -> bool {
    matches!(w, "background" | "status-stale")
}

/// Returns the gate hold reason (`box`, `draft`, `attended`, `limited`, `login`) only a person clears.
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

pub const DRAIN_S: u64 = 30 * 60;

const _: () = assert!(DRAIN_S >= REASK_S);

pub const HOLD_S: u64 = 5 * 60;

pub const HOLD_GAP_S: u64 = 3 * 60;

const _: () = assert!(HOLD_GAP_S < HOLD_S);

/// Updates the `(since, seen)` timestamps for a standing human hold.
#[must_use]
pub fn hold_since(prior: (u64, u64), f: &Facts, now_s: u64) -> (u64, u64) {
    let (since, seen) = prior;
    match person_hold(f) {
        None => (0, 0),
        Some(_) if since == 0 || now_s.saturating_sub(seen) > HOLD_GAP_S => (now_s, now_s),
        Some(_) => (since, now_s),
    }
}

/// Returns whether the composer prompt is present and holds no user-typed text.
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
    dim_at_caret && !aterm_phase::phase::cursor_inside_text(cursor, caret)
}

/// Returns the screen row whose cell style distinguishes a dim placeholder from typed text.
#[must_use]
pub fn composer_dim_row(rows: &[String], cursor: Option<(usize, usize)>) -> Option<usize> {
    let (caret, lines) = aterm_phase::phase::composer_draft(rows)?;
    if lines.iter().skip(1).any(|l| !l.is_empty())
        || aterm_phase::phase::cursor_inside_text(cursor, caret)
    {
        return None;
    }
    Some(caret)
}

/// Persisted per-session state machine phase for an upgrade.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Pending,
    Announced {
        at_s: u64,
        asks: u32,
    },
    Exiting {
        at_s: u64,
    },
    Relaunched {
        at_s: u64,
    },
    Done,
    Failed(String),
}

pub const GAVE_UP: &str = "unanswered";

pub const REASK_S: u64 = 30 * 60;

pub const MAX_ASKS: u32 = 4;

pub const REQUEUE_MAX: u32 = 2;

pub const RETRY_S: u64 = MAX_ASKS as u64 * REASK_S;

const _: () = assert!(RETRY_S >= REASK_S);

pub const RETRY_BACKOFF_MAX_SHIFT: u32 = 2;

/// Additional retry backoff in seconds for the `streak`-th consecutive identical failure.
#[must_use]
pub fn rest_extension(streak: u32) -> u64 {
    let shift = streak.saturating_sub(1).min(RETRY_BACKOFF_MAX_SHIFT);
    RETRY_S.saturating_mul((1_u64 << shift) - 1)
}

/// Additional retry backoff in seconds after the `streak`-th unanswered round (`GAVE_UP`).
#[must_use]
pub const fn unanswered_rest_extension(streak: u32) -> u64 {
    let multiples = match streak {
        0 | 1 => 0,
        2 => 1,
        3 => 3,
        _ => 11,
    };
    RETRY_S.saturating_mul(multiples)
}

pub const WORK_END_FLOOR_S: u64 = 10 * 60;

pub const WORK_END_DAILY: u32 = 6;

pub const WORK_END_WINDOW_S: u64 = 24 * 60 * 60;

impl Phase {
    /// Formats the phase word for the ledger and status output.
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

/// Next driver action produced by the upgrade state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Announce,
    Wait(&'static str),
    Terminate,
    Void(&'static str),
    GiveUp,
    Fresh,
    Rearm,
}

/// Returns whether a failed phase has rested long enough to rearm.
#[must_use]
pub fn retry_due(phase: &Phase, failed_s: u64, ready: bool) -> bool {
    match phase {
        Phase::Failed(why) => !(why == GAVE_UP && ready) && failed_s >= RETRY_S,
        _ => false,
    }
}

#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeProgress",
        action = "StepAtIdle",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
/// Advances the upgrade state machine from `phase` given `f`, `ready`, and `now_s`.
pub fn next_step(phase: &Phase, f: &Facts, ready: bool, now_s: u64) -> Step {
    if unrecorded_holds(phase, f, ready) {
        return Step::Wait(READY_UNRECORDED);
    }
    if retry_due(phase, f.failed_s, ready) {
        return Step::Rearm;
    }
    if f.work_ended
        && !ready
        && !f.background_point
        && matches!(phase, Phase::Failed(why) if why == GAVE_UP)
    {
        return match gate_restart(f, true) {
            Gate::Go => Step::Rearm,
            Gate::Wait(w) => Step::Wait(w),
        };
    }
    if f.taskless && matches!(phase, Phase::Pending | Phase::Announced { .. }) {
        if f.limited {
            return Step::Wait("limited");
        }
        if f.background_point {
            return Step::Wait("background");
        }
        return match gate_restart(f, true) {
            Gate::Go if f.unnoticed => Step::Wait(NOTICE_SHOWN),
            Gate::Go => Step::Fresh,
            Gate::Wait(w) => Step::Wait(w),
        };
    }
    if f.quiet
        && let Some(step) = quiet_step(phase, f, ready, now_s)
    {
        return step;
    }
    let reannounce = || match gate_announce(f) {
        Gate::Go => Step::Announce,
        Gate::Wait(w) => Step::Wait(w),
    };
    if f.background_point && *phase != Phase::Pending {
        let Phase::Announced { at_s, asks } = phase else {
            // A given-up upgrade's late READY at a break: the agent's work
            // outliving the answer voids it here as at an idle point, so an
            // always-at-a-break session is not held on it for good.
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
        if ready && let Some(who) = person_void(f, since >= DRAIN_S) {
            return Step::Void(who);
        }
        if !ready && f.undelivered {
            return reannounce();
        }
        if !ready && f.ready_superseded {
            return reask(f, *asks);
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
                        None if own_work_holds(w) && clock(since, f, ready) >= REASK_S => {
                            reask(f, *asks)
                        }
                        None => Step::Wait(w),
                    },
                };
            }
            if f.undelivered {
                return reannounce();
            }
            if f.ready_superseded {
                return reask_superseded(f, *asks);
            }
            if f.work_ended {
                match gate_restart(f, true) {
                    Gate::Go => return Step::Announce,
                    Gate::Wait(w) if since < REASK_S => return Step::Wait(w),
                    Gate::Wait(_) => {}
                }
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
                Gate::Wait(w) => void_of(f, w, REASK_S.saturating_add(f.ready_s) >= DRAIN_S)
                    .map_or(Step::Wait(w), Step::Void),
            }
        }
        Phase::Failed(_) => Step::Wait("failed"),
    }
}

pub const NOTICE_SHOWN: &str = "notice-shown";

pub const NEXT_POINT: &str = "next-point";

/// Computes the next step for a quiet upgrade (`f.quiet`), or `None` when ask-and-drain applies.
#[must_use]
pub fn quiet_step(phase: &Phase, f: &Facts, ready: bool, now_s: u64) -> Option<Step> {
    let announced = match phase {
        Phase::Pending => None,
        Phase::Announced { at_s, .. } => Some(now_s.saturating_sub(*at_s)),
        _ => return None,
    };
    if f.limited {
        return Some(Step::Wait("limited"));
    }
    if f.login {
        return Some(Step::Wait("login"));
    }
    let wait = if f.background_point {
        "background"
    } else {
        match restart_gate(f, f.owner_now && status_lags(f)) {
            Gate::Go if f.unnoticed => return Some(Step::Wait(NOTICE_SHOWN)),
            Gate::Go => return Some(Step::Terminate),
            Gate::Wait(w) => w,
        }
    };
    if let Some(since) = announced {
        if ready && let Some(who) = person_void(f, since >= DRAIN_S) {
            return Some(Step::Void(who));
        }
        if own_work_holds(wait) && clock(since, f, ready) >= REASK_S {
            return Some(Step::GiveUp);
        }
    }
    Some(Step::Wait(wait))
}

fn person_void(f: &Facts, drained: bool) -> Option<&'static str> {
    if !drained || f.hold_s < HOLD_S {
        return None;
    }
    person_hold(f)
}

fn void_of(f: &Facts, w: &'static str, drained: bool) -> Option<&'static str> {
    person_void(f, drained)
        .or_else(|| (drained && own_work_holds(w) && f.ready_s >= DRAIN_S).then_some(w))
}

/// Tracks the timestamp since which an actionable READY reply has stood.
#[must_use]
pub const fn ready_since(prior: u64, ready: bool, f: &Facts, now_s: u64) -> u64 {
    if !ready {
        0
    } else if prior == 0 || f.limited || f.login {
        now_s
    } else {
        prior
    }
}

/// Freezes an announced upgrade's re-ask clock while held at a usage limit or login wall.
#[must_use]
pub fn clock_held(phase: &Phase, f: &Facts, now_s: u64) -> Phase {
    if f.limited || f.login || f.queued {
        clock_held_until(phase, now_s)
    } else {
        phase.clone()
    }
}

/// Advances an announced upgrade's timestamp to at least `until` after a limit lifts.
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

/// Returns whether a wait reason `why` permits typing a pending release prompt.
#[must_use]
pub fn release_is_next(why: &str) -> bool {
    matches!(
        why,
        "awaiting-ready" | "failed" | "skipped" | "deferred" | BUILD_FAILS
    )
}

pub const BUILD_FAILS: &str = "build-fails";

/// Returns whether the current round has asked the agent to hold for a move.
#[must_use]
pub fn asked_for_a_move(phase: &Phase, ready: bool) -> bool {
    match phase {
        Phase::Announced { .. } => true,
        Phase::Failed(why) => why == GAVE_UP && ready,
        Phase::Pending | Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
    }
}

/// Converts a step into a hold when the target build fails to start.
#[must_use]
pub fn build_hold(step: Step, phase: &Phase, ready: bool) -> Step {
    if asked_for_a_move(phase, ready) {
        return Step::Void(BUILD_FAILS);
    }
    match step {
        Step::Announce | Step::Terminate | Step::Fresh => Step::Wait(BUILD_FAILS),
        Step::Wait(_) | Step::Void(_) | Step::GiveUp | Step::Rearm => step,
    }
}

/// Evaluates whether a release prompt may be typed into the session now.
#[must_use]
pub fn gate_release(f: &Facts, ready: bool) -> Gate {
    if ready {
        return Gate::Wait("ready");
    }
    gate_announce(f)
}

pub const RELEASE_HEAD: &str = "[aterm harness] Upgrade off: ";

/// Returns whether `draft` is a prefix of aterm's own upgrade notice or release line.
#[must_use]
pub fn own_draft(draft: &str) -> bool {
    let compact = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    let draft = compact(draft);
    let draft = draft.trim_start_matches(['\u{276F}', '\u{203A}', '\u{00BB}']);
    draft.chars().nth(1).is_some()
        && [ANNOUNCE_HEAD, RELEASE_HEAD]
            .iter()
            .map(|head| compact(head))
            .any(|head| draft.starts_with(&head) || head.starts_with(draft))
}

/// Builds the release prompt telling an agent it may resume normal work.
#[must_use]
pub fn release_prompt(agent: Agent, quiet: bool) -> String {
    if quiet {
        return format!(
            "{RELEASE_HEAD}The {} upgrade is off for now, and nothing the notice asked of you \
             still applies. Carry on as you would have without it.",
            agent.product()
        );
    }
    format!(
        "{RELEASE_HEAD}The {} upgrade is off for now: nothing will restart this session without \
         asking you again first, and nothing the notice asked of you still applies. Carry on as \
         you would have without it.",
        agent.product()
    )
}

#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeProgress",
        action = "StepAtBreak",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
/// Downgrades a restart step to `Wait("circuit-breaker")` when the circuit breaker is open.
pub fn break_step(step: Step) -> Step {
    match step {
        Step::Announce | Step::GiveUp | Step::Void(_) | Step::Wait(_) | Step::Rearm => step,
        Step::Terminate | Step::Fresh => Step::Wait("background"),
    }
}

fn clock(since: u64, f: &Facts, ready: bool) -> u64 {
    if ready { since.min(f.ready_s) } else { since }
}

fn reask(f: &Facts, asks: u32) -> Step {
    if asks >= MAX_ASKS {
        return Step::GiveUp;
    }
    match gate_announce(f) {
        Gate::Go => Step::Announce,
        Gate::Wait(w) => Step::Wait(w),
    }
}

pub const READY_UNRECORDED: &str = "ready-unrecorded";

pub const SUPERSEDED: &str = "superseded";

pub const UNRECORDED_S: u64 = 60;

fn unrecorded_holds(phase: &Phase, f: &Facts, ready: bool) -> bool {
    let hears = match phase {
        Phase::Announced { .. } => !f.quiet,
        Phase::Failed(why) => why == GAVE_UP,
        Phase::Pending | Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
    };
    hears
        && f.ready_unrecorded
        && !ready
        && !f.background_point
        && !f.limited
        && !f.login
        && f.still_s < UNRECORDED_S
}

fn reask_superseded(f: &Facts, asks: u32) -> Step {
    let own_turn = matches!(f.status.as_str(), "busy" | "shell");
    match reask(f, asks) {
        Step::Wait("settling" | "busy") => Step::Wait(SUPERSEDED),
        Step::Wait("not-idle" | "status-stale") if own_turn => Step::Wait(SUPERSEDED),
        step => step,
    }
}

/// Operator override (`--now`, `--defer`, `--skip`) recorded for a session's upgrade.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Request {
    #[default]
    None,
    Now,
    DeferUntil(u64),
    Skip(String),
}

impl Request {
    /// Canonical state-file and JSON representation of the request.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Self::None => "-".to_string(),
            Self::Now => "now".to_string(),
            Self::DeferUntil(t) => format!("defer-until:{t}"),
            Self::Skip(v) => format!("skip:{v}"),
        }
    }

    /// Human-readable representation of the request at `now_s`.
    #[must_use]
    pub fn shown(&self, now_s: u64) -> String {
        match self {
            Self::DeferUntil(t) if *t > now_s => format!("defer:{}", span(t - now_s)),
            Self::DeferUntil(_) => "defer:due".to_string(),
            _ => self.word(),
        }
    }

    /// Returns whether this request holds an upgrade in `phase` to `target` at `now_s`.
    #[must_use]
    pub fn holds(&self, phase: &Phase, target: &str, now_s: u64) -> bool {
        matches!(phase, Phase::Pending | Phase::Announced { .. }) && self.holds_build(target, now_s)
    }

    /// Returns whether this request skips or defers upgrading to `target` at `now_s`.
    #[must_use]
    pub fn holds_build(&self, target: &str, now_s: u64) -> bool {
        match self {
            Self::Skip(v) => Version::parse(v).is_some_and(|v| Some(v) == Version::parse(target)),
            Self::DeferUntil(t) => now_s < *t,
            Self::None | Self::Now => false,
        }
    }

    /// Parses a [`Request`] from its state-file word.
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

/// Computes [`next_step`] honoring the operator's [`Request`] override for `target`.
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
        let now = Facts {
            owner_now: true,
            ..f.clone()
        };
        next_step(phase, &now, ready, now_s)
    } else {
        next_step(phase, f, ready, now_s)
    };
    rearm_held(request, step, target, now_s)
}

const fn held_word(request: &Request) -> &'static str {
    if matches!(request, Request::Skip(_)) {
        "skipped"
    } else {
        "deferred"
    }
}

/// Suppresses [`Step::Rearm`] while `request` holds the upgrade to `target` at `now_s`.
#[must_use]
pub fn rearm_held(request: &Request, step: Step, target: &str, now_s: u64) -> Step {
    if step == Step::Rearm && request.holds(&Phase::Pending, target, now_s) {
        Step::Wait(held_word(request))
    } else {
        step
    }
}

/// Returns the gate wait word holding up a step that would otherwise act.
#[must_use]
pub fn held_point(
    request: &Request,
    step: &Step,
    f: &Facts,
    now_s: u64,
    target: &str,
) -> Option<&'static str> {
    match step {
        Step::GiveUp => match gate_announce(f) {
            Gate::Go => None,
            Gate::Wait(w) => Some(w),
        },
        Step::Rearm => {
            let request = if *request == Request::Now {
                Request::None
            } else {
                request.clone()
            };
            match requested_step(&request, &Phase::Pending, f, false, now_s, target) {
                Step::Wait(w) | Step::Void(w) => Some(w),
                Step::Announce | Step::Terminate | Step::Fresh | Step::GiveUp | Step::Rearm => None,
            }
        }
        Step::Announce | Step::Wait(_) | Step::Terminate | Step::Void(_) | Step::Fresh => None,
    }
}

/// Formats the gate wait word for sweep records, qualifying `not-idle` with `status`.
#[must_use]
pub fn wait_word(why: &str, status: &str) -> String {
    if why != "not-idle" && why != "status-stale" {
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

/// Formats a duration in seconds as a compact two-unit span (`45s`, `12m`, `8h22m`).
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

#[cfg(test)]
#[path = "steps_prompt_agreement_tests.rs"]
mod steps_prompt_agreement_tests;

#[path = "upgrade_readiness.rs"]
mod readiness;

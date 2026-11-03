// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure logic for Muse Code (`muse`) crash/exit relaunch and session restore.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::upgrade::ArgvRefusal;

/// Muse Code executable basename.
pub const PROGRAM: &str = "muse";

/// The model a fresh Muse Code launch aterm makes runs, where none is named
/// ([`rewrite_argv`], and the Muse Code row of `super::worker::WORKERS`).
pub const DEFAULT_MODEL: &str = "muse-wonder-full-1.0";

/// The reasoning effort a fresh Muse Code launch aterm makes runs.
pub const DEFAULT_EFFORT: &str = "xhigh";

/// Flag pairs every fresh Muse Code launch aterm makes carries: contributor
/// mode off (`--contributor-mode` is per-run, so nothing else keeps it off)
/// and the default effort.
pub const DEFAULT_FLAGS: &[&str] = &[
    "--contributor-mode",
    "off",
    "--reasoning-effort",
    DEFAULT_EFFORT,
];

/// Environment variable overriding Muse Code's data directory.
pub const DATA_ENV: &str = "XDG_DATA_HOME";

/// Environment variable overriding Muse Code's config directory.
pub const CONFIG_ENV: &str = "XDG_CONFIG_HOME";

/// Home directory environment variable.
pub const HOME_ENV: &str = "HOME";

/// How a root option takes its value (`muse --help`, 1.4.13, clap).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arity {
    /// A switch.
    None,
    /// Exactly one value: the next token whatever it looks like, or attached
    /// (`--model=x`).
    One,
}

/// What a relaunch does with a root option.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fate {
    /// Carried verbatim.
    Keep,
    /// Dropped with its value.
    Drop,
    /// The relaunch is refused ([`ArgvRefusal::NotResumable`]).
    Refuse,
}

/// Muse Code's root options (`muse --help`, 1.4.13-R6996.1), each with its
/// fate on a relaunch. Approval and sandbox policy is process-launch state
/// no session record restores, so those options are carried verbatim: a
/// relaunch that dropped them silently downgraded a `--yolo` run to
/// approval-on (measured 2026-10-31: a restored tab resumed bare).
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--agents", Arity::One, Fate::Keep),
    ("--approval-judge", Arity::One, Fate::Keep),
    ("--approval-mode", Arity::One, Fate::Keep),
    ("--base-url", Arity::One, Fate::Keep),
    ("-c", Arity::One, Fate::Keep),
    ("--config", Arity::One, Fate::Keep),
    ("--contributor-mode", Arity::One, Fate::Drop),
    ("--disable-approval", Arity::None, Fate::Keep),
    ("--disable-request-user-input", Arity::None, Fate::Keep),
    ("--disable-sandbox", Arity::None, Fate::Keep),
    ("--disable-shell", Arity::None, Fate::Keep),
    ("--disable-write", Arity::None, Fate::Keep),
    ("--echo-delay-ms", Arity::One, Fate::Keep),
    ("--effort", Arity::One, Fate::Drop),
    ("--enable-shell-tool", Arity::None, Fate::Keep),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    ("--image", Arity::One, Fate::Drop),
    ("--model", Arity::One, Fate::Drop),
    ("--no-session-log", Arity::None, Fate::Refuse),
    ("--no-parallel-tool-calls", Arity::None, Fate::Keep),
    ("--parallel-tool-calls", Arity::None, Fate::Keep),
    ("--permission-profile", Arity::One, Fate::Keep),
    ("--preset", Arity::One, Fate::Keep),
    ("--provider", Arity::One, Fate::Keep),
    ("--reasoning-effort", Arity::One, Fate::Drop),
    ("--sandbox-network", Arity::One, Fate::Keep),
    ("--subagent-worktree-isolation", Arity::None, Fate::Keep),
    ("--trust-workspace", Arity::None, Fate::Keep),
    ("-V", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    ("-w", Arity::None, Fate::Refuse),
    ("--workspace", Arity::One, Fate::Refuse),
    ("--worktree", Arity::None, Fate::Refuse),
    ("--worktree-base", Arity::One, Fate::Refuse),
    ("--worktree-existing", Arity::One, Fate::Refuse),
    ("--yolo", Arity::None, Fate::Keep),
];

/// `resume`'s own option: the most recent session in the workspace, never
/// typed (the witness names the session).
const RESUME_LAST: &str = "--last";

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Muse Code's subcommands (`muse --help`, 1.4.13, and clap's `help`). Only
/// `resume` runs the interactive TUI a relaunch brings back; `exec` is its
/// one-shot face, and the rest are tools.
const SUBCOMMANDS: &[&str] = &[
    "resume",
    "exec",
    "config",
    "export",
    "trace",
    "model-profile",
    "skills",
    "voice",
    "plugins",
    "sandbox",
    "schema",
    "serve",
    "session-message",
    "mcp",
    "auth",
    "login",
    "logout",
    "usage",
    "init",
    "help",
];

/// Whether clap reads `tok` as an option: a leading `-` and more after it.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// An option token's name and whether its value is attached (`--model=x`,
/// `-cKEY=VALUE`, `-wcreate`): clap attaches the value-taking short options'
/// values, while the switches `-h`/`-V` take none.
fn option_name(tok: &str) -> (&str, bool) {
    if let Some((name, _)) = tok.split_once('=')
        && name.starts_with("--")
    {
        return (name, true);
    }
    if tok.len() > 2
        && let Some(name) = tok.get(..2).filter(|name| matches!(*name, "-c" | "-w"))
    {
        return (name, true);
    }
    (tok, false)
}

/// Whether `tok` is a known value-taking root option without an attached value (`--model`, `-c`).
#[must_use]
pub(super) fn takes_value(tok: &str) -> bool {
    if !maybe_option(tok) || tok == RESUME_LAST {
        return false;
    }
    let (name, attached) = option_name(tok);
    !attached && matches!(flag(name), Some((Arity::One, _)))
}

/// One launch's argv, read: the root options it carries (verbatim, in
/// order) and what it asked of `resume`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Read {
    /// The kept options, each with its value.
    kept: Vec<String>,
    /// The `resume` subcommand was given.
    resume: bool,
    /// Its `<session-ref>`, when one was given.
    reference: Option<String>,
}

/// [`Read`] of `argv` (`argv[0]` first), or why it cannot be carried.
fn read(argv: &[String]) -> Result<Read, ArgvRefusal> {
    let mut out = Read::default();
    let mut positional_seen = false;
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            // Everything after is a positional: the first prompt, dropped.
            break;
        }
        if !maybe_option(tok) {
            if !positional_seen {
                positional_seen = true;
                if tok == "resume" {
                    out.resume = true;
                    continue;
                }
                if SUBCOMMANDS.contains(&tok) {
                    // `exec` and the tools: no TUI to bring back.
                    return Err(ArgvRefusal::NotResumable(tok.to_string()));
                }
                continue;
            }
            if out.resume && out.reference.is_none() {
                out.reference = Some(tok.to_string());
            }
            continue;
        }
        if out.resume && tok == RESUME_LAST {
            continue;
        }
        let (name, attached) = option_name(tok);
        let Some((arity, fate)) = flag(name) else {
            return Err(ArgvRefusal::UnknownFlag(name.to_string()));
        };
        let mut words = vec![tok.to_string()];
        if arity == Arity::One && !attached {
            let Some(value) = argv.get(i) else {
                return Err(ArgvRefusal::UnknownFlag(name.to_string()));
            };
            words.push(value.clone());
            i += 1;
        }
        match fate {
            Fate::Keep => out.kept.extend(words),
            Fate::Drop => {}
            Fate::Refuse => return Err(ArgvRefusal::NotResumable(name.to_string())),
        }
    }
    Ok(out)
}

/// The root options that say HOW A MUSE CODE RUN REACHES ITS PROVIDER: its
/// provider, its preset, its base URL and its per-run settings, each carried
/// by a relaunch ([`FLAGS`]).
const PROVIDER_FLAGS: &[&str] = &["--provider", "--preset", "--base-url", "-c", "--config"];

/// The root options that say what a Muse Code run may do unasked: its worker's are its parent's
/// (owner ruling 2026-11-02).
const PERMISSION_FLAGS: &[&str] = &[
    "--approval-judge",
    "--approval-mode",
    "--disable-approval",
    "--disable-sandbox",
    "--sandbox-network",
    "--yolo",
];

/// Extract the root provider options (`--provider`, `--preset`, `--base-url`, `-c`, `--config`) from `argv`.
///
/// # Errors
/// Returns [`ArgvRefusal::UnknownFlag`] if `argv` contains an unrecognized flag before `--`.
pub fn provider_flags(argv: &[String]) -> Result<Vec<String>, ArgvRefusal> {
    flags_named(argv, PROVIDER_FLAGS)
}

/// The flags of `argv` a worker of this Muse Code starts with: [`provider_flags`] and its
/// permissions ([`PERMISSION_FLAGS`]), in `argv`'s order.
///
/// # Errors
/// Returns [`ArgvRefusal::UnknownFlag`] if `argv` contains an unrecognized flag before `--`.
pub fn worker_flags(argv: &[String]) -> Result<Vec<String>, ArgvRefusal> {
    let both: Vec<&str> = PROVIDER_FLAGS
        .iter()
        .chain(PERMISSION_FLAGS)
        .copied()
        .collect();
    flags_named(argv, &both)
}

fn flags_named(argv: &[String], named: &[&str]) -> Result<Vec<String>, ArgvRefusal> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) || tok == RESUME_LAST {
            continue;
        }
        let (name, attached) = option_name(tok);
        let Some((arity, _)) = flag(name) else {
            return Err(ArgvRefusal::UnknownFlag(name.to_string()));
        };
        let value = if arity == Arity::One && !attached {
            let Some(value) = argv.get(i) else {
                return Err(ArgvRefusal::UnknownFlag(name.to_string()));
            };
            i += 1;
            Some(value)
        } else {
            None
        };
        if named.contains(&name) {
            out.push(tok.to_string());
            out.extend(value.cloned());
        }
    }
    Ok(out)
}

/// Default-route model launches only; explicit profiles, provider settings,
/// resume operations, tools, and unknown native options keep their own login.
pub fn shared_key_defaults(args: &[String]) -> bool {
    let mut i = 0;
    let mut positional_seen = false;
    let mut exec = false;
    while let Some(tok) = args.get(i) {
        i += 1;
        if tok == "--" {
            return true;
        }
        if !maybe_option(tok) {
            if !positional_seen {
                positional_seen = true;
                if tok == "exec" {
                    exec = true;
                    continue;
                }
                if SUBCOMMANDS.contains(&tok.as_str()) {
                    return false;
                }
            }
            continue;
        }
        let (name, attached) = option_name(tok);
        if PROVIDER_FLAGS.contains(&name) || is_one_shot_flag(name) {
            return false;
        }
        let arity = if exec && name == "--prompt-file" {
            Arity::One
        } else if let Some((arity, _)) = flag(name) {
            arity
        } else {
            return false;
        };
        if arity == Arity::One && !attached {
            if args.get(i).is_none() {
                return false;
            }
            i += 1;
        }
    }
    true
}

/// Whether `argv` launches the interactive Muse Code TUI.
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    !one_shot_or_tool(argv)
}

/// `-h`, `--help`, `-V`, `--version`: a run that prints and exits.
fn is_one_shot_flag(name: &str) -> bool {
    matches!(name, "-h" | "--help" | "-V" | "--version")
}

/// A one-shot flag or a subcommand other than `resume` before `--`.
fn one_shot_or_tool(argv: &[String]) -> bool {
    let mut positional_seen = false;
    let mut after_unknown_flag = false;
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            if !positional_seen {
                if tok == "resume" {
                    positional_seen = true;
                } else if SUBCOMMANDS.contains(&tok) {
                    return true;
                } else if !after_unknown_flag {
                    positional_seen = true;
                }
            }
            after_unknown_flag = false;
            continue;
        }
        after_unknown_flag = false;
        if tok == RESUME_LAST {
            continue;
        }
        let (name, attached) = option_name(tok);
        if is_one_shot_flag(name) {
            return true;
        }
        match flag(name) {
            Some((Arity::One, _)) if !attached => {
                if argv
                    .get(i)
                    .is_some_and(|v| v != "--" && !is_one_shot_flag(option_name(v).0))
                {
                    i += 1;
                }
            }
            Some(_) => {}
            None => {
                after_unknown_flag = !attached;
            }
        }
    }
    false
}

/// Return the `<session-ref>` passed to `muse resume`, if any.
#[must_use]
pub fn resumed_ref(argv: &[String]) -> Option<String> {
    read(argv).ok()?.reference
}

/// Whether `argv` resumes a conversation (`muse resume`, a reference or `--last`): its first
/// positional before `--` is `resume`, read past flags a relaunch refuses or [`FLAGS`] does not
/// know (taken as bare), which [`read`] stops at.
#[must_use]
pub fn resumes(argv: &[String]) -> bool {
    if !is_tui_argv(argv) {
        return false;
    }
    let mut toks = argv.iter().skip(1).take_while(|tok| *tok != "--");
    while let Some(tok) = toks.next() {
        if !maybe_option(tok) {
            return tok == "resume";
        }
        let (name, attached) = option_name(tok);
        if !attached && matches!(flag(name), Some((Arity::One, _))) {
            toks.next();
        }
    }
    false
}

/// Rewrite `argv` into relaunch arguments resuming `session` (or starting fresh when `None`).
/// Every rewrite runs with contributor mode off (the launch's own word on it is
/// dropped, [`FLAGS`]); a fresh start also runs [`DEFAULT_MODEL`] at
/// [`DEFAULT_EFFORT`], while a resumed session keeps the model and effort it
/// already runs, so neither is named for it.
///
/// # Errors
/// Returns [`ArgvRefusal`] if `argv` has an unknown/non-resumable flag or subcommand, or `session` is not a UUID.
pub fn rewrite_argv(argv: &[String], session: Option<&str>) -> Result<Vec<String>, ArgvRefusal> {
    let read = read(argv)?;
    let mut out = read.kept;
    out.push("--contributor-mode".to_string());
    out.push("off".to_string());
    if session.is_none() {
        out.push("--model".to_string());
        out.push(DEFAULT_MODEL.to_string());
        out.push("--reasoning-effort".to_string());
        out.push(DEFAULT_EFFORT.to_string());
    }
    if let Some(session) = session {
        if !is_session_uuid(session) {
            return Err(ArgvRefusal::NotResumable(format!("session-ref {session}")));
        }
        out.push("resume".to_string());
        out.push(session.to_string());
    }
    Ok(out)
}

/// Whether `s` is a hyphenated 8-4-4-4-12 hex UUID.
#[must_use]
pub fn is_session_uuid(s: &str) -> bool {
    s.len() == 36 && super::codex_rollout::is_thread_id(s)
}

/// Classification of how a Muse Code process exited ([`exit_kind`], [`log_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// Clean exit or handled `SIGINT`/`SIGTERM`.
    Clean,
    /// Startup refusal (`1` or `2`).
    Refused,
    /// Abnormal termination (`SIGHUP`, `SIGKILL`, or other unhandled exit).
    Crashed,
}

impl ExitKind {
    /// Wire name for steps and the turn ledger.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ExitKind::Clean => "clean",
            ExitKind::Refused => "refused",
            ExitKind::Crashed => "crashed",
        }
    }
}

/// Classify a relaunch command line's exit status, treating `126`/`127` as [`ExitKind::Refused`].
#[must_use]
pub const fn line_exit_kind(status: i64) -> ExitKind {
    match status {
        126 | 127 => ExitKind::Refused,
        status => exit_kind(status),
    }
}

/// Classify a Muse Code process exit status.
#[must_use]
pub const fn exit_kind(status: i64) -> ExitKind {
    match status {
        0 | 130 | 143 => ExitKind::Clean,
        1 | 2 => ExitKind::Refused,
        _ => ExitKind::Crashed,
    }
}

/// One session log line's top-level `payload_type` and the line, parsed —
/// only a line that names one of `kinds` at all, so the records between
/// (most of a long log) cost a substring scan and no parse.
fn record_of(line: &str, kinds: &[&str]) -> Option<(String, Value)> {
    if !kinds.iter().any(|k| line.contains(k)) {
        return None;
    }
    let v: Value = aterm_json::from_str(line.trim()).ok()?;
    let kind = v.get("payload_type")?.as_str()?.to_string();
    Some((kind, v))
}

/// Determine [`ExitKind`] from the session's `session.jsonl` log.
#[must_use]
pub fn log_verdict(log: &str) -> Option<ExitKind> {
    let mut started = false;
    let mut clean = false;
    for (kind, v) in log
        .lines()
        .filter_map(|l| record_of(l, &["session.started", "session.end"]))
    {
        match kind.as_str() {
            "session.started" => {
                started = true;
                clean = false;
            }
            "session.end" if started => {
                clean = v
                    .get("payload")
                    .and_then(|p| p.get("record"))
                    .and_then(|r| r.get("exit_reason"))
                    .and_then(Value::as_str)
                    == Some("clean");
            }
            _ => {}
        }
    }
    started.then_some(if clean {
        ExitKind::Clean
    } else {
        ExitKind::Crashed
    })
}

/// Extract the most recent `workspace_root` from `session.jsonl`.
#[must_use]
pub fn log_workspace(log: &str) -> Option<String> {
    log.lines()
        .rev()
        .filter_map(|l| {
            record_of(
                l,
                &[
                    "runtime.session_start.intake_v3",
                    "runtime.session.metadata",
                ],
            )
        })
        .filter_map(|(kind, v)| {
            let payload = v.get("payload")?;
            let meta = match kind.as_str() {
                "runtime.session_start.intake_v3" => {
                    payload.get("payload")?.get("session_metadata")?
                }
                "runtime.session.metadata" => payload.get("record")?,
                _ => return None,
            };
            meta.get("workspace_root")?.as_str().map(str::to_owned)
        })
        .find(|w| Path::new(w).is_absolute() && !w.chars().any(char::is_control))
}

/// Parse `pid=<pid>` from a `.session.lock` file body.
#[must_use]
pub fn lock_pid(body: &str) -> Option<u32> {
    body.lines()
        .find_map(|l| l.trim().strip_prefix("pid="))
        .filter(|s| !s.starts_with('0'))
        .and_then(crate::parse_uint::<u32>)
        .filter(|&p| p > 1)
}

/// Session lock filename.
pub const LOCK: &str = ".session.lock";

/// Session log filename.
pub const LOG: &str = "session.jsonl";

/// Sessions root directory under `data`.
#[must_use]
pub fn sessions_root(data: &Path) -> PathBuf {
    data.join("muse").join("sessions")
}

/// Extract the session UUID if `lock` is at `<root>/YYYY/MM/DD/<uuid>/.session.lock`.
#[must_use]
pub fn session_of_lock(root: &Path, lock: &Path) -> Option<String> {
    let rest = lock.strip_prefix(root).ok()?;
    let parts: Vec<&str> = rest
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let [year, month, day, session, file] = parts.as_slice() else {
        return None;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    (digits(year, 4)
        && digits(month, 2)
        && digits(day, 2)
        && is_session_uuid(session)
        && *file == LOCK)
        .then(|| (*session).to_string())
}

/// Resolve the Muse Code data directory from `XDG_DATA_HOME`, `HOME`, or `fallback`.
#[must_use]
pub fn data_home(xdg: Option<&str>, home: Option<&str>, fallback: &Path) -> Option<PathBuf> {
    match xdg.filter(|v| !v.is_empty()) {
        Some(set) if Path::new(set).is_absolute() => Some(PathBuf::from(set)),
        Some(_) => None,
        None => Some(
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| fallback.to_path_buf())
                .join(".local")
                .join("share"),
        ),
    }
}

/// Filter `env` to the non-empty `XDG_DATA_HOME` and `XDG_CONFIG_HOME` pairs needed on a relaunch line.
#[must_use]
pub fn line_env(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// Read `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, and `HOME` from `get` for relaunch persistence.
#[must_use]
pub fn relaunch_env<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
    user_home: &Path,
) -> Vec<(String, String)> {
    let home = get(HOME_ENV).filter(|h| Path::new(h).is_absolute() && Path::new(h) != user_home);
    [DATA_ENV, CONFIG_ENV]
        .into_iter()
        .filter_map(|name| get(name).map(|value| (name.to_string(), value.to_string())))
        .chain(home.map(|h| (HOME_ENV.to_string(), h.to_string())))
        .collect()
}

/// Format [`relaunch_env`] pairs as `NAME=VALUE` strings.
#[must_use]
pub fn env_words(env: &[(String, String)]) -> Vec<String> {
    env.iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect()
}

/// Parse `NAME=VALUE` strings back into [`relaunch_env`] pairs.
#[must_use]
pub fn env_of_words(words: &[String]) -> Vec<(String, String)> {
    words
        .iter()
        .filter_map(|w| w.split_once('='))
        .filter(|(name, _)| [DATA_ENV, CONFIG_ENV, HOME_ENV].contains(name))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// Whether `program` lies inside an `atpkg` store (`.../store/muse/<build>/bin/muse`).
#[must_use]
pub fn store_build(program: &Path) -> bool {
    let plain = program.components().all(|c| {
        matches!(
            c,
            std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::Normal(_)
        )
    });
    if !program.has_root() || !plain {
        return false;
    }
    let names: Vec<&str> = program
        .components()
        .rev()
        .take(5)
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    matches!(
        names.as_slice(),
        [PROGRAM, "bin", build, PROGRAM, "store"]
            if !(build.len() > 1 && build.starts_with('0'))
                && crate::parse_uint::<u64>(build).is_some()
    )
}

/// Format the manual `muse --contributor-mode off resume <session>` hint shown to the user:
/// a hand-typed resume runs contributor mode off like every relaunch
/// ([`rewrite_argv`]), and keeps the session's own model and effort.
#[must_use]
pub fn told_line(session: &str, env: &[(String, String)]) -> Option<String> {
    if !is_session_uuid(session) {
        return None;
    }
    let mut said = String::new();
    for (name, value) in env {
        if value
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return None;
        }
        let plain = !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./:@%+,".contains(&b));
        said.push_str(name);
        said.push('=');
        if plain {
            said.push_str(value);
        } else {
            said.push_str(&super::upgrade::quote(super::upgrade::Dialect::Zsh, value));
        }
        said.push(' ');
    }
    said.push_str(&format!(
        "{PROGRAM} --contributor-mode off resume {session}"
    ));
    Some(said)
}

/// Environment variables Muse Code's `tbh_http_client` reads for custom TLS root certificates.
const MUSE_CA_ENV_VARS: [&str; 3] = ["TBH_CA_CERTIFICATE", "SSL_CERT_FILE", "SSL_CERT_DIR"];

/// What a Muse Code launch's `argv` says of the HTTP endpoint it speaks to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchEndpoint {
    /// Explicit base URL (`--base-url` or `-c endpoint_transport.base_url`), with provider `meta`
    /// or `local` and no `-c endpoint_transport` proxy, custom CA, or mTLS auth.
    BaseUrl { url: String, source: &'static str },
    /// Why `argv` names no directly probeable base URL.
    Gap(String),
}

struct ConfigEndpoint {
    provider: Option<String>,
    preset: Result<bool, &'static str>,
    base_url: Option<Result<Option<String>, &'static str>>,
    proxy: Result<bool, &'static str>,
    ca_bundle: Result<bool, &'static str>,
    auth: Result<bool, &'static str>,
}

impl Default for ConfigEndpoint {
    fn default() -> Self {
        Self {
            provider: None,
            preset: Ok(false),
            base_url: None,
            proxy: Ok(false),
            ca_bundle: Ok(false),
            auth: Ok(false),
        }
    }
}

fn muse_env_var<'a>(env: &'a [String], key: &str) -> Option<&'a str> {
    env.iter()
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

fn resolve_muse_whole_var(raw: &str, env: &[String]) -> Result<String, &'static str> {
    if let Some(name) = raw.strip_prefix("${").and_then(|r| r.strip_suffix('}'))
        && !name.is_empty()
        && !name.contains(['$', '{', '}'])
    {
        return muse_env_var(env, name)
            .map(str::to_string)
            .ok_or("Muse Code's -c endpoint_transport references an unset environment variable");
    }
    Ok(raw.to_string())
}

fn parse_muse_transport_flag(
    val: &Value,
    env: &[String],
    bad_msg: &'static str,
) -> Result<bool, &'static str> {
    match val {
        Value::Null => Ok(false),
        Value::String(s) => {
            let resolved = resolve_muse_whole_var(s.trim(), env)?;
            Ok(!resolved.trim().is_empty())
        }
        _ => Err(bad_msg),
    }
}

fn parse_muse_base_url_value(val: &Value, env: &[String]) -> Result<Option<String>, &'static str> {
    match val {
        Value::Null => Ok(None),
        Value::String(s) => {
            let resolved = resolve_muse_whole_var(s.trim(), env)?;
            let trimmed = resolved.trim();
            Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
        }
        _ => Err("Muse Code's -c endpoint_transport.base_url is not a JSON string"),
    }
}

fn parse_muse_auth_flag(val: &Value, env: &[String]) -> Result<bool, &'static str> {
    match val {
        Value::Null => Ok(false),
        Value::String(s) => {
            let resolved = resolve_muse_whole_var(s.trim(), env)?;
            let trimmed = resolved.trim();
            Ok(!trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("bearer"))
        }
        Value::Object(map) => {
            if map.is_empty() {
                return Ok(false);
            }
            let bearer_only = map.len() == 1
                && map
                    .get("type")
                    .or_else(|| map.get("mode"))
                    .and_then(Value::as_str)
                    .is_some_and(|t| {
                        resolve_muse_whole_var(t.trim(), env)
                            .is_ok_and(|r| r.trim().eq_ignore_ascii_case("bearer"))
                    });
            Ok(!bearer_only)
        }
        _ => Err("Muse Code's -c endpoint_transport.auth is not a valid auth setting"),
    }
}

fn apply_muse_config(spec: &str, env: &[String], state: &mut ConfigEndpoint) -> Result<(), String> {
    let Some((raw_key, raw_val)) = spec.split_once('=') else {
        return Err("Muse Code's -c/--config argument has no '='".to_string());
    };
    let key = raw_key.trim();
    let raw_val = raw_val.trim();
    match key {
        "provider" => {
            let parsed = match aterm_json::from_str::<Value>(raw_val) {
                Ok(Value::String(s)) => resolve_muse_whole_var(s.trim(), env)
                    .map_err(str::to_string)?
                    .to_ascii_lowercase(),
                Ok(Value::Null) => String::new(),
                Ok(_) => {
                    return Err("Muse Code's -c provider is not a JSON string".to_string());
                }
                Err(_) => raw_val.trim_matches('"').trim().to_ascii_lowercase(),
            };
            state.provider = Some(parsed);
        }
        "preset" => {
            let parsed = match aterm_json::from_str::<Value>(raw_val) {
                Ok(v) => Ok(v),
                Err(_) if !raw_val.starts_with(['{', '[', '"']) => {
                    Ok(Value::String(raw_val.to_string()))
                }
                Err(_) => Err("Muse Code's -c preset is not valid JSON"),
            };
            state.preset = parsed.and_then(|v| {
                parse_muse_transport_flag(&v, env, "Muse Code's -c preset is not a JSON string")
            });
        }
        "endpoint_transport.proxy" => {
            state.proxy = aterm_json::from_str::<Value>(raw_val)
                .map_err(|_| "Muse Code's -c endpoint_transport.proxy is not valid JSON")
                .and_then(|v| {
                    parse_muse_transport_flag(
                        &v,
                        env,
                        "Muse Code's -c endpoint_transport.proxy is not a JSON string",
                    )
                });
        }
        "endpoint_transport.ca_bundle" => {
            state.ca_bundle = aterm_json::from_str::<Value>(raw_val)
                .map_err(|_| "Muse Code's -c endpoint_transport.ca_bundle is not valid JSON")
                .and_then(|v| {
                    parse_muse_transport_flag(
                        &v,
                        env,
                        "Muse Code's -c endpoint_transport.ca_bundle is not a JSON string",
                    )
                });
        }
        "endpoint_transport.auth" => {
            let parsed = match aterm_json::from_str::<Value>(raw_val) {
                Ok(v) => Ok(v),
                Err(_) if !raw_val.starts_with(['{', '[', '"']) => {
                    Ok(Value::String(raw_val.to_string()))
                }
                Err(_) => Err("Muse Code's -c endpoint_transport.auth is not valid JSON"),
            };
            state.auth = parsed.and_then(|v| parse_muse_auth_flag(&v, env));
        }
        k if k.starts_with("endpoint_transport.auth.") => {
            let parsed = match aterm_json::from_str::<Value>(raw_val) {
                Ok(v) => Ok(v),
                Err(_) if !raw_val.starts_with(['{', '[', '"']) => {
                    Ok(Value::String(raw_val.to_string()))
                }
                Err(_) => Err("Muse Code's -c endpoint_transport.auth is not valid JSON"),
            };
            match parsed {
                Ok(Value::Null) => {}
                Ok(Value::String(s)) if s.trim().is_empty() => {}
                Ok(Value::String(s))
                    if matches!(
                        k,
                        "endpoint_transport.auth.type" | "endpoint_transport.auth.mode"
                    ) && resolve_muse_whole_var(s.trim(), env)
                        .is_ok_and(|r| r.trim().eq_ignore_ascii_case("bearer")) => {}
                Ok(_) => state.auth = Ok(true),
                Err(e) => state.auth = Err(e),
            }
        }
        "endpoint_transport.base_url" => {
            state.base_url = Some(
                aterm_json::from_str::<Value>(raw_val)
                    .map_err(|_| "Muse Code's -c endpoint_transport.base_url is not valid JSON")
                    .and_then(|v| parse_muse_base_url_value(&v, env)),
            );
        }
        "endpoint_transport" => match aterm_json::from_str::<Value>(raw_val) {
            Ok(Value::Null) => {
                state.base_url = Some(Ok(None));
                state.proxy = Ok(false);
                state.ca_bundle = Ok(false);
                state.auth = Ok(false);
            }
            Ok(Value::Object(map)) => {
                if let Some(v) = map.get("proxy") {
                    state.proxy = parse_muse_transport_flag(
                        v,
                        env,
                        "Muse Code's -c endpoint_transport.proxy is not a JSON string",
                    );
                }
                if let Some(v) = map.get("ca_bundle") {
                    state.ca_bundle = parse_muse_transport_flag(
                        v,
                        env,
                        "Muse Code's -c endpoint_transport.ca_bundle is not a JSON string",
                    );
                }
                if let Some(v) = map.get("auth") {
                    state.auth = parse_muse_auth_flag(v, env);
                }
                if let Some(v) = map.get("base_url") {
                    state.base_url = Some(parse_muse_base_url_value(v, env));
                }
            }
            _ => {
                return Err("Muse Code's -c endpoint_transport is not a JSON object".to_string());
            }
        },
        _ => {}
    }
    Ok(())
}

fn muse_option_val<'a>(
    argv: &'a [String],
    tok: &'a str,
    name: &str,
    attached: bool,
    arity: Arity,
    i: &mut usize,
) -> Option<&'a str> {
    if arity != Arity::One {
        return None;
    }
    if attached {
        if name.starts_with("--") {
            tok.split_once('=').map(|(_, v)| v)
        } else {
            let rest = &tok[2..];
            Some(rest.strip_prefix('=').unwrap_or(rest))
        }
    } else {
        let next = argv
            .get(*i)
            .filter(|v| v.as_str() != "--")
            .map(String::as_str);
        if next.is_some() {
            *i += 1;
        }
        next
    }
}

fn muse_base_url_or_ca_gap(url: String, source: &'static str, env: &[String]) -> LaunchEndpoint {
    if url
        .trim()
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
        && let Some(ca_var) = MUSE_CA_ENV_VARS
            .into_iter()
            .find(|k| muse_env_var(env, k).is_some())
    {
        return LaunchEndpoint::Gap(format!(
            "Muse Code's launch sets {ca_var}, so its TLS endpoint cannot be probed with the OS \
             trust store"
        ));
    }
    LaunchEndpoint::BaseUrl { url, source }
}

/// Read the HTTP endpoint a Muse Code launch (`argv` and `env`) names through [`FLAGS`]'s option
/// grammar: `--base-url` (any form before `--`, the last winning and outranking
/// `-c endpoint_transport.base_url`), gated on `--provider` (`meta` or `local`, default `meta`),
/// an unproxied `-c endpoint_transport` with no custom CA or mTLS auth, and no custom TLS CA
/// environment variable on `https://` targets.
/// Named `--preset` / `-c preset` profiles are treated as a gap because their transport settings
/// live outside `argv`.
#[must_use]
pub fn launch_endpoint(argv: &[String], env: &[String]) -> LaunchEndpoint {
    let mut positional_seen = false;
    let mut exec = false;
    let mut base_url_flag: Option<Option<&str>> = None;
    let mut provider_flag: Option<String> = None;
    let mut preset_flag = false;
    let mut cfg = ConfigEndpoint::default();
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            if !positional_seen {
                positional_seen = true;
                if tok == "exec" {
                    exec = true;
                } else if tok != "resume" && SUBCOMMANDS.contains(&tok) {
                    return LaunchEndpoint::Gap(format!(
                        "Muse Code subcommand {tok} runs no model session"
                    ));
                }
            }
            continue;
        }
        if tok == RESUME_LAST {
            continue;
        }
        let (name, attached) = option_name(tok);
        let arity = if exec && name == "--prompt-file" {
            Arity::One
        } else if let Some((arity, _)) = flag(name) {
            arity
        } else {
            return LaunchEndpoint::Gap(
                "Muse Code's launch passes an unrecognized option before --, so its route is not \
                 read"
                    .to_string(),
            );
        };
        let val = muse_option_val(argv, tok, name, attached, arity, &mut i);
        match name {
            "--base-url" => {
                base_url_flag = Some(val.map(str::trim).filter(|s| !s.is_empty()));
            }
            "--provider" => {
                provider_flag =
                    Some(val.map_or_else(String::new, |s| s.trim().to_ascii_lowercase()));
            }
            "--preset" => {
                preset_flag = true;
            }
            "-c" | "--config" => {
                let Some(spec) = val else {
                    return LaunchEndpoint::Gap(
                        "Muse Code's launch passes -c/--config with no value".to_string(),
                    );
                };
                if let Err(why) = apply_muse_config(spec, env, &mut cfg) {
                    return LaunchEndpoint::Gap(why);
                }
            }
            _ => {}
        }
    }
    match cfg.preset {
        Ok(cfg_preset) if preset_flag || cfg_preset => {
            return LaunchEndpoint::Gap(
                "Muse Code's launch selects a preset profile, whose transport settings are not read"
                    .to_string(),
            );
        }
        Ok(_) => {}
        Err(why) => return LaunchEndpoint::Gap(why.to_string()),
    }
    if let Some(p) = provider_flag.as_deref().or(cfg.provider.as_deref())
        && !matches!(p, "meta" | "local")
    {
        return LaunchEndpoint::Gap(
            "Muse Code's launch selects a provider that does not use its HTTP base URL".to_string(),
        );
    }
    for check in [cfg.proxy, cfg.ca_bundle] {
        match check {
            Ok(false) => {}
            Ok(true) => {
                return LaunchEndpoint::Gap(
                    "Muse Code's -c endpoint_transport configures its own proxy or CA bundle, so \
                     its endpoint cannot be probed directly"
                        .to_string(),
                );
            }
            Err(why) => return LaunchEndpoint::Gap(why.to_string()),
        }
    }
    match cfg.auth {
        Ok(false) => {}
        Ok(true) => {
            return LaunchEndpoint::Gap(
                "Muse Code's -c endpoint_transport.auth configures client-certificate or custom \
                 transport auth, so its endpoint cannot be probed directly"
                    .to_string(),
            );
        }
        Err(why) => return LaunchEndpoint::Gap(why.to_string()),
    }
    match base_url_flag {
        Some(Some(url)) => muse_base_url_or_ca_gap(url.to_string(), "Muse Code's --base-url", env),
        Some(None) => {
            LaunchEndpoint::Gap("Muse Code's launch passes --base-url with no value".to_string())
        }
        None => match cfg.base_url {
            Some(Ok(Some(url))) => {
                muse_base_url_or_ca_gap(url, "Muse Code's -c endpoint_transport.base_url", env)
            }
            Some(Err(why)) => LaunchEndpoint::Gap(why.to_string()),
            None | Some(Ok(None)) => LaunchEndpoint::Gap(
                "Muse Code's launch names no --base-url (its default endpoint is not read)"
                    .to_string(),
            ),
        },
    }
}

#[cfg(test)]
#[path = "upgrade_muse_tests.rs"]
mod tests;

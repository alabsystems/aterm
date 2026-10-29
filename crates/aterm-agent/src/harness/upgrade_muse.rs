// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure logic for Muse Code (`muse`) crash/exit relaunch and session restore.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::upgrade::ArgvRefusal;

/// Muse Code executable basename.
pub const PROGRAM: &str = "muse";

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
/// fate on a relaunch.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--agents", Arity::One, Fate::Keep),
    ("--approval-judge", Arity::One, Fate::Drop),
    ("--approval-mode", Arity::One, Fate::Drop),
    ("--base-url", Arity::One, Fate::Keep),
    ("-c", Arity::One, Fate::Keep),
    ("--config", Arity::One, Fate::Keep),
    ("--contributor-mode", Arity::One, Fate::Keep),
    ("--disable-approval", Arity::None, Fate::Drop),
    ("--disable-request-user-input", Arity::None, Fate::Keep),
    ("--disable-sandbox", Arity::None, Fate::Drop),
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
    ("--permission-profile", Arity::One, Fate::Drop),
    ("--preset", Arity::One, Fate::Keep),
    ("--provider", Arity::One, Fate::Keep),
    ("--reasoning-effort", Arity::One, Fate::Drop),
    ("--sandbox-network", Arity::One, Fate::Drop),
    ("--subagent-worktree-isolation", Arity::None, Fate::Keep),
    ("--trust-workspace", Arity::None, Fate::Keep),
    ("-V", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    ("-w", Arity::None, Fate::Refuse),
    ("--workspace", Arity::One, Fate::Refuse),
    ("--worktree", Arity::None, Fate::Refuse),
    ("--worktree-base", Arity::One, Fate::Refuse),
    ("--worktree-existing", Arity::One, Fate::Refuse),
    ("--yolo", Arity::None, Fate::Drop),
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

/// Extract the root provider options (`--provider`, `--preset`, `--base-url`, `-c`, `--config`) from `argv`.
///
/// # Errors
/// Returns [`ArgvRefusal::UnknownFlag`] if `argv` contains an unrecognized flag before `--`.
pub fn provider_flags(argv: &[String]) -> Result<Vec<String>, ArgvRefusal> {
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
        if PROVIDER_FLAGS.contains(&name) {
            out.push(tok.to_string());
            out.extend(value.cloned());
        }
    }
    Ok(out)
}

/// Whether `argv` launches the interactive Muse Code TUI.
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    match read(argv) {
        Ok(_) => true,
        Err(ArgvRefusal::UnknownFlag(_)) => !one_shot_or_tool(argv),
        Err(ArgvRefusal::NotResumable(what)) => {
            !(SUBCOMMANDS.contains(&what.as_str()) || is_one_shot_flag(&what))
        }
    }
}

/// `-h`, `--help`, `-V`, `--version`: a run that prints and exits.
fn is_one_shot_flag(name: &str) -> bool {
    matches!(name, "-h" | "--help" | "-V" | "--version")
}

/// A one-shot flag or a subcommand other than `resume` anywhere before `--`,
/// read without the flag table (an unknown flag may or may not take a value).
fn one_shot_or_tool(argv: &[String]) -> bool {
    argv.iter()
        .skip(1)
        .take_while(|t| t.as_str() != "--")
        .any(|t| {
            is_one_shot_flag(option_name(t).0)
                || (SUBCOMMANDS.contains(&t.as_str()) && t != "resume")
        })
}

/// Return the `<session-ref>` passed to `muse resume`, if any.
#[must_use]
pub fn resumed_ref(argv: &[String]) -> Option<String> {
    read(argv).ok()?.reference
}

/// Rewrite `argv` into relaunch arguments resuming `session` (or starting fresh when `None`).
///
/// # Errors
/// Returns [`ArgvRefusal`] if `argv` has an unknown/non-resumable flag or subcommand, or `session` is not a UUID.
pub fn rewrite_argv(argv: &[String], session: Option<&str>) -> Result<Vec<String>, ArgvRefusal> {
    let read = read(argv)?;
    let mut out = read.kept;
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
        .and_then(|p| p.parse().ok())
        .filter(|&p| p > 0)
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
    let names: Vec<&str> = program
        .components()
        .rev()
        .take(5)
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    matches!(names.as_slice(), [PROGRAM, "bin", build, PROGRAM, "store"] if !build.is_empty())
}

/// Format the manual `muse resume <session>` hint shown to the user.
#[must_use]
pub fn told_line(session: &str, env: &[(String, String)]) -> Option<String> {
    if !is_session_uuid(session) {
        return None;
    }
    let mut said = String::new();
    for (name, value) in env {
        if value.chars().any(char::is_control) {
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
    said.push_str(&format!("{PROGRAM} resume {session}"));
    Some(said)
}

#[cfg(test)]
#[path = "upgrade_muse_tests.rs"]
mod tests;

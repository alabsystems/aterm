// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grok Build as a worker CLI (`spawn agent=grok`): its program, homes and the flags a starter's
//! argv lends a worker ([`super::WORKERS`]).
//!
//! The option table is Grok Build 1.0.70's own `grok --help` (measured 2026-10-24 on xAI's
//! darwin-arm64 build with a scratch `HOME`; grok-alab's `src/lanes.rs` holds the same table for
//! its relaunches), plus the hidden `--dangerously-skip-permissions` atpkg's vendor row knows and
//! the hidden `--yolo` its own guide names as `--always-approve`'s alias (1.0.70 takes both,
//! measured 2026-11-05). aterm has no Grok relaunch lane (`lanes.rs`: `Program::Grok =>
//! Lanes::NONE`): the table here reads a starter's argv and nothing more.

use std::path::Path;

/// The PATH word and atpkg program of Grok Build.
pub const PROGRAM: &str = "grok";

/// The variable that moves Grok Build's home (sessions, config, `auth.json`; default `~/.grok`).
pub const HOME_ENV: &str = "GROK_HOME";

/// Grok Build's home under the user's home where [`HOME_ENV`] is unset.
pub const DEFAULT_HOME: &str = ".grok";

/// The variable naming the `auth.json` a pooled login reads in place of the home's
/// (`ai::connector::GROK_ACCOUNTS`' seam).
pub const AUTH_PATH_ENV: &str = "GROK_AUTH_PATH";

/// The file under the home Grok Build keeps its sign-in in (`ai::connector::GROK`'s `stores`).
pub const AUTH_FILE: &str = "auth.json";

/// The variable that turns Grok Build's own updater off (atpkg's vendor row and its twin set it):
/// a worker never updates the CLI under its own task.
pub const NO_SELF_UPDATE: (&str, &str) = ("GROK_DISABLE_AUTOUPDATER", "1");

/// How a root option takes its value (clap, as `grok --help` prints it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arity {
    /// A switch.
    None,
    /// One value: the next token whatever it looks like, or attached (`--model=x`).
    One,
    /// An optional value (`[<V>]`): attached, or the next token when it is no option.
    Optional,
}

/// What a worker makes of a starter's root option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// Rides onto the worker's line: it says how Grok Build reaches its provider (its leader).
    Ride,
    /// Says what the run may do unasked (its permission mode and sandbox): rides held to
    /// [`WORKER_FLOOR`] ([`worker_flags`]).
    Posture,
    /// Stays the starter's: its model, permission rules, folders, session and setup.
    Stay,
    /// The starter answers and exits (help, version, a one-shot prompt): it lends nothing.
    OneShot,
}

/// Grok Build 1.0.70's root options, each with its arity and what a worker makes of it.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--add-dir", Arity::One, Fate::Stay),
    ("--agent", Arity::One, Fate::Stay),
    ("--agents", Arity::One, Fate::Stay),
    ("--allow", Arity::One, Fate::Stay),
    ("--allowedTools", Arity::One, Fate::Stay),
    ("--allowed-tools", Arity::One, Fate::Stay),
    ("--always-approve", Arity::None, Fate::Posture),
    ("-c", Arity::None, Fate::Stay),
    ("--continue", Arity::None, Fate::Stay),
    ("--cwd", Arity::One, Fate::Stay),
    ("--dangerously-skip-permissions", Arity::None, Fate::Posture),
    ("--debug", Arity::None, Fate::Stay),
    ("--debug-file", Arity::One, Fate::Stay),
    ("--deny", Arity::One, Fate::Stay),
    ("--disallowedTools", Arity::One, Fate::Stay),
    ("--disable-web-search", Arity::None, Fate::Stay),
    ("--disallowed-tools", Arity::One, Fate::Stay),
    ("--fork-session", Arity::None, Fate::Stay),
    ("--fullscreen", Arity::None, Fate::Stay),
    ("-h", Arity::None, Fate::OneShot),
    ("--help", Arity::None, Fate::OneShot),
    ("--include-partial-messages", Arity::None, Fate::Stay),
    ("--json-schema", Arity::One, Fate::Stay),
    ("--leader-socket", Arity::One, Fate::Ride),
    ("-m", Arity::One, Fate::Stay),
    ("--model", Arity::One, Fate::Stay),
    ("--max-turns", Arity::One, Fate::Stay),
    ("--minimal", Arity::None, Fate::Stay),
    ("--no-alt-screen", Arity::None, Fate::Stay),
    ("--no-plan", Arity::None, Fate::Stay),
    ("--no-subagents", Arity::None, Fate::Stay),
    ("--oauth", Arity::None, Fate::Stay),
    ("--output-format", Arity::One, Fate::Stay),
    ("-p", Arity::One, Fate::OneShot),
    ("--single", Arity::One, Fate::OneShot),
    ("--permission-mode", Arity::One, Fate::Posture),
    ("--prompt-file", Arity::One, Fate::OneShot),
    ("--prompt-json", Arity::One, Fate::OneShot),
    ("-r", Arity::Optional, Fate::Stay),
    ("--resume", Arity::Optional, Fate::Stay),
    ("--reasoning-effort", Arity::One, Fate::Stay),
    ("--effort", Arity::One, Fate::Stay),
    ("--restore-code", Arity::None, Fate::Stay),
    ("--rules", Arity::One, Fate::Stay),
    ("-s", Arity::One, Fate::Stay),
    ("--session-id", Arity::One, Fate::Stay),
    ("--sandbox", Arity::One, Fate::Posture),
    ("--system-prompt-override", Arity::One, Fate::Stay),
    ("--system-prompt", Arity::One, Fate::Stay),
    ("--tools", Arity::One, Fate::Stay),
    ("-v", Arity::None, Fate::OneShot),
    ("--version", Arity::None, Fate::OneShot),
    ("--verbatim", Arity::None, Fate::Stay),
    ("-w", Arity::Optional, Fate::Stay),
    ("--worktree", Arity::Optional, Fate::Stay),
    ("--worktree-ref", Arity::One, Fate::Stay),
    ("--ref", Arity::One, Fate::Stay),
    ("--yolo", Arity::None, Fate::Posture),
];

/// Grok Build 1.0.70's subcommands and their aliases, and the 1.0.67 words atpkg's vendor row
/// names. A starter running one runs no interactive session, so it lends no flags.
const COMMANDS: &[&str] = &[
    "agent",
    "clone",
    "completions",
    "cursor-worker",
    "dashboard",
    "disk-usage",
    "doctor",
    "du",
    "export",
    "help",
    "host-server",
    "inspect",
    "leader",
    "login",
    "logout",
    "mcp",
    "memory",
    "models",
    "plugin",
    "sessions",
    "setup",
    "share",
    "ssh",
    "trace",
    "update",
    "usage",
    "v",
    "version",
    "worker",
    "workspace",
    "worktree",
    "wrap",
];

/// GROK BUILD'S PERMISSION FLOOR (its worker row's, `worker::WorkerCli::unnamed_posture`):
/// `--permission-mode auto`, which 1.0.70's own guide calls Auto-review, "a classifier approve[s]
/// safe tool calls", a call it will not pass being blocked or escalated: Grok Build's counterpart
/// of Claude Code's auto mode, its modes spelled as Claude Code's. Where its auto-mode gate is
/// off (`GROK_AUTO_PERMISSION_MODE=0`, `[auto_mode] enabled = false`, an administrator's policy)
/// Auto-review is not offered, and a worker of an `acceptEdits` starter then asks more than its
/// starter did; Grok workers are refused as unsupervisable ([`super::WorkerCli::end_unseen`]), so
/// no worker runs at this floor until a reader of Grok's turns lifts that, which then decides
/// whether the gate is read first. Measured 2026-11-05 on 1.0.70: it takes `auto` and refuses
/// `--permission-mode` given twice, so [`worker_flags`] drops a starter's first.
pub const WORKER_FLOOR: [&str; 2] = ["--permission-mode", "auto"];

/// The permission modes at or above [`WORKER_FLOOR`], which a starter lends as they are: none
/// stops for a person's box on each edit or command (`dontAsk` refuses what it would ask).
const FLOOR_KEEPS: [&str; 3] = [WORKER_FLOOR[1], "dontAsk", "bypassPermissions"];

/// The flags of a Grok Build starter's `argv` (its `argv[0]` first) that ride onto its worker's
/// line: `--leader-socket`, which names the leader (and so the login) its sessions reach xAI
/// through, then its posture held to [`WORKER_FLOOR`] (owner, 2026-11-04: workers are fully
/// automated by default). A starter that always approves (`--always-approve`, its aliases
/// `--yolo` and `--dangerously-skip-permissions`) or runs in a mode of [`FLOOR_KEEPS`] lends its
/// posture as it is, with its `--sandbox`; any other mode, or none, lends the floor in place of
/// its `--permission-mode`, beside its `--sandbox`. Its model, permission rules, folders, session
/// and prompt stay its own. A starter that runs a subcommand or a one-shot prompt lends only the
/// floor.
///
/// # Errors
///
/// An option 1.0.70's `--help` does not list, as typed: no table here knows what it moves.
pub fn worker_flags(argv: &[String]) -> Result<Vec<String>, String> {
    let (mut out, posture) = lent(argv)?;
    let always = posture.iter().any(|(name, _)| {
        matches!(
            *name,
            "--always-approve" | "--yolo" | "--dangerously-skip-permissions"
        )
    });
    let mode = posture
        .iter()
        .rfind(|(name, _)| *name == WORKER_FLOOR[0])
        .and_then(|(name, words)| match words.as_slice() {
            [_, value] => Some(value.as_str()),
            [attached] => attached.strip_prefix(*name)?.strip_prefix('='),
            _ => None,
        });
    if always || mode.is_some_and(|mode| FLOOR_KEEPS.contains(&mode)) {
        out.extend(posture.into_iter().flat_map(|(_, words)| words));
        return Ok(out);
    }
    out.extend(
        posture
            .into_iter()
            .filter(|(name, _)| *name != WORKER_FLOOR[0])
            .flat_map(|(_, words)| words),
    );
    out.extend(WORKER_FLOOR.map(str::to_string));
    Ok(out)
}

/// What a Grok Build starter's `argv` lends ([`worker_flags`]): the flags that ride, and its
/// posture flags, each by its name with its words. A starter that runs a subcommand or a
/// one-shot prompt lends neither.
type Lent = (Vec<String>, Vec<(&'static str, Vec<String>)>);

/// [`worker_flags`]' reading of `argv`.
fn lent(argv: &[String]) -> Result<Lent, String> {
    let mut out = Vec::new();
    let mut posture = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        if tok.len() < 2 || !tok.starts_with('-') {
            if COMMANDS.contains(&tok) {
                return Ok((Vec::new(), Vec::new()));
            }
            continue;
        }
        let (name, attached) = match tok.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (tok, None),
        };
        let Some((name, arity, fate)) = FLAGS.iter().find(|(n, _, _)| *n == name).copied() else {
            return Err(name.to_string());
        };
        let value = match (arity, attached) {
            (Arity::None, Some(_)) => return Err(tok.to_string()),
            (Arity::None, None) | (Arity::One | Arity::Optional, Some(_)) => None,
            (Arity::One, None) => {
                let Some(value) = argv.get(i).filter(|v| v.as_str() != "--").cloned() else {
                    return Err(tok.to_string());
                };
                i += 1;
                Some(value)
            }
            (Arity::Optional, None) => match argv.get(i) {
                Some(v) if !v.starts_with('-') => {
                    i += 1;
                    Some(v.clone())
                }
                _ => None,
            },
        };
        match fate {
            Fate::OneShot => return Ok((Vec::new(), Vec::new())),
            Fate::Stay => {}
            Fate::Ride => {
                out.push(tok.to_string());
                out.extend(value);
            }
            Fate::Posture => {
                posture.push((
                    name,
                    std::iter::once(tok.to_string()).chain(value).collect(),
                ));
            }
        }
    }
    Ok((out, posture))
}

/// The environment a Grok Build starter's worker carries: its own `GROK_HOME` and pooled
/// `GROK_AUTH_PATH` where set (a relative `GROK_HOME` is the line's refusal, a relative
/// `GROK_AUTH_PATH` the sign-in gate's, [`signed_out`]); else, for a
/// starter run under another `HOME` than `user`'s, that home's `.grok`, so the worker reads the
/// starter's sessions, config and login. No credential's value is carried (`GROK_AUTH` stays the
/// starter's: a typed line is no place for one).
#[must_use]
pub fn relaunch_env<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
    user: &Path,
) -> Vec<(String, String)> {
    let mut env = Vec::new();
    match get(HOME_ENV).filter(|home| !home.is_empty()) {
        Some(home) => env.push((HOME_ENV.to_string(), home.to_string())),
        None => {
            if let Some(grok) = get("HOME")
                .map(Path::new)
                .filter(|home| home.has_root() && *home != user)
                .and_then(|home| home.join(DEFAULT_HOME).to_str().map(str::to_string))
            {
                env.push((HOME_ENV.to_string(), grok));
            }
        }
    }
    if let Some(auth) = get(AUTH_PATH_ENV).filter(|auth| !auth.is_empty()) {
        env.push((AUTH_PATH_ENV.to_string(), auth.to_string()));
    }
    env
}

/// Whether `program` is a build in atpkg's store (`…/store/grok/<build>/bin/grok`).
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

/// The sign-in a worker carrying `env` would start Grok Build on, under the user's home `user`:
/// `GROK_AUTH_PATH`'s file where the line sets it, else `<GROK_HOME or ~/.grok>/auth.json`.
/// `None` where neither is set and no user home is known, or a path is relative (the line
/// refuses a relative home before it is typed).
#[must_use]
pub fn auth_file(env: &[(String, String)], user: Option<&Path>) -> Option<std::path::PathBuf> {
    let var = |name: &str| {
        env.iter()
            .rfind(|(n, _)| n == name)
            .filter(|(_, v)| !v.is_empty())
            .map(|(_, v)| Path::new(v))
    };
    let file = match var(AUTH_PATH_ENV) {
        Some(auth) => auth.to_path_buf(),
        None => var(HOME_ENV)
            .map(Path::to_path_buf)
            .or_else(|| user.map(|home| home.join(DEFAULT_HOME)))?
            .join(AUTH_FILE),
    };
    file.has_root().then_some(file)
}

/// Why a Grok Build worker carrying `env` must not start, under the user's home `user`: its
/// sign-in file ([`auth_file`]) is missing, empty, refused (aterm saw grok refuse it:
/// [`crate::harness::grok_signin::login_accepted`]) or cannot be named. Signed out, Grok Build
/// draws xAI's device sign-in instead of a composer and opens that page in the default browser
/// by itself (through the OS, whatever `PATH` says), one page per launch, and its task could
/// never run. `None` where the file is there, not empty and not refused.
///
/// This is a floor: a login that has expired but that aterm has not yet seen grok refuse still
/// passes it, and the first worker on it draws Grok's own sign-in once (the sign-in gate then
/// marks it refused). Grok workers stay refused until aterm reads their turn end
/// (`docs/DESIGN-harness-repos-2026-10-24.md` D40).
#[must_use]
pub fn signed_out(env: &[(String, String)], user: Option<&Path>) -> Option<String> {
    let Some(auth) = auth_file(env, user) else {
        return Some(format!(
            "Grok Build's sign-in file cannot be named for this worker (no home is known), so \
             no worker was started. {SIGN_IN_IS_A_PERSONS}"
        ));
    };
    match std::fs::symlink_metadata(&auth) {
        Ok(meta) if meta.is_file() && meta.len() > 0 => {
            (!crate::harness::grok_signin::login_accepted(&auth)).then(|| {
                format!(
                    "Grok Build is signed out for this worker (grok refused the sign-in at {}, \
                     and no newer one replaced it), so no worker was started and nothing was \
                     installed. {SIGN_IN_IS_A_PERSONS}",
                    auth.display()
                )
            })
        }
        _ => Some(format!(
            "Grok Build is signed out for this worker (no sign-in at {}), so no worker was \
             started and nothing was installed. {SIGN_IN_IS_A_PERSONS}",
            auth.display()
        )),
    }
}

/// What a signed-out refusal tells whoever reads it, a lead agent included: signing in is a
/// person's act, and an agent that runs `grok` to try it opens a browser page at the desk.
const SIGN_IN_IS_A_PERSONS: &str = "A person must sign in to Grok Build; an agent never runs \
     grok to try it, because signed out it opens xAI's sign-in page in the person's browser by \
     itself and never reads its task";

#[cfg(test)]
#[path = "worker_grok_tests.rs"]
mod tests;

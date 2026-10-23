// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE AGY LANE, ITS PURE HALF.

use std::path::{Component, Path, PathBuf};

use super::upgrade::ArgvRefusal;

/// The program's name: its image's basename, `aterm_phase::program_of`'s, and the managed twin's (`agents/agy`).
pub const PROGRAM: &str = "agy";

/// The one variable that moves agy's config (its conversations, their locks and its crash files).
pub const HOME_ENV: &str = "HOME";

/// The default Gemini directory under the home (`.gemini`), which `--gemini_dir=<abs>` replaces.
pub const GEMINI_DIR: &str = ".gemini";

/// The default app data directory under it (`antigravity-cli`), which `--app_data_dir=<rel>` replaces.
pub const APP_DATA_DIR: &str = "antigravity-cli";

/// The directory of the conversations' presence locks under the config.
pub const PRESENCE: &str = "presence";

/// The directory of the running processes' crash files under the config.
pub const CRASHES: &str = "crashes";

/// The directory of the conversations' databases under the config.
pub const CONVERSATIONS: &str = "conversations";

/// The hidden flag that moves the Gemini directory (absolute; a relative one
/// is ignored by agy, which logs `.gemini must be an absolute path`).
const FLAG_GEMINI_DIR: &str = "gemini_dir";

/// The hidden flag that moves the app data directory under it (relative).
const FLAG_APP_DATA_DIR: &str = "app_data_dir";

/// The flag that resumes a conversation by id.
const FLAG_CONVERSATION: &str = "conversation";

/// How a flag takes its value (Go's `flag` package, which agy's parser
/// follows: measured, a switch alone or `=true`/`=false`, a value attached or
/// the next token whatever it looks like).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arity {
    /// A switch: never takes the next token.
    None,
    /// Exactly one value: attached (`--effort=high`) or the next token.
    One,
}

/// What a relaunch does with a flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fate {
    /// Carried verbatim, as typed.
    Keep,
    /// Dropped with its value.
    Drop,
    /// The relaunch is refused ([`ArgvRefusal::NotResumable`]); the launch is
    /// still a TUI.
    Refuse,
    /// The launch is no TUI at all: it prints, or runs one prompt, and exits
    /// ([`is_tui_argv`]).
    OneShot,
}

/// agy's flags (`agy --help`, 1.4.0, and the two hidden ones the premeasure
/// found), named without their dashes as Go reads them, each with its fate on
/// a relaunch.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("add-dir", Arity::One, Fate::Drop),
    ("agent", Arity::One, Fate::Drop),
    (FLAG_APP_DATA_DIR, Arity::One, Fate::Keep),
    ("c", Arity::None, Fate::Drop),
    ("continue", Arity::None, Fate::Drop),
    (FLAG_CONVERSATION, Arity::One, Fate::Drop),
    ("dangerously-skip-permissions", Arity::None, Fate::Keep),
    ("disable-slash-commands", Arity::None, Fate::Refuse),
    ("effort", Arity::One, Fate::Keep),
    (FLAG_GEMINI_DIR, Arity::One, Fate::Keep),
    ("h", Arity::None, Fate::OneShot),
    ("help", Arity::None, Fate::OneShot),
    ("i", Arity::One, Fate::Drop),
    ("prompt-interactive", Arity::One, Fate::Drop),
    ("input-format", Arity::One, Fate::Refuse),
    ("json-schema", Arity::One, Fate::Refuse),
    ("log-file", Arity::One, Fate::Keep),
    ("mode", Arity::One, Fate::Keep),
    ("model", Arity::One, Fate::Keep),
    ("new-project", Arity::None, Fate::Drop),
    ("output-format", Arity::One, Fate::OneShot),
    ("p", Arity::One, Fate::OneShot),
    ("print", Arity::One, Fate::OneShot),
    ("prompt", Arity::One, Fate::OneShot),
    ("print-timeout", Arity::One, Fate::Refuse),
    ("project", Arity::One, Fate::Drop),
    ("remote-control", Arity::None, Fate::Refuse),
    ("sandbox", Arity::None, Fate::Keep),
    ("version", Arity::None, Fate::OneShot),
];

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// agy's subcommands (`agy --help`, 1.4.0): each a tool, never the TUI.
const SUBCOMMANDS: &[&str] = &[
    "agent",
    "agents",
    "changelog",
    "help",
    "install",
    "mcp",
    "mic-serve",
    "models",
    "plugin",
    "plugins",
    "remote-control",
    "update",
];

/// A flag token as Go reads it: its name (one or two dashes off) and the
/// value attached after `=`, or `None` for a token that is no flag (a
/// positional, `-` alone, `--`, or three dashes).
fn flag_token(tok: &str) -> Option<(&str, Option<&str>)> {
    let bare = tok.strip_prefix("--").or_else(|| tok.strip_prefix('-'))?;
    if bare.is_empty() || bare.starts_with('-') || bare.starts_with('=') {
        return None;
    }
    Some(match bare.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (bare, None),
    })
}

/// One launch's argv, read: the flags it carries, verbatim and in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Read {
    kept: Vec<String>,
}

/// Why a [`read`] stopped, with its word: a flag the table does not know, a
/// flag that refuses a relaunch of a TUI, or what makes the launch no TUI at
/// all (a one-shot flag, a subcommand, a positional).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stop {
    Unknown(String),
    Refuse(String),
    Ends(String),
}

impl From<Stop> for ArgvRefusal {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Unknown(name) => ArgvRefusal::UnknownFlag(name),
            Stop::Refuse(what) | Stop::Ends(what) => ArgvRefusal::NotResumable(what),
        }
    }
}

/// [`Read`] of `argv` (`argv[0]` first), or why it cannot be carried
/// ([`Stop`]).
fn read(argv: &[String]) -> Result<Read, Stop> {
    let mut out = Read::default();
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            return match argv.get(i) {
                Some(_) => Err(Stop::Ends("positional".to_string())),
                None => Ok(out),
            };
        }
        let Some((name, attached)) = flag_token(tok) else {
            let what = if SUBCOMMANDS.contains(&tok) {
                tok
            } else {
                "positional"
            };
            return Err(Stop::Ends(what.to_string()));
        };
        let Some((arity, fate)) = flag(name) else {
            return Err(Stop::Unknown(name.to_string()));
        };
        let mut words = vec![tok.to_string()];
        if arity == Arity::One && attached.is_none() {
            let Some(value) = argv.get(i) else {
                return Err(Stop::Unknown(name.to_string()));
            };
            words.push(value.clone());
            i += 1;
        }
        match fate {
            Fate::Keep => out.kept.extend(words),
            Fate::Drop => {}
            Fate::Refuse => return Err(Stop::Refuse(name.to_string())),
            Fate::OneShot => return Err(Stop::Ends(name.to_string())),
        }
    }
    Ok(out)
}

/// Whether `argv` is an interactive agy TUI's.
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    match read(argv) {
        Ok(_) | Err(Stop::Refuse(_)) => true,
        Err(Stop::Ends(_)) => false,
        Err(Stop::Unknown(_)) => !one_shot_anywhere(argv),
    }
}

/// A one-shot flag or a subcommand anywhere before `--`, read without the
/// flag table (an unknown flag may or may not take a value).
fn one_shot_anywhere(argv: &[String]) -> bool {
    argv.iter()
        .skip(1)
        .take_while(|t| t.as_str() != "--")
        .any(|t| match flag_token(t) {
            Some((name, _)) => flag(name).is_some_and(|(_, fate)| fate == Fate::OneShot),
            None => SUBCOMMANDS.contains(&t.as_str()),
        })
}

/// The last value of the flag `want` in `argv`, any spelling (`-x=v`,
/// `--x=v`, `-x v`, `--x v`), read as Go reads it — before `--`, the last
/// occurrence winning.
fn flag_value<'a>(argv: &'a [String], want: &str) -> Option<&'a str> {
    let mut found = None;
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        let Some((name, attached)) = flag_token(tok) else {
            continue;
        };
        if name != want {
            continue;
        }
        found = match attached {
            Some(value) => Some(value),
            None => {
                let value = argv.get(i).map(String::as_str);
                i += 1;
                value
            }
        };
    }
    found
}

/// THE CONVERSATION AN ARGV RESUMES (`--conversation`, any spelling), as given.
#[must_use]
pub fn resumed_conversation(argv: &[String]) -> Option<String> {
    flag_value(argv, FLAG_CONVERSATION)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// The flags of `argv` that MOVE agy's config, verbatim and in order (`--gemini_dir`, `--app_data_dir`, each with its value).
#[must_use]
pub fn config_flags(argv: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        let Some((name, attached)) = flag_token(tok) else {
            continue;
        };
        if name != FLAG_GEMINI_DIR && name != FLAG_APP_DATA_DIR {
            continue;
        }
        out.push(tok.to_string());
        if attached.is_none()
            && let Some(value) = argv.get(i)
        {
            out.push(value.clone());
            i += 1;
        }
    }
    out
}

/// THE WORDS A RELAUNCH TYPES AFTER THE PROGRAM: the launch's carried flags verbatim ([`FLAGS`]), then `--conversation=<uuid>`.
///
/// # Errors
/// [`ArgvRefusal::UnknownFlag`], [`ArgvRefusal::NotResumable`], or a non-UUIDv4 conversation id.
pub fn rewrite_argv(
    argv: &[String],
    conversation: Option<&str>,
) -> Result<Vec<String>, ArgvRefusal> {
    let mut out = read(argv).map_err(ArgvRefusal::from)?.kept;
    if let Some(conversation) = conversation {
        if !is_conversation_uuid(conversation) {
            return Err(ArgvRefusal::NotResumable(format!(
                "conversation {conversation}"
            )));
        }
        out.push(format!("--{FLAG_CONVERSATION}={conversation}"));
    }
    Ok(out)
}

/// Whether `s` is a conversation id agy prints and takes back.
#[must_use]
pub fn is_conversation_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 36
        && super::codex_rollout::is_thread_id(s)
        && !s.bytes().any(|c| c.is_ascii_uppercase())
        && b[14] == b'4'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
}

/// THE CONFIG DIRECTORY A PROCESS WITH THIS ARGV AND HOME USES.
#[must_use]
pub fn config_dir(argv: &[String], home: Option<&str>, fallback: &Path) -> Option<PathBuf> {
    let gemini = match flag_value(argv, FLAG_GEMINI_DIR).filter(|d| Path::new(d).has_root()) {
        Some(abs) => PathBuf::from(abs),
        None => home
            .map(PathBuf::from)
            .filter(|h| h.has_root())
            .unwrap_or_else(|| fallback.to_path_buf())
            .join(GEMINI_DIR),
    };
    let app = match flag_value(argv, FLAG_APP_DATA_DIR) {
        None => APP_DATA_DIR,
        Some(rel)
            if !rel.is_empty()
                && Path::new(rel)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))) =>
        {
            rel
        }
        Some(_) => return None,
    };
    Some(gemini.join(app))
}

/// The presence lock of `conversation` under the config `config`.
#[must_use]
pub fn presence_lock(config: &Path, conversation: &str) -> PathBuf {
    config.join(PRESENCE).join(format!("{conversation}.lock"))
}

/// The conversation `lock` is the presence lock of, under `config`: `<config>/presence/<uuid>.lock`, the uuid a UUIDv4.
#[must_use]
pub fn lock_conversation(config: &Path, lock: &Path) -> Option<String> {
    let rest = lock.strip_prefix(config).ok()?;
    let parts: Vec<&str> = rest
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let [dir, file] = parts.as_slice() else {
        return None;
    };
    let conversation = file.strip_suffix(".lock")?;
    (*dir == PRESENCE && is_conversation_uuid(conversation)).then(|| conversation.to_string())
}

/// THE DATABASE OF `conversation` under the config `config` (`<config>/conversations/<uuid>.db`).
#[must_use]
pub fn conversation_db(config: &Path, conversation: &str) -> PathBuf {
    config
        .join(CONVERSATIONS)
        .join(format!("{conversation}.db"))
}

/// Whether the files an agy holds open (`files`, `lsof`'s canonical paths) hold no presence lock under its config `config` ([`lock_conversation`]).
#[must_use]
pub fn holds_no_lock(config: &Path, files: &[PathBuf]) -> bool {
    !files
        .iter()
        .any(|file| lock_conversation(config, file).is_some())
}

/// The pid a crash file's name names (`crash_<pid>_<uuid>.log`, its uuid any UUID: a new one each launch, never the conversation's).
#[must_use]
pub fn crash_file_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("crash_")?.strip_suffix(".log")?;
    let (pid, uuid) = rest.split_once('_')?;
    (super::codex_rollout::is_thread_id(uuid) && pid.bytes().all(|b| b.is_ascii_digit()))
        .then(|| pid.parse().ok())
        .flatten()
        .filter(|&p| p > 0)
}

/// Whether `file` is a crash file of `pid` under `config` (`<config>/crashes/crash_<pid>_<uuid>.log`).
#[must_use]
pub fn crash_file(config: &Path, file: &Path, pid: u32) -> bool {
    file.parent() == Some(config.join(CRASHES).as_path())
        && file
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(crash_file_pid)
            == Some(pid)
}

/// How an agy process ended, by its shell's exit status and its crash file ([`exit_kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// `0`: its own exit or someone's SIGTERM, SIGHUP or SIGINT (each graceful, mid-turn too: the crash file removed): never relaunched.
    Clean,
    /// `146`: Ctrl-Z — or any other stop signal's status ([`stopped_status`]). agy is STOPPED, alive, its lock and crash file held.
    Stopped,
    /// `2` with no crash file left: a start agy refused (bad arguments, a positional), never retried.
    Refused,
    /// Any other status with the dead pid's crash file still there (SIGKILL, measured; a Go panic only if it leaves the file, unmeasured).
    Crashed,
    /// Any other status and no crash file: an end no witness tells (the file swept by another agy's start, or never written).
    Unwitnessed,
}

impl ExitKind {
    /// One word for the step and the ledger.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ExitKind::Clean => "clean",
            ExitKind::Stopped => "stopped",
            ExitKind::Refused => "refused",
            ExitKind::Crashed => "crashed",
            ExitKind::Unwitnessed => "unwitnessed",
        }
    }
}

/// The signals that stop a job (`SIGTSTP`, Ctrl-Z's; `SIGSTOP`; `SIGTTIN`;
/// `SIGTTOU`): a stopped job's block ends with `128` plus one of them, the
/// process alive.
#[cfg(unix)]
const STOP_SIGNALS: [libc::c_int; 4] = [libc::SIGTSTP, libc::SIGSTOP, libc::SIGTTIN, libc::SIGTTOU];
#[cfg(not(unix))]
const STOP_SIGNALS: [i32; 0] = [];

/// Whether `status` is the shell's word for a job a signal STOPPED (`128 +` a [`STOP_SIGNALS`] member).
#[must_use]
pub fn stopped_status(status: i64) -> bool {
    STOP_SIGNALS
        .iter()
        .any(|&signal| status == 128 + i64::from(signal))
}

/// AGY'S EXIT TABLE, measured on 1.4.0 (the block's exit, which equals the typed `$?` in every case, fresh and resumed, with and without the lab launcher's `clear`).
#[must_use]
pub fn exit_kind(status: i64, crash_left: bool) -> ExitKind {
    match status {
        0 => ExitKind::Clean,
        status if stopped_status(status) => ExitKind::Stopped,
        _ if crash_left => ExitKind::Crashed,
        2 => ExitKind::Refused,
        _ => ExitKind::Unwitnessed,
    }
}

/// THE WORD OF A RELAUNCH LINE'S OWN COMMAND, once its block ended.
#[must_use]
pub fn line_exit_kind(status: i64, crash_left: bool) -> ExitKind {
    match status {
        1 | 126 | 127 if !crash_left => ExitKind::Refused,
        status => exit_kind(status, crash_left),
    }
}

/// THE ENVIRONMENT AN AGY RELAUNCH LINE MUST CARRY.
#[must_use]
pub fn relaunch_env<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
    user_home: &Path,
) -> Vec<(String, String)> {
    get(HOME_ENV)
        .filter(|h| Path::new(h).has_root() && Path::new(h) != user_home)
        .map(|h| vec![(HOME_ENV.to_string(), h.to_string())])
        .unwrap_or_default()
}

/// [`relaunch_env`]'s pairs as the line holds them.
#[must_use]
pub fn line_env(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// [`relaunch_env`]'s pairs as a record keeps them (`NAME=VALUE`).
#[must_use]
pub fn env_words(env: &[(String, String)]) -> Vec<String> {
    env.iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect()
}

/// [`env_words`] read back: a word naming another variable, or none, is dropped.
#[must_use]
pub fn env_of_words(words: &[String]) -> Vec<(String, String)> {
    words
        .iter()
        .filter_map(|w| w.split_once('='))
        .filter(|(name, _)| *name == HOME_ENV)
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// Whether `program` is a build in an atpkg store (`…/store/agy/<build>/ bin/agy`).
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

/// THE LINE A PERSON IS TOLD to resume an agy conversation by hand.
#[must_use]
pub fn told_line(conversation: &str, env: &[(String, String)], argv: &[String]) -> Option<String> {
    if !is_conversation_uuid(conversation) {
        return None;
    }
    let plain = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./:@%+,=".contains(&b))
    };
    let word = |value: &str| {
        if plain(value) {
            value.to_string()
        } else {
            super::upgrade::quote(super::upgrade::Dialect::Zsh, value)
        }
    };
    let mut said = String::new();
    for (name, value) in env {
        if value.chars().any(char::is_control) {
            return None;
        }
        said.push_str(name);
        said.push('=');
        said.push_str(&word(value));
        said.push(' ');
    }
    said.push_str(PROGRAM);
    for flag in config_flags(argv) {
        if flag.chars().any(char::is_control) {
            return None;
        }
        said.push(' ');
        said.push_str(&word(&flag));
    }
    said.push_str(&format!(" --{FLAG_CONVERSATION}={conversation}"));
    Some(said)
}

#[cfg(test)]
#[path = "upgrade_agy_tests.rs"]
mod tests;

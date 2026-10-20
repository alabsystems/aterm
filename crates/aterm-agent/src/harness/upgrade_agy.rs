// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE AGY LANE, ITS PURE HALF: relaunch on exit and the restore of an agy
//! (Google's Antigravity CLI, `agy`) tab, never its live upgrade
//! ([`super::lanes::lanes`]: `relaunch` only). The I/O half is
//! `upgrade_drive`'s `agy` module (`upgrade_agy_drive.rs`). Every fact here
//! was measured on 1.4.0 in the lab (`crates/aterm-lab`, the real binary
//! against its Gemini API stub, no account; the lanes research and its
//! premeasure of 2026-10-17):
//!
//! * **One process.** A native Go binary launched directly (the managed twin
//!   `agents/agy` exports `AGY_CLI_DISABLE_AUTO_UPDATE='true'` and then
//!   `exec '<store build>' "$@"`), one TUI process, its language server in
//!   process. `argv[0]` is the store build, which a relaunch never types: it
//!   would pin that build and skip the twin's export.
//! * **Its flags are Go's** ([`FLAGS`]): `-name` and `--name` alike, a value
//!   attached (`--effort=high`) or the next token (`-effort high`), a switch
//!   alone; the argv keeps each as typed. A positional prompt is REFUSED by
//!   agy (exit 2, before any TUI), so a running agy never has one; one-shots
//!   (`-p`, `--output-format`, `--version`, `help`) and the subcommands are
//!   no TUI ([`is_tui_argv`]).
//! * **Resume by id.** `agy --conversation=<uuid>` brings a conversation back,
//!   its prior turns in the next request (proven on the stub). The id is a
//!   UUIDv4 ([`is_conversation_uuid`]); `-c`/`--continue` (the newest) is
//!   never typed. An unknown id is no refusal: agy prints `conversation not
//!   found` and starts afresh with no lock, which the I/O half reads after the
//!   relaunch (the new process must hold the conversation's lock).
//! * **Where its conversations live** ([`config_dir`]): `$HOME/.gemini/
//!   antigravity-cli`, moved by nothing in the environment (seven variables
//!   tried) but `HOME` and two hidden flags, `--gemini_dir=<abs>` and
//!   `--app_data_dir=<rel>`, which a relaunch therefore carries (without them
//!   the resume says `not found` and starts fresh), and a relaunch under
//!   another `HOME` carries that `HOME` ([`relaunch_env`]).
//! * **The conversation witness** ([`lock_conversation`]): the exclusive
//!   flock on `<config>/presence/<uuid>.lock`, held open by the process
//!   (`lsof`), taken at the first message or at once with `--conversation`,
//!   moved by `/clear` and an in-TUI `/resume`. The file is never removed, so
//!   only a HELD lock names a live process's conversation; a fresh agy that
//!   took no turn holds none (no conversation yet). A second agy opened on a
//!   conversation another holds gets NO lock, even after the holder exits,
//!   however it opened it — `--conversation`, `-c`/`--continue` (whose argv
//!   names no id) or an in-TUI `/resume` (measured on 1.4.2): so a free lock
//!   alone never proves nobody runs it. Every agy that has a conversation open
//!   holds its database (`<config>/conversations/<uuid>.db`,
//!   [`conversation_db`]) open, the lockless one too, and one that holds that
//!   database and no presence lock at all ([`holds_no_lock`]) is read as
//!   having it open; the argv of every live agy is read as well
//!   ([`resumed_conversation`]). A `/clear` takes a new conversation's lock at
//!   once and keeps the old database open, so a database alone is no witness.
//! * **The exit table** ([`exit_kind`]): the shell's exit status of the block
//!   it ran in, AND its crash file. `0` is every graceful way out (`/quit`,
//!   Ctrl-C or Ctrl-D twice, SIGTERM, SIGHUP, SIGINT, mid-turn too): someone's
//!   end, never relaunched. `146` is Ctrl-Z: agy STOPPED and still alive,
//!   holding its lock — never a crash; so is every other stop signal's status
//!   ([`stopped_status`]). Any other status is a crash only where the dead
//!   pid's crash file (`<config>/crashes/crash_<pid>_<uuid>.log`,
//!   [`crash_file`]) is still there: SIGKILL leaves it (measured), every
//!   graceful way out removes it, and the NEXT agy start deletes a dead pid's
//!   file (so it is read before any relaunch). Without it, `2` is a start agy
//!   REFUSED (bad arguments), and any other is an end no witness tells:
//!   neither is relaunched. A Go panic is UNMEASURED — its status, and whether
//!   its crash file survives it: it is relaunched only if it leaves that file,
//!   and else read as a refusal or an unwitnessed end, never relaunched.
//! * **The relaunch line**: `cd <the launch's directory>` (the workspace IS
//!   the launch cwd: from another directory agy asks to trust that one), then
//!   the MANAGED `agy` `--conversation=<uuid>` after the launch's carried
//!   flags ([`rewrite_argv`]), behind its `HOME` where that was another than
//!   the user's ([`line_env`]).
//! * **A relaunch is typed once per crash**: a line agy refused, or the shell
//!   could not run ([`line_exit_kind`]), is never typed again; only that
//!   line's own process crashing (its crash file left) types it again.

use std::path::{Component, Path, PathBuf};

use super::upgrade::ArgvRefusal;

/// The program's name: its image's basename, `aterm_phase::program_of`'s,
/// and the managed twin's (`agents/agy`).
pub const PROGRAM: &str = "agy";

/// The one variable that moves agy's config (its conversations, their locks
/// and its crash files): `$HOME/.gemini/antigravity-cli` (measured: none of
/// `ANTIGRAVITY_CONFIG_REL_DIR`, `ANTIGRAVITY_EXECUTABLE_DATA_DIR`,
/// `GEMINI_CLI_HOME`, `XDG_CONFIG_HOME`, `JETSKI_APP_DATA_DIR`,
/// `ANTIGRAVITY_APP_DATA_DIR` moves it).
pub const HOME_ENV: &str = "HOME";

/// The default Gemini directory under the home (`.gemini`), which
/// `--gemini_dir=<abs>` replaces.
pub const GEMINI_DIR: &str = ".gemini";

/// The default app data directory under it (`antigravity-cli`), which
/// `--app_data_dir=<rel>` replaces.
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
/// a relaunch. A flag missing here refuses the rewrite
/// ([`ArgvRefusal::UnknownFlag`]): nothing guesses whether a new flag takes a
/// value. The rule: what the CONVERSATION keeps, or what does nothing on a
/// resume, is dropped (measured flag by flag); what this RUN chose and the
/// conversation does not keep is carried, so the relaunched run is the
/// launched one (`--effort`, `--model`, `--mode` measured; the run's
/// permission posture and restrictions with them); what makes the relaunch
/// another run, or is print mode's and unmeasured on a resume, refuses.
const FLAGS: &[(&str, Arity, Fate)] = &[
    // Kept by the conversation (measured: its workspace URIs and the system
    // prompt's active workspaces name it on a resume without the flag;
    // carried again it changes nothing).
    ("add-dir", Arity::One, Fate::Drop),
    // Kept by the conversation (measured: footer, summary and the agent's
    // sentinel come back without it; a resume WITH one prints `ignoring
    // --agent … because we are resuming`).
    ("agent", Arity::One, Fate::Drop),
    // Hidden: moves the config to `$HOME/.gemini/<rel>` (measured). Without
    // it the resume says `conversation not found` and starts fresh.
    (FLAG_APP_DATA_DIR, Arity::One, Fate::Keep),
    // The newest conversation: the witness names the very one, typed as
    // `--conversation` in its place.
    ("c", Arity::None, Fate::Drop),
    ("continue", Arity::None, Fate::Drop),
    // The launch's own conversation: replaced by the witnessed one (which a
    // `/clear` or `/resume` may have moved since).
    (FLAG_CONVERSATION, Arity::One, Fate::Drop),
    // The run's permission posture, as `--mode` is, which the conversation
    // measured does NOT keep: carried so the relaunched run asks as the
    // launched one did (unmeasured across a resume itself; carried, it can
    // only restate what the person launched).
    ("dangerously-skip-permissions", Arity::None, Fate::Keep),
    // Print mode's: unmeasured on an interactive launch and on a resume.
    ("disable-slash-commands", Arity::None, Fate::Refuse),
    // Session-only (measured: a resume without it drops to `low`).
    ("effort", Arity::One, Fate::Keep),
    // Hidden: moves the Gemini directory (absolute; measured). Without it the
    // resume says `conversation not found` and starts fresh.
    (FLAG_GEMINI_DIR, Arity::One, Fate::Keep),
    ("h", Arity::None, Fate::OneShot),
    ("help", Arity::None, Fate::OneShot),
    // An initial prompt, sent at once (measured): carried, it would be sent
    // again into the resumed conversation.
    ("i", Arity::One, Fate::Drop),
    ("prompt-interactive", Arity::One, Fate::Drop),
    // Print mode's: unmeasured on an interactive launch and on a resume.
    ("input-format", Arity::One, Fate::Refuse),
    ("json-schema", Arity::One, Fate::Refuse),
    // Where this run logs: nothing of the conversation's, so the relaunched
    // run logs where the launched one did (unmeasured across a resume; it
    // moves no conversation).
    ("log-file", Arity::One, Fate::Keep),
    // Not kept by the conversation (measured: a plan-mode conversation came
    // back in the default mode, on the default model).
    ("mode", Arity::One, Fate::Keep),
    ("model", Arity::One, Fate::Keep),
    // A no-op on a resume (measured: no project made, none changed).
    ("new-project", Arity::None, Fate::Drop),
    ("output-format", Arity::One, Fate::OneShot),
    ("p", Arity::One, Fate::OneShot),
    ("print", Arity::One, Fate::OneShot),
    ("prompt", Arity::One, Fate::OneShot),
    ("print-timeout", Arity::One, Fate::Refuse),
    // Ignored on a resume without a word (measured).
    ("project", Arity::One, Fate::Drop),
    // Types `/remote-control` into itself and opens a NEW remote session with
    // a new URL on every launch, never kept with the conversation (measured):
    // a relaunch would hand the session to a link nobody holds.
    ("remote-control", Arity::None, Fate::Refuse),
    // A restriction of this run: carried, so a relaunch is never looser than
    // its launch (unmeasured across a resume; it only restricts).
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
/// all (a one-shot flag, a subcommand, a positional). Kept apart, since one
/// word names both a flag and a subcommand (`remote-control`).
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
            // Go stops reading flags here: anything after is a positional,
            // which agy refuses.
            return match argv.get(i) {
                Some(_) => Err(Stop::Ends("positional".to_string())),
                None => Ok(out),
            };
        }
        let Some((name, attached)) = flag_token(tok) else {
            // The first positional: a subcommand (a tool) or a prompt agy
            // refuses (exit 2, before any TUI).
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

/// Whether `argv` is an interactive agy TUI's — no subcommand, no positional
/// (agy refuses one), no one-shot flag (`-p`, `--output-format`, `--version`,
/// `-h`) — whose end is the one it was launched for otherwise. An argv the
/// rewrite refuses for a flag it does not know, or one that refuses, is still
/// a TUI's (its relaunch is refused at the exit, with its word).
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
/// occurrence winning. Read without the table, so a value is found whatever
/// unknown flag stands beside it.
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

/// THE CONVERSATION AN ARGV RESUMES (`--conversation`, any spelling), as
/// given: what a live agy opened on a conversation names in its argv even
/// where it holds no lock (a second agy on a held conversation, measured),
/// and what a relaunch line's process names. `None` with none.
#[must_use]
pub fn resumed_conversation(argv: &[String]) -> Option<String> {
    flag_value(argv, FLAG_CONVERSATION)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// The flags of `argv` that MOVE agy's config, verbatim and in order
/// (`--gemini_dir`, `--app_data_dir`, each with its value): what the line a
/// person is told carries, so its resume finds the conversation.
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

/// THE WORDS A RELAUNCH TYPES AFTER THE PROGRAM: the launch's carried flags
/// verbatim ([`FLAGS`]), then `--conversation=<uuid>` — or, with no
/// conversation (`None`), the carried flags alone, a fresh start. The
/// launch's own `--conversation`, `-c`, `-i` prompt and dropped flags are
/// not carried.
///
/// # Errors
/// An unknown flag ([`ArgvRefusal::UnknownFlag`]), a launch no relaunch
/// brings back ([`ArgvRefusal::NotResumable`]: a one-shot, a subcommand, a
/// positional, `--remote-control`, a print-mode flag), or a conversation
/// that is no UUIDv4.
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

/// Whether `s` is a conversation id agy prints and takes back: a UUIDv4,
/// lowercase (8-4-4-4-12 hex, its version `4`, its variant `8`-`b`).
#[must_use]
pub fn is_conversation_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 36
        && super::codex_rollout::is_thread_id(s)
        && !s.bytes().any(|c| c.is_ascii_uppercase())
        && b[14] == b'4'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
}

// ------------------------------------------------------------- the config

/// THE CONFIG DIRECTORY A PROCESS WITH THIS ARGV AND HOME USES:
/// `<gemini dir>/<app data dir>`, the Gemini directory the last absolute
/// `--gemini_dir` (a relative one agy ignores) else `<home>/.gemini`, the app
/// data directory the last `--app_data_dir` else `antigravity-cli`. `home`
/// is the process's own `HOME` (absolute), else `fallback`. `None` for an
/// `--app_data_dir` that is absolute, empty or climbs (`..`): unmeasured,
/// so nothing is read and nothing relaunched. Absolute here and in
/// [`relaunch_env`] is rooted ([`Path::has_root`]), as Gemini CLI's lane
/// reads it ([`super::upgrade_gemini::gemini_dir`]).
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

/// The conversation `lock` is the presence lock of, under `config`:
/// `<config>/presence/<uuid>.lock`, the uuid a UUIDv4. Any other path is
/// none.
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

/// THE DATABASE OF `conversation` under the config `config`
/// (`<config>/conversations/<uuid>.db`): every agy that has the conversation
/// open holds it open — its lock's holder, AND one opened on it while another
/// held it (`--conversation`, `-c`/`--continue`, an in-TUI `/resume`), which
/// holds no lock and stays so once the holder is gone (measured on 1.4.2). A
/// `/clear` keeps it open too, as it takes its new conversation's lock: so
/// only a holder of it that holds no presence lock ([`holds_no_lock`]) is
/// read as having the conversation open without its lock.
#[must_use]
pub fn conversation_db(config: &Path, conversation: &str) -> PathBuf {
    config
        .join(CONVERSATIONS)
        .join(format!("{conversation}.db"))
}

/// Whether the files an agy holds open (`files`, `lsof`'s canonical paths)
/// hold no presence lock under its config `config` ([`lock_conversation`]):
/// a fresh agy that took no turn, or one that opened a conversation another
/// agy held ([`conversation_db`]).
#[must_use]
pub fn holds_no_lock(config: &Path, files: &[PathBuf]) -> bool {
    !files
        .iter()
        .any(|file| lock_conversation(config, file).is_some())
}

/// The pid a crash file's name names (`crash_<pid>_<uuid>.log`, its uuid
/// any UUID: a new one each launch, never the conversation's). `None` for
/// any other name.
#[must_use]
pub fn crash_file_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("crash_")?.strip_suffix(".log")?;
    let (pid, uuid) = rest.split_once('_')?;
    (super::codex_rollout::is_thread_id(uuid) && pid.bytes().all(|b| b.is_ascii_digit()))
        .then(|| pid.parse().ok())
        .flatten()
        .filter(|&p| p > 0)
}

/// Whether `file` is a crash file of `pid` under `config`
/// (`<config>/crashes/crash_<pid>_<uuid>.log`): created at its launch, held
/// open for writing, removed by every graceful way out, left by a SIGKILL.
#[must_use]
pub fn crash_file(config: &Path, file: &Path, pid: u32) -> bool {
    file.parent() == Some(config.join(CRASHES).as_path())
        && file
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(crash_file_pid)
            == Some(pid)
}

// ------------------------------------------------------------- the exit

/// How an agy process ended, by its shell's exit status and its crash file
/// ([`exit_kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// `0`: its own exit or someone's SIGTERM, SIGHUP or SIGINT (each
    /// graceful, mid-turn too: the crash file removed): never relaunched.
    Clean,
    /// `146`: Ctrl-Z — or any other stop signal's status
    /// ([`stopped_status`]). agy is STOPPED, alive, its lock and crash file
    /// held: never a crash, never relaunched (after `fg`, a new block carries
    /// its real end).
    Stopped,
    /// `2` with no crash file left: a start agy refused (bad arguments, a
    /// positional), never retried — a relaunch would meet the same refusal.
    Refused,
    /// Any other status with the dead pid's crash file still there (SIGKILL,
    /// measured; a Go panic only if it leaves the file, unmeasured):
    /// relaunched on its conversation.
    Crashed,
    /// Any other status and no crash file: an end no witness tells (the file
    /// swept by another agy's start, or never written). Nothing is relaunched
    /// on a guess.
    Unwitnessed,
}

impl ExitKind {
    /// One word for the step and the ledger.
    #[must_use]
    pub fn word(self) -> &'static str {
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

/// Whether `status` is the shell's word for a job a signal STOPPED (`128 +`
/// a [`STOP_SIGNALS`] member): Ctrl-Z's `146` on macOS (measured), and
/// SIGSTOP's, SIGTTIN's and SIGTTOU's — the shell marks the block complete
/// while the process lives on, holding its lock and its crash file.
#[must_use]
pub fn stopped_status(status: i64) -> bool {
    STOP_SIGNALS
        .iter()
        .any(|&signal| status == 128 + i64::from(signal))
}

/// AGY'S EXIT TABLE, measured on 1.4.0 (the block's exit, which equals the
/// typed `$?` in every case, fresh and resumed, with and without the lab
/// launcher's `clear`): `0` [`ExitKind::Clean`]; `146` (Ctrl-Z) and every
/// other stop signal's status ([`stopped_status`]) [`ExitKind::Stopped`];
/// any other status a crash ONLY where `crash_left` — the dead pid's crash
/// file still there ([`crash_file`]) — and else `2` [`ExitKind::Refused`],
/// any other [`ExitKind::Unwitnessed`]. The status alone could not tell a
/// refusal (`2`) from a Go panic that exited `2` (unmeasured); the crash file
/// can, where the panic leaves it.
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

/// THE WORD OF A RELAUNCH LINE'S OWN COMMAND, once its block ended: agy's
/// table ([`exit_kind`], `crash_left` of the process the line started, where
/// one was found), and with no crash file left `126`/`127` — the shell could
/// not run the program the line names (the twin, or the build it execs) —
/// and `1` — a `cd` the line could not make (`( cd -- <dir> && … )`) — a
/// refusal too: typed again, the line meets the same wall.
#[must_use]
pub fn line_exit_kind(status: i64, crash_left: bool) -> ExitKind {
    match status {
        1 | 126 | 127 if !crash_left => ExitKind::Refused,
        status => exit_kind(status, crash_left),
    }
}

// ------------------------------------------------------------- the line

/// THE ENVIRONMENT AN AGY RELAUNCH LINE MUST CARRY: its `HOME` where the
/// process ran under an absolute `HOME` other than `user_home` (the user's
/// own, the shell's the line is typed at; `get` names a variable of its exec
/// environment). Such an agy keeps its conversations under ITS home's
/// `.gemini`: relaunched under the shell's, the resume says `conversation
/// not found` and starts fresh (measured), and so would the line a person is
/// told ([`told_line`]). Nothing else in the environment moves the config
/// (measured), so nothing else is carried. The Claude Code and Muse Code
/// lanes carry the `HOME` of an agent under another one the same way.
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

/// [`relaunch_env`]'s pairs as the line holds them; REQUIRED, as Muse Code's
/// homes are (`upgrade_muse::line_env`): a line that cannot hold a value is
/// refused rather than typed without it.
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

/// [`env_words`] read back: a word naming another variable, or none, is
/// dropped.
#[must_use]
pub fn env_of_words(words: &[String]) -> Vec<(String, String)> {
    words
        .iter()
        .filter_map(|w| w.split_once('='))
        .filter(|(name, _)| *name == HOME_ENV)
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// Whether `program` is a build in an atpkg store (`…/store/agy/<build>/
/// bin/agy`): the path the twin execs, which a relaunch never types (it
/// would pin that build and skip the twin's `AGY_CLI_DISABLE_AUTO_UPDATE`,
/// critic X3) — the managed twin is typed in its place, or nothing.
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

/// THE LINE A PERSON IS TOLD to resume an agy conversation by hand: agy's own
/// resume hint (`agy --conversation=<uuid>`, which it prints on every way
/// out), after the flags of the launch that moved its config
/// ([`config_flags`]: without them the resume starts fresh) and behind the
/// `HOME` its relaunch carries, each word quoted as a shell reads it. `None`
/// for a conversation that is no UUIDv4 or a word no line can hold (a
/// control character).
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

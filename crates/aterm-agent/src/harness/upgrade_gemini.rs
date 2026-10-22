// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE GEMINI CLI LANE, ITS PURE HALF: relaunch on exit and the restore of a
//! Gemini CLI (Google's `gemini`, the npm package `@google/gemini-cli`) tab,
//! never its live upgrade ([`super::lanes::lanes`]: `relaunch` only; it is a
//! Homebrew or npm install with no atpkg row, so no build is moved under it).
//! The I/O half is `upgrade_drive`'s `gemini` module
//! (`upgrade_gemini_drive.rs`). Every fact here was measured on 0.46.0
//! (Homebrew) in the lab (`crates/aterm-lab`, the real CLI against its Gemini
//! API stub, no account; the lanes research and its premeasure of
//! 2026-10-17), or read in its bundle where marked:
//!
//! * **Two node processes.** Typed `gemini` runs the PATH script
//!   (`/opt/homebrew/bin/gemini`) under its shebang's `node`: the PARENT
//!   `node /opt/homebrew/bin/gemini [args]` is the tab's foreground job, and
//!   it starts a CHILD `node --max-old-space-size=… <same script> [args]` in
//!   its group, which runs the TUI. Both are `node` to the kernel, so the
//!   snapshot reads `argv[1]` ([`is_gemini_process`]): the script's basename
//!   `gemini`, or the bundle's `@google/gemini-cli/bundle/gemini.js` a launch
//!   by path names.
//! * **Its flags are yargs's** ([`FLAGS`]): `--name`, `--name=value`, `-x`,
//!   `-x value`; a switch takes a following `true`/`false`. A positional is
//!   an INTERACTIVE first prompt (0.46.0: `Positional arguments now default
//!   to interactive mode`), sent once: dropped. One-shots (`-p`, `-o`,
//!   `--acp`, `--list-sessions`, `-v`, `-h`) and the subcommands are no TUI
//!   ([`is_tui_argv`]).
//! * **Resume by id.** `gemini --resume <uuid>`, run IN THE PROJECT'S
//!   DIRECTORY (sessions are kept per project, the working directory: from
//!   another, `No previous sessions found for this project`, exit 42), brings
//!   a session back with its turns in the next request (proven on the stub).
//!   The id is a full UUID ([`is_session_uuid`]): `--resume <N>` is an index
//!   and `--resume`/`latest` the newest, never typed; `--session-id` takes
//!   any `[A-Za-z0-9_-]`, so a custom id that is no UUID is never typed.
//! * **Where its sessions live** ([`gemini_dir`], [`project_hash`]):
//!   `<home>/.gemini/tmp/<slug>/chats/session-<UTC minute>-<id8>.jsonl`, the
//!   home `GEMINI_CLI_HOME` where set, else `HOME` (bundle: `homedir()`); the
//!   slug the directory under `tmp/` whose `.project_root` names the project
//!   (bundle: `ProjectRegistry`); every header records `projectHash`, the
//!   SHA-256 of the project's path (measured).
//! * **The witness is a START-TIME RULE** ([`witness`]): no file names a pid
//!   and none is held open. Every launch, fresh or resumed, and every
//!   `/clear` writes a session header (`sessionId`, `projectHash`,
//!   `startTime`) 542-1108 ms after the child starts, its `startTime` the
//!   file's birth to under 1 ms (measured). So the ONE header of this project
//!   stamped after the process started names its session — and anything else
//!   is no witness: two (a `/clear`, another launch in the project since), none
//!   (not written yet), another session's file moved since it started (an
//!   in-TUI `/resume` switches to an older file and writes no header,
//!   measured), or an argv naming another id. Never resumed on a guess.
//! * **History loss on repeated resumes** ([`precheck`], measured): each
//!   `--resume` writes a header-only duplicate, and the CLI resumes the file
//!   of the id with the newest `lastUpdated`; after two resumes with no turn
//!   between, the third loads that empty duplicate. So before a resume is
//!   typed, the id's files are folded as the CLI folds them and the pick must
//!   hold every turn any of them holds.
//! * **The exit table** ([`exit_kind`]): the shell's exit status of the block
//!   the parent ran in. `0` is every graceful way out (`/quit`, `/exit`,
//!   Ctrl-C or Ctrl-D twice, SIGTERM to the child) and `130` a cancel: never
//!   relaunched. `1` is the child killed (its parent relays `code ?? 1`) and
//!   `137` the parent killed (its child orphaned): crashes. `41`, `42`, `44`,
//!   `52` (and print mode's `53`-`55`) are starts the CLI REFUSED (bundle:
//!   `FatalError` classes; `42` measured for an unknown id, another project's
//!   and a duplicate id): never crashes — but for a worker's own restart
//!   ([`restart_id`], [`ExitKind::Restarted`]): its `--session-id` met by
//!   the CLI's own restart, which runs the same argv again, is resumed by id
//!   with nothing typed. The parent ignores SIGTERM, SIGHUP and SIGINT
//!   (measured), so `143`, `129` and any other status are no witness.
//! * **The relaunch line**: `cd <the project>` and the `gemini` on the shell's
//!   PATH (never a Cellar path: the shell's own resolution is the install a
//!   person types), the launch's carried flags and `--resume <uuid>`
//!   ([`rewrite_argv`]), behind the homes its process ran under
//!   ([`line_env`]). The continuation is typed at the relaunched process's
//!   first idle point, never as `-i` on the line (measured to send at once,
//!   on top of the whole history): sent at once, it would reach a session
//!   whose task was done under Land, which every lane adopts with nothing
//!   typed, and a tab a person took meanwhile.

use std::path::{Component, Path, PathBuf};

use super::upgrade::ArgvRefusal;

/// The program's name: the script's basename, `aterm_phase::program_of`'s,
/// and the word a relaunch types (the shell's own `gemini`).
pub const PROGRAM: &str = "gemini";

/// The variable that moves the whole Gemini home (bundle: `homedir()`):
/// `<GEMINI_CLI_HOME>/.gemini` where set and not empty.
pub const CLI_HOME_ENV: &str = "GEMINI_CLI_HOME";

/// The home it falls back to: `os.homedir()`, `$HOME` on a Unix.
pub const HOME_ENV: &str = "HOME";

/// The variable naming the trusted-folders file the CLI reads in place of
/// `<gemini dir>/trustedFolders.json` (bundle 0.46.0:
/// `Storage.getTrustedFoldersPath`): a worker's own, which trusts the one
/// folder its starter trusted for it (`harness::worker`).
pub const TRUSTED_FOLDERS_ENV: &str = "GEMINI_CLI_TRUSTED_FOLDERS_PATH";

/// The Gemini directory under the home.
pub const GEMINI_DIR: &str = ".gemini";

/// The per-project directories under the Gemini directory.
pub const TMP_DIR: &str = "tmp";

/// The sessions' directory under a project's.
pub const CHATS_DIR: &str = "chats";

/// The file in a project's directory naming the project it is (bundle:
/// `PROJECT_ROOT_FILE`).
pub const PROJECT_ROOT_FILE: &str = ".project_root";

/// The flag that resumes a session.
const FLAG_RESUME: &str = "resume";

/// The flag that sends a first prompt and stays interactive, dropped from
/// every argv a relaunch reads (sent at once, measured: carried it would be
/// sent again).
const FLAG_PROMPT_INTERACTIVE: &str = "prompt-interactive";

/// The flag that starts a new session under a caller's id.
const FLAG_SESSION_ID: &str = "session-id";

/// The flag that starts a new session from a file's conversation.
const FLAG_SESSION_FILE: &str = "session-file";

/// The status a start the CLI refused for its input exits with (bundle
/// 0.46.0: `ExitCodes.FATAL_INPUT_ERROR`): an unknown id, another project's,
/// and a `--session-id` whose session is already written.
pub const FATAL_INPUT_ERROR: i64 = 42;

/// The words a session's user message opens with when it is the CLI's own
/// context, never a person's turn.
pub const SESSION_CONTEXT: &str = "<session_context>";

// ------------------------------------------------------------- the process

/// Whether `arg` (a parent's `argv[1]`) is Gemini CLI's script: a path whose
/// basename is `gemini` (the PATH link `/opt/homebrew/bin/gemini`, measured,
/// or npm's `bin/gemini`), or the bundle itself
/// (`…/@google/gemini-cli/bundle/gemini.js`, which a launch by path names).
#[must_use]
pub fn is_gemini_script(arg: &str) -> bool {
    let path = Path::new(arg);
    let names: Vec<&str> = path
        .components()
        .rev()
        .take(4)
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    matches!(
        names.as_slice(),
        [PROGRAM, ..] | ["gemini.js", "bundle", "gemini-cli", "@google"]
    )
}

/// Whether a process whose image is `image` and whose argv is `argv` is a
/// Gemini CLI process: a `node` image (the shebang's, measured) running
/// Gemini CLI's script as its first argument ([`is_gemini_script`]).
#[must_use]
pub fn is_gemini_process(image: Option<&Path>, argv: &[String]) -> bool {
    image
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "node")
        && argv.get(1).is_some_and(|a| is_gemini_script(a))
}

// ------------------------------------------------------------- the flags

/// How a flag takes its value (yargs, as 0.46.0 declares each: measured on
/// `--help` and read in the bundle's option table).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arity {
    /// A boolean: alone, `=value`, or followed by a bare `true`/`false`,
    /// which yargs takes as its value.
    Switch,
    /// Exactly one value (`nargs: 1`): attached, or the next token.
    One,
    /// A string with no `nargs` (`--resume`, `--worktree`): the next token
    /// where it is no flag, else none (`--resume` alone resumes the newest).
    Optional,
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
    /// The launch is no TUI at all: it prints, or runs one prompt, and exits.
    OneShot,
}

/// Gemini CLI's flags (0.46.0, `--help` and the bundle's yargs table, the
/// hidden test flags included), named without their dashes, each with its
/// fate on a relaunch. A flag missing here — a camelCase spelling, a `--no-`
/// negation, a short cluster — refuses the rewrite
/// ([`ArgvRefusal::UnknownFlag`]): nothing guesses what it does. The rule
/// (the lanes research's flag fates): what the CLI does not keep with the
/// session (it records only the model of each reply) is carried, so the
/// relaunched run is the launched one; what names the session or sends a
/// prompt again is dropped (the witnessed id is typed in its place); what
/// makes the relaunch another project or another run refuses.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("acp", Arity::Switch, Fate::OneShot),
    ("accept-raw-output-risk", Arity::Switch, Fate::Keep),
    ("admin-policy", Arity::One, Fate::Keep),
    ("allowed-mcp-server-names", Arity::One, Fate::Keep),
    ("allowed-tools", Arity::One, Fate::Keep),
    // The run's posture, never kept with the session (an in-session change
    // is lost with the process): carried as launched.
    ("approval-mode", Arity::One, Fate::Keep),
    ("d", Arity::Switch, Fate::Keep),
    ("debug", Arity::Switch, Fate::Keep),
    ("delete-session", Arity::One, Fate::OneShot),
    ("e", Arity::One, Fate::Keep),
    ("experimental-acp", Arity::Switch, Fate::OneShot),
    ("extensions", Arity::One, Fate::Keep),
    // Hidden test flags: the run answers from a file, never the model.
    ("fake-responses", Arity::One, Fate::Refuse),
    ("fake-responses-non-strict", Arity::One, Fate::Refuse),
    ("h", Arity::Switch, Fate::OneShot),
    ("help", Arity::Switch, Fate::OneShot),
    // A first prompt, sent at once (measured with `--resume`): carried, it
    // would be sent again into the resumed session.
    ("i", Arity::One, Fate::Drop),
    (FLAG_PROMPT_INTERACTIVE, Arity::One, Fate::Drop),
    ("include-directories", Arity::One, Fate::Keep),
    ("l", Arity::Switch, Fate::OneShot),
    ("list-extensions", Arity::Switch, Fate::OneShot),
    ("list-sessions", Arity::Switch, Fate::OneShot),
    ("m", Arity::One, Fate::Keep),
    ("model", Arity::One, Fate::Keep),
    ("o", Arity::One, Fate::OneShot),
    ("output-format", Arity::One, Fate::OneShot),
    ("p", Arity::One, Fate::OneShot),
    ("policy", Arity::One, Fate::Keep),
    ("prompt", Arity::One, Fate::OneShot),
    // The launch's own session: replaced by the witnessed one (which a
    // `/clear` or an in-TUI `/resume` may have moved past).
    ("r", Arity::Optional, Fate::Drop),
    ("raw-output", Arity::Switch, Fate::Keep),
    ("record-responses", Arity::One, Fate::Refuse),
    (FLAG_RESUME, Arity::Optional, Fate::Drop),
    ("s", Arity::Switch, Fate::Keep),
    // A restriction of this run (its sandbox): carried, never looser.
    ("sandbox", Arity::Switch, Fate::Keep),
    ("screen-reader", Arity::Switch, Fate::Keep),
    // An import under a new id, which the witness names: the id is typed.
    ("session-file", Arity::One, Fate::Drop),
    (FLAG_SESSION_ID, Arity::One, Fate::Drop),
    ("skip-trust", Arity::Switch, Fate::Keep),
    ("v", Arity::Switch, Fate::OneShot),
    ("version", Arity::Switch, Fate::OneShot),
    // Another project (a new git worktree, behind an experimental setting):
    // the sessions are the working directory's, so never carried.
    ("w", Arity::Optional, Fate::Refuse),
    ("worktree", Arity::Optional, Fate::Refuse),
    ("y", Arity::Switch, Fate::Keep),
    ("yolo", Arity::Switch, Fate::Keep),
];

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Gemini CLI's subcommands and their aliases (0.46.0 `--help`): each a
/// tool, never the TUI, when it is the first positional.
const SUBCOMMANDS: &[&str] = &[
    "extension",
    "extensions",
    "gemma",
    "hook",
    "hooks",
    "mcp",
    "skill",
    "skills",
];

/// A flag token as yargs reads one this table can name: `--name` or
/// `--name=value` (a long name), `-x` or `-x=value` (one letter). `None` for
/// a token that is no flag (a positional, `-` alone). A short cluster
/// (`-sy`) or a long name with one dash is returned whole, so the table
/// refuses it.
fn flag_token(tok: &str) -> Option<(&str, Option<&str>)> {
    let bare = tok.strip_prefix("--").or_else(|| tok.strip_prefix('-'))?;
    if bare.is_empty() {
        return None;
    }
    Some(match bare.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (bare, None),
    })
}

/// Whether `tok` reads as a flag to yargs (`-` and a letter or a dash).
fn looks_like_flag(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// One launch's argv, read: the flags it carries, verbatim and in order,
/// and whether it carries a first prompt (`-i`/`--prompt-interactive`, a
/// positional, words after `--`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Read {
    kept: Vec<String>,
    prompt: bool,
}

/// Why a [`read`] stopped, with its word.
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

/// [`Read`] of `argv` (the parent's: `argv[0]` its node, `argv[1]` its
/// script), or why it cannot be carried ([`Stop`]). Positionals are the
/// first prompt and dropped; the first one naming a subcommand ends it.
fn read(argv: &[String]) -> Result<Read, Stop> {
    let mut out = Read::default();
    let mut positional_seen = false;
    let mut i = 2;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            // Everything after is the first prompt's words: dropped.
            out.prompt |= i < argv.len();
            return Ok(out);
        }
        let Some((name, attached)) = flag_token(tok) else {
            if !positional_seen && SUBCOMMANDS.contains(&tok) {
                return Err(Stop::Ends(tok.to_string()));
            }
            positional_seen = true;
            out.prompt = true;
            continue;
        };
        let Some((arity, fate)) = flag(name) else {
            return Err(Stop::Unknown(name.to_string()));
        };
        out.prompt |= name == "i" || name == FLAG_PROMPT_INTERACTIVE;
        let mut words = vec![tok.to_string()];
        if attached.is_none() {
            match arity {
                Arity::Switch => {
                    if let Some(value) = argv
                        .get(i)
                        .filter(|v| matches!(v.as_str(), "true" | "false"))
                    {
                        words.push(value.clone());
                        i += 1;
                    }
                }
                Arity::One => {
                    // A value that reads as a flag is no value this table
                    // can vouch yargs took: refused, never guessed.
                    let Some(value) = argv.get(i).filter(|v| !looks_like_flag(v)) else {
                        return Err(Stop::Unknown(name.to_string()));
                    };
                    words.push(value.clone());
                    i += 1;
                }
                Arity::Optional => {
                    if let Some(value) = argv.get(i).filter(|v| !looks_like_flag(v)) {
                        words.push(value.clone());
                        i += 1;
                    }
                }
            }
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

/// Whether `argv` is an interactive Gemini CLI TUI's — no subcommand, no
/// one-shot flag (`-p`, `-o`, `--acp`, `--list-sessions`, `-v`, `-h`) —
/// whose end is the one it was launched for otherwise. An argv the rewrite
/// refuses for a flag it does not know, or one that refuses, is still a
/// TUI's (its relaunch is refused at the exit, with its word).
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    match read(argv) {
        Ok(_) | Err(Stop::Refuse(_)) => true,
        Err(Stop::Ends(_)) => false,
        Err(Stop::Unknown(_)) => !one_shot_anywhere(argv),
    }
}

/// A one-shot flag anywhere before `--`, or a subcommand as the first word
/// that is no flag, read without the flag table's arities.
fn one_shot_anywhere(argv: &[String]) -> bool {
    let words = || argv.iter().skip(2).take_while(|t| t.as_str() != "--");
    words().any(|t| {
        flag_token(t)
            .is_some_and(|(name, _)| flag(name).is_some_and(|(_, fate)| fate == Fate::OneShot))
    }) || words()
        .find(|t| flag_token(t).is_none())
        .is_some_and(|t| SUBCOMMANDS.contains(&t.as_str()))
}

/// The last value of a flag named one of `names` in `argv` (`--x=v`, `--x
/// v`, `-x v`), before `--`, read without the table.
fn flag_value<'a>(argv: &'a [String], names: &[&str]) -> Option<&'a str> {
    let mut found = None;
    let mut i = 2;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        let Some((name, attached)) = flag_token(tok) else {
            continue;
        };
        if !names.contains(&name) {
            continue;
        }
        found = match attached {
            Some(value) => Some(value),
            None => {
                let value = argv
                    .get(i)
                    .map(String::as_str)
                    .filter(|v| !looks_like_flag(v));
                if value.is_some() {
                    i += 1;
                }
                value
            }
        };
    }
    found
}

/// WHETHER `argv` CARRIES A FIRST PROMPT — `-i`/`--prompt-interactive
/// <text>`, a positional, words after `--` — which no relaunch types again
/// ([`rewrite_argv`]). `true` for an argv the table cannot read: nothing
/// is vouched for it.
#[must_use]
pub fn carries_first_prompt(argv: &[String]) -> bool {
    read(argv).map_or(true, |r| r.prompt)
}

/// THE SESSION AN ARGV NAMES, where it names one by its full UUID: the
/// last `--resume`/`-r <uuid>` or `--session-id <uuid>`. `None` for none, an
/// index, `latest`, or a custom id that is no UUID. What a live Gemini CLI
/// resuming a session names in its argv, and what a witness must agree with.
#[must_use]
pub fn argv_session(argv: &[String]) -> Option<String> {
    flag_value(argv, &[FLAG_RESUME, "r", FLAG_SESSION_ID])
        .filter(|v| is_session_uuid(v))
        .map(str::to_owned)
}

/// THE WORDS A RELAUNCH TYPES AFTER THE PROGRAM: the launch's carried flags
/// verbatim ([`FLAGS`]), then `--resume <uuid>` — or, with no session
/// (`None`), the carried flags alone. The launch's own `--resume`,
/// `--session-id`, `--session-file`, `-i` prompt and positional prompt are
/// never carried.
///
/// # Errors
/// An unknown flag ([`ArgvRefusal::UnknownFlag`]), a launch no relaunch
/// brings back ([`ArgvRefusal::NotResumable`]: a one-shot, a subcommand,
/// `--worktree`, a test flag), or a session that is no full UUID.
pub fn rewrite_argv(argv: &[String], session: Option<&str>) -> Result<Vec<String>, ArgvRefusal> {
    let mut out = read(argv).map_err(ArgvRefusal::from)?.kept;
    if let Some(session) = session {
        if !is_session_uuid(session) {
            return Err(ArgvRefusal::NotResumable(format!("session {session}")));
        }
        out.push(format!("--{FLAG_RESUME}"));
        out.push(session.to_string());
    }
    Ok(out)
}

/// Whether `s` is a session id a relaunch may type back: a full hyphenated
/// UUID (8-4-4-4-12 hex). Never an index (`--resume 3` is one), `latest`, or
/// a `--session-id` of any other shape.
#[must_use]
pub fn is_session_uuid(s: &str) -> bool {
    s.len() == 36 && super::codex_rollout::is_thread_id(s)
}

// ------------------------------------------------------------- the homes

/// THE GEMINI DIRECTORY A PROCESS WITH THIS ENVIRONMENT USES: its
/// `GEMINI_CLI_HOME`'s `.gemini` where set and not empty, else its `HOME`'s
/// (absolute), else `fallback`'s. `None` for a `GEMINI_CLI_HOME` that is not
/// absolute: the CLI joins it to its working directory, unmeasured, so
/// nothing is read and nothing relaunched. Absolute here and in
/// [`relaunch_env`] is rooted ([`Path::has_root`]): on Windows `\h`, the
/// current drive's, counts and a drive-relative `C:h` does not.
#[must_use]
pub fn gemini_dir(cli_home: Option<&str>, home: Option<&str>, fallback: &Path) -> Option<PathBuf> {
    match cli_home.filter(|h| !h.is_empty()) {
        Some(h) if Path::new(h).has_root() => Some(Path::new(h).join(GEMINI_DIR)),
        Some(_) => None,
        None => Some(
            home.map(PathBuf::from)
                .filter(|h| h.has_root())
                .unwrap_or_else(|| fallback.to_path_buf())
                .join(GEMINI_DIR),
        ),
    }
}

/// THE PROJECT HASH every session header of the project at `root` records
/// (`projectHash`): the SHA-256 of its path as the CLI's working directory
/// names it, lowercase hex (measured: `/private/tmp/at.u46v/proj`'s).
#[must_use]
pub fn project_hash(root: &str) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(root.as_bytes()))
}

/// Whether `marker` (a project directory's `.project_root`) names the
/// project at `root`: its words, trimmed, are that path (the CLI's own
/// ownership check, bundle: `verifySlugOwnership`).
#[must_use]
pub fn owns(marker: &str, root: &str) -> bool {
    !root.is_empty() && marker.trim() == root
}

/// The chats directory of the project directory `project` (`<tmp>/<slug>`).
#[must_use]
pub fn chats_dir(project: &Path) -> PathBuf {
    project.join(CHATS_DIR)
}

/// Whether `name` is a session file of the session whose id is `session`:
/// `session-<minute>-<first 8 of the id>.jsonl` (`.json` for a legacy one,
/// which [`precheck`] cannot fold).
#[must_use]
pub fn names_session(name: &str, session: &str) -> bool {
    let Some(id8) = session.get(..8) else {
        return false;
    };
    name.starts_with("session-")
        && (name.ends_with(&format!("-{id8}.jsonl")) || name.ends_with(&format!("-{id8}.json")))
}

/// Whether `name` is any session file (`session-…jsonl`).
#[must_use]
pub fn is_session_file(name: &str) -> bool {
    name.starts_with("session-") && name.ends_with(".jsonl")
}

// ------------------------------------------------------------- the records

/// Unix milliseconds of an ISO stamp as the CLI writes one
/// (`2026-10-17T18:21:34.531Z`); `None` for anything else.
#[must_use]
pub fn stamp_ms(stamp: &str) -> Option<u64> {
    let secs = u64::try_from(aterm_types::rfc3339::parse_utc_fractional(stamp)?).ok()?;
    let frac = stamp
        .strip_suffix('Z')?
        .get(19..)
        .unwrap_or("")
        .trim_start_matches('.');
    let mut ms = 0_u64;
    for (i, d) in frac.bytes().take(3).enumerate() {
        ms += u64::from(d - b'0') * [100, 10, 1][i];
    }
    Some(secs * 1000 + ms)
}

/// One record of a session file, as the CLI's fold tells them apart (bundle:
/// `loadConversationRecord`).
enum Record<'a> {
    /// `{"$rewindTo": id}`: that message and every one after it dropped.
    Rewind(&'a str),
    /// A message (`id`), its kind and its stamp.
    Message(Message),
    /// `{"$set": {...}}`: merged into the metadata; `messages` replaces them.
    Set(&'a aterm_json::Value),
    /// A header or partial metadata (`sessionId` and `projectHash`).
    Header(&'a aterm_json::Value),
    /// Anything else.
    Other,
}

/// A message record: its id, whether it is a TURN (a person's message that is
/// no `<session_context>`, or any model reply), whether it is a user's or a
/// model's at all (a file holding one is a session to the CLI), its stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Message {
    id: String,
    turn: bool,
    user_or_model: bool,
    at_ms: Option<u64>,
}

fn text_of(content: Option<&aterm_json::Value>) -> String {
    match content {
        Some(aterm_json::Value::String(s)) => s.clone(),
        Some(aterm_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(aterm_json::Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

fn message_of(v: &aterm_json::Value) -> Option<Message> {
    let id = v.get("id")?.as_str()?.to_string();
    let kind = v.get("type").and_then(aterm_json::Value::as_str);
    let turn = match kind {
        Some("gemini") => true,
        Some("user") => !text_of(v.get("content")).starts_with(SESSION_CONTEXT),
        _ => false,
    };
    Some(Message {
        id,
        turn,
        user_or_model: matches!(kind, Some("user" | "gemini")),
        at_ms: v
            .get("timestamp")
            .and_then(aterm_json::Value::as_str)
            .and_then(stamp_ms),
    })
}

fn record_of(v: &aterm_json::Value) -> Record<'_> {
    if let Some(id) = v.get("$rewindTo").and_then(aterm_json::Value::as_str) {
        return Record::Rewind(id);
    }
    if v.get("id").and_then(aterm_json::Value::as_str).is_some() {
        return message_of(v).map_or(Record::Other, Record::Message);
    }
    if let Some(set) = v.get("$set").filter(|s| s.is_object()) {
        return Record::Set(set);
    }
    if v.get("sessionId")
        .and_then(aterm_json::Value::as_str)
        .is_some()
        && v.get("projectHash")
            .and_then(aterm_json::Value::as_str)
            .is_some()
    {
        return Record::Header(v);
    }
    Record::Other
}

/// Every line of `text` that parses, as JSON (a torn last line is skipped,
/// as the CLI skips it).
fn values(text: &str) -> Vec<aterm_json::Value> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .collect()
}

/// ONE SESSION FILE FOLDED AS THE CLI FOLDS IT: its session, project and
/// `lastUpdated` (the metadata, later records winning), and its messages in
/// order (a message id written again keeps its place; `$set.messages`
/// replaces them all; `$rewindTo` drops that message and every one after
/// it, all of them where it names none).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fold {
    /// `sessionId`.
    pub session: Option<String>,
    /// `projectHash`.
    pub project_hash: Option<String>,
    /// `lastUpdated`, as written (the CLI compares the strings).
    pub last_updated: Option<String>,
    /// The ids of its TURNS, in order: a person's messages that are no
    /// `<session_context>`, and every model reply.
    pub turns: Vec<String>,
    /// Whether it holds a user's or a model's message at all — the CLI's own
    /// test of a file that is a session (its `<session_context>` counts:
    /// a header-only duplicate is one).
    pub valid: bool,
}

/// [`Fold`] of one session file's `text`.
#[must_use]
pub fn fold(text: &str) -> Fold {
    fn str_of(v: &aterm_json::Value, key: &str) -> Option<String> {
        v.get(key)
            .and_then(aterm_json::Value::as_str)
            .map(str::to_owned)
    }
    let mut out = Fold::default();
    let mut messages: Vec<Message> = Vec::new();
    let put = |messages: &mut Vec<Message>, m: Message| match messages
        .iter_mut()
        .find(|old| old.id == m.id)
    {
        Some(old) => *old = m,
        None => messages.push(m),
    };
    for v in values(text) {
        let meta = |set: &aterm_json::Value, out: &mut Fold| {
            if let Some(s) = str_of(set, "sessionId") {
                out.session = Some(s);
            }
            if let Some(h) = str_of(set, "projectHash") {
                out.project_hash = Some(h);
            }
            if let Some(l) = str_of(set, "lastUpdated") {
                out.last_updated = Some(l);
            }
        };
        match record_of(&v) {
            Record::Rewind(id) => match messages.iter().position(|m| m.id == id) {
                Some(at) => messages.truncate(at),
                None => messages.clear(),
            },
            Record::Message(m) => put(&mut messages, m),
            Record::Set(set) => {
                if let Some(list) = set.get("messages").and_then(aterm_json::Value::as_array) {
                    messages.clear();
                    for m in list.iter().filter_map(message_of) {
                        put(&mut messages, m);
                    }
                }
                meta(set, &mut out);
            }
            Record::Header(h) => {
                meta(h, &mut out);
                if let Some(list) = h.get("messages").and_then(aterm_json::Value::as_array) {
                    for m in list.iter().filter_map(message_of) {
                        put(&mut messages, m);
                    }
                }
            }
            Record::Other => {}
        }
    }
    out.valid = messages.iter().any(|m| m.user_or_model);
    out.turns = messages
        .iter()
        .filter(|m| m.turn)
        .map(|m| m.id.clone())
        .collect();
    out
}

/// One session header a file holds (a launch's, a resume's duplicate, a
/// `/clear`'s): its session, project and `startTime`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// `sessionId`.
    pub session: String,
    /// `projectHash`.
    pub project_hash: String,
    /// `startTime`, unix milliseconds.
    pub start_ms: u64,
}

/// Every session header in `text` with a `startTime` (a resume in the same
/// UTC minute as its session's file APPENDS its header to that file:
/// measured).
#[must_use]
pub fn headers(text: &str) -> Vec<Header> {
    values(text)
        .iter()
        .filter_map(|v| match record_of(v) {
            Record::Header(h) => Some(Header {
                session: h.get("sessionId")?.as_str()?.to_string(),
                project_hash: h.get("projectHash")?.as_str()?.to_string(),
                start_ms: h
                    .get("startTime")
                    .and_then(aterm_json::Value::as_str)
                    .and_then(stamp_ms)?,
            }),
            _ => None,
        })
        .collect()
}

/// What one session file says of WHEN it was written, for the witness
/// ([`scan`]): the session headers it holds, the session it folds to, its
/// latest stamped write, and the switch onto it no stamped write follows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// Every header with a `startTime` ([`headers`]).
    pub headers: Vec<Header>,
    /// The session it folds to ([`Fold::session`]).
    pub session: Option<String>,
    /// Its latest stamped write, unix milliseconds: a message's `timestamp`
    /// or a `$set.lastUpdated` (a header's `startTime` is [`Self::headers`]').
    pub latest_ms: Option<u64>,
    /// The session a `$set` naming `sessionId` switches onto, where NO
    /// STAMPED WRITE FOLLOWS IT (a message with a `timestamp`, a
    /// `$set.lastUpdated`, a header with a `startTime`): what the CLI appends
    /// to a session's file as it switches onto it (bundle:
    /// `ChatRecordingService.initialize`, `updateMetadata({sessionId})`) —
    /// the one record an in-TUI `/resume` writes (measured: +62 B, no header,
    /// no file), and a `--resume` launch writes beside its header. Unstamped,
    /// so read with the file's mtime ([`Scan::moved`]). The UNSTAMPED records
    /// that may follow it hide nothing: the summary a later start writes into
    /// that file in the background, after an LLM call
    /// (`generateAndSaveSummary`: `$set` of `summary` and `memoryScratchpad`),
    /// the process's own `$set.summary` and `$set.directories`, a
    /// `$rewindTo` (bundle).
    pub trailing_switch: Option<String>,
}

/// [`Scan`] of one session file's `text`.
#[must_use]
pub fn scan(text: &str) -> Scan {
    let vals = values(text);
    let mut out = Scan {
        headers: headers(text),
        session: fold(text).session,
        ..Scan::default()
    };
    let stamp = |set: &aterm_json::Value, key: &str| {
        set.get(key)
            .and_then(aterm_json::Value::as_str)
            .and_then(stamp_ms)
    };
    for v in &vals {
        let (at, stamped, switch) = match record_of(v) {
            Record::Message(m) => (m.at_ms, m.at_ms.is_some(), None),
            Record::Set(set) => {
                let at = stamp(set, "lastUpdated");
                let switch = set.get("sessionId").and_then(aterm_json::Value::as_str);
                (at, at.is_some(), switch)
            }
            Record::Header(h) => (None, stamp(h, "startTime").is_some(), None),
            Record::Rewind(_) | Record::Other => (None, false, None),
        };
        if let Some(at) = at {
            out.latest_ms = Some(out.latest_ms.map_or(at, |was| was.max(at)));
        }
        // A stamped write tells its own time ([`Scan::latest_ms`]); only a
        // switch no stamped write follows is left to the mtime.
        if stamped {
            out.trailing_switch = None;
        } else if let Some(session) = switch {
            out.trailing_switch = Some(session.to_string());
        }
    }
    out
}

impl Scan {
    /// WHETHER THIS FILE MAY HAVE MOVED SINCE `start_ms` (it is another
    /// session's than the witness names): a write stamped since then
    /// (someone took a turn in it, or a resume rewrote it), or — written
    /// since then by its mtime `mtime_ms` (`None`: unread, read as since) —
    /// a switch onto it no stamped write follows ([`Scan::trailing_switch`]:
    /// an in-TUI `/resume` writes only that), unless `resumed` says every
    /// switch onto that session is a `--resume` launch's
    /// ([`resumes_account_for_switches`]). A summary the CLI writes into an
    /// older file at a start (`$set.summary`, measured) is unstamped and no
    /// switch, so on its own it moves nothing (its write moves the mtime),
    /// and an old switch a later stamped write follows was written before
    /// that write.
    #[must_use]
    pub fn moved(
        &self,
        start_ms: u64,
        mtime_ms: Option<u64>,
        resumed: &dyn Fn(&str) -> bool,
    ) -> bool {
        self.latest_ms.is_some_and(|at| at >= start_ms)
            || self.trailing_switch.as_deref().is_some_and(|session| {
                mtime_ms.is_none_or(|at| at >= start_ms) && !resumed(session)
            })
    }
}

/// WHETHER EVERY SWITCH ONTO `session` ITS FILES HOLD IS A `--resume`
/// LAUNCH'S, read over the session's files `files` (every one: a file of
/// another session is skipped). A `--resume` launch writes one header of the
/// session (its duplicate, or appended to its own minute's file) and one
/// switch onto it (into the file it loads); its first launch, or the
/// `/clear` that began it, one header and no switch; an in-TUI `/resume`
/// the switch alone (measured, bundle). So the switches are all resumes'
/// only where they are fewer than its headers. `false` where none is read,
/// or one is a legacy `.json` (its conversion writes a header with its
/// switch). The stated limits: a `--resume` that died between its header and
/// its switch (20-40 ms), or a file of the session deleted, can hide one
/// in-TUI `/resume`.
#[must_use]
pub fn resumes_account_for_switches(files: &[ChatsFile<'_>], session: &str) -> bool {
    let mine: Vec<&ChatsFile<'_>> = files
        .iter()
        .filter(|f| names_session(f.name, session))
        .collect();
    if mine.is_empty() || mine.iter().any(|f| !f.name.ends_with(".jsonl")) {
        return false;
    }
    let (mut headers, mut switches) = (0_usize, 0_usize);
    for f in mine {
        for v in values(f.text) {
            match record_of(&v) {
                Record::Header(h) => {
                    headers += usize::from(
                        h.get("sessionId").and_then(aterm_json::Value::as_str) == Some(session),
                    );
                }
                Record::Set(set) => {
                    switches += usize::from(
                        set.get("sessionId").and_then(aterm_json::Value::as_str) == Some(session),
                    );
                }
                Record::Message(_) | Record::Rewind(_) | Record::Other => {}
            }
        }
    }
    switches < headers
}

// ------------------------------------------------------------- the witness

/// One session file as the witness and the pre-check read it: its name, its
/// words and its mtime.
#[derive(Clone, Copy, Debug)]
pub struct ChatsFile<'a> {
    /// Its name in the chats directory.
    pub name: &'a str,
    /// Its words.
    pub text: &'a str,
    /// When it was last written, unix milliseconds (`None`: unread).
    pub mtime_ms: Option<u64>,
}

/// Why the start-time rule names no session ([`witness`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoWitness {
    /// The project's chats directory was not found or not read.
    NoChats,
    /// No session header was stamped since the process started (not written
    /// yet, or a resume whose header the rule cannot see).
    NoHeader,
    /// The one header stamped since the process started was begun later
    /// than its own could be ([`LATE_HEADER_MS`]), and its argv names no
    /// session it agrees with: another launch's, its own never written (it
    /// died first).
    LateHeader,
    /// Two or more were: a `/clear` since, or another Gemini CLI started in
    /// the project since this one did.
    TwoHeaders,
    /// Another Gemini CLI of the project started within the window its
    /// header could fall in: a start order no file tells apart (measured: 48
    /// ms apart, the files' order came out inverted).
    StartedTogether,
    /// The header names an id that is no full UUID (a custom
    /// `--session-id`), or another project.
    NotTyped,
    /// The argv names another session than the header does.
    ArgvDisagrees,
    /// Another session's file moved since the process started
    /// ([`Scan::moved`]): an in-TUI `/resume` onto it leaves no header.
    AnotherMoved,
}

impl NoWitness {
    /// One word for the step and the ledger.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            NoWitness::NoChats => "no-chats",
            NoWitness::NoHeader => "no-header",
            NoWitness::LateHeader => "late-header",
            NoWitness::TwoHeaders => "two-headers",
            NoWitness::StartedTogether => "started-together",
            NoWitness::NotTyped => "not-typed",
            NoWitness::ArgvDisagrees => "argv-disagrees",
            NoWitness::AnotherMoved => "another-moved",
        }
    }

    /// What aterm could not tell, in a person's words.
    #[must_use]
    pub fn said(self) -> &'static str {
        match self {
            NoWitness::NoChats => "its project's session files could not be read",
            NoWitness::NoHeader => "no session file of its project was begun after it started",
            NoWitness::LateHeader => {
                "the one session file of its project begun after it started was begun too late to \
                 be its own"
            }
            NoWitness::TwoHeaders => {
                "two sessions of its project were begun after it started (a /clear, or another \
                 Gemini CLI)"
            }
            NoWitness::StartedTogether => {
                "another Gemini CLI started in its project at the same moment"
            }
            NoWitness::NotTyped => "its session's id is no id aterm types back",
            NoWitness::ArgvDisagrees => "its launch named another session than its file does",
            NoWitness::AnotherMoved => {
                "another session of its project moved after it started (an in-TUI /resume)"
            }
        }
    }
}

/// How far apart two Gemini CLI starts in one project must be for the
/// start-time rule to tell their headers apart: the header lands 542-1108 ms
/// after the child starts and the child 284-458 ms after the parent
/// (measured), so a start within this of another may own either header.
pub const START_WINDOW_MS: u64 = 2_000;

/// How far before a process's real start a start read may name
/// (`upgrade_gemini_drive`'s start read): none where the kernel keeps its
/// microsecond birth (`proc_pidinfo`); elsewhere a read to the second, less
/// two seconds so it is never later than the real one.
pub const START_READ_SLACK_MS: u64 = if cfg!(target_vendor = "apple") {
    0
} else {
    2_000
};

/// The latest a process's own header may be begun after its start as read
/// ([`START_WINDOW_MS`], plus [`START_READ_SLACK_MS`]; measured at most
/// about 1.6 s): one begun later is another launch's
/// ([`NoWitness::LateHeader`]). A launch whose header falls within it started
/// within [`START_WINDOW_MS`] of this one, which the rule reads as started
/// together while it runs.
pub const LATE_HEADER_MS: u64 = START_WINDOW_MS + START_READ_SLACK_MS;

/// THE SESSION A GEMINI CLI PROCESS RUNS, BY THE START-TIME RULE: among the
/// session files `files` of its project (every file written since it
/// started; an older one can hold nothing of it), the ONE header stamped at
/// or after `start_ms` (the process's start, never later than it), and begun
/// within [`LATE_HEADER_MS`] of it unless it names the session the argv
/// does, names it — a full UUID, of the project
/// whose hash is `root_hash`, agreeing with the session the argv names
/// (`argv_session`), and no other session's file moved since
/// ([`Scan::moved`], `resumed` telling a session every switch onto which is
/// a `--resume` launch's: [`resumes_account_for_switches`] over its files).
/// `started_together`: another Gemini CLI of the project started within
/// [`START_WINDOW_MS`] of this one.
///
/// # Errors
/// [`NoWitness`]: anything else names no session, and nothing is resumed on
/// a guess.
pub fn witness(
    files: &[ChatsFile<'_>],
    start_ms: u64,
    root_hash: &str,
    argv_session: Option<&str>,
    started_together: bool,
    resumed: &dyn Fn(&str) -> bool,
) -> Result<String, NoWitness> {
    if started_together {
        return Err(NoWitness::StartedTogether);
    }
    let scans: Vec<(Scan, Option<u64>)> =
        files.iter().map(|f| (scan(f.text), f.mtime_ms)).collect();
    let since: Vec<&Header> = scans
        .iter()
        .flat_map(|(s, _)| &s.headers)
        .filter(|h| h.start_ms >= start_ms)
        .collect();
    let header = match since.as_slice() {
        [] => return Err(NoWitness::NoHeader),
        [one] => *one,
        _ => return Err(NoWitness::TwoHeaders),
    };
    // Its own header lands within the window of its start (measured): one
    // begun later is another launch's, its own never written — unless it
    // names the session its argv does, which is that session whoever began
    // it (a loaded machine's own header, late).
    if header.start_ms > start_ms.saturating_add(LATE_HEADER_MS)
        && argv_session != Some(header.session.as_str())
    {
        return Err(NoWitness::LateHeader);
    }
    if !is_session_uuid(&header.session) || header.project_hash != root_hash {
        return Err(NoWitness::NotTyped);
    }
    if argv_session.is_some_and(|s| s != header.session) {
        return Err(NoWitness::ArgvDisagrees);
    }
    let moved = scans.iter().any(|(s, mtime)| {
        s.session.as_deref() != Some(header.session.as_str()) && s.moved(start_ms, *mtime, resumed)
    });
    if moved {
        return Err(NoWitness::AnotherMoved);
    }
    Ok(header.session.clone())
}

// ------------------------------------------------------------- the pre-check

/// What `gemini --resume <id>` would load, read before it is typed
/// ([`precheck`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Precheck {
    /// The file it would load holds every turn of the session: typed.
    Safe {
        /// The file it would load.
        pick: String,
        /// How many turns it holds.
        turns: usize,
    },
    /// The session took no turn in any of its files: nothing to resume.
    NoTurns,
    /// No file of the session is one the CLI would load: the resume would
    /// exit 42.
    NoFile,
    /// The file it would load lacks turns another file of the session holds
    /// (an empty duplicate, or a history split across files): typed, the
    /// session would come back without them, its id unchanged.
    Unsafe {
        /// The file it would load.
        pick: String,
        /// How many turns only other files hold.
        missing: usize,
    },
    /// A file of the session cannot be folded (a legacy `.json`): what the
    /// resume would load is unknown.
    Unreadable(String),
}

impl Precheck {
    /// One word for the step and the ledger.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Precheck::Safe { .. } => "safe",
            Precheck::NoTurns => "no-turns",
            Precheck::NoFile => "no-file",
            Precheck::Unsafe { .. } => "unsafe",
            Precheck::Unreadable(_) => "unreadable",
        }
    }
}

/// THE READ-ONLY PRE-CHECK OF A RESUME of `session` (the premeasure's,
/// which predicted every live outcome): the session's files `files` (every
/// `session-*-<id8>` of the project, [`names_session`]) folded as the CLI
/// folds them ([`fold`]), those of this session that hold a user's or a
/// model's message; the one with the strictly newest `lastUpdated` is what
/// it loads (a tie: the earlier name, the CLI's sorted order). SAFE only
/// where that pick holds every turn any of them holds.
#[must_use]
pub fn precheck(files: &[ChatsFile<'_>], session: &str) -> Precheck {
    let mut sorted: Vec<&ChatsFile<'_>> = files
        .iter()
        .filter(|f| names_session(f.name, session))
        .collect();
    sorted.sort_by(|a, b| a.name.cmp(b.name));
    if let Some(legacy) = sorted.iter().find(|f| !f.name.ends_with(".jsonl")) {
        return Precheck::Unreadable(legacy.name.to_string());
    }
    let folds: Vec<(&str, Fold)> = sorted
        .iter()
        .map(|f| (f.name, fold(f.text)))
        .filter(|(_, fold)| fold.session.as_deref() == Some(session))
        .collect();
    let mut pick: Option<&(&str, Fold)> = None;
    for f in folds.iter().filter(|(_, fold)| fold.valid) {
        let newer = match pick {
            None => true,
            Some((_, best)) => {
                f.1.last_updated.as_deref().unwrap_or("")
                    > best.last_updated.as_deref().unwrap_or("")
            }
        };
        if newer {
            pick = Some(f);
        }
    }
    let Some((name, best)) = pick else {
        return Precheck::NoFile;
    };
    let all: std::collections::BTreeSet<&str> = folds
        .iter()
        .flat_map(|(_, f)| f.turns.iter().map(String::as_str))
        .collect();
    if all.is_empty() {
        return Precheck::NoTurns;
    }
    let held: std::collections::BTreeSet<&str> = best.turns.iter().map(String::as_str).collect();
    let missing = all.difference(&held).count();
    if missing == 0 {
        Precheck::Safe {
            pick: (*name).to_string(),
            turns: held.len(),
        }
    } else {
        Precheck::Unsafe {
            pick: (*name).to_string(),
            missing,
        }
    }
}

// ------------------------------------------------------------- the exit

/// How a Gemini CLI process ended, by its shell's exit status
/// ([`exit_kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// `0`, `130`: its own exit (`/quit`, `/exit`, Ctrl-C or Ctrl-D twice), a
    /// SIGTERM to its child, a cancel: never relaunched.
    Clean,
    /// A stop signal's status (Ctrl-Z where its tty sends one, SIGSTOP):
    /// stopped, alive — never a crash.
    Stopped,
    /// `41`, `42`, `44`, `52`, `53`, `54`, `55`: a start the CLI refused (no
    /// credential, an unknown or another project's session, a duplicate id,
    /// its sandbox, its config; print mode's): never retried.
    Refused,
    /// `1` (its child killed: the parent relays `code ?? 1`; an uncaught
    /// error) or `128` plus a signal nothing catches (`137` its parent
    /// SIGKILLed, measured; a fault): relaunched on its session.
    Crashed,
    /// Any other status (the parent ignores SIGTERM, SIGHUP and SIGINT, so
    /// `143` and `129` name nothing measured): an end no witness tells,
    /// nothing relaunched on a guess.
    Unwitnessed,
    /// `42` where ITS OWN RESTART MET THE ID ITS FIRST START WROTE
    /// ([`restart_id`], the start-time rule naming that id for the process):
    /// Gemini CLI's own restart (`/settings`, IDE trust, admin settings, the
    /// folder-trust dialog's `Trust folder`: exit `199`) runs the CLI again
    /// with the same argv, and a worker's `--session-id <uuid>`
    /// (`harness::worker`) is then a session already written (`Session ID …
    /// already exists`, exit 42, which the parent relays; MEASURED on 0.46.0
    /// in the lab, with and without a first turn: the header is written at
    /// the UI's mount, before any dialog is answered). Never [`exit_kind`]'s:
    /// the status alone is a refusal. Resumed on that session by id, nothing
    /// typed: a person's restart, never a crash. The parent's argv and its
    /// child's are the same on every run (the bundle's relaunch spawns the
    /// child with the parent's own arguments), so no argv tells a rerun from
    /// a first run: the session file begun since the start is the witness.
    Restarted,
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
            ExitKind::Restarted => "restarted",
        }
    }
}

/// The signals no process catches or survives, whose death `128 +` the
/// signal is a crash: SIGKILL (measured on the parent: `137`) and the
/// faults.
#[cfg(unix)]
const CRASH_SIGNALS: [libc::c_int; 6] = [
    libc::SIGKILL,
    libc::SIGSEGV,
    libc::SIGABRT,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
];
#[cfg(not(unix))]
const CRASH_SIGNALS: [i32; 0] = [];

/// GEMINI CLI'S EXIT TABLE, measured on 0.46.0 (the block's exit, which
/// equals the typed `$?`, fresh and resumed — ED 3J no longer erases it):
/// `0` and `130` [`ExitKind::Clean`]; a stop signal's status
/// ([`super::upgrade_agy::stopped_status`]) [`ExitKind::Stopped`]; `41`,
/// `42`, `44`, `52`-`55` [`ExitKind::Refused`]; `1` and `128` plus a
/// [`CRASH_SIGNALS`] member [`ExitKind::Crashed`]; any other
/// [`ExitKind::Unwitnessed`].
#[must_use]
pub fn exit_kind(status: i64) -> ExitKind {
    match status {
        0 | 130 => ExitKind::Clean,
        41 | 42 | 44 | 52..=55 => ExitKind::Refused,
        1 => ExitKind::Crashed,
        status if super::upgrade_agy::stopped_status(status) => ExitKind::Stopped,
        status
            if CRASH_SIGNALS
                .iter()
                .any(|&signal| status == 128 + i64::from(signal)) =>
        {
            ExitKind::Crashed
        }
        _ => ExitKind::Unwitnessed,
    }
}

/// THE WORD OF A RELAUNCH LINE'S OWN COMMAND, once its block ended with no
/// relaunched Gemini CLI found leading the tab: its table ([`exit_kind`]),
/// and `1` — an unknown flag the CLI rejects before any TUI (yargs, exit 1:
/// measured), or a `cd` the line could not make — `126` and `127` — the
/// shell could not run `gemini` — a refusal too: typed again, the line meets
/// the same wall.
#[must_use]
pub fn line_exit_kind(status: i64) -> ExitKind {
    match status {
        1 | 126 | 127 => ExitKind::Refused,
        status => exit_kind(status),
    }
}

/// THE SESSION ID A RESTART OF `argv` REFUSED, where `status` may be one:
/// [`FATAL_INPUT_ERROR`] from a launch that starts a new session under its
/// own id (`--session-id <uuid>`: a worker's, `harness::worker`), naming no
/// session to resume (`--resume`, `-r`) or import (`--session-file`) — the
/// one launch whose own restart is refused for the session its first start
/// wrote. The caller holds the id to the start-time rule ([`witness`]): the
/// session begun since the process started is that very id only where its
/// first start ran and a later run of the same argv met it
/// ([`ExitKind::Restarted`]); a launch whose id was written before it
/// started was refused at its first start, as any other `42`. `None` for
/// any other status or launch.
#[must_use]
pub fn restart_id(status: i64, argv: &[String]) -> Option<&str> {
    let names_another = argv
        .iter()
        .skip(2)
        .take_while(|t| t.as_str() != "--")
        .filter_map(|t| flag_token(t))
        .any(|(name, _)| [FLAG_RESUME, "r", FLAG_SESSION_FILE].contains(&name));
    if status != FATAL_INPUT_ERROR || names_another {
        return None;
    }
    flag_value(argv, &[FLAG_SESSION_ID]).filter(|id| is_session_uuid(id))
}

// ------------------------------------------------------------- the line

/// THE ENVIRONMENT A GEMINI CLI RELAUNCH LINE MUST CARRY: its
/// `GEMINI_CLI_HOME` where set (it moves the whole home, its sessions with
/// it), and its `HOME` where that is absolute and other than `user_home`
/// (the user's own, the shell's the line is typed at): either way its
/// sessions are under another `.gemini`, which a resume under the shell's
/// would not find (`Invalid session identifier`, exit 42) — and its
/// trusted-folders file ([`TRUSTED_FOLDERS_ENV`]) where an absolute one is
/// set: a worker's, without which its relaunch would stop at a folder-trust
/// dialog its launch never drew. `get` names a variable of its exec
/// environment. The other lanes carry the `HOME` of an agent under another
/// one the same way. A record's homes keep no trusted-folders file
/// ([`env_of_words`]): the file is the live worker's, removed with its
/// session, so a relaunch carries it only while it stands
/// ([`trusted_folders_of_words`], the lane's `plan`).
#[must_use]
pub fn relaunch_env<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
    user_home: &Path,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(h) = get(HOME_ENV).filter(|h| Path::new(h).has_root() && Path::new(h) != user_home)
    {
        out.push((HOME_ENV.to_string(), h.to_string()));
    }
    if let Some(h) = get(CLI_HOME_ENV).filter(|h| !h.is_empty()) {
        out.push((CLI_HOME_ENV.to_string(), h.to_string()));
    }
    if let Some(f) = get(TRUSTED_FOLDERS_ENV).filter(|f| Path::new(f).has_root()) {
        out.push((TRUSTED_FOLDERS_ENV.to_string(), f.to_string()));
    }
    out
}

/// [`relaunch_env`]'s pairs as the line holds them; REQUIRED, as the other
/// lanes' homes are: a line that cannot hold a value is refused rather than
/// typed without it.
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
        .filter(|(name, _)| *name == HOME_ENV || *name == CLI_HOME_ENV)
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// THE WORKER'S TRUSTED-FOLDERS FILE a record's words name
/// ([`env_words`] of [`relaunch_env`]'s pairs), where an absolute one is
/// named: what a relaunch carries while the file stands (the worker's own,
/// removed with its session), never a record's homes ([`env_of_words`]).
#[must_use]
pub fn trusted_folders_of_words(words: &[String]) -> Option<&str> {
    words
        .iter()
        .filter_map(|w| w.split_once('='))
        .find(|(name, _)| *name == TRUSTED_FOLDERS_ENV)
        .map(|(_, value)| value)
        .filter(|value| Path::new(value).has_root())
}

/// THE LINE A PERSON IS TOLD to resume a Gemini CLI session by hand: `cd`
/// to its project (a resume from another directory finds no session) and
/// Gemini CLI's own `gemini --resume <uuid>` (its farewell's hint), behind
/// the homes its relaunch carries, each word quoted as a shell reads it —
/// never a worker's own trusted-folders file ([`TRUSTED_FOLDERS_ENV`]),
/// which goes with its session: the person's own trust stands. `None` for a
/// session that is no full UUID, or a word no line can hold.
#[must_use]
pub fn told_line(session: &str, env: &[(String, String)], root: &str) -> Option<String> {
    if !is_session_uuid(session) {
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
    if root.chars().any(char::is_control) {
        return None;
    }
    let mut said = String::new();
    if names_one_folder(Path::new(root)) {
        said.push_str("cd ");
        said.push_str(&word(root));
        said.push_str(" && ");
    }
    for (name, value) in env.iter().filter(|(name, _)| name != TRUSTED_FOLDERS_ENV) {
        if value.chars().any(char::is_control) {
            return None;
        }
        said.push_str(name);
        said.push('=');
        said.push_str(&word(value));
        said.push(' ');
    }
    said.push_str(PROGRAM);
    said.push_str(&format!(" --{FLAG_RESUME} {session}"));
    Some(said)
}

/// Whether a `cd` to `path` lands in the one folder it names, whatever the
/// shell's working directory: rooted, and plain names only. On Windows a
/// rooted path may lead with a drive (`C:\w\p`) or not (`\w\p`, the current
/// drive's); a drive-relative `C:p` is no one folder.
fn names_one_folder(path: &Path) -> bool {
    path.has_root()
        && path.components().all(|c| match c {
            Component::RootDir | Component::Normal(_) => true,
            Component::Prefix(p) => matches!(p.kind(), std::path::Prefix::Disk(_)),
            Component::CurDir | Component::ParentDir => false,
        })
}

/// The version a Gemini CLI install's `package.json` names (`"version"`).
#[must_use]
pub fn package_version(json: &str) -> Option<String> {
    let v: aterm_json::Value = aterm_json::from_str(json).ok()?;
    if v.get("name").and_then(aterm_json::Value::as_str) != Some("@google/gemini-cli") {
        return None;
    }
    v.get("version")
        .and_then(aterm_json::Value::as_str)
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "upgrade_gemini_tests.rs"]
mod tests;

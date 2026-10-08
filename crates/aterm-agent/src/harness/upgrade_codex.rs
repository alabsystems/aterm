// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Live upgrade of a running Codex, the pure half. The Claude lane
//! ([`super::upgrade`]) restarts one process; Codex runs as two, and each moves
//! by its own vendor verb:
//!
//! * **The daemon.** A plain `codex` copies its package into
//!   `$CODEX_HOME/packages/app-server-daemon/releases/<version>-<triple>/` and
//!   starts a detached shared app server from it (`codex app-server --listen
//!   unix:// --managed-daemon`) plus the vendor's `pid-update-loop`, which
//!   re-runs the vendor installer outside atpkg while
//!   `packages/app-server-daemon/auto-update-version` exists. The daemon holds
//!   every conversation (thread writer locks and rollouts). `<managed codex>
//!   app-server daemon update --from-cli --yes` copies the managed build in,
//!   pins it (removing the updater marker) and restarts the daemon; attached
//!   TUIs reconnect. The restart kills background terminals running under the
//!   daemon, so [`daemon_step`] runs it only when every loaded thread is idle,
//!   no background terminal runs ([`terminals_under`]: unified-exec processes
//!   lead their own session), and every tab's word and attached Codex allow it
//!   ([`daemon_clients_in`]).
//! * **A daemon-mode client** (the TUI) holds nothing: `/exit` exits 0, restores
//!   the terminal and prints `codex resume <uuid>` under `To reconnect, run:`
//!   (0.157 printed `Reconnect: codex resume <uuid>`); the daemon keeps
//!   the thread and its background terminals. No wind-down or continuation is
//!   owed. Codex also carries an unobserved `The current turn was stopped.`
//!   disconnect message, so `/exit` is never typed while the client's own
//!   conversation runs a turn ([`daemon_turn`]): the thread the kernel names as
//!   its own ([`own_thread`]: every daemon thread in one root's spawn tree,
//!   [`Lineage`], and this TUI the only client), or a running root this tab's
//!   screen has not placed in another session ([`still_through`]).
//! * **An embedded session** (`--no-daemon`, or a launch Codex keeps out of the
//!   daemon) holds `$CODEX_HOME/thread-writer-locks/<uuid>.lock` itself (an
//!   exclusive flock, so the kernel proves which pid owns which thread), and
//!   its background terminals die with it, so the exit waits for them. It gets
//!   the Claude lane's cooperative protocol ([`prepare_prompt`] → a READY in
//!   the rollout ([`rollout_has_ready`]) → `/exit` → `codex resume` →
//!   [`continue_prompt`]) with `/exit` in place of SIGTERM: signals kill the
//!   TUI without restoring terminal modes, while `/exit` restores them. The
//!   Codex lane never sends a signal.
//!
//! **The goal pause.** A goal-mode Codex starts its next turn within
//! milliseconds of the last, so no turn end is a pause to move it in:
//! [`goal_step`] makes one by typing `/goal pause`, moving Codex, then `/goal
//! resume`; no running tool call is cut off. Per Codex's source:
//!
//! * `/goal` is available while a turn runs and is dispatched at once, never
//!   queued or steered into the turn.
//! * Pause stops the next turn, not the running one: it sets the goal's status
//!   (`thread/goal/set`), and the next goal turn starts only when the goal is
//!   `Active`.
//! * `/goal resume` sets it `Active`, continuing at once on an idle thread or
//!   after the running turn.
//! * `codex resume <thread>` over a paused goal with no prompt first shows
//!   `Resume paused goal?` (`Resume goal` focused, over `Leave paused`); the
//!   upgrade answers it with the resume (`aterm_phase::codex::resume_box`) or
//!   types `/goal resume` where no box came.
//! * Codex records each change in the rollout (`thread_goal_updated`) and draws
//!   it on the footer (`Goal paused (/goal resume)`, `Pursuing goal (…)`),
//!   which the upgrade reads.
//!
//! Esc is the fallback for a pause that never shows: pressed only at a goal
//! turn's head ([`TurnState::Head`]: nothing of the turn's own work in the
//! rollout yet), re-read just before the key, with the composer free and
//! nobody typing; it interrupts the turn and pauses the goal at once. The
//! model's first item arrives seconds after a goal turn starts, and the head
//! is read within a second of the key.
//!
//! The upgrade resumes the goal once: its hold is on the tab's one goal record
//! ([`super::goal_hold`]), written before every key and shared with the
//! supervisor's save-then-wait switch; an open switch keeps the pause off.
//!
//! Everything here is pure over facts the driver (`upgrade_drive`'s `codex`
//! module) measured: package version, flag table and argv rewrite, exit hint,
//! rollout readers, composer, prompts and the two gates. Codex's files are a
//! third party's, so every reader refuses a shape it was not written for.
//!
//! The daemon rule is a derived model
//! (`aterm_spec::derive::harness_codex_daemon_update_model`), bound to
//! [`daemon_step`] by this module's tests. The whole branch runs against a real
//! Codex in `tools/test-codex-live-upgrade.sh` (private headless aterm,
//! loopback fake model): both modes, an inline TUI at a two-row prompt, and
//! the SIGTERM negative control.
//!
//! The composer is read by the shared reader (`aterm_phase::codex`, both input
//! marks `›` and `»`); the exit hint is read off the TUI's own last rows.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aterm_json::Value;

use aterm_phase::codex::CodexGoal;

use super::goal_hold::Hold;
use super::upgrade::{ArgvRefusal, Facts, Gate, NOTICE_CLOSE, Phase, Request, Step, Version};

// ---------------------------------------------------------------- the package

/// The manifest every Codex package carries at its root (layoutVersion 1).
pub const PACKAGE_JSON: &str = "codex-package.json";

/// The version of the Codex `exe`, read from its package manifest, never by
/// running it (`codex --version` is unparseable and an exec can hang, holding
/// the sweep lock). The manifest must be the root whose `entrypoint` names
/// `exe` itself (`<root>/bin/codex`), so a stray parent manifest never answers.
/// `None` otherwise.
#[must_use]
pub fn package_version(exe: &Path) -> Option<Version> {
    let exe = std::fs::canonicalize(exe).ok()?;
    exe.ancestors().skip(1).take(2).find_map(|root| {
        let text = std::fs::read_to_string(root.join(PACKAGE_JSON)).ok()?;
        let (version, entry) = parse_package(&text)?;
        (std::fs::canonicalize(root.join(entry)).ok()? == exe).then_some(version)
    })
}

/// One `codex-package.json`: its version and entrypoint (relative to the root).
/// `None` unless `layoutVersion` is 1 and both are present.
#[must_use]
pub fn parse_package(text: &str) -> Option<(Version, PathBuf)> {
    let v: Value = aterm_json::from_str(text).ok()?;
    if v.get("layoutVersion").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let version = Version::parse(v.get("version")?.as_str()?)?;
    let entry = v.get("entrypoint")?.as_str()?;
    let entry = Path::new(entry);
    if entry.is_absolute() || entry.components().any(|c| c.as_os_str() == "..") {
        return None;
    }
    Some((version, entry.to_path_buf()))
}

// ---------------------------------------------------------------- the flags

/// How a Codex flag takes its value (clap, `codex --help` 0.157.1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    /// A switch.
    None,
    /// Exactly one value: the next token whatever it looks like, or attached
    /// (`--model=x`, `-mx`).
    One,
    /// `<FILE>...`: one value or more, up to the next token that starts with
    /// `-`.
    Many,
}

/// What the rewrite does with a flag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Carried verbatim.
    Keep,
    /// Dropped with its value.
    Drop,
    /// The relaunch is refused.
    Refuse,
}

/// Codex's interactive options (`codex --help`, `codex resume --help`). A flag
/// missing here refuses the rewrite; new flags are added once known safe. The
/// flags that keep a session out of the daemon (`-c`, `--profile`, `--oss`,
/// `--strict-config`, `--no-daemon`, the hook-trust bypass) are kept, so the
/// relaunch lands in the mode it left.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("-a", Arity::One, Fate::Keep),
    ("--add-dir", Arity::One, Fate::Keep),
    ("--all", Arity::None, Fate::Drop),
    ("--approve-for-me", Arity::None, Fate::Keep),
    ("--ask-for-approval", Arity::One, Fate::Keep),
    ("-C", Arity::One, Fate::Keep),
    ("-c", Arity::One, Fate::Keep),
    ("--cd", Arity::One, Fate::Keep),
    ("--config", Arity::One, Fate::Keep),
    (
        "--dangerously-bypass-approvals-and-sandbox",
        Arity::None,
        Fate::Keep,
    ),
    ("--dangerously-bypass-hook-trust", Arity::None, Fate::Keep),
    ("--disable", Arity::One, Fate::Keep),
    ("--enable", Arity::One, Fate::Keep),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    // Attached to the first prompt, which the conversation already holds.
    ("-i", Arity::Many, Fate::Drop),
    ("--image", Arity::Many, Fate::Drop),
    ("--include-non-interactive", Arity::None, Fate::Drop),
    ("--last", Arity::None, Fate::Drop),
    ("--local-provider", Arity::One, Fate::Keep),
    // The model is carried, never moved: aterm reads no Codex model catalog,
    // and what `-m` on `codex resume` does to a daemon-held thread is
    // unknown. A launch's model is kept verbatim.
    ("-m", Arity::One, Fate::Keep),
    ("--model", Arity::One, Fate::Keep),
    ("--no-alt-screen", Arity::None, Fate::Keep),
    ("--no-daemon", Arity::None, Fate::Keep),
    ("--oss", Arity::None, Fate::Keep),
    ("-p", Arity::One, Fate::Keep),
    ("--profile", Arity::One, Fate::Keep),
    // A TUI attached to another app server: not this machine's daemon, and
    // not a session this sweep can see the thread of.
    ("--remote", Arity::One, Fate::Refuse),
    ("--remote-auth-token-env", Arity::One, Fate::Refuse),
    ("-s", Arity::One, Fate::Keep),
    ("--sandbox", Arity::One, Fate::Keep),
    ("--search", Arity::None, Fate::Keep),
    ("--strict-config", Arity::None, Fate::Keep),
    ("-V", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    // A session in a managed worktree Codex made for it: a resume in the tab
    // would not land there.
    ("--worktree", Arity::None, Fate::Refuse),
];

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Codex's subcommands. Only none, `resume` and `fork` run the interactive TUI
/// a relaunch resumes; the rest are refused ([`ArgvRefusal::NotResumable`]).
const SUBCOMMANDS: &[&str] = &[
    "agents",
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "plugin",
    "app-server",
    "remote-control",
    "app",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "resume",
    "queue",
    "archive",
    "delete",
    "migrate-rollouts",
    "unarchive",
    "fork",
    "cloud",
    "exec-server",
    "features",
    "help",
];

/// Whether clap reads `tok` as an option: a leading `-` and more after it (a
/// lone `-` is a value).
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// The argv rewrite for Codex: the TUI's argv (argv[0] dropped) becomes the
/// words after the executable on the relaunch line: `resume <kept flags
/// verbatim> <thread>`, or with no thread (no conversation yet) the kept flags
/// alone. The launch's subcommand, session id, first prompt and images are
/// dropped: the conversation holds them, and a `fork` has already forked.
///
/// # Errors
/// An unknown flag or subcommand ([`ArgvRefusal::UnknownFlag`]), or a session
/// not resumable in place ([`ArgvRefusal::NotResumable`]: `exec`, `--remote`,
/// `--worktree`, …).
pub fn rewrite_argv(argv: &[String], thread: Option<&str>) -> Result<Vec<String>, ArgvRefusal> {
    let mut kept: Vec<String> = Vec::new();
    let mut subcommand: Option<&str> = None;
    let mut i = 1;
    let mut positional_only = false;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" && !positional_only {
            positional_only = true;
            continue;
        }
        if positional_only || !maybe_option(tok) {
            // The first positional is the subcommand when it names one, else
            // the prompt (dropped, as is every positional after it).
            if subcommand.is_none() && !positional_only {
                if SUBCOMMANDS.contains(&tok) {
                    if tok != "resume" && tok != "fork" {
                        return Err(ArgvRefusal::NotResumable(tok.to_string()));
                    }
                    subcommand = Some(tok);
                    continue;
                }
                subcommand = Some("");
            }
            continue;
        }
        // `--name=value`, `-Xvalue` (a short flag with its value attached), or
        // the flag alone.
        let (name, attached) = if let Some((n, _)) = tok.split_once('=')
            && n.starts_with("--")
        {
            (n.to_string(), true)
        } else if !tok.starts_with("--") && tok.chars().count() > 2 {
            (tok.chars().take(2).collect::<String>(), true)
        } else {
            (tok.to_string(), false)
        };
        let (arity, fate) = flag(&name).ok_or_else(|| ArgvRefusal::UnknownFlag(name.clone()))?;
        if fate == Fate::Refuse {
            return Err(ArgvRefusal::NotResumable(name));
        }
        if attached && arity == Arity::None {
            // `--no-daemon=x`, `-hV`: a shape clap refuses; so does this.
            return Err(ArgvRefusal::UnknownFlag(tok.to_string()));
        }
        let start = i - 1;
        if !attached {
            match arity {
                Arity::None => {}
                Arity::One => {
                    if i < argv.len() {
                        i += 1;
                    }
                }
                Arity::Many => {
                    if i < argv.len() {
                        i += 1;
                    }
                    while i < argv.len() && !maybe_option(&argv[i]) && argv[i] != "--" {
                        i += 1;
                    }
                }
            }
        }
        if fate == Fate::Keep {
            kept.extend(argv[start..i].iter().cloned());
        }
    }
    Ok(match thread {
        Some(t) => std::iter::once("resume".to_string())
            .chain(kept)
            .chain(std::iter::once(t.to_string()))
            .collect(),
        None => kept,
    })
}

/// Whether `argv` is an interactive Codex TUI's (no subcommand, `resume` or
/// `fork`), not the daemon, its updater, `exec` or another face of the binary.
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            return true;
        }
        if !maybe_option(tok) {
            return !SUBCOMMANDS.contains(&tok) || tok == "resume" || tok == "fork";
        }
        let attached = tok.contains('=') || (!tok.starts_with("--") && tok.chars().count() > 2);
        if !attached {
            let name = tok;
            if matches!(flag(name), Some((Arity::One | Arity::Many, _))) {
                i += 1;
            }
        }
    }
    true
}

/// Whether `s` is a thread id Codex prints and takes back: a hyphenated UUID
/// (8-4-4-4-12 hex digits). Nothing else is ever put on a relaunch line.
#[must_use]
pub fn is_thread_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| aterm_codec::hex::is_hex(p, n))
}

/// The thread a TUI's argv resumes (`codex [flags] resume [flags] <id>`): the
/// first positional after `resume` when it is a thread id ([`is_thread_id`]).
/// `None` for a plain launch, `resume --last`, a picker, or a `fork`.
#[must_use]
pub fn resumed_thread(argv: &[String]) -> Option<String> {
    let mut i = 1;
    let mut resuming = false;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            if !resuming {
                if tok != "resume" {
                    return None;
                }
                resuming = true;
                continue;
            }
            return is_thread_id(tok).then(|| tok.to_string());
        }
        let attached = tok.contains('=') || (!tok.starts_with("--") && tok.chars().count() > 2);
        if !attached && matches!(flag(tok), Some((Arity::One | Arity::Many, _))) {
            i += 1;
        }
    }
    None
}

/// When a thread was created, in unix milliseconds, from its UUIDv7 id's first
/// 48 bits. `None` for any id that is not version-7.
#[must_use]
pub fn thread_created_ms(thread: &str) -> Option<u64> {
    if !is_thread_id(thread) || thread.as_bytes().get(14) != Some(&b'7') {
        return None;
    }
    let hex: String = thread.split('-').take(2).collect();
    u64::from_str_radix(&hex, 16).ok()
}

/// The working directory a rollout's conversation began in: its first line's
/// `session_meta` `payload.cwd`. `None` for any other first line.
#[must_use]
pub fn rollout_cwd(first_line: &str) -> Option<String> {
    let v: Value = aterm_json::from_str(first_line.trim()).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    v.get("payload")?.get("cwd")?.as_str().map(str::to_string)
}

/// Whether a Codex TUI's exit status (the shell integration's block `exit`) was
/// a crash: a signal nobody sends to end a program on purpose (SIGKILL's 137,
/// as an OOM kill uses; SIGSEGV, SIGABRT, SIGBUS) or a failure status. `0` is
/// its orderly exit, and SIGHUP, SIGINT and SIGTERM (129, 130, 143) are
/// someone's signal, matching Claude Code, which removes its session record on
/// those and never on SIGKILL.
#[must_use]
pub fn crashed_status(exit: i64) -> bool {
    !matches!(exit, 0 | 129 | 130 | 143)
}

// ---------------------------------------------------------------- the exit

/// What a Codex TUI printed as it left: the thread it held and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitHint {
    /// The thread `codex resume` takes back.
    pub thread: String,
    /// `true` for a daemon-mode client (`To reconnect, run:` / `  codex resume
    /// <id>`, or 0.157's one-line `Reconnect: codex resume <id>`), `false` for
    /// an embedded session (`To continue this session, run:` / `  codex resume
    /// <id>`).
    pub daemon: bool,
}

/// The daemon-mode hint's line prefix as 0.157 printed it. 0.159 and 0.160 print
/// the embedded shape instead, under [`RECONNECT_HEADING`] (measured in both
/// store builds' strings, 2026-10-01: neither carries this line).
const RECONNECT: &str = "Reconnect: codex resume ";

/// The heading over a daemon-mode client's `codex resume <id>` row (0.159+).
const RECONNECT_HEADING: &str = "To reconnect";

/// The heading over an embedded session's `codex resume <id>` row.
const CONTINUE_HEADING: &str = "To continue this session";

/// The embedded line's words (the row under `To continue this session,
/// run:`).
const CONTINUE: &str = "codex resume ";

/// The daemon-mode hint's last line, under its `codex resume` one.
const STOP_HINT: &str = "Stop the current turn:";

/// The token tally a TUI prints on leaving after a turn: under the daemon-mode
/// hint (`Token usage so far: …`) or above the embedded one (`Token usage: …`).
const TOKEN_USAGE: &str = "Token usage";

/// The last exit hint in `rows`: `codex resume <id>` under `To reconnect, run:`
/// (daemon mode; 0.157 printed `Reconnect: codex resume <id>`) or under `To
/// continue this session, run:` (embedded), the id
/// checked ([`is_thread_id`]); a row wrapped by a narrow pane is joined with the
/// next. `rows` must be the TUI's own output (its command block, or
/// [`hint_above_prompt`]'s rows), never the whole screen, which may hold an
/// older exit's hint.
#[must_use]
pub fn parse_exit_hint(rows: &[String]) -> Option<ExitHint> {
    let mut found = None;
    for (i, row) in rows.iter().enumerate() {
        let joined = rows
            .get(i + 1)
            .map(|next| format!("{}{}", row.trim_end(), next.trim()));
        for text in std::iter::once(row.trim().to_string()).chain(joined) {
            if let Some(hint) = hint_in(&text, rows.get(i.wrapping_sub(1)).map(String::as_str)) {
                found = Some(hint);
                break;
            }
        }
    }
    found
}

/// One row's hint, `above` the row over it (the heading that says which mode).
fn hint_in(text: &str, above: Option<&str>) -> Option<ExitHint> {
    let t = text.trim();
    if let Some(id) = t.strip_prefix(RECONNECT) {
        let id = id.trim();
        return is_thread_id(id).then(|| ExitHint {
            thread: id.to_string(),
            daemon: true,
        });
    }
    let id = t.strip_prefix(CONTINUE)?.trim();
    let heading = above.map(str::trim)?;
    let daemon = if heading.starts_with(RECONNECT_HEADING) {
        true
    } else if heading.starts_with(CONTINUE_HEADING) {
        false
    } else {
        return None;
    };
    is_thread_id(id).then(|| ExitHint {
        thread: id.to_string(),
        daemon,
    })
}

/// The rows a TUI printed as it left, when shell integration gives no command
/// output: the hint must sit directly above `prompt_row`, the first row of the
/// next prompt, with only its own trailing lines ([`STOP_HINT`], [`TOKEN_USAGE`])
/// and blanks between, so a TUI that printed nothing reads no hint. The caller
/// knows the prompt's first row: the cursor row less the rows a multi-row
/// prompt draws above it (the entering block's `cmd - prompt`).
#[must_use]
pub fn hint_above_prompt(rows: &[String], prompt_row: usize) -> Option<ExitHint> {
    let start = prompt_row.saturating_sub(8);
    let window = rows.get(start..prompt_row)?;
    let hint = parse_exit_hint(window)?;
    // Where it sits: the last row carrying the id, and nothing but the
    // TUI's own trailing lines (or blank rows) after it.
    let at = window.iter().rposition(|r| r.contains(&hint.thread))?;
    window[at + 1..]
        .iter()
        .all(|r| {
            let t = r.trim_start();
            t.trim().is_empty() || t.starts_with(STOP_HINT) || t.starts_with(TOKEN_USAGE)
        })
        .then_some(hint)
}

// ---------------------------------------------------------------- the rollout

/// Where a thread's last turn stands, by its rollout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnState {
    /// The last turn event is `task_complete` or `turn_aborted`.
    Idle,
    /// The last turn event is `task_started`: a turn is running.
    Busy,
    /// The turn's head: the last turn event is `task_started` and nothing of the
    /// turn's own work follows it, only what it opened with ([`head_part`]).
    /// Read only by [`rollout_head_back`]; [`rollout_turn_back`] reads it
    /// [`TurnState::Busy`]. A goal turn's first work lands seconds after it
    /// opens; only here may an Esc stop a turn without cutting anything off
    /// ([`goal_step`]).
    Head,
    /// No turn event as far back as read ([`rollout_turn_back`]): an older turn
    /// or an unknown shape, never idle.
    Unknown,
}

/// Whether one rollout line is part of a turn's head ([`TurnState::Head`]): its
/// `turn_context`, `world_state`, a `user` or `developer` `message`, an
/// `event_msg` that is a `user_message`, `token_count` or
/// `thread_goal_updated`, or a `token_usage_record`. Anything else (reasoning,
/// assistant messages, calls and outputs, `item_completed`, non-JSON or unknown
/// shapes) is the turn's work.
#[must_use]
pub fn head_part(line: &str) -> bool {
    let Ok(v) = super::transcript::codex_metadata(line.trim()) else {
        return false;
    };
    let payload = |k: &str| {
        v.get("payload")
            .and_then(|p| p.get(k))
            .and_then(Value::as_str)
    };
    match v.get("type").and_then(Value::as_str) {
        // `world_state`: the environment state a turn opens with, written
        // between its opening messages and `turn_context` (and after a
        // `compacted`).
        Some("turn_context" | "token_usage_record" | "world_state") => true,
        Some("response_item") => {
            payload("type") == Some("message")
                && matches!(payload("role"), Some("user" | "developer"))
        }
        Some("event_msg") => matches!(
            payload("type"),
            Some("user_message" | "token_count" | "thread_goal_updated")
        ),
        _ => false,
    }
}

/// The thread's turn from `text`, newest line first, with its head told apart:
/// the last turn event's state ([`turn_event`]), a running turn with no work
/// yet read [`TurnState::Head`] ([`head_part`]); `None` when `text` holds none.
/// Blank lines are passed; any other non-head line, or a cut line, is work, so a
/// head is read only when every line after `task_started` was read whole.
/// `worked` carries across the chunks of one read ([`rollout_head_back`]).
fn scan_head(text: &str, worked: &mut bool) -> Option<TurnState> {
    for line in text.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(turn) = turn_event(line) {
            return Some(match turn {
                TurnState::Busy if !*worked => TurnState::Head,
                other => other,
            });
        }
        if !*worked && !head_part(line) {
            *worked = true;
        }
    }
    None
}

/// The payload types that start and end a turn — the words a rollout line
/// must hold before it is parsed as one ([`turn_event`]).
const TURN_EVENTS: [&str; 3] = ["task_started", "task_complete", "turn_aborted"];

/// The payload of one rollout line, when it is an `event_msg`.
fn event_payload(line: &str) -> Option<Value> {
    if !line.contains("event_msg") {
        return None;
    }
    let mut v = super::transcript::codex_metadata(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    v.as_object_mut()?.remove("payload")
}

/// The turn event one rollout line is, if any: an `event_msg` whose payload
/// starts a turn (`task_started`, busy) or ends one (`task_complete` /
/// `turn_aborted`, idle). Non-JSON (a cut or half-written line) is none. Parsed
/// only when it holds one of those words ([`TURN_EVENTS`]), so other long lines
/// cost a byte search.
fn turn_event(line: &str) -> Option<TurnState> {
    if !TURN_EVENTS.iter().any(|w| line.contains(w)) {
        return None;
    }
    match event_payload(line)?.get("type").and_then(Value::as_str) {
        Some("task_started") => Some(TurnState::Busy),
        Some("task_complete" | "turn_aborted") => Some(TurnState::Idle),
        _ => None,
    }
}

/// A rollout's last turn event, read back from its end `chunk` bytes at a time
/// and no further than `bound` bytes (`len` bytes of `file`), since a tail
/// alone may hold no turn event while the thread idles (a single command
/// output line can be hundreds of KiB).
///
/// Each chunk ends where the previous began, and the line a chunk's start
/// cuts is carried into the next, whole ([`TurnBack`]), so a turn event is
/// read wherever chunks cut it. A line is parsed only when it holds a turn
/// event's word ([`turn_event`]). A line the bound cuts is never read; past
/// the bound, or for an unreadable file, the state is [`TurnState::Unknown`]
/// (counted running).
#[must_use]
pub fn rollout_turn_back<R: Read + Seek>(
    file: &mut R,
    len: u64,
    chunk: u64,
    bound: u64,
) -> TurnState {
    read_back(file, len, chunk, bound, |text| {
        text.lines().rev().find_map(turn_event)
    })
    .unwrap_or(TurnState::Unknown)
}

/// [`rollout_turn_back`] with the turn's head told apart: a running turn whose
/// lines since `task_started` hold none of its own work reads
/// [`TurnState::Head`], the one point the goal pause may press Esc
/// ([`goal_step`]). Same chunks, bound and carry; a cut or partial line is no
/// head.
#[must_use]
pub fn rollout_head_back<R: Read + Seek>(
    file: &mut R,
    len: u64,
    chunk: u64,
    bound: u64,
) -> TurnState {
    let mut worked = false;
    read_back(file, len, chunk, bound, |text| scan_head(text, &mut worked))
        .unwrap_or(TurnState::Unknown)
}

/// The read-back the turn and context readers share: complete lines are fed
/// to `scan` newest chunk first, no further than `bound` from the end. Each
/// carried chunk is moved and joined once, including a multi-MiB tool row.
/// `scan` walks its lines newest first and returns the first relevant fact.
pub(crate) fn read_back<R: Read + Seek, T>(
    file: &mut R,
    len: u64,
    chunk: u64,
    bound: u64,
    mut scan: impl FnMut(&str) -> Option<T>,
) -> Option<T> {
    let mut back = TurnBack::default();
    let floor = len.saturating_sub(bound);
    let mut end = len;
    while end > floor {
        let start = end.saturating_sub(chunk.max(1)).max(floor);
        let n = usize::try_from(end - start).ok()?;
        let mut bytes = vec![0; n];
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut bytes).is_err() {
            return None;
        }
        if let Some(found) = back.feed(bytes, start == 0, &mut scan) {
            return Some(found);
        }
        end = start;
    }
    None
}

/// [`rollout_turn_back`]'s carry between chunks: the one line not yet whole,
/// kept as the pieces read (newest read, earliest in the file, last). Pieces are
/// moved, never copied, and joined once when the chunk holding the line's start
/// arrives, so a line longer than a chunk costs its length, not its length
/// squared over the chunk.
#[derive(Debug, Default)]
struct TurnBack {
    pieces: Vec<Vec<u8>>,
}

impl TurnBack {
    /// Feeds `chunk`, the bytes ending where the previous chunk began (the
    /// first: the file's end); `from_start` when it begins the file. Returns
    /// the first fact `scan` finds in its completed lines, or `None`.
    fn feed<T>(
        &mut self,
        chunk: Vec<u8>,
        from_start: bool,
        scan: &mut impl FnMut(&str) -> Option<T>,
    ) -> Option<T> {
        // Whole lines begin past the line this chunk's start cuts, unless it
        // starts the file. A chunk with no newline is all one line's middle.
        let whole = if from_start {
            0
        } else if let Some(nl) = chunk.iter().position(|&b| b == b'\n') {
            nl + 1
        } else {
            self.pieces.push(chunk);
            return None;
        };
        // The lines completed: this chunk's from `whole`, then the carry in
        // file order.
        let carried: usize = self.pieces.iter().map(Vec::len).sum();
        let mut lines = Vec::with_capacity(chunk.len() - whole + carried);
        lines.extend_from_slice(&chunk[whole..]);
        for piece in self.pieces.drain(..).rev() {
            lines.extend_from_slice(&piece);
        }
        let text = String::from_utf8_lossy(&lines);
        let found = scan(&text);
        let mut cut = chunk;
        cut.truncate(whole);
        self.pieces.push(cut);
        found
    }

    /// The bytes carried: the line not yet whole (tests: the carry is moved
    /// in, never grown by a copy).
    #[cfg(test)]
    fn carried(&self) -> usize {
        self.pieces.iter().map(Vec::len).sum()
    }
}

/// The text of a user message on one rollout line: a `response_item` `message`
/// of role `user` (its `input_text` parts) or an `event_msg` `user_message`.
/// Codex's own `developer` rows and `<environment_context>` user rows are
/// skipped.
fn user_text(line: &str) -> Option<String> {
    if !line.contains("user") {
        return None;
    }
    let v: Value = aterm_json::from_str(line).ok()?;
    let p = v.get("payload")?;
    let text = match (v.get("type")?.as_str()?, p.get("type")?.as_str()?) {
        ("response_item", "message") if p.get("role")?.as_str()? == "user" => parts_text(p)?,
        ("event_msg", "user_message") => p.get("message")?.as_str()?.to_string(),
        _ => return None,
    };
    (!text.trim_start().starts_with("<environment_context>")).then_some(text)
}

/// The assistant's words on one rollout line: a `response_item` `message` of
/// role `assistant` (its `output_text` parts), an `event_msg` `agent_message`,
/// or a `task_complete`'s `last_agent_message`.
fn assistant_text(line: &str) -> Option<String> {
    if !line.contains("assistant") && !line.contains("agent_message") {
        return None;
    }
    let v: Value = aterm_json::from_str(line).ok()?;
    let p = v.get("payload")?;
    match (v.get("type")?.as_str()?, p.get("type")?.as_str()?) {
        ("response_item", "message") if p.get("role")?.as_str()? == "assistant" => parts_text(p),
        ("event_msg", "agent_message") => p.get("message")?.as_str().map(str::to_string),
        ("event_msg", "task_complete") => p.get("last_agent_message")?.as_str().map(str::to_string),
        _ => None,
    }
}

/// A message payload's text parts, joined by newlines.
fn parts_text(p: &Value) -> Option<String> {
    let parts = p.get("content")?.as_array()?;
    let texts: Vec<&str> = parts
        .iter()
        .filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("input_text" | "output_text" | "text")
            )
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// Whether the agent's last words are the READY answer to the latest notice:
/// the Claude lane's rule ([`super::upgrade::transcript_has_ready`]) over a
/// Codex rollout. A user message that is a notice ([`ANNOUNCE_HEAD`]) ends any
/// READY before it; each later assistant message decides afresh (a line,
/// trimmed of spaces and backticks, equal to `marker`).
#[must_use]
pub fn rollout_has_ready(tail: &str, marker: &str) -> bool {
    let mut ready = false;
    for line in tail.lines() {
        if let Some(text) = user_text(line) {
            if text.trim_start().starts_with(ANNOUNCE_HEAD) {
                ready = false;
            }
            continue;
        }
        if let Some(text) = assistant_text(line) {
            ready = text.lines().any(|l| l.trim().trim_matches('`') == marker);
        }
    }
    ready
}

// ---------------------------------------------------------------- the screen

// The composer is read by the shared reader `aterm_phase::codex` (`composer`,
// `composer_draft`, `caret_on`, the input marks `CARETS`), as every lane reads
// Codex.

/// Whether Codex's composer holds no typed text, the Claude lane's rule
/// ([`super::upgrade::composer_is_empty`]) over Codex's input line
/// ([`aterm_phase::codex::composer_draft`], either mark): present and blank, or
/// the placeholder, which is text at column 2 of the caret row with the cursor
/// there and that cell drawn dim (`dim_at_caret`, via the `cell` verb). A typed
/// draft with its caret moved home has the cursor there too but no dim cell. A
/// second draft row is typed text regardless.
#[must_use]
pub fn composer_is_empty(
    rows: &[String],
    cursor: Option<(usize, usize)>,
    dim_at_caret: bool,
) -> bool {
    let Some((caret, lines)) = aterm_phase::codex::composer_draft(rows) else {
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

/// Whether a turn runs on screen: the status row's `esc to interrupt` (`•
/// Working (7s • esc to interrupt)`), the busy anchor shared with Claude
/// Code's footer.
#[must_use]
pub fn busy_on_screen(rows: &[String]) -> bool {
    let busy = aterm_phase::anchors::anchor("busy.interrupt");
    rows.iter().any(|row| row.contains(busy))
}

/// Whether a choice box is up: Codex's own reader's prompt phase.
#[must_use]
pub fn box_on_screen(rows: &[String]) -> bool {
    aterm_phase::reader::read(Some("codex"), rows, None).phase == aterm_phase::Phase::Prompt
}

/// Whether Codex's status line says a background terminal still runs (`  1
/// background terminal running · /ps to view · /stop to close`): an indented
/// row opening with the count. A transcript row saying so only ever makes the
/// move wait; the kernel's count ([`DaemonFacts::terminals`]) is the proof.
#[must_use]
pub fn terminals_on_screen(rows: &[String]) -> bool {
    rows.iter()
        .any(|row| aterm_phase::codex::is_background_terminal_row(row))
}

/// Whether Codex's footer says a goal is being pursued (`Pursuing goal (…)` on
/// the row under the input line), via the shared goal reader
/// [`aterm_phase::codex::goal_state`] (the footer status row,
/// [`aterm_phase::codex::footer_status`], anchor `codex.goal.pursuing`) naming
/// [`aterm_phase::codex::CodexGoal::Pursuing`], as the save-then-wait switch
/// reads it (`CodexReader::goal_active`). A transcript row is never the footer
/// and a box reads no goal. Paused, stalled or limited goals are not pursued.
#[must_use]
pub fn goal_on_screen(rows: &[String]) -> bool {
    aterm_phase::codex::goal_state(rows) == Some(aterm_phase::codex::CodexGoal::Pursuing)
}

// ---------------------------------------------------------------- the goal pause

/// How long a pause is given to show on the footer (`Goal paused (/goal
/// resume)`) before the Esc at a goal turn's head is tried ([`GoalStep::Esc`]).
pub const PAUSE_TAKE_S: u64 = 60;

/// How long from the first pause a pause that never showed is tried before
/// being given up ([`GoalStep::GiveUp`]: said once; the goal holds its tab as
/// any turn would).
pub const PAUSE_GIVE_UP_S: u64 = 30 * 60;

/// How long a goal aterm paused may stay paused without the move (its last turn
/// running, or a draft, box, keystroke or daemon work holding the tab) before
/// aterm resumes it anyway ([`GoalStep::Resume`] `bound`).
pub const GOAL_HOLD_BOUND_S: u64 = 60 * 60;

/// How long a resume is given to show on the footer before it is made again
/// ([`GoalStep::Resume`] `again`).
pub const RESUME_TAKE_S: u64 = 60;

/// How many resumes one hold makes at most; past them the goal left paused
/// is said, with the one hand step ([`GoalStep::Wait`] `goal-left-paused`).
pub const MAX_RESUMES: u32 = 3;

/// How long after a hold that ended without its move (resumed at its bound,
/// abandoned, released to a person, or a pause that never took) no new pause
/// is made ([`super::upgrade::RETRY_S`]), so a failing move does not pause and
/// resume the goal at every look.
pub const GOAL_REST_S: u64 = super::upgrade::RETRY_S;

/// One look at a Codex goal for the upgrade's pause ([`goal_step`]), read at an
/// idle point of a Codex tab or in the relaunched Codex's first moments.
#[derive(Clone, Debug)]
pub struct GoalLook<'a> {
    /// The tab's goal hold ([`super::goal_hold`]), of either owner.
    pub hold: Option<&'a Hold>,
    /// The Codex on screen is still behind its target: pauses are made only for
    /// a move still owed (a current Codex's look only resumes).
    pub behind: bool,
    /// The goal the footer names ([`aterm_phase::codex::goal_state`]).
    pub goal: Option<CodexGoal>,
    /// Whether a footer was read at all ([`aterm_phase::codex::footer_status`]):
    /// no footer (a box stands where it was) says nothing of the goal.
    pub footer: bool,
    /// The ladder's Land rung is in force (two hours behind, or `--now`,
    /// [`super::upgrade::rung_in_force`]): pauses are made only there.
    pub land: bool,
    /// What holds the move is the goal's own turn: a daemon client's
    /// ([`super::upgrade::DaemonTurn::Goal`]) or an embedded conversation's
    /// rollout turn under the footer's goal.
    pub goal_holds: bool,
    /// The gate with that turn waived ([`super::upgrade::gate_announce`]): `Go`
    /// only when the goal's turn is the only thing holding the move.
    pub waived: Gate,
    /// A save-then-wait switch is open on the tab (its ledger's open
    /// wind-down): the session is the switch's, and no pause is made.
    pub switch: bool,
    /// The move is made: the Codex on screen is not the one the pause went
    /// into, and runs the build the move was for.
    pub moved: bool,
    /// The move will not come now, and why: the round stopped (`failed`), a
    /// `--skip` or `--defer`, or the paused Codex no longer behind or in the
    /// tab.
    pub abandoned: Option<&'static str>,
    /// The goal thread's rollout head ([`rollout_head_back`]): `Some(true)` a
    /// turn at its head, `Some(false)` anything else, `None` not read (read only
    /// where an Esc is weighed).
    pub head: Option<bool>,
    /// The composer takes a line: no box, no turn's status row, nothing
    /// typed in it (the typing fence asks it again, fresh, before any key).
    pub free: bool,
    /// An Esc would reach the goal's turn and nothing else: no box, nothing
    /// typed; the turn's own status row may stand (it does through a turn's
    /// head). The rollout head is the guard ([`GoalLook::head`]).
    pub esc_free: bool,
    /// The goal's thread fell into a sandbox its launch bypassed
    /// (`supervise::codex_usage::sandbox_fell`, read only where a resume without
    /// the move is weighed): the goal is not resumed there, since sandboxed work
    /// cannot commit or push. A resume after the move still happens: the
    /// relaunch's launch flags bring the thread back out.
    pub sandboxed: bool,
    /// A person typed within [`super::upgrade::KEYS_GAP_S`]: the floor no
    /// rung and no word waives.
    pub typing: bool,
    /// A person's hand since the hold's last edge ([`Hold::last_at`]); an
    /// unknown stamp counts.
    pub person_since: bool,
    /// The look's clock.
    pub now: u64,
}

/// What the upgrade's goal pause does at one look ([`goal_step`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalStep {
    /// Nothing of the goal's: the ordinary step decides.
    Pass,
    /// Wait on the goal's own step, by this word.
    Wait(&'static str),
    /// Type `/goal pause` (the goal claimed first, [`super::goal_hold::claim`]).
    Pause,
    /// Press the guarded Esc: the pause did not show, and the goal's turn is
    /// at its HEAD.
    Esc,
    /// The pause shows on the footer: the goal is aterm's to resume, once.
    Took,
    /// Paused, the move not made: the ordinary step goes on (its wait on the
    /// paused goal's last turn worded `goal-held`).
    Hold,
    /// Type `/goal resume` (or at the relaunched Codex's first moments press its
    /// box's `Resume goal`), and why: `moved`, `abandoned:<why>`, `bound`
    /// ([`GOAL_HOLD_BOUND_S`]), `again` (a resume that did not show).
    Resume(&'static str),
    /// The resume shows on the footer: nothing owed.
    Resumed,
    /// The goal is no longer aterm's to resume, and why: `by-hand` (a person's
    /// hand since aterm's last edge, or pursued again without aterm's resume),
    /// `changed` (the footer shows it neither paused nor pursued).
    Release(&'static str),
    /// The pause never showed within [`PAUSE_GIVE_UP_S`]: given up, said
    /// once; the goal holds its tab as a running turn does.
    GiveUp,
}

/// Why a goal is resumed without the move when the paused Codex is gone and the
/// tab's Codex is not aterm's relaunch: the person restarted it
/// ([`GoalLook::abandoned`]); a hand since makes the goal theirs.
pub const BY_HAND: &str = "abandoned:by-hand";

/// The note a hold carries once the relaunched Codex's paused-goal box was left
/// to a person ([`Hold::say`]): their answer since is theirs ([`goal_step`]).
pub const BOX_THEIRS: &str = "box-theirs";

/// Whether a hold that ended keeps the next pause off ([`GOAL_REST_S`]): it
/// ended without its move, within the rest.
fn resting(h: &Hold, now: u64) -> bool {
    goal_rest_until(h, now).is_some()
}

/// Until when an upgrade hold that ended without its move keeps the next pause
/// off ([`GOAL_REST_S`] past its last edge); `None` for a hold still owed, one
/// that ended with its move, or an expired rest.
#[must_use]
pub fn goal_rest_until(h: &Hold, now: u64) -> Option<u64> {
    let until = h.last_at().saturating_add(GOAL_REST_S);
    (h.owner == super::goal_hold::Owner::Upgrade && !h.owes() && h.why != "moved" && now < until)
        .then_some(until)
}

/// Whether the goal hold `h` paused has stayed paused past its bound at `now`
/// ([`GOAL_HOLD_BOUND_S`] from the later of pause made and pause seen), making
/// the resume due without the move. The goal pause's model projects `overdue`
/// through it too (`upgrade_drive::goal_projection`).
#[must_use]
pub fn goal_hold_past_bound(h: &Hold, now: u64) -> bool {
    now.saturating_sub(h.took_at.max(h.at)) >= GOAL_HOLD_BOUND_S
}

/// How long a paused goal may owe its resume before it is told to a person as
/// left paused, with the hand step (`/goal resume`): the hold's bound plus half
/// an hour of idle points that could not take the resume.
pub const GOAL_LEFT_AFTER_S: u64 = GOAL_HOLD_BOUND_S + 30 * 60;

/// Since when the goal hold `h` paused counts as left paused at `now`: owed its
/// resume past [`GOAL_LEFT_AFTER_S`] from its pause, or its resumes spent
/// ([`MAX_RESUMES`]) and the last not seen within [`RESUME_TAKE_S`]. `None`
/// otherwise.
#[must_use]
pub fn goal_left_since(h: &Hold, now: u64) -> Option<u64> {
    if h.owner != super::goal_hold::Owner::Upgrade || !h.owes() {
        return None;
    }
    let bound = h.at.saturating_add(GOAL_LEFT_AFTER_S);
    let spent = (h.resumes >= MAX_RESUMES).then(|| h.resume_at.saturating_add(RESUME_TAKE_S));
    [Some(bound), spent]
        .into_iter()
        .flatten()
        .filter(|at| now >= *at)
        .min()
}

/// The upgrade's goal pause over one look ([`GoalLook`]): pause the goal
/// briefly (`/goal pause`, or Esc at a new goal turn's head), move Codex, then
/// `/goal resume`; no running tool call is cut off.
///
/// * No hold owed: a pursued goal whose own turn alone holds the move
///   (`goal_holds`, `waived` open), at the Land rung, Codex behind, no `--skip`
///   or `--defer`, no switch or switch hold, no rest, composer free and nobody
///   typing: [`GoalStep::Pause`]. Otherwise pass.
/// * Pausing (made, not seen): shown paused is taken, unless a person's hand
///   since (released to them). Still pursued: a hand since releases; within
///   [`PAUSE_TAKE_S`] wait; past [`PAUSE_GIVE_UP_S`] give up; else, at the goal
///   turn's head with no box, nothing typed and nobody typing
///   ([`GoalLook::esc_free`]), the one Esc ([`GoalStep::Esc`]).
/// * Paused (taken): pursued again without aterm's resume is released. Still
///   paused: once the move is made, abandoned, or past [`GOAL_HOLD_BOUND_S`],
///   the resume (or a wait while the composer is busy), unless the relaunched
///   box was left to a person or the person restarted Codex ([`BY_HAND`]) with
///   a hand since ([`BOX_THEIRS`]), which releases; a resume without the move
///   into a sandboxed thread is withheld ([`SANDBOXED`]). Before that,
///   [`GoalStep::Hold`].
/// * Resuming (made, not seen): pursued is resumed. Still paused: a hand since
///   releases; within [`RESUME_TAKE_S`] wait; past [`MAX_RESUMES`]
///   `goal-left-paused` (said, with the hand step); else resume again
///   (`again`).
/// * A footer naming the goal neither paused nor pursued (limit, stalled,
///   achieved, unmet, or with a hand since no goal at all) while a hold is owed
///   is released (`changed`). No footer, or no goal with no hand since, decides
///   nothing: a paused goal leaves that state only by a key or a turn.
/// * The switch's hold is not the upgrade's; while a switch is open nothing is
///   typed for the upgrade's hold either (`switch`), and its records go on.
#[must_use]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeGoalPause",
        action = "GoalPause",
        project = "aterm_agent::harness::upgrade_drive::goal_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeGoalPause",
        action = "GoalEsc",
        project = "aterm_agent::harness::upgrade_drive::goal_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeGoalPause",
        action = "GoalResume",
        project = "aterm_agent::harness::upgrade_drive::goal_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeGoalPause",
        action = "Release",
        project = "aterm_agent::harness::upgrade_drive::goal_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeGoalPause",
        action = "Took",
        project = "aterm_agent::harness::upgrade_drive::goal_projection"
    )
)]
pub fn goal_step(g: &GoalLook) -> GoalStep {
    use super::goal_hold::Owner;
    let Some(h) = g.hold.filter(|h| h.owes()) else {
        return goal_start(g);
    };
    if h.owner == Owner::Switch {
        return GoalStep::Pass;
    }
    let step = owed_step(g, h);
    // An open switch owns the session: no Esc or resume for the upgrade's goal
    // (a goal resumed under it would run on the cheaper model); record edges
    // go on.
    if g.switch && matches!(step, GoalStep::Esc | GoalStep::Resume(_)) {
        return GoalStep::Wait("switch");
    }
    step
}

/// Why a resume without the move is withheld: the goal's thread fell into a
/// sandbox ([`GoalLook::sandboxed`]); the goal stays paused and the tab's row
/// gives the fix (quit Codex, resume the thread with its launch flags).
pub const SANDBOXED: &str = "goal-sandboxed";

/// [`goal_step`] for a hold of the upgrade's that owes the goal its resume.
fn owed_step(g: &GoalLook, h: &Hold) -> GoalStep {
    use super::goal_hold::{How, Stage};
    let now = g.now;
    // A look that read no goal decides nothing: no footer (a box stands there),
    // or no goal named with nobody's hand since the hold's last edge. A paused
    // goal leaves that state only by a key or by a turn naming its end
    // ([`CodexGoal::Achieved`], [`CodexGoal::Unmet`], released below), so a
    // misread row must never release a resume still owed.
    if !g.footer || (g.goal.is_none() && !g.person_since) {
        return match h.stage {
            Stage::Pausing => GoalStep::Wait("goal-pausing"),
            Stage::Resuming => GoalStep::Wait("goal-resuming"),
            _ if g.moved || g.abandoned.is_some() => GoalStep::Wait("goal-resume"),
            _ => GoalStep::Hold,
        };
    }
    match (h.stage, g.goal) {
        // The pause shows, but a person's hand since it was typed means it may
        // be theirs: never claimed, never resumed over them.
        (Stage::Pausing, Some(CodexGoal::Paused)) if g.person_since => GoalStep::Release("by-hand"),
        (Stage::Pausing, Some(CodexGoal::Paused)) => GoalStep::Took,
        (Stage::Pausing, Some(CodexGoal::Pursuing)) => {
            if g.person_since {
                GoalStep::Release("by-hand")
            } else if now.saturating_sub(h.tried_at) < PAUSE_TAKE_S {
                GoalStep::Wait("goal-pausing")
            } else if now.saturating_sub(h.at) >= PAUSE_GIVE_UP_S {
                GoalStep::GiveUp
            } else if h.how == How::Typed && g.head == Some(true) && g.esc_free && !g.typing {
                GoalStep::Esc
            } else {
                GoalStep::Wait("goal-pausing")
            }
        }
        (Stage::Paused, Some(CodexGoal::Pursuing)) => GoalStep::Release("by-hand"),
        (Stage::Paused, Some(CodexGoal::Paused)) => {
            let due = if g.moved {
                Some("moved")
            } else if let Some(why) = g.abandoned {
                Some(why)
            } else if goal_hold_past_bound(h, now) {
                Some("bound")
            } else {
                None
            };
            match due {
                // The relaunched Codex's box was left to a person
                // (`upgrade_codex_drive::resume_on_relaunch`) and their hand has
                // been on it since: its answer was theirs, `Leave paused`
                // included. Likewise a Codex the person restarted themselves
                // (`abandoned:by-hand`).
                Some(why) if (h.said(BOX_THEIRS) || why == BY_HAND) && g.person_since => {
                    GoalStep::Release("by-hand")
                }
                // Resumed without the move, the goal would go on inside the
                // sandbox its thread fell into: left paused, and said.
                Some(why) if why != "moved" && g.sandboxed => GoalStep::Wait(SANDBOXED),
                Some(why) if g.free && !g.typing => GoalStep::Resume(why),
                Some(_) => GoalStep::Wait("goal-resume"),
                None => GoalStep::Hold,
            }
        }
        (Stage::Resuming, Some(CodexGoal::Pursuing)) => GoalStep::Resumed,
        (Stage::Resuming, Some(CodexGoal::Paused)) => {
            if g.person_since {
                GoalStep::Release("by-hand")
            } else if now.saturating_sub(h.resume_at) < RESUME_TAKE_S {
                GoalStep::Wait("goal-resuming")
            } else if h.resumes >= MAX_RESUMES {
                GoalStep::Wait("goal-left-paused")
            } else if h.why != "moved" && g.sandboxed {
                GoalStep::Wait(SANDBOXED)
            } else if g.free && !g.typing {
                GoalStep::Resume("again")
            } else {
                GoalStep::Wait("goal-resuming")
            }
        }
        _ => GoalStep::Release("changed"),
    }
}

/// [`goal_step`] with no hold of the upgrade's owed: the pause, or nothing.
fn goal_start(g: &GoalLook) -> GoalStep {
    use super::goal_hold::Owner;
    let pursued = g.behind && g.goal == Some(CodexGoal::Pursuing) && g.goal_holds;
    let switch_holds = g.hold.is_some_and(|h| h.owner == Owner::Switch && h.owes());
    let rest = g.hold.is_some_and(|h| resting(h, g.now));
    if pursued
        && g.land
        && g.abandoned.is_none()
        && !g.switch
        && !switch_holds
        && !rest
        && g.waived == Gate::Go
        && g.free
        && !g.typing
    {
        GoalStep::Pause
    } else {
        GoalStep::Pass
    }
}

/// Whether the tab of goal hold `hold` must still be looked at for it (the
/// upgrade's hold owes a resume), whatever else its upgrade says: the host's
/// `due` ([`super::upgrade_drive::due`]) keeps the tab due so no goal is left
/// paused by aterm.
#[must_use]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeGoalPause",
        action = "LetGo",
        project = "aterm_agent::harness::upgrade_drive::goal_projection"
    )
)]
pub fn goal_owed(hold: Option<&Hold>) -> bool {
    hold.is_some_and(|h| h.owner == super::goal_hold::Owner::Upgrade && h.owes())
}

// ---------------------------------------------------------------- the spawn trees

/// Where a thread the daemon holds sits in its conversation's spawn tree, from
/// its rollout's first line (`session_meta`, [`session_lineage`]). A daemon's
/// writer locks can all belong to one conversation: the thread a TUI resumed
/// (`"source":"cli"`) plus subagents it spawned
/// (`"source":{"subagent":{"thread_spawn":{"parent_thread_id":…,"depth":1,…}}}`),
/// whose turns run on their own and never draw on the TUI's main screen. So a
/// lock is not a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lineage {
    /// A conversation's own thread: `source` a plain word (`"cli"`).
    Root,
    /// A subagent spawned by the thread `parent` (`source.subagent.
    /// thread_spawn.parent_thread_id`, a thread id).
    Spawned(String),
    /// A subagent whose parent is not named (`source.subagent` of another
    /// shape): a thread of some conversation, never a root.
    Subagent,
    /// No `session_meta` read (no rollout yet, a first line past the bound or
    /// not JSON, or an unknown shape): proven neither root nor subagent.
    Unread,
}

/// A thread's lineage from its rollout's first line (`session_meta`, see
/// [`Lineage`]). Anything else, non-JSON, or an unknown `source` shape is
/// [`Lineage::Unread`].
#[must_use]
pub fn session_lineage(first_line: &str) -> Lineage {
    let Ok(v) = aterm_json::from_str::<Value>(first_line.trim()) else {
        return Lineage::Unread;
    };
    if v.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Lineage::Unread;
    }
    let Some(source) = v.get("payload").and_then(|p| p.get("source")) else {
        return Lineage::Unread;
    };
    if source.as_str().is_some() {
        return Lineage::Root;
    }
    let Some(sub) = source.get("subagent") else {
        return Lineage::Unread;
    };
    match sub
        .get("thread_spawn")
        .and_then(|spawn| spawn.get("parent_thread_id"))
        .and_then(Value::as_str)
    {
        Some(parent) if is_thread_id(parent) => Lineage::Spawned(parent.to_string()),
        _ => Lineage::Subagent,
    }
}

/// One thread the daemon holds (its writer lock) as one read found it: its id,
/// its turn state by the rollout's last turn event ([`rollout_turn_back`];
/// [`TurnState::Unknown`] counts as running), and its spawn-tree place
/// ([`Lineage`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// The thread.
    pub thread: String,
    /// Its turn.
    pub turn: TurnState,
    /// Its spawn tree.
    pub lineage: Lineage,
}

impl Loaded {
    /// Whether a turn runs in it, or might ([`TurnState::Unknown`]).
    #[must_use]
    pub fn running(&self) -> bool {
        self.turn != TurnState::Idle
    }
}

/// The root of `thread`'s spawn tree among `threads`: its parent chain followed
/// to a [`Lineage::Root`]. `None` when the chain leaves the daemon, meets an
/// unnamed-parent subagent or an unread lineage, or loops.
#[must_use]
pub fn tree_root<'a>(thread: &str, threads: &'a [Loaded]) -> Option<&'a str> {
    let mut at = threads.iter().find(|l| l.thread == thread)?;
    for _ in 0..=threads.len() {
        match &at.lineage {
            Lineage::Root => return Some(at.thread.as_str()),
            Lineage::Spawned(parent) => at = threads.iter().find(|l| l.thread == *parent)?,
            Lineage::Subagent | Lineage::Unread => return None,
        }
    }
    None
}

// ---------------------------------------------------------------- the daemon's turns

/// The client's own conversation, where the kernel names it: every thread the
/// daemon holds hangs from one root ([`tree_root`]) and `client` is the only
/// attached Codex ([`daemon_clients_in`]); returns that root, which `codex
/// resume` takes back. The daemon holds a lock for every loaded thread,
/// including a client's from attach, so one conversation and one client is
/// that client's. A single thread is named whatever its lineage reads unless
/// it is a subagent. `None` otherwise (two roots, an unclaimed thread, more
/// clients, or an unlistable client set). Never from argv, which names only
/// the thread a TUI started on.
#[must_use]
pub fn own_thread<'a>(
    client: u32,
    threads: &'a [Loaded],
    clients: Option<&[u32]>,
) -> Option<&'a str> {
    if !matches!(clients, Some([pid]) if *pid == client) {
        return None;
    }
    match threads {
        [] => None,
        [only] => {
            matches!(only.lineage, Lineage::Root | Lineage::Unread).then_some(only.thread.as_str())
        }
        [first, ..] => {
            let root = tree_root(&first.thread, threads)?;
            threads
                .iter()
                .all(|l| tree_root(&l.thread, threads) == Some(root))
                .then_some(root)
        }
    }
}

/// Where a turn runs that a daemon client's `/exit` must wait for
/// ([`super::upgrade::DaemonTurn`], the gate's running-work floor), over the
/// daemon's threads ([`Loaded`]), attached clients (`clients`), threads this
/// tab's screen placed elsewhere (`elsewhere`, [`still_through`]), and whether
/// its footer shows a pursued goal (`goal`, [`goal_on_screen`]):
///
/// * the client's own conversation named by the kernel ([`own_thread`]): a
///   turn anywhere in it (root or subagent) holds: `Goal` under a shown goal,
///   else `Own`;
/// * not named: a running thread holds as `Unplaced` unless placed elsewhere,
///   which only a root can be, only with another Codex attached, and never
///   under a goal on this screen:
///   * a subagent is never placed: its turns draw on no main screen;
///   * with this client the daemon's only one nothing is placed: a second root
///     may be this client's own conversation shown through another view;
///   * under a goal nothing is placed: a pursued goal's next turn streams with
///     no status row under the last turn's end row, looking exactly like
///     another session's.
///
/// Still assumed where a root is placed: this client's screen draws its own
/// root's turns (not while it shows a subagent's or `/side` fork's thread, nor
/// a goal pursued without a status line). The ladder's model states it
/// (`HarnessUpgradeLadder`, `work`).
#[must_use]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeLadder",
        action = "Move",
        project = "aterm_agent::harness::upgrade_drive::ladder_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeGoalPause",
        action = "LadderMove",
        project = "aterm_agent::harness::upgrade_drive::goal_projection"
    )
)]
pub fn daemon_turn(
    client: u32,
    threads: &[Loaded],
    clients: Option<&[u32]>,
    elsewhere: &[String],
    goal: bool,
) -> super::upgrade::DaemonTurn {
    use super::upgrade::DaemonTurn;
    if own_thread(client, threads, clients).is_some() {
        return match (threads.iter().any(Loaded::running), goal) {
            (false, _) => DaemonTurn::None,
            (true, true) => DaemonTurn::Goal,
            (true, false) => DaemonTurn::Own,
        };
    }
    let another = clients.is_some_and(|c| c.iter().any(|pid| *pid != client));
    let placed = |l: &Loaded| {
        !goal && another && l.lineage == Lineage::Root && elsewhere.contains(&l.thread)
    };
    if threads.iter().any(|l| l.running() && !placed(l)) {
        DaemonTurn::Unplaced
    } else {
        DaemonTurn::None
    }
}

/// The threads this tab's screen places in another session, and the record it
/// keeps: each daemon thread running at a look of this screen's still run
/// (looks reading it authoritatively idle with the same last words,
/// `upgrade_drive::St::still`), with the run's first look that found it
/// running (`record`). At `now`, given `running` and whether this look
/// continues the run (`in_run`; otherwise nothing carries over), returns the
/// new record and those placed: running now and first found at least
/// [`super::upgrade::QUIET_S`] ago. The client's own root draws on its screen,
/// so a root running across a quiet still run is another session's; that is
/// the assumption, and [`daemon_turn`] uses a placement only for a root, with
/// another Codex attached and no goal on screen. A thread found running only
/// now is not placed: a turn streams nothing at first.
#[must_use]
pub fn still_through(
    record: &[(String, u64)],
    running: &[String],
    in_run: bool,
    now: u64,
) -> (Vec<(String, u64)>, Vec<String>) {
    let kept: Vec<(String, u64)> = running
        .iter()
        .map(|thread| {
            let since = record
                .iter()
                .find(|(seen, _)| in_run && seen == thread)
                .map_or(now, |(_, since)| (*since).min(now));
            (thread.clone(), since)
        })
        .collect();
    let placed = kept
        .iter()
        .filter(|(_, since)| now.saturating_sub(*since) >= super::upgrade::QUIET_S)
        .map(|(thread, _)| thread.clone())
        .collect();
    (kept, placed)
}

// ---------------------------------------------------------------- the words

/// How every Codex notice opens: the supervisor's and the READY reader's tag
/// ([`rollout_has_ready`] resets on it).
pub const ANNOUNCE_HEAD: &str = concat!(harness_mark!(), " Codex ");

/// THE LONGEST PASTE CODEX DRAWS AS TEXT, in characters (measured 2026-09-30
/// in a private headless aterm on Codex 0.159.2): a bracketed paste of 1000
/// characters is drawn in the composer as it is; one of 1001 or more is drawn
/// as `› [Pasted Content 1001 chars]`. A notice past it — the base notice is
/// about 724, and the running clause of five long commands takes it past —
/// had its guarded Enter miss its last word, `left_standing` find neither the
/// text nor its head in the placeholder and drop the record, and aterm's
/// placeholder left in the person's composer reading as their draft. The
/// notice is held to it (`upgrade::with_running_within`).
pub const PASTE_SHOWN_MAX: usize = 1000;

/// THE NOTICE to an EMBEDDED session: one ordinary user turn asking for a
/// good stopping point and naming the one line to answer with. Never asks the
/// agent to cancel work that is still making progress. Like Claude Code's
/// (`upgrade::STOPPING_POINT`), it says that a wait on something that has
/// already ended is not such work. It ENDS in the shared closing sentence
/// ([`NOTICE_CLOSE`]), bound to the constant, so what runs under the agent
/// is named before it (`upgrade::with_running`) — never after, where a long
/// path's wrap hid the guarded Enter's last word (the review of 2026-09-30).
#[must_use]
pub fn prepare_prompt(from: &Version, to: &Version, marker: &str) -> String {
    format!(
        "{ANNOUNCE_HEAD}{to} (managed) is installed; this session runs {from}. To move you onto \
         it, aterm will exit this Codex (/exit) and resume this same conversation in place \
         (codex resume, same tab, same flags). Please get to a good stopping point first: let \
         commands or background tasks you started finish while they are still making progress \
         (do not cancel them), but stop any of yours that only waits for something that has \
         already ended or can never happen: that wait is not work. Save work in progress, and \
         do not start new long-running work. When nothing of yours is still running, reply \
         with {marker} on a line by itself. {NOTICE_CLOSE}"
    )
}

/// The continuation, typed once the relaunched embedded session holds its
/// thread again.
#[must_use]
pub fn continue_prompt(from: &Version, to: &Version) -> String {
    format!(
        "{} Upgraded: this session was restarted on Codex {to} (from {from}) and resumed. {}",
        super::upgrade::HARNESS_MARK,
        super::upgrade::CARRY_ON
    )
}

// ---------------------------------------------------------------- the client

/// How a Codex TUI runs, as the kernel proves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A client of the daemon: it holds no thread lock.
    Daemon,
    /// It holds `thread` itself (`thread-writer-locks/<thread>.lock`).
    Embedded {
        /// The thread it holds.
        thread: String,
        /// Its rollout holds a user message: a conversation to wind down and
        /// resume. `false`: nothing to resume yet.
        conversation: bool,
    },
}

impl Mode {
    /// The word the state file and the ledger carry.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Mode::Daemon => "daemon",
            Mode::Embedded { .. } => "embedded",
        }
    }

    /// Whether this client's move needs the cooperative protocol: an embedded
    /// session with a conversation. Others hold nothing that ends with the TUI.
    #[must_use]
    pub fn cooperative(&self) -> bool {
        matches!(
            self,
            Mode::Embedded {
                conversation: true,
                ..
            }
        )
    }
}

/// The client's next step: [`super::upgrade::next_step`] for an embedded
/// session with a conversation (notice, READY, re-asks and give-up, as the
/// Claude lane), and for every other client the gate alone (idle, settled by
/// the ladder's rung, composer empty, no box, busy row, hold or person, and
/// for a daemon client no own or unplaceable turn in its daemon,
/// [`Facts::daemon_turn`]), then [`Step::Terminate`], which the driver carries
/// out as a typed `/exit`, never a signal. A stopped round restarts after
/// [`super::upgrade::RETRY_S`] ([`Step::Rearm`]). A daemon client also waits
/// for its daemon to reach the target build first (`daemon_behind`).
#[must_use]
pub fn next_step(
    mode: &Mode,
    phase: &Phase,
    f: &Facts,
    ready: bool,
    daemon_behind: bool,
    now_s: u64,
) -> Step {
    if mode.cooperative() {
        return super::upgrade::next_step(phase, f, ready, now_s);
    }
    // No stop is for good: a stopped round rests `RETRY_S`, then a new one
    // starts — it types nothing, so a break takes it too.
    if super::upgrade::retry_due(phase, f.failed_s, false) {
        return Step::Rearm;
    }
    // A client with no notice to take ends at its `/exit`: an idle point's
    // alone, never a break of the agent's own background work.
    if f.background_point {
        return Step::Wait("background");
    }
    match phase {
        Phase::Pending | Phase::Announced { .. } => {
            if *mode == Mode::Daemon && daemon_behind {
                return Step::Wait("daemon-first");
            }
            match super::upgrade::gate_announce(f) {
                // A thread with nothing to resume is ended and started plain,
                // the user told first ([`Facts::unnoticed`]). A daemon client's
                // `/exit` ends none of its work.
                Gate::Go
                    if f.unnoticed
                        && matches!(
                            mode,
                            Mode::Embedded {
                                conversation: false,
                                ..
                            }
                        ) =>
                {
                    Step::Wait(super::upgrade::NOTICE_SHOWN)
                }
                Gate::Go => Step::Terminate,
                Gate::Wait(w) => Step::Wait(w),
            }
        }
        Phase::Exiting { .. } | Phase::Relaunched { .. } => Step::Wait("in-flight"),
        Phase::Done => Step::Wait("done"),
        Phase::Failed(_) => Step::Wait("failed"),
    }
}

/// [`next_step`] under the user's word, per
/// [`super::upgrade::requested_step`]: a skip of this target or an unexpired
/// deferral holds it and a stopped round's new one
/// ([`super::upgrade::rearm_held`]); `--now` sets the Land rung
/// ([`super::upgrade::Rung::Land`]: no settling window, a keystroke within
/// [`super::upgrade::KEYS_GAP_S`] still holds, every floor kept).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn requested_step(
    request: &Request,
    mode: &Mode,
    phase: &Phase,
    f: &Facts,
    ready: bool,
    daemon_behind: bool,
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
    let step = if *request == Request::Now {
        let now = Facts {
            owner_now: true,
            ..f.clone()
        };
        next_step(mode, phase, &now, ready, daemon_behind, now_s)
    } else {
        next_step(mode, phase, f, ready, daemon_behind, now_s)
    };
    super::upgrade::rearm_held(request, step, target, now_s)
}

// ---------------------------------------------------------------- the daemon

/// What the sweep measured of one `$CODEX_HOME`'s daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonFacts {
    /// The version of the package the daemon runs (its executable's
    /// [`package_version`]), `None` when unreadable.
    pub running: Option<Version>,
    /// The managed build's version.
    pub managed: Version,
    /// No `auto-update-version` marker: the vendor's updater is not armed.
    pub pinned: bool,
    /// Threads the daemon holds whose turn runs or is unreadable
    /// ([`TurnState::Busy`], [`TurnState::Unknown`]), attached or not: a
    /// detached background thread has no TUI, only its lock.
    pub busy_threads: usize,
    /// Background terminals still running under the daemon: a unified-exec
    /// process a turn left running leads its own session under the daemon,
    /// while the daemon's MCP helpers stay in the daemon's session. Its thread
    /// reads idle, yet the update's restart would kill it, so it counts as
    /// running work.
    pub terminals: usize,
    /// Idle threads written within the settle ([`super::upgrade::quiet_s`]).
    pub settling_threads: usize,
    /// A person gave input to a Codex tab on this home within that tab's rung
    /// grace ([`super::upgrade::person_grace`]: `[harness] human_grace_s`, then
    /// [`super::upgrade::KEYS_GAP_S`] from [`super::upgrade::Rung::KeysOnly`]
    /// on and under `--now`).
    pub attended: bool,
    /// A hold on a Codex tab on this home.
    pub held: bool,
    /// The user's word on a Codex tab on this home holds this target (a `--skip`
    /// of it or an unexpired `--defer`, [`Request::holds`]). The daemon runs
    /// that tab's turns, so moving it would move that conversation too.
    pub owner_held: bool,
    /// Codex processes attached to the daemon (socket peers) this sweep does
    /// not list: a TUI in another aterm, a pane, or an unreadable one. Their
    /// tabs were not asked, so the daemon is not restarted.
    pub unseen_clients: usize,
}

/// The daemon step's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonStep {
    /// Already on the managed build, and pinned.
    Current,
    /// Run `app-server daemon update --from-cli --yes` from the managed build.
    Update,
    /// Not now, and why.
    Wait(&'static str),
}

/// The daemon rule: move the daemon onto the managed build and pin it
/// (disarming the vendor updater) when it runs an older build, or the same
/// build unpinned; never onto an older one (a vendor-updated daemon ahead of
/// atpkg waits, `vendor-ahead`). Only while nothing runs in it (every thread
/// idle and settled, no background terminal), no person is at a Codex tab on
/// this home, nothing holds one, no word keeps it, and every attached Codex is
/// one this sweep sees. The update restarts the daemon, so running work is
/// waited for without deadline; nothing is killed.
#[must_use]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HarnessUpgradeLadder",
        action = "Pin",
        project = "aterm_agent::harness::upgrade_drive::ladder_projection"
    )
)]
pub fn daemon_step(f: &DaemonFacts) -> DaemonStep {
    let Some(running) = &f.running else {
        return DaemonStep::Wait("daemon-version");
    };
    if *running > f.managed {
        return DaemonStep::Wait("vendor-ahead");
    }
    if *running == f.managed && f.pinned {
        return DaemonStep::Current;
    }
    if f.busy_threads > 0 {
        return DaemonStep::Wait("busy-thread");
    }
    if f.terminals > 0 {
        return DaemonStep::Wait("background-terminal");
    }
    if f.owner_held {
        return DaemonStep::Wait("owner-held");
    }
    if f.unseen_clients > 0 {
        return DaemonStep::Wait("unseen-client");
    }
    if f.held {
        return DaemonStep::Wait("held");
    }
    if f.attended {
        return DaemonStep::Wait("attended");
    }
    if f.settling_threads > 0 {
        return DaemonStep::Wait("settling");
    }
    DaemonStep::Update
}

/// The daemon's pid from `$CODEX_HOME/app-server-daemon/daemon.pid`
/// (`{"pid":92181,"processStartTime":…,…}`).
#[must_use]
pub fn parse_daemon_pid(text: &str) -> Option<u32> {
    let v: Value = aterm_json::from_str(text).ok()?;
    u32::try_from(v.get("pid")?.as_u64()?)
        .ok()
        .filter(|&p| p > 1)
}

/// What `app-server daemon update --from-cli --yes` answered: its last line, a
/// JSON object with `status` `updated` and the `runningVersion` it names.
///
/// # Errors
/// No JSON answer (`no-answer`), or a status other than `updated`.
pub fn parse_update_answer(stdout: &str) -> Result<Version, String> {
    let v: Value = stdout
        .lines()
        .rev()
        .find_map(|l| aterm_json::from_str::<Value>(l.trim()).ok())
        .ok_or_else(|| "no-answer".to_string())?;
    let status = v.get("status").and_then(Value::as_str).unwrap_or("?");
    if status != "updated" {
        return Err(status
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(24)
            .collect());
    }
    v.get("runningVersion")
        .and_then(Value::as_str)
        .and_then(Version::parse)
        .ok_or_else(|| "no-running-version".to_string())
}

/// Whether `name` is a rollout file of `thread`:
/// `rollout-<stamp>-<thread>.jsonl`. A compressed `.jsonl.zst` is not read.
#[must_use]
pub fn is_rollout_of(name: &str, thread: &str) -> bool {
    name.starts_with("rollout-")
        && name
            .strip_suffix(".jsonl")
            .is_some_and(|stem| stem.ends_with(&format!("-{thread}")))
}

/// The thread a writer-lock file name names (`<thread>.lock`).
#[must_use]
pub fn lock_thread(name: &str) -> Option<&str> {
    name.strip_suffix(".lock").filter(|t| is_thread_id(t))
}

/// The background terminals under `pid` (a daemon or an embedded TUI): every
/// descendant in `table` (`(pid, ppid, name)`) that leads its own session by
/// `getsid`, as `(pid, name)`. Codex runs unified-exec commands in their own
/// session directly under the thread-holding process, while its own helpers
/// (per-thread MCP servers) stay in its session. No shell stands between, so
/// the Claude lane's shell-name rule does not apply. An unreadable session is
/// counted, so an unreadable kernel waits.
#[must_use]
pub fn terminals_under(
    pid: u32,
    table: &[(u32, u32, String)],
    sid: impl Fn(u32) -> Option<u32>,
) -> Vec<(u32, String)> {
    let mut frontier = vec![pid];
    let mut found = Vec::new();
    let mut seen = 0;
    while let Some(p) = frontier.pop() {
        seen += 1;
        if seen > 4096 {
            break;
        }
        for (c, pp, name) in table {
            if *pp == p && *c != pid {
                frontier.push(*c);
                if sid(*c).is_none_or(|s| s == *c) {
                    found.push((*c, name.clone()));
                }
            }
        }
    }
    found
}

/// The Codex processes attached to `daemon`, from `lsof -F pdn` over every
/// `codex` process's unix sockets: each other pid holding a socket whose peer
/// (`n->0x…`) is one of the daemon's own (`d0x…`); the vendor updater is no
/// peer. `None` when the daemon itself is not listed (never "no clients").
#[must_use]
pub fn daemon_clients_in(lsof_f: &str, daemon: u32) -> Option<Vec<u32>> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut own: BTreeMap<u32, BTreeSet<&str>> = BTreeMap::new();
    let mut peers: BTreeMap<u32, BTreeSet<&str>> = BTreeMap::new();
    let mut pid = None;
    let mut dev = None;
    for line in lsof_f.lines() {
        let (tag, value) = line.split_at_checked(1).unwrap_or(("", ""));
        match tag {
            "p" => {
                pid = value.parse::<u32>().ok();
                dev = None;
            }
            "f" => dev = None,
            "d" => dev = Some(value),
            "n" => {
                let Some(p) = pid else { continue };
                if let Some(d) = dev {
                    own.entry(p).or_default().insert(d);
                }
                if let Some(peer) = value.strip_prefix("->") {
                    peers.entry(p).or_default().insert(peer);
                }
            }
            _ => {}
        }
    }
    let mine = own.get(&daemon).filter(|s| !s.is_empty())?;
    Some(
        peers
            .iter()
            .filter(|(p, ps)| **p != daemon && ps.iter().any(|x| mine.contains(x)))
            .map(|(p, _)| *p)
            .collect(),
    )
}

/// `session_meta` first lines in the real shapes, with stand-in values and cut
/// base instructions: a root (`"source":"cli"`) and a spawned subagent
/// (`multi_agent_version` 2, parent in `source` and at the top, the root as
/// `session_id`).
#[cfg(test)]
pub(crate) mod fixtures {
    /// The root `thread`'s first line.
    pub(crate) fn root_meta(thread: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-08-26T05:23:33.621Z","ordinal":0,"type":"session_meta","payload":{{"session_id":"{thread}","id":"{thread}","timestamp":"2026-08-26T05:21:24.804Z","cwd":"/stand-in/project","originator":"codex-tui","cli_version":"0.149.1","source":"cli","thread_source":"user","model_provider":"openai","base_instructions":{{"text":"You are Codex (stand-in)","provenance":{{"type":"model","model":"stand-in"}}}},"history_mode":"paginated","context_window":{{"window_id":"{thread}"}}}}}}"#
        )
    }

    /// The first line of `thread`, a subagent `parent` spawned at `depth` in
    /// the tree of `root`.
    pub(crate) fn spawned_meta(thread: &str, parent: &str, root: &str, depth: u32) -> String {
        format!(
            r#"{{"timestamp":"2026-09-28T15:28:00.257Z","ordinal":0,"type":"session_meta","payload":{{"creator_user_id":"user-stand-in","session_id":"{root}","id":"{thread}","parent_thread_id":"{parent}","timestamp":"2026-09-28T15:28:00.201Z","cwd":"/stand-in/project","originator":"codex-tui","cli_version":"0.158.0","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent}","depth":{depth},"agent_path":"/root/stand_in","agent_nickname":"Stand-in","agent_role":null}}}}}},"thread_source":"subagent","agent_nickname":"Stand-in","agent_path":"/root/stand_in","model_provider":"openai","base_instructions":{{"text":"You are Codex (stand-in)","provenance":{{"type":"model","model":"stand-in"}}}},"history_mode":"paginated","multi_agent_version":"v2","context_window":{{"window_id":"{thread}"}}}}}}"#
        )
    }
}

#[cfg(test)]
#[path = "upgrade_codex_tests.rs"]
mod tests;

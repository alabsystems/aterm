// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! LIVE UPGRADE OF A RUNNING CODEX — THE PURE HALF. The Claude lane
//! ([`super::upgrade`]) restarts one process; Codex 0.157 is TWO, and each
//! half moves by its own vendor verb (measured 2026-09-25 on a throwaway Codex
//! 0.157.0 → 0.157.1 in a private headless aterm, network denied, a fake
//! offline key):
//!
//! * **THE DAEMON** — a plain `codex` copies its own package into
//!   `$CODEX_HOME/packages/app-server-daemon/releases/<version>-<triple>/` and
//!   starts a detached, shared app server from it (`codex app-server --listen
//!   unix:// --managed-daemon`, its own process group) plus the vendor's
//!   `pid-update-loop`, which re-runs chatgpt.com's installer outside atpkg's
//!   checks while `packages/app-server-daemon/auto-update-version` exists. The
//!   daemon HOLDS every conversation (the thread's writer lock and its rollout
//!   are open in it, not in the TUI). `<managed codex> app-server daemon update
//!   --from-cli --yes` copies the managed build in, PINS it (the marker goes,
//!   the updater with it) and restarts the daemon; an attached TUI stays alive
//!   and prints `• Reconnected. No input was resent.` The verb's own help says
//!   it "may interrupt running work", and it does: a BACKGROUND TERMINAL a
//!   finished turn left running under the daemon dies with the restart
//!   (measured, review of 2026-09-26). So [`daemon_step`] runs it only when
//!   every loaded thread is idle and no background terminal runs under it
//!   ([`terminals_under`]: a unified-exec process leads a session of its
//!   own, where the daemon's MCP servers stay in its session), and only
//!   past every tab's owner word and every Codex attached to it
//!   ([`daemon_clients_in`]).
//! * **A DAEMON-MODE CLIENT** (the TUI) holds nothing: `/exit` exits 0,
//!   restores the terminal, and prints `Disconnected from this task. Any
//!   running work continues.` / `Reconnect: codex resume <uuid>`; the daemon
//!   keeps the thread, and `codex resume <uuid>` reattaches. No wind-down and
//!   no continuation are owed: the conversation never stopped, and nor does a
//!   background terminal it left, which runs in the daemon (measured: a
//!   `sleep` survived the client's `/exit`).
//! * **AN EMBEDDED SESSION** (`--no-daemon`, or a launch Codex keeps out of
//!   the daemon) holds `$CODEX_HOME/thread-writer-locks/<uuid>.lock` itself —
//!   an exclusive flock, so the kernel proves which pid owns which thread —
//!   and its runtime ends with it — its background terminals too (measured:
//!   a `sleep` the turn left running died with the `/exit`), so the exit
//!   waits for them as it waits for a shell. It gets the Claude lane's cooperative
//!   protocol ([`prepare_prompt`] → a READY answer in the rollout
//!   ([`rollout_has_ready`]) → `/exit` → `codex resume` → [`continue_prompt`]),
//!   with `/exit` in place of SIGTERM: SIGTERM, SIGINT and SIGHUP all kill the
//!   Codex TUI at once with no terminal restore (the research's pty run: the
//!   tab left in the alternate screen, kitty keyboard `>7u`, mouse reporting
//!   and bracketed paste on), and `/exit` restores every mode (measured here:
//!   `modes` reads `alt_screen=false kitty_keyboard=none mouse_mode=none`
//!   after it). NOTHING IN THE CODEX LANE SENDS A SIGNAL.
//!
//! Everything here is PURE over facts the driver
//! (`upgrade_drive`'s `codex` module) measured: the package version, the flag
//! table and the argv rewrite, the exit hint, the rollout readers, the
//! composer, the prompts, and the two gates. Codex's files are a third
//! party's, so every reader refuses a shape it was not written for rather
//! than guessing.
//!
//! The daemon rule is a derived model (`aterm_spec::derive::
//! harness_codex_daemon_update_model`), bound to [`daemon_step`] over every
//! state it reaches by this module's tests. The whole branch runs against a
//! REAL Codex in `tools/test-codex-live-upgrade.sh` (a private headless
//! aterm, no network but a loopback fake model), driven by that aterm's own
//! host: both modes moved, an inline TUI at a two-row prompt, and the
//! SIGTERM negative control.
//!
//! The screen grammar here (the composer, the exit hint) is the Codex lane's
//! own until aterm-phase reads Codex's composer; the day it does, these
//! readers give way to it.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::upgrade::{ArgvRefusal, Facts, Gate, Phase, Request, Step, Version};

// ---------------------------------------------------------------- the package

/// The manifest every Codex package carries at its root (layoutVersion 1,
/// measured in the atpkg store's 0.157.0 and 0.157.1, the daemon's own
/// `releases/<…>/` copies, and — by the research — the Homebrew cask).
pub const PACKAGE_JSON: &str = "codex-package.json";

/// THE VERSION OF THE CODEX `exe` IS, read from its package's manifest —
/// never by running it. `codex --version` prints `codex-cli 0.157.1`, whose
/// first word no version parser takes, and an exec of a candidate can hang
/// (a quarantined cask measured at over ten minutes, the research), holding
/// the sweep lock for every upgrade on the machine. The manifest is the root
/// whose `entrypoint` names `exe` itself (`<root>/bin/codex`), so a stray
/// manifest in a parent directory never answers for a binary it does not
/// describe. `None` for anything else.
#[must_use]
pub fn package_version(exe: &Path) -> Option<Version> {
    let exe = std::fs::canonicalize(exe).ok()?;
    exe.ancestors().skip(1).take(2).find_map(|root| {
        let text = std::fs::read_to_string(root.join(PACKAGE_JSON)).ok()?;
        let (version, entry) = parse_package(&text)?;
        (std::fs::canonicalize(root.join(entry)).ok()? == exe).then_some(version)
    })
}

/// One `codex-package.json`: its version and its entrypoint (relative to the
/// package root). `None` unless `layoutVersion` is 1 and both are present —
/// a changed layout reads as no version, never as a guess.
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

/// Codex 0.157.1's interactive options (`codex --help`, `codex resume
/// --help`, the same set under both). A flag missing here refuses the rewrite
/// (fail closed): a new version's flag is added on purpose, once it is known
/// to be safe to carry. The flags that keep a session OUT of the daemon (`-c`,
/// `--profile`, `--oss`, `--strict-config`, `--no-daemon`, the hook-trust
/// bypass) are all KEPT, so the relaunch lands in the mode it left.
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
    // Attached to the FIRST prompt, which the conversation already holds.
    ("-i", Arity::Many, Fate::Drop),
    ("--image", Arity::Many, Fate::Drop),
    ("--include-non-interactive", Arity::None, Fate::Drop),
    ("--last", Arity::None, Fate::Drop),
    ("--local-provider", Arity::One, Fate::Keep),
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

/// Codex 0.157.1's subcommands (`codex --help`). Only no subcommand, `resume`
/// and `fork` run the interactive TUI whose conversation a relaunch resumes;
/// every other one is refused ([`ArgvRefusal::NotResumable`]).
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

/// THE ARGV REWRITE for Codex: the TUI's argv (argv[0] included, dropped)
/// becomes the words after the executable on the relaunch line — `resume
/// <every kept flag and its value, verbatim> <thread>`, or, with no thread
/// (`None`: a session with no conversation yet, nothing to resume), the kept
/// flags alone. The launch's subcommand (`resume`, `fork`), its session id,
/// its first prompt and the images attached to it are dropped: the
/// conversation already holds them, and `fork` has already forked — the TUI
/// runs the fork, whose own id the exit hint names.
///
/// # Errors
/// An unknown flag or subcommand ([`ArgvRefusal::UnknownFlag`]), or one
/// whose session is not a TUI resumable in place ([`ArgvRefusal::NotResumable`]:
/// `exec`, `--remote`, `--worktree`, …).
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

/// Whether `argv` is an interactive Codex TUI's — no subcommand, `resume` or
/// `fork` — and not the daemon, its updater, `exec`, or any other of the
/// binary's many faces (every process of the daemon is also named `codex`).
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
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------- the exit

/// What a Codex TUI printed as it left: the thread it held and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitHint {
    /// The thread `codex resume` takes back.
    pub thread: String,
    /// `true` for a daemon-mode client (`Reconnect: codex resume <id>`: the
    /// thread lives on in the daemon), `false` for an embedded session (`To
    /// continue this session, run:` / `  codex resume <id>`).
    pub daemon: bool,
}

/// The daemon-mode line's words (measured 0.157.0 and 0.157.1).
const RECONNECT: &str = "Reconnect: codex resume ";

/// The embedded line's words (the row under `To continue this session,
/// run:`).
const CONTINUE: &str = "codex resume ";

/// The daemon-mode hint's last line, under the `Reconnect:` one (measured on
/// every daemon-mode exit, whether or not a turn ran).
const STOP_HINT: &str = "Stop the current turn:";

/// The token tally a TUI prints as it leaves once a turn ran: under the
/// daemon-mode hint (`Token usage so far: total=2 input=1 output=1`, measured
/// 0.157.0 2026-09-26, after the stop line) and above the embedded one
/// (`Token usage: total=4 input=2 output=2`).
const TOKEN_USAGE: &str = "Token usage";

/// THE EXIT HINT in `rows`, the LAST one: `Reconnect: codex resume <id>`
/// (daemon mode) or `codex resume <id>` indented under `To continue this
/// session, run:` (embedded), the id checked ([`is_thread_id`]). A row wrapped
/// in a narrow pane is read joined with the next. `rows` must be the TUI's
/// OWN output — the shell integration's block for the command that ran it, or
/// [`hint_above_prompt`]'s rows — never the whole screen: an earlier exit's
/// hint stays on it, and read from there a TUI that printed nothing would be
/// relaunched into ANOTHER conversation (measured: the screen after the
/// second exit in one tab showed both).
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

/// One row's hint, `above` the row over it (the embedded form's heading).
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
    let heading = above.is_some_and(|a| a.trim().starts_with("To continue this session"));
    (heading && is_thread_id(id)).then(|| ExitHint {
        thread: id.to_string(),
        daemon: false,
    })
}

/// The rows a TUI printed as it left, when the shell integration does not
/// hand over the command's own output: the hint must sit DIRECTLY above
/// `prompt_row`, the FIRST row of the prompt the shell drew after it — with
/// nothing between them but the lines the TUI itself prints under its hint
/// ([`STOP_HINT`]'s, the [`TOKEN_USAGE`] tally) and blank rows — so a TUI
/// that printed nothing (the rows above the prompt are then the command that
/// ran it, with an older exit's hint above that) reads no hint and nothing is
/// typed. The prompt's first row is the caller's to know: the cursor row
/// less the rows a multi-row prompt draws above it (the entering block's
/// `cmd - prompt`). Bounded by the cursor row alone, a two-row prompt (`%~`
/// then `%#`) put its first row between the hint and the cursor and a daemon
/// client was exited and never relaunched (review of 2026-09-26, measured
/// with an inline Codex whose daemon update had wiped the tab's blocks).
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
    /// No turn event in the tail read: a turn that started past it, or a
    /// shape this reader does not know — never read as idle.
    Unknown,
}

/// The payload of one rollout line, when it is an `event_msg`.
fn event_payload(line: &str) -> Option<Value> {
    if !line.contains("event_msg") {
        return None;
    }
    let v: Value = aterm_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    v.get("payload").cloned()
}

/// THE THREAD'S TURN STATE from its rollout's tail: the LAST of
/// `task_started` (busy) and `task_complete` / `turn_aborted` (idle), the
/// shapes 0.145 and 0.157 both write (measured: 0.157 `task_started …
/// turn_aborted` after an Esc; 0.145 `task_started … task_complete`). A
/// line that is not JSON (the tail's cut first line, a last line caught
/// half-written) is skipped.
#[must_use]
pub fn rollout_turn(tail: &str) -> TurnState {
    let mut state = TurnState::Unknown;
    for line in tail.lines() {
        let Some(p) = event_payload(line) else {
            continue;
        };
        match p.get("type").and_then(Value::as_str) {
            Some("task_started") => state = TurnState::Busy,
            Some("task_complete" | "turn_aborted") => state = TurnState::Idle,
            _ => {}
        }
    }
    state
}

/// The text of a USER message on one rollout line: a `response_item`
/// `message` of role `user` (its `input_text` parts), or an `event_msg`
/// `user_message`. The developer and environment rows Codex writes itself are
/// role `developer`, or a `user` row whose text opens `<environment_context>`
/// — not a person's message, and skipped.
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

/// The ASSISTANT's words on one rollout line: a `response_item` `message` of
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

/// WHETHER THE AGENT'S LAST WORDS ARE THE READY ANSWER to the latest notice:
/// the Claude lane's rule ([`super::upgrade::transcript_has_ready`]) over a
/// Codex rollout. A user message that IS a notice ([`ANNOUNCE_HEAD`]) ends
/// any READY before it; each assistant message after it decides afresh — a
/// line of it, trimmed of spaces and backticks, is `marker` — so a READY the
/// agent moved past (it answered again and went back to work) is no consent.
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

/// Codex's composer caret (U+203A), in column 0 (measured 0.157.0/0.157.1:
/// `› Ask Codex to do anything`, the placeholder DIM with the cursor at
/// column 2; a typed draft `› my draft here` in plain attributes).
pub const CARET: char = '›';

/// Rows in column 0 that are the TRANSCRIPT's, not the composer's: an agent
/// block, an error or interrupt, an approval echo, the session card's corners,
/// a tool row's output.
const BLOCK_GLYPHS: &[char] = &['•', '■', '✔', '╭', '╰', '│', '└'];

/// `› 1. Trust and continue`: a choice box's option row, never the composer.
fn is_option_row(row: &str) -> bool {
    let t = row.trim_start().trim_start_matches(CARET).trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    (1..=2).contains(&digits) && t[digits..].starts_with(". ")
}

/// THE COMPOSER ROW: the LAST row with the caret in column 0 that is not a
/// box's option row, with nothing of the transcript under it — only its own
/// draft rows and the footer (model and directory, the key hints), each
/// indented. `None` while a box replaces it, and on a screen Codex left.
#[must_use]
pub fn composer_row(rows: &[String]) -> Option<usize> {
    let c = rows
        .iter()
        .rposition(|r| r.starts_with(CARET) && !is_option_row(r))?;
    let clean = rows[c + 1..].iter().all(|r| {
        let t = r.trim_start();
        t.is_empty() || (!r.starts_with(BLOCK_GLYPHS) && !t.starts_with('└') && r.starts_with(' '))
    });
    clean.then_some(c)
}

/// The composer's text as `(caret row, lines)`: the caret row's text first
/// (the caret stripped; the dim placeholder when nothing is typed — the text
/// cannot tell them apart, the cursor and the cell can), then every row down
/// to the footer (the last run of non-blank rows on the screen), less the
/// blank row that separates the two.
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let c = composer_row(rows)?;
    let last = (c + 1..rows.len())
        .rev()
        .find(|&i| !rows[i].trim().is_empty());
    let end = match last {
        Some(last) => {
            let mut top = last;
            while top > c + 1 && !rows[top - 1].trim().is_empty() {
                top -= 1;
            }
            if top > c + 1 && rows[top - 1].trim().is_empty() {
                top - 1
            } else if top == c + 1 {
                // No blank row between the caret and the run: all of it is
                // the draft's (a footer is always separated by one).
                last + 1
            } else {
                top
            }
        }
        None => c + 1,
    };
    let caret = rows[c].strip_prefix(CARET).unwrap_or(&rows[c]).trim();
    let mut lines = vec![caret.to_string()];
    lines.extend(rows[c + 1..end].iter().map(|r| r.trim().to_string()));
    Some((c, lines))
}

/// Whether Codex's composer holds no TYPED text — the Claude lane's rule
/// ([`super::upgrade::composer_is_empty`]) over Codex's caret: present, and
/// blank, or the placeholder, which is text at column 2 of the caret row with
/// the cursor there AND the cell there drawn DIM (`dim_at_caret`, read with
/// the `cell` verb). Measured 0.157.1: the placeholder's first cell reads
/// `dim`; a typed draft whose caret was moved home (Home) puts the cursor at
/// column 2 too, and its cell reads `none`. A second draft row is typed text
/// whatever the cursor says.
#[must_use]
pub fn composer_is_empty(
    rows: &[String],
    cursor: Option<(usize, usize)>,
    dim_at_caret: bool,
) -> bool {
    let Some((caret, lines)) = composer_draft(rows) else {
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

/// Whether a turn runs on screen: the status row's `esc to interrupt`
/// (measured `• Working (7s • esc to interrupt)`, `• Reconnecting... waiting
/// for network (22s • esc to interrupt)`) — the busy anchor Claude Code's
/// footer shares.
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

/// Whether Codex's status line says a BACKGROUND TERMINAL still runs
/// (measured 0.157.0 2026-09-26, over the composer after a unified-exec
/// `sleep` the turn left running: `  1 background terminal running · /ps to
/// view · /stop to close`): an indented row that OPENS with the count. A
/// transcript row that happens to say so reads the same, and only ever makes
/// the move wait — the kernel's own count ([`DaemonFacts::terminals`], the
/// driver's session leaders) is the proof this backs.
#[must_use]
pub fn terminals_on_screen(rows: &[String]) -> bool {
    rows.iter()
        .any(|row| aterm_phase::codex::is_background_terminal_row(row))
}

// ---------------------------------------------------------------- the words

/// How every Codex notice opens: the supervisor's and the READY reader's tag
/// ([`rollout_has_ready`] resets on it).
pub const ANNOUNCE_HEAD: &str = "[aterm harness] Codex ";

/// THE NOTICE to an EMBEDDED session: one ordinary user turn asking for a
/// good stopping point and naming the one line to answer with. Never asks the
/// agent to cancel anything.
#[must_use]
pub fn prepare_prompt(from: &Version, to: &Version, marker: &str) -> String {
    format!(
        "{ANNOUNCE_HEAD}{to} (managed) is installed; this session runs {from}. To move you onto \
         it, aterm will exit this Codex (/exit) and resume this same conversation in place \
         (codex resume, same tab, same flags). Please get to a good stopping point first: let \
         any commands or background tasks you started finish (do not cancel them), save work \
         in progress, and do not start new long-running work. When nothing of yours is still \
         running, reply with {marker} on a line by itself. If you cannot stop now, say why; \
         aterm will wait and ask again later."
    )
}

/// THE CONTINUATION, typed once the relaunched embedded session holds its
/// thread again.
#[must_use]
pub fn continue_prompt(from: &Version, to: &Version) -> String {
    format!(
        "[aterm harness] Upgraded: this session was restarted on Codex {to} (from {from}) and \
         resumed. {}",
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
    /// session with a conversation. The others hold nothing that ends with
    /// the TUI.
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

/// THE CLIENT'S NEXT STEP: [`super::upgrade::next_step`] for an embedded
/// session with a conversation (the notice, the READY answer, the re-asks
/// and the give-up, word for word the Claude lane's), and for every other
/// client the gate alone — idle, settled, the composer empty, no box, no
/// busy row, no hold, nobody at the tab — then [`Step::Terminate`], which the
/// Codex driver carries out as a typed `/exit`, never a signal. A daemon-mode
/// client also waits for its daemon to be on the build it moves to
/// (`daemon_behind`: the daemon's half goes first, so a relaunched client
/// never attaches to an older server).
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
                Gate::Go => Step::Terminate,
                Gate::Wait(w) => Step::Wait(w),
            }
        }
        Phase::Exiting { .. } | Phase::Relaunched { .. } => Step::Wait("in-flight"),
        Phase::Done => Step::Wait("done"),
        Phase::Failed(_) => Step::Wait("failed"),
    }
}

/// [`next_step`] under the owner's word — [`super::upgrade::requested_step`]'s
/// rule: a skip of THIS target or a deferral not run out holds it; `--now`
/// waives the settling window and the attended-tab guard and nothing else.
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
    if *request == Request::Now {
        let waived = Facts {
            owner_now: true,
            attended: false,
            ..f.clone()
        };
        return next_step(mode, phase, &waived, ready, daemon_behind, now_s);
    }
    next_step(mode, phase, f, ready, daemon_behind, now_s)
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
    /// Threads the daemon holds whose turn runs, or whose state could not be
    /// read ([`TurnState::Busy`], [`TurnState::Unknown`]) — attached to a tab
    /// or not: a detached "Run in background" thread has no TUI, and only the
    /// daemon's own locks reveal it.
    pub busy_threads: usize,
    /// BACKGROUND TERMINALS still running under the daemon: a unified-exec
    /// process a turn left running (`1 background terminal running · /ps to
    /// view`) leads a SESSION of its own under the daemon (measured 0.157.0
    /// 2026-09-26: `sleep 7771`, `sid == pid`, the daemon its parent), where
    /// the daemon's helpers — its MCP servers — stay in the daemon's session.
    /// Its thread reads IDLE (the turn that started it ended), and the update
    /// restarts the daemon and ends it with it (measured, review of
    /// 2026-09-26), so it is running work like a turn is.
    pub terminals: usize,
    /// Idle threads written within [`super::upgrade::QUIET_S`].
    pub settling_threads: usize,
    /// A person gave input within [`super::upgrade::ATTENDED_IDLE`] to a
    /// Codex tab on this home — a tab whose owner said `--now` excepted: that
    /// is the person asking.
    pub attended: bool,
    /// A hold on a Codex tab on this home.
    pub held: bool,
    /// THE OWNER'S WORD on a Codex tab on this home holds this target: a
    /// `--skip` of it, or a `--defer` not yet run out
    /// ([`Request::holds`]). The daemon runs that tab's turns and tools, so
    /// moving it would move the conversation the owner said to keep where it
    /// is (review of 2026-09-26: a `--skip` held the TUI while its daemon
    /// was restarted onto exactly the skipped build).
    pub owner_held: bool,
    /// Codex processes attached to the daemon (its socket's peers) that this
    /// sweep does not list: a TUI in another aterm instance, in a pane, one
    /// whose argv or build could not be read. Nothing was asked of their
    /// tabs — a person at one, a hold, the owner's word — so the daemon under
    /// them is not restarted.
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

/// THE DAEMON RULE: move the daemon onto the managed build — and pin it, which
/// disarms the vendor's own updater — when it runs an OLDER build, or the same
/// build unpinned; never onto an older one (a vendor-updated daemon ahead of
/// atpkg waits, `vendor-ahead`, until atpkg catches up). And only while
/// NOTHING RUNS IN IT — every thread it holds idle and settled, attached or
/// detached, and no background terminal left running under it — no person is
/// at a Codex tab on this home, nothing holds one, the owner's word on none
/// keeps it, and every Codex attached to it is one this sweep sees: the update
/// restarts the daemon ("may interrupt running work", its own help), so
/// running work is waited for, with no deadline, and nothing is ever killed.
#[must_use]
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

/// The daemon's pid from `$CODEX_HOME/app-server-daemon/daemon.pid` (measured
/// 0.157: `{"pid":92181,"processStartTime":…,"processIdentity":{…},
/// "executableIdentity":{…}}`).
#[must_use]
pub fn parse_daemon_pid(text: &str) -> Option<u32> {
    let v: Value = aterm_json::from_str(text).ok()?;
    u32::try_from(v.get("pid")?.as_u64()?)
        .ok()
        .filter(|&p| p > 1)
}

/// What `app-server daemon update --from-cli --yes` answered: its last line,
/// a JSON object whose `status` is `updated` (measured: `{"status":"updated",
/// "managedCodexPath":…,"installedVersion":"0.157.1","runningVersion":
/// "0.157.1","message":…}`) — the running version it names. `Err` with the
/// status, or with `no-answer`.
///
/// # Errors
/// The command answered no JSON, or a status other than `updated`.
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

/// Whether `name` is a rollout file of `thread`: `rollout-<stamp>-<thread>.jsonl`
/// (measured `rollout-2026-09-25T22-35-29-01a0dc36-1dc1-7ee2-be91-11bc323e377c.jsonl`).
/// A compressed rollout (`.jsonl.zst`) is not read.
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

/// THE BACKGROUND TERMINALS UNDER `pid` (a daemon, or an embedded TUI): every
/// descendant in `table` (`(pid, ppid, name)`) that LEADS A SESSION OF ITS
/// OWN by `sid` (the kernel's `getsid`), as `(pid, name)`. Codex runs a
/// unified-exec command in a session of its own, directly under the process
/// that holds its thread (measured 0.157.0 2026-09-26: `sleep 7771`, parent
/// the daemon, `sid == pid`; under an embedded TUI the same), where the
/// helpers it starts itself — an MCP server per thread — stay in its session
/// (`sid` the daemon's, or the tab shell's under an embedded TUI). A shell
/// name says nothing here (the Claude lane's `background` rule): no shell
/// stands between Codex and the command, so that rule never saw it (review
/// of 2026-09-26). A pid whose session cannot be read is counted — it may be
/// one — so an unreadable kernel waits, never moves.
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

/// THE CODEX PROCESSES ATTACHED TO `daemon`, from `lsof -F pdn` over the unix
/// sockets of every process named `codex`: each other pid holding a socket
/// whose peer (`n->0x…`) is one of the daemon's own sockets (`d0x…`) —
/// measured 0.157.0 2026-09-26: the TUI's socket names the daemon's listener
/// at `/private/tmp/codex-daemon-<uid>/<hash>`, and the vendor's updater is
/// no peer. `None` when the daemon itself is not in the listing: nothing
/// read, never "no clients".
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

#[cfg(test)]
#[path = "upgrade_codex_tests.rs"]
mod tests;

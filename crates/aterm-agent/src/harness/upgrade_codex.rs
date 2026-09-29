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
//!   running work continues.` / `Reconnect: codex resume <uuid>` (measured on
//!   0.157, with no turn running); the daemon keeps the thread, and `codex
//!   resume <uuid>` reattaches. No wind-down and no continuation are owed: the
//!   conversation never stopped, and nor does a background terminal it left,
//!   which runs in the daemon (measured: a `sleep` survived the client's
//!   `/exit`). OPEN MEASUREMENT (2026-09-28): Codex 0.158.0's binary holds a
//!   second disconnect string beside that one, `Disconnected from this task.
//!   The current turn was stopped.` — found in the binary, never seen printed;
//!   which case prints it (an `/exit` while the client's own turn runs, a
//!   goal's turn, another setting) is unmeasured. Until it is, an `/exit` is
//!   never typed while the client's OWN conversation runs a turn in the
//!   daemon ([`daemon_turn`], the gate's running-work floor): the one the
//!   kernel names as its own ([`own_thread`]: every thread of the daemon one
//!   root's spawn tree — that thread and the subagents it spawned,
//!   [`Lineage`] — and this TUI its one client), or a running thread this
//!   tab's screen has not placed in another session ([`still_through`]) —
//!   which only a root is, only with another Codex attached to the daemon,
//!   and never while this screen's footer shows a goal being pursued. A live
//!   lane that measures it — an `/exit` typed mid-turn, mid-goal, and while a
//!   subagent runs — is owed (`tools/test-codex-live-upgrade.sh` has none).
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
//! THE GOAL PAUSE (the owner's decision of 2026-09-28: "Pause the goal
//! briefly — once the upgrade is due, aterm types `/goal pause` (or presses
//! Esc the instant a new goal turn starts, before it has done anything),
//! moves Codex onto the new build, then types `/goal resume`. The goal carries
//! on where it was; no running tool call is ever cut off."). A goal-mode
//! Codex starts its next turn within 14 ms of the last one's end (89 of 90 in
//! the owner's rollout), so no turn end is ever a pause to move it in:
//! [`goal_step`] makes one. WHY `/goal pause` IS TYPED, AND NOT AN ESC, read
//! from the vendor's source at the tags of both store builds (openai/codex
//! `rust-v0.157.1`, `rust-v0.158.0`; the source matched to the binaries by
//! the tracing sites compiled into both, `ext/goal/src/runtime.rs:450` among
//! them, and by their strings):
//!
//! * `/goal` is taken WHILE A TURN RUNS: `tui/src/slash_command.rs`
//!   `available_during_task` lists `SlashCommand::Goal => true` (its test
//!   `certain_commands_are_available_during_task` asserts it), so
//!   `slash_command_blocked_by_active_task` never refuses it. Enter is the
//!   composer's `submit` key (`keymap.rs`: `plain(KeyCode::Enter)`; Tab is
//!   `queue`), which submits unqueued once the session is configured
//!   (`queue_submissions` is set only until then) through
//!   `try_dispatch_slash_command_with_args` — `/goal pause` is dispatched at
//!   once, never queued behind the turn and never steered into it.
//! * It STOPS THE NEXT TURN, NOT THE RUNNING ONE: `slash_dispatch.rs` sends
//!   `"pause" => SetThreadGoalStatus(Paused)`, a `thread/goal/set` request
//!   (`app/thread_goal_actions.rs`), and nothing that interrupts; the server
//!   applies it by `apply_external_goal_set`, whose `Paused` arm only clears
//!   the goal's active accounting (`ext/goal/src/runtime.rs`); the goal's
//!   next turn is started by `on_thread_idle` → `continue_if_idle`, which
//!   returns without a turn when the goal is not `Active`. So the turn
//!   running when it is typed finishes whatever it does — its tool calls
//!   included — and no turn follows it.
//! * `/goal resume` is `SetThreadGoalStatus(Active)`: `continue_if_idle`
//!   starts the goal's continuation at once on an idle thread, and after the
//!   running turn otherwise.
//! * A PAUSED goal taken back by `codex resume <thread>` with no prompt draws
//!   a box first (`app/startup.rs`, `should_prompt_for_paused_goal_after_
//!   startup_resume`; `chatwidget/goal_menu.rs` `show_resume_paused_goal_
//!   prompt`): `Resume paused goal?`, `Resume goal` focused — which sends the
//!   same `SetThreadGoalStatus(Active)` — over `Leave paused`. After the move
//!   the relaunched Codex opens on it, and the upgrade answers it with the
//!   resume (`aterm_phase::codex::resume_box`), or types `/goal resume` where
//!   no box came.
//! * Codex RECORDS each change in the rollout (MEASURED read-only in the
//!   owner's rollout, 2026-09-28: `event_msg` `thread_goal_updated`,
//!   `"status":"paused"` at the owner's Esc of 15:01:13Z, `"active"` at his
//!   `codex resume` of 15:27:29Z), and draws it on the footer (`Goal paused
//!   (/goal resume)`, `Pursuing goal (…)`), which is what the upgrade reads.
//!
//! The Esc is the FALLBACK, for a pause that never shows: pressed only at a
//! goal turn's HEAD ([`TurnState::Head`]: its rollout holds nothing of the
//! turn's own work yet), read again just before the key, the composer free
//! and nobody typing — Esc interrupts the turn and pauses the goal in the
//! same instant (the owner's rollout: `turn_aborted`, then `thread_goal_
//! updated … paused`). What the Esc can still meet, stated: the model's first
//! item arrives 4 to 10 s after a goal turn starts (measured), and the head
//! is read within a second of the key, not at it.
//!
//! The goal is the upgrade's to resume ONCE: its hold is on the tab's one
//! goal record ([`super::goal_hold`]), written before every key, shared with
//! the supervisor's save-then-wait switch — which never holds the same goal
//! at the same time, and an open switch keeps the pause off.
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
//! The composer is read by the one reader every lane reads Codex by
//! (`aterm_phase::codex`: both of its input line's marks, `›` and 0.158.0's
//! `»`); the exit hint is this lane's own, read off the TUI's own last rows.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aterm_json::Value;

use aterm_phase::codex::CodexGoal;

use super::goal_hold::Hold;
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
    // The model is CARRIED, never moved (2026-09-26): the Claude lane's
    // same-family move (`upgrade_models`) has no Codex counterpart. It rests
    // on the installed build's own catalog, and aterm reads no Codex catalog
    // (the vendor's `models_cache.json` is a network cache with its own
    // `upgrade` pointer per model, which Codex acts on itself); and a
    // daemon-mode client's thread keeps running in the daemon across the
    // relaunch, where what a `-m` on `codex resume` does to it is unmeasured.
    // So nothing is added, and a launch's model is kept verbatim: never
    // guessed, never down.
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

/// The thread a TUI's argv RESUMES (`codex [flags] resume [flags] <id>`): the
/// first positional after the `resume` subcommand, when it is a thread id
/// ([`is_thread_id`]). `None` for a plain launch, `resume --last` or a
/// picker (`resume` with no id), or a `fork` (a new thread).
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

/// When a thread was CREATED, in unix milliseconds, read off its id: Codex's
/// thread ids are UUIDv7 (measured 0.157.1, 2026-09-27: `01a0e4bd-2b37-75a0-…`
/// is 2026-09-27T21:19:57.751Z, the second after its TUI started), whose
/// first 48 bits are the creation instant. `None` for an id that is no
/// version-7 UUID — a changed id scheme reads as unknown, never a guess.
#[must_use]
pub fn thread_created_ms(thread: &str) -> Option<u64> {
    if !is_thread_id(thread) || thread.as_bytes().get(14) != Some(&b'7') {
        return None;
    }
    let hex: String = thread.split('-').take(2).collect();
    u64::from_str_radix(&hex, 16).ok()
}

/// The working directory a rollout's conversation began in: its first line's
/// `session_meta` (`payload.cwd`, measured 0.157.1). `None` for any other
/// first line.
#[must_use]
pub fn rollout_cwd(first_line: &str) -> Option<String> {
    let v: Value = aterm_json::from_str(first_line.trim()).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    v.get("payload")?.get("cwd")?.as_str().map(str::to_string)
}

/// Whether a Codex TUI's exit status, as its shell recorded it (the shell
/// integration's command block, `exit`), was a CRASH — no one's decision:
/// killed by a signal nobody sends to end a program on purpose (SIGKILL's
/// 137, the one an OOM kill uses; a SIGSEGV, SIGABRT, SIGBUS), or a failure
/// status. `0` is its own orderly exit (a person's `/exit` or double ctrl-c,
/// an orchestrator's typed `/exit`: measured 0 on 0.157.1, the exit hint
/// printed), and SIGHUP, SIGINT and SIGTERM (129, 130, 143) are someone's
/// signal to end it — the parity of Claude Code, whose own handler removes
/// its session record on those and never on SIGKILL.
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
    /// THE TURN'S HEAD: the last turn event is `task_started`, and nothing of
    /// the turn's OWN WORK follows it — only what the turn opened with
    /// ([`head_part`]: its `turn_context`, the messages it began with, token
    /// counts). Read by the head readers alone ([`rollout_turn_head`],
    /// [`rollout_head_back`]); [`rollout_turn`] and [`rollout_turn_back`] read
    /// such a turn [`TurnState::Busy`]. Measured 2026-09-28 read-only in the
    /// owner's goal-mode rollout: a goal turn opens with `task_started`,
    /// `turn_context` and the goal's `<codex_internal_context source="goal">`
    /// user message within 10 ms, and its first work — a reasoning item, an
    /// assistant message, a tool call — lands 4 to 10 s later. Only here may
    /// an Esc stop a turn without cutting off anything it did (the upgrade's
    /// goal pause, [`goal_step`]).
    Head,
    /// No turn event in what was read — a tail ([`rollout_turn`]), or the
    /// rollout as far back as it is read ([`rollout_turn_back`]): a turn
    /// that started before that, or a shape this reader does not know —
    /// never read as idle.
    Unknown,
}

/// Whether one rollout line is PART OF A TURN'S HEAD — what a turn opens with,
/// no work of its own ([`TurnState::Head`]): its `turn_context`, its
/// `world_state`, a
/// `response_item` `message` of role `user` or `developer` (the goal's
/// continuation, the environment context, the instructions), an `event_msg`
/// that is a `user_message`, a `token_count` or a `thread_goal_updated`, and
/// a `token_usage_record`. Anything else — a reasoning item, an assistant
/// message, any call or its output, an `item_completed`, a line that is not
/// JSON or is of a shape this reader was not written for — is the turn's
/// work: never a head.
#[must_use]
pub fn head_part(line: &str) -> bool {
    let Ok(v) = aterm_json::from_str::<Value>(line.trim()) else {
        return false;
    };
    let payload = |k: &str| {
        v.get("payload")
            .and_then(|p| p.get(k))
            .and_then(Value::as_str)
    };
    match v.get("type").and_then(Value::as_str) {
        // `world_state`: the environment's state a turn opens with, written
        // between its opening messages and its `turn_context` (0.158.0,
        // measured read-only in the owner's rollouts, 2026-09-28: 15:28:29.649Z
        // between 15:28:29.648Z's message and 15:28:29.650Z's
        // `turn_context`; and after a `compacted`).
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

/// THE THREAD'S TURN, ITS HEAD TOLD APART, from a text of its rollout (a
/// tail): [`rollout_turn`]'s answer, with a running turn that has done
/// nothing yet read [`TurnState::Head`] ([`head_part`]). A blank line is
/// passed by; a line that is neither a turn event nor part of a head is
/// work, and a cut or half-written line is too — so a head is read only
/// where every line after `task_started` was read whole.
#[must_use]
pub fn rollout_turn_head(tail: &str) -> TurnState {
    let mut worked = false;
    scan_head(tail, &mut worked).unwrap_or(TurnState::Unknown)
}

/// [`rollout_turn_head`]'s scan of `text`, newest line first, `worked`
/// carried across the texts of one read (the chunks [`rollout_head_back`]
/// reads back): the state of the last turn event, or `None` where `text`
/// holds none.
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
    let v: Value = aterm_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    v.get("payload").cloned()
}

/// THE THREAD'S TURN STATE from a text of its rollout (a tail — the file
/// itself is read back from its end by [`rollout_turn_back`], which reads
/// each chunk so): the LAST of `task_started` (busy) and `task_complete` /
/// `turn_aborted` (idle), the shapes 0.145 and 0.157 both write (measured:
/// 0.157 `task_started … turn_aborted` after an Esc; 0.145 `task_started …
/// task_complete`). A line that is not JSON (the tail's cut first line, a
/// last line caught half-written) is skipped.
#[must_use]
pub fn rollout_turn(tail: &str) -> TurnState {
    tail.lines()
        .rev()
        .find_map(turn_event)
        .unwrap_or(TurnState::Unknown)
}

/// The turn event one rollout line is, if it is one: an `event_msg` whose
/// payload type starts a turn (busy) or ends one (idle). Parsed only when it
/// holds one of those types' words ([`TURN_EVENTS`]), so a long line of
/// anything else costs a byte search.
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

/// A ROLLOUT'S LAST TURN EVENT, READ BACK FROM ITS END `chunk` bytes at a
/// time, no further than `bound` bytes from it: [`rollout_turn`] over as
/// much of the rollout (`len` bytes of `file`) as its caller bounds, where a
/// tail alone can hold no turn event while the thread idles. Measured
/// read-only on the owner's live 3 GB root rollout (Codex 0.158.0, the fourth
/// review of 2026-09-28): a turn ended (`task_complete`,
/// 2026-09-27T20:45:24Z), a command that finished after it wrote ONE
/// 542,736-byte `item_completed` line, and nothing but settings rows
/// followed until the next `task_started` six hours later — read by its
/// 256 KiB tail alone the idle conversation was Unknown, counted running,
/// and held its tab `wait:daemon-turn` all that time.
///
/// Each chunk read ENDS where the one read before it began, and the line a
/// chunk's start cuts is carried into the next one read, whole
/// ([`TurnBack`]): a turn event is read wherever the chunks cut it, and a
/// line longer than a chunk is carried until its start is read. A line is
/// parsed only when it holds a turn event's word ([`turn_event`]). A line
/// the bound cuts is never read, and past the bound the state stays
/// [`TurnState::Unknown`] — counted running — as it does where `file`
/// cannot be read whole.
#[must_use]
pub fn rollout_turn_back<R: Read + Seek>(
    file: &mut R,
    len: u64,
    chunk: u64,
    bound: u64,
) -> TurnState {
    read_back(file, len, chunk, bound, TurnBack::default())
}

/// [`rollout_turn_back`] with the turn's HEAD told apart
/// ([`rollout_turn_head`]): a running turn none of whose lines since its
/// `task_started` is its own work reads [`TurnState::Head`] — the one point
/// the upgrade's goal pause may press Esc at ([`goal_step`]). The same
/// chunks, bound and carry; a line the bound cuts, or one not read whole, is
/// no head.
#[must_use]
pub fn rollout_head_back<R: Read + Seek>(
    file: &mut R,
    len: u64,
    chunk: u64,
    bound: u64,
) -> TurnState {
    read_back(
        file,
        len,
        chunk,
        bound,
        TurnBack {
            head: Some(false),
            ..TurnBack::default()
        },
    )
}

/// The read-back both readers share: `back` fed the chunks of `file` from its
/// end, `chunk` bytes at a time, no further than `bound` from it.
fn read_back<R: Read + Seek>(
    file: &mut R,
    len: u64,
    chunk: u64,
    bound: u64,
    mut back: TurnBack,
) -> TurnState {
    let floor = len.saturating_sub(bound);
    let mut end = len;
    while end > floor {
        let start = end.saturating_sub(chunk.max(1)).max(floor);
        let Ok(n) = usize::try_from(end - start) else {
            return TurnState::Unknown;
        };
        let mut bytes = vec![0; n];
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut bytes).is_err() {
            return TurnState::Unknown;
        }
        if let Some(turn) = back.feed(bytes, start == 0) {
            return turn;
        }
        end = start;
    }
    TurnState::Unknown
}

/// [`rollout_turn_back`]'s carry between the chunks it reads: the one line
/// not yet whole — its end, read, and its start still in a chunk to come —
/// kept as the PIECES read of it, the newest read (the earliest in the file)
/// last. A piece is moved in, never copied; the line is joined ONCE, when
/// the chunk that holds its start arrives. So a line longer than a chunk
/// costs its length, not its length squared over the chunk (the fourth
/// review of 2026-09-28: the carry was re-copied, and re-scanned for its
/// first newline, at every chunk — an 8 MiB line read 256 KiB at a time
/// copied about 130 MiB).
#[derive(Debug, Default)]
struct TurnBack {
    pieces: Vec<Vec<u8>>,
    /// The head reader's memory ([`rollout_head_back`]): `Some(worked)` —
    /// whether a line read so far (a later one in the file) was the turn's
    /// own work; `None` for the turn reader, which tells no head apart.
    head: Option<bool>,
}

impl TurnBack {
    /// Feeds `chunk`, the bytes that end where the chunk fed before began
    /// (the first: the file's end), `from_start` when it begins the file.
    /// The state of the LAST turn event among the lines it completes, or
    /// `None`: none there — read further back.
    fn feed(&mut self, chunk: Vec<u8>, from_start: bool) -> Option<TurnState> {
        // Where this chunk's whole lines begin: past the line its start
        // cuts, unless it starts the file. A chunk with no newline is all
        // one line's middle: carried on, nothing completed.
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
        let turn = match &mut self.head {
            Some(worked) => scan_head(&text, worked),
            None => text.lines().rev().find_map(turn_event),
        };
        let mut cut = chunk;
        cut.truncate(whole);
        self.pieces.push(cut);
        turn
    }

    /// The bytes carried: the line not yet whole (tests: the carry is moved
    /// in, never grown by a copy).
    #[cfg(test)]
    fn carried(&self) -> usize {
        self.pieces.iter().map(Vec::len).sum()
    }
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

// THE COMPOSER IS READ BY ONE READER, `aterm_phase::codex` (`composer`,
// `composer_draft`, `caret_on`, the input line's marks `CARETS`): the screen
// grammar every other lane reads Codex by. This lane kept a copy of its own
// until 2026-09-28, which knew `›` alone; Codex 0.158.0 draws its input line
// `»`, and the copy — like the reader it copied — found no composer there.

/// Whether Codex's composer holds no TYPED text — the Claude lane's rule
/// ([`super::upgrade::composer_is_empty`]) over Codex's input line as the one
/// reader reads it ([`aterm_phase::codex::composer_draft`], either mark):
/// present, and blank, or the placeholder, which is text at column 2 of the
/// caret row with the cursor there AND the cell there drawn DIM
/// (`dim_at_caret`, read with the `cell` verb). Measured 0.157.1: the
/// placeholder's first cell reads `dim`; a typed draft whose caret was moved
/// home (Home) puts the cursor at column 2 too, and its cell reads `none`;
/// measured 0.158.0 (2026-09-28): the `»` line's placeholder reads `dim` at
/// column 2 the same. A second draft row is typed text whatever the cursor
/// says.
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

/// Whether Codex's footer says a GOAL is being pursued (measured 0.158.0,
/// 2026-09-28: `… · Main [default]   Pursuing goal (10d 3h 14m)` on the
/// row under the input line): Codex's ONE goal reader,
/// [`aterm_phase::codex::goal_state`] — the footer's status row
/// ([`aterm_phase::codex::footer_status`]: the row at column 2 with a ` · `
/// in the screen's last run of rows, the one under the input line), read
/// through the anchor `codex.goal.pursuing` — naming
/// [`aterm_phase::codex::CodexGoal::Pursuing`]; the supervisor's
/// save-then-wait switch reads the same (`CodexReader::goal_active`). A
/// transcript row that says so is never the footer, and a box up reads no
/// goal. A paused, stalled or usage-limited goal is none being pursued: its
/// next turn does not start by itself.
#[must_use]
pub fn goal_on_screen(rows: &[String]) -> bool {
    aterm_phase::codex::goal_state(rows) == Some(aterm_phase::codex::CodexGoal::Pursuing)
}

// ---------------------------------------------------------------- the goal pause

/// How long a pause is given to show on the footer (`Goal paused (/goal
/// resume)`) before the next way of pausing is tried — the Esc at a goal
/// turn's head ([`GoalStep::Esc`]). Codex writes the goal's new state and
/// redraws its footer as it answers the `thread/goal/set` the command sends.
pub const PAUSE_TAKE_S: u64 = 60;

/// How long, from the first pause, a pause that never showed is tried before
/// aterm gives it up ([`GoalStep::GiveUp`]: said once, the goal holding its
/// tab as before the owner's decision, the ladder's floor).
pub const PAUSE_GIVE_UP_S: u64 = 30 * 60;

/// How long a goal aterm paused may stay paused without the move — its last
/// turn still running, or another floor holding the tab (a draft, a box, a
/// keystroke, the daemon's other work) — before aterm resumes it anyway
/// ([`GoalStep::Resume`] `bound`): a pause is for a moment, and the goal
/// never waits on the upgrade for long.
pub const GOAL_HOLD_BOUND_S: u64 = 60 * 60;

/// How long a resume is given to show on the footer before it is made again
/// ([`GoalStep::Resume`] `again`).
pub const RESUME_TAKE_S: u64 = 60;

/// How many resumes one hold makes at most; past them the goal left paused
/// is said, with the one hand step ([`GoalStep::Wait`] `goal-left-paused`).
pub const MAX_RESUMES: u32 = 3;

/// How long after a hold that ended WITHOUT its move — resumed at its bound
/// or because the move was abandoned, released to a person's hand, or a
/// pause that never took — no new pause is made: the round's rest
/// ([`super::upgrade::RETRY_S`]), so a goal whose move keeps failing is not
/// paused and resumed at every look.
pub const GOAL_REST_S: u64 = super::upgrade::RETRY_S;

/// ONE LOOK AT A CODEX GOAL, for the upgrade's pause ([`goal_step`]): what
/// the visit read at an idle point of a Codex tab, or at the relaunched
/// Codex's first moments.
#[derive(Clone, Debug)]
pub struct GoalLook<'a> {
    /// The tab's goal hold ([`super::goal_hold`]), of either owner.
    pub hold: Option<&'a Hold>,
    /// The Codex on screen is still behind the build it is moved to: a pause
    /// is made only for a move still owed (a current Codex's look only ever
    /// resumes).
    pub behind: bool,
    /// The goal the footer names ([`aterm_phase::codex::goal_state`]).
    pub goal: Option<CodexGoal>,
    /// Whether a footer was read at all ([`aterm_phase::codex::footer_status`]):
    /// no footer (a box stands where it was) says nothing of the goal.
    pub footer: bool,
    /// The ladder's Land rung is in force — two hours behind, or the owner's
    /// `--now` ([`super::upgrade::rung_in_force`]): the pause is made there
    /// and never at first sight.
    pub land: bool,
    /// What holds the move is the GOAL'S OWN TURN: a daemon-mode client's
    /// ([`super::upgrade::DaemonTurn::Goal`]: its own conversation, named by
    /// the kernel, runs a turn under the goal its footer shows), or an
    /// embedded conversation's whose rollout runs a turn under that goal.
    pub goal_holds: bool,
    /// The gate with that turn waived ([`super::upgrade::gate_announce`]):
    /// `Go` where the goal's turn is the ONLY thing holding the move — a
    /// pause bought for anything else would buy nothing.
    pub waived: Gate,
    /// A save-then-wait switch is open on the tab (its ledger's open
    /// wind-down): the session is the switch's, and no pause is made.
    pub switch: bool,
    /// The move is made: the Codex on screen is not the one the pause went
    /// into, and runs the build the move was for.
    pub moved: bool,
    /// The move will not come now, and why: the round stopped (`failed`), the
    /// owner's `--skip` or `--defer`, the Codex paused no longer behind or no
    /// longer in the tab.
    pub abandoned: Option<&'static str>,
    /// The goal thread's rollout, its HEAD told apart
    /// ([`rollout_head_back`]): `Some(true)` a turn at its head, `Some(false)`
    /// anything else read, `None` not read (read only where an Esc is
    /// weighed).
    pub head: Option<bool>,
    /// The composer takes a line: no box, no turn's status row, nothing
    /// typed in it (the typing fence asks it again, fresh, before any key).
    pub free: bool,
    /// An Esc would reach the goal's turn and nothing else: no box, nothing
    /// typed in the composer — the turn's own status row (`Working (… • esc
    /// to interrupt)`) may stand, as it does for the whole of a turn's head
    /// (the goal-pause review of 2026-09-28: the Esc was weighed on
    /// [`GoalLook::free`], which that row makes false, so it could never be
    /// pressed at a head). The rollout's head is the guard ([`GoalLook::head`]).
    pub esc_free: bool,
    /// The goal's thread FELL INTO A SANDBOX its launch bypassed
    /// (`supervise::codex_usage::sandbox_fell`, read only where a resume
    /// without the move is weighed): nothing resumes the goal there — it
    /// would go on inside the sandbox, which cannot commit or push (the
    /// owner: "not continue work in a sandbox"). A resume after the move is
    /// made all the same: the relaunch carries the launch's own flags, which
    /// bring the thread back out of it (measured in the owner's rollout,
    /// 2026-09-28 15:27:29Z: `danger-full-access` again after a resume with
    /// the bypass flag).
    pub sandboxed: bool,
    /// A person typed within [`super::upgrade::KEYS_GAP_S`]: the floor no
    /// rung and no word waives.
    pub typing: bool,
    /// A person's hand since the hold's last edge — its pause, or its
    /// resume ([`Hold::last_at`]); an unknown stamp counts as one.
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
    /// Type `/goal resume` — or, at the relaunched Codex's first moments,
    /// press its box's `Resume goal` — and why: `moved`, `abandoned:<why>`,
    /// `bound` ([`GOAL_HOLD_BOUND_S`]), `again` (a resume that did not show).
    Resume(&'static str),
    /// The resume shows on the footer: nothing owed.
    Resumed,
    /// The goal is no longer aterm's to resume, and why: `by-hand` (a
    /// person's hand on it since aterm's last edge, or the goal pursued
    /// again with no resume of aterm's), `changed` (the footer shows it
    /// neither paused nor pursued).
    Release(&'static str),
    /// The pause never showed within [`PAUSE_GIVE_UP_S`]: given up, said
    /// once; the goal holds its tab as a running turn does.
    GiveUp,
}

/// Why a goal is resumed without the move where the Codex paused is gone and
/// the one in the tab now is no relaunch of aterm's: the person restarted it
/// ([`GoalLook::abandoned`]) — a hand on the tab since then makes the goal
/// theirs ([`goal_step`]).
pub const BY_HAND: &str = "abandoned:by-hand";

/// The note a hold carries once the relaunched Codex's paused-goal box was
/// left to a person at the keys ([`Hold::say`]): their answer to it, since,
/// is theirs ([`goal_step`]).
pub const BOX_THEIRS: &str = "box-theirs";

/// Whether a hold that ended keeps the next pause off ([`GOAL_REST_S`]): it
/// ended without its move, within the rest.
fn resting(h: &Hold, now: u64) -> bool {
    goal_rest_until(h, now).is_some()
}

/// Until when a hold of the upgrade's that ended WITHOUT its move keeps the
/// next pause off ([`GOAL_REST_S`] past its last edge); `None` for a hold
/// still owed, one that ended with its move, or a rest run out.
#[must_use]
pub fn goal_rest_until(h: &Hold, now: u64) -> Option<u64> {
    let until = h.last_at().saturating_add(GOAL_REST_S);
    (h.owner == super::goal_hold::Owner::Upgrade && !h.owes() && h.why != "moved" && now < until)
        .then_some(until)
}

/// Whether the goal the hold `h` paused has stayed paused past the hold's
/// bound at `now` ([`GOAL_HOLD_BOUND_S`] from the later of the pause made
/// and the pause seen): the resume is due without the move ([`goal_step`]'s
/// `bound`). The one reading of that clock — the goal pause's model projects
/// its `overdue` through it too (`upgrade_drive::goal_projection`).
#[must_use]
pub fn goal_hold_past_bound(h: &Hold, now: u64) -> bool {
    now.saturating_sub(h.took_at.max(h.at)) >= GOAL_HOLD_BOUND_S
}

/// How long a goal the upgrade paused may owe its resume before it is said
/// to a person as LEFT PAUSED, with the one hand step (`/goal resume` in the
/// tab): the hold's bound ([`GOAL_HOLD_BOUND_S`], where the resume is due
/// even with no move) and half an hour of idle points past it, none of which
/// could take the resume — the Codex gone, a person's box, a screen that
/// cannot be read.
pub const GOAL_LEFT_AFTER_S: u64 = GOAL_HOLD_BOUND_S + 30 * 60;

/// Since when the goal the upgrade's hold `h` paused counts as LEFT PAUSED
/// at `now`: owed its resume past [`GOAL_LEFT_AFTER_S`] from its pause, or
/// its resumes spent ([`MAX_RESUMES`]) and the last one not seen within
/// [`RESUME_TAKE_S`]. `None` otherwise — a hold not the upgrade's, one no
/// longer owed, one within its time.
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

/// THE UPGRADE'S GOAL PAUSE (the owner's decision of 2026-09-28: "Pause the
/// goal briefly — once the upgrade is due, aterm types `/goal pause` (or
/// presses Esc the instant a new goal turn starts, before it has done
/// anything), moves Codex onto the new build, then types `/goal resume`. The
/// goal carries on where it was; no running tool call is ever cut off.").
/// Over one look ([`GoalLook`]):
///
/// * NO HOLD OF THE UPGRADE'S OWED: a goal the footer shows PURSUED, its own
///   turn the only thing holding the move (`goal_holds`, `waived` open), at
///   the Land rung (or `--now`), its Codex still behind and its move not
///   held by the owner's `--skip` or `--defer`, no switch open, no hold of
///   the switch's, no
///   rest after a hold that ended without its move, the composer free and
///   nobody typing — [`GoalStep::Pause`]: `/goal pause`, which Codex takes
///   while a turn runs and which stops the goal's NEXT turn, never the
///   running one (module header, "THE GOAL PAUSE"). Anything else passes.
/// * PAUSING (made, not seen): the footer shows it paused — taken, unless a
///   person's hand has been on the tab since (the pause may be theirs: it is
///   released to them). Still pursued: a person's hand since releases it;
///   within [`PAUSE_TAKE_S`] it waits; past [`PAUSE_GIVE_UP_S`] from the
///   first pause it is given up; else, a typed pause that never showed, the
///   goal's turn at its HEAD, no box, nothing typed and nobody typing (the
///   turn's own status row may stand, [`GoalLook::esc_free`]) — the ONE Esc
///   ([`GoalStep::Esc`]).
/// * PAUSED (taken): pursued again with no resume of aterm's — released, the
///   goal a person's again (or anyone's but aterm's). Still paused: once the
///   move is made, or abandoned, or past [`GOAL_HOLD_BOUND_S`], the resume —
///   with the composer free and nobody typing, else a wait — unless the
///   relaunched Codex's box was left to a person at the keys, or the Codex
///   in the tab is one the person restarted themselves ([`BY_HAND`]), and
///   their hand has been on the tab since ([`BOX_THEIRS`]: `Leave paused` is
///   theirs to choose), which releases it; a resume WITHOUT the move into a
///   thread fallen into a sandbox is withheld ([`SANDBOXED`]: the goal stays
///   paused, and is said); before that, the ordinary step
///   ([`GoalStep::Hold`]).
/// * RESUMING (made, not seen): pursued — resumed. Still paused: a person's
///   hand since releases it (theirs to resume); within [`RESUME_TAKE_S`] it
///   waits; past [`MAX_RESUMES`], `goal-left-paused` (said, with the hand
///   step); else the resume once more (`again`).
/// * A footer that names the goal neither paused nor pursued — stopped at a
///   usage limit, stalled, achieved (its last turn may complete it) or unmet,
///   or (a person's hand on the tab since the hold's last edge) no goal at
///   all — while a hold is owed: released (`changed`).
///   No footer read, or one that names no goal with nobody's hand since:
///   nothing decided (a wait while owed; the ordinary step for a hold only
///   paused) — a goal aterm paused leaves that state only by a key or a
///   turn, and a look that read none never drops the resume it owes.
/// * The SWITCH's hold: nothing of the upgrade's (the switch owns that goal);
///   and while a switch is open, nothing is typed for the upgrade's own hold
///   either — no Esc, no resume (`switch`): its records go on.
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
    // A SWITCH OPEN on the tab owns the session: nothing is typed into it
    // for the upgrade's goal — no Esc, no resume (a goal resumed under the
    // switch would run on the cheaper model); the record's own edges go on.
    if g.switch && matches!(step, GoalStep::Esc | GoalStep::Resume(_)) {
        return GoalStep::Wait("switch");
    }
    step
}

/// Why a resume WITHOUT the move is withheld: the goal's thread fell into a
/// sandbox its launch bypassed ([`GoalLook::sandboxed`]) — the goal stays
/// paused, and the tab's row says so with the fix (quit Codex and resume the
/// thread with its launch flags).
pub const SANDBOXED: &str = "goal-sandboxed";

/// [`goal_step`] for a hold of the upgrade's that owes the goal its resume.
fn owed_step(g: &GoalLook, h: &Hold) -> GoalStep {
    use super::goal_hold::{How, Stage};
    let now = g.now;
    // A LOOK THAT READ NO GOAL decides nothing about it: no footer (a box
    // stands where it was), or a footer that names no goal with nobody's
    // hand on the tab since the hold's last edge — a goal aterm paused
    // leaves its paused state only by a key (a person's `/goal clear`, a
    // thread of their own) or by a turn, which names its end (`Goal
    // achieved`, `Goal unmet`: [`CodexGoal::Achieved`], [`CodexGoal::Unmet`],
    // released below), and a paused goal starts none. The
    // goal-pause review of 2026-09-28 met the other reading: in a narrow pane
    // the key-hint row was read as the status row, the goal as none, and the
    // hold released `changed` with its resume still owed — no row, and the
    // relaunched Codex's box escalated as a goal aterm did not pause.
    if !g.footer || (g.goal.is_none() && !g.person_since) {
        return match h.stage {
            Stage::Pausing => GoalStep::Wait("goal-pausing"),
            Stage::Resuming => GoalStep::Wait("goal-resuming"),
            _ if g.moved || g.abandoned.is_some() => GoalStep::Wait("goal-resume"),
            _ => GoalStep::Hold,
        };
    }
    match (h.stage, g.goal) {
        // The pause shows — but with a person's hand on the tab since it
        // was typed, it may be theirs (their Esc into the last turn pauses
        // the goal too, or their own `/goal pause`): never claimed, never
        // resumed over them (the review of 2026-09-28).
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
                // The relaunched Codex's box was left to a person at the keys
                // (`upgrade_codex_drive::resume_on_relaunch`), and their hand
                // has been on it since: its answer was theirs — `Leave
                // paused` among them — and aterm resumes nothing over it.
                // So is a Codex the person restarted themselves, in place of
                // the one paused (`abandoned:by-hand`).
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

/// Whether the tab whose goal hold is `hold` must still be LOOKED AT for it —
/// the upgrade's hold owes the goal its resume — whatever else its upgrade
/// says: the host's `due` ([`super::upgrade_drive::due`]) keeps a tab due
/// while this holds, so the host never lets a tab go with its goal left
/// paused by aterm (a move made, the Codex current, and nothing else owed).
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

/// WHERE A THREAD THE DAEMON HOLDS SITS IN ITS CONVERSATION'S SPAWN TREE, by
/// the first line of its rollout, `session_meta` ([`session_lineage`]).
/// Measured 2026-09-28, read-only, on the owner's live Codex 0.158.0 daemon:
/// its four writer locks were ONE conversation — the `cli` thread the one TUI
/// attached to it had resumed (`"source":"cli"`) and three subagents that
/// thread spawned, each `"source":{"subagent":{"thread_spawn":
/// {"parent_thread_id":"<that thread>","depth":1,…}}}`, their turns running
/// on their own after the spawn and never drawn on the TUI's main screen.
/// Across that home's 788 rollouts every one of the 785 subagents has that
/// shape (depth 1 to 3); its direct parent is named there. So a lock is not
/// a conversation: counting locks, the lane named none of the owner's, and
/// took a running subagent of the tab's own conversation for another
/// session's (the third review of 2026-09-28).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lineage {
    /// A conversation's own thread: `source` a plain word (`"cli"`, measured;
    /// Codex's other launch sources are words too).
    Root,
    /// A subagent spawned by the thread `parent` (`source.subagent.
    /// thread_spawn.parent_thread_id`, a thread id).
    Spawned(String),
    /// A subagent whose parent is not named (`source.subagent` of any other
    /// shape — a review's, a compaction's, a spawn written differently): a
    /// thread of SOME conversation, never a root.
    Subagent,
    /// No `session_meta` read: no rollout yet (a thread with no message), a
    /// first line past the bound or not JSON, or a shape this reader was not
    /// written for. Neither a root nor anyone's subagent, proven.
    Unread,
}

/// A THREAD'S LINEAGE from its rollout's FIRST line (`session_meta`, module
/// header of [`Lineage`]). A line that is not a `session_meta`, not JSON, or
/// a `source` of a shape not written for is [`Lineage::Unread`] — never a
/// guess.
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

/// One thread the daemon holds (its writer lock), as one read of it found
/// it: its id, its turn state by its rollout's last turn event (read back
/// from its end, [`rollout_turn_back`]; a state not read,
/// [`TurnState::Unknown`], counts as running) and its place in its
/// conversation's spawn tree ([`Lineage`]).
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

/// THE ROOT OF `thread`'S SPAWN TREE among the threads the daemon holds
/// (`threads`): its parent chain followed through loaded threads to a
/// [`Lineage::Root`]. `None` where the chain leaves the daemon (a parent it
/// does not hold), meets a subagent that names no parent or a thread whose
/// lineage was not read, or loops — a tree this read cannot hang from a
/// conversation.
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

/// THE CLIENT'S OWN CONVERSATION, where the kernel names it: every thread
/// the daemon holds (its writer locks, `threads`) hangs from ONE root
/// ([`tree_root`]: that conversation and the subagents it spawned) and
/// `client` is the only Codex attached to it (`clients`,
/// [`daemon_clients_in`]) — the ROOT, the thread `codex resume` takes back.
/// The daemon holds the writer lock of every thread it has loaded, a
/// client's from the moment it attaches (a thread with no message yet
/// included), so one conversation and one client is that client's. One
/// thread alone is named whatever its lineage reads, unless it is a
/// subagent (which no client's conversation is): a first line this reader
/// could not read is no second conversation. `None` wherever that does not
/// hold: two roots (another session's, one run in the background with no
/// tab, this client's own earlier one), a thread no loaded root claims, more
/// clients, or clients the kernel would not list. Never a guess: a TUI's
/// `codex resume <id>` argv names the thread it STARTED on, and `/new` or
/// `/resume` inside it moves it to another. Until the third review of
/// 2026-09-28 this counted LOCKS, and a conversation with subagents was
/// never named — the owner's, with three.
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

/// WHERE A TURN RUNS THAT A DAEMON-MODE CLIENT'S `/exit` MUST WAIT FOR
/// ([`super::upgrade::DaemonTurn`], the gate's running-work floor) — over the
/// daemon's threads ([`Loaded`]: each one's turn and spawn tree), the Codex
/// attached to it (`clients`), the threads PLACED ELSEWHERE by this tab's
/// screen (`elsewhere`: each ran a turn through [`super::upgrade::QUIET_S`]
/// of its still run, [`still_through`]) and whether its footer shows a goal
/// being pursued (`goal`, [`goal_on_screen`]):
///
/// * the client's own conversation named by the kernel ([`own_thread`]: one
///   root, one client): a turn anywhere in it holds it — the root's, or a
///   subagent's it spawned — `Goal` where the footer shows one, else `Own`;
/// * not named: a running thread holds it as `Unplaced` — it may be this
///   client's own, so nothing is typed — unless it is placed elsewhere, which
///   only a ROOT is, only where the kernel lists ANOTHER Codex attached to
///   the daemon, and never under a goal on this screen:
///   * A SUBAGENT IS NEVER PLACED (the third review of 2026-09-28): its
///     turns never draw on any client's main screen — its own
///     conversation's included — so a still run of this screen says nothing
///     of whose it is. The owner's daemon held three subagents of the tab's
///     own conversation, running on their own after their spawn, and the
///     rule that placed them typed `/exit` over them.
///   * WITH THIS CLIENT THE DAEMON'S ONLY ONE, nothing is placed: a second
///     root is then a thread no tab shows, or this client's own conversation
///     while its screen shows another thread (a subagent, a `/side` fork),
///     whose turns it does not draw — not provably another session's.
///   * UNDER A GOAL ON THIS SCREEN nothing is placed: a pursued goal's next
///     turn streams under the last turn's end row with no status row, and
///     the screen reads that ENDED turn at the same last words (measured
///     2026-09-28, `aterm_phase::codex::fixtures::GOAL_NEXT_TURN_0_158`) —
///     its own thread runs through a still run of this screen exactly as
///     another session's would. The second review of 2026-09-28 drove the
///     incident's goal looks with one more thread on the daemon through the
///     rule that placed anyway, and it typed `/exit` at 06:12:42Z, mid-goal
///     (`upgrade_ladder_tests`, the second-thread replay).
///
/// What stays assumed where a root IS placed — another Codex attached, no
/// goal on this screen — is that this client's screen draws its own root's
/// turn: not where it shows a subagent's or a `/side` fork's thread instead
/// (how Codex 0.158.0 marks that view is unmeasured), nor a goal it pursues
/// with no status line configured (the footer draws the goal only through
/// one). The ladder's model states it (`HarnessUpgradeLadder`, `work`).
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

/// THE THREADS THIS TAB'S SCREEN PLACES IN ANOTHER SESSION, and the record
/// it keeps for that: each thread of the daemon running a turn at a look of
/// this screen's STILL RUN (looks that read it authoritatively idle with the
/// same last words, `upgrade_drive::St::still`), with the first look of the
/// run that found it running (`record`, as the last look left it). At this
/// look (`now`), with the threads running now (`running`) and whether the
/// look continues that run (`in_run`: an idle reading at the same words — a
/// look that begins a run, or reads no idle screen, keeps nothing from
/// before): the new record — every thread running now, from when the run
/// first found it — and those PLACED: found running at a look of the run
/// [`super::upgrade::QUIET_S`] (20 s) or more ago, and running now. A turn
/// of the client's own ROOT draws on its screen — its status row, its
/// streaming words, the rows a finished turn leaves — so a root running at
/// two looks 20 s apart of one still run is another session's (a goal's
/// thread that ended and began again between them too: its turns never drew
/// here). That is the placement's ASSUMPTION, and [`daemon_turn`] takes a
/// thread this returns as placed only where it holds as far as the lane can
/// tell: a ROOT (a subagent's turns draw on no main screen, its own
/// conversation's included), with another Codex attached to the daemon, and
/// no goal on this screen — a pursued GOAL's next turn draws no status row
/// and leaves the last words standing (measured 2026-09-28,
/// `GOAL_NEXT_TURN_0_158`), so under one the visit keeps no record at all.
/// One found running only now is not placed: a turn streams nothing for its
/// first moments, and a look can catch this client's own there.
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

/// THE NOTICE to an EMBEDDED session: one ordinary user turn asking for a
/// good stopping point and naming the one line to answer with. Never asks the
/// agent to cancel work that is still making progress. Like Claude Code's
/// (`upgrade::STOPPING_POINT`), it says that a wait on something that has
/// already ended is not such work.
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
         with {marker} on a line by itself. If you cannot stop now, say why; aterm will wait \
         and ask again later."
    )
}

/// THE CONTINUATION, typed once the relaunched embedded session holds its
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
/// client the gate alone — idle, settled by the ladder's rung, the composer
/// empty, no box, no busy row, no hold, nobody at the tab, and for a
/// daemon-mode client no turn of its own running in its daemon, nor one there
/// it cannot place in another session ([`Facts::daemon_turn`]: its `/exit`
/// might stop its own) — then
/// [`Step::Terminate`], which the Codex driver carries out as a typed
/// `/exit`, never a signal. A stopped
/// round of either kind starts a new one once it has rested
/// [`super::upgrade::RETRY_S`] ([`Step::Rearm`]): no stop is for good. A daemon-mode
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
/// rule: a skip of THIS target or a deferral not run out holds it, and a
/// stopped round's new one ([`super::upgrade::rearm_held`]); `--now` stands
/// it at the ladder's last rung ([`super::upgrade::Rung::Land`]: no settling
/// window, a keystroke within [`super::upgrade::KEYS_GAP_S`] still holding
/// it, every floor kept) and nothing else.
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
    /// A person gave input to a Codex tab on this home within the grace that
    /// tab's rung of the ladder keeps ([`super::upgrade::person_grace`]:
    /// `[harness] human_grace_s`, then [`super::upgrade::KEYS_GAP_S`] from
    /// [`super::upgrade::Rung::KeysOnly`] on and under the owner's `--now`).
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

/// `session_meta` FIRST LINES in the shapes measured 2026-09-28 on the
/// owner's Codex daemon (read-only), every value but the ones [`Lineage`]
/// reads a stand-in and the base instructions cut short: a conversation's
/// root as 0.149.1 wrote it (`"source":"cli"`), and a subagent as 0.158.0
/// spawns one (`multi_agent_version` 2: the parent named in `source` and
/// again at the top, the tree's root as its `session_id`).
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

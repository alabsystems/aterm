// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE MUSE CODE LANE, ITS PURE HALF: relaunch on exit and the restore of a
//! Muse Code (Meta's `muse`) tab. Its live upgrade's quiet flow is
//! [`super::upgrade_muse_live`], whose restart this relaunch carries. The I/O
//! half is `upgrade_drive`'s `muse` module (`upgrade_muse_drive.rs`). Every
//! fact here was measured on 1.4.10-R6427.1 in the lab (`crates/aterm-lab`,
//! the real binary against its stub, no account):
//!
//! * **One process.** A native arm64 binary launched directly (the managed
//!   twin `agents/muse` is `exec '<store build>' "$@"`), one TUI process with
//!   no daemon. Its tool shells are `/bin/sh -c` children.
//! * **Resume by id.** `muse resume <session-ref>` brings a session back, the
//!   prior turns in its next request; root options may stand on either side
//!   of `resume`. The ref is an exact session UUID OR a Session Name, so only
//!   a UUID ([`is_session_uuid`]) is ever typed. `resume --last` and a bare
//!   `resume` (its picker) are never typed. `resume` takes no prompt.
//! * **The conversation witness.** From launch, before any turn, the process
//!   holds `${XDG_DATA_HOME:-$HOME/.local/share}/muse/sessions/YYYY/MM/DD/
//!   <uuidv7>/.session.lock` (its body `pid=<pid>\nhost=<host>\n`, under an
//!   exclusive flock, open). It follows `/new` (the old lock truncated, the
//!   new one naming the pid) and an in-TUI `/resume`. A subagent's lock is
//!   deeper (`subagent/<uuid>/.session.lock`) and never the session's
//!   ([`session_of_lock`]). The per-uid runtime registry under `/private/tmp`
//!   is NOT a witness (it misses an in-TUI `/resume` and keeps dead
//!   processes' leases), nor is the resume line on screen (`/new` prints the
//!   OLD session's line while the process runs on).
//! * **The exit table** ([`exit_kind`]): `0` its own `/exit`, `/quit`, Ctrl-C
//!   or Ctrl-D twice; `130` an external SIGINT and `143` a SIGTERM, both of
//!   which write a clean `session.end` — someone's end, never relaunched (the
//!   live upgrade's own SIGTERM is claimed before it is sent, and carried as
//!   the harness's restart, never read here);
//!   `129` SIGHUP and `137` SIGKILL write none — a crash; `1` (`retained
//!   session not found`, an unknown session name, a refused start) and `2`
//!   (bad arguments) are REFUSALS of the start, never a crash, so never
//!   retried. Where the shell's word cannot be read, the session's own log
//!   ([`log_verdict`]): a `session.end` with `exit_reason: clean` after the
//!   last `session.started` is a clean end; none is a crash. The lock is no
//!   witness of the end: a SIGTERM leaves `pid=` in it as a SIGKILL does.
//! * **The relaunch line**: `cd <workspace_root>` (the session's own record,
//!   [`log_workspace`]: from another directory a resume asks to trust that
//!   directory and moves the session's workspace there), then the MANAGED
//!   `muse` (the twin, never `argv[0]`'s store path, which would pin the old
//!   build) `resume <uuid>` with the launch's carried flags ([`rewrite_argv`]),
//!   behind the `XDG_DATA_HOME`/`XDG_CONFIG_HOME` its own exec environment
//!   set, and its `HOME` where that was another than the user's
//!   ([`line_env`]). A session another live process holds opens Muse's
//!   PICKER, not an error, so nothing is typed while its lock is held.
//! * **A relaunch is typed once per crash**: a line Muse refused (`1`, `2`),
//!   or the shell could not run ([`line_exit_kind`]), is never typed again;
//!   only that line's own block ending in a crash types it again.
//! * **Kept with the session**: its permission mode and its model (a flag on
//!   the resume overrides them, undoing a `/permissions` or `/model` choice),
//!   so those flags are dropped ([`FLAGS`]).

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::upgrade::ArgvRefusal;

/// The program's name: its image's basename, `aterm_phase::program_of`'s.
pub const PROGRAM: &str = "muse";

/// The variable that moves Muse Code's data (its sessions, their locks and
/// logs, the session index): `${XDG_DATA_HOME:-$HOME/.local/share}/muse`.
pub const DATA_ENV: &str = "XDG_DATA_HOME";

/// The variable that moves its config (its login, settings and workspace
/// trust): `${XDG_CONFIG_HOME:-$HOME/.config}/muse`.
pub const CONFIG_ENV: &str = "XDG_CONFIG_HOME";

/// The home both of them default under, and anything else Muse Code keeps
/// relative to it.
pub const HOME_ENV: &str = "HOME";

/// How a root option takes its value (`muse --help`, 1.4.10, clap).
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

/// Muse Code's root options (`muse --help`, 1.4.10-R6427.1), each with its
/// fate on a relaunch. A flag missing here refuses the rewrite
/// ([`ArgvRefusal::UnknownFlag`]): nothing guesses whether a new flag takes a
/// value. The rule: what changes HOW Muse reaches its provider, runs its
/// tools or holds back is carried (the relaunched run is the launched one);
/// what the SESSION keeps and a person can change in it (permission mode,
/// model, effort) is dropped, since the flag would override that choice on
/// the resume; what makes a resume impossible or is unmeasured on one
/// refuses.
const FLAGS: &[(&str, Arity, Fate)] = &[
    // An ephemeral agent-definition overlay: this run's, never persisted, so
    // the relaunched run carries it to be the same run.
    ("--agents", Arity::One, Fate::Keep),
    // Permission mode, kept with the session (measured: a bare resume after
    // a `--yolo` resume came back YOLO; `/permissions` writes the session
    // and settings): a flag would undo a `/permissions` choice. The judge
    // and the sandbox's network mode are part of that permission snapshot
    // (the session log's permission transaction names `reviewer` and
    // `local_command_network`), and so are `--disable-approval` and
    // `--disable-sandbox` (the halves of `--yolo`). Dropped, a mode the
    // session did not keep falls back to the default, the stricter side.
    ("--approval-judge", Arity::One, Fate::Drop),
    ("--approval-mode", Arity::One, Fate::Drop),
    ("--base-url", Arity::One, Fate::Keep),
    // A per-run setting (`KEY=VALUE`, values JSON): how this run reaches its
    // provider (`provider_retry.max_attempts`, …).
    ("-c", Arity::One, Fate::Keep),
    ("--config", Arity::One, Fate::Keep),
    ("--contributor-mode", Arity::One, Fate::Keep),
    ("--disable-approval", Arity::None, Fate::Drop),
    ("--disable-request-user-input", Arity::None, Fate::Keep),
    ("--disable-sandbox", Arity::None, Fate::Drop),
    // Restrictions of this run: carried, so a relaunch is never looser than
    // its launch.
    ("--disable-shell", Arity::None, Fate::Keep),
    ("--disable-write", Arity::None, Fate::Keep),
    ("--echo-delay-ms", Arity::One, Fate::Keep),
    // Effort: `--reasoning-effort` rewrites the GLOBAL settings.json
    // (measured), so carried it would change every later session's default.
    ("--effort", Arity::One, Fate::Drop),
    ("--enable-shell-tool", Arity::None, Fate::Keep),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    // Attached to the next prompt, which the conversation already sent.
    ("--image", Arity::One, Fate::Drop),
    // The model, kept with the session (measured: a bare resume after a
    // `--model` resume came back on that model): a flag would undo `/model`.
    ("--model", Arity::One, Fate::Drop),
    // Without a session log there is nothing to resume (`resume requires
    // retained session logging`).
    ("--no-session-log", Arity::None, Fate::Refuse),
    ("--no-parallel-tool-calls", Arity::None, Fate::Keep),
    ("--parallel-tool-calls", Arity::None, Fate::Keep),
    ("--permission-profile", Arity::One, Fate::Drop),
    // A built-in preset, this run's choice and nothing the session can
    // change: carried.
    ("--preset", Arity::One, Fate::Keep),
    ("--provider", Arity::One, Fate::Keep),
    ("--reasoning-effort", Arity::One, Fate::Drop),
    ("--sandbox-network", Arity::One, Fate::Drop),
    ("--subagent-worktree-isolation", Arity::None, Fate::Keep),
    // Trust for this run only (not saved): carried, or the relaunch would
    // stop at the trust box the launch never drew.
    ("--trust-workspace", Arity::None, Fate::Keep),
    ("-V", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    // Workspace tools rooted elsewhere, and a session in a Git worktree Muse
    // made or was pointed at: unmeasured on a resume, so refused.
    ("-w", Arity::None, Fate::Refuse),
    ("--workspace", Arity::One, Fate::Refuse),
    ("--worktree", Arity::None, Fate::Refuse),
    ("--worktree-base", Arity::One, Fate::Refuse),
    ("--worktree-existing", Arity::One, Fate::Refuse),
    // Approval, sandbox, trust and no questions, for this run: the
    // permission mode the session keeps (measured), so dropped.
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

/// Muse Code's subcommands (`muse --help`, 1.4.10, and clap's `help`). Only
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
    "init",
    "help",
];

/// Whether clap reads `tok` as an option: a leading `-` and more after it.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// An option token's name and whether its value is attached
/// (`--model=x`): a short option is never written attached here (`-cX` is
/// no form [`FLAGS`] knows, so it refuses as unknown).
fn option_name(tok: &str) -> (&str, bool) {
    match tok.split_once('=') {
        Some((name, _)) if name.starts_with("--") => (name, true),
        _ => (tok, false),
    }
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
                // The first prompt (`muse "…"`), submitted at once and held
                // by the conversation: dropped.
                continue;
            }
            if out.resume && out.reference.is_none() {
                out.reference = Some(tok.to_string());
            }
            // A further positional is the prompt's (dropped); clap refused
            // any other at the launch.
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

/// THE ROOT OPTIONS OF `argv` THAT SAY HOW ITS RUN REACHES ITS PROVIDER
/// ([`PROVIDER_FLAGS`]), each with its value, verbatim and in order: what a
/// worker the run starts carries (`harness::worker`), so its task goes where
/// the starter's run goes, never to a default provider. Read with the table
/// ([`FLAGS`]), whatever each other option's fate, before `--`.
///
/// # Errors
/// A flag the table does not know ([`ArgvRefusal::UnknownFlag`]), which
/// could hold the value of one of these: nothing guesses.
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

/// Whether `argv` is an interactive Muse Code TUI's — no subcommand, or
/// `resume` — and not `exec`, a tool subcommand, `--help` or `--version`,
/// whose end is the one they were launched for. An argv the rewrite would
/// refuse for a flag it does not know is still a TUI's (its relaunch is
/// refused at the exit, with its word).
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

/// The `<session-ref>` a TUI's argv resumes (`muse [opts] resume [opts]
/// <ref>`), as given: a UUID or a Session Name, which only
/// [`is_session_uuid`] admits to a line. `None` for a plain launch, `resume
/// --last`, a bare `resume` (the picker), or an argv the table refuses.
#[must_use]
pub fn resumed_ref(argv: &[String]) -> Option<String> {
    read(argv).ok()?.reference
}

/// THE WORDS A RELAUNCH TYPES AFTER THE PROGRAM: the launch's kept root
/// options verbatim ([`FLAGS`]), then `resume <session>` — or, with no
/// session (`None`), the kept options alone, a fresh start. The launch's own
/// `resume` ref, `--last`, first prompt and dropped options are not carried.
///
/// # Errors
/// An unknown flag ([`ArgvRefusal::UnknownFlag`]), a launch no relaunch
/// brings back ([`ArgvRefusal::NotResumable`]: `exec`, a tool subcommand,
/// `--help`, `--no-session-log`, a worktree or another workspace), or a
/// session that is no UUID (a Session Name is never typed).
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

/// Whether `s` is a session id Muse Code prints and takes back by id: a
/// hyphenated UUID (8-4-4-4-12 hex digits; Muse's own are version 7). A
/// Session Name is never one, so a relaunch line never resumes by name.
#[must_use]
pub fn is_session_uuid(s: &str) -> bool {
    s.len() == 36 && super::codex_rollout::is_thread_id(s)
}

/// When a session began, unix milliseconds, from its UUIDv7 id's first 48
/// bits; `None` for an id that is not version 7.
#[must_use]
pub fn session_created_ms(session: &str) -> Option<u64> {
    if !is_session_uuid(session) {
        return None;
    }
    super::codex_rollout::thread_created_ms(session)
}

// ------------------------------------------------------------- the exit

/// How a Muse Code process ended, by its shell's exit status ([`exit_kind`])
/// or its session's log ([`log_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// Its own exit, or someone's SIGINT or SIGTERM (a clean `session.end`
    /// written): never relaunched.
    Clean,
    /// Muse refused its own start (`1`: the session is not found, an unknown
    /// session name; `2`: its arguments): never a crash, never retried — a
    /// relaunch would meet the same refusal (critic X4).
    Refused,
    /// SIGHUP, SIGKILL, any other signal or status: no clean shutdown was
    /// recorded. Relaunched on its conversation.
    Crashed,
}

impl ExitKind {
    /// One word for the step and the ledger.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            ExitKind::Clean => "clean",
            ExitKind::Refused => "refused",
            ExitKind::Crashed => "crashed",
        }
    }
}

/// THE WORD OF A RELAUNCH LINE'S OWN COMMAND, once its block ended: Muse
/// Code's table ([`exit_kind`]), and `126`/`127` — the shell could not run
/// the program the line names (the twin, or the build it execs, gone or not
/// executable) — a refusal too: typed again, the line meets the same wall.
/// A `cd` the line could not make (`( cd -- <workspace> && … )`) exits `1`,
/// a refusal by the table itself.
#[must_use]
pub fn line_exit_kind(status: i64) -> ExitKind {
    match status {
        126 | 127 => ExitKind::Refused,
        status => exit_kind(status),
    }
}

/// MUSE CODE'S EXIT TABLE, measured on 1.4.10-R6427.1 (`echo $?` in the
/// lab's tab after each): `0` its own exit (`/exit`, `/quit`, Ctrl-C or
/// Ctrl-D twice), `130` an external SIGINT and `143` SIGTERM — each writes a
/// clean `session.end` — are [`ExitKind::Clean`]; `1` and `2` are
/// [`ExitKind::Refused`]; `129` SIGHUP, `137` SIGKILL and every other status
/// (another signal, a failure) are [`ExitKind::Crashed`]. SIGHUP is a crash,
/// not someone's end: Muse writes no `session.end` on it, and its next resume
/// says the run `ended abnormally`.
#[must_use]
pub fn exit_kind(status: i64) -> ExitKind {
    match status {
        0 | 130 | 143 => ExitKind::Clean,
        1 | 2 => ExitKind::Refused,
        _ => ExitKind::Crashed,
    }
}

/// One session log line's top-level `payload_type` and the line, parsed —
/// only a line that names one of `kinds` at all, so the records between
/// (most of a long log) cost a substring scan and no parse. A retained frame
/// (the first line, its records nested as strings) has none.
fn record_of(line: &str, kinds: &[&str]) -> Option<(String, Value)> {
    if !kinds.iter().any(|k| line.contains(k)) {
        return None;
    }
    let v: Value = aterm_json::from_str(line.trim()).ok()?;
    let kind = v.get("payload_type")?.as_str()?.to_string();
    Some((kind, v))
}

/// THE CLEAN-VERSUS-CRASH WITNESS IN THE SESSION'S OWN LOG (`session.jsonl`
/// beside its lock), for an exit whose shell word cannot be read: after the
/// LAST `session.started` record, a `session.end` whose `exit_reason` is
/// `clean` is [`ExitKind::Clean`]; none (SIGHUP and SIGKILL write none), or
/// one with another reason, is [`ExitKind::Crashed`]. The next run's start
/// writes `crash_inferred` for a run that recorded no end, BEFORE its own
/// `session.started`, so a later run never reads as this one's end. `None`
/// with no `session.started` in `log` (a log not of a run, or a window that
/// does not reach one): no witness, nothing relaunched. A start this run
/// refused wrote nothing here, so a log is read only for a process the
/// conversation witness saw holding the session.
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

/// THE SESSION'S WORKSPACE, as its own log last recorded it: the newest
/// `workspace_root` of a run's start (`runtime.session_start.intake_v3`'s
/// `session_metadata`) or of `runtime.session.metadata`. Where a resume from
/// another directory moved the session there, its next run's start names
/// that directory (session-index.db keeps the first one, measured), so the
/// log is the session's own word. `None` where `log` names none, or one that
/// is not absolute.
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

// ------------------------------------------------------- the conversation

/// The pid a `.session.lock`'s body names (`pid=<pid>\nhost=<host>\n`);
/// `None` for a truncated lock (a clean exit, or the session `/new` left)
/// or any other body.
#[must_use]
pub fn lock_pid(body: &str) -> Option<u32> {
    body.lines()
        .find_map(|l| l.trim().strip_prefix("pid="))
        .and_then(|p| p.parse().ok())
        .filter(|&p| p > 0)
}

/// The lock file's name.
pub const LOCK: &str = ".session.lock";

/// The session's log beside it.
pub const LOG: &str = "session.jsonl";

/// Where Muse Code keeps its sessions under its data home `data`.
#[must_use]
pub fn sessions_root(data: &Path) -> PathBuf {
    data.join("muse").join("sessions")
}

/// The session `lock` is the lock of, AT SESSION DEPTH under `root`
/// ([`sessions_root`]): `<root>/YYYY/MM/DD/<uuid>/.session.lock`, the date
/// four, two and two digits and the session a UUID. A subagent's lock
/// (`…/<uuid>/subagent/<uuid>/.session.lock`) and any other path is none.
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

/// THE DATA HOME A PROCESS WITH THIS EXEC ENVIRONMENT USES:
/// `$XDG_DATA_HOME` (absolute), else `$HOME/.local/share` (absolute), else
/// `<fallback>/.local/share`. An empty `XDG_DATA_HOME` is unset. `None` for a
/// relative `XDG_DATA_HOME`, which names no directory aterm can place (the
/// XDG rule ignores one; Muse's own reading of it is unmeasured): nothing is
/// read, and nothing relaunched.
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

/// THE ENVIRONMENT A MUSE CODE RELAUNCH LINE MUST CARRY: `XDG_DATA_HOME`
/// and `XDG_CONFIG_HOME` with the values the process it replaces had in its
/// own exec environment, verbatim, each only where it had one, and its
/// `HOME` where that was another than the user's (`env`, as
/// [`relaunch_env`] read it). A Muse started `XDG_DATA_HOME=<dir> muse`
/// keeps its session under `<dir>`: relaunched without it, `resume` looks in
/// the shell's data home, does not find it and refuses (`1`). Required, as
/// codex-alab's `CODEX_HOME` is (`codex_alab::line_env`): a line that cannot
/// hold a value is refused rather than typed without it.
#[must_use]
pub fn line_env(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// The pairs [`line_env`] carries, read off one process's exec environment
/// (`get` names a variable's value): [`DATA_ENV`] and [`CONFIG_ENV`], in that
/// order, each as set (empty included) — and [`HOME_ENV`] where the process
/// ran under an absolute `HOME` other than `user_home` (the user's own, the
/// shell's the line is typed at). Such a Muse keeps its sessions under ITS
/// home's `.local/share` (where `XDG_DATA_HOME` is unset): relaunched under
/// the shell's `HOME`, `resume` finds no such session and refuses (`1`), and
/// so does the line a person is told ([`told_line`]). The Claude Code lane
/// carries the `HOME` of an agent under another one the same way
/// (`upgrade_drive::home_carried`).
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

/// [`relaunch_env`]'s pairs as a record keeps them (`NAME=VALUE`), and back.
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
        .filter(|(name, _)| [DATA_ENV, CONFIG_ENV, HOME_ENV].contains(name))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// Whether `program` is a build in an atpkg store (`…/store/muse/<build>/
/// bin/muse`): the path a twin execs, which a relaunch never types (it would
/// pin that build and skip the twin's own exports, critic X3) — the managed
/// twin is typed in its place, or nothing.
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

/// THE LINE A PERSON IS TOLD to resume a Muse Code session by hand: Muse's
/// own resume line (`To continue this session, run muse resume <uuid>`),
/// behind the data and config homes its relaunch carries ([`line_env`]),
/// each quoted as a shell reads an assignment. `None` for a session that is
/// no UUID or a word no line can hold (a control character).
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

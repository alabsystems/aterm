// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-drive` — the AI-friendly "sugar" CLI over the core `await`/`send`/`key`/
//! `text` primitives. It teaches itself through `--help` and emits actionable
//! errors, so an AI agent builds correct intuition for the kernel without docs.
//! All real work uses the core control verbs: a configured prompt reuses one
//! [`RelayClient`] connection, while discovery and the other commands retain the
//! std-only `aterm-ctl` client.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::supervise::approvals;
use crate::supervise::limit::tz_offset_s;
use crate::supervise::run::Resume;
use crate::supervise::transport::{Endpoint, Transport};
use crate::supervise::{
    self, ClockAnchor, EXIT_TIMEOUT, LedgerFormat, LedgerHost, LedgerOpts, MailOpts, Mark,
    ReportOpts, Session, SuperviseOpts, TaskOpts, View, classify_command_with, exit_reason,
    render_phase_and_survey, worker_phase,
};
use crate::{ControlClient, CtlClient, DRIVE_HELP, RelayClient, SelfGovernor, Turn};

/// Resolve the `aterm-ctl` binary: `$ATERM_CTL`, then a sibling of this binary
/// (the cargo/install layout), then bare `aterm-ctl` on `PATH`.
fn resolve_ctl() -> PathBuf {
    if let Ok(p) = std::env::var("ATERM_CTL") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| sibling_ctl(&exe))
        .unwrap_or_else(|| PathBuf::from("aterm-ctl"))
}

/// The `aterm-ctl` beside the binary at `exe`, looked up from its RESOLVED path. On
/// macOS `current_exe()` is the path the process was started by, so an `aterm` run
/// through `~/.local/bin/aterm` would otherwise take `~/.local/bin/aterm-ctl` — where
/// a copy from an old install sits — over the bundle's own alias.
pub(crate) fn sibling_ctl(exe: &std::path::Path) -> Option<PathBuf> {
    let exe = exe.canonicalize().ok()?;
    let sib = exe.parent()?.join("aterm-ctl");
    sib.is_file().then_some(sib)
}

#[derive(Debug)]
struct Opts {
    socket: Option<String>,
    /// A saved REMOTE connection name to `dial` via the local host (`--dial`).
    dial: Option<String>,
    idle_ms: u64,
    timeout_ms: u64,
    /// The prompt-ready regex for the best-effort settle confirm. `None` = the
    /// built-in default; `Some("")` = idle-only (skip the confirm entirely).
    ready: Option<String>,
    cmd: Vec<String>,
}

/// Resolve the prompt-ready pattern: `--ready` beats `$ATERM_DRIVE_READY`, which
/// beats the built-in default.
///
/// The default ([`crate::claude_prompt_ready_pattern`]) matches a Claude input
/// caret, which is only correct when the driven session IS Claude. Driving any
/// other REPL/agent needs its own prompt, and driving a plain shell wants no
/// pattern at all — so this is a knob, not a constant. It stays a BEST-EFFORT
/// extra settle either way: a non-matching pattern costs a bounded wait, never
/// a failed turn.
fn resolve_ready(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("ATERM_DRIVE_READY").ok())
        .unwrap_or_else(|| crate::claude_prompt_ready_pattern().to_string())
}

fn parse(argv: Vec<std::ffi::OsString>) -> Result<Opts, String> {
    let mut socket = None;
    let mut dial = None;
    let mut idle_ms = 600u64;
    let mut timeout_ms = 180_000u64;
    let mut ready = None;
    let mut cmd = Vec::new();
    let mut it = argv.into_iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" | "--sock" => {
                socket = Some(it.next().ok_or("--socket needs a PATH")?);
            }
            "--dial" | "--remote" => {
                dial = Some(it.next().ok_or("--dial needs a saved connection NAME")?);
            }
            "--idle" => {
                idle_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--idle needs a millisecond integer")?;
            }
            "--timeout" => {
                timeout_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--timeout needs a millisecond integer")?;
            }
            // An EMPTY value is meaningful (idle-only), so this takes the next
            // argument verbatim rather than treating "" as "unset".
            "--ready" => {
                ready = Some(it.next().ok_or(
                    "--ready needs a REGEX (use '' for idle-only, no prompt-ready confirm)",
                )?);
            }
            "-h" | "--help" | "help" => {
                cmd = vec!["help".to_string()];
                break;
            }
            _ => {
                cmd.push(a);
                cmd.extend(it.by_ref());
                break;
            }
        }
    }
    Ok(Opts {
        socket,
        dial,
        idle_ms,
        timeout_ms,
        ready,
        cmd,
    })
}

/// Resolve an explicitly configured LOCAL control endpoint. `Ok(None)` means
/// neither `--socket` nor `$ATERM_CONTROL_SOCK` selected one, so a local prompt
/// must retain the existing `aterm-ctl` discovery path. Once a socket IS selected,
/// a missing token is an error rather than permission to silently drive some
/// other discovered instance.
fn resolve_configured_local_endpoint_with(
    flag_socket: Option<String>,
    env_socket: Option<String>,
    env_token: Option<String>,
    read_token: impl FnOnce(&std::path::Path) -> Option<String>,
) -> Result<Option<(String, String)>, String> {
    let Some(sock) = flag_socket
        .or(env_socket)
        .filter(|s| !s.is_empty() && s != "0" && s != "off")
    else {
        return Ok(None);
    };
    let token = env_token
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // The SHARED convention, not a hand-rolled one: an instance socket
            // (`aterm-<pid>.sock`) pairs with `aterm-<pid>.token`, and an explicit
            // `$ATERM_CONTROL_SOCK` path with a token named after that socket,
            // resolved in the socket's own directory and through the `latest`
            // alias. This used to derive `<stem>.token`, which the server never
            // wrote, so `--dial` silently failed to authenticate where `aterm ctl`
            // worked; deriving it here AGAIN is how that came back.
            let tok_path = aterm_uds::latest::token_path_for_sock(&sock)?;
            read_token(&tok_path).map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .ok_or(
            "could not resolve the LOCAL control token: set ATERM_CONTROL_TOKEN, or ensure the \
             token file beside the socket is readable (`aterm-<pid>.token` for an instance \
             socket, else the token named after the socket itself)",
        )?;
    Ok(Some((sock, token)))
}

/// Resolve the LOCAL host's control socket + capability token when a socket was
/// explicitly selected by `--socket` or `$ATERM_CONTROL_SOCK`. The token comes
/// from `$ATERM_CONTROL_TOKEN`, else the sibling token file located by the SHARED
/// convention (`aterm_uds::latest::token_path_for_sock`) — `aterm-<pid>.token`
/// for an instance socket, and a token named after any explicit socket. The
/// legacy shared `aterm.token` is accepted only when the per-socket file is absent.
fn resolve_configured_local_endpoint(opts: &Opts) -> Result<Option<(String, String)>, String> {
    resolve_configured_local_endpoint_with(
        opts.socket.clone(),
        std::env::var("ATERM_CONTROL_SOCK").ok(),
        std::env::var("ATERM_CONTROL_TOKEN").ok(),
        |path| aterm_ctl::read_control_token_file(path).ok(),
    )
}

/// `--dial` always needs an explicit/env LOCAL endpoint; it has no discovery
/// fallback because the local host is the authority that resolves the saved name.
fn resolve_local_endpoint(opts: &Opts) -> Result<(String, String), String> {
    resolve_configured_local_endpoint(opts)?.ok_or_else(|| {
        "--dial needs the LOCAL host socket: pass --socket <PATH> or set ATERM_CONTROL_SOCK"
            .to_string()
    })
}

#[derive(Debug, PartialEq, Eq)]
enum LocalPromptRoute {
    Persistent { socket: String, token: String },
    ShellDiscovery,
}

/// A missing/unreadable token keeps the pre-existing shell client in charge: it
/// has its own discovery and classified authentication diagnostics. Only a fully
/// resolved endpoint opts into the persistent protocol path.
fn local_prompt_route(endpoint: Result<Option<(String, String)>, String>) -> LocalPromptRoute {
    match endpoint {
        Ok(Some((socket, token))) => LocalPromptRoute::Persistent { socket, token },
        Ok(None) | Err(_) => LocalPromptRoute::ShellDiscovery,
    }
}

fn run_prompt_turn<C: ControlClient>(
    opts: &Opts,
    client: &mut C,
    text: &str,
) -> Result<String, String>
where
    C::Error: std::fmt::Display,
{
    let mut gov = SelfGovernor::disabled(64, 8, 5_000_000);
    gov.enable_self_write();
    let turn = Turn {
        idle: Duration::from_millis(opts.idle_ms),
        timeout: Duration::from_millis(opts.timeout_ms),
        ready_pattern: resolve_ready(opts.ready.clone()),
    };
    turn.run(client, &mut gov, text.as_bytes())
        .map_err(|e| e.to_string())
}

/// Run the configured-endpoint prompt fast path when `route` selected it.
/// Keeping route resolution outside this seam makes the protocol test
/// independent of ambient process environment while production still uses the
/// exact same connection/Turn body.
fn run_persistent_local_prompt(
    opts: &Opts,
    route: LocalPromptRoute,
) -> Option<Result<String, String>> {
    if opts.cmd.first().map(String::as_str) != Some("prompt") {
        return None;
    }
    let LocalPromptRoute::Persistent {
        socket: sock,
        token,
    } = route
    else {
        return None;
    };
    let text = opts.cmd[1..].join(" ");
    if text.is_empty() {
        return Some(Err(
            "prompt needs text, e.g. `aterm-drive prompt 'say hi'`".to_string()
        ));
    }
    Some(
        RelayClient::connect_local(&sock, &token)
            .map_err(|e| {
                format!(
                    "cannot reach the configured target aterm at control socket '{sock}' ({e}).\n  \
                     • Is that aterm still running?\n  \
                     • Is the socket/token pair current? (--socket / ATERM_CONTROL_SOCK / \
                     ATERM_CONTROL_TOKEN)"
                )
            })
            .and_then(|mut client| run_prompt_turn(opts, &mut client, &text)),
    )
}

/// What a command prints and the code it exits with: `0` for success, `1` for
/// `classify`'s not-read-only, [`EXIT_TIMEOUT`] for a spent supervisor budget.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub code: u8,
}

impl Reply {
    fn text(text: String) -> Self {
        Self { text, code: 0 }
    }
}

/// The arguments after a supervisor verb: `[@sid]` then its flags.
#[derive(Debug, Default, PartialEq, Eq)]
struct SubArgs {
    sid: Option<String>,
    timeout_ms: Option<u64>,
    auto_reads: bool,
    max_s: Option<u64>,
    /// How long an outage is ridden out (`--reconnect-s`; `None` =
    /// the loop's default).
    reconnect_s: Option<u64>,
    allow_python: Vec<String>,
    notes: Option<PathBuf>,
    /// `report --since <origin:i>`: start right after that archived row.
    since: Option<Mark>,
    /// `report --max-rows N`: the most archived rows read.
    max_rows: Option<usize>,
    /// `watch --report`: count a report into each idle/question/limited EVENT.
    report: bool,
    /// `watch` / `supervise --dismiss-surveys`: press `0` on the session
    /// survey (guarded) instead of reporting it.
    dismiss_surveys: bool,
    /// `watch` / `supervise --context-warn <pct>`: say `EVENT context` when
    /// the worker's context left first reads at or below it, then `EVENT
    /// compacted` (`None` = [`DEFAULT_CONTEXT_WARN`]; `0` = neither).
    context_warn: Option<u8>,
    /// `watch` / `supervise --journal FILE`: the file the loop appends one
    /// JSON object to per line it prints; `ledger --journal FILE`: the file
    /// it reads back.
    journal: Option<PathBuf>,
    /// `report --final` / `--messages`: which of the report's rows print.
    view: View,
    /// `ledger --format`.
    format: Option<LedgerFormat>,
    /// `ledger --out PATH`: write the ledger there instead of to stdout.
    out: Option<PathBuf>,
    /// `ledger --since`: Unix ms, from a time word.
    since_ms: Option<i64>,
    /// `watch` / `supervise --mail`: park the mail lane on the manager's
    /// inbox beside the loop.
    mail: bool,
    /// `--inbox @<sid>`: the manager's session (`watch`, `supervise`, `task`;
    /// `@self` unless given).
    inbox: Option<String>,
    /// `--report-window S` / `--idle-grace S` (`watch`, `supervise`).
    report_window_s: Option<u64>,
    idle_grace_s: Option<u64>,
    /// `task --deadline S`: the advisory deadline the post carries (and the
    /// bound of `--wait`).
    deadline_s: Option<u64>,
    /// `task --wait`: park for the answer.
    wait: bool,
    /// `watch --resume [RULES]`: `Some(None)` continues the worker after a
    /// limit's reset, `Some(Some(file))` types that file's rules with it.
    resume: Option<Option<PathBuf>>,
    /// Positional words (the command text for `classify`, the task's text
    /// for `task`).
    rest: Vec<String>,
}

fn parse_sub(verb: &str, args: &[String]) -> Result<SubArgs, String> {
    let mut out = SubArgs::default();
    // `classify`'s positionals are a shell line: from its first word on, every
    // word — a `--oneline` included — is the command's, and `--` ends the
    // flags for every verb.
    let words_after_first = verb == "classify";
    let mut it = args.iter();
    let need = |flag: &str, what: &str, v: Option<&String>| -> Result<String, String> {
        v.cloned()
            .ok_or_else(|| format!("{verb}: {flag} needs {what}"))
    };
    let int = |flag: &str, what: &str, v: Option<&String>| -> Result<u64, String> {
        need(flag, what, v)?
            .parse()
            .map_err(|_| format!("{verb}: {flag} needs {what}"))
    };
    while let Some(a) = it.next() {
        if words_after_first && !out.rest.is_empty() {
            out.rest.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => {
                out.rest.extend(it.by_ref().cloned());
                break;
            }
            s if s.starts_with('@') && !words_after_first && out.sid.is_none() => {
                out.sid = Some(s.to_string());
            }
            "--timeout" => {
                out.timeout_ms = Some(int("--timeout", "a millisecond integer", it.next())?)
            }
            "--auto-reads" => out.auto_reads = true,
            "--max-s" => out.max_s = Some(int("--max-s", "a seconds integer", it.next())?),
            "--reconnect-s" => {
                out.reconnect_s = Some(int("--reconnect-s", "a seconds integer", it.next())?)
            }
            "--allow-python" => out.allow_python.push(need(
                "--allow-python",
                "a GLOB (e.g. 'scripts/*report*.py')",
                it.next(),
            )?),
            "--notes" => out.notes = Some(PathBuf::from(need("--notes", "a FILE", it.next())?)),
            // `ledger`'s `--since` is a TIME (the journal's clock), every
            // other verb's an archive mark.
            "--since" if verb == "ledger" => {
                let what = "a time: Unix milliseconds, or YYYY-MM-DD[THH:MM[:SS]][Z|±HH:MM]";
                let v = need("--since", what, it.next())?;
                out.since_ms = Some(
                    supervise::parse_since(&v, tz_offset_s())
                        .ok_or_else(|| format!("{verb}: --since needs {what}"))?,
                );
            }
            "--since" => {
                let what = "ORIGIN:INDEX (a report's last=) or INDEX";
                let v = need("--since", what, it.next())?;
                out.since =
                    Some(Mark::parse(&v).ok_or_else(|| format!("{verb}: --since needs {what}"))?);
            }
            "--max-rows" => {
                let what = "a positive row count";
                let n = int("--max-rows", what, it.next())?;
                out.max_rows = Some(
                    usize::try_from(n)
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or_else(|| format!("{verb}: --max-rows needs {what}"))?,
                );
            }
            "--report" => out.report = true,
            // The loops write a journal; the ledger reads one.
            "--journal" => {
                if !matches!(verb, "watch" | "supervise" | "ledger") {
                    return Err(format!(
                        "{verb}: --journal is watch's and supervise's (they write it) and \
                         ledger's (it reads it back)"
                    ));
                }
                out.journal = Some(PathBuf::from(need("--journal", "a FILE", it.next())?));
            }
            // What a report prints: the rows are read the same way either way.
            "--final" | "--messages" => {
                if verb != "report" {
                    return Err(format!(
                        "{verb}: {a} is report's (it chooses which of the report's rows print)",
                        a = a.as_str()
                    ));
                }
                let want = if a == "--final" {
                    View::Final
                } else {
                    View::Messages
                };
                if out.view != View::All && out.view != want {
                    return Err(format!(
                        "{verb}: --final and --messages are two views; pass one"
                    ));
                }
                out.view = want;
            }
            "--format" => {
                if verb != "ledger" {
                    return Err(format!("{verb}: --format is ledger's"));
                }
                let what = "text, md or html";
                let v = need("--format", what, it.next())?;
                out.format = Some(
                    LedgerFormat::parse(&v)
                        .ok_or_else(|| format!("{verb}: --format needs {what}"))?,
                );
            }
            "--out" => {
                if verb != "ledger" {
                    return Err(format!(
                        "{verb}: --out is ledger's (every other verb prints to stdout)"
                    ));
                }
                out.out = Some(PathBuf::from(need("--out", "a PATH", it.next())?));
            }
            // Only the loops that see a survey appear press anything on it.
            "--dismiss-surveys" => {
                if !matches!(verb, "watch" | "supervise") {
                    return Err(format!(
                        "{verb}: --dismiss-surveys is watch's and supervise's (the loops that \
                         see the session survey appear and press 0 on it)"
                    ));
                }
                out.dismiss_surveys = true;
            }
            // Only the loops that watch the worker's turns see its context
            // run low and the compaction that follows.
            "--context-warn" => {
                if !matches!(verb, "watch" | "supervise") {
                    return Err(format!(
                        "{verb}: --context-warn is watch's and supervise's (the loops that \
                         see the worker's context run low and compact)"
                    ));
                }
                let what = "a percentage from 0 to 100 (0: off)";
                let n = int("--context-warn", what, it.next())?;
                out.context_warn = Some(
                    u8::try_from(n)
                        .ok()
                        .filter(|&n| n <= 100)
                        .ok_or_else(|| format!("{verb}: --context-warn needs {what}"))?,
                );
            }
            // Mail is the loops' channel; `task` sends by it.
            "--mail" => {
                if !matches!(verb, "watch" | "supervise") {
                    return Err(format!(
                        "{verb}: --mail is watch's and supervise's (the loops that park the \
                         mail lane on your inbox beside the worker's screen)"
                    ));
                }
                out.mail = true;
            }
            "--inbox" => {
                if !matches!(verb, "watch" | "supervise" | "task") {
                    return Err(format!(
                        "{verb}: --inbox is watch's, supervise's and task's (the session whose \
                         inbox is yours: @self unless given)"
                    ));
                }
                let v = need("--inbox", "@<sid> (your own session)", it.next())?;
                if !v.starts_with('@') {
                    return Err(format!("{verb}: --inbox needs @<sid> (your own session)"));
                }
                out.inbox = Some(v);
            }
            "--report-window" | "--idle-grace" => {
                if !matches!(verb, "watch" | "supervise") {
                    return Err(format!(
                        "{verb}: {a} is watch's and supervise's (how --mail folds the worker's \
                         report into its idle point)",
                        a = a.as_str()
                    ));
                }
                let n = int(a, "a seconds integer", it.next())?;
                if a == "--report-window" {
                    out.report_window_s = Some(n);
                } else {
                    out.idle_grace_s = Some(n);
                }
            }
            "--deadline" => {
                if verb != "task" {
                    return Err(format!(
                        "{verb}: --deadline is task's (the advisory deadline the post carries)"
                    ));
                }
                out.deadline_s = Some(int("--deadline", "a seconds integer", it.next())?);
            }
            // Only the unattended loop lives through a limit's reset.
            "--resume" => {
                if verb != "watch" {
                    return Err(format!(
                        "{verb}: --resume is watch's (the loop that stays through a usage \
                         limit's reset and continues the worker after it)"
                    ));
                }
                // An optional RULES file: the next word, unless it is a flag
                // or the worker's @sid.
                let file = it
                    .clone()
                    .next()
                    .filter(|w| !w.starts_with('-') && !w.starts_with('@'))
                    .map(|w| {
                        it.next();
                        PathBuf::from(w)
                    });
                out.resume = Some(file);
            }
            "--wait" => {
                if verb != "task" {
                    return Err(format!(
                        "{verb}: --wait is task's (it sends the task by mail)"
                    ));
                }
                out.wait = true;
            }
            other if other.starts_with("--") => {
                return Err(format!(
                    "{verb}: unknown option '{other}'. Run `aterm-drive --help` for the flags."
                ));
            }
            other => out.rest.push(other.to_string()),
        }
    }
    Ok(out)
}

/// `classify <cmd>`: pure, no host needed.
fn classify_verb(args: &[String]) -> Result<Reply, String> {
    let sub = parse_sub("classify", args)?;
    let cmd = sub.rest.join(" ");
    if cmd.trim().is_empty() {
        return Err(
            "classify needs a shell command line, e.g. `aterm-drive classify 'git status && git pull'`"
                .to_string(),
        );
    }
    let allow = python_allow(&sub);
    let v = classify_command_with(&cmd, &allow);
    Ok(if v.read_only {
        Reply::text("read-only\n".to_string())
    } else {
        Reply {
            text: format!("not-read-only {}\n", v.reason),
            code: 1,
        }
    })
}

fn python_allow(sub: &SubArgs) -> Vec<String> {
    if sub.allow_python.is_empty() {
        supervise::DEFAULT_PYTHON_ALLOW
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        sub.allow_python.clone()
    }
}

fn no_positionals(verb: &str, sub: &SubArgs) -> Result<(), String> {
    if sub.rest.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{verb} takes only [@sid] and flags; unexpected: {}",
            sub.rest.join(" ")
        ))
    }
}

/// The whole drive CLI as a callable: `argv[1..]` in, exit code out. Served
/// in-process by the ONE `aterm` binary (`aterm drive …` / argv0 alias) and
/// by the thin standalone bin.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let opts = match parse(argv) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("aterm-drive: {e}\n\nRun `aterm-drive --help` for usage.");
            return ExitCode::FAILURE;
        }
    };
    match run(&opts) {
        Ok(Reply { text, code }) => {
            print!("{text}");
            ExitCode::from(code)
        }
        Err(e) => {
            eprintln!("aterm-drive: {e}");
            if let Some(line) = watch_exit_line(&opts.cmd, &e) {
                // The journal gets this EXIT too. `watch --journal FILE` says
                // it appends one record for EVERY line it prints, and a replay
                // reads that file alone — so a run that died BEFORE the loop
                // (no host to reach, a flag it cannot parse) has to leave the
                // reason there, not only on a stdout nobody kept. The loop's
                // own `Journal::open` is never reached on this path, so the
                // file is opened here, from the argv as typed.
                let mut warn = std::io::stderr();
                let mut journal = supervise::Journal::open(
                    journal_arg(&opts.cmd).as_deref(),
                    watch_sid_arg(&opts.cmd),
                    &mut warn,
                );
                journal.record(&line, None, &mut warn);
                println!("{line}");
            }
            ExitCode::FAILURE
        }
    }
}

/// The `--journal FILE` of a command line, read straight from the argv rather
/// than from [`parse_sub`] — a failure BEFORE the loop may BE a sub-flag that
/// would not parse, and the journal still has to record why the run ended.
fn journal_arg(cmd: &[String]) -> Option<PathBuf> {
    cmd.windows(2)
        .find(|w| w[0] == "--journal")
        .map(|w| PathBuf::from(&w[1]))
}

/// The `@sid` of a command line, the same way (`None` when the operator named
/// none and the loop would have driven the focused session).
fn watch_sid_arg(cmd: &[String]) -> Option<&str> {
    cmd.iter()
        .skip(1)
        .map(String::as_str)
        .find(|a| a.starts_with('@'))
}

/// The `EXIT <reason>` line `watch` ends on when it fails before its loop
/// runs — a flag of its own it cannot parse, no host to reach — so a harness
/// that reads its stdout line by line learns why from the last line, as it
/// does when the loop fails: the error's first line.
fn watch_exit_line(cmd: &[String], err: &str) -> Option<String> {
    (cmd.first().map(String::as_str) == Some("watch"))
        .then(|| format!("EXIT {}", exit_reason(err.lines().next().unwrap_or(err))))
}

/// What `phase` and `await-turn` print for a turn: the phase and its detail,
/// then `survey 0` while the session survey is open and `context <n>%` while
/// Claude Code's context indicator is up ([`render_phase_and_survey`]) —
/// `await-turn` prints its turn exactly like `phase` — and exit 124 for a
/// turn the timeout cut short.
fn phase_reply(turn: &supervise::Turn, allow: &[String]) -> Reply {
    Reply {
        text: render_phase_and_survey(turn, allow),
        code: if turn.timed_out { EXIT_TIMEOUT } else { 0 },
    }
}

fn run(opts: &Opts) -> Result<Reply, String> {
    let verb = opts.cmd.first().map(String::as_str).unwrap_or("help");
    if verb == "help" {
        return Ok(Reply::text(format!("{DRIVE_HELP}\n")));
    }
    if verb == "classify" {
        return classify_verb(&opts.cmd[1..]);
    }
    // `--dial <name>`: drive a REMOTE aterm over the local host's `dial` relay. A
    // persistent `RelayClient` speaks the SAME verbs as the local path, so the Turn
    // is byte-identical — predicates run on the authoritative remote host. Supports
    // the `prompt` drive loop (the remote use case); other verbs stay local.
    //
    // ITS REFUSAL RUNS BEFORE `ledger`. The ledger needs no preflight and so
    // used to be dispatched above this guard — and `--dial box ledger @sid`
    // then read the LOCAL host in silence and printed a report of the wrong
    // machine under a remote name, at exit 0, while every other verb said so.
    if let Some(name) = &opts.dial {
        if verb != "prompt" {
            return Err(format!(
                "--dial supports the `prompt` command (the drive loop); got `{verb}`. \
                 Run local read/await/shot/ledger without --dial."
            ));
        }
        let text = opts.cmd[1..].join(" ");
        if text.is_empty() {
            return Err(
                "prompt needs text, e.g. `aterm-drive --dial work prompt 'say hi'`".to_string(),
            );
        }
        let (sock, token) = resolve_local_endpoint(opts)?;
        let mut client = RelayClient::dial_via_local(&sock, &token, name).map_err(|e| {
            format!(
                "could not dial remote connection '{name}' via the local host ({e}).\n  \
                 • Is '{name}' a connection saved on THIS host? (the local aterm resolves it)\n  \
                 • Is the LOCAL socket/token right? (--socket / ATERM_CONTROL_SOCK / ATERM_CONTROL_TOKEN)"
            )
        })?;
        return run_prompt_turn(opts, &mut client, &text).map(Reply::text);
    }

    // The ledger reads whatever answers and says what did not: a session that
    // is gone still has a journal to replay, so it runs before the preflight.
    if verb == "ledger" {
        return ledger_verb(opts, &opts.cmd[1..]);
    }

    // A configured local endpoint already gives us everything the control CLI
    // would rediscover on every verb. Keep one authenticated connection for the
    // entire prompt turn (send/key/await/text), and let the connect itself replace
    // the subprocess `cursor` preflight. Other commands deliberately retain the
    // shell client because this fast path is scoped to the Turn flow.
    if let Some(result) = run_persistent_local_prompt(
        opts,
        local_prompt_route(resolve_configured_local_endpoint(opts)),
    ) {
        return result.map(Reply::text);
    }

    let ctl = resolve_ctl();
    let mut client = CtlClient::new(ctl.clone(), opts.socket.clone());

    // A friendly preflight: if we cannot even read the screen, explain why before
    // attempting to drive — this is the error an AI hits most and learns from.
    if let Err(e) = client.run(&["cursor"]) {
        return Err(format!(
            "cannot reach a target aterm over the control socket ({e}).\n  \
             • Is a host aterm running? Launch one headless:\n      \
             aterm-gui --headless &\n  \
             • Point at its socket (it prints 'control socket listening at <PATH>'):\n      \
             export ATERM_CONTROL_SOCK=<PATH>   (or pass --socket <PATH>)\n  \
             • aterm-ctl resolved to: {}",
            ctl.display()
        ));
    }

    match verb {
        "prompt" => {
            let text = opts.cmd[1..].join(" ");
            if text.is_empty() {
                return Err("prompt needs text, e.g. `aterm-drive prompt 'say hi'`".to_string());
            }
            run_prompt_turn(opts, &mut client, &text).map(Reply::text)
        }
        "read" => client.run(&["text"]).map(Reply::text),
        "shot" => {
            let path = opts.cmd.get(1).cloned();
            let mut args = vec!["image"];
            if let Some(p) = &path {
                args.push(p);
            }
            client.run(&args).map(Reply::text)
        }
        "phase" => {
            let sub = parse_sub(verb, &opts.cmd[1..])?;
            no_positionals(verb, &sub)?;
            let allow = python_allow(&sub);
            let mut session = Session::new(&mut client, sub.sid);
            let screen = session.read_screen()?;
            let turn = supervise::Turn {
                phase: worker_phase(&screen.rows),
                screen,
                timed_out: false,
            };
            Ok(phase_reply(&turn, &allow))
        }
        "await-turn" => {
            let sub = parse_sub(verb, &opts.cmd[1..])?;
            no_positionals(verb, &sub)?;
            let allow = python_allow(&sub);
            let timeout = Duration::from_millis(sub.timeout_ms.unwrap_or(opts.timeout_ms));
            let reconnect = sub.reconnect_s;
            let mut session = Session::new(&mut client, sub.sid);
            set_reconnect(&mut session, reconnect);
            let turn = session.await_turn(timeout)?;
            Ok(phase_reply(&turn, &allow))
        }
        "supervise" => {
            let sub = parse_sub(verb, &opts.cmd[1..])?;
            no_positionals(verb, &sub)?;
            let sopts = supervise_opts(&sub);
            mail_needs_sid(verb, &sub)?;
            let reconnect = sub.reconnect_s;
            // The loop's client is one persistent connection where it can be
            // opened (`aterm-ctl` per request otherwise); `--mail`'s lane is
            // a client of its own: it parks on YOUR inbox while the loop's
            // client watches the worker.
            let mut client = supervise_transport(opts, &ctl);
            let mut lane = sopts
                .mail
                .is_some()
                .then(|| supervise_transport(opts, &ctl));
            let ledger = approvals::default_path(sub.sid.as_deref());
            let mut session = Session::new(&mut client, sub.sid);
            session.set_approval_ledger(ledger);
            set_reconnect(&mut session, reconnect);
            let (text, code) = session.supervise_mail(&sopts, lane.as_mut())?;
            Ok(Reply { text, code })
        }
        // The same loop, one flushed stdout line per decision, for a harness
        // monitor that wakes its agent per line; it prints as it goes, so the
        // reply carries only the exit code.
        "watch" => {
            let sub = parse_sub(verb, &opts.cmd[1..])?;
            no_positionals(verb, &sub)?;
            let sopts = supervise_opts(&sub);
            mail_needs_sid(verb, &sub)?;
            let resume = resume_opts(&sub)?;
            let manager = manager_sid(&sub);
            let reconnect = sub.reconnect_s;
            let mut client = supervise_transport(opts, &ctl);
            let mut lane = sopts
                .mail
                .is_some()
                .then(|| supervise_transport(opts, &ctl));
            // The story lane: the worker's window is told each decision as
            // `aterm ctl @sid story …` (round 19), through a client of its own.
            let mut teller = supervise_transport(opts, &ctl);
            let ledger = approvals::default_path(sub.sid.as_deref());
            let mut session = Session::new(&mut client, sub.sid);
            session.set_approval_ledger(ledger);
            set_reconnect(&mut session, reconnect);
            session.set_manager(manager);
            session.set_resume(resume);
            // stdout itself, not its lock: the mail lane's thread prints
            // through the same sink.
            let code = session.watch_telling(
                &sopts,
                lane.as_mut(),
                Some(&mut teller),
                &mut std::io::stdout(),
            );
            Ok(Reply {
                text: String::new(),
                code,
            })
        }
        // The task goes by mail; the PTY gets at most the one-line nudge.
        "task" => {
            let sub = parse_sub(verb, &opts.cmd[1..])?;
            let topts = task_opts(opts, &sub)?;
            let code = supervise::task(&mut client, &topts, &mut std::io::stdout())?;
            Ok(Reply {
                text: String::new(),
                code,
            })
        }
        // What the worker said since the manager's turn: the rows a fullscreen
        // app scrolled away joined with the screen's.
        "report" => {
            let sub = parse_sub(verb, &opts.cmd[1..])?;
            no_positionals(verb, &sub)?;
            let ropts = report_opts(&sub);
            let mut session = Session::new(&mut client, sub.sid);
            let report = session.report(&ropts)?;
            Ok(Reply::text(report.render_view(ropts.view)))
        }
        "await" => {
            if opts.cmd.len() < 2 {
                return Err(
                    "await needs a condition: idle <ms> | match <regex> | gone <regex> | seq | block\n  \
                     e.g. `aterm-drive await match BUILD.SUCCESSFUL` or `aterm-drive await gone \
                     esc.to.interrupt` (a regex is ONE whitespace-free token: the wire never quotes)"
                        .to_string(),
                );
            }
            // Pass the condition straight through to the core verb, but supply the
            // tool's --timeout so a bare `await idle 500` still has a sane bound.
            let mut args: Vec<String> = opts.cmd[1..].to_vec();
            if !args.iter().any(|a| a == "timeout") {
                args.push("timeout".to_string());
                args.push(opts.timeout_ms.to_string());
            }
            let mut a: Vec<&str> = vec!["await"];
            a.extend(args.iter().map(String::as_str));
            client.run(&a).map(Reply::text)
        }
        other => Err(format!(
            "unknown command '{other}'. Valid: prompt | read | await | shot | classify | phase | \
             await-turn | supervise | watch | task | report | ledger | help.\n  \
             Run `aterm-drive --help` for the full guide."
        )),
    }
}

/// `aterm drive ledger`: read the worker's turn ledger, the watcher's journal,
/// this session's mail with it and the archive's reply sizes, and print the
/// one timeline (`--format text|md|html`, `--out PATH` to write it instead of
/// printing). The worker is `@sid`, or — with none — the session the journal's
/// own lines name.
fn ledger_verb(opts: &Opts, args: &[String]) -> Result<Reply, String> {
    let sub = parse_sub("ledger", args)?;
    no_positionals("ledger", &sub)?;
    let worker = match sub
        .sid
        .clone()
        .or_else(|| journal_sid(sub.journal.as_deref()))
    {
        Some(sid) => sid,
        None => {
            return Err(
                "ledger needs the worker: `aterm drive ledger @s-… ` (or a --journal \
                        whose lines name one)"
                    .to_string(),
            );
        }
    };
    let lopts = LedgerOpts {
        worker,
        journal: sub.journal.clone(),
        since_ms: sub.since_ms,
    };
    let ctl = resolve_ctl();
    let mut client = CtlClient::new(ctl, opts.socket.clone());
    let mut anchor = clock_anchor;
    let mut host = LedgerHost {
        now_ms: supervise::journal::unix_ms(),
        tz_offset_s: tz_offset_s(),
        anchor: &mut anchor,
    };
    let ledger = supervise::gather(&mut client, &lopts, &mut host);
    let text = supervise::render_ledger(&ledger, sub.format.unwrap_or_default());
    match &sub.out {
        None => Ok(Reply::text(text)),
        Some(path) => {
            write_private(path, &text)?;
            Ok(Reply::text(format!("wrote {}\n", path.display())))
        }
    }
}

/// Write `text` to `path`, created 0600 (a ledger carries what the manager
/// typed and what the worker said), truncating what was there.
fn write_private(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write as _;

    let mut o = std::fs::OpenOptions::new();
    o.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o
        .open(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.flush())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The sid the journal's own lines name, when they all name one.
fn journal_sid(path: Option<&std::path::Path>) -> Option<String> {
    let (records, _) = supervise::read_journal(path?).ok()?;
    let mut sids: Vec<&str> = records.iter().filter_map(|r| r.sid.as_deref()).collect();
    sids.sort_unstable();
    sids.dedup();
    match sids.as_slice() {
        [one] => Some((*one).to_string()),
        _ => None,
    }
}

/// Where the aterm process hosting `sid` started the clock its `history`,
/// `inbox` and `timeline` stamps count from: the BIRTH TIME of its control
/// socket, which it binds as it starts (measured on 2026-09-14: within 0.12 s
/// of the fabric bus's own wall-clock stamps on the same three messages,
/// where the process's start time — `ps -o lstart=` — was 5.3 s early,
/// because the clock is pinned lazily, at the first thing that asks for it).
fn clock_anchor(sid: &str) -> Result<ClockAnchor, String> {
    let sessions = aterm_ctl::fleet_sessions().map_err(|e| format!("`ls` found nothing: {e}"))?;
    let pid = sessions
        .iter()
        .find(|s| s.sid() == Some(sid))
        .map(|s| s.pid)
        .ok_or_else(|| format!("no live instance hosts @{sid}"))?;
    let instances =
        aterm_ctl::local_instances().map_err(|e| format!("`instances` found nothing: {e}"))?;
    let sock = instances
        .iter()
        .find(|(p, _)| *p == pid)
        .map(|(_, sock)| sock.clone())
        .ok_or_else(|| format!("instance {pid} has no control socket to date"))?;
    let meta = std::fs::metadata(&sock).map_err(|e| format!("{sock}: {e}"))?;
    let when = meta
        .created()
        .or_else(|_| meta.modified())
        .map_err(|e| format!("{sock}: no birth time: {e}"))?;
    let epoch_ms = when
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("{sock}: a birth time before 1970: {e}"))?
        .as_millis();
    Ok(ClockAnchor {
        pid,
        epoch_ms: i64::try_from(epoch_ms).unwrap_or(i64::MAX),
        how: "its control socket's birth time".to_string(),
    })
}

/// `supervise`'s and `watch`'s default budget: the longest a worker is left
/// unattended before the manager is told (30 min — the rate-limit wait the
/// owner chose).
const DEFAULT_MAX_S: u64 = 1800;

/// `supervise`'s and `watch`'s default `--context-warn`: the worker's context
/// left, in percent, at or below which `EVENT context` is said — room for
/// one turn that brings its handoff notes up to date before it compacts.
const DEFAULT_CONTEXT_WARN: u8 = 10;

/// `--reconnect-s`, when given: how long the loop rides out an outage (an
/// aterm self-update's handoff, from its first unserved request) before it
/// ends.
fn set_reconnect<C: supervise::Ctl>(session: &mut Session<'_, C>, reconnect_s: Option<u64>) {
    if let Some(s) = reconnect_s {
        session.set_reconnect(Duration::from_secs(s));
    }
}

/// The client `supervise` and `watch` run on: ONE persistent connection to
/// the socket `--socket` names (else the one `aterm-ctl` would resolve for
/// this terminal's session, re-resolved at every redial), authenticated with
/// `$ATERM_CONTROL_TOKEN` or the token beside the socket; where that cannot
/// be opened, `aterm-ctl` per request, with its own discovery and errors.
fn supervise_transport(opts: &Opts, ctl: &std::path::Path) -> Transport {
    let endpoint = match &opts.socket {
        Some(sock) => Endpoint::Socket(sock.clone()),
        None => Endpoint::Resolved {
            self_sid: std::env::var("ATERM_PARENT_SESSION_ID")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        },
    };
    let token = std::env::var("ATERM_CONTROL_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());
    Transport::open(
        endpoint,
        token,
        CtlClient::new(ctl.to_path_buf(), opts.socket.clone()),
    )
}

/// `--max-s`: seconds, `0` for no budget at all.
fn max_budget(max_s: Option<u64>) -> Duration {
    match max_s {
        Some(0) => supervise::run::UNBOUNDED,
        Some(s) => Duration::from_secs(s),
        None => Duration::from_secs(DEFAULT_MAX_S),
    }
}

fn supervise_opts(sub: &SubArgs) -> SuperviseOpts {
    SuperviseOpts {
        auto_reads: sub.auto_reads,
        max: max_budget(sub.max_s),
        python_allow: sub.allow_python.clone(),
        notes: sub.notes.clone(),
        report: sub.report,
        dismiss_surveys: sub.dismiss_surveys,
        context_warn: sub.context_warn.unwrap_or(DEFAULT_CONTEXT_WARN),
        journal: sub.journal.clone(),
        mail: sub.mail.then(|| MailOpts {
            inbox: sub.inbox.clone(),
            report_window: sub
                .report_window_s
                .map_or(supervise::DEFAULT_REPORT_WINDOW, Duration::from_secs),
            idle_grace: sub
                .idle_grace_s
                .map_or(supervise::DEFAULT_IDLE_GRACE, Duration::from_secs),
        }),
        policy: supervise::SupervisorConfig::cli(sub.resume.is_some()),
        resume: None,
        // `watch` behind another supervisor watches (`WATCHING …`).
        yield_when_held: false,
    }
}

/// `watch --resume [RULES]` as the loop takes it: a RULES file named must be
/// readable NOW (a run that dies on it after two days of waiting is the
/// wrong time to learn the path was mistyped), and not empty.
fn resume_opts(sub: &SubArgs) -> Result<Option<Resume>, String> {
    let Some(rules) = &sub.resume else {
        return Ok(None);
    };
    if let Some(path) = rules {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("watch --resume: cannot read RULES {}: {e}", path.display()))?;
        if text.trim().is_empty() {
            return Err(format!(
                "watch --resume: RULES {} is empty (the file typed with every continuation)",
                path.display()
            ));
        }
    }
    Ok(Some(Resume {
        rules: rules.clone(),
    }))
}

/// The manager's session for `watch`'s escalation mail: `--inbox @sid`, else
/// `@$ATERM_PARENT_SESSION_ID` (the session this terminal is), else none —
/// the loop then skips the mail and journals why.
fn manager_sid(sub: &SubArgs) -> Option<String> {
    sub.inbox.clone().or_else(|| {
        std::env::var("ATERM_PARENT_SESSION_ID")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("@{}", s.trim()))
    })
}

/// `--mail` folds a report only when it is the watched worker's, so the
/// worker must be named.
fn mail_needs_sid(verb: &str, sub: &SubArgs) -> Result<(), String> {
    if sub.mail && sub.sid.is_none() {
        return Err(format!(
            "{verb} --mail needs the worker's @sid (a report is folded into its turn only \
             when it is that worker's)"
        ));
    }
    Ok(())
}

/// `task @sid [--deadline S] [--wait] [--inbox @sid] <text>`.
fn task_opts(opts: &Opts, sub: &SubArgs) -> Result<TaskOpts, String> {
    let worker = sub
        .sid
        .clone()
        .ok_or("task needs the worker: `aterm drive task @s-… 'run the suite and report'`")?;
    let text = sub.rest.join(" ");
    if text.trim().is_empty() {
        return Err(
            "task needs the text, e.g. `aterm drive task @s-… 'run the suite and report'`"
                .to_string(),
        );
    }
    let deadline = sub.deadline_s.map(Duration::from_secs);
    Ok(TaskOpts {
        worker,
        inbox: sub
            .inbox
            .clone()
            .unwrap_or_else(|| supervise::mail::SELF.to_string()),
        text,
        deadline,
        wait: sub
            .wait
            .then(|| deadline.unwrap_or(Duration::from_millis(opts.timeout_ms))),
    })
}

fn report_opts(sub: &SubArgs) -> ReportOpts {
    ReportOpts {
        since: sub.since,
        max_rows: sub.max_rows.unwrap_or(supervise::DEFAULT_MAX_ROWS),
        view: sub.view,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sibling is found beside the RESOLVED binary: a launch link in a directory
    /// holding a stale `aterm-ctl` must still reach the bundle's own.
    #[cfg(unix)]
    #[test]
    fn the_ctl_sibling_is_read_beside_the_resolved_binary() {
        let dir = std::env::temp_dir().join(format!("aterm-drive-ctl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        let bundle = root.join("aterm.app/Contents/MacOS");
        let links = root.join("bin");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(&links).unwrap();
        std::fs::write(bundle.join("aterm"), b"").unwrap();
        std::fs::write(bundle.join("aterm-ctl"), b"").unwrap();
        std::fs::write(links.join("aterm-ctl"), b"stale copy").unwrap();
        std::os::unix::fs::symlink(bundle.join("aterm"), links.join("aterm")).unwrap();
        assert_eq!(
            sibling_ctl(&links.join("aterm")),
            Some(bundle.join("aterm-ctl"))
        );
        std::fs::remove_file(bundle.join("aterm-ctl")).unwrap();
        assert_eq!(
            sibling_ctl(&links.join("aterm")),
            None,
            "never the stale copy"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    use crate::supervise::limit::parse_zone;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `classify <cmd>`: the command is the positional text (joined), the
    /// verdict is the exit code, `--allow-python` REPLACES the python globs
    /// (`python_allow` falls back to the defaults only when none is given), and
    /// no command is a usage error.
    #[test]
    fn classify_parses_the_command_and_exits_on_the_verdict() {
        let r = classify_verb(&args(&["git status --short && git pull | tail"])).expect("ok");
        assert_eq!(
            r,
            Reply {
                text: "not-read-only git pull\n".to_string(),
                code: 1
            }
        );
        let r = classify_verb(&args(&["git", "log", "-3"])).expect("joined words");
        assert_eq!(
            r,
            Reply {
                text: "read-only\n".to_string(),
                code: 0
            }
        );
        // Unquoted, with a flag inside the command: the words after the first
        // are the command's, not classify's.
        let r = classify_verb(&args(&["git", "log", "--oneline"])).expect("a flag in the command");
        assert_eq!(r.code, 0, "{r:?}");
        let sub = parse_sub("classify", &args(&["git", "log", "--oneline", "-5"])).expect("parses");
        assert_eq!(sub.rest, args(&["git", "log", "--oneline", "-5"]));
        let sub = parse_sub("classify", &args(&["--", "--weird", "x"])).expect("-- ends the flags");
        assert_eq!(sub.rest, args(&["--weird", "x"]));
        let sub = parse_sub("classify", &args(&["@foo", "bar"])).expect("no sid for classify");
        assert_eq!((sub.sid, sub.rest), (None, args(&["@foo", "bar"])));
        let sub = parse_sub("supervise", &args(&["@s-1", "--", "extra"])).expect("parses");
        assert_eq!(
            (sub.sid.as_deref(), sub.rest),
            (Some("@s-1"), args(&["extra"]))
        );
        let r = classify_verb(&args(&["python3 tools/audit.py"])).expect("ok");
        assert_eq!(r.code, 1);
        let r = classify_verb(&args(&[
            "--allow-python",
            "tools/*.py",
            "python3 tools/audit.py",
        ]))
        .expect("ok");
        assert_eq!(r.code, 0, "{r:?}");
        let err = classify_verb(&args(&[])).expect_err("no command");
        assert!(err.contains("classify needs a shell command line"), "{err}");
        // Through the top-level parse: the verb and its text reach `run`.
        let o = parse(vec!["classify".into(), "ls -la".into()]).expect("parses");
        assert_eq!(o.cmd, args(&["classify", "ls -la"]));
        assert_eq!(run(&o).expect("pure verb needs no host").code, 0);
    }

    #[test]
    fn phase_parses_an_optional_sid_and_nothing_else() {
        let sub = parse_sub("phase", &args(&["@s-abc"])).expect("parses");
        assert_eq!(sub.sid.as_deref(), Some("@s-abc"));
        assert!(no_positionals("phase", &sub).is_ok());
        let sub = parse_sub("phase", &args(&[])).expect("parses");
        assert_eq!(sub.sid, None);
        let sub = parse_sub("phase", &args(&["@s-abc", "extra"])).expect("parses");
        let err = no_positionals("phase", &sub).expect_err("no positionals");
        assert!(err.contains("unexpected: extra"), "{err}");
        let err = parse_sub("phase", &args(&["--bogus"])).expect_err("unknown flag");
        assert!(err.contains("unknown option '--bogus'"), "{err}");
    }

    #[test]
    fn await_turn_parses_sid_and_timeout() {
        let sub = parse_sub("await-turn", &args(&["@s-1", "--timeout", "5000"])).expect("parses");
        assert_eq!(sub.sid.as_deref(), Some("@s-1"));
        assert_eq!(sub.timeout_ms, Some(5000));
        let sub = parse_sub("await-turn", &args(&["--timeout", "7"])).expect("no sid");
        assert_eq!((sub.sid, sub.timeout_ms), (None, Some(7)));
        let err = parse_sub("await-turn", &args(&["--timeout", "soon"])).expect_err("not an int");
        assert!(
            err.contains("--timeout needs a millisecond integer"),
            "{err}"
        );
        let err = parse_sub("await-turn", &args(&["--timeout"])).expect_err("missing");
        assert!(err.contains("--timeout needs"), "{err}");
    }

    #[test]
    fn supervise_parses_every_flag_and_repeats_allow_python() {
        let sub = parse_sub(
            "supervise",
            &args(&[
                "@s-9",
                "--auto-reads",
                "--max-s",
                "600",
                "--allow-python",
                "tools/*.py",
                "--allow-python",
                "scripts/*report*.py",
                "--notes",
                "/tmp/notes.txt",
            ]),
        )
        .expect("parses");
        assert_eq!(
            sub,
            SubArgs {
                sid: Some("@s-9".to_string()),
                timeout_ms: None,
                auto_reads: true,
                max_s: Some(600),
                reconnect_s: None,
                allow_python: args(&["tools/*.py", "scripts/*report*.py"]),
                notes: Some(PathBuf::from("/tmp/notes.txt")),
                journal: None,
                resume: None,
                view: View::All,
                format: None,
                out: None,
                since_ms: None,
                since: None,
                max_rows: None,
                report: false,
                dismiss_surveys: false,
                context_warn: None,
                mail: false,
                inbox: None,
                report_window_s: None,
                idle_grace_s: None,
                deadline_s: None,
                wait: false,
                rest: vec![],
            }
        );
        // Defaults: no auto-reads, no notes, and no python script is a read.
        let sub = parse_sub("supervise", &args(&[])).expect("parses");
        assert!(!sub.auto_reads && sub.notes.is_none() && sub.max_s.is_none());
        assert!(python_allow(&sub).is_empty());
        let err = parse_sub("supervise", &args(&["--max-s", "-1"])).expect_err("not a u64");
        assert!(err.contains("--max-s needs a seconds integer"), "{err}");
        let err = parse_sub("supervise", &args(&["--notes"])).expect_err("missing");
        assert!(err.contains("--notes needs a FILE"), "{err}");
    }

    /// `watch` takes supervise's flags, into the same options.
    #[test]
    fn watch_parses_supervises_flags_into_the_same_options() {
        let sub = parse_sub(
            "watch",
            &args(&[
                "@s-1e918c46",
                "--auto-reads",
                "--notes",
                "notes.txt",
                "--allow-python",
                "tools/*.py",
                "--max-s",
                "7200",
            ]),
        )
        .expect("parses");
        assert!(no_positionals("watch", &sub).is_ok());
        assert_eq!(sub.sid.as_deref(), Some("@s-1e918c46"));
        assert_eq!(
            supervise_opts(&sub),
            SuperviseOpts {
                auto_reads: true,
                max: Duration::from_secs(7200),
                python_allow: args(&["tools/*.py"]),
                notes: Some(PathBuf::from("notes.txt")),
                report: false,
                dismiss_surveys: false,
                context_warn: 10,
                journal: None,
                mail: None,
                policy: supervise::SupervisorConfig::cli(false),
                resume: None,
                yield_when_held: false,
            }
        );
        // The CLI types no continuation, retries nothing and switches no
        // model unless a flag asks: those switches are off in its policy.
        let p = supervise_opts(&sub).policy;
        assert!(!p.continue_policy && !p.retry_api_errors && p.model_fallback.is_none());
        assert_eq!(p.approvals, supervise::ApprovalToggles::default());
        // `--max-s 0` is no budget at all.
        let sub = parse_sub("watch", &args(&["@s-1", "--max-s", "0"])).expect("parses");
        assert_eq!(supervise_opts(&sub).max, supervise::UNBOUNDED);
        let sub = parse_sub("watch", &args(&["@s-1", "--report"])).expect("parses");
        assert!(supervise_opts(&sub).report, "watch --report");
        let sub = parse_sub("watch", &args(&[])).expect("parses");
        assert_eq!(supervise_opts(&sub).max, Duration::from_secs(DEFAULT_MAX_S));
        assert_eq!(sub.reconnect_s, None, "the loop's own default");
        // How long an outage is ridden out: await-turn's, supervise's
        // and watch's flag alike.
        for verb in ["watch", "supervise", "await-turn"] {
            let sub = parse_sub(verb, &args(&["@s-1", "--reconnect-s", "30"])).expect("parses");
            assert_eq!(
                (sub.sid.as_deref(), sub.reconnect_s),
                (Some("@s-1"), Some(30))
            );
        }
        let err = parse_sub("watch", &args(&["--reconnect-s", "soon"])).expect_err("not an int");
        assert!(
            err.contains("watch: --reconnect-s needs a seconds integer"),
            "{err}"
        );
        let err = parse_sub("watch", &args(&["--every", "5"])).expect_err("unknown flag");
        assert!(err.contains("watch: unknown option '--every'"), "{err}");
        // A failure before the loop is an `EXIT` line on stdout too.
        assert_eq!(
            watch_exit_line(&args(&["watch", "--every", "5"]), &err),
            Some(format!("EXIT {}", err.lines().next().expect("a line")))
        );
        assert_eq!(
            watch_exit_line(
                &args(&["watch"]),
                "cannot reach a target aterm over the control socket (refused).\n  • Is a host aterm running?"
            )
            .as_deref(),
            Some("EXIT cannot reach a target aterm over the control socket (refused).")
        );
        assert_eq!(watch_exit_line(&args(&["supervise"]), &err), None);
    }

    /// `--mail` and its knobs are watch's and supervise's, into the same
    /// option (`None` unless given, so nothing changes without the flag);
    /// the loop needs the worker's @sid to fold only its reports; `--inbox`
    /// is theirs and task's, and must be a `@sid`.
    #[test]
    fn mail_flags_are_the_loops_and_need_the_worker() {
        let sub = parse_sub(
            "watch",
            &args(&[
                "@s-1",
                "--mail",
                "--inbox",
                "@s-9",
                "--report-window",
                "60",
                "--idle-grace",
                "90",
            ]),
        )
        .expect("parses");
        assert!(no_positionals("watch", &sub).is_ok() && mail_needs_sid("watch", &sub).is_ok());
        assert_eq!(
            supervise_opts(&sub).mail,
            Some(MailOpts {
                inbox: Some("@s-9".to_string()),
                report_window: Duration::from_secs(60),
                idle_grace: Duration::from_secs(90),
            })
        );
        let sub = parse_sub("supervise", &args(&["@s-1", "--mail"])).expect("parses");
        assert_eq!(
            supervise_opts(&sub).mail,
            Some(MailOpts {
                inbox: None,
                report_window: supervise::DEFAULT_REPORT_WINDOW,
                idle_grace: supervise::DEFAULT_IDLE_GRACE,
            })
        );
        let sub = parse_sub("watch", &args(&["@s-1"])).expect("parses");
        assert_eq!(supervise_opts(&sub).mail, None, "off unless given");
        let sub = parse_sub("watch", &args(&["--mail"])).expect("parses");
        let err = mail_needs_sid("watch", &sub).expect_err("no worker");
        assert!(
            err.starts_with("watch --mail needs the worker's @sid"),
            "{err}"
        );
        for verb in ["phase", "await-turn", "report", "task"] {
            let err = parse_sub(verb, &args(&["--mail"])).expect_err(verb);
            assert!(
                err.starts_with(&format!("{verb}: --mail is watch's and supervise's")),
                "{err}"
            );
        }
        let err = parse_sub("report", &args(&["--inbox", "@s-1"])).expect_err("report");
        assert!(
            err.starts_with("report: --inbox is watch's, supervise's and task's"),
            "{err}"
        );
        let err = parse_sub("watch", &args(&["--inbox", "s-1"])).expect_err("no @");
        assert!(err.contains("--inbox needs @<sid>"), "{err}");
        let err = parse_sub("watch", &args(&["--idle-grace", "soon"])).expect_err("not an int");
        assert!(
            err.contains("--idle-grace needs a seconds integer"),
            "{err}"
        );
        let err = parse_sub("phase", &args(&["--report-window", "5"])).expect_err("phase");
        assert!(
            err.starts_with("phase: --report-window is watch's and supervise's"),
            "{err}"
        );
    }

    /// `watch --resume [RULES]`: watch's alone; the file is the next word
    /// unless that is a flag or the worker's @sid; the loop gets it only
    /// when it can be read now, and never empty; the escalation mail's
    /// address is `--inbox`, else this terminal's own session.
    #[test]
    fn resume_is_watchs_and_takes_an_optional_rules_file() {
        let sub = parse_sub("watch", &args(&["@s-1", "--resume"])).expect("parses");
        assert_eq!(sub.resume, Some(None));
        assert_eq!(
            resume_opts(&sub).expect("no file to read"),
            Some(Resume { rules: None })
        );
        // The file after the flag; a flag or the sid after it is not one.
        let sub = parse_sub("watch", &args(&["--resume", "rules.md", "@s-1"])).expect("parses");
        assert_eq!(
            (sub.resume, sub.sid.as_deref()),
            (Some(Some(PathBuf::from("rules.md"))), Some("@s-1"))
        );
        let sub = parse_sub("watch", &args(&["--resume", "@s-1", "--report"])).expect("parses");
        assert_eq!(
            (sub.resume, sub.sid.as_deref(), sub.report),
            (Some(None), Some("@s-1"), true)
        );
        let sub = parse_sub("watch", &args(&["--resume", "--mail", "@s-1"])).expect("parses");
        assert_eq!((sub.resume, sub.mail), (Some(None), true));
        let sub = parse_sub("watch", &args(&["@s-1"])).expect("parses");
        assert_eq!(resume_opts(&sub), Ok(None), "off unless given");
        assert_eq!(sub.resume, None);
        for verb in [
            "supervise",
            "phase",
            "await-turn",
            "report",
            "task",
            "ledger",
        ] {
            let err = parse_sub(verb, &args(&["--resume"])).expect_err(verb);
            assert!(
                err.starts_with(&format!("{verb}: --resume is watch's")),
                "{err}"
            );
        }
        // The rules file is read at the launch: missing or empty is the error.
        let dir = std::env::temp_dir().join(format!("aterm-drive-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let missing = dir.join("nope.md");
        let sub = parse_sub("watch", &args(&["--resume", missing.to_str().unwrap()])).unwrap();
        let err = resume_opts(&sub).expect_err("missing");
        assert!(
            err.starts_with("watch --resume: cannot read RULES"),
            "{err}"
        );
        let empty = dir.join("empty.md");
        std::fs::write(&empty, "  \n").expect("write");
        let sub = parse_sub("watch", &args(&["--resume", empty.to_str().unwrap()])).unwrap();
        let err = resume_opts(&sub).expect_err("empty");
        assert!(err.contains("is empty"), "{err}");
        let rules = dir.join("rules.md");
        std::fs::write(&rules, "1. run nothing heavy\n").expect("write");
        let sub = parse_sub("watch", &args(&["--resume", rules.to_str().unwrap()])).unwrap();
        assert_eq!(
            resume_opts(&sub).expect("readable"),
            Some(Resume {
                rules: Some(rules.clone())
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
        // The manager's address: --inbox first.
        let sub = parse_sub("watch", &args(&["@s-1", "--inbox", "@s-9"])).unwrap();
        assert_eq!(manager_sid(&sub).as_deref(), Some("@s-9"));
        let sub = parse_sub("watch", &args(&["@s-1"])).unwrap();
        let own = std::env::var("ATERM_PARENT_SESSION_ID")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("@{}", s.trim()));
        assert_eq!(
            manager_sid(&sub),
            own,
            "this terminal's own session, if any"
        );
    }

    /// `task @sid [--deadline S] [--wait] [--inbox @sid] <text>`: the text is
    /// the positionals, the deadline rides on the post and bounds `--wait`
    /// (else the global `--timeout` does); its flags are refused everywhere
    /// else, and `--no-nudge` is no flag at all — the typed nudge is the verb.
    #[test]
    fn task_parses_the_worker_its_flags_and_the_text() {
        let opts = parse(vec!["task".into()]).expect("parses");
        let sub = parse_sub(
            "task",
            &args(&["@s-1", "--deadline", "600", "--wait", "run it", "now"]),
        )
        .expect("parses");
        assert_eq!(
            task_opts(&opts, &sub).expect("a task"),
            TaskOpts {
                worker: "@s-1".to_string(),
                inbox: "@self".to_string(),
                text: "run it now".to_string(),
                deadline: Some(Duration::from_secs(600)),
                wait: Some(Duration::from_secs(600)),
            }
        );
        let sub =
            parse_sub("task", &args(&["@s-1", "--wait", "--inbox", "@s-9", "x"])).expect("parses");
        let t = task_opts(&opts, &sub).expect("a task");
        assert_eq!(
            (t.inbox.as_str(), t.deadline, t.wait),
            ("@s-9", None, Some(Duration::from_millis(180_000)))
        );
        let sub = parse_sub("task", &args(&["@s-1", "x"])).expect("parses");
        assert_eq!(task_opts(&opts, &sub).expect("a task").wait, None);
        let sub = parse_sub("task", &args(&["x"])).expect("parses");
        let err = task_opts(&opts, &sub).expect_err("no worker");
        assert!(err.starts_with("task needs the worker"), "{err}");
        let sub = parse_sub("task", &args(&["@s-1"])).expect("parses");
        let err = task_opts(&opts, &sub).expect_err("no text");
        assert!(err.starts_with("task needs the text"), "{err}");
        for (verb, flag) in [
            ("watch", "--wait"),
            ("supervise", "--wait"),
            ("report", "--deadline"),
        ] {
            let err = parse_sub(verb, &args(&[flag, "5"])).expect_err(verb);
            assert!(
                err.starts_with(&format!("{verb}: {flag} is task's")),
                "{err}"
            );
        }
        let err = parse_sub("task", &args(&["@s-1", "--no-nudge", "x"])).expect_err("gone");
        assert!(
            err.starts_with("task: unknown option '--no-nudge'"),
            "{err}"
        );
    }

    /// `await-turn` prints its turn exactly like `phase` — the `survey 0`
    /// line included while the session survey is open — and exits 124 on a
    /// turn the timeout cut short; with no survey the text is the phase's
    /// alone.
    #[test]
    fn await_turn_prints_like_phase_the_survey_line_included() {
        let rule = "─".repeat(120);
        let body = [
            "⏺ Done.",
            "",
            "✻ Cogitated for 4s · done 2:41 PM",
            "",
            "● How is Claude doing this session? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
            &rule,
            "❯",
            &rule,
            "  ? for shortcuts",
        ];
        let turn = |rows: Vec<String>, timed_out| supervise::Turn {
            phase: worker_phase(&rows),
            screen: supervise::Screen {
                rows,
                ..supervise::Screen::default()
            },
            timed_out,
        };
        let rows: Vec<String> = body.iter().map(|r| r.to_string()).collect();
        let r = phase_reply(&turn(rows.clone(), false), &[]);
        assert_eq!((r.text.as_str(), r.code), ("idle\nsurvey 0\n", 0));
        let r = phase_reply(&turn(rows.clone(), true), &[]);
        assert_eq!(
            (r.text.as_str(), r.code),
            ("idle\nsurvey 0\n", EXIT_TIMEOUT)
        );
        let plain: Vec<String> = rows
            .into_iter()
            .filter(|r| !r.contains("How is Claude doing") && !r.contains("0: Dismiss"))
            .collect();
        assert_eq!(phase_reply(&turn(plain, false), &[]).text, "idle\n");
    }

    /// `--dismiss-surveys` is watch's and supervise's — the loops that see
    /// the session survey appear — into the same option, off unless given;
    /// every other verb refuses it rather than take it silently.
    #[test]
    fn dismiss_surveys_is_watchs_and_supervises_only() {
        for verb in ["watch", "supervise"] {
            let sub = parse_sub(verb, &args(&["@s-1", "--dismiss-surveys", "--auto-reads"]))
                .expect("parses");
            assert!(no_positionals(verb, &sub).is_ok());
            assert_eq!(sub.sid.as_deref(), Some("@s-1"));
            assert!(sub.dismiss_surveys && sub.auto_reads, "{verb}");
            assert!(supervise_opts(&sub).dismiss_surveys, "{verb}");
            let sub = parse_sub(verb, &args(&["@s-1"])).expect("parses");
            assert!(
                !supervise_opts(&sub).dismiss_surveys,
                "{verb}: off by default"
            );
        }
        for verb in ["phase", "await-turn", "report", "classify"] {
            let err = parse_sub(verb, &args(&["--dismiss-surveys"])).expect_err(verb);
            assert!(
                err.starts_with(&format!(
                    "{verb}: --dismiss-surveys is watch's and supervise's"
                )),
                "{err}"
            );
        }
    }

    /// `--context-warn <pct>` is watch's and supervise's — the loops that see
    /// the worker's context run low and compact — a percentage from 0 (off)
    /// to 100, 10 unless given. It needs a value; anything else, or a verb
    /// that is not a loop, is refused rather than taken silently.
    #[test]
    fn context_warn_is_watchs_and_supervises_a_percentage() {
        // What a value that is not one is told, after the verb.
        const NEEDS: &str = ": --context-warn needs a percentage from 0 to 100 (0: off)";
        for verb in ["watch", "supervise"] {
            let sub = parse_sub(verb, &args(&["@s-1", "--context-warn", "25"])).expect("parses");
            assert!(no_positionals(verb, &sub).is_ok());
            assert_eq!(sub.sid.as_deref(), Some("@s-1"));
            assert_eq!(sub.context_warn, Some(25), "{verb}");
            assert_eq!(supervise_opts(&sub).context_warn, 25, "{verb}");
            for (given, want) in [("0", 0), ("100", 100), ("7", 7)] {
                let sub = parse_sub(verb, &args(&["--context-warn", given])).expect(given);
                assert_eq!(supervise_opts(&sub).context_warn, want, "{verb} {given}");
            }
            let sub = parse_sub(verb, &args(&["@s-1"])).expect("parses");
            assert_eq!(sub.context_warn, None, "{verb}");
            // 10 unless given.
            assert_eq!(supervise_opts(&sub).context_warn, 10, "{verb}");
            for bad in ["101", "256", "-1", "ten", "", "1.5", "10%"] {
                let err = parse_sub(verb, &args(&["--context-warn", bad])).expect_err(bad);
                assert_eq!(err, format!("{verb}{NEEDS}"), "{bad}");
            }
            let err = parse_sub(verb, &args(&["--context-warn"])).expect_err("no value");
            assert_eq!(err, format!("{verb}{NEEDS}"));
        }
        for verb in ["phase", "await-turn", "report", "classify"] {
            let err = parse_sub(verb, &args(&["--context-warn", "10"])).expect_err(verb);
            assert!(
                err.starts_with(&format!(
                    "{verb}: --context-warn is watch's and supervise's"
                )),
                "{err}"
            );
        }
    }

    /// `phase` and `await-turn` end with `context <n>%` while Claude Code's
    /// context indicator is up above the composer — after the `survey 0`
    /// line when the survey is open too — and a timed-out turn still exits
    /// 124; without the indicator the text is as before.
    #[test]
    fn phase_and_await_turn_end_with_the_context_left() {
        let rule = "─".repeat(120);
        let indicator = format!("{}1% until auto-compact", " ".repeat(97));
        let body = [
            "⏺ Done.",
            "",
            "✻ Cogitated for 4s · done 2:41 PM",
            "",
            "● How is Claude doing this session? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
            &indicator,
            &rule,
            "❯",
            &rule,
            "  ? for shortcuts",
        ];
        let turn = |rows: Vec<String>, timed_out| supervise::Turn {
            phase: worker_phase(&rows),
            screen: supervise::Screen {
                rows,
                ..supervise::Screen::default()
            },
            timed_out,
        };
        let rows: Vec<String> = body.iter().map(|r| r.to_string()).collect();
        let r = phase_reply(&turn(rows.clone(), false), &[]);
        assert_eq!(
            (r.text.as_str(), r.code),
            ("idle\nsurvey 0\ncontext 1%\n", 0)
        );
        let r = phase_reply(&turn(rows.clone(), true), &[]);
        assert_eq!(
            (r.text.as_str(), r.code),
            ("idle\nsurvey 0\ncontext 1%\n", EXIT_TIMEOUT)
        );
        let no_survey: Vec<String> = rows
            .iter()
            .filter(|r| !r.contains("How is Claude doing") && !r.contains("0: Dismiss"))
            .cloned()
            .collect();
        assert_eq!(
            phase_reply(&turn(no_survey.clone(), false), &[]).text,
            "idle\ncontext 1%\n"
        );
        let plain: Vec<String> = no_survey
            .into_iter()
            .filter(|r| !r.contains("until auto-compact"))
            .collect();
        assert_eq!(phase_reply(&turn(plain, false), &[]).text, "idle\n");
    }

    /// `report [@sid] [--since ORIGIN:I] [--max-rows N]`: the mark is a
    /// report's `last=` (or a bare index), the row cap is positive, and the
    /// defaults are your turn and 8000 rows.
    #[test]
    fn report_parses_since_and_max_rows() {
        let sub = parse_sub(
            "report",
            &args(&["@s-9", "--since", "7730:1291", "--max-rows", "500"]),
        )
        .expect("parses");
        assert!(no_positionals("report", &sub).is_ok());
        assert_eq!(sub.sid.as_deref(), Some("@s-9"));
        assert_eq!(
            report_opts(&sub),
            ReportOpts {
                since: Some(Mark {
                    origin: Some(7730),
                    index: 1291
                }),
                max_rows: 500,
                view: View::All,
            }
        );
        let sub = parse_sub("report", &args(&["--since", "42"])).expect("a bare index");
        assert_eq!(
            sub.since,
            Some(Mark {
                origin: None,
                index: 42
            })
        );
        let sub = parse_sub("report", &args(&[])).expect("parses");
        assert_eq!(report_opts(&sub), ReportOpts::default());
        assert_eq!(ReportOpts::default().max_rows, 8000);
        for bad in ["", "x", "1:", ":1", "-1", "+3", "1:2:3", "7730: 1"] {
            let err = parse_sub("report", &args(&["--since", bad])).expect_err(bad);
            assert!(
                err.contains("report: --since needs ORIGIN:INDEX"),
                "{bad}: {err}"
            );
        }
        let err = parse_sub("report", &args(&["--since"])).expect_err("missing");
        assert!(err.contains("--since needs"), "{err}");
        for bad in ["0", "-5", "many"] {
            let err = parse_sub("report", &args(&["--max-rows", bad])).expect_err(bad);
            assert!(
                err.contains("report: --max-rows needs a positive row count"),
                "{bad}: {err}"
            );
        }
        let sub = parse_sub("report", &args(&["@s-9", "extra"])).expect("parses");
        let err = no_positionals("report", &sub).expect_err("no positionals");
        assert!(err.contains("unexpected: extra"), "{err}");
    }

    fn prompt_opts(socket: String) -> Opts {
        Opts {
            socket: Some(socket),
            dial: None,
            idle_ms: 7,
            timeout_ms: 1_000,
            ready: Some(String::new()),
            cmd: vec!["prompt".to_string(), "hello".to_string()],
        }
    }

    fn unique_endpoint() -> (std::path::PathBuf, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aterm-agent-fast-path-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("create unique endpoint directory");
        (dir.join("control.sock"), dir)
    }

    /// The prompt-ready pattern must be a KNOB, not a constant: the default only
    /// matches a Claude input caret, so driving any other REPL — or a plain
    /// shell, which wants no confirm at all — needs an override.
    #[test]
    fn ready_pattern_precedence_flag_beats_env_beats_default() {
        let dflt = crate::claude_prompt_ready_pattern();

        // Flag wins outright (no env read needed for this branch).
        assert_eq!(resolve_ready(Some(r"^\$ ".to_string())), r"^\$ ");

        // An EMPTY flag is meaningful — idle-only — and must NOT fall through
        // to the default. This is the branch a naive `filter(|s| !s.is_empty())`
        // would silently break.
        assert_eq!(resolve_ready(Some(String::new())), "");

        // No flag, no env -> the built-in default.
        // (Guarded: another test in this process could have set the var.)
        if std::env::var("ATERM_DRIVE_READY").is_err() {
            assert_eq!(resolve_ready(None), dflt);
        }
    }

    /// `--ready` takes its value verbatim, including an empty string, and a
    /// missing value is a clear usage error rather than swallowing the command.
    #[test]
    fn ready_flag_parses_and_requires_a_value() {
        let os = |v: &[&str]| -> Vec<std::ffi::OsString> {
            v.iter().map(std::ffi::OsString::from).collect()
        };

        let o = parse(os(&["--ready", r"^> ", "prompt", "hi"])).expect("parses");
        assert_eq!(o.ready.as_deref(), Some(r"^> "));
        assert_eq!(o.cmd, vec!["prompt".to_string(), "hi".to_string()]);

        let empty = parse(os(&["--ready", "", "read"])).expect("empty value is legal");
        assert_eq!(empty.ready.as_deref(), Some(""), "'' means idle-only");

        let err = parse(os(&["--ready"])).expect_err("a missing value is an error");
        assert!(err.contains("--ready needs a REGEX"), "actionable: {err}");
    }

    /// Regression: the drive CLI must not re-derive the token path itself. The
    /// shared helper is the single source of truth for the convention.
    ///
    /// This used to pin the RESULT (`…/aterm.token`) rather than the agreement,
    /// which made it a second statement of the convention — and it went red the
    /// day the convention moved: an explicit socket now pairs with a token named
    /// after ITSELF, because one shared `aterm.token` per directory meant two
    /// private instances there overwrote each other's credential (F9). The guard
    /// that actually holds is the one asserted here — whatever the rule says, the
    /// drive CLI says the same thing, because it asks the same function.
    #[test]
    fn token_path_uses_the_shared_convention_not_a_stem_swap() {
        for sock in ["/tmp/run/c.sock", "/tmp/run/aterm-42.sock", "/tmp/run/ctl"] {
            let resolved = aterm_uds::latest::token_path_for_sock(sock).expect("resolves");
            // Named through the SAME mirror the CLI itself calls, so this crate
            // gains no dependency to state the rule twice.
            let expected = std::path::Path::new(sock)
                .parent()
                .expect("a directory")
                .join(aterm_uds::latest::token_name_for_sock(
                    std::path::Path::new(sock)
                        .file_name()
                        .expect("a file name")
                        .to_string_lossy()
                        .as_ref(),
                ));
            assert_eq!(
                resolved, expected,
                "{sock}: the drive CLI must name the token the shared rule names"
            );
        }
        // And the hand-rolled stem swap this test was born to forbid stays gone:
        // `c.sock` pairs with `c.sock.token`, never `c.token`.
        let p = aterm_uds::latest::token_path_for_sock("/tmp/run/c.sock").expect("resolves");
        assert!(
            !p.to_string_lossy().ends_with("/c.token"),
            "the old hand-rolled <stem>.token derivation is gone, got {p:?}"
        );
    }

    #[test]
    fn configured_endpoint_uses_one_persistent_route_and_flag_precedence() {
        let endpoint = resolve_configured_local_endpoint_with(
            Some("/run/flag.sock".to_string()),
            Some("/run/env.sock".to_string()),
            Some("capability".to_string()),
            |_| panic!("an env token must avoid a token-file read"),
        )
        .expect("configured endpoint resolves");

        assert_eq!(
            local_prompt_route(Ok(endpoint)),
            LocalPromptRoute::Persistent {
                socket: "/run/flag.sock".to_string(),
                token: "capability".to_string(),
            },
            "the prompt fast path receives the exact explicit endpoint and token"
        );
    }

    #[test]
    fn configured_prompt_uses_one_connection_and_no_cursor_preflight() {
        use std::io::{BufRead, BufReader, Write};

        let (socket, dir) = unique_endpoint();
        let socket_text = socket.to_string_lossy().into_owned();
        let token_path = aterm_uds::latest::token_path_for_sock(&socket_text).expect("token path");
        std::fs::write(&token_path, "capability\n").expect("write token");
        let listener = aterm_uds::CtlListener::bind(&socket).expect("bind endpoint");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one persistent connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("write timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut writer = stream;
            let mut requests = Vec::new();

            for reply in [b"".as_slice(), b"OK\n", b"OK\n", b"OK idle 1\n"] {
                let mut request = String::new();
                reader.read_line(&mut request).expect("request line");
                requests.push(request.trim_end().to_string());
                writer.write_all(reply).expect("reply");
            }
            let mut request = String::new();
            reader.read_line(&mut request).expect("text request");
            requests.push(request.trim_end().to_string());
            writer
                .write_all(b"OK 2\nsettled\n> \n")
                .expect("text reply");
            requests
        });

        let out = run_persistent_local_prompt(
            &prompt_opts(socket_text.clone()),
            LocalPromptRoute::Persistent {
                socket: socket_text,
                token: "capability".to_string(),
            },
        )
        .expect("persistent route selected")
        .expect("persistent prompt turn");
        let requests = server.join().expect("server completes");

        assert_eq!(out, "settled\n> \n");
        assert_eq!(
            requests,
            [
                "AUTH capability",
                "send hello",
                "key enter",
                "await idle 7 timeout 1000",
                "text",
            ],
            "one connection carries the whole Turn and never sends a cursor preflight"
        );

        std::fs::remove_file(&socket).expect("remove socket");
        std::fs::remove_file(&token_path).expect("remove token");
        std::fs::remove_dir(dir).expect("remove endpoint directory");
    }

    #[test]
    fn absent_or_disabled_endpoint_preserves_shell_discovery_fallback() {
        for flag in [
            None,
            Some(String::new()),
            Some("0".to_string()),
            Some("off".to_string()),
        ] {
            let endpoint = resolve_configured_local_endpoint_with(flag, None, None, |_| {
                panic!("fallback must not probe a token file")
            })
            .expect("no configured endpoint is not an error");
            assert_eq!(
                local_prompt_route(Ok(endpoint)),
                LocalPromptRoute::ShellDiscovery,
                "None selects the existing shell discovery path"
            );
        }
    }

    #[test]
    fn env_endpoint_resolves_the_per_socket_token_file() {
        let endpoint = resolve_configured_local_endpoint_with(
            None,
            Some("/tmp/run/custom.sock".to_string()),
            None,
            |path| {
                assert!(
                    path.ends_with("custom.sock.token"),
                    "per-socket token path: {path:?}"
                );
                Some("  file-capability\n".to_string())
            },
        )
        .expect("env endpoint resolves");

        assert_eq!(
            endpoint,
            Some((
                "/tmp/run/custom.sock".to_string(),
                "file-capability".to_string()
            ))
        );
    }

    #[test]
    fn unresolved_token_preserves_shell_diagnostics_while_dial_still_gets_the_error() {
        let err = resolve_configured_local_endpoint_with(
            None,
            Some("/tmp/run/custom.sock".to_string()),
            None,
            |_| None,
        )
        .expect_err("the endpoint itself cannot resolve without its token");

        assert!(
            err.contains("could not resolve the LOCAL control token"),
            "{err}"
        );
        assert_eq!(
            local_prompt_route(Err(err)),
            LocalPromptRoute::ShellDiscovery,
            "local prompt retains aterm-ctl's existing classified error path"
        );
    }

    #[test]
    fn resolved_but_unreachable_endpoint_surfaces_connect_error_without_retargeting() {
        let (socket, dir) = unique_endpoint();
        let socket_text = socket.to_string_lossy().into_owned();
        let token_path = aterm_uds::latest::token_path_for_sock(&socket_text).expect("token path");
        std::fs::write(&token_path, "capability\n").expect("write token");

        let err = run(&prompt_opts(socket_text))
            .expect_err("a resolved endpoint must not silently retarget");
        assert!(
            err.contains("cannot reach the configured target aterm"),
            "actionable endpoint error: {err}"
        );

        std::fs::remove_file(token_path).expect("remove token");
        std::fs::remove_dir(dir).expect("remove endpoint directory");
    }

    /// The round-11 flags belong to their verbs: `--journal` to the loops
    /// that write one and to the ledger that reads it back, `--final` and
    /// `--messages` to `report`, `--format` and `--out` to `ledger`, whose
    /// `--since` is a TIME where every other verb's is an archive mark.
    #[test]
    fn the_round_11_flags_belong_to_their_verbs() {
        for verb in ["watch", "supervise"] {
            let sub = parse_sub(verb, &args(&["@s-1", "--journal", "/tmp/j.jsonl"])).expect(verb);
            assert!(no_positionals(verb, &sub).is_ok());
            assert_eq!(sub.journal, Some(PathBuf::from("/tmp/j.jsonl")));
            assert_eq!(
                supervise_opts(&sub).journal,
                Some(PathBuf::from("/tmp/j.jsonl")),
                "{verb} hands it to the loop"
            );
            let sub = parse_sub(verb, &args(&["@s-1"])).expect(verb);
            assert_eq!(supervise_opts(&sub).journal, None, "{verb}: off by default");
        }
        let sub = parse_sub(
            "ledger",
            &args(&[
                "@s-1",
                "--journal",
                "j.jsonl",
                "--format",
                "md",
                "--out",
                "out.md",
                "--since",
                "1789344000000",
            ]),
        )
        .expect("parses");
        assert!(no_positionals("ledger", &sub).is_ok());
        assert_eq!(
            (
                sub.sid.as_deref(),
                sub.journal.as_deref(),
                sub.format,
                sub.out.as_deref(),
                sub.since_ms
            ),
            (
                Some("@s-1"),
                Some(std::path::Path::new("j.jsonl")),
                Some(LedgerFormat::Md),
                Some(std::path::Path::new("out.md")),
                Some(1_789_344_000_000)
            )
        );
        // The formats, and what is not one.
        for (word, want) in [
            ("text", LedgerFormat::Text),
            ("md", LedgerFormat::Md),
            ("markdown", LedgerFormat::Md),
            ("html", LedgerFormat::Html),
        ] {
            let sub = parse_sub("ledger", &args(&["--format", word])).expect(word);
            assert_eq!(sub.format, Some(want), "{word}");
        }
        for bad in ["", "HTML", "pdf", "json"] {
            let err = parse_sub("ledger", &args(&["--format", bad])).expect_err(bad);
            assert_eq!(err, "ledger: --format needs text, md or html", "{bad}");
        }
        let err = parse_sub("ledger", &args(&["--format"])).expect_err("no value");
        assert!(err.contains("--format needs"), "{err}");
        // A time word, or Unix milliseconds; an archive mark is not one.
        let sub = parse_sub("ledger", &args(&["--since", "2026-09-14T10:30:00Z"])).expect("a time");
        assert_eq!(sub.since_ms, Some(1_789_381_800_000));
        assert_eq!(sub.since, None, "ledger reads no archive mark");
        for bad in ["7730:1291", "yesterday", "2026-13-01"] {
            let err = parse_sub("ledger", &args(&["--since", bad])).expect_err(bad);
            assert!(
                err.starts_with("ledger: --since needs a time:"),
                "{bad}: {err}"
            );
        }
        // `report`'s `--since` is still the archive mark it always was.
        let sub = parse_sub("report", &args(&["--since", "7730:1291"])).expect("a mark");
        assert_eq!(sub.since.map(|m| m.index), Some(1291));
        assert_eq!(sub.since_ms, None);
        // The two report views: one of them, and only on `report`.
        let sub = parse_sub("report", &args(&["@s-1", "--final"])).expect("parses");
        assert!(no_positionals("report", &sub).is_ok());
        assert_eq!(
            (sub.view, report_opts(&sub).view),
            (View::Final, View::Final)
        );
        let sub = parse_sub("report", &args(&["--messages"])).expect("parses");
        assert_eq!(sub.view, View::Messages);
        assert_eq!(
            parse_sub("report", &args(&[])).expect("parses").view,
            View::All,
            "the whole report unless a view is asked for"
        );
        let err = parse_sub("report", &args(&["--final", "--messages"])).expect_err("two views");
        assert_eq!(
            err,
            "report: --final and --messages are two views; pass one"
        );
        assert!(
            parse_sub("report", &args(&["--final", "--final"])).is_ok(),
            "the same view twice"
        );
        // Every flag refuses the verbs it is not for, by name.
        for (verb, flag, says) in [
            (
                "report",
                "--journal",
                "--journal is watch's and supervise's",
            ),
            ("phase", "--journal", "--journal is watch's and supervise's"),
            ("watch", "--final", "--final is report's"),
            ("ledger", "--messages", "--messages is report's"),
            ("report", "--format", "--format is ledger's"),
            ("watch", "--out", "--out is ledger's"),
        ] {
            let err = parse_sub(verb, &args(&[flag, "x"])).expect_err(flag);
            assert!(
                err.starts_with(&format!("{verb}: {says}")),
                "{verb} {flag}: {err}"
            );
        }
    }

    /// **`--dial` IS REFUSED FOR `ledger`, not silently ignored.**
    ///
    /// The ledger needs no control-socket preflight, so it was dispatched
    /// ABOVE the `--dial` guard — and `aterm drive --dial nonexistent-box
    /// ledger @sid` read the LOCAL host and printed a full report of the wrong
    /// machine under a remote name, at exit 0, while `--dial ... report @sid`
    /// on the same machine said "`--dial` supports the `prompt` command".
    #[test]
    fn dial_is_refused_for_ledger_like_every_other_non_prompt_verb() {
        for verb in ["ledger", "report", "watch", "read"] {
            let err = run(&Opts {
                socket: None,
                dial: Some("nonexistent-box".to_string()),
                idle_ms: 1,
                timeout_ms: 1,
                ready: None,
                cmd: vec![verb.to_string(), "@s-1".to_string()],
            })
            .expect_err("--dial drives only the prompt loop");
            assert!(
                err.starts_with(&format!(
                    "--dial supports the `prompt` command (the drive loop); got `{verb}`."
                )),
                "{verb}: {err}"
            );
        }
    }

    /// **THE `EXIT` LINE OF A PRE-LOOP FAILURE REACHES THE JOURNAL.**
    ///
    /// `watch --journal FILE` promises one record for EVERY line it prints. A
    /// run that dies before the loop — no host to reach, a flag it cannot
    /// parse — prints `EXIT <reason>` from `main_entry`, but `Journal::open`
    /// lived inside `watch_to`, so the file was never even created and a
    /// replay saw no record that the watch had run or why it ended.
    #[test]
    fn a_watch_that_dies_before_its_loop_still_journals_its_exit() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-drive-preloop-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("journal.jsonl");
        let _ = std::fs::remove_file(&path);

        // The argv as typed, read straight back — the sub-flags may be exactly
        // what would not parse.
        let cmd = vec![
            "watch".to_string(),
            "@s-1".to_string(),
            "--journal".to_string(),
            path.display().to_string(),
        ];
        assert_eq!(journal_arg(&cmd).as_deref(), Some(path.as_path()));
        assert_eq!(watch_sid_arg(&cmd), Some("@s-1"));
        assert_eq!(journal_arg(&cmd[..2]), None);
        assert_eq!(watch_sid_arg(&["watch".to_string()]), None);

        let mut warn = Vec::new();
        let mut journal = crate::supervise::Journal::open(
            journal_arg(&cmd).as_deref(),
            watch_sid_arg(&cmd),
            &mut warn,
        );
        let line = watch_exit_line(&cmd, "cannot reach a target aterm (refused).\n  • hint")
            .expect("watch ends on EXIT");
        journal.record(&line, None, &mut warn);

        let (records, bad) =
            crate::supervise::read_journal(&path).expect("the journal exists and reads");
        assert_eq!(bad, 0);
        assert_eq!(records.len(), 1, "one line printed, one record");
        assert_eq!(records[0].kind, "exit");
        assert_eq!(records[0].sid.as_deref(), Some("s-1"));
        assert_eq!(records[0].line, line);
        assert!(
            records[0]
                .line
                .starts_with("EXIT cannot reach a target aterm"),
            "{}",
            records[0].line
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the journal is the loop's own");
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// The zone `date +%z` prints, in both spellings, as seconds; anything
    /// else is no zone (and the ledger's times fall back to UTC).
    #[test]
    fn the_local_zone_is_read_from_date() {
        assert_eq!(parse_zone("+0000"), Some(0));
        assert_eq!(parse_zone("-0700"), Some(-7 * 3600));
        assert_eq!(parse_zone("+0530"), Some(5 * 3600 + 1800));
        assert_eq!(parse_zone("-07:00"), Some(-7 * 3600));
        for bad in ["", "UTC", "0700", "+07", "+070000", "x+0700"] {
            assert_eq!(parse_zone(bad), None, "{bad}");
        }
    }

    /// The ledger needs a worker: the `@sid` given, or the one the journal's
    /// own lines name (and nothing to go on is an actionable error).
    #[test]
    fn the_ledger_takes_its_worker_from_the_journal_when_none_is_named() {
        let dir = std::env::temp_dir().join(format!("aterm-drive-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("journal.jsonl");
        let line = |sid: &str| {
            crate::supervise::JournalRecord::of_line(1, Some(sid), "TIMEOUT", None).to_json()
        };
        std::fs::write(&path, format!("{}\n{}\n", line("s-work"), line("@s-work"))).expect("write");
        assert_eq!(journal_sid(Some(&path)).as_deref(), Some("s-work"));
        // Two sessions in one file: the ledger cannot guess which.
        std::fs::write(&path, format!("{}\n{}\n", line("s-work"), line("s-other"))).expect("write");
        assert_eq!(journal_sid(Some(&path)), None);
        assert_eq!(journal_sid(None), None);
        assert_eq!(journal_sid(Some(&dir.join("nothing.jsonl"))), None);
        let err = ledger_verb(
            &Opts {
                socket: None,
                dial: None,
                idle_ms: 1,
                timeout_ms: 1,
                ready: None,
                cmd: vec!["ledger".to_string()],
            },
            &args(&["--journal", &path.to_string_lossy()]),
        )
        .expect_err("no worker");
        assert!(err.starts_with("ledger needs the worker:"), "{err}");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir);
    }
}

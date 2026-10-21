// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm keeper serve|status|start|stop`, the verbs `crates/aterm` routes here.
//!
//! `start` submits a keeper as a transient `launchctl submit` job, `stop` removes it, `serve` is
//! what the job runs, and `status` asks one what it holds. Nothing starts a keeper by itself.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use crate::identity::Identity;
use crate::identity::IdentityPolicy;

/// The usage text.
pub const USAGE: &str = "\
aterm keeper — the PTY keeper: a per-login holder of a custody copy of every
terminal's master, so a window that crashes does not hang up its shells.

USAGE:
    aterm keeper start  [--sock <path>] [--identity designated|uid] [--label <label>]
    aterm keeper stop   [--label <label>]
    aterm keeper status [--sock <path>] [--identity designated|uid]
    aterm keeper serve  [--sock <path>] [--identity designated|uid] [--no-relaunch]

    start    Submit this build's keeper as a transient launchd job
             (`launchctl submit`: no plist, gone at logout). It runs
             `serve --no-relaunch`: the next launch you make recovers.
    stop     Remove the job (launchd ends the keeper; the shells it alone
             held are hung up, as a quit would), then remove the copies of
             code left by keepers that have ended.
    status   Ask the keeper what it holds: masters by state, the relaunch
             brake, and which identity it requires. `keeper=absent` when none
             answers.
    serve    Run a keeper in the foreground on its socket (what the job
             runs). It never reads a terminal or signals anything, and
             relaunches only its own app bundle (never with --no-relaunch or
             from a build tree's bare binary). Under `designated` it runs
             from a copy of this build's code (~/Library/Application
             Support/aterm/keeper-code), so windows can still check it after
             an update removes this build. `stop` removes the copy; so does
             the next keeper start, once its keeper has ended.

OPTIONS:
    --sock <path>        The socket (default: this build's own, in the per-user
                         private directory; the installed app and every dev
                         tree have different ones).
    --identity <which>   `designated` (the default): a peer must run code that
                         satisfies this build's designated requirement.
                         `uid`: the same-uid floor only, for tests and by-hand
                         runs; a window registers nothing with such a keeper.
    --label <label>      The launchd label (default: this build's own). Only
                         this build's label or a test label
                         (`com.aterm.aterm.keeper.test.*`) is accepted.
    --no-relaunch        Never bring a window back; orphans wait for the next
                         launch.

macOS only: a window uses the keeper only with `[keeper] enabled = true` in
aterm.toml (read at launch).
";

struct Args {
    verb: String,
    // Read by the verbs only unix builds implement.
    #[cfg_attr(not(unix), allow(dead_code))]
    sock: Option<PathBuf>,
    #[cfg_attr(not(unix), allow(dead_code))]
    identity: IdentityPolicy,
    #[cfg_attr(not(unix), allow(dead_code))]
    label: Option<String>,
    #[cfg_attr(not(unix), allow(dead_code))]
    no_relaunch: bool,
}

fn parse(args: &[OsString]) -> Result<Args, String> {
    let mut it = args.iter().map(|a| a.to_string_lossy().into_owned());
    let verb = it.next().ok_or_else(|| "missing verb".to_string())?;
    let mut sock = None;
    let mut identity = IdentityPolicy::Designated;
    let mut label = None;
    let mut no_relaunch = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sock" => sock = Some(PathBuf::from(it.next().ok_or("--sock needs a path")?)),
            "--identity" => {
                identity = match it.next().as_deref() {
                    Some("designated") => IdentityPolicy::Designated,
                    Some("uid") => IdentityPolicy::SameUid,
                    other => return Err(format!("--identity designated|uid, not {other:?}")),
                };
            }
            "--label" => label = Some(it.next().ok_or("--label needs a label")?),
            "--no-relaunch" => no_relaunch = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(Args {
        verb,
        sock,
        identity,
        label,
        no_relaunch,
    })
}

/// Whether `start`/`stop` may touch `label`: this build's own, or a test
/// label. Never another build's (a dev tree cannot remove the installed
/// app's keeper, nor the installed app a dev tree's).
#[must_use]
pub fn label_is_ours(label: &str, own: &str) -> bool {
    label == own
        || label
            .strip_prefix(crate::job::TEST_LABEL_PREFIX)
            .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1)
}

/// `aterm keeper <args>`. `version`: the running build's version, which a
/// served keeper reports (WELCOME, status) and a status client says at HELLO —
/// the binary's, handed in because this crate links nothing that knows it.
#[must_use]
pub fn main_entry(args: Vec<OsString>, version: &str) -> ExitCode {
    let first = args.first().map(|a| a.to_string_lossy().into_owned());
    if matches!(first.as_deref(), None | Some("-h" | "--help" | "help")) {
        return aterm_uds::stdout::answer_exit(
            "aterm keeper",
            USAGE,
            if first.is_none() { 2 } else { 0 },
        );
    }
    // A runtime test, not a `cfg`, so the Unix verbs stay compiled (and
    // linted) on Linux, where launchd and code identity do not exist.
    if !cfg!(target_vendor = "apple") {
        eprintln!("{MACOS_ONLY}");
        return ExitCode::FAILURE;
    }
    let parsed = match parse(&args) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("aterm keeper: {why}; run `aterm keeper --help`");
            return ExitCode::from(2);
        }
    };
    match parsed.verb.as_str() {
        "serve" => serve(&parsed, version),
        "status" => status(&parsed, version),
        "start" => start(&parsed, version),
        "stop" => stop(&parsed, version),
        other => {
            eprintln!("aterm keeper: unknown verb {other:?}; run `aterm keeper --help`");
            ExitCode::from(2)
        }
    }
}

/// What every verb says off macOS.
const MACOS_ONLY: &str = "aterm keeper: macOS only";

#[cfg(unix)]
fn default_socket() -> Option<PathBuf> {
    crate::job::socket_path(&crate::job::own_place())
}

#[cfg(unix)]
fn serve(args: &Args, version: &str) -> ExitCode {
    let identity = match Identity::for_self(args.identity) {
        Ok(i) => i,
        Err(why) => {
            eprintln!("aterm keeper serve: {why}");
            return ExitCode::FAILURE;
        }
    };
    // Sweep the copies of code that ended keepers (a crash, a logout) left. A keeper whose peers
    // check its code then runs from its own copy, so an update that deletes its build leaves it
    // checkable; `run_from_own_copy` returns only when it could not.
    let code = crate::code::code_dir();
    let own_copy = code.as_deref().and_then(crate::code::own_copy);
    // First thing from a copy: its exec is over, so a sweep may remove it once this keeper ends.
    if let Some(copy) = &own_copy {
        crate::code::mark_ran(copy);
    }
    let exe = crate::code::origin_exe(own_copy.is_some());
    if let Some(dir) = code.as_deref() {
        crate::code::sweep(dir, crate::server::born);
    }
    if own_copy.is_none() && identity.policy() == IdentityPolicy::Designated {
        let why = match (exe.as_deref(), code.as_deref()) {
            (Some(exe), Some(dir)) => crate::code::run_from_own_copy(exe, dir, &identity),
            (None, _) => "this executable's path cannot be read".to_string(),
            (_, None) => "no $HOME to keep a copy in".to_string(),
        };
        eprintln!(
            "aterm keeper serve: running from this build, not a copy ({why}); no window can \
             check it once an update removes this build"
        );
    }
    let exit = serve_as(args, identity, exe.as_deref(), version);
    // Nothing checks this keeper once it stops serving: its copy goes with it.
    if let Some(copy) = own_copy {
        let _ = std::fs::remove_dir_all(copy);
    }
    exit
}

/// Serve as the keeper of `exe`, the executable it was started as.
#[cfg(unix)]
fn serve_as(
    args: &Args,
    identity: Identity,
    exe: Option<&std::path::Path>,
    version: &str,
) -> ExitCode {
    // The socket, label and relaunch bundle are the build's it was started
    // as, never its copy's.
    let Some(socket) = args
        .sock
        .clone()
        .or_else(|| exe.and_then(|e| crate::job::socket_path(&crate::job::place_for(e))))
    else {
        eprintln!("aterm keeper serve: no private directory for the socket");
        return ExitCode::FAILURE;
    };
    let relaunch = match exe.and_then(crate::job::own_bundle) {
        Some(bundle) if !args.no_relaunch => crate::server::Relauncher::OpenBundle(bundle),
        _ => crate::server::Relauncher::Unavailable,
    };
    let config = crate::server::ServeConfig {
        socket,
        identity,
        relaunch,
        version: version.to_string(),
        test_drop_custody: false,
    };
    let mut server = match crate::server::Server::bind(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("aterm keeper serve: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "aterm keeper: serving on {} (pid {}, running {})",
        server.socket().display(),
        std::process::id(),
        std::env::current_exe().map_or_else(|e| e.to_string(), |p| p.display().to_string())
    );
    match server.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aterm keeper serve: {e}");
            ExitCode::FAILURE
        }
    }
}

// Off Unix only these stubs exist, and `main_entry` answers `macOS only`
// before it reaches them.
#[cfg(not(unix))]
fn serve(_args: &Args, _version: &str) -> ExitCode {
    eprintln!("{MACOS_ONLY}");
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn status(_args: &Args, _version: &str) -> ExitCode {
    eprintln!("{MACOS_ONLY}");
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn start(_args: &Args, _version: &str) -> ExitCode {
    eprintln!("{MACOS_ONLY}");
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn stop(_args: &Args, _version: &str) -> ExitCode {
    eprintln!("{MACOS_ONLY}");
    ExitCode::FAILURE
}

/// The label `start`/`stop` act on: `--label`, checked, else this build's.
#[cfg(unix)]
fn chosen_label(args: &Args, verb: &str) -> Result<String, ExitCode> {
    let own = crate::job::own_place().label;
    match args.label.clone() {
        None => Ok(own),
        Some(label) if label_is_ours(&label, &own) => Ok(label),
        Some(label) => {
            eprintln!(
                "aterm keeper {verb}: {label:?} is not this build's keeper ({own}) nor a test \
                 label ({}.*)",
                crate::job::TEST_LABEL_PREFIX
            );
            Err(ExitCode::from(2))
        }
    }
}

/// Why a dial of the socket reached no keeper of this build's.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum NoKeeper {
    /// A keeper answered and failed this build's identity check.
    NotOurs,
    /// Nothing answers: no socket there, or a stale one.
    Nothing,
    /// The dial or the exchange failed another way; why.
    Failed(String),
}

#[cfg(unix)]
impl NoKeeper {
    fn of(e: &std::io::Error) -> Self {
        if crate::client::refused_identity(e) {
            Self::NotOurs
        } else if matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        ) {
            Self::Nothing
        } else {
            Self::Failed(e.to_string())
        }
    }
}

/// Ask the keeper on `socket` for its status, as a client of build `version`.
#[cfg(unix)]
fn ask(socket: &std::path::Path, identity: &Identity, version: &str) -> Result<String, NoKeeper> {
    crate::client::KeeperClient::connect(socket, identity, Duration::from_secs(2))
        .and_then(|c| c.status(version))
        .map_err(|e| NoKeeper::of(&e))
}

/// launchd's word on a job, as the verbs print it after `job=`.
#[cfg(unix)]
fn job_words(job: &crate::job::JobState) -> String {
    match job {
        crate::job::JobState::Absent => "absent".to_string(),
        crate::job::JobState::Loaded { pid: Some(pid) } => format!("running pid={pid}"),
        crate::job::JobState::Loaded { pid: None } => "loaded".to_string(),
    }
}

/// A refused keeper and its remedy, offered only when a bare `stop` and
/// `start` act on its label and socket (this build's own).
#[cfg(unix)]
const REPLACE: &str = "(not this build's keeper); aterm keeper stop, then aterm keeper start";

/// What `start` prints instead of submitting — a job is loaded under the
/// label, or a keeper already answers on the socket — and whether that is
/// success. `None`: submit. `log` is the job's log when it exists.
/// `own_place`: the label and the socket are this build's own, so the job's
/// pid is the keeper's and a bare `stop` and `start` act on them; otherwise
/// the job is named apart from what answers.
#[cfg(unix)]
fn start_without_submit(
    label: &str,
    socket: &std::path::Path,
    own_place: bool,
    job: &crate::job::JobState,
    answer: &Result<String, NoKeeper>,
    log: Option<&std::path::Path>,
) -> Option<(String, bool)> {
    use crate::job::JobState;
    let at = socket.display();
    let line = match (job, answer) {
        (JobState::Absent, Err(NoKeeper::Nothing | NoKeeper::Failed(_))) => return None,
        (JobState::Absent, Ok(status)) => {
            return Some((
                format!(
                    "keeper=running label={label} job=absent{} socket={at}",
                    status_pid(status).map_or_else(String::new, |pid| format!(" pid={pid}"))
                ),
                true,
            ));
        }
        // A loaded job that runs nothing is not what answers: the next arm
        // names it `job=loaded`, never `pid=-` as the keeper's.
        (JobState::Loaded { pid: Some(pid) }, Ok(_)) if own_place => {
            return Some((
                format!("keeper=running label={label} pid={pid} socket={at}"),
                true,
            ));
        }
        (JobState::Loaded { .. }, Ok(_)) => {
            return Some((
                format!(
                    "keeper=running label={label} job={} socket={at}",
                    job_words(job)
                ),
                true,
            ));
        }
        (JobState::Loaded { pid: Some(pid) }, Err(NoKeeper::NotOurs)) if own_place => {
            format!("keeper=refused label={label} pid={pid} socket={at} {REPLACE}")
        }
        (_, Err(NoKeeper::NotOurs)) => format!(
            "keeper=refused label={label} job={} socket={at} (not this build's keeper)",
            job_words(job)
        ),
        (JobState::Loaded { .. }, Err(miss)) => {
            let mut why = Vec::new();
            if let NoKeeper::Failed(e) = miss {
                why.push(e.clone());
            }
            if let Some(log) = log {
                why.push(format!("its errors: {}", log.display()));
            }
            let mut line = format!(
                "keeper=absent label={label} job={} socket={at}",
                job_words(job)
            );
            if !why.is_empty() {
                line.push_str(&format!(" ({})", why.join("; ")));
            }
            line
        }
    };
    Some((line, false))
}

/// `status`'s first line when no keeper of this build's answered.
/// `replaceable`: the socket is this build's own and launchd runs this
/// build's job now, so a bare `stop` and `start` replace what answers.
#[cfg(unix)]
fn status_miss_line(socket: &std::path::Path, miss: &NoKeeper, replaceable: bool) -> String {
    let at = socket.display();
    match miss {
        NoKeeper::NotOurs if replaceable => format!("keeper=refused socket={at} {REPLACE}"),
        NoKeeper::NotOurs => format!("keeper=refused socket={at} (not this build's keeper)"),
        NoKeeper::Nothing => format!("keeper=absent socket={at}"),
        NoKeeper::Failed(why) => format!("keeper=absent socket={at} ({why})"),
    }
}

/// Empty the job's log (0600), making its directory (0700) if missing:
/// launchd opens the log before the keeper runs, and what it then holds is
/// this job's alone.
#[cfg(unix)]
fn fresh_log(log: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = log.parent() {
        crate::server::ensure_private_dir(dir)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(log)
        .map(drop)
}

#[cfg(unix)]
fn start(args: &Args, version: &str) -> ExitCode {
    let label = match chosen_label(args, "start") {
        Ok(label) => label,
        Err(code) => return code,
    };
    let Some(socket) = args.sock.clone().or_else(default_socket) else {
        eprintln!("aterm keeper start: no private directory for the socket");
        return ExitCode::FAILURE;
    };
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
    else {
        eprintln!("aterm keeper start: this executable's path cannot be read");
        return ExitCode::FAILURE;
    };
    let job = match crate::job::state(&label) {
        Ok(job) => job,
        Err(e) => {
            eprintln!("aterm keeper start: launchctl: {e}");
            return ExitCode::FAILURE;
        }
    };
    let own = match Identity::for_self(args.identity) {
        Ok(i) => i,
        Err(why) => {
            eprintln!("aterm keeper start: {why}");
            return ExitCode::FAILURE;
        }
    };
    let log = crate::job::log_path(&socket);
    let answer = ask(&socket, &own, version);
    let existing = log.exists().then_some(log.as_path());
    let own_place = label == crate::job::own_place().label
        && default_socket().as_deref() == Some(socket.as_path());
    if let Some((line, ok)) =
        start_without_submit(&label, &socket, own_place, &job, &answer, existing)
    {
        println!("{line}");
        return if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    // The socket is resolved in the caller's environment and passed as `--sock`: a launchd job's
    // `$TMPDIR` and `$XDG_RUNTIME_DIR` are not the caller's. Its directory is made now (the log).
    if let Err(e) = fresh_log(&log) {
        eprintln!("aterm keeper start: {}: {e}", log.display());
        return ExitCode::FAILURE;
    }
    let socket_arg = socket.to_string_lossy().into_owned();
    let identity = match args.identity {
        IdentityPolicy::Designated => "designated",
        IdentityPolicy::SameUid => "uid",
    };
    let program = exe.to_string_lossy().into_owned();
    let argv = [
        "keeper",
        "serve",
        "--sock",
        socket_arg.as_str(),
        "--identity",
        identity,
        "--no-relaunch",
    ];
    if let Err(e) = crate::job::submit(&label, &program, &argv, &log) {
        eprintln!("aterm keeper start: {e}");
        return ExitCode::FAILURE;
    }
    // launchd starts the job asynchronously: wait (bounded) for it to answer.
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if ask(&socket, &own, version).is_ok() {
            println!("keeper=running label={label} socket={}", socket.display());
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "keeper=submitted label={label} socket={} (not answering after 10 s; its errors: {})",
        socket.display(),
        log.display()
    );
    ExitCode::FAILURE
}

/// How long `stop` waits for the keeper to end after `launchctl remove`: a hang detector, not a
/// budget (launchd SIGKILLs a submitted job that outlives its SIGTERM; measured 5 s).
#[cfg(unix)]
const STOP_WAIT: Duration = Duration::from_secs(60);

#[cfg(unix)]
fn stop(args: &Args, version: &str) -> ExitCode {
    let label = match chosen_label(args, "stop") {
        Ok(label) => label,
        Err(code) => return code,
    };
    let exit = match crate::job::state(&label) {
        Ok(crate::job::JobState::Absent) => {
            // A keeper this build's socket still answers is one `stop` cannot
            // end: say it runs rather than report the job alone.
            let answering = (label == crate::job::own_place().label)
                .then(default_socket)
                .flatten()
                .and_then(|sock| {
                    let own = Identity::for_self(args.identity).ok()?;
                    let status = ask(&sock, &own, version).ok()?;
                    Some((sock, status_pid(&status)))
                });
            let (line, ok) = stop_absent_line(
                &label,
                answering.as_ref().map(|(sock, pid)| (sock.as_path(), *pid)),
            );
            println!("{line}");
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Ok(crate::job::JobState::Loaded { pid }) => {
            // Which process the job runs, read before it ends, so its end is
            // told from a reused pid.
            let running = pid.and_then(|pid| crate::server::born(pid).map(|b| (pid, b)));
            match crate::job::remove(&label) {
                Ok(()) => match running {
                    Some((pid, birth))
                        if !await_end(pid, birth, STOP_WAIT, crate::server::born) =>
                    {
                        println!("{}", still_running_line(&label, pid));
                        ExitCode::FAILURE
                    }
                    _ => {
                        println!("keeper=stopped label={label}");
                        ExitCode::SUCCESS
                    }
                },
                Err(e) => {
                    eprintln!("aterm keeper stop: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            eprintln!("aterm keeper stop: launchctl: {e}");
            ExitCode::FAILURE
        }
    };
    // The ended keeper's copy of its code goes now, with any a crash or a logout left; a live
    // keeper's copy is kept.
    if let Some(dir) = crate::code::code_dir() {
        crate::code::sweep(&dir, crate::server::born);
    }
    exit
}

/// `stop`'s line when launchd has no job under `label`, and whether that is
/// success. `answering`: the socket on which a keeper of this build's still
/// answers, with the pid its status named, when one does.
#[cfg(unix)]
fn stop_absent_line(
    label: &str,
    answering: Option<(&std::path::Path, Option<u32>)>,
) -> (String, bool) {
    match answering {
        Some((sock, pid)) => (
            format!(
                "keeper=running label={label} job=absent{} socket={} (this label's stop \
                 cannot end it)",
                pid.map_or_else(String::new, |pid| format!(" pid={pid}")),
                sock.display()
            ),
            false,
        ),
        None => (format!("job=absent label={label}"), true),
    }
}

/// The `pid=` field of a keeper's status text ([`crate::server`]'s
/// `status_text`), when it names one.
#[cfg(unix)]
fn status_pid(status: &str) -> Option<u32> {
    status
        .split_whitespace()
        .find_map(|field| field.strip_prefix("pid="))
        .and_then(|pid| pid.parse().ok())
}

/// `stop`'s line when the job is removed but its keeper outlived the wait.
#[cfg(unix)]
fn still_running_line(label: &str, pid: u32) -> String {
    format!(
        "keeper=running label={label} pid={pid} (still running {} s after its job was removed)",
        STOP_WAIT.as_secs()
    )
}

/// Wait up to `wait` until `pid` is no longer the process born at `birth`;
/// whether it ended.
#[cfg(unix)]
fn await_end(
    pid: u32,
    birth: crate::wire::Birth,
    wait: Duration,
    born: impl Fn(u32) -> Option<crate::wire::Birth>,
) -> bool {
    let started = std::time::Instant::now();
    while born(pid) == Some(birth) {
        if started.elapsed() > wait {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

#[cfg(unix)]
fn status(args: &Args, version: &str) -> ExitCode {
    let place = crate::job::own_place();
    let own_socket = default_socket();
    let Some(socket) = args.sock.clone().or_else(|| own_socket.clone()) else {
        println!("keeper=absent (no private directory for a socket)");
        return ExitCode::FAILURE;
    };
    let job = crate::job::state(&place.label);
    // `stop` and `start` act on this build's job and socket: the remedy is
    // offered only for that socket.
    let replaceable = own_socket.as_ref() == Some(&socket)
        && matches!(job, Ok(crate::job::JobState::Loaded { pid: Some(_) }));
    let job = match job {
        Ok(job) => job_words(&job),
        Err(e) => format!("unknown ({e})"),
    };
    let identity = match Identity::for_self(args.identity) {
        Ok(i) => i,
        Err(why) => {
            eprintln!("aterm keeper status: {why}");
            return ExitCode::FAILURE;
        }
    };
    match ask(&socket, &identity, version) {
        Ok(text) => {
            println!("{text}");
            println!("label={} job={job}", place.label);
            ExitCode::SUCCESS
        }
        Err(miss) => {
            println!("{}", status_miss_line(&socket, &miss, replaceable));
            println!("label={} job={job}", place.label);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a failed identity check is `NotOurs`; no socket or a stale one is `Nothing`;
    /// anything else keeps its words.
    #[cfg(unix)]
    #[test]
    fn a_dial_error_is_read_as_what_it_is() {
        use std::io::{Error, ErrorKind};
        let refused = Error::new(
            ErrorKind::PermissionDenied,
            crate::client::NotThisBuild("OSStatus -67050".into()),
        );
        assert_eq!(NoKeeper::of(&refused), NoKeeper::NotOurs);
        let eacces = Error::from(ErrorKind::PermissionDenied);
        assert!(matches!(NoKeeper::of(&eacces), NoKeeper::Failed(_)));
        assert_eq!(
            NoKeeper::of(&Error::from(ErrorKind::NotFound)),
            NoKeeper::Nothing
        );
        assert_eq!(
            NoKeeper::of(&Error::from(ErrorKind::ConnectionRefused)),
            NoKeeper::Nothing
        );
        assert_eq!(
            NoKeeper::of(&Error::other("timed out")),
            NoKeeper::Failed("timed out".into())
        );
    }

    /// `stop` says `keeper=stopped` only when the keeper ended, and a keeper
    /// that still answers with no job is named running, never `job=absent`.
    #[cfg(unix)]
    #[test]
    fn stop_says_stopped_only_when_the_keeper_ended() {
        let birth = crate::wire::Birth {
            seconds: 1,
            micros: 2,
        };
        let reused = crate::wire::Birth {
            seconds: 3,
            micros: 0,
        };
        let short = Duration::from_millis(50);
        assert!(!await_end(7, birth, short, |_| Some(birth)));
        assert!(await_end(7, birth, short, |_| None));
        assert!(await_end(7, birth, short, |_| Some(reused)));
        assert_eq!(
            still_running_line("L", 7),
            "keeper=running label=L pid=7 (still running 60 s after its job was removed)"
        );
        assert_eq!(
            stop_absent_line("L", None),
            ("job=absent label=L".to_string(), true)
        );
        assert_eq!(
            stop_absent_line("L", Some((std::path::Path::new("/p/k.sock"), Some(42)))),
            (
                "keeper=running label=L job=absent pid=42 socket=/p/k.sock (this label's stop \
                 cannot end it)"
                    .to_string(),
                false
            )
        );
        assert_eq!(
            stop_absent_line("L", Some((std::path::Path::new("/p/k.sock"), None))).0,
            "keeper=running label=L job=absent socket=/p/k.sock (this label's stop cannot end it)"
        );
        assert_eq!(
            status_pid("keeper=running\npid=4242 version=0.70.0\nsocket=/p/k.sock"),
            Some(4242)
        );
        assert_eq!(status_pid("keeper=running\nsocket=/p/k.sock"), None);
    }

    /// `start` submits only when no job is loaded and nothing answers, and says `keeper=running`
    /// only when this build's keeper answered, never from launchd's word alone (a job that
    /// restarts every 10 s is loaded). Only for this build's own label and socket is the job's pid
    /// the keeper's, and a refused keeper offered `stop` then `start`.
    #[cfg(unix)]
    #[test]
    fn start_says_running_only_when_this_builds_keeper_answers() {
        use crate::job::JobState;
        let sock = std::path::Path::new("/p/k.sock");
        let log = std::path::Path::new("/p/k.sock.log");
        let say = |job: JobState, answer: Result<String, NoKeeper>, log| {
            start_without_submit("L", sock, true, &job, &answer, log)
        };
        let ours = || Ok("keeper=running".to_string());
        let line = |s: &str, ok: bool| Some((s.to_string(), ok));
        assert_eq!(say(JobState::Absent, Err(NoKeeper::Nothing), None), None);
        assert_eq!(
            say(JobState::Absent, Err(NoKeeper::Failed("x".into())), None),
            None
        );
        assert_eq!(
            say(JobState::Loaded { pid: Some(7) }, ours(), None),
            line("keeper=running label=L pid=7 socket=/p/k.sock", true)
        );
        assert_eq!(
            say(JobState::Loaded { pid: None }, ours(), None),
            line("keeper=running label=L job=loaded socket=/p/k.sock", true),
            "a job launchd holds but runs nothing is not the keeper that answers"
        );
        assert_eq!(
            start_without_submit(
                "L",
                sock,
                false,
                &JobState::Loaded { pid: Some(7) },
                &ours(),
                None
            ),
            line(
                "keeper=running label=L job=running pid=7 socket=/p/k.sock",
                true
            ),
            "another label or socket: the job's pid is not claimed as the keeper's"
        );
        assert_eq!(
            say(
                JobState::Loaded { pid: None },
                Err(NoKeeper::Nothing),
                Some(log)
            ),
            line(
                "keeper=absent label=L job=loaded socket=/p/k.sock (its errors: /p/k.sock.log)",
                false
            )
        );
        assert_eq!(
            say(
                JobState::Loaded { pid: Some(7) },
                Err(NoKeeper::Failed("timed out".into())),
                None
            ),
            line(
                "keeper=absent label=L job=running pid=7 socket=/p/k.sock (timed out)",
                false
            )
        );
        assert_eq!(
            say(
                JobState::Loaded { pid: Some(7) },
                Err(NoKeeper::NotOurs),
                None
            ),
            line(
                "keeper=refused label=L pid=7 socket=/p/k.sock (not this build's keeper); aterm \
                 keeper stop, then aterm keeper start",
                false
            )
        );
        assert_eq!(
            start_without_submit(
                "L",
                sock,
                false,
                &JobState::Loaded { pid: Some(7) },
                &Err(NoKeeper::NotOurs),
                None
            ),
            line(
                "keeper=refused label=L job=running pid=7 socket=/p/k.sock (not this build's \
                 keeper)",
                false
            ),
            "another label or socket: its job may not be what answers, and a bare stop and \
             start do not act on it"
        );
        assert_eq!(
            say(JobState::Loaded { pid: None }, Err(NoKeeper::NotOurs), None),
            line(
                "keeper=refused label=L job=loaded socket=/p/k.sock (not this build's keeper)",
                false
            )
        );
        assert_eq!(
            say(JobState::Absent, Err(NoKeeper::NotOurs), None),
            line(
                "keeper=refused label=L job=absent socket=/p/k.sock (not this build's keeper)",
                false
            )
        );
        assert_eq!(
            say(JobState::Absent, ours(), None),
            line("keeper=running label=L job=absent socket=/p/k.sock", true)
        );
        assert_eq!(
            say(
                JobState::Absent,
                Ok("keeper=running\npid=42 version=0.70.0\nsocket=/p/k.sock".to_string()),
                None
            ),
            line(
                "keeper=running label=L job=absent pid=42 socket=/p/k.sock",
                true
            ),
            "start names the pid stop names for the same state"
        );
    }

    /// `status` says `keeper=absent` only when nothing answered; a keeper
    /// that answered but is not this build's is `keeper=refused`, with the
    /// replacement when the socket is this build's and its job runs.
    #[cfg(unix)]
    #[test]
    fn status_tells_a_refused_keeper_from_an_absent_one() {
        let sock = std::path::Path::new("/p/k.sock");
        assert_eq!(
            status_miss_line(sock, &NoKeeper::NotOurs, true),
            "keeper=refused socket=/p/k.sock (not this build's keeper); aterm keeper stop, then \
             aterm keeper start"
        );
        assert_eq!(
            status_miss_line(sock, &NoKeeper::NotOurs, false),
            "keeper=refused socket=/p/k.sock (not this build's keeper)"
        );
        assert_eq!(
            status_miss_line(sock, &NoKeeper::Nothing, true),
            "keeper=absent socket=/p/k.sock"
        );
        assert_eq!(
            status_miss_line(sock, &NoKeeper::Failed("timed out".into()), false),
            "keeper=absent socket=/p/k.sock (timed out)"
        );
    }

    #[test]
    fn start_and_stop_touch_only_this_builds_label_or_a_test_one() {
        let own = "com.aterm.aterm.keeper.dev.0123abcd";
        assert!(label_is_ours(own, own));
        assert!(label_is_ours("com.aterm.aterm.keeper.test.42.e2e", own));
        assert!(
            !label_is_ours(crate::job::INSTALLED_LABEL, own),
            "a dev build never touches the installed app's keeper"
        );
        assert!(!label_is_ours("com.aterm.aterm.keeper.test", own));
        assert!(!label_is_ours("com.aterm.aterm.keeper.testx", own));
        assert!(!label_is_ours("com.apple.Finder", own));
    }

    #[test]
    fn the_options_parse() {
        let args: Vec<OsString> = [
            "start",
            "--label",
            "com.aterm.aterm.keeper.test.1.x",
            "--identity",
            "uid",
            "--sock",
            "/p/k.sock",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let a = parse(&args).expect("parse");
        assert_eq!(a.verb, "start");
        assert_eq!(a.label.as_deref(), Some("com.aterm.aterm.keeper.test.1.x"));
        assert_eq!(a.identity, IdentityPolicy::SameUid);
        assert_eq!(a.sock, Some(PathBuf::from("/p/k.sock")));
        let serve: Vec<OsString> = ["serve", "--no-relaunch"]
            .iter()
            .map(OsString::from)
            .collect();
        assert!(parse(&serve).expect("parse").no_relaunch);
    }
}

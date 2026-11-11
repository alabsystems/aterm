// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm keeper serve|status|start|stop`, the verbs `crates/aterm` routes here.

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
    aterm keeper stop   [--identity designated|uid] [--label <label>]
    aterm keeper status [--sock <path>] [--identity designated|uid]
    aterm keeper serve  [--sock <path>] [--identity designated|uid] [--no-relaunch]

    start    Submit this build's keeper as a launchd job (`launchctl submit`:
             no plist; launchd restarts it whenever it exits, until `stop` or
             logout) unless a job is loaded or a keeper answers, and wait up
             to 10 s for it to answer. A window does the same at launch. The
             installed app's job runs `serve`; any other build's and a test
             label's `serve --no-relaunch`.
    stop     Remove the job, wait up to 60 s for its keeper to end (shells
             only it held hang up, as on a quit) and remove ended keepers'
             code copies.
    status   What the keeper holds (masters by state, the relaunch brake, its
             identity), then this build's `label=` and `job=`. `keeper=absent`
             when none answers, `keeper=refused` when it is not this build's.
    serve    Run a keeper in the foreground (what the job runs). From an app
             bundle without --no-relaunch, it reopens that app after a crash.
             Under `designated` it runs from a copy of its code in
             ~/Library/Application Support/aterm/keeper-code, so windows can
             still check it after an update removes this build.

OPTIONS (a verb ignores those its USAGE line leaves out):
    --sock <path>        The socket (default: this build's own, as `status`
                         prints it; each executable path has its own).
    --identity <which>   `designated` (the default): the peer must run code
                         that satisfies this build's designated requirement.
                         `uid`: the same user only, for tests and by-hand
                         runs; a window registers nothing with such a keeper.
    --label <label>      The launchd label start and stop act on (default:
                         this build's own). Only that or a test label
                         (`com.aterm.aterm.keeper.test.*`) is accepted.
    --no-relaunch        Never reopen a window; a crashed window's shells wait
                         for the next launch.

macOS only. Each window of an app bundle uses the keeper and starts it; turn
that off with `[keeper] enabled = false` in aterm.toml (read at launch), then
`stop` the one running. A bare build's window and a headless launch use one
only under `[keeper] enabled = true`, and never start it. Exit: 0 when this build's
keeper answers (start, status) or has ended (stop), 1 if not, 2 on bad usage.
";

struct Args {
    verb: String,
    help: bool,
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
    if !matches!(verb.as_str(), "serve" | "status" | "start" | "stop") {
        return Err(format!("unknown verb {verb:?}"));
    }
    let mut help = false;
    let mut sock = None;
    let mut identity = IdentityPolicy::Designated;
    let mut label = None;
    let mut no_relaunch = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => help = true,
            "--sock" => {
                sock = Some(PathBuf::from(
                    it.next()
                        .filter(|s| !s.is_empty())
                        .ok_or("--sock needs a path")?,
                ));
            }
            "--identity" => {
                identity = match it.next().as_deref() {
                    Some("designated") => IdentityPolicy::Designated,
                    Some("uid") => IdentityPolicy::SameUid,
                    Some(other) => {
                        return Err(format!("--identity takes designated or uid, not {other:?}"));
                    }
                    None => return Err("--identity needs designated or uid".to_string()),
                };
            }
            "--label" => label = Some(it.next().ok_or("--label needs a label")?),
            "--no-relaunch" => no_relaunch = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(Args {
        verb,
        help,
        sock,
        identity,
        label,
        no_relaunch,
    })
}

/// Whether `start`/`stop` may touch `label`: this build's own, or a test label.
#[must_use]
pub fn label_is_ours(label: &str, own: &str) -> bool {
    label == own
        || label
            .strip_prefix(crate::job::TEST_LABEL_PREFIX)
            .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1)
}

/// `aterm keeper <args>`.
#[must_use = "return or inspect the keeper command's exit status"]
pub fn main_entry(args: Vec<OsString>, version: &str) -> ExitCode {
    let first = args.first().map(|a| a.to_string_lossy().into_owned());
    if matches!(first.as_deref(), None | Some("-h" | "--help" | "help")) {
        return aterm_uds::stdout::answer_exit(
            "aterm keeper",
            USAGE,
            if first.is_none() { 2 } else { 0 },
        );
    }
    let parsed = parse(&args);
    if matches!(&parsed, Ok(p) if p.help) {
        return aterm_uds::stdout::answer_exit("aterm keeper", USAGE, 0);
    }
    if !cfg!(target_vendor = "apple") {
        eprintln!("{MACOS_ONLY}");
        return ExitCode::FAILURE;
    }
    let parsed = match parsed {
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
    let code = crate::code::code_dir();
    let own_copy = code.as_deref().and_then(crate::code::own_copy);
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
            (_, None) => "no absolute $HOME to keep a copy in".to_string(),
        };
        eprintln!(
            "aterm keeper serve: running from this build, not a copy ({why}); no window can \
             check it once an update removes this build"
        );
    }
    let exit = serve_as(args, identity, exe.as_deref(), version);
    if let Some(copy) = own_copy {
        let _ = std::fs::remove_dir_all(copy);
    }
    exit
}

#[cfg(unix)]
fn serve_as(
    args: &Args,
    identity: Identity,
    exe: Option<&std::path::Path>,
    version: &str,
) -> ExitCode {
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

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum NoKeeper {
    NotOurs,
    Nothing,
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

#[cfg(unix)]
fn ask(socket: &std::path::Path, identity: &Identity, version: &str) -> Result<String, NoKeeper> {
    let within = crate::identity::peer_wait(Duration::from_secs(2));
    crate::client::KeeperClient::connect(socket, identity, within)
        .and_then(|c| c.status(version))
        .map_err(|e| NoKeeper::of(&e))
}

#[cfg(unix)]
fn job_words(job: &crate::job::JobState) -> String {
    match job {
        crate::job::JobState::Absent => "absent".to_string(),
        crate::job::JobState::Loaded { pid: Some(pid) } => format!("running pid={pid}"),
        crate::job::JobState::Loaded { pid: None } => "loaded".to_string(),
    }
}

#[cfg(unix)]
const REPLACE: &str = "(not this build's keeper); aterm keeper stop, then aterm keeper start";

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

#[cfg(unix)]
fn fresh_log(log: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let dir = log
        .parent()
        .ok_or_else(|| std::io::Error::other("the log path has no directory"))?;
    crate::server::ensure_private_dir(dir)?;
    match std::fs::symlink_metadata(log) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
        Ok(meta) => {
            if !meta.file_type().is_file() || meta.uid() != aterm_uds::peer::our_uid() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{} is not a regular file of ours", log.display()),
                ));
            }
            std::fs::remove_file(log)?;
        }
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(log)
        .map(drop)
}

/// The `serve` a job runs: it reopens the app after a crash only where `relaunch` says so.
#[cfg(unix)]
fn serve_argv<'a>(socket: &'a str, identity: &'a str, relaunch: bool) -> Vec<&'a str> {
    let mut argv = vec!["keeper", "serve", "--sock", socket, "--identity", identity];
    if !relaunch {
        argv.push("--no-relaunch");
    }
    argv
}

/// Where [`ensure_job`] left the keeper.
#[cfg(unix)]
enum Ensured {
    /// Nothing to submit: the line that says why, and whether a keeper of ours answers.
    Settled(String, bool),
    /// A job was submitted; its keeper has not answered yet.
    Submitted {
        socket: PathBuf,
        log: PathBuf,
        label: String,
        own: Identity,
    },
}

/// Submit this build's keeper job unless a job is loaded or a keeper answers. The installed app's
/// own job reopens the app after a crash (under the brake); a dev build's or a test label's never
/// does, so killing an instance on a desk brings nothing back.
#[cfg(unix)]
fn ensure_job(
    label: String,
    sock: Option<PathBuf>,
    policy: IdentityPolicy,
    version: &str,
) -> Result<Ensured, String> {
    let socket = sock
        .or_else(default_socket)
        .ok_or("no private directory for the socket")?;
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .ok_or("this executable's path cannot be read")?;
    let job = crate::job::state(&label).map_err(|e| format!("launchctl: {e}"))?;
    let own = Identity::for_self(policy)?;
    let log = crate::job::log_path(&socket);
    let answer = ask(&socket, &own, version);
    let existing = log.exists().then_some(log.as_path());
    let place = crate::job::own_place();
    let own_place = label == place.label && default_socket().as_deref() == Some(socket.as_path());
    if let Some((line, ok)) =
        start_without_submit(&label, &socket, own_place, &job, &answer, existing)
    {
        return Ok(Ensured::Settled(line, ok));
    }
    fresh_log(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let socket_arg = socket.to_string_lossy().into_owned();
    let identity = match policy {
        IdentityPolicy::Designated => "designated",
        IdentityPolicy::SameUid => "uid",
    };
    let program = exe.to_string_lossy().into_owned();
    let argv = serve_argv(&socket_arg, identity, own_place && place.installed);
    crate::job::submit(&label, &program, &argv, &log).map_err(|e| e.to_string())?;
    Ok(Ensured::Submitted {
        socket,
        log,
        label,
        own,
    })
}

/// A window's start of this build's keeper (`label` is a test's): one submit at most and no wait,
/// because the window's link registers its terminals once the keeper answers.
///
/// # Errors
/// What kept the keeper from running: launchd or the code identity refusing, or a keeper at the
/// socket that is not this build's.
#[cfg(unix)]
pub fn ensure_for_window(label: Option<String>, version: &str) -> Result<String, String> {
    let label = label.unwrap_or_else(|| crate::job::own_place().label);
    match ensure_job(label, None, IdentityPolicy::Designated, version)? {
        Ensured::Settled(line, true) => Ok(line),
        Ensured::Settled(line, false) => Err(line),
        Ensured::Submitted { label, socket, .. } => Ok(format!(
            "keeper=submitted label={label} socket={}",
            socket.display()
        )),
    }
}

#[cfg(unix)]
fn wait_for_start(
    socket: &std::path::Path,
    identity: &Identity,
    version: &str,
    started: std::time::Instant,
) -> bool {
    let deadline = started + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if ask(socket, identity, version).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[cfg(unix)]
fn start(args: &Args, version: &str) -> ExitCode {
    let label = match chosen_label(args, "start") {
        Ok(label) => label,
        Err(code) => return code,
    };
    let (socket, log, label, own) =
        match ensure_job(label, args.sock.clone(), args.identity, version) {
            Ok(Ensured::Settled(line, ok)) => {
                println!("{line}");
                return if ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }
            Ok(Ensured::Submitted {
                socket,
                log,
                label,
                own,
            }) => (socket, log, label, own),
            Err(why) => {
                eprintln!("aterm keeper start: {why}");
                return ExitCode::FAILURE;
            }
        };
    if wait_for_start(&socket, &own, version, std::time::Instant::now()) {
        println!("keeper=running label={label} socket={}", socket.display());
        return ExitCode::SUCCESS;
    }
    println!(
        "keeper=submitted label={label} socket={} (not answering after 60 s; its errors: {})",
        socket.display(),
        log.display()
    );
    ExitCode::FAILURE
}

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
    if let Some(dir) = crate::code::code_dir() {
        crate::code::sweep(&dir, crate::server::born);
    }
    exit
}

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

#[cfg(unix)]
fn status_pid(status: &str) -> Option<u32> {
    status
        .split_whitespace()
        .find_map(|field| field.strip_prefix("pid="))
        .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|pid| pid.parse().ok())
}

#[cfg(unix)]
fn still_running_line(label: &str, pid: u32) -> String {
    format!(
        "keeper=running label={label} pid={pid} (still running {} s after its job was removed)",
        STOP_WAIT.as_secs()
    )
}

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
#[path = "cli_tests.rs"]
mod tests;

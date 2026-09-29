// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm keeper serve|status|start|stop` — the verbs `crates/aterm` routes
//! here.
//!
//! P3 is OPT-IN: a window registers its terminals with a keeper only under
//! `[keeper] enabled = true` in `aterm.toml`, and nothing starts one by
//! itself. `start` submits one by hand (a transient `launchctl submit` job,
//! decision 1), `stop` removes it, `serve` is what the job runs, and `status`
//! asks one what it holds.

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
             held are hung up, as a quit would).
    status   Ask the keeper what it holds: masters by state, the relaunch
             brake, and which identity it requires. `keeper=absent` when none
             answers.
    serve    Run a keeper in the foreground on its socket (what the job
             runs). It never reads a terminal, never signals anything, and
             relaunches only its own app bundle — never with --no-relaunch.

OPTIONS:
    --sock <path>        The socket (default: this build's own, in the per-user
                         private directory; the installed app and every dev
                         tree have different ones).
    --identity <which>   `designated` (the default): a peer must run code that
                         satisfies this build's designated requirement.
                         `uid`: the same-uid floor only.
    --label <label>      The launchd label (default: this build's own). Only
                         this build's label or a test label
                         (`com.aterm.aterm.keeper.test.*`) is accepted.
    --no-relaunch        Never bring a window back; orphans wait for the next
                         launch.

A window uses the keeper only with `[keeper] enabled = true` in aterm.toml
(P3 of the keeper design: opt-in, hold only).
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

/// `aterm keeper <args>`.
#[must_use]
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let first = args.first().map(|a| a.to_string_lossy().into_owned());
    if matches!(first.as_deref(), None | Some("-h" | "--help" | "help")) {
        print!("{USAGE}");
        return if first.is_none() {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        };
    }
    let parsed = match parse(&args) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("aterm keeper: {why}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match parsed.verb.as_str() {
        "serve" => serve(&parsed),
        "status" => status(&parsed),
        "start" => start(&parsed),
        "stop" => stop(&parsed),
        other => {
            eprintln!("aterm keeper: unknown verb {other:?}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(unix)]
fn default_socket() -> Option<PathBuf> {
    crate::job::socket_path(&crate::job::own_place())
}

#[cfg(unix)]
fn serve(args: &Args) -> ExitCode {
    let Some(socket) = args.sock.clone().or_else(default_socket) else {
        eprintln!("aterm keeper serve: no private directory for the socket");
        return ExitCode::FAILURE;
    };
    let identity = match Identity::for_self(args.identity) {
        Ok(i) => i,
        Err(why) => {
            eprintln!("aterm keeper serve: {why}");
            return ExitCode::FAILURE;
        }
    };
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    let relaunch = match exe.as_deref().and_then(crate::job::own_bundle) {
        Some(bundle) if !args.no_relaunch => crate::server::Relauncher::OpenBundle(bundle),
        _ => crate::server::Relauncher::Unavailable,
    };
    let config = crate::server::ServeConfig {
        socket,
        identity,
        relaunch,
        version: env!("CARGO_PKG_VERSION").to_string(),
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
        "aterm keeper: serving on {} (pid {})",
        server.socket().display(),
        std::process::id()
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
fn serve(_args: &Args) -> ExitCode {
    eprintln!("aterm keeper: not available on this platform (macOS; Linux is P6d)");
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn status(_args: &Args) -> ExitCode {
    println!("keeper=absent (not available on this platform)");
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn start(_args: &Args) -> ExitCode {
    eprintln!("aterm keeper: not available on this platform (macOS; Linux is P6d)");
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn stop(_args: &Args) -> ExitCode {
    eprintln!("aterm keeper: not available on this platform (macOS; Linux is P6d)");
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

#[cfg(unix)]
fn start(args: &Args) -> ExitCode {
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
    match crate::job::state(&label) {
        Ok(crate::job::JobState::Loaded { pid }) => {
            println!(
                "keeper=running label={label} pid={} socket={}",
                pid.map_or_else(|| "-".to_string(), |p| p.to_string()),
                socket.display()
            );
            return ExitCode::SUCCESS;
        }
        Ok(crate::job::JobState::Absent) => {}
        Err(e) => {
            eprintln!("aterm keeper start: launchctl: {e}");
            return ExitCode::FAILURE;
        }
    }
    // The socket's directory is made private by the keeper itself at bind;
    // it is resolved HERE, in the caller's environment, because a launchd
    // job's is not the caller's (`$TMPDIR`, `$XDG_RUNTIME_DIR`).
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
    if let Err(e) = crate::job::submit(&label, &program, &argv) {
        eprintln!("aterm keeper start: {e}");
        return ExitCode::FAILURE;
    }
    // launchd starts the job asynchronously: wait (bounded) for it to answer.
    let own = match Identity::for_self(args.identity) {
        Ok(i) => i,
        Err(why) => {
            eprintln!("aterm keeper start: {why}");
            return ExitCode::FAILURE;
        }
    };
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if crate::client::KeeperClient::connect(&socket, &own, Duration::from_secs(2))
            .and_then(|c| c.status())
            .is_ok()
        {
            println!("keeper=running label={label} socket={}", socket.display());
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "keeper=submitted label={label} socket={} (not answering yet; `aterm keeper status`)",
        socket.display()
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn stop(args: &Args) -> ExitCode {
    let label = match chosen_label(args, "stop") {
        Ok(label) => label,
        Err(code) => return code,
    };
    match crate::job::state(&label) {
        Ok(crate::job::JobState::Absent) => {
            println!("keeper=absent label={label}");
            ExitCode::SUCCESS
        }
        Ok(crate::job::JobState::Loaded { .. }) => match crate::job::remove(&label) {
            Ok(()) => {
                println!("keeper=stopped label={label}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("aterm keeper stop: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("aterm keeper stop: launchctl: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn status(args: &Args) -> ExitCode {
    let place = crate::job::own_place();
    let Some(socket) = args.sock.clone().or_else(default_socket) else {
        println!("keeper=absent (no private directory for a socket)");
        return ExitCode::FAILURE;
    };
    let job = match crate::job::state(&place.label) {
        Ok(crate::job::JobState::Absent) => "absent".to_string(),
        Ok(crate::job::JobState::Loaded { pid: Some(pid) }) => format!("running pid={pid}"),
        Ok(crate::job::JobState::Loaded { pid: None }) => "loaded".to_string(),
        Err(e) => format!("unknown ({e})"),
    };
    let identity = match Identity::for_self(args.identity) {
        Ok(i) => i,
        Err(why) => {
            eprintln!("aterm keeper status: {why}");
            return ExitCode::FAILURE;
        }
    };
    let answer = crate::client::KeeperClient::connect(&socket, &identity, Duration::from_secs(2))
        .and_then(|c| c.status());
    match answer {
        Ok(text) => {
            println!("{text}");
            println!("label={} job={job}", place.label);
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("keeper=absent socket={} ({e})", socket.display());
            println!("label={} job={job}", place.label);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

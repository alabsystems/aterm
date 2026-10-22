// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE BOOT of a real `aterm --headless` for the live integration tests.
//!
//! The two ways the PRODUCT can fail to start — the child exits before its
//! control socket listens, or the socket never listens — both PANIC here with
//! the log tail: a startup crash is never a `SKIP` and a green test.
//!
//! READINESS IS THE INSTANCE'S OWN WORD (`--ready-fd`, `aterm_agent::ready`):
//! the instance writes one byte down a pipe once its control listener is bound,
//! and this boot blocks on that byte under the hang detector instead of polling
//! the socket. End-of-file with nothing written is a startup failure (the
//! instance exited, or runs with no socket), read off its exit status and log.
//!
//! A headless instance opens no window, and it is no WindowServer client. Until
//! 2026-09-26 it was one on macOS: `--headless` built the real AppKit event
//! loop, and an idle instance drew a WindowServer preflight within three
//! seconds (AGENTS.md). Headless now runs winit's display-free loop
//! (`crates/aterm-window/src/platform_impl/macos/headless.rs`: no
//! `NSApplication`, no connection), so CoreGraphics' refusal of a connection in
//! a child's log means the instance ASKED for one — a door reopened
//! ([`window_server_refused`]) — and the boot FAILS naming it.
//! `window_server_census_live_headless.rs` reads the same fact out of tccd's
//! log wherever a GUI session exists.
//!
//! A refusal that happens before or around the launch and says nothing about
//! the product — no scratch base short enough for `sun_path`, a log file that
//! cannot be created, a `spawn` that fails, a control-socket `bind` the OS
//! refuses with `EPERM`/`EACCES` (a sandboxed runner) — ends the test as COULD
//! NOT RUN (`aterm_spec::host::could_not_run`), never as a pass.
//!
//! A STAND-IN IS WHAT THE TAB RUNS, OR THE TEST DOES NOT RUN. A tab runs an
//! ordinary login shell, and macOS's system profile (`path_helper`) puts the
//! machine's own folders in front of the PATH the instance was booted with:
//! measured 2026-10-18, the worker tests' stand-in `claude` lost to Homebrew's
//! real Claude Code, which they started under the scratch HOME. So every boot
//! here has the scratch home's login profiles say that PATH again
//! ([`launch_isolation::pin_login_path`]), and a boot whose `configure` put a
//! stand-in ([`standin`]) in a folder on that PATH asks the instance's own login
//! shell — its boot session — what it names each one (`command -v`, which runs
//! nothing; [`login_resolution`]) before the test gets the instance. A name the
//! shell resolves to anything but its stand-in ends the test COULD NOT RUN
//! ([`STANDIN_UNRESOLVED`]): a line typed for the stand-in would have run that
//! instead. A stand-in off that PATH is reached by its absolute path alone, and
//! has no name to ask about.
//!
//! THE CLOCK. Every instance booted here runs its harness's and hosts' waits at
//! their test values: the fast-timers development seam
//! (`aterm_agent::harness::timings::FAST_SEAM`) is set before `configure`
//! runs, so a test whose property IS a production clock takes it out there
//! (`cmd.env_remove(..)`).
//!
//! Each file keeps its own scratch prefix (so parallel test binaries never
//! share a directory), its own launch arguments and its own readiness extras;
//! [`boot_with`] takes them. A test that launches its instances itself (several
//! sharing one scratch world, an explicit socket path) arms each with
//! [`arm_ready`] and waits with [`await_ready`]. The `aterm ctl` client helpers
//! ([`ctl`], [`ctl_ok`], [`field`], …) are here once for every live file. The
//! file that includes this module includes `support/launch_isolation.rs` as
//! `launch_isolation`, `support/child_output.rs` as `child_output` and
//! `support/ctl_accept.rs` as `ctl_accept` too.

#![allow(dead_code)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use aterm_agent::ready::{Ready, Signal};
use aterm_spec::host::could_not_run;
use aterm_spec::wait::MUST_HAPPEN;

pub use crate::ctl_accept::{until_served, was_never_accepted, was_turned_away};
use crate::{child_output, launch_isolation};

/// How often a readiness EXTRA ([`boot_with`]'s `ready`: a token file, a
/// journal line) is asked once the instance has said its socket listens. The
/// socket itself is never polled: the instance says so ([`arm_ready`]).
pub const POLL_GAP: Duration = Duration::from_millis(20);

/// One booted headless instance plus its scratch world, torn down (kill, reap,
/// remove) on every exit path — Drop runs on panic too, so a failing assert
/// never leaks a live aterm or a scratch dir.
pub struct Instance {
    pub child: Child,
    /// Cut after `child` is killed (fields drop after `Drop::drop`), and closed by
    /// the kernel if this test process dies first: the instance goes with it
    /// ([`launch_isolation::lifeline`]).
    _lifeline: aterm_uds::lifeline::Lifeline,
    pub tmp: PathBuf,
    pub log: PathBuf,
    /// The control socket (`<tmp>/run/aterm/aterm.sock`).
    pub sock: String,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        remove_world(&self.tmp);
    }
}

/// Remove a scratch world whose instances (and keeper) are dead. A session's
/// shell outlives the last holder of its PTY's master by a moment — the
/// hangup — and writes `home/.bash_history` as it goes: a file made after the
/// walk began leaves the scratch world behind (measured: two of about 130
/// boots, 2026-09-26). Removed again until it is gone, bounded; once `home/` is
/// gone nothing can write under it.
pub fn remove_world(root: &Path) {
    for _ in 0..40 {
        if std::fs::remove_dir_all(root).is_ok() || !root.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The instance's descendants as `ps` shows them now (pid, parent, state, wait
/// channel, age, command), for failure diagnostics: whether a session's shell is
/// alive and parked reading its terminal, still starting, or gone says what a
/// line typed into it met. Empty when `ps` cannot run.
#[cfg(unix)]
pub fn process_tree(inst: &Instance) -> String {
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,stat=,wchan=,etime=,command="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<(u32, u32, &str)> = text
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let (pid, ppid) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
            Some((pid, ppid, line.trim()))
        })
        .collect();
    let mut under = std::collections::BTreeSet::from([inst.child.id()]);
    loop {
        let before = under.len();
        for &(pid, ppid, _) in &rows {
            if under.contains(&ppid) {
                under.insert(pid);
            }
        }
        if under.len() == before {
            break;
        }
    }
    rows.iter()
        .filter(|(pid, _, _)| under.contains(pid))
        .map(|(_, _, line)| (*line).chars().take(200).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tail of the instance log, for failure diagnostics.
pub fn log_tail(log: &Path) -> String {
    let body = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(15);
    lines[start..].join("\n")
}

/// Whether `path` exists as a unix socket or a symlink (the `latest` alias).
pub fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket() || m.file_type().is_symlink())
        .unwrap_or(false)
}

/// A fresh exclusively created scratch root whose socket path fits `sun_path`
/// as its clients dial it ([`launch_isolation::sock_fits`]: the instance's
/// harness host dials the canonical spelling): `$TMPDIR` (via `temp_dir`), else
/// `/tmp`, prepared by [`launch_isolation::prepare`]. A failed attempt drops
/// only the directory this call created; a standing path is never adopted or
/// removed. The returned path transfers that ownership to its caller (an
/// [`Instance`] removes it). When no base can be created, fits or can be
/// prepared, the test is COULD NOT RUN: an environment refusal.
#[track_caller]
pub fn scratch_root(prefix: &str) -> PathBuf {
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let Ok(root) = aterm_tempfile::Builder::new()
            .prefix(&format!("{prefix}-"))
            .tempdir_in(base)
        else {
            continue;
        };
        if !launch_isolation::sock_fits(&root.path().join("run/aterm/aterm.sock")) {
            continue;
        }
        if launch_isolation::prepare(root.path()).is_err() {
            continue;
        }
        return root.keep();
    }
    could_not_run("no scratch base with a socket path short enough for sun_path")
}

/// Whether the log shows the OS refusing the control socket's `bind` — a
/// sandboxed runner, not a product failure.
fn bind_refused_by_the_os(log: &str) -> bool {
    log.lines().any(|l| {
        l.contains("control socket bind failed")
            && (l.contains("Operation not permitted") || l.contains("Permission denied"))
    })
}

/// Whether the log shows CoreGraphics refusing a WindowServer connection — its
/// own words when a process with no GUI session to join (an ssh login, a CI
/// box) asks for one. Read over the WHOLE log: the line comes first, well
/// before the tail a crash leaves.
fn window_server_refused(log: &str) -> bool {
    log.contains("FAILED TO establish the default connection to the WindowServer")
        || log.contains("_CGSDefaultConnection() is NULL")
}

/// A dead or never-ready child whose log shows it ASKED for a WindowServer
/// connection fails the boot, naming the door (module doc): a headless
/// instance never asks, so the refusal is the product's, not the login's.
fn fail_if_it_asked_for_the_window_server(log: &Path, tail: &str) {
    let whole = std::fs::read_to_string(log).unwrap_or_default();
    assert!(
        !window_server_refused(&whole),
        "aterm --headless asked for a WindowServer connection (CoreGraphics logged its refusal: \
         this login has no GUI session). A headless process never opens one — on macOS it runs \
         winit's display-free loop, with no NSApplication (AGENTS.md) — so a code path \
         reached AppKit or CoreGraphics: a door, not the environment. Log tail:\n{tail}"
    );
}

/// The environment refusal a dead or socketless child's log names, if any:
/// the OS refusing the socket's `bind`.
fn environment_refusal(tail: &str) -> Option<&'static str> {
    bind_refused_by_the_os(tail).then_some("the OS refused the control socket's bind")
}

/// [`boot_with`] with `args` as the extra launch arguments and no readiness
/// extras.
#[track_caller]
pub fn boot(prefix: &str, args: &[&str]) -> Instance {
    boot_with(
        prefix,
        |_, cmd| {
            cmd.args(args);
        },
        |_| true,
    )
}

/// Arm `cmd` — a `--headless` launch, before any `-e` payload — with its ready
/// signal (`--ready-fd`, the FIFO in `dir`): the instance says once that its
/// control socket listens, and [`await_ready`] waits on that word.
///
/// # Panics
/// The FIFO could not be made in `dir`: a scratch world this test cannot write
/// to is a broken fixture.
pub fn arm_ready(cmd: &mut Command, dir: &Path) -> Ready {
    let ready = Ready::open(dir)
        .unwrap_or_else(|e| panic!("arm the instance's ready signal in {}: {e}", dir.display()));
    ready.hand(cmd);
    ready
}

/// Boot one headless instance under a fresh scratch world named `prefix`.
///
/// `configure` runs after the isolation environment, the base arguments
/// (`--headless --no-reroute --control-sock <sock>`), the lifeline, the ready
/// signal and the fast-timers seam are set and before the launch (so an `-e`
/// payload it adds comes last): it may write into the scratch root (a config
/// table, fixtures), add arguments or development-seam variables, or take the
/// fast clock out. `ready` is asked once the instance has said its socket
/// listens (a token file, say), and again every [`POLL_GAP`] until it holds;
/// the instance is returned then.
///
/// An environment refusal (module doc) is COULD NOT RUN. A child that exits
/// before it is ready, one that closes its ready signal with nothing said, or
/// one that is not ready within the hang detector PANICS with the log tail.
#[track_caller]
pub fn boot_with(
    prefix: &str,
    configure: impl FnOnce(&Path, &mut Command),
    ready: impl Fn(&Instance) -> bool,
) -> Instance {
    boot_under(prefix, None, configure, ready)
}

/// [`boot_with`], the instance launched by `/usr/bin/sandbox-exec` under the
/// Seatbelt `profile` (SBPL), which it and everything it starts keep for good:
/// a network witness, say, that kills a process at its first connection to
/// anything but loopback. `sandbox-exec` execs the instance in place, so the
/// child's pid is the instance's. macOS only.
#[cfg(target_os = "macos")]
#[track_caller]
pub fn boot_jailed(
    prefix: &str,
    profile: &str,
    configure: impl FnOnce(&Path, &mut Command),
    ready: impl Fn(&Instance) -> bool,
) -> Instance {
    boot_under(prefix, Some(profile), configure, ready)
}

#[track_caller]
fn boot_under(
    prefix: &str,
    jail: Option<&str>,
    configure: impl FnOnce(&Path, &mut Command),
    ready: impl Fn(&Instance) -> bool,
) -> Instance {
    let tmp = scratch_root(prefix);
    let log = tmp.join("gui.log");
    let (out, err) = match std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            could_not_run(format!("cannot open the instance log ({e})"))
        }
    };
    let aterm = launch_isolation::aterm_executable();
    // The system's first-exec wait, paid here and not on the ready wait below
    // ([`aterm_spec::wait::first_exec`]); `--help` is answered above the mode
    // fork, before any setup.
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    let mut cmd = match jail {
        None => Command::new(&aterm),
        Some(profile) => {
            let mut cmd = Command::new("/usr/bin/sandbox-exec");
            cmd.args(["-p", profile]).arg(&aterm);
            cmd
        }
    };
    launch_isolation::apply(&mut cmd, &tmp);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .args(launch_isolation::control_sock(&tmp))
        .env(aterm_agent::harness::timings::FAST_SEAM, "1")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let lifeline = launch_isolation::lifeline(&mut cmd, &tmp);
    let mut signal = arm_ready(&mut cmd, &tmp);
    CONFIGURED.set(Some(Vec::new()));
    configure(&tmp, &mut cmd);
    let standins = CONFIGURED.take().unwrap_or_default();
    let path = launch_path(&cmd);
    if let Some(path) = &path
        && let Err(e) = launch_isolation::pin_login_path(&tmp, path)
    {
        let _ = std::fs::remove_dir_all(&tmp);
        could_not_run(format!("cannot pin the scratch home's login PATH ({e})"));
    }
    let by_name = reached_by_name(standins, path.as_deref());
    if !by_name.is_empty() && cmd.get_args().any(|a| a == "-e" || a == "--") {
        let _ = std::fs::remove_dir_all(&tmp);
        could_not_run(format!(
            "{STANDIN_UNRESOLVED}: none was asked — the boot session runs a program of its own \
             (`-e`), so no login shell of the instance can be asked what it names {by_name:?}"
        ));
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            could_not_run(format!("cannot launch aterm --headless ({e})"))
        }
    };
    let sock_path = tmp.join("run/aterm/aterm.sock");
    let mut inst = Instance {
        child,
        _lifeline: lifeline,
        sock: sock_path.to_string_lossy().into_owned(),
        tmp,
        log,
    };
    let log = inst.log.clone();
    await_ready(&mut inst, |i| &mut i.child, &mut signal, &log, ready);
    if !by_name.is_empty() {
        prove_standins_resolve(&inst, &by_name);
    }
    inst
}

std::thread_local! {
    /// The stand-ins [`standin`] puts in place while a boot's `configure` runs
    /// ([`boot_with`], on that boot's thread); `None` outside one.
    static CONFIGURED: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

/// The opening words of the refusal [`boot_with`] ends a test with when the
/// instance's login shell names a stand-in by another path (module doc).
pub const STANDIN_UNRESOLVED: &str = "the instance's login shell names a stand-in by another path";

/// The PATH `cmd` launches with: the one set on it, none where it was removed,
/// else this process's, which it inherits.
fn launch_path(cmd: &Command) -> Option<std::ffi::OsString> {
    match cmd.get_envs().find(|(key, _)| *key == "PATH") {
        Some((_, value)) => value.map(std::ffi::OsStr::to_owned),
        None => std::env::var_os("PATH"),
    }
}

/// Whether `a` and `b` name one file or folder: the same text, or the same
/// place once both resolve.
fn same_place(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// The stand-ins of `standins` a shell reaches by name: each whose folder is
/// an entry of `path`, once.
fn reached_by_name(standins: Vec<PathBuf>, path: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let Some(path) = path else {
        return Vec::new();
    };
    let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    let mut out: Vec<PathBuf> = Vec::new();
    for standin in standins {
        let on_path = standin
            .parent()
            .is_some_and(|dir| dirs.iter().any(|entry| same_place(entry, dir)));
        if on_path && !out.contains(&standin) {
            out.push(standin);
        }
    }
    out
}

/// THE GUARD ([`boot_with`], module doc): the instance's own login shell names
/// each of `standins` by the stand-in's path, or the test ends COULD NOT RUN
/// ([`STANDIN_UNRESOLVED`]) with what it named instead — before the test can
/// type anything: nothing has been typed but `command -v`.
#[track_caller]
fn prove_standins_resolve(inst: &Instance, standins: &[PathBuf]) {
    let names: Vec<&str> = standins
        .iter()
        .map(|standin| {
            standin
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_else(|| panic!("a stand-in named in UTF-8: {}", standin.display()))
        })
        .collect();
    let said = login_resolution(inst, &names);
    let wrong: Vec<String> = standins
        .iter()
        .zip(&names)
        .enumerate()
        .filter_map(|(at, (standin, name))| {
            let named = said.get(at).map_or("", String::as_str);
            (named.is_empty() || !same_place(Path::new(named), standin)).then(|| {
                format!(
                    "`{name}` is {named:?}, not the stand-in {}",
                    standin.display()
                )
            })
        })
        .collect();
    if said.len() != standins.len() || !wrong.is_empty() {
        could_not_run(format!(
            "{STANDIN_UNRESOLVED}, so a line typed for it would run that instead; nothing was \
             typed but `command -v`: {} (the shell said {said:?})",
            wrong.join("; ")
        ));
    }
}

/// What the instance's own login shell — its boot session's — names each of
/// `names` (`command -v`, which runs nothing): one entry a name, empty where
/// nothing answers. Typed into the boot session as one line and read back
/// from a file in the scratch home once that line has run (a hang detector);
/// the line stays in the session's history and on its screen.
///
/// # Panics
/// A name a shell would read as more than one word, or a scratch root it would
/// read as more than one quoted word, is a broken fixture; a line the shell
/// has not run within the hang detector fails with the session's screen.
#[track_caller]
pub fn login_resolution(inst: &Instance, names: &[&str]) -> Vec<String> {
    for name in names {
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "a stand-in name a shell reads as one word: {name:?}"
        );
    }
    let home = inst.tmp.join("home");
    let done = home.join(".standin-resolved");
    let part = home.join(".standin-resolved.part");
    let quoted = |p: &Path| {
        let text = p.to_str().expect("a UTF-8 scratch root").to_owned();
        assert!(!text.contains('\''), "quoted below: {text}");
        format!("'{text}'")
    };
    let (done_q, part_q) = (quoted(&done), quoted(&part));
    let _ = std::fs::remove_file(&done);
    let asks: String = names
        .iter()
        .map(|name| format!("command -v {name} || echo; "))
        .collect();
    let (_, sid) = boot_session(inst);
    type_line(
        inst,
        &sid,
        &format!("{{ {asks}}} >{part_q} 2>/dev/null; mv {part_q} {done_q}"),
    );
    let said = aterm_spec::wait::within(MUST_HAPPEN, POLL_GAP, || {
        std::fs::read_to_string(&done).ok()
    });
    let Some(said) = said else {
        let screen =
            String::from_utf8_lossy(&ctl(inst, &[&format!("@{sid}"), "text", "trim"]).stdout)
                .into_owned();
        panic!(
            "the boot session's shell did not run the `command -v` line within {MUST_HAPPEN:?}; \
             its screen:\n{screen}\nprocesses:\n{}\nlog tail:\n{}",
            process_tree(inst),
            log_tail(&inst.log)
        );
    };
    let _ = std::fs::remove_file(&done);
    said.lines().map(str::to_owned).collect()
}

/// Wait for a spawned headless instance — `inst`, whose child `child` reaches,
/// armed with `signal` ([`arm_ready`]) — to say its control socket listens,
/// then for `ready` to hold. A child that exits first, closes its signal with
/// nothing said, or says nothing within the hang detector PANICS with the tail
/// of `log` — the product failed to start (module doc), and one whose log shows
/// it asked for a WindowServer connection says so; an environment refusal in
/// the log is COULD NOT RUN. `inst`'s own drop reaps it.
#[track_caller]
pub fn await_ready<T>(
    inst: &mut T,
    child: impl Fn(&mut T) -> &mut Child,
    signal: &mut Ready,
    log: &Path,
    ready: impl Fn(&T) -> bool,
) {
    match signal.wait(MUST_HAPPEN) {
        Signal::Ready => {}
        Signal::Closed => {
            // Closed: it exited, or it runs with no socket. An environment
            // refusal in the log (a sandbox's `EPERM` on the bind, which leaves
            // the instance running) is said at once; otherwise its exit, if it
            // is on its way out, is waited for — bounded — so the verdict names it.
            let tail = log_tail(log);
            fail_if_it_asked_for_the_window_server(log, &tail);
            if let Some(why) = environment_refusal(&tail) {
                could_not_run(format!("{why}; log tail:\n{tail}"));
            }
            let status = aterm_spec::wait::within(MUST_HAPPEN, POLL_GAP, || {
                child(inst).try_wait().ok().flatten()
            });
            let tail = log_tail(log);
            if let Some(why) = environment_refusal(&tail) {
                could_not_run(format!("{why}; log tail:\n{tail}"));
            }
            match status {
                Some(status) => panic!(
                    "aterm --headless exited ({status}) before its control socket listened — a \
                     startup failure, not an environment refusal; log tail:\n{tail}"
                ),
                None => panic!(
                    "aterm --headless closed its ready signal with nothing said and is still \
                     running: it has no control socket; log tail:\n{tail}"
                ),
            }
        }
        Signal::TimedOut => {
            let tail = log_tail(log);
            fail_if_it_asked_for_the_window_server(log, &tail);
            if let Some(why) = environment_refusal(&tail) {
                could_not_run(format!("{why}; log tail:\n{tail}"));
            }
            let stack = thread_stack(child(inst).id(), log, None);
            panic!(
                "aterm --headless did not say its control socket listens within {MUST_HAPPEN:?}; \
                 log tail:\n{tail}\nits main thread now:\n{stack}"
            );
        }
        Signal::Failed(e) => panic!("could not read the instance's ready signal: {e}"),
    }
    let extras = aterm_spec::wait::within(MUST_HAPPEN, POLL_GAP, || ready(inst).then_some(()));
    assert!(
        extras.is_some(),
        "aterm --headless listens, but its readiness extras did not hold within \
         {MUST_HAPPEN:?}; log tail:\n{}",
        log_tail(log)
    );
}

/// Where a hung instance waits: one of its threads as `/usr/bin/sample`
/// reads it over one second (macOS) — the main thread, or the first whose
/// line names `thread` (a Rust thread's name, `aterm-spare`) — its call
/// graph's first [`STACK_LINES`] lines, written beside `log` and read back. A
/// log names only the last step that finished: one startup hang, 1 in 160
/// runs of the live spare module, said `headless mode` and nothing more. What
/// could not be read says so instead.
pub fn thread_stack(pid: u32, log: &Path, thread: Option<&str>) -> String {
    /// Enough for the frames from a thread's entry to its wait, at aterm's
    /// depth.
    const STACK_LINES: usize = 80;
    if !cfg!(target_os = "macos") {
        return "(no sampler on this platform)".to_owned();
    }
    let file = log.with_file_name("hang.sample.txt");
    // A hang detector on the sampler too: it suspends and symbolicates the
    // instance, and a report that waited on it unbounded could outlast the
    // very hang it reports.
    let sampled = aterm_uds::spawnfd::command("/usr/bin/sample")
        .arg(pid.to_string())
        .args(["1", "-mayDie", "-file"])
        .arg(&file)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .map(|mut sampler| {
            let ended = aterm_spec::wait::within(MUST_HAPPEN, POLL_GAP, || {
                sampler.try_wait().ok().flatten()
            });
            if ended.is_none() {
                let _ = sampler.kill();
                let _ = sampler.wait();
            }
            ended
        });
    let text = match (sampled, std::fs::read_to_string(&file)) {
        (Ok(Some(_)), Ok(text)) => text,
        (sampled, read) => return format!("(not sampled: {sampled:?}, {read:?})"),
    };
    let _ = std::fs::remove_file(&file);
    let lines: Vec<&str> = text.lines().collect();
    let Some(graph) = lines.iter().position(|l| l.starts_with("Call graph:")) else {
        return "(the sample has no call graph)".to_owned();
    };
    // A frame's depth is where its count starts, past the tree's own marks
    // (`+ ! : |`, which `sample` draws once a graph has several threads).
    let depth = |l: &str| l.len() - l.trim_start_matches([' ', '+', '!', ':', '|']).len();
    // The call graph's first thread is the main one (`Main Thread`, or the
    // main dispatch queue's); every thread's line sits at that depth.
    let Some(top) = lines.get(graph + 1).map(|l| depth(l)) else {
        return "(the sample's call graph is empty)".to_owned();
    };
    let Some(at) = (graph + 1..lines.len())
        .find(|&k| depth(lines[k]) == top && thread.is_none_or(|name| lines[k].contains(name)))
    else {
        return format!("(the sample names no thread {thread:?})");
    };
    let below = lines[at + 1..].iter().take_while(|l| depth(l) > top);
    std::iter::once(lines[at])
        .chain(below.copied())
        .take(STACK_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `ctl` client that has not exited by now is hung: past every minute-long
/// hang wait a call makes, so a server-side `await` that times out answers
/// before it is killed.
pub const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(120);

/// `aterm ctl --sock <sock> <args…>` against `inst`, in its scratch world,
/// stdout and stderr piped — not yet spawned.
pub fn client_command(inst: &Instance, args: &[&str]) -> Command {
    let aterm = launch_isolation::aterm_executable();
    // The file [`boot_with`] ran before its launch, so one lookup; a file cargo
    // has put there since is run here, before the client's clock.
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    let mut cmd = Command::new(&aterm);
    cmd.arg("ctl")
        .arg("--sock")
        .arg(&inst.sock)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, &inst.tmp);
    cmd
}

/// `aterm drive <args…>` in `root`'s scratch world (its HOME, XDG roots and
/// `$TMPDIR`, no inherited aterm variable), `extra` set on top and run from
/// `cwd` where one is named, read until it exits under the client's hang
/// detector ([`CLIENT_EXIT_DEADLINE`]). No socket is named: a command that
/// wants one is given `--socket` in `args`.
pub fn drive_in(root: &Path, args: &[&str], extra: &[(&str, &str)], cwd: Option<&Path>) -> Output {
    let aterm = launch_isolation::aterm_executable();
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    let mut cmd = Command::new(&aterm);
    cmd.arg("drive")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, root);
    for (key, value) in extra {
        cmd.env(key, value);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let child = cmd.spawn().expect("spawn aterm drive");
    child_output::wait_and_read(child, Instant::now() + CLIENT_EXIT_DEADLINE)
        .unwrap_or_else(|| panic!("aterm drive {args:?} did not exit in time"))
}

/// What a finished client `out` said, for a failure's message.
pub fn said(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// One bounded `aterm ctl` call, read until it exits
/// (`support/child_output.rs`: a sibling's spawn can hold its pipes open), asked
/// again while the instance has not yet ACCEPTED it or answers it busy
/// ([`until_served`]): either way nothing was served.
pub fn ctl(inst: &Instance, args: &[&str]) -> Output {
    until_served(|| {
        let child = client_command(inst, args).spawn().expect("spawn aterm ctl");
        child_output::wait_and_read(child, Instant::now() + CLIENT_EXIT_DEADLINE)
            .unwrap_or_else(|| panic!("aterm ctl {args:?} did not exit in time"))
    })
}

/// `aterm ctl --sock <sock> subscribe <args…>` against `inst`, returned once the
/// server has ACKNOWLEDGED the subscription (`OK subscribe <n>`, which the client
/// writes to stderr before any frame): from then on every push reaches the
/// child's stdout, so a test that moves something next sees it. A client the
/// instance never accepted ([`was_never_accepted`]) is started again, up to the
/// hang detector, as is one the instance answered busy ([`was_turned_away`]);
/// one that ends any other way before its acknowledgement fails
/// the test with what it said.
pub fn subscribed(inst: &Instance, args: &[&str]) -> Child {
    let mut request = vec!["subscribe"];
    request.extend_from_slice(args);
    let give_up = Instant::now() + MUST_HAPPEN;
    loop {
        let mut child = client_command(inst, &request)
            .spawn()
            .expect("spawn aterm ctl subscribe");
        // The client prefixes every stderr line with its name: `aterm-ctl: OK subscribe 1`.
        let acked = child_output::await_stderr_line(
            &mut child,
            |line| {
                line.trim_start_matches("aterm-ctl: ")
                    .starts_with("OK subscribe")
            },
            Instant::now() + CLIENT_EXIT_DEADLINE,
        );
        match acked {
            Ok(()) => return child,
            Err(child_output::NoLine::Exited(out))
                if (was_never_accepted(&out) || was_turned_away(&out))
                    && Instant::now() < give_up => {}
            Err(child_output::NoLine::Exited(out)) => panic!(
                "aterm ctl {request:?} ended before its subscription was acknowledged:\n{}",
                said(&out)
            ),
            Err(child_output::NoLine::Hung(stderr)) => panic!(
                "aterm ctl {request:?} was not acknowledged within {}s; its stderr:\n{stderr}",
                CLIENT_EXIT_DEADLINE.as_secs()
            ),
            Err(child_output::NoLine::OutputLimit { stream, captured }) => panic!(
                "aterm ctl {request:?} reached the {} byte {stream} diagnostic limit before \
                 its subscription was acknowledged:\n{captured}",
                child_output::ACK_OUTPUT_LIMIT
            ),
        }
    }
}

/// [`ctl`] that must succeed: its stdout.
pub fn ctl_ok(inst: &Instance, args: &[&str]) -> String {
    let out = ctl(inst, args);
    assert!(
        out.status.success(),
        "aterm ctl {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

/// Put the executable script `text` at `path`, replacing whatever is there,
/// and return once the file it names has been run: ONE FILE PER TEXT, kept
/// under the build's `CARGO_TARGET_TMPDIR` by a digest of the text and
/// hard-linked into place, and run once per process (`--version`, every
/// output and input null) before any caller returns.
///
/// WHY. macOS assesses an executable file at its FIRST exec — per inode, one
/// file at a time machine-wide — and every later exec of it, from any
/// program and through any link, takes milliseconds (the first one 0.2 s idle
/// and 6.6 s on a loaded machine; links to a file already run skip it). A first
/// exec the PRODUCT made paid it where a test could not wait for it: the host's
/// version probe of a fake twin (bounded at 5 s,
/// `aterm_agent::harness::upgrade_drive`'s `VERSION_WAIT`), and a `/bin/sh`
/// script still running from a path its replacement was renamed onto
/// stopped at its next exec until that check ended (5.2 s to 8.5 s at a load
/// of 15 to 50). So a test ran each fake it wrote once before the product met
/// it — and wrote every fake anew: `host_live` paid the check 50 times a run
/// for six texts, 0.15 s to 2.7 s each with its tests started together at a
/// load of 10 to 30, three times in each upgrade test.
///
/// So every text a test writes the same way is one inode, checked once: by
/// the first run of this build tree that met it, and run here once per
/// process, a caller that finds it being run WAITING for that run to end. A
/// TEXT THAT HOLDS A PATH IS SHARED BY NO ONE: a fake finds its scratch world
/// from `$HOME` (`<root>/home`, `launch_isolation::apply`), as it finds its
/// `.claude` there. The shared file is read-only, so a fake written over in
/// place fails loudly instead of rewriting every test's: replace one through
/// here, or by a rename, which swaps the link. A link that cannot be made
/// (another filesystem) is a file of its own, run here once: slower, and as
/// correct.
///
/// Every text given must answer `--version` at once, or end at once with
/// nothing to draw and nothing written.
///
/// Put from a boot's `configure` ([`boot_with`]) in a folder on the instance's
/// PATH, the stand-in is one the instance's login shell must name by `path`
/// before the test gets the instance, or the test does not run (module doc).
///
/// # Panics
/// When `path`'s directory cannot be made or the script cannot be written: a
/// test without its fake decides nothing.
pub fn standin(path: &Path, text: &str) {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    static SHARED: Mutex<BTreeMap<String, Arc<OnceLock<Option<PathBuf>>>>> =
        Mutex::new(BTreeMap::new());

    CONFIGURED.with_borrow_mut(|configured| {
        if let Some(standins) = configured {
            standins.push(path.to_owned());
        }
    });
    let once = Arc::clone(
        SHARED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(text.to_owned())
            .or_default(),
    );
    let shared = once.get_or_init(|| {
        let file = shared_file(text)?;
        run_once(&file);
        Some(file)
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    let _ = std::fs::remove_file(path);
    if shared
        .as_ref()
        .is_some_and(|file| std::fs::hard_link(file, path).is_ok())
    {
        return;
    }
    std::fs::write(path, text).expect("write the fake");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    run_once(path);
}

/// The build tree's read-only copy of `text` ([`standin`]): made by whichever
/// process first needs it — written under a name of its own, then linked
/// into place, so a concurrent maker finds it whole or makes it — and
/// trusted only when it reads back as `text` (a digest is no proof) and is
/// still read-only and executable: the mode is what keeps one test from
/// writing every test's fake, so a file that lost it (a `chmod -R u+w` of the
/// target dir, say) is not shared — the caller writes its own.
fn shared_file(text: &str) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::PermissionsExt;
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("aterm-live-standins");
    std::fs::create_dir_all(&dir).ok()?;
    let mut digest = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut digest);
    let name = format!("{:016x}-{}", digest.finish(), text.len());
    let file = dir.join(&name);
    let whole = |file: &Path| {
        std::fs::metadata(file)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o777 == 0o555)
            && std::fs::read(file).ok().as_deref() == Some(text.as_bytes())
    };
    if !whole(&file) {
        let own = dir.join(format!(".{name}.{}", std::process::id()));
        // A copy a killed run of this pid left behind is read-only too.
        let _ = std::fs::remove_file(&own);
        std::fs::write(&own, text).ok()?;
        std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o555)).ok()?;
        let _ = std::fs::hard_link(&own, &file);
        let _ = std::fs::remove_file(&own);
    }
    whole(&file).then_some(file)
}

/// Run `file` once, unbounded, `--version` its argument and every output and
/// input null: the first exec the system checks ([`standin`]).
fn run_once(file: &Path) {
    let _ = Command::new(file)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `key=value` out of a status/sessions line.
pub fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
}

/// The boot session's `(local, sid)` from `sessions`.
pub fn boot_session(inst: &Instance) -> (u64, String) {
    let body = ctl_ok(inst, &["sessions"]);
    let row = body
        .lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("a session row: {body}"));
    let mut toks = row.split_whitespace();
    let local = toks.next().unwrap().parse().unwrap();
    let sid = toks.next().unwrap().to_string();
    (local, sid)
}

/// The session's `status` record.
pub fn status(inst: &Instance, sid: &str) -> String {
    ctl_ok(inst, &[&format!("@{sid}"), "status"])
}

/// Type one line into the session and submit it.
pub fn type_line(inst: &Instance, sid: &str, line: &str) {
    ctl_ok(inst, &[&format!("@{sid}"), "send", line]);
    ctl_ok(inst, &[&format!("@{sid}"), "key", "enter"]);
}

/// A zsh for a live test, found as every live suite finds its shells
/// ([`aterm_spec::host::test_shell`]): `$ATERM_TEST_ZSH`, which
/// fails loudly when it names no file, else the usual places, else `$PATH`,
/// else `None`: the host has none. A tab told `SHELL=<missing>` starts the
/// person's login shell instead (`aterm: … does not exist (from $SHELL)`), so
/// a test that names a zsh the host lacks runs bash and proves nothing of zsh:
/// it asks [`need_zsh`] or [`login_shell`] instead.
pub fn zsh() -> Option<&'static str> {
    static ZSH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ZSH.get_or_init(|| {
        aterm_spec::host::test_shell("zsh").map(|path| path.to_string_lossy().into_owned())
    })
    .as_deref()
}

/// [`zsh`] for a test whose claim is zsh's own: COULD NOT RUN where the host
/// has none, never a pass under the shell aterm would start instead. The
/// refusal names the test's line, not this one.
#[track_caller]
pub fn need_zsh() -> &'static str {
    match zsh() {
        Some(zsh) => zsh,
        None => could_not_run(aterm_spec::host::tool("zsh").refusal()),
    }
}

/// A login shell for a live test whose claim holds under every shell aterm
/// integrates alike, and the startup file a tab of it reads from the
/// instance's home.
#[derive(Clone, Copy, Debug)]
pub struct LoginShell {
    pub path: &'static str,
    /// `.zshrc` or `.bashrc`: aterm's loader reads it in every tab (bash's,
    /// after the login profiles on macOS).
    pub rc: &'static str,
    pub kind: aterm_shell_integration::ShellType,
}

/// zsh where the host has one (every Mac; the lab's Linux lane), else bash,
/// which every Unix host aterm supports has at `/bin/bash`. An rc a test
/// writes for it reads the same in either: no `print`, no `setopt`.
pub fn login_shell() -> LoginShell {
    use aterm_shell_integration::ShellType;
    match zsh() {
        Some(path) => LoginShell {
            path,
            rc: ".zshrc",
            kind: ShellType::Zsh,
        },
        None => LoginShell {
            path: "/bin/bash",
            rc: ".bashrc",
            kind: ShellType::Bash,
        },
    }
}

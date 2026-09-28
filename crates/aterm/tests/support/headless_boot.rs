// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE BOOT of a real `aterm --headless` for the live integration tests.
//!
//! Eight test files carried a copy of this function, and every copy turned the
//! two ways the PRODUCT can fail to start — the child exits before its control
//! socket listens, or the socket never listens — into `SKIP` and a green test:
//! `targo test -p aterm` reported green on exactly the startup crash the verify
//! gate's own headless smoke (`aterm-verify` `smoke_stages.rs` `bring_up`)
//! reports red. Here both PANIC with the log tail, as the smoke fails them.
//!
//! A headless instance opens no window, but it is NOT free of WindowServer: on
//! macOS there is no display-free winit backend, so `--headless` builds the
//! real AppKit event loop (`headless_activation.rs`), and an idle instance drew
//! a WindowServer preflight within three seconds (AGENTS.md rule 5, measured
//! 2026-09-26). So a login with NO GUI session — an ssh shell, a CI box — is
//! an environment this boot can meet: AppKit then logs its refusal to connect
//! ([`window_server_refused`]), and a child that exits with that in its log is
//! a `SKIP` naming it. Every other early exit still fails.
//!
//! A `SKIP` (`None`, announced on stderr) is kept for refusals that happen
//! before or around the launch and say nothing about the product: no scratch
//! base short enough for `sun_path`, a log file that cannot be created, a
//! `spawn` that fails, a control-socket `bind` the OS refuses with
//! `EPERM`/`EACCES` (a sandboxed runner), and AppKit's refused WindowServer
//! connection (above).
//!
//! Each file keeps its own scratch prefix (so parallel test binaries never
//! share a directory), its own launch arguments and its own readiness extras;
//! [`boot_with`] takes them. The file that includes this module includes
//! `support/launch_isolation.rs` as `launch_isolation` too.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::launch_isolation;

/// The socket-bind budget (the verify smoke's, tripled): GENEROUS, because it
/// only turns a wedged boot into a named failure instead of a hung run.
pub const SOCKET_POLLS: usize = 300;
pub const POLL_GAP: Duration = Duration::from_millis(100);

/// `sockaddr_un.sun_path` is ~104 bytes on macOS/BSD; a base that would
/// overflow it (with margin) is refused instead of failing deep in `bind`.
pub const MAX_SOCK_PATH: usize = 100;

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
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

/// The tail of the instance log, for failure diagnostics.
pub fn log_tail(log: &Path) -> String {
    let body = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(15);
    lines[start..].join("\n")
}

/// Whether `path` exists as a unix socket or a symlink (the `latest` alias) —
/// the verify smoke's readiness probe.
pub fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket() || m.file_type().is_symlink())
        .unwrap_or(false)
}

/// A scratch base whose socket path fits `sun_path`: `$TMPDIR` (via
/// `temp_dir`), else `/tmp`, prepared by [`launch_isolation::prepare`]. `None`
/// when neither fits — an environment refusal.
pub fn scratch_root(prefix: &str) -> Option<PathBuf> {
    let name = format!("{prefix}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let tmp = base.join(&name);
        let sock = tmp.join("run/aterm/aterm.sock");
        if sock.as_os_str().len() >= MAX_SOCK_PATH {
            continue;
        }
        if launch_isolation::prepare(&tmp).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            continue;
        }
        return Some(tmp);
    }
    None
}

/// Whether the log shows the OS refusing the control socket's `bind` — a
/// sandboxed runner, not a product failure.
fn bind_refused_by_the_os(log: &str) -> bool {
    log.lines().any(|l| {
        l.contains("control socket bind failed")
            && (l.contains("Operation not permitted") || l.contains("Permission denied"))
    })
}

/// Whether the log shows AppKit refusing the WindowServer connection at
/// startup — CoreGraphics' own words when the process has no GUI session to
/// join (an ssh login, a CI box): an environment refusal, not a product
/// failure. Read over the WHOLE log: the line comes first, well before the
/// tail a crash leaves.
fn window_server_refused(log: &str) -> bool {
    log.contains("FAILED TO establish the default connection to the WindowServer")
        || log.contains("_CGSDefaultConnection() is NULL")
}

/// The environment refusal a dead or never-ready child's log names, if any:
/// the OS refusing the socket's `bind`, or AppKit refusing WindowServer.
fn environment_refusal(log: &Path, tail: &str) -> Option<&'static str> {
    if bind_refused_by_the_os(tail) {
        return Some("the OS refused the control socket's bind");
    }
    let whole = std::fs::read_to_string(log).unwrap_or_default();
    window_server_refused(&whole)
        .then_some("AppKit could not connect to the WindowServer (a login with no GUI session)")
}

/// [`boot_with`] with `args` as the extra launch arguments and no readiness
/// extras.
pub fn boot(prefix: &str, args: &[&str]) -> Option<Instance> {
    boot_with(
        prefix,
        |_, cmd| {
            cmd.args(args);
        },
        |_| true,
    )
}

/// Boot one headless instance under a fresh scratch world named `prefix`.
///
/// `configure` runs after the isolation environment, the base arguments
/// (`--headless --no-reroute --control-sock <sock>`) and the lifeline are set
/// and before the launch (so an `-e` payload it adds comes last): it may write into the scratch root (a config table, fixtures) and
/// add arguments or development-seam variables. `ready` is asked each poll
/// once the socket listens (a token file, say); the instance is returned when
/// it holds.
///
/// `None` only for an environment refusal (module doc), announced as `SKIP`.
/// A child that exits before it is ready, or one that is not ready within the
/// budget, PANICS with the log tail.
pub fn boot_with(
    prefix: &str,
    configure: impl FnOnce(&Path, &mut Command),
    ready: impl Fn(&Instance) -> bool,
) -> Option<Instance> {
    let Some(tmp) = scratch_root(prefix) else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return None;
    };
    let log = tmp.join("gui.log");
    let (out, err) = match std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("SKIP: cannot open the instance log ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &tmp);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .args(launch_isolation::control_sock(&tmp))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let lifeline = launch_isolation::lifeline(&mut cmd, &tmp);
    configure(&tmp, &mut cmd);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: cannot launch aterm --headless ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
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
    await_ready(&mut inst, |i| &mut i.child, &sock_path, &log, ready).then_some(inst)
}

/// Wait for a spawned headless instance — `inst`, whose child `child` reaches —
/// to listen on `sock` and satisfy `ready`: `true` once it does, `false`
/// (announced `SKIP`) when its log names an environment refusal (the OS refused
/// the socket's `bind`, or AppKit refused the WindowServer connection). A child that
/// exits first, or one not ready within the budget, PANICS with the tail of
/// `log` — the product failed to start (module doc) — and `inst`'s own drop
/// reaps it. For a test that launches its instances itself (several sharing
/// one scratch world, an explicit socket path); [`boot_with`] uses it too.
pub fn await_ready<T>(
    inst: &mut T,
    child: impl Fn(&mut T) -> &mut Child,
    sock: &Path,
    log: &Path,
    ready: impl Fn(&T) -> bool,
) -> bool {
    for _ in 0..SOCKET_POLLS {
        match child(inst).try_wait() {
            Ok(Some(status)) => {
                let tail = log_tail(log);
                if let Some(why) = environment_refusal(log, &tail) {
                    eprintln!("SKIP: {why}; log tail:\n{tail}");
                    return false;
                }
                panic!(
                    "aterm --headless exited ({status}) before its control socket listened — a \
                     startup failure, not an environment refusal; log tail:\n{tail}"
                );
            }
            Ok(None) => {}
            Err(e) => panic!("could not poll the aterm --headless child: {e}"),
        }
        if is_socket_or_symlink(sock) && launch_isolation::control_listening(sock) && ready(inst) {
            return true;
        }
        std::thread::sleep(POLL_GAP);
    }
    let tail = log_tail(log);
    if let Some(why) = environment_refusal(log, &tail) {
        eprintln!("SKIP: {why}; log tail:\n{tail}");
        return false;
    }
    panic!(
        "aterm --headless was not ready within {:?} (its control socket never listened at {}); \
         log tail:\n{tail}",
        POLL_GAP * SOCKET_POLLS as u32,
        sock.display()
    );
}

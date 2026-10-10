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
//! A headless instance opens no window, and it is no WindowServer client. Until
//! 2026-09-26 it was one on macOS: `--headless` built the real AppKit event
//! loop, and an idle instance drew a WindowServer preflight within three
//! seconds (AGENTS.md rule 5) — so for that while a login with NO GUI session
//! (an ssh shell, a CI box) made AppKit log its refusal to connect, and a child
//! that exited with that in its log was a `SKIP`. Headless now runs winit's
//! display-free loop (`vendor/winit/src/platform_impl/macos/headless.rs`: no
//! `NSApplication`, no connection), so that refusal in a child's log means the
//! instance ASKED for the connection — a door reopened
//! ([`window_server_refused`]) — and the boot FAILS naming it, never skips.
//! `window_server_census_live_headless.rs` reads the same fact out of tccd's
//! log wherever a GUI session exists.
//!
//! A `SKIP` (`None`, announced on stderr) is kept for refusals that happen
//! before or around the launch and say nothing about the product: no scratch
//! base short enough for `sun_path`, a log file that cannot be created, a
//! `spawn` that fails, and a control-socket `bind` the OS refuses with
//! `EPERM`/`EACCES` (a sandboxed runner).
//!
//! Each file keeps its own scratch prefix (so parallel test binaries never
//! share a directory), its own launch arguments and its own readiness extras;
//! [`boot_with`] takes them. The `aterm ctl` client helpers ([`ctl`],
//! [`ctl_ok`], [`field`], …) are here once for every live file. The file that
//! includes this module includes `support/launch_isolation.rs` as
//! `launch_isolation` and `support/child_output.rs` as `child_output` too.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::{child_output, launch_isolation};

/// The socket-bind budget, a minute of [`POLL_GAP`]s: a hang detector
/// (AGENTS.md), never a latency budget — it only turns a wedged boot into a
/// named failure instead of a hung run, and a boot that listens ends it at once.
pub const SOCKET_POLLS: usize = 600;
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
        // The session's shell outlives the instance by a moment — its PTY's
        // hangup — and writes `home/.bash_history` as it goes: a file made
        // after the walk began leaves the scratch world behind (measured:
        // two of about 130 boots, 2026-09-26). Removed again until it is
        // gone, bounded; once `home/` is gone nothing can write under it.
        for _ in 0..40 {
            if std::fs::remove_dir_all(&self.tmp).is_ok() || !self.tmp.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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
         winit's display-free loop, with no NSApplication (AGENTS.md rule 5) — so a code path \
         reached AppKit or CoreGraphics: a door, not the environment. Log tail:\n{tail}"
    );
}

/// The environment refusal a dead or never-ready child's log names, if any:
/// the OS refusing the socket's `bind`.
fn environment_refusal(tail: &str) -> Option<&'static str> {
    bind_refused_by_the_os(tail).then_some("the OS refused the control socket's bind")
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
    let aterm = Path::new(env!("CARGO_BIN_EXE_aterm"));
    // The system's first-exec wait, paid here and not on the socket wait below
    // ([`aterm_spec::wait::first_exec`]); `--help` is answered above the mode
    // fork, before any setup.
    aterm_spec::wait::first_exec(aterm, &["--help"]);
    let mut cmd = Command::new(aterm);
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
/// the socket's `bind`). A child that exits first, or one not ready within the
/// budget, PANICS with the tail of `log` — the product failed to start (module
/// doc), and one whose log shows it asked for a WindowServer connection says so
/// — and `inst`'s own drop reaps it. For a test that launches its instances
/// itself (several sharing one scratch world, an explicit socket path);
/// [`boot_with`] uses it too.
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
                fail_if_it_asked_for_the_window_server(log, &tail);
                if let Some(why) = environment_refusal(&tail) {
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
    fail_if_it_asked_for_the_window_server(log, &tail);
    if let Some(why) = environment_refusal(&tail) {
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

/// A `ctl` client that has not exited by now is hung: past every minute-long
/// hang wait a call makes, so a server-side `await` that times out answers
/// before it is killed.
pub const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(120);

/// `aterm ctl --sock <sock> <args…>` against `inst`, in its scratch world,
/// stdout and stderr piped — not yet spawned.
pub fn client_command(inst: &Instance, args: &[&str]) -> Command {
    let aterm = Path::new(env!("CARGO_BIN_EXE_aterm"));
    // The file [`boot_with`] ran before its launch, so one lookup; a file cargo
    // has put there since is run here, before the client's clock.
    aterm_spec::wait::first_exec(aterm, &["--help"]);
    let mut cmd = Command::new(aterm);
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

/// One bounded `aterm ctl` call, read until it exits
/// (`support/child_output.rs`: a sibling's spawn can hold its pipes open).
///
/// A call the instance never ACCEPTED is asked again, for up to
/// [`aterm_spec::wait::MUST_HAPPEN`]. `aterm ctl` gives the accept phase 10 s
/// (`ACCEPT_DEADLINE`, which `--timeout` can only shorten) and then exits 2
/// with "the instance took no request, so none was sent" — nothing ran, so
/// asking again cannot run anything twice. A loaded merge gate saw a live
/// instance miss those 10 s once (`host_live_program`, 2026-10-05); a correctness
/// test's wait for a listener that must answer is a hang detector, not a budget.
pub fn ctl(inst: &Instance, args: &[&str]) -> Output {
    until_accepted(|| {
        let child = client_command(inst, args).spawn().expect("spawn aterm ctl");
        child_output::wait_and_read(child, Instant::now() + CLIENT_EXIT_DEADLINE)
            .unwrap_or_else(|| panic!("aterm ctl {args:?} did not exit in time"))
    })
}

/// Whether `out` is the `aterm ctl` client's refusal to send anything: the
/// instance's control socket did not ACCEPT its connection within the client's own
/// 10 s (`aterm-ctl`'s `ACCEPT_DEADLINE`, which `--timeout` can only shorten), and
/// the client says "so none was sent", exit 2. The instance is alive and not yet
/// taking connections - a debug build on a machine under a build storm can be slow
/// to start - and nothing reached it.
pub fn was_never_accepted(out: &Output) -> bool {
    out.status.code() == Some(2)
        && String::from_utf8_lossy(&out.stderr).contains("so none was sent")
}

/// Run `attempt` (one `aterm ctl` call) again for as long as it comes back
/// [`was_never_accepted`], up to the hang detector. Nothing was sent, so asking again
/// is waiting for the instance to be ready, not retrying an effect; any other
/// answer, success or failure, is returned at once.
pub fn until_accepted(mut attempt: impl FnMut() -> Output) -> Output {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let out = attempt();
        if !was_never_accepted(&out) || Instant::now() >= give_up {
            return out;
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
/// program and through any link, takes milliseconds (0.2 s idle and 6.6 s
/// under a loaded gate, 2026-09-26; links to a file already run skip it,
/// 2026-09-30). A first exec the PRODUCT made paid it where a test could not
/// wait for it: the host's version probe of a fake twin (bounded at 5 s,
/// `aterm_agent::harness::upgrade_drive`'s `VERSION_WAIT`), and a `/bin/sh`
/// script still running from a path its replacement was renamed onto
/// stopped at its next exec until that check ended (5.2 s to 8.5 s at a load
/// of 15 to 50, 2026-10-04). So a test ran each fake it wrote once before
/// the product met it — and wrote every fake anew: `host_live` paid the
/// check 50 times a run for six texts, 0.15 s to 2.7 s each with its tests
/// started together at a load of 10 to 30, three times in each upgrade test
/// (measured 2026-10-05).
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
/// # Panics
/// When `path`'s directory cannot be made or the script cannot be written: a
/// test without its fake decides nothing.
pub fn standin(path: &Path, text: &str) {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    static SHARED: Mutex<BTreeMap<String, Arc<OnceLock<Option<PathBuf>>>>> =
        Mutex::new(BTreeMap::new());

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

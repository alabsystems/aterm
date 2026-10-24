// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE BOOT of a real `aterm --headless` for the live integration tests.

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

/// How often a readiness EXTRA ([`boot_with`]'s `ready`: a token file, a journal line) is asked
/// once the instance has said its socket listens.
pub const POLL_GAP: Duration = Duration::from_millis(20);

/// One booted headless instance plus its scratch world.
pub struct Instance {
    pub child: Child,
    /// Cut after `child` is killed (fields drop after `Drop::drop`).
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

/// Remove a scratch world whose instances (and keeper) are dead, first releasing the warm spares
/// they left waiting on their FIFOs as the next launch in the world would: no launch comes to a
/// removed world, and a spare its hangup did not end waits in its FIFO's open for ever.
pub fn remove_world(root: &Path) {
    aterm_shell_integration::spare_fifo::sweep_dead(
        &root
            .join("run/aterm")
            .join(aterm_shell_integration::SPARE_DIR),
    );
    for _ in 0..40 {
        if std::fs::remove_dir_all(root).is_ok() || !root.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The instance's descendants as `ps` shows them now.
#[cfg(unix)]
pub fn process_tree(inst: &Instance) -> String {
    let out = std::process::Command::new(aterm_uds::os_path!("ps"))
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

/// A fresh exclusively created scratch root whose socket path fits `sun_path` as its clients dial
/// it.
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

fn bind_refused_by_the_os(log: &str) -> bool {
    log.lines().any(|l| {
        l.contains("control socket bind failed")
            && (l.contains("Operation not permitted") || l.contains("Permission denied"))
    })
}

fn window_server_refused(log: &str) -> bool {
    log.contains("FAILED TO establish the default connection to the WindowServer")
        || log.contains("_CGSDefaultConnection() is NULL")
}

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

fn environment_refusal(tail: &str) -> Option<&'static str> {
    bind_refused_by_the_os(tail).then_some("the OS refused the control socket's bind")
}

/// [`boot_with`] with `args` as the extra launch arguments and no readiness extras.
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

/// Arm `cmd`.
/// # Panics
/// The FIFO could not be made in `dir`: a scratch world this test cannot write to is a broken
/// fixture.
pub fn arm_ready(cmd: &mut Command, dir: &Path) -> Ready {
    let ready = Ready::open(dir)
        .unwrap_or_else(|e| panic!("arm the instance's ready signal in {}: {e}", dir.display()));
    ready.hand(cmd);
    ready
}

/// Boot one headless instance under a fresh scratch world named `prefix`.
#[track_caller]
pub fn boot_with(
    prefix: &str,
    configure: impl FnOnce(&Path, &mut Command),
    ready: impl Fn(&Instance) -> bool,
) -> Instance {
    boot_under(prefix, None, configure, ready)
}

/// [`boot_with`], the instance launched by `/usr/bin/sandbox-exec` under the Seatbelt `profile`.
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
    static CONFIGURED: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

/// The opening words of the refusal [`boot_with`] ends a test with when the instance's login shell
/// names a stand-in by another path.
pub const STANDIN_UNRESOLVED: &str = "the instance's login shell names a stand-in by another path";

fn launch_path(cmd: &Command) -> Option<std::ffi::OsString> {
    match cmd.get_envs().find(|(key, _)| *key == "PATH") {
        Some((_, value)) => value.map(std::ffi::OsStr::to_owned),
        None => std::env::var_os("PATH"),
    }
}

fn same_place(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

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

/// THE GUARD ([`boot_with`]).
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

/// What the instance's own login shell names each of `names` (`command -v`).
/// # Panics
/// A name a shell would read as more than one word, or a scratch root it would read as more than
/// one quoted word, is a broken fixture.
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

/// Wait for a spawned headless instance.
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

/// Where a hung instance waits.
pub fn thread_stack(pid: u32, log: &Path, thread: Option<&str>) -> String {
    const STACK_LINES: usize = 80;
    if !cfg!(target_os = "macos") {
        return "(no sampler on this platform)".to_owned();
    }
    let file = log.with_file_name("hang.sample.txt");
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
    let depth = |l: &str| l.len() - l.trim_start_matches([' ', '+', '!', ':', '|']).len();
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

/// A `ctl` client that has not exited by now is hung.
pub const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(120);

/// `aterm ctl --sock <sock> <args…>` against `inst`.
pub fn client_command(inst: &Instance, args: &[&str]) -> Command {
    let aterm = launch_isolation::aterm_executable();
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

/// Run `aterm drive <args…>` in `root`'s scratch environment with `extra` and `cwd` under
/// [`CLIENT_EXIT_DEADLINE`].
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

/// One bounded `aterm ctl` call.
pub fn ctl(inst: &Instance, args: &[&str]) -> Output {
    until_served(|| {
        let child = client_command(inst, args).spawn().expect("spawn aterm ctl");
        child_output::wait_and_read(child, Instant::now() + CLIENT_EXIT_DEADLINE)
            .unwrap_or_else(|| panic!("aterm ctl {args:?} did not exit in time"))
    })
}

/// `aterm ctl --sock <sock> subscribe <args…>` against `inst`.
pub fn subscribed(inst: &Instance, args: &[&str]) -> Child {
    let mut request = vec!["subscribe"];
    request.extend_from_slice(args);
    let give_up = Instant::now() + MUST_HAPPEN;
    loop {
        let mut child = client_command(inst, &request)
            .spawn()
            .expect("spawn aterm ctl subscribe");
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

/// Put the executable script `text` at `path`.
/// # Panics
/// When `path`'s directory cannot be made or the script cannot be written: a test without its fake
/// decides nothing.
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
        let _ = std::fs::remove_file(&own);
        std::fs::write(&own, text).ok()?;
        std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o555)).ok()?;
        let _ = std::fs::hard_link(&own, &file);
        let _ = std::fs::remove_file(&own);
    }
    whole(&file).then_some(file)
}

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

/// A zsh for a live test.
pub fn zsh() -> Option<&'static str> {
    static ZSH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ZSH.get_or_init(|| {
        aterm_spec::host::test_shell("zsh").map(|path| path.to_string_lossy().into_owned())
    })
    .as_deref()
}

/// [`zsh`] for a test whose claim is zsh's own.
#[track_caller]
pub fn need_zsh() -> &'static str {
    match zsh() {
        Some(zsh) => zsh,
        None => could_not_run(aterm_spec::host::tool("zsh").refusal()),
    }
}

/// A login shell for a live test whose claim holds under every shell aterm integrates alike.
#[derive(Clone, Copy, Debug)]
pub struct LoginShell {
    pub path: &'static str,
    /// `.zshrc` or `.bashrc`.
    pub rc: &'static str,
    pub kind: aterm_shell_integration::ShellType,
}

/// zsh where the host has one (every Mac; the lab's Linux lane).
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

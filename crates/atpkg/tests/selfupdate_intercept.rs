// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SELF-UPDATE INTERCEPT at the REAL process edge (2026-09-19, `atpkg::selfupdate`):
//! the dev `atpkg` binary driven as `__selfupdate claude <prefix> -- <args…>`, the way
//! the `agents/claude` twin execs it, over a temp prefix with a fake `bin/claude` that
//! echoes its argv and exits 7.
//!
//! What is pinned here and nowhere else: the CHILD — the standard `update claude
//! --wait-lock 1800` this verb spawns through the real dispatch edge — and the verb's
//! relay of its status. A store lock held by THIS process (a `Layout` over the temp
//! prefix, exactly what a sibling process presents to `flock`) for the first seconds of
//! the run makes the child WAIT — the default wait, thirty minutes, the window's own
//! bound, with no environment knob to shorten it (owner, 2026-09-19) — and, on STDOUT,
//! print the `--wait-lock` lane's two markers a typed `aterm pkg update` never prints
//! (`lock-waiting:` after the 2 s grace, `lock-acquired:` when the holder lets go;
//! review, 2026-09-19: the child IS a `--wait-lock` caller, so a person's terminal sees
//! them); then, the holder gone, the child runs its verb — claude's vendor-direct lane,
//! which a `dir:` registry cannot reach — and fails on its own line, so the verb answers
//! with the incomplete line, exit 1 — never 75 while a pass is merely busy. The 75 ending itself (the whole bound
//! elapsing) is not drivable here without a knob, and there is none: it is pinned
//! in-process by `cli.rs`'s own test, where `selfupdate_check` takes the bound as a
//! parameter and the same dev binary waits one second at a lock that test holds. The
//! other endings — help, a declined shape, an unrostered program, the disabled
//! manager, the prefix cross-check, the usage line, the tool-name gate and a `bin/`
//! shim that cannot be exec'd (126) — are pinned in-process by `cli.rs`'s test where a
//! plan exists and re-driven here so the exec, the exit code and the byte on the glass
//! are the binary's, not the plan's.
//!
//! WHAT CANNOT BE DRIVEN HERE: an up-to-date child (`atpkg: claude already current
//! (build N)`, exit 0). The binary verifies a `dir:` registry under the COMMITTED paper
//! master (`atpkg::PKG_TRUST_ANCHORS`, `Anchor::pinned`), whose secret half is on paper
//! and on no computer; the crate's registry fixtures sign with `sig::testkit`'s synthetic
//! master, which only an in-process `Anchor::of` accepts, and there is deliberately no
//! environment seam to swap the anchor (`cli.rs` asserts `ATPKG_ROOTKEY_OVERRIDE` is
//! gone). So the exit-0 arm is pinned as a literal arm by the registry scan in `lock.rs`
//! and reached live only against the real index.
//!
//! NEVER THE REAL STORE. The child's HOME is a temp directory and its XDG_CONFIG_HOME an
//! absent one, so `store::resolve_configured` lands on the DEFAULT prefix under that
//! temp HOME — the very prefix the twin operand names, so the cross-check agrees — and
//! the registry is a local directory (no network).

#![cfg(unix)]

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    config_home: PathBuf,
    registry: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("atpkg-selfupdate-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let config_home = root.join("config");
        let registry = root.join("registry");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&registry).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        assert!(
            prefix.starts_with(&home),
            "fixture: the default prefix must sit under the temp HOME: {}",
            prefix.display()
        );
        let fx = Self {
            root,
            home,
            config_home,
            registry,
            prefix,
        };
        fx.layout()
            .ensure_dir(&fx.prefix)
            .expect("fixture: the 0700 prefix directory");
        fx
    }

    fn layout(&self) -> atpkg::store::Layout {
        atpkg::store::Layout {
            prefix: self.prefix.clone(),
        }
    }

    fn row() -> &'static atpkg::selfupdate::Row {
        atpkg::selfupdate::row_for("claude").expect("claude is rostered")
    }

    /// A fake `bin/<tool>` — the shim the verb forwards to — that echoes its argv on
    /// stdout as `fake: <args>` and exits 7, so an exec'd forward is unmistakable in
    /// the exit code too.
    fn fake_shim(&self, tool: &str) {
        let layout = self.layout();
        layout.ensure_dir(&layout.bin_dir()).unwrap();
        let shim = layout.shim(&atpkg::store::ToolName::new(tool).unwrap());
        std::fs::write(&shim, b"#!/bin/sh\necho \"fake: $*\"\nexit 7\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A managed `claude` at `build`, the real way: a store tree whose `bin/claude` is
    /// the same echoing fake, its `bin/` shim laid by the activation code, so
    /// `ops::active_builds` names the build the epilogues speak of.
    fn install_claude(&self, build: u64) {
        let layout = self.layout();
        let dir = layout.build_dir("claude", build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin/claude"),
            b"#!/bin/sh\necho \"fake: $*\"\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(
            dir.join("bin/claude"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        atpkg::activate::install_shims(
            &layout,
            &dir,
            &["claude".to_string()],
            atpkg::activate::Aliases::Off,
        )
        .unwrap();
        atpkg::store::mark_build_ready(&dir).unwrap();
        assert_eq!(
            atpkg::ops::active_builds(&layout).get("claude"),
            Some(&build),
            "fixture: the build is active"
        );
    }

    /// The verb's argv: `__selfupdate <program> <prefix> -- <args…>`.
    fn verb_argv(&self, program: &str, args: &[&str]) -> Vec<OsString> {
        let mut argv: Vec<OsString> = vec![
            OsString::from(atpkg::selfupdate::HIDDEN_VERB),
            OsString::from(program),
            self.prefix.clone().into_os_string(),
            OsString::from("--"),
        ];
        argv.extend(args.iter().map(OsString::from));
        argv
    }

    /// The verb, confined to this fixture: `HOME`, config and registry pinned, the
    /// manager's kill switch and the spawner's pid cleared unless `env` sets them, both
    /// pipes captured, run to completion.
    fn selfupdate(&self, program: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.raw(&self.verb_argv(program, args), env)
    }

    /// The binary as `atpkg <argv…>` verbatim, confined the same way — a command not
    /// yet run, so a lane may stream it ([`stream`]) or wait for it ([`Fixture::raw`]).
    fn command(&self, argv: &[OsString], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.args(argv)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("ATPKG_REGISTRY", format!("dir:{}", self.registry.display()))
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    /// [`Fixture::command`], run to completion — for the endings ahead of the hand-over
    /// grammar (the usage line, the tool-name gate) and every ending that does not wait.
    fn raw(&self, argv: &[OsString], env: &[(&str, &str)]) -> Output {
        self.command(argv, env).output().expect("run the dev atpkg")
    }

    fn lock_held(&self) -> bool {
        self.layout().store_lock().exists()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        text(&out.stdout),
        text(&out.stderr)
    )
}

/// A spawned child with its stdout collected LINE BY LINE as it arrives (so a lane can
/// react to the `lock-waiting:` marker while the child is still waiting) and its stderr
/// drained concurrently (the two-pipe rule) — the shape `tests/store_lock_wait.rs` uses.
struct Streamed {
    child: Child,
    stdout: Arc<Mutex<Vec<String>>>,
    stderr: std::thread::JoinHandle<String>,
    stdout_reader: std::thread::JoinHandle<()>,
    started: Instant,
}

fn stream(mut cmd: Command) -> Streamed {
    let mut child = cmd.spawn().expect("spawn the dev atpkg");
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&stdout);
    let stdout_reader = std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    let stderr = std::thread::spawn(move || {
        let mut text = String::new();
        let mut err = err;
        let _ = err.read_to_string(&mut text);
        text
    });
    Streamed {
        child,
        stdout,
        stderr,
        stdout_reader,
        started: Instant::now(),
    }
}

impl Streamed {
    fn stdout_has(&self, needle: &str) -> bool {
        self.stdout
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(needle))
    }

    /// Block until stdout carries `needle`, or panic (killing the child) after
    /// `within`.
    fn wait_for_line(&mut self, needle: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.stdout_has(needle) {
            if let Some(status) = self.child.try_wait().expect("poll") {
                panic!(
                    "the child exited ({status}) before printing {needle:?}; stdout: {:?}",
                    self.stdout.lock().unwrap()
                );
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "no {needle:?} on stdout within {within:?}; stdout: {:?}",
                    self.stdout.lock().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Wait for the child to exit (killing it past `within`), returning its status,
    /// the elapsed time since spawn, every stdout line and the whole stderr.
    fn finish(mut self, within: Duration) -> (ExitStatus, Duration, Vec<String>, String) {
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "the child did not exit within {within:?}; stdout: {:?}",
                    self.stdout.lock().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let elapsed = self.started.elapsed();
        self.stdout_reader.join().unwrap();
        let stderr = self.stderr.join().unwrap();
        let stdout = self.stdout.lock().unwrap().clone();
        (status, elapsed, stdout, stderr)
    }
}

/// The child's own line when it cannot check: claude updates vendor-direct (design
/// §1.5), a `dir:` registry reaches no vendor, and the legacy build the fixture laid is
/// named by its index build number — the line an unreachable Anthropic channel prints.
const UNREACHABLE_LINE: &str =
    "atpkg: claude — Anthropic's release channel is unreachable; keeping build 2026091601";

/// Generous for a loaded CI box; the grace itself is 2 s.
const ANNOUNCE_WITHIN: Duration = Duration::from_secs(15);

/// The stdout lines a pass may carry that are not the wait lane's markers: what the
/// `[machine]` settings, applied at the dispatch edge ABOVE the lock and refused here
/// under a synthetic `HOME`, say about the host (the shape `tests/store_lock_wait.rs`
/// filters the same way).
fn wait_markers(stdout: &[String]) -> Vec<&str> {
    stdout
        .iter()
        .map(String::as_str)
        .filter(|l| {
            !(l.starts_with("atpkg: machine")
                || l.starts_with("atpkg machine:")
                || l.starts_with("atpkg noindex:")
                || l.starts_with("atpkg: Universal Control"))
        })
        .collect()
}

/// THE DEFAULT WAIT REALLY WAITS (owner, 2026-09-19: no environment knob — the bound is
/// the window's own thirty minutes, `selfupdate::WAIT_LOCK_SECS`): the store lock held
/// here for the first seconds of the run, and the child — the standard `update claude
/// --wait-lock 1800` — does not exit 75 while a pass is merely busy. It announces the
/// wait on STDOUT once the 2 s grace has passed (`lock-waiting:`, naming THIS fixture's
/// lock and the 1800 s bound), stands there until the holder lets go, answers
/// `lock-acquired:`, and only then runs its verb — claude's vendor-direct lane, which the
/// fixture's `dir:` registry cannot reach, so it fails on its own [`UNREACHABLE_LINE`] —
/// and the verb answers with the incomplete line and exit 1, the announce line FIRST on stderr, the
/// active build named as the one that stays, nothing of the vendor's on stdout. Exactly
/// those two markers and no `seed-busy:` — that is the terminal of a wait that ran OUT,
/// pinned in-process by `cli.rs`'s test with a one-second bound, since no knob exists
/// to shorten this one (and none may).
#[test]
fn a_held_store_is_waited_for_and_the_check_runs_once_the_holder_lets_go() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        // A build that pins no root key is disabled everywhere; the verb refuses before
        // the child (pinned below) and this ending is unreachable.
        return;
    }
    let fx = Fixture::new("held-then-released");
    fx.install_claude(2026091601);
    let missing = fx.root.join("no-such-registry");
    let guard = atpkg::lock::try_lock_store(&fx.layout()).expect("the fixture holds the lock");
    let registry = format!("dir:{}", missing.display());
    let mut child = stream(fx.command(
        &fx.verb_argv("claude", &["update"]),
        &[("ATPKG_REGISTRY", &registry)],
    ));
    let waiting = format!("atpkg: {}", atpkg::cli::LOCK_WAITING_MARKER);
    child.wait_for_line(&waiting, ANNOUNCE_WITHIN);
    let announced_after = child.started.elapsed();
    assert!(
        announced_after >= atpkg::lock::WAIT_ANNOUNCE_GRACE,
        "announced only once the grace had passed: {announced_after:?}"
    );
    assert!(
        child.child.try_wait().expect("poll").is_none(),
        "the child is still waiting, not exited 75, while the pass is merely busy"
    );
    // The holder lets go, a few seconds in.
    drop(guard);
    let (status, elapsed, stdout, stderr) = child.finish(Duration::from_secs(30));
    let shown = format!("status {status:?}\nstdout:\n{stdout:?}\nstderr:\n{stderr}");
    assert_eq!(
        status.code(),
        Some(1),
        "the check ran and failed on its own line: {shown}"
    );
    assert!(
        elapsed < Duration::from_secs(25),
        "the wait ended with the holder, not with the bound: {elapsed:?}\n{shown}"
    );
    let lock = fx.layout().store_lock().display().to_string();
    assert_eq!(
        wait_markers(&stdout),
        [
            format!(
                "atpkg: {}another atpkg process holds the store lock at {lock} \u{2014} waiting \
                 up to {} s for it to finish",
                atpkg::cli::LOCK_WAITING_MARKER,
                atpkg::selfupdate::WAIT_LOCK_SECS
            ),
            format!(
                "atpkg: {}the other process finished \u{2014} running now",
                atpkg::cli::LOCK_ACQUIRED_MARKER
            ),
        ],
        "the wait lane's two markers, in order, and nothing else of the child's: {shown}"
    );
    assert!(
        !stdout
            .iter()
            .any(|l| l.contains(atpkg::cli::SEED_BUSY_MARKER)),
        "no stand-aside: the wait ended in the lock: {shown}"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some(atpkg::selfupdate::announce_line(Fixture::row(), "build 2026091601").as_str()),
        "the announce line first: {shown}"
    );
    assert!(
        !stderr.contains("holds the store lock"),
        "no refusal on stderr — the child got the lock: {shown}"
    );
    assert!(
        lines.contains(&UNREACHABLE_LINE),
        "the child's own line, after the wait: {shown}"
    );
    assert_eq!(
        lines.last().copied(),
        Some(atpkg::selfupdate::incomplete_line(Fixture::row(), Some(2026091601)).as_str()),
        "the incomplete line last: {shown}"
    );
    assert!(
        !stdout.iter().any(|l| l.contains("fake:")),
        "the vendor's verb never ran: {shown}"
    );
    // The child took and released the lock: takeable again now.
    let _ = atpkg::lock::try_lock_store(&fx.layout()).expect("released by the child");
}

/// THE INCOMPLETE ENDING: claude updates vendor-direct and the fixture's `dir:` registry
/// reaches no vendor, so the child's `update claude` fails on its own line
/// ([`UNREACHABLE_LINE`], formerly the index lane's `update claude failed:`) and the verb exits 1
/// with the announce line first and the incomplete line last — the build that stays
/// named — and the store lock, taken by the child for its try, released again.
#[test]
fn an_unreachable_registry_answers_1_with_the_childs_line_then_the_incomplete_line() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        // A build that pins no root key is disabled everywhere; the verb refuses before
        // the child (pinned below) and this ending is unreachable.
        return;
    }
    let fx = Fixture::new("unreachable");
    fx.install_claude(2026091601);
    let missing = fx.root.join("no-such-registry");
    let out = fx.selfupdate(
        "claude",
        &["install", "latest"],
        &[("ATPKG_REGISTRY", &format!("dir:{}", missing.display()))],
    );
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(1), "{shown}");
    let stderr = text(&out.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some(atpkg::selfupdate::announce_line(Fixture::row(), "build 2026091601").as_str()),
        "{shown}"
    );
    assert!(
        lines.contains(&UNREACHABLE_LINE),
        "the child's own line: {shown}"
    );
    assert_eq!(
        lines.last().copied(),
        Some(atpkg::selfupdate::incomplete_line(Fixture::row(), Some(2026091601)).as_str()),
        "{shown}"
    );
    assert!(!text(&out.stdout).contains("fake:"), "{shown}");
    // The child took and released the lock: takeable again now.
    let _ = atpkg::lock::try_lock_store(&fx.layout()).expect("released by the child");
}

/// HELP FORWARDS: `update --help` prints the one note on stderr and execs the `bin/`
/// shim with the arguments verbatim — its stdout, its exit code (7) — and no store lock
/// ever comes into being.
#[test]
fn help_forwards_to_the_vendor_after_one_note_and_takes_no_lock() {
    let fx = Fixture::new("help");
    fx.fake_shim("claude");
    let out = fx.selfupdate("claude", &["update", "--help"], &[]);
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(7), "the fake's own code: {shown}");
    assert_eq!(text(&out.stdout), "fake: update --help\n", "{shown}");
    assert_eq!(
        text(&out.stderr),
        format!("{}\n", atpkg::selfupdate::help_line(Fixture::row())),
        "{shown}"
    );
    assert!(!fx.lock_held(), "help takes no lock");
}

/// DECLINED SHAPES: `install stable`, `install 2.1.200` and `update --foo` exit 2 with
/// their exact line, nothing on stdout, the fake never run, no lock — and no
/// environment can change that: there is no escape hatch (the first cut's was removed
/// 2026-09-19 at the owner's direction), so the vendor's installer is never run through
/// the managed name.
#[test]
fn a_declined_shape_exits_2_with_its_line_and_runs_nothing() {
    use atpkg::selfupdate::{Decline, declined_line};
    let fx = Fixture::new("declined");
    fx.fake_shim("claude");
    let row = Fixture::row();
    let rest = vec![String::from("--foo")];
    for (typed, line) in [
        (
            &["install", "stable"][..],
            declined_line(
                row,
                &Decline::NotAChannel {
                    verb: "install",
                    target: "stable",
                },
            ),
        ),
        (
            &["install", "2.1.200"],
            declined_line(
                row,
                &Decline::Version {
                    verb: "install",
                    target: "2.1.200",
                },
            ),
        ),
        (
            &["update", "--foo"],
            declined_line(
                row,
                &Decline::Shape {
                    verb: "update",
                    rest: &rest,
                },
            ),
        ),
    ] {
        let out = fx.selfupdate("claude", typed, &[]);
        let shown = describe(&out);
        assert_eq!(out.status.code(), Some(2), "{typed:?}: {shown}");
        assert_eq!(text(&out.stderr), format!("{line}\n"), "{typed:?}: {shown}");
        assert_eq!(text(&out.stdout), "", "{typed:?}: {shown}");
        assert!(
            !text(&out.stderr).contains("ATPKG_"),
            "no line names an environment variable: {shown}"
        );
    }
    assert!(!fx.lock_held());
}

/// AN UNROSTERED PROGRAM (a twin laid by a newer roster than this binary's) forwards to
/// its `bin/` shim verbatim with nothing printed at all.
#[test]
fn an_unrostered_program_forwards_silently() {
    let fx = Fixture::new("unrostered");
    fx.fake_shim("ay");
    let out = fx.selfupdate("ay", &["update", "--x"], &[]);
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(7), "{shown}");
    assert_eq!(text(&out.stdout), "fake: update --x\n", "{shown}");
    assert_eq!(text(&out.stderr), "", "{shown}");
    assert!(!fx.lock_held());
}

/// NO ENVIRONMENT KILL SWITCH (2026-09-23): `ATPKG_DISABLE=1`, which used to reach the
/// disabled line, is inert — the verb a person typed goes on to its check. The disabled
/// line itself is an unpinned build's alone now, pinned as a pure render elsewhere.
#[test]
fn an_environment_variable_does_not_disable_the_intercept() {
    let fx = Fixture::new("disabled");
    fx.install_claude(2026091601);
    let out = fx.selfupdate("claude", &["update"], &[("ATPKG_DISABLE", "1")]);
    let shown = describe(&out);
    assert!(
        !text(&out.stderr).contains(&atpkg::selfupdate::disabled_line(
            Fixture::row(),
            Some(2026091601)
        )),
        "{shown}"
    );
}

/// THE PREFIX CROSS-CHECK at the edge: the twin's operand names a store other than the
/// one `HOME` resolves in this environment, so the verb refuses with the exact line
/// naming both, exit 2, and runs nothing — never an update of a store this twin does
/// not run.
#[test]
fn a_prefix_the_environment_does_not_resolve_is_refused_with_both_named() {
    let fx = Fixture::new("prefix");
    fx.fake_shim("claude");
    let elsewhere = fx.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
    cmd.arg(atpkg::selfupdate::HIDDEN_VERB)
        .arg("claude")
        .arg(&elsewhere)
        .arg("--")
        .arg("update")
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", &fx.config_home)
        .env("ATPKG_REGISTRY", format!("dir:{}", fx.registry.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().unwrap();
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(2), "{shown}");
    assert_eq!(
        text(&out.stderr),
        format!(
            "{}\n",
            atpkg::selfupdate::prefix_mismatch_line(Fixture::row(), &elsewhere, &fx.prefix)
        ),
        "{shown}"
    );
    assert_eq!(text(&out.stdout), "", "{shown}");
    assert!(!fx.lock_held());
    assert!(!Path::new(&elsewhere).join("store.lock").exists());
}

/// THE USAGE LINE, byte for byte, exit 2, nothing on stdout, no lock: a bare
/// `atpkg __selfupdate` and `atpkg __selfupdate --help` (a `-`-led program operand is
/// usage, not a tool name — the verb's second review correction).
#[test]
fn a_bare_or_help_invocation_prints_the_usage_line_and_exits_2() {
    let fx = Fixture::new("usage");
    for argv in [
        vec![OsString::from(atpkg::selfupdate::HIDDEN_VERB)],
        vec![
            OsString::from(atpkg::selfupdate::HIDDEN_VERB),
            OsString::from("--help"),
        ],
    ] {
        let out = fx.raw(&argv, &[]);
        let shown = describe(&out);
        assert_eq!(out.status.code(), Some(2), "{argv:?}: {shown}");
        assert_eq!(
            text(&out.stderr),
            format!(
                "usage: atpkg {} <program> [<prefix>] -- [args…]\n",
                atpkg::selfupdate::HIDDEN_VERB
            ),
            "{argv:?}: {shown}"
        );
        assert_eq!(text(&out.stdout), "", "{argv:?}: {shown}");
    }
    assert!(!fx.lock_held());
}

/// THE TOOL-NAME GATE, byte for byte: a sensitive name (`sudo`) and the empty name are
/// refused before anything resolves — exit 2, `atpkg: "<name>" is not a tool name`, no
/// lock, nothing run.
#[test]
fn a_name_that_is_not_a_tool_name_is_refused_with_its_line_and_exits_2() {
    let fx = Fixture::new("toolname");
    for name in ["sudo", ""] {
        let argv = vec![
            OsString::from(atpkg::selfupdate::HIDDEN_VERB),
            OsString::from(name),
            fx.prefix.clone().into_os_string(),
            OsString::from("--"),
            OsString::from("update"),
        ];
        let out = fx.raw(&argv, &[]);
        let shown = describe(&out);
        assert_eq!(out.status.code(), Some(2), "{name:?}: {shown}");
        assert_eq!(
            text(&out.stderr),
            format!("atpkg: {name:?} is not a tool name\n"),
            "{name:?}: {shown}"
        );
        assert_eq!(text(&out.stdout), "", "{name:?}: {shown}");
    }
    assert!(!fx.lock_held());
}

/// A FORWARD THAT CANNOT EXEC: `bin/claude` present but not executable (mode 0644), a
/// pass-through shape (`--probe`) — the verb's `exec` fails and it says so on stderr,
/// `atpkg: could not run <shim>: <err>` (the `cmd_landing` literal), exit 126, nothing
/// on stdout, no lock. The error text is the OS's (`Permission denied (os error 13)` on
/// macOS and Linux alike), so the line is pinned up to it and the reason is checked by
/// its word.
#[test]
fn a_shim_that_cannot_be_run_answers_126_with_the_could_not_run_line() {
    let fx = Fixture::new("noexec");
    fx.fake_shim("claude");
    let layout = fx.layout();
    let shim = layout.shim(&atpkg::store::ToolName::new("claude").unwrap());
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = fx.selfupdate("claude", &["--probe"], &[]);
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(126), "{shown}");
    let stderr = text(&out.stderr);
    let head = format!("atpkg: could not run {}: ", shim.display());
    assert!(stderr.starts_with(&head), "{shown}");
    assert!(stderr.contains("Permission denied"), "{shown}");
    assert_eq!(stderr.lines().count(), 1, "{shown}");
    assert_eq!(text(&out.stdout), "", "{shown}");
    assert!(!fx.lock_held());
}

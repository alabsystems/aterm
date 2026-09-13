// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The store lock at the REAL process edge, two processes at a time (2026-09-10).
//!
//! The incident: a second aterm instance (the macOS Full Disk Access grant quits the
//! app and opens it again) ran its launch-time `atpkg seed` while the first
//! instance's pass still held `store.lock`, was refused, and reported "install
//! failed" — and nothing retried for six hours. This test holds the lock in THIS
//! process (a `Layout` over a temp prefix, exactly what a sibling process presents to
//! `flock`) and drives the dev `atpkg` binary against it: a `--wait-lock` child
//! announces the wait and proceeds once the holder lets go, times out with exit 75
//! (`EX_TEMPFAIL`) and says it stood aside (`seed-busy:`) when it does not, a typed
//! verb stays fail-fast with the same code and prints no marker at all,
//! an unwritable prefix is never waited on (exit 1), an uncontended child prints
//! no marker at all, a waiter whose spawner has gone stands down silently, and a
//! waiter whose parent is init from the start is no orphan and waits like any other.
//!
//! NEVER THE REAL STORE. The child's HOME is a temp directory and its
//! XDG_CONFIG_HOME is an absent one, so `store::resolve` lands on the DEFAULT prefix
//! under that temp HOME (the developer's `[packages].prefix`, if any, is never read);
//! `<prefix>/declined` makes `seed` exit 0 with one sentence before any index work,
//! so no network and no store mutation happen even when the child does run.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The sentence a `--wait-lock` child prints once its wait outlasts the grace
/// (`atpkg::cli::LOCK_WAITING_MARKER` with the `atpkg: ` prefix the GUI strips).
fn waiting_line() -> String {
    format!("atpkg: {}", atpkg::cli::LOCK_WAITING_MARKER)
}

/// The sentence a declined machine's `seed` prints instead of provisioning.
const DECLINED_SENTENCE: &str = "removed on this machine";

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
            std::env::temp_dir().join(format!("atpkg-lock-wait-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let config_home = root.join("config");
        let registry = root.join("registry");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&registry).unwrap();
        // The DEFAULT prefix under the temp HOME — the one place `store::resolve`
        // can land with no config: it cannot fall back onto the real store because
        // the default itself is under this HOME.
        let prefix = atpkg::store::default_prefix(&home);
        assert!(
            prefix.starts_with(&home),
            "fixture: the default prefix must sit under the temp HOME: {}",
            prefix.display()
        );
        Self {
            root,
            home,
            config_home,
            registry,
            prefix,
        }
    }

    fn layout(&self) -> atpkg::store::Layout {
        atpkg::store::Layout {
            prefix: self.prefix.clone(),
        }
    }

    /// Take the store lock in THIS process (creates the 0700 prefix), and write the
    /// `declined` marker so a child that does get to run its verb exits 0 at once.
    fn hold(&self) -> atpkg::lock::StoreLock {
        let guard =
            atpkg::lock::try_lock_store(&self.layout()).expect("the fixture holds the lock");
        self.decline();
        guard
    }

    fn decline(&self) {
        let layout = self.layout();
        layout
            .ensure_dir(&self.prefix)
            .expect("fixture: the prefix directory");
        std::fs::write(layout.declined(), b"declined by the test fixture\n").unwrap();
    }

    /// A command confined to this fixture's HOME, config and registry, both pipes
    /// captured — for the dev `atpkg` itself or for a shell that runs it.
    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("ATPKG_REGISTRY", format!("dir:{}", self.registry.display()))
            .env_remove("ATPKG_DISABLE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn spawn(&self, args: &[&str]) -> Child {
        self.command(env!("CARGO_BIN_EXE_atpkg"))
            .args(args)
            .spawn()
            .expect("spawn dev atpkg child")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A spawned child with its stdout collected LINE BY LINE as it arrives (so the test
/// can react to the waiting marker while the child is still waiting) and its stderr
/// drained concurrently (the two-pipe rule).
struct Streamed {
    child: Child,
    stdout: Arc<Mutex<Vec<String>>>,
    stderr: std::thread::JoinHandle<String>,
    stdout_reader: std::thread::JoinHandle<()>,
    started: Instant,
}

fn stream(mut child: Child) -> Streamed {
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

fn lock_path_of(prefix: &Path) -> String {
    atpkg::store::Layout {
        prefix: prefix.to_path_buf(),
    }
    .store_lock()
    .display()
    .to_string()
}

/// Generous for a loaded CI box; the grace itself is 2 s.
const ANNOUNCE_WITHIN: Duration = Duration::from_secs(15);

/// THE INCIDENT, FIXED: a `--wait-lock` child finds the lock held, announces the
/// wait (naming THIS fixture's lock path, never the real store's), and — once the
/// holder lets go — runs its verb and exits 0, with its normal output AFTER the
/// announcement and nothing on stderr about the lock.
#[test]
fn a_waiting_seed_proceeds_once_the_holder_releases() {
    let fx = Fixture::new("proceeds");
    let guard = fx.hold();
    let mut child = stream(fx.spawn(&["seed", "--wait-lock", "60"]));
    child.wait_for_line(&waiting_line(), ANNOUNCE_WITHIN);
    assert!(
        !child.stdout_has(DECLINED_SENTENCE),
        "the verb must not have run while the lock was held"
    );
    drop(guard);
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(20));
    assert!(status.success(), "proceeded after the release: {status}");
    let lock = lock_path_of(&fx.prefix);
    let waiting_at = stdout
        .iter()
        .position(|l| l.starts_with(&waiting_line()))
        .expect("the waiting line");
    assert!(
        stdout[waiting_at].contains(&lock),
        "the announcement names the lock path: {}",
        stdout[waiting_at]
    );
    let ran_at = stdout
        .iter()
        .position(|l| l.contains(DECLINED_SENTENCE))
        .expect("the verb's own sentence");
    assert!(
        ran_at > waiting_at,
        "the verb runs AFTER the wait: {stdout:?}"
    );
    assert!(
        !stderr.contains("holds the store lock"),
        "no refusal on stderr: {stderr}"
    );
}

/// A holder that never lets go: the waiter announces, waits its whole bound, and
/// exits 75 (`EX_TEMPFAIL`) with the unchanged loud sentence on stderr — never
/// having run its verb — and tells the lane that asked to wait, on stdout and once,
/// that it stood aside (0.82.0's `seed-busy:` terminal, which a typed verb never
/// prints: the test after this one pins the silence).
#[test]
fn a_waiting_seed_times_out_with_exit_75() {
    let fx = Fixture::new("timeout");
    let _guard = fx.hold();
    let bound = atpkg::lock::WAIT_ANNOUNCE_GRACE + Duration::from_secs(1);
    let child = stream(fx.spawn(&["seed", "--wait-lock", &bound.as_secs().to_string()]));
    let (status, elapsed, stdout, stderr) = child.finish(Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(i32::from(atpkg::lock::CONTENDED_EXIT)),
        "a timed-out wait is the contention code: {status}; stderr: {stderr}"
    );
    assert!(elapsed >= bound, "waited the whole bound: {elapsed:?}");
    let waiting_at = stdout
        .iter()
        .position(|l| l.starts_with(&waiting_line()))
        .unwrap_or_else(|| panic!("announced the wait: {stdout:?}"));
    // THE STOOD-ASIDE TERMINAL REACHES THE `--wait-lock` CALLER. It is what the
    // window's reader folds into its one timed-out-wait log line (the exit code, 75,
    // is what keys its "another aterm is installing" reading), so a waiter that timed
    // out without it would leave the window with an exit code and no sentence. Once,
    // and after the announcement: a terminal that came first would answer a wait not
    // yet begun.
    let busy_line = format!("atpkg: {}", atpkg::cli::SEED_BUSY_MARKER);
    let busy: Vec<usize> = stdout
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with(&busy_line))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        busy.len(),
        1,
        "exactly one `seed-busy:` line reaches the waiting caller: {stdout:?}"
    );
    assert!(
        busy[0] > waiting_at,
        "stood aside AFTER announcing the wait: {stdout:?}"
    );
    assert!(
        stdout[busy[0]].contains("stood aside"),
        "the terminal says what this pass did: {}",
        stdout[busy[0]]
    );
    assert!(
        !stdout.iter().any(|l| l.contains(DECLINED_SENTENCE)),
        "never ran the verb: {stdout:?}"
    );
    let lock = lock_path_of(&fx.prefix);
    assert!(
        stderr.contains("another atpkg process holds the store lock at") && stderr.contains(&lock),
        "the loud sentence, naming this fixture's lock: {stderr}"
    );
}

/// A typed verb (no flag) is unchanged in everything but the code: instant, loud on
/// stderr, silent on stdout — a person at a terminal is told at once and never
/// waits invisibly.
#[test]
fn a_typed_seed_stays_fail_fast_with_exit_75() {
    let fx = Fixture::new("typed");
    let _guard = fx.hold();
    let child = stream(fx.spawn(&["seed"]));
    let (status, elapsed, stdout, stderr) = child.finish(Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(i32::from(atpkg::lock::CONTENDED_EXIT)),
        "{status}; stderr: {stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "fail-fast, not a wait: {elapsed:?}"
    );
    assert!(stdout.is_empty(), "no marker for a typed verb: {stdout:?}");
    assert!(
        stderr.contains("another atpkg process holds the store lock at"),
        "{stderr}"
    );
}

/// An unwritable prefix is not waited on, even with `--wait-lock`: the `Io` refusal
/// keeps exit 1 and comes back at once — the remedy is a different one.
#[test]
fn an_unwritable_prefix_never_waits_and_keeps_exit_1() {
    let fx = Fixture::new("badprefix");
    std::fs::create_dir_all(fx.prefix.parent().expect("a parent")).unwrap();
    std::fs::write(&fx.prefix, b"a file where the prefix directory should be").unwrap();
    let child = stream(fx.spawn(&["seed", "--wait-lock", "30"]));
    let (status, elapsed, stdout, stderr) = child.finish(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1), "{status}; stderr: {stderr}");
    assert!(
        elapsed < Duration::from_secs(2),
        "no wait on Io: {elapsed:?}"
    );
    assert!(stdout.is_empty(), "{stdout:?}");
    assert!(stderr.contains("cannot take the store lock"), "{stderr}");
}

/// Whether `pid` is alive, asked the way atpkg's own progress reader asks
/// (`kill -0`), because the waiter below is a GRANDCHILD this process cannot wait
/// for.
fn pid_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// An ORPHANED waiter stands down on its own, silently, long before its bound: the
/// window that spawned it quits while it is still queued — the incident's relaunch,
/// had the first window been the one waiting — and it must neither poll out its
/// half-hour (and then race the successor's own waiter for the freed lock) nor
/// announce to a pipe nobody reads. The waiter is spawned by a throwaway `sh`
/// (`script`, with `$1` the dev atpkg) that prints the waiter's pid and exits at
/// once, so the waiter is re-parented — to launchd on macOS, to a subreaper or init
/// on Linux — while the lock stays held here the whole time, so the only way the
/// waiter can exit early is the orphan check. Its exit code is unobservable by
/// design (init reaps it); what is observable is the pid going away within seconds
/// of a 60 s bound, a stdout carrying nothing but the pid line (no `lock-waiting:`
/// announcement — the orphan check precedes it on every poll) and an empty stderr.
fn an_orphaned_waiter_stands_down(case: &str, script: &str) {
    let fx = Fixture::new(case);
    let guard = fx.hold();
    let sh = fx
        .command("/bin/sh")
        .arg("-c")
        // The waiter inherits both pipes, so this process sees EOF on them only
        // once the waiter itself has exited.
        .arg(script)
        .arg("sh")
        .arg(env!("CARGO_BIN_EXE_atpkg"))
        .spawn()
        .expect("spawn the throwaway parent");
    let started = Instant::now();
    let streamed = stream(sh);
    // The throwaway parent exits at once; the waiter's pid is its one line.
    let pid = loop {
        let found = streamed.stdout.lock().unwrap().iter().find_map(|l| {
            l.strip_prefix("waiter=")
                .and_then(|p| p.trim().parse::<u32>().ok())
        });
        if let Some(pid) = found {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no waiter pid line within 10 s: {:?}",
            streamed.stdout.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    // The waiter is gone within seconds; its bound was 60 s.
    while pid_alive(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the orphaned waiter (pid {pid}) is still alive 20 s into a 60 s bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let gone_after = started.elapsed();
    // Both pipes reach EOF once the waiter is gone: it said nothing on either.
    let (status, _, stdout, stderr) = streamed.finish(Duration::from_secs(10));
    assert!(status.success(), "the throwaway parent itself: {status}");
    assert_eq!(
        stdout,
        vec![format!("waiter={pid}")],
        "the waiter printed nothing — no announcement to a dead window (gone after {gone_after:?})"
    );
    assert!(
        stderr.trim().is_empty(),
        "…and nothing on stderr either: {stderr}"
    );
    // Neither the announcement nor the verb's own sentence ever arrived — the
    // orphan check preceded the announcement on every poll, and the waiter never
    // held the lock (a verb run on this declined fixture prints its sentence).
    assert!(
        !stdout.iter().any(|l| l.starts_with(&waiting_line())),
        "no `lock-waiting:` announcement: {stdout:?}"
    );
    assert!(
        !stdout.iter().any(|l| l.contains(DECLINED_SENTENCE)),
        "the verb never ran: {stdout:?}"
    );
    // …and once THIS holder lets go the lock is takeable at once: the orphan left
    // nothing behind on it. (Asserting `Contended` while `guard` was still alive
    // could never fail — a second flock from the holder's own process is always
    // refused — so the guard is released first, and this is the assertion that can.)
    drop(guard);
    assert!(
        atpkg::lock::try_lock_store(&fx.layout()).is_ok(),
        "the lock is free once the holder releases it"
    );
}

/// The window's shape: the spawner NAMES ITSELF (`ATPKG_SPAWNER_PID`, the `sh`'s
/// own `$$`) beside `--wait-lock`, so "my parent is gone" is a comparison against
/// that pid and holds on every Unix — including a Linux subreaper, where the orphan
/// is re-parented to a pid that is not 1. This `sh` exits within microseconds of
/// the fork, before the waiter has even reached its dispatch edge: exactly the race
/// a `getppid`-at-the-edge guess loses (measured 2026-09-10 — the guess recorded
/// launchd as the spawner and the waiter polled out its whole bound under ppid 1).
#[test]
fn an_orphaned_waiter_stands_down_silently_before_its_bound() {
    an_orphaned_waiter_stands_down(
        "orphan",
        r#"ATPKG_SPAWNER_PID=$$ "$1" seed --wait-lock 60 & echo "waiter=$!""#,
    );
}

/// A waiter whose parent IS init from the start — launchd's or a subreaper's job,
/// with no `ATPKG_SPAWNER_PID` (a launchd agent's) — is not an orphan: the edge's
/// own `getppid` recorded that parent as the spawner, it never changes, and the
/// waiter waits out its bound like any other — announcing after the grace and
/// timing out loudly with 75 at the deadline. Under the old rule ("init as the
/// parent is orphaned") this stood down silently at its first contended poll,
/// which made an unnamed `--wait-lock` a silent instant exit 75 from every
/// pid-1-parented caller. (A `nohup … &` job is NOT this shape: its parent at the
/// edge is the shell that launched it, and it stands down once that shell exits;
/// a cron job's parent is cron's own child or the `sh` it runs, not init.) The
/// shape is DETERMINISTIC, not a race on the spawner's exit: a throwaway `sh` forks
/// a subshell that sleeps and exits at once; the subshell (the pid `$!` names) is
/// re-parented while it sleeps and only THEN execs the waiter, so the parent at the
/// dispatch edge is already init (or the subreaper). Both pipes reach this process
/// through the inherited fds.
#[test]
fn a_waiter_whose_parent_is_init_from_the_start_is_not_an_orphan() {
    let fx = Fixture::new("init-parent");
    let _guard = fx.hold();
    let bound = atpkg::lock::WAIT_ANNOUNCE_GRACE + Duration::from_secs(1);
    let sh = fx
        .command("/bin/sh")
        .arg("-c")
        .arg(r#"( sleep 1; exec "$1" seed --wait-lock "$2" ) & echo "waiter=$!""#)
        .arg("sh")
        .arg(env!("CARGO_BIN_EXE_atpkg"))
        .arg(bound.as_secs().to_string())
        .spawn()
        .expect("spawn the throwaway parent");
    let sh_pid = sh.id();
    let started = Instant::now();
    let streamed = stream(sh);
    // The announcement is the proof it did NOT stand down: it is printed only
    // after the grace and only while the waiter still wants the lock.
    let deadline = started + Duration::from_secs(1) + ANNOUNCE_WITHIN;
    while !streamed.stdout_has(&waiting_line()) {
        assert!(
            Instant::now() < deadline,
            "no announcement — the waiter stood down: {:?}",
            streamed.stdout.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = streamed
        .stdout
        .lock()
        .unwrap()
        .iter()
        .find_map(|l| {
            l.strip_prefix("waiter=")
                .and_then(|p| p.trim().parse::<u32>().ok())
        })
        .expect("the waiter's pid line");
    // The shape held: the announcing waiter's parent is not the (dead) `sh`.
    let ppid = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok());
    assert!(
        ppid.is_some_and(|p| p != sh_pid),
        "the waiter (pid {pid}) is re-parented away from sh (pid {sh_pid}): ppid {ppid:?}"
    );
    // Both pipes reach EOF once the waiter is gone.
    let (status, _, stdout, stderr) = streamed.finish(Duration::from_secs(30));
    let elapsed = started.elapsed();
    assert!(status.success(), "the throwaway parent itself: {status}");
    assert!(
        elapsed >= Duration::from_secs(1) + bound,
        "waited out the whole bound after the sleep: {elapsed:?}"
    );
    assert!(
        !stdout.iter().any(|l| l.contains(DECLINED_SENTENCE)),
        "never ran the verb: {stdout:?}"
    );
    let lock = lock_path_of(&fx.prefix);
    assert!(
        stderr.contains("another atpkg process holds the store lock at") && stderr.contains(&lock),
        "timed out LOUDLY, not silently: {stderr}"
    );
}

/// No contention, no marker: the flag changes nothing about an uncontended pass.
#[test]
fn a_declined_seed_without_contention_is_unchanged() {
    let fx = Fixture::new("uncontended");
    fx.decline();
    let child = stream(fx.spawn(&["seed", "--wait-lock", "30"]));
    let (status, elapsed, stdout, stderr) = child.finish(Duration::from_secs(10));
    assert!(status.success(), "{status}; stderr: {stderr}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert!(
        !stdout.iter().any(|l| l.starts_with(&waiting_line())),
        "the marker prints only on contention: {stdout:?}"
    );
    assert!(
        stdout.iter().any(|l| l.contains(DECLINED_SENTENCE)),
        "the verb ran: {stdout:?}"
    );
}
